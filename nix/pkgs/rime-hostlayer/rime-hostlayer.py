#!/usr/bin/env python3
"""rime-hostlayer: the per-machine layer of a Rime OS NixOS system (PLAN.md D2).

The system closure is built once, in CI, for every machine. What belongs to ONE machine lives outside it:

  /var/lib/rime/host.toml      hostname, timezone, locale, keyboards, extra mounts, the LUKS volume
  /var/lib/rime/switch/        runtime switches: a unit that bootc enabled at run time (a guest's wipe unit,
                               rime-firewall) carries a ConditionPathExists on a file here
  /var/lib/rime/carry/         what `carry` brought over from the old system, and the "left behind" report

Subcommands:

  carry --from SRC --root DEST --system TOPLEVEL [--declared JSON] [--image-etc DIR] [--cmdline FILE]
        [--plan] [--force]
      Bring one machine's state from an old root (bootc: its /etc and /var) into a new NixOS root, before the
      new system's first activation. Opens SRC read-only and never writes under it. Refuses BY NAME, before
      writing anything, when an account the machine must keep (a person, an account with subordinate ids, their
      primary groups) collides with an id the NixOS system declares. --cmdline is the old system's kernel
      command line (the bridge passes /proc/cmdline): its per-machine arguments (LUKS, console keymap, resume)
      go to host.toml [boot] kernel_params, which the boot hook appends to every entry; the rest are reported.
  host show | get KEY | validate [--file F]
  generate-mounts NORMAL [EARLY LATE]      the systemd generator (installed as rime-hostlayer-mounts)
  apply [--root R]                         activation: hostname, timezone, locale, keyboards from host.toml
  subid save | restore [--root R]          keeps carried /etc/subuid and /etc/subgid ranges across activations
  switch list | get NAME | on NAME | off NAME [--now]

Exit codes: 0 done; 1 failed; 2 refused (a named reason, nothing written); 64 usage.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tomllib

PROG = "rime-hostlayer"

HOST_TOML = os.environ.get("RIME_HOST_TOML", "/var/lib/rime/host.toml")
SWITCH_DIR = os.environ.get("RIME_SWITCH_DIR", "/var/lib/rime/switch")
SWITCH_REGISTRY = os.environ.get("RIME_SWITCH_REGISTRY", "/etc/rime/hostlayer/switches")
SUBID_STASH = os.environ.get("RIME_SUBID_STASH", "/run/rime-hostlayer")
SW = "/run/current-system/sw"
# bootc's shipped /etc/greetd/config.toml (files/desktop/rime-greet/greetd-config.toml), set by the package.
GREETD_DEFAULT = "@greetdDefault@"

EXIT_FAIL, EXIT_REFUSED, EXIT_USAGE = 1, 2, 64

# Mount points that are the operating system's, never a per-machine extra (D2: NixOS's fileSystems own them).
OS_MOUNTPOINTS = {
    "/", "/boot", "/boot/efi", "/efi", "/usr", "/nix", "/nix/store", "/etc", "/var", "/sysroot", "/home",
    "/var/home", "/proc", "/sys", "/dev", "/run", "/tmp",
}

# The keys host.toml knows, each with its pattern. A value that does not match is refused, never written.
LOCALE_KEYS = ("LANG", "LANGUAGE", "LC_CTYPE", "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY",
               "LC_MESSAGES", "LC_PAPER", "LC_NAME", "LC_ADDRESS", "LC_TELEPHONE", "LC_MEASUREMENT",
               "LC_IDENTIFICATION")
PATTERNS = {
    "hostname": r"[A-Za-z0-9]([A-Za-z0-9-]{0,62})(\.[A-Za-z0-9]([A-Za-z0-9-]{0,62}))*",
    "timezone": r"[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*",
    "locale": r"[A-Za-z0-9_.@:-]+",
    "keymap": r"[A-Za-z0-9_.-]+",
    "font": r"[A-Za-z0-9_.-]+",
    "xkb": r"[A-Za-z0-9_,:()+-]*",
    "fstype": r"[a-z0-9._]+",
    "options": r"[^\s#]+",
    "what": r"[^\s#]+",
    "unit": r"[A-Za-z0-9_.:-]+(@[A-Za-z0-9_.:-]*)?\.(service|socket|timer|path|mount)",
    # Exactly what rime-boot (nix/pkgs/rime-boot SAFE_PARAM) writes into a menu entry: nothing GRUB would expand,
    # quote or split.
    "kernel_param": r"[A-Za-z0-9_./:=,+@%~^\-\[\]!]+",
}

# [boot] layout values rime-boot reads (nix/modules/boot.nix rime.boot.layout, plus "auto").
BOOT_LAYOUTS = ("auto", "bls", "custom-cfg")
# [boot] microcode values rime-boot reads (nix/pkgs/rime-boot MICROCODE): which vendors' early-microcode cpios every
# entry loads.
BOOT_MICROCODE = ("both", "intel", "amd", "none")
# /proc/cpuinfo vendor_id -> the [boot] microcode carry writes (installer/rime-install-nixos.sh decides the same way)
CPU_MICROCODE = {"GenuineIntel": "intel", "AuthenticAMD": "amd"}

# Network filesystems (libmount's mnt_fstype_is_netfs, as systemd-fstab-generator uses it): such a mount waits for
# the network and hangs from remote-fs.target, never from local-fs.target, and is never fsck'd.
NETFS_TYPES = {"nfs", "nfs4", "cifs", "smb3", "smbfs", "ncpfs", "coda", "ocfs2", "gfs", "gfs2", "ceph", "9p",
               "afs", "glusterfs", "fuse.sshfs", "fuse.glusterfs", "davfs", "fuse.davfs2", "lustre", "pvfs2"}

# The old kernel command line: per-machine arguments carried to host.toml [boot] kernel_params, and the ones that
# belong to the old boot itself (dropped silently). Everything else is reported, not carried.
KARG_CARRY = re.compile(r"(rd\.)?luks\.[a-z.]+=.*|rd\.luks=.*|rd\.vconsole\.(keymap|font)=.*|resume(_offset)?=.*")
KARG_OLD_BOOT = re.compile(r"(BOOT_IMAGE|initrd|ostree|root|rootflags|rootfstype|ro|rw|rd\.auto|systemd\.machine_id)"
                           r"(=.*)?")


def log(msg):
    print(f"{PROG}: {msg}", file=sys.stderr)


class Refused(Exception):
    """A named reason not to act. Nothing was written."""


def matches(kind, value):
    return isinstance(value, str) and re.fullmatch(PATTERNS[kind], value) is not None


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_atomic(path, data, mode=0o644, uid=None, gid=None):
    """Write `data` (str or bytes) to `path` through a temporary file and rename(2). Never follows a symlink at
    `path`: rename replaces the link itself."""
    d = os.path.dirname(path) or "."
    os.makedirs(d, exist_ok=True)
    tmp = os.path.join(d, f".{os.path.basename(path)}.rime-hostlayer.tmp")
    if isinstance(data, str):
        data = data.encode()
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, mode)
    try:
        os.write(fd, data)
        os.fchmod(fd, mode)
        if uid is not None and os.geteuid() == 0:
            os.fchown(fd, uid, gid if gid is not None else -1)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.rename(tmp, path)


# ── host.toml ────────────────────────────────────────────────────────────────


def toml_str(s):
    # TOML basic strings take JSON's escapes (\" \\ \n \t \uXXXX).
    return json.dumps(s, ensure_ascii=True)


def render_host_toml(host):
    out = ["# Rime OS: this machine's own facts (PLAN.md D2). Read by rime-hostlayer at every activation and by its",
           "# mount generator at every boot. `rime-hostlayer host validate` checks an edit.",
           f"schema = {host.get('schema', 1)}", ""]
    h = host.get("host", {})
    if h:
        out.append("[host]")
        for k in ("hostname", "timezone"):
            if h.get(k):
                out.append(f"{k} = {toml_str(h[k])}")
        out.append("")
    loc = host.get("locale", {})
    if loc:
        out.append("[locale]")
        for k in LOCALE_KEYS:
            if loc.get(k):
                out.append(f"{k} = {toml_str(loc[k])}")
        out.append("")
    kb = host.get("keyboard", {})
    if kb:
        out.append("[keyboard]")
        for k in ("console", "console_font", "xkb_layout", "xkb_model", "xkb_variant", "xkb_options"):
            if kb.get(k):
                out.append(f"{k} = {toml_str(kb[k])}")
        out.append("")
    for m in host.get("mount", []):
        out.append("[[mount]]")
        for k in ("what", "where", "type", "options"):
            out.append(f"{k} = {toml_str(m[k])}")
        out.append(f"freq = {int(m.get('freq', 0))}")
        out.append(f"passno = {int(m.get('passno', 0))}")
        out.append("")
    boot = host.get("boot", {})
    if boot:
        out.append("[boot]")
        if boot.get("layout"):
            out.append(f"layout = {toml_str(boot['layout'])}")
        if boot.get("microcode"):
            out.append(f"microcode = {toml_str(boot['microcode'])}")
        if boot.get("kernel_params"):
            out.append("kernel_params = [" + ", ".join(toml_str(k) for k in boot["kernel_params"]) + "]")
        for k in ("device", "fstype"):
            if boot.get(k):
                out.append(f"{k} = {toml_str(boot[k])}")
        out.append("")
    for v in host.get("luks", []):
        out.append("[[luks]]")
        for k in ("name", "device", "keyfile", "options"):
            if v.get(k):
                out.append(f"{k} = {toml_str(v[k])}")
        out.append(f"tpm2 = {'true' if v.get('tpm2') else 'false'}")
        out.append("")
    return "\n".join(out).rstrip("\n") + "\n"


def validate_host(host):
    """Every problem with a parsed host.toml, as text. Empty = valid."""
    errs = []
    if host.get("schema", 1) != 1:
        errs.append(f"schema {host.get('schema')!r} is not 1")
    known = {"schema", "host", "locale", "keyboard", "mount", "luks", "boot"}
    for k in host:
        if k not in known:
            errs.append(f"unknown key {k!r}")
    h = host.get("host", {})
    if not isinstance(h, dict):
        errs.append("[host] is not a table")
        h = {}
    for k, v in h.items():
        if k == "hostname" and not matches("hostname", v):
            errs.append(f"host.hostname {v!r} is not a hostname")
        elif k == "timezone" and (not matches("timezone", v) or ".." in v):
            errs.append(f"host.timezone {v!r} is not a zone name")
        elif k not in ("hostname", "timezone"):
            errs.append(f"unknown key host.{k}")
    for k, v in host.get("locale", {}).items():
        if k not in LOCALE_KEYS:
            errs.append(f"unknown key locale.{k}")
        elif not matches("locale", v):
            errs.append(f"locale.{k} {v!r} is not a locale")
    for k, v in host.get("keyboard", {}).items():
        kind = {"console": "keymap", "console_font": "font"}.get(k, "xkb" if k.startswith("xkb_") else None)
        if kind is None or k not in ("console", "console_font", "xkb_layout", "xkb_model", "xkb_variant",
                                     "xkb_options"):
            errs.append(f"unknown key keyboard.{k}")
        elif not matches(kind, v):
            errs.append(f"keyboard.{k} {v!r} is not valid")
    mounts = host.get("mount", [])
    if not isinstance(mounts, list):
        errs.append("mount is not an array of tables")
        mounts = []
    for i, m in enumerate(mounts):
        where = m.get("where", "")
        if not isinstance(where, str) or not where.startswith("/") or os.path.normpath(where) != where \
                or re.search(r"\s", where):
            errs.append(f"mount[{i}].where {where!r} is not a clean absolute path")
        elif where in OS_MOUNTPOINTS:
            errs.append(f"mount[{i}].where {where} is the operating system's own mount point")
        if not matches("what", m.get("what")):
            errs.append(f"mount[{i}].what {m.get('what')!r} is not a device")
        if not matches("fstype", m.get("type")):
            errs.append(f"mount[{i}].type {m.get('type')!r} is not a filesystem type")
        if not matches("options", m.get("options")):
            errs.append(f"mount[{i}].options {m.get('options')!r} is not an option list")
        for k in ("freq", "passno"):
            if not isinstance(m.get(k, 0), int):
                errs.append(f"mount[{i}].{k} is not an integer")
        for k in m:
            if k not in ("what", "where", "type", "options", "freq", "passno"):
                errs.append(f"unknown key mount[{i}].{k}")
    boot = host.get("boot", {})
    if not isinstance(boot, dict):
        errs.append("[boot] is not a table")
        boot = {}
    for k, v in boot.items():
        if k == "layout":
            if v not in BOOT_LAYOUTS:
                errs.append(f"boot.layout {v!r} is not one of {', '.join(BOOT_LAYOUTS)}")
        elif k == "kernel_params":
            if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
                errs.append("boot.kernel_params is not a list of strings")
            else:
                errs += [f"boot.kernel_params {x!r} has a character the boot menu would expand or split"
                         for x in v if not matches("kernel_param", x)]
        elif k == "device":
            # A migrated machine whose /boot is its own partition (the installer's LUKS layout): the bridge records
            # it here and rime.rootfs's generator mounts it at /boot (nix/modules/rootfs.nix).
            if not matches("what", v):
                errs.append(f"boot.device {v!r} is not a device")
        elif k == "fstype":
            if not matches("fstype", v):
                errs.append(f"boot.fstype {v!r} is not a filesystem type")
        elif k == "microcode":
            if v not in BOOT_MICROCODE:
                errs.append(f"boot.microcode {v!r} is not one of {', '.join(BOOT_MICROCODE)}")
        else:
            errs.append(f"unknown key boot.{k}")
    for i, v in enumerate(host.get("luks", [])):
        for k in ("name", "device"):
            if not matches("what", v.get(k)):
                errs.append(f"luks[{i}].{k} {v.get(k)!r} is not valid")
        if v.get("options") and not matches("options", v["options"]):
            errs.append(f"luks[{i}].options {v['options']!r} is not an option list")
    return errs


def load_host(path=None, required=False):
    path = path or HOST_TOML
    try:
        with open(path, "rb") as f:
            host = tomllib.load(f)
    except FileNotFoundError:
        if required:
            raise Refused(f"{path} does not exist")
        return {}
    errs = validate_host(host)
    if errs:
        raise Refused(f"{path} is not valid: " + "; ".join(errs))
    return host


def cmd_host(a):
    if a.action == "validate":
        try:
            load_host(a.file, required=True)
        except (Refused, tomllib.TOMLDecodeError) as e:
            log(str(e))
            return EXIT_FAIL
        print(f"{a.file or HOST_TOML}: valid")
        return 0
    host = load_host(a.file)
    if a.action == "show":
        print(render_host_toml(host) if host else "# no host.toml: every fact is the system's default", end="")
        return 0
    # get KEY: dotted, e.g. host.hostname, keyboard.xkb_layout
    cur = host
    for part in (a.key or "").split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        else:
            return EXIT_FAIL
    print(json.dumps(cur) if isinstance(cur, (dict, list)) else cur)
    return 0


# ── the mount generator ──────────────────────────────────────────────────────


def unit_escape_path(path):
    """systemd-escape --path."""
    p = re.sub(r"/+", "/", path).strip("/")
    if not p:
        return "-"
    out = []
    for i, ch in enumerate(p):
        if ch == "/":
            out.append("-")
        elif (ch.isascii() and (ch.isalnum() or ch in ":_.")) and not (i == 0 and ch == "."):
            out.append(ch)
        else:
            out.extend(f"\\x{b:02x}" for b in ch.encode())
    return "".join(out)


def device_path(what):
    for tag, d in (("UUID=", "by-uuid"), ("LABEL=", "by-label"), ("PARTUUID=", "by-partuuid"),
                   ("PARTLABEL=", "by-partlabel")):
        if what.startswith(tag):
            return f"/dev/disk/{d}/{what[len(tag):]}"
    return what


def is_netfs(m):
    return m["type"] in NETFS_TYPES or "_netdev" in m["options"].split(",")


def mount_target(m):
    return "remote-fs.target" if is_netfs(m) else "local-fs.target"


def mount_unit(m, source):
    """The text of one .mount unit, and the dependency it hangs from its target (mount_target): local-fs.target,
    or remote-fs.target for a network filesystem, which also waits for the network (systemd-fstab-generator's
    rules; hanging it from local-fs.target would order the network before the local filesystems: a cycle)."""
    opts = m["options"].split(",")
    nofail = "nofail" in opts
    noauto = "noauto" in opts
    netfs = is_netfs(m)
    what = device_path(m["what"])
    lines = ["# Generated by rime-hostlayer-mounts from " + source + " (PLAN.md D2). Options as written there.",
             "[Unit]", f"SourcePath={source}", "Documentation=man:systemd.mount(5)"]
    if netfs:
        lines += ["After=network-online.target", "Wants=network-online.target"]
    if not nofail:
        # Without nofail the boot waits for this mount, as for an fstab line without it.
        lines.append(f"Before={mount_target(m)}")
    if int(m.get("passno", 0)) > 0 and what.startswith("/dev/") and not netfs:
        fsck = f"systemd-fsck@{unit_escape_path(what)}.service"
        lines += [f"Requires={fsck}", f"After={fsck}"]
    lines += ["", "[Mount]", f"What={what}", f"Where={m['where']}", f"Type={m['type']}",
              f"Options={m['options']}", ""]
    dep = None if noauto else ("wants" if nofail else "requires")
    return "\n".join(lines), dep


def cmd_generate_mounts(a):
    """A systemd generator: never fails the boot. A bad host.toml writes no unit and says why."""
    try:
        host = load_host(a.host_toml)
    except (Refused, tomllib.TOMLDecodeError, OSError) as e:
        log(f"no extra mounts: {e}")
        return 0
    for m in host.get("mount", []):
        name = unit_escape_path(m["where"]) + ".mount"
        target = os.path.join(a.normal, name)
        if os.path.lexists(target):
            log(f"{name} is already generated; leaving it")
            continue
        text, dep = mount_unit(m, a.host_toml or HOST_TOML)
        try:
            write_atomic(target, text)
            if dep:
                d = os.path.join(a.normal, f"{mount_target(m)}.{dep}")
                os.makedirs(d, exist_ok=True)
                link = os.path.join(d, name)
                if not os.path.lexists(link):
                    os.symlink(f"../{name}", link)
        except OSError as e:
            log(f"could not write {name}: {e}")
    return 0


# ── apply (activation) ───────────────────────────────────────────────────────

XKB_HEADER = "# Written by rime-hostlayer from /var/lib/rime/host.toml. Change [keyboard] there."
# The system configuration's keyboard (services.xserver.xkb), used when host.toml names no X11 layout.
XKB_DEFAULT = "/etc/rime/hostlayer/00-keyboard.conf"


def managed_by_config(path):
    """A file NixOS's own configuration writes (a link into /etc/static) is not ours to change."""
    try:
        target = os.readlink(path)
    except OSError:
        return False
    return "/etc/static/" in target or target.startswith("/nix/store/")


def put(path, text, notes, mode=0o644):
    if managed_by_config(path):
        notes.append(f"{path}: set by the system configuration, left alone")
        return
    try:
        with open(path) as f:
            if f.read() == text:
                return
    except OSError:
        pass
    write_atomic(path, text, mode)
    notes.append(f"{path}: written")


def cmd_apply(a):
    root = a.root.rstrip("/")
    notes = []
    try:
        host = load_host(a.host_toml or (root + HOST_TOML if root else None))
    except (Refused, tomllib.TOMLDecodeError) as e:
        # Activation must not stop on a bad edit: say so and keep the machine as it is.
        log(f"not applied: {e}")
        return 0
    h, loc, kb = host.get("host", {}), host.get("locale", {}), host.get("keyboard", {})

    etc = root + "/etc"
    hostname = h.get("hostname")
    if hostname is None and not os.path.lexists(etc + "/hostname"):
        hostname = "rime"  # base.nix's name for a machine nobody named
    if hostname:
        put(etc + "/hostname", hostname + "\n", notes)
        if not root and not a.no_kernel:
            try:
                with open("/proc/sys/kernel/hostname") as f:
                    cur = f.read().strip()
                if cur != hostname and not managed_by_config("/etc/hostname"):
                    with open("/proc/sys/kernel/hostname", "w") as f:
                        f.write(hostname)
            except OSError as e:
                notes.append(f"kernel hostname not set: {e}")

    tz = h.get("timezone")
    if tz:
        lt = etc + "/localtime"
        zone = f"/etc/zoneinfo/{tz}"
        if managed_by_config(lt):
            notes.append(f"{lt}: set by the system configuration (time.timeZone), left alone")
        elif not os.path.exists(root + zone):
            notes.append(f"timezone {tz}: no {zone} on this system, left as it is")
        else:
            try:
                cur = os.readlink(lt)
            except OSError:
                cur = None
            if cur != zone:
                tmp = lt + ".rime-hostlayer.tmp"
                if os.path.lexists(tmp):
                    os.unlink(tmp)
                os.symlink(zone, tmp)
                os.rename(tmp, lt)
                notes.append(f"{lt} -> {zone}")

    if loc:
        put(etc + "/locale.conf", "".join(f"{k}={loc[k]}\n" for k in LOCALE_KEYS if loc.get(k)), notes)
    if kb.get("console") or kb.get("console_font"):
        text = (f"KEYMAP={kb['console']}\n" if kb.get("console") else "") + \
               (f"FONT={kb['console_font']}\n" if kb.get("console_font") else "")
        put(etc + "/vconsole.conf", text, notes)

    xkb = etc + "/X11/xorg.conf.d/00-keyboard.conf"
    opts = [(o, kb.get(k)) for o, k in (("XkbLayout", "xkb_layout"), ("XkbModel", "xkb_model"),
                                         ("XkbVariant", "xkb_variant"), ("XkbOptions", "xkb_options")) if kb.get(k)]
    if opts:
        body = "\n".join(f'        Option "{o}" "{v}"' for o, v in opts)
        put(xkb, f'{XKB_HEADER}\nSection "InputClass"\n        Identifier "system-keyboard"\n'
                 f'        MatchIsKeyboard "on"\n{body}\nEndSection\n', notes)
    else:
        # No X11 layout of the machine's own: the system configuration's, still marked as ours so a later
        # host.toml layout replaces it. With no default either, a file this wrote earlier goes.
        try:
            with open(root + XKB_DEFAULT) as f:
                default = f.read()
        except OSError:
            default = None
        if default is not None:
            put(xkb, f"{XKB_HEADER}\n# No xkb_* in host.toml: the system configuration's keyboard.\n{default}", notes)
        else:
            try:
                with open(xkb) as f:
                    if f.readline().rstrip("\n") == XKB_HEADER:
                        os.unlink(xkb)
                        notes.append(f"{xkb}: removed (no [keyboard] xkb_* in host.toml)")
            except OSError:
                pass
    for n in notes:
        log(n)
    return 0


# ── subordinate ids across activations ──────────────────────────────────────


def read_lines(path):
    try:
        with open(path) as f:
            return [ln.rstrip("\n") for ln in f if ln.strip() and not ln.startswith("#")]
    except FileNotFoundError:
        return []


def parse_subid(line):
    p = line.split(":")
    if len(p) != 3 or not p[1].isdigit() or not p[2].isdigit():
        return None
    return p[0], int(p[1]), int(p[2])


def cmd_subid(a):
    """NixOS's update-users-groups rewrites /etc/subuid and /etc/subgid from DECLARED users only, so ranges that
    a carried account (or `usermod --add-subuids`) owns would go at the first activation. `save` runs before it,
    `restore` after: a saved range comes back for an account that still exists and has none from NixOS, unless
    it overlaps a range NixOS handed out (then it is reported, not restored)."""
    root = a.root.rstrip("/")
    stash = root + SUBID_STASH
    if a.action == "save":
        os.makedirs(stash, mode=0o700, exist_ok=True)
        for f in ("subuid", "subgid"):
            write_atomic(f"{stash}/{f}.before", "".join(ln + "\n" for ln in read_lines(f"{root}/etc/{f}")), 0o600)
        return 0
    users = {ln.split(":")[0] for ln in read_lines(f"{root}/etc/passwd")}
    for f in ("subuid", "subgid"):
        before = f"{stash}/{f}.before"
        if not os.path.exists(before):
            continue
        cur = [parse_subid(ln) for ln in read_lines(f"{root}/etc/{f}")]
        cur = [c for c in cur if c]
        have = {c[0] for c in cur}
        add = []
        for ln in read_lines(before):
            e = parse_subid(ln)
            if not e or e[0] in have or e[0] not in users:
                continue
            clash = [c for c in cur + add if c[1] < e[1] + e[2] and e[1] < c[1] + c[2]]
            if clash:
                log(f"/etc/{f}: {e[0]}'s range {e[1]}:{e[2]} overlaps {clash[0][0]}'s {clash[0][1]}:{clash[0][2]}; "
                    f"not restored")
                continue
            add.append(e)
        if add:
            text = "".join(f"{n}:{s}:{c}\n" for n, s, c in cur + add)
            write_atomic(f"{root}/etc/{f}", text, 0o644)
            log(f"/etc/{f}: kept " + ", ".join(f"{n}:{s}:{c}" for n, s, c in add))
    return 0


# ── runtime switches ─────────────────────────────────────────────────────────


def load_registry(path=None):
    """`<unit> <on|off>` per line: the unit (a template ends in `@.service`) and its state when no marker says
    otherwise. Default-off units carry ConditionPathExists=<dir>/%n.on, default-on units
    ConditionPathExists=!<dir>/%n.off."""
    reg = {}
    for ln in read_lines(path or SWITCH_REGISTRY):
        p = ln.split()
        if len(p) == 2 and p[1] in ("on", "off") and matches("unit", p[0]):
            reg[p[0]] = p[1]
    return reg


def resolve_switch(name, reg):
    if not matches("unit", name) or "/" in name or name.startswith("."):
        raise Refused(f"{name!r} is not a unit name")
    if name in reg:
        return name, reg[name]
    if "@" in name:
        tmpl = re.sub(r"@[^.]*\.", "@.", name, count=1)
        inst = name.split("@", 1)[1].rsplit(".", 1)[0]
        if tmpl in reg and inst and re.fullmatch(r"[A-Za-z0-9_.:-]+", inst):
            return name, reg[tmpl]
    raise Refused(f"{name} is not a runtime switch on this system (see {SWITCH_REGISTRY})")


def switch_state(name, default):
    if default == "off":
        return "on" if os.path.exists(f"{SWITCH_DIR}/{name}.on") else "off"
    return "off" if os.path.exists(f"{SWITCH_DIR}/{name}.off") else "on"


def cmd_switch(a):
    reg = load_registry()
    if a.action == "list":
        for name, default in sorted(reg.items()):
            print(f"{name} default={default}")
        for f in sorted(os.listdir(SWITCH_DIR)) if os.path.isdir(SWITCH_DIR) else []:
            print(f"marker {f}")
        return 0
    name, default = resolve_switch(a.name, reg)
    if a.action == "get":
        print(switch_state(name, default))
        return 0
    if os.geteuid() != 0 and not os.environ.get("RIME_SWITCH_DIR"):
        raise Refused("changing a switch needs root (rimed's org.rimeos.Rimed1.Switch asks polkit for you)")
    want = a.action  # on | off
    marker = f"{SWITCH_DIR}/{name}.{'on' if default == 'off' else 'off'}"
    os.makedirs(SWITCH_DIR, mode=0o755, exist_ok=True)
    if (want == "on") == (default == "off"):
        write_atomic(marker, f"{now_utc()} {os.environ.get('SUDO_USER') or os.getuid()}\n", 0o644)
    elif os.path.lexists(marker):
        os.unlink(marker)
    log(f"{name}: {want}")
    if a.now:
        r = subprocess.run(["systemctl", "start" if want == "on" else "stop", name])
        return 0 if r.returncode == 0 else EXIT_FAIL
    return 0


# ── carry ────────────────────────────────────────────────────────────────────


class Src:
    """The old root, read-only. Every path is resolved under it; a symlink is never followed out of it."""

    def __init__(self, root):
        self.root = os.path.abspath(root)

    def p(self, rel):
        return os.path.join(self.root, rel.lstrip("/"))

    def lexists(self, rel):
        return os.path.lexists(self.p(rel))

    def is_file(self, rel):
        try:
            return stat.S_ISREG(os.lstat(self.p(rel)).st_mode)
        except OSError:
            return False

    def is_dir(self, rel):
        try:
            return stat.S_ISDIR(os.lstat(self.p(rel)).st_mode)
        except OSError:
            return False

    def read(self, rel):
        fd = os.open(self.p(rel), os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as f:
            return f.read()

    def text(self, rel, default=None):
        try:
            return self.read(rel).decode()
        except FileNotFoundError:
            if default is None:
                raise
            return default

    def readlink(self, rel):
        try:
            return os.readlink(self.p(rel))
        except OSError:
            return None


def parse_colon(text, nfields):
    out = []
    for ln in text.splitlines():
        if not ln.strip() or ln.startswith("#"):
            continue
        p = ln.split(":")
        if len(p) < nfields:
            p += [""] * (nfields - len(p))
        out.append(p)
    return out


def login_defs(src):
    vals = {"UID_MIN": 1000, "GID_MIN": 1000}
    for ln in src.text("etc/login.defs", "").splitlines():
        p = ln.split()
        if len(p) == 2 and p[0] in vals and p[1].isdigit():
            vals[p[0]] = int(p[1])
    return vals


def plan_accounts(src, declared, system):
    """Decide every account. Returns (plan, refusals). Nothing is written here."""
    passwd = parse_colon(src.text("etc/passwd"), 7)
    group = parse_colon(src.text("etc/group"), 4)
    shadow = {p[0]: ":".join(p) for p in parse_colon(src.text("etc/shadow", ""), 9)}
    gshadow = {p[0]: ":".join(p) for p in parse_colon(src.text("etc/gshadow", ""), 4)}
    subuid = [parse_subid(ln) for ln in src.text("etc/subuid", "").splitlines() if ln.strip()]
    subgid = [parse_subid(ln) for ln in src.text("etc/subgid", "").splitlines() if ln.strip()]
    subuid, subgid = [s for s in subuid if s], [s for s in subgid if s]
    defs = login_defs(src)

    dusers = declared.get("users", {})
    dgroups = declared.get("groups", {})
    static_uid = {u["uid"]: n for n, u in dusers.items() if u.get("uid") is not None}
    static_gid = {g["gid"]: n for n, g in dgroups.items() if g.get("gid") is not None}

    with_subids = {s[0] for s in subuid + subgid}

    def human(uid):
        return defs["UID_MIN"] <= uid < 65534

    refusals, left, notes = [], [], []
    users_out, groups_out = [], []
    pinned_gids = {}
    pinned_users = set()

    # Users first: which of them the machine must keep exactly.
    for p in passwd:
        name, uid = p[0], int(p[2]) if p[2].isdigit() else None
        if uid is None:
            left.append({"kind": "user", "name": name, "why": "unreadable uid"})
            continue
        if human(uid) or name in with_subids:
            pinned_users.add(name)
            if p[3].isdigit():
                pinned_gids[int(p[3])] = name

    gid_new = {}  # old gid -> new gid, for every group carried
    gname_new = {}
    for g in group:
        name = g[0]
        if not g[2].isdigit():
            left.append({"kind": "group", "name": name, "why": "unreadable gid"})
            continue
        gid = int(g[2])
        pinned = gid in pinned_gids
        if name in dgroups:
            want = dgroups[name].get("gid")
            if want is None:
                other = static_gid.get(gid)
                if other and other != name:
                    if pinned:
                        refusals.append(f"group {name} (gid {gid}, primary group of {pinned_gids[gid]}) collides with "
                                        f"the NixOS group {other} (gid {gid})")
                        continue
                    left.append({"kind": "group", "name": name,
                                 "why": f"gid {gid} is the NixOS group {other}'s; NixOS allocates {name} anew"})
                    continue
                want = gid
            elif want != gid:
                if pinned:
                    refusals.append(f"group {name} (gid {gid}, primary group of {pinned_gids[gid]}) is declared by "
                                    f"NixOS with gid {want}")
                    continue
                notes.append(f"group {name}: gid {gid} -> {want} (NixOS's)")
        else:
            other = static_gid.get(gid)
            if other:
                if pinned:
                    refusals.append(f"group {name} (gid {gid}, primary group of {pinned_gids[gid]}) collides with the "
                                    f"NixOS group {other} (gid {gid})")
                    continue
                left.append({"kind": "group", "name": name,
                             "why": f"gid {gid} is the NixOS group {other}'s (an old-image group)"})
                continue
            want = gid
        gid_new[gid] = want
        gname_new[name] = want
        groups_out.append([name, g[1], want, g[3]])

    carried_users = set()
    for p in passwd:
        name = p[0]
        if not p[2].isdigit():
            continue
        uid, gid = int(p[2]), int(p[3]) if p[3].isdigit() else -1
        pinned = name in pinned_users
        if name in dusers:
            if human(uid):
                refusals.append(f"user {name} (uid {uid}) is a person on this machine, and the NixOS system declares "
                                f"an account of the same name")
                continue
            want = dusers[name].get("uid")
            if want is None:
                other = static_uid.get(uid)
                if other and other != name:
                    if pinned:
                        refusals.append(f"user {name} (uid {uid}) collides with the NixOS user {other} (uid {uid})")
                        continue
                    left.append({"kind": "user", "name": name,
                                 "why": f"uid {uid} is the NixOS user {other}'s; NixOS allocates {name} anew"})
                    continue
                want = uid
            elif want != uid:
                if pinned:
                    refusals.append(f"user {name} (uid {uid}) is declared by NixOS with uid {want}")
                    continue
                notes.append(f"user {name}: uid {uid} -> {want} (NixOS's)")
            # NixOS sets a declared user's primary group itself; this only has to be a gid it will replace.
            dg = dusers[name].get("group")
            new_gid = gname_new.get(dg) or (dgroups.get(dg) or {}).get("gid") or gid_new.get(gid, gid)
        else:
            other = static_uid.get(uid)
            if other:
                if pinned:
                    refusals.append(f"user {name} (uid {uid}) collides with the NixOS user {other} (uid {uid})")
                    continue
                left.append({"kind": "user", "name": name,
                             "why": f"uid {uid} is the NixOS user {other}'s (an old-image account)"})
                continue
            want = uid
            new_gid = gid_new.get(gid)
        if new_gid is None:
            if pinned:
                refusals.append(f"user {name} (uid {uid}): its primary group (gid {gid}) cannot be carried")
                continue
            left.append({"kind": "user", "name": name, "why": f"its primary group (gid {gid}) was left behind"})
            continue
        shell, why = map_shell(p[6], system, human(uid))
        if why:
            notes.append(f"user {name}: {why}")
        users_out.append([name, p[1], want, new_gid, p[4], p[5], shell])
        carried_users.add(name)

    # Members: only accounts that exist after the carry (carried, or declared by NixOS).
    known = carried_users | set(dusers)
    for g in groups_out:
        members = [m for m in g[3].split(",") if m]
        keep = [m for m in members if m in known]
        dropped = [m for m in members if m not in known]
        if dropped:
            notes.append(f"group {g[0]}: members {','.join(dropped)} dropped (accounts not carried)")
        g[3] = ",".join(keep)

    carried_groups = {g[0] for g in groups_out}
    gshadow_out = []
    for g in groups_out:
        line = gshadow.get(g[0])
        if line:
            gp = line.split(":")
            gp[3] = g[3]
            gshadow_out.append(":".join(gp[:4]))
    plan = {
        "passwd": [":".join(str(x) for x in u) for u in users_out],
        "group": [":".join(str(x) for x in g) for g in groups_out],
        "shadow": [shadow[u[0]] for u in users_out if u[0] in shadow],
        "gshadow": gshadow_out,
        "subuid": [f"{n}:{s}:{c}" for n, s, c in subuid if n in carried_users],
        "subgid": [f"{n}:{s}:{c}" for n, s, c in subgid if n in carried_users],
        "uids": {u[0]: u[2] for u in users_out},
        "gids": {g[0]: g[2] for g in groups_out},
        "people": sorted(n for n in carried_users if human(int(next(u[2] for u in users_out if u[0] == n)))),
        "left": left,
        "notes": notes,
        "groups_carried": sorted(carried_groups),
    }
    for kind, lines in (("subuid", subuid), ("subgid", subgid)):
        for n, s, c in lines:
            if n not in carried_users:
                left.append({"kind": kind, "name": n, "why": f"range {s}:{c} belongs to an account not carried"})
    return plan, refusals


def map_shell(shell, system, is_human):
    """A login shell NixOS has, as /run/current-system/sw/bin/<name>. Returns (shell, note or None)."""
    if not shell:
        return shell, None
    base = os.path.basename(shell)
    if shell.startswith(SW + "/bin/") or shell.startswith("/run/wrappers/bin/"):
        return shell, None
    if os.path.lexists(os.path.join(system, "sw/bin", base)):
        new = f"{SW}/bin/{base}"
        return new, (f"login shell {shell} -> {new}" if new != shell else None)
    if is_human:
        return f"{SW}/bin/bash", f"login shell {shell} is not on this system; set to bash"
    return f"{SW}/bin/nologin", f"login shell {shell} is not on this system; set to nologin"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


# Configuration directories whose files NixOS builds from the system configuration: a file written there on the
# old machine cannot be carried as a file (on NixOS /etc/udev/rules.d is one store directory). The report says so
# and names the option to port it to.
HOST_LOCAL_WHY = {
    "etc/udev/rules.d": "a udev rule written on this machine: not carried (NixOS builds /etc/udev/rules.d from "
                        "the system configuration; port it there, services.udev)",
    "etc/sysctl.d": "a kernel setting written on this machine: not carried (port it to the system "
                    "configuration, boot.kernel.sysctl)",
    "etc/modprobe.d": "module options written on this machine: not carried (port them to the system "
                      "configuration, boot.extraModprobeConfig)",
    "etc/modules-load.d": "a module list written on this machine: not carried (port it to the system "
                          "configuration, boot.kernelModules)",
    "etc/tmpfiles.d": "a tmpfiles rule written on this machine: not carried (port it to the system "
                      "configuration, systemd.tmpfiles.rules)",
}


def settings_of(data):
    """A udev rule or sysctl file's settings: its non-comment lines, whitespace around = and between tokens
    folded. Two files with the same settings and different comments are the same configuration."""
    out = []
    for ln in data.decode(errors="replace").splitlines():
        ln = ln.strip()
        if not ln or ln.startswith(("#", ";")):
            continue
        out.append(re.sub(r"\s*=\s*", "=", re.sub(r"\s+", " ", ln)))
    return out


class Carry:
    def __init__(self, a):
        self.src = Src(a.src)
        self.dest = os.path.abspath(a.root)
        self.system = os.path.abspath(a.system)
        self.plan_only = a.plan
        self.report = {"from": self.src.root, "to": self.dest, "system": self.system, "at": now_utc(),
                       "carried": [], "host_toml": {}, "checked": [], "left_behind": [], "accounts": {},
                       "not_copied_private_keys": [], "switches": []}
        self.copied = set()  # source paths (relative) carried or folded in
        self.uids, self.gids = {}, {}
        dpath = a.declared or os.path.join(self.system, "etc/rime/hostlayer/declared-accounts.json")
        with open(dpath) as f:
            self.declared = json.load(f)
        self.image_etc = a.image_etc or self.src.p("usr/etc")
        self.cmdline = a.cmdline
        self.cpuinfo = a.cpuinfo

    def d(self, rel):
        return os.path.join(self.dest, rel.lstrip("/"))

    def owner(self, user, group):
        return self.uids.get(user, 0), self.gids.get(group, 0)

    def put_file(self, rel_src, rel_dest, mode, user="root", group="root", data=None):
        if data is None:
            data = self.src.read(rel_src)
        self.report["carried"].append({"from": "/" + rel_src, "to": "/" + rel_dest.lstrip("/"),
                                       "sha256": sha256(data)})
        if rel_src:
            self.copied.add(rel_src)
        if self.plan_only:
            return
        uid, gid = self.owner(user, group)
        write_atomic(self.d(rel_dest), data, mode, uid, gid)

    def put_tree(self, rel_src, rel_dest, user_map=True):
        """Copy a directory tree: regular files and directories, modes kept, owners mapped by NAME through the new
        tables (an owner that was not carried becomes root). Symlinks and special files are reported, not copied."""
        base = self.src.p(rel_src)
        if not self.src.is_dir(rel_src):
            return
        old_users = {int(p[2]): p[0] for p in parse_colon(self.src.text("etc/passwd"), 7) if p[2].isdigit()}
        old_groups = {int(g[2]): g[0] for g in parse_colon(self.src.text("etc/group"), 4) if g[2].isdigit()}
        for dirpath, dirnames, filenames in os.walk(base, followlinks=False):
            rel = os.path.relpath(dirpath, self.src.root)
            st = os.lstat(dirpath)
            out = self.d(os.path.join(rel_dest, os.path.relpath(dirpath, base)))
            if not self.plan_only:
                os.makedirs(out, exist_ok=True)
                os.chmod(out, stat.S_IMODE(st.st_mode))
                if os.geteuid() == 0:
                    os.chown(out, self.uids.get(old_users.get(st.st_uid), 0), self.gids.get(old_groups.get(st.st_gid), 0))
            for fn in filenames + [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]:
                srel = os.path.join(rel, fn)
                st = os.lstat(self.src.p(srel))
                if not stat.S_ISREG(st.st_mode):
                    self.report["left_behind"].append({"path": "/" + srel, "why": "not a regular file"})
                    continue
                data = self.src.read(srel)
                self.copied.add(srel)
                self.report["carried"].append({"from": "/" + srel,
                                               "to": "/" + os.path.join(rel_dest, os.path.relpath(srel, rel_src)),
                                               "sha256": sha256(data)})
                if self.plan_only:
                    continue
                uid = self.uids.get(old_users.get(st.st_uid), 0)
                gid = self.gids.get(old_groups.get(st.st_gid), 0)
                write_atomic(os.path.join(out, fn), data, stat.S_IMODE(st.st_mode), uid, gid)
        self.copied.add(rel_src)

    # ── preflight ──
    def preflight(self):
        if not self.src.is_file("etc/passwd"):
            raise Refused(f"{self.src.p('etc/passwd')} is missing; --from must be the old system's root")
        for rel in ("etc/apex", "etc/apex-greet", "var/lib/apex", "var/lib/apex-greet"):
            if self.src.is_dir(rel):
                raise Refused(f"/{rel} is still a directory: rime-migrate-from-apex has not finished on the old "
                              f"system (boot it once more, then carry)")
        if os.path.exists(self.d("var/lib/rime/carry/done")) and not self.force:
            raise Refused(f"{self.d('var/lib/rime/carry/done')} exists: this root was already carried into "
                          f"(--force to carry again)")
        if not os.path.isdir(os.path.join(self.system, "sw/bin")):
            raise Refused(f"{self.system} is not a NixOS system (no sw/bin)")

    # ── accounts ──
    def accounts(self):
        plan, refusals = plan_accounts(self.src, self.declared, self.system)
        if refusals:
            raise Refused("account collisions with the NixOS system:\n  " + "\n  ".join(refusals))
        self.uids, self.gids = plan["uids"], plan["gids"]
        self.report["accounts"] = {k: plan[k] for k in ("people", "notes", "groups_carried")}
        self.report["accounts"]["users_carried"] = sorted(plan["uids"])
        for e in plan["left"]:
            self.report["left_behind"].append({"path": f"{e['kind']}:{e['name']}", "why": e["why"]})
        shadow_gid = self.gids.get("shadow", 0)
        for f, mode, gid in (("passwd", 0o644, 0), ("group", 0o644, 0), ("shadow", 0o640, shadow_gid),
                             ("gshadow", 0o640, shadow_gid), ("subuid", 0o644, 0), ("subgid", 0o644, 0)):
            data = "".join(ln + "\n" for ln in plan[f])
            self.report["carried"].append({"from": f"/etc/{f}", "to": f"/etc/{f}", "sha256": sha256(data.encode()),
                                           "lines": len(plan[f])})
            self.copied.add(f"etc/{f}")
            if not self.plan_only:
                write_atomic(self.d(f"etc/{f}"), data, mode, 0, gid)

    # ── host.toml ──
    def host(self):
        host = {"schema": 1}
        h = {}
        hn = self.src.text("etc/hostname", "").strip()
        if hn:
            h["hostname"] = hn
            self.copied.add("etc/hostname")
        lt = self.src.readlink("etc/localtime")
        if lt and "zoneinfo/" in lt:
            h["timezone"] = lt.split("zoneinfo/", 1)[1]
            self.copied.add("etc/localtime")
        if h:
            host["host"] = h
        loc = {}
        for ln in self.src.text("etc/locale.conf", "").splitlines():
            k, _, v = ln.partition("=")
            if k.strip() in LOCALE_KEYS and v.strip():
                loc[k.strip()] = v.strip().strip('"')
        if loc:
            host["locale"] = loc
            self.copied.add("etc/locale.conf")
        kb = {}
        for ln in self.src.text("etc/vconsole.conf", "").splitlines():
            k, _, v = ln.partition("=")
            v = v.strip().strip('"')
            if k.strip() == "KEYMAP" and v:
                kb["console"] = v
            elif k.strip() == "FONT" and v:
                kb["console_font"] = v
        if self.src.lexists("etc/vconsole.conf"):
            self.copied.add("etc/vconsole.conf")
        xk = "etc/X11/xorg.conf.d/00-keyboard.conf"
        for ln in self.src.text(xk, "").splitlines():
            m = re.match(r'\s*Option\s+"(Xkb\w+)"\s+"([^"]*)"', ln)
            if m and m.group(1) in ("XkbLayout", "XkbModel", "XkbVariant", "XkbOptions"):
                kb["xkb_" + m.group(1)[3:].lower()] = m.group(2)
        if self.src.lexists(xk):
            self.copied.add(xk)
        if kb:
            host["keyboard"] = kb
        mounts = []
        for ln in self.src.text("etc/fstab", "").splitlines():
            p = ln.split()
            if not p or p[0].startswith("#") or len(p) < 4:
                continue
            what, where, fstype, opts = p[:4]
            freq = int(p[4]) if len(p) > 4 and p[4].isdigit() else 0
            passno = int(p[5]) if len(p) > 5 and p[5].isdigit() else 0
            if fstype == "swap" or where == "none":
                self.report["left_behind"].append({"path": f"fstab:{what}", "why": "swap: not carried (TODO D2)"})
                continue
            if where in OS_MOUNTPOINTS or where.startswith("/sysroot"):
                continue  # the NixOS system's own fileSystems
            mounts.append({"what": what, "where": where, "type": fstype, "options": opts, "freq": freq,
                           "passno": passno})
        if mounts:
            host["mount"] = mounts
        self.copied.add("etc/fstab")
        luks = []
        for ln in self.src.text("etc/crypttab", "").splitlines():
            p = ln.split()
            if not p or p[0].startswith("#") or len(p) < 2:
                continue
            opts = p[3] if len(p) > 3 else ""
            luks.append({"name": p[0], "device": p[1], "keyfile": p[2] if len(p) > 2 and p[2] != "none" else "",
                         "options": opts, "tpm2": "tpm2-device" in opts})
        params = self.params
        boot = {}
        if self.microcode:
            boot["microcode"] = self.microcode
        if params:
            boot["kernel_params"] = params
        if boot:
            host["boot"] = boot
        if params:
            have = {v["device"].split("=", 1)[-1].lower() for v in luks}
            for uuid, v in self.luks_from_args(params).items():
                if uuid not in have:
                    luks.append(v)
        if luks:
            host["luks"] = luks
            if self.src.lexists("etc/crypttab"):
                self.copied.add("etc/crypttab")
        errs = validate_host(host)
        if errs:
            raise Refused("the old system's facts do not make a valid host.toml: " + "; ".join(errs))
        self.report["host_toml"] = host
        text = render_host_toml(host)
        self.report["carried"].append({"from": "(facts)", "to": "/var/lib/rime/host.toml",
                                       "sha256": sha256(text.encode())})
        if not self.plan_only:
            write_atomic(self.d("var/lib/rime/host.toml"), text, 0o644)

    def kernel_args(self):
        """The old kernel command line's per-machine arguments (PLAN.md D2: the LUKS volume and its unlock options,
        the console keymap the passphrase prompt uses, resume) for host.toml [boot] kernel_params. bootc keeps them
        as kernel arguments (the installer's `bootc install --karg`), not in /etc/crypttab. A LUKS argument the
        boot menu cannot hold is refused by name: without it the machine would not unlock."""
        if not self.cmdline:
            self.report["checked"].append({"path": "kernel command line", "result": "not given (--cmdline): "
                                           "no kernel arguments carried"})
            return []
        try:
            with open(self.cmdline) as f:
                args = f.read().split()
        except OSError as e:
            raise Refused(f"cannot read the old kernel command line {self.cmdline}: {e}")
        carried, other = [], []
        for arg in args:
            if KARG_CARRY.fullmatch(arg):
                if not matches("kernel_param", arg):
                    raise Refused(f"the kernel argument {arg!r} cannot be written into a boot entry (a character "
                                  f"GRUB would expand or split); without it this machine may not unlock")
                if arg not in carried:
                    carried.append(arg)
            elif not KARG_OLD_BOOT.fullmatch(arg) and arg not in other:
                other.append(arg)
        self.report["checked"].append({"path": "kernel command line", "result": (
            "carried to host.toml [boot] kernel_params: " + (" ".join(carried) or "none")
            + "; not carried (the image's own, or port them to the system configuration): "
            + (" ".join(other) or "none"))})
        return carried

    def cpu_microcode(self):
        """host.toml [boot] microcode from the CPU this carry runs on (the machine being moved): its vendor_id, as the
        installer decides it (installer/rime-install-nixos.sh). A bridged machine then loads one vendor's early
        microcode, not both (rime-boot MICROCODE; docs/nixos/boot-matrix.md). An unknown vendor or an unreadable
        file writes nothing: rime-boot's default, both vendors, is the safe answer."""
        why = f"no vendor_id in {self.cpuinfo}"
        try:
            with open(self.cpuinfo) as f:
                text = f.read()
        except OSError as e:
            why = f"cannot read {self.cpuinfo}: {e}"
            text = ""
        m = re.search(r"^vendor_id\s*:\s*(\S+)", text, re.M)
        vendor = m.group(1) if m else None
        mc = CPU_MICROCODE.get(vendor)
        if mc:
            result = f"vendor_id {vendor}: host.toml [boot] microcode = {mc} (only that vendor's early microcode)"
        elif vendor:
            result = f"vendor_id {vendor} is neither Intel's nor AMD's: no [boot] microcode (both vendors load)"
        else:
            result = why + ": no [boot] microcode (both vendors load)"
        self.report["checked"].append({"path": "CPU vendor", "result": result})
        return mc

    @staticmethod
    def luks_from_args(params):
        """rd.luks.uuid= / rd.luks.name= / rd.luks.options= as host.toml [[luks]] entries, keyed by UUID."""
        out, opts_all, opts_by = {}, "", {}
        for p in params:
            k, _, v = p.partition("=")
            k = k.removeprefix("rd.")
            if k == "luks.uuid":
                uuid = v.removeprefix("UUID=").removeprefix("luks-").lower()
                out.setdefault(uuid, f"luks-{uuid}")
            elif k == "luks.name" and "=" in v:
                uuid, name = v.split("=", 1)
                out[uuid.removeprefix("UUID=").lower()] = name
            elif k == "luks.options":
                # systemd-cryptsetup-generator: "<uuid>=<options>" names one volume, anything else is for all.
                m = re.fullmatch(r"(?:UUID=)?([0-9a-fA-F]{8}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?"
                                 r"[0-9a-fA-F]{12})=(.*)", v)
                if m:
                    opts_by[m.group(1).lower()] = m.group(2)
                else:
                    opts_all = v
        return {u: {"name": n, "device": f"UUID={u}", "keyfile": "", "options": opts_by.get(u, opts_all),
                    "tpm2": "tpm2-device" in opts_by.get(u, opts_all)} for u, n in out.items()}

    # ── files ──
    def files(self):
        s = self.src
        if s.is_file("etc/machine-id"):
            mid = s.text("etc/machine-id").strip()
            if re.fullmatch(r"[0-9a-f]{32}", mid):
                self.put_file("etc/machine-id", "etc/machine-id", 0o444, data=(mid + "\n").encode())
            else:
                self.report["left_behind"].append({"path": "/etc/machine-id", "why": "not a machine id"})
        nm = "etc/NetworkManager/system-connections"
        if s.is_dir(nm):
            for fn in sorted(os.listdir(s.p(nm))):
                if s.is_file(f"{nm}/{fn}"):
                    self.put_file(f"{nm}/{fn}", f"{nm}/{fn}", 0o600)
                    self.profile_files(f"{nm}/{fn}")
            self.copied.add(nm)
        if s.is_dir("etc/ssh"):
            for fn in sorted(os.listdir(s.p("etc/ssh"))):
                if re.fullmatch(r"ssh_host_\w+_key(\.pub)?", fn) and s.is_file(f"etc/ssh/{fn}"):
                    self.put_file(f"etc/ssh/{fn}", f"etc/ssh/{fn}", 0o644 if fn.endswith(".pub") else 0o600)
        self.put_tree("var/lib/bluetooth", "var/lib/bluetooth")
        # CUPS on NixOS: /etc/cups is a link to /var/lib/cups, and the service writes its own cupsd.conf and
        # cups-files.conf from the configuration. Only the machine's printers and their options are carried.
        for fn in ("printers.conf", "classes.conf", "subscriptions.conf", "lpoptions"):
            if s.is_file(f"etc/cups/{fn}"):
                self.put_file(f"etc/cups/{fn}", f"var/lib/cups/{fn}",
                              stat.S_IMODE(os.lstat(s.p(f"etc/cups/{fn}")).st_mode), "root", "lp")
        for d in ("ppd", "ssl"):
            self.put_tree(f"etc/cups/{d}", f"var/lib/cups/{d}")
        # Rime's own state that lived under /etc (D2: it moves to /var/lib/rime).
        if s.is_file("etc/rime/guest-accounts"):
            self.put_file("etc/rime/guest-accounts", "var/lib/rime/guest-accounts", 0o644)
        self.put_tree("etc/rime/firewall.d", "var/lib/rime/firewall.d")
        if s.is_file("etc/sing-box/config.json"):
            self.put_file("etc/sing-box/config.json", "var/lib/rime/sing-box/config.json", 0o600)
        # The session choice: the greeter's own state (D2: next to last-desktop).
        for fn in ("last-desktop", "last-session", "last-user"):
            if s.is_file(f"var/lib/rime-greet/{fn}"):
                st = os.lstat(s.p(f"var/lib/rime-greet/{fn}"))
                owner = {int(p[2]): p[0] for p in parse_colon(s.text("etc/passwd"), 7) if p[2].isdigit()}
                user = owner.get(st.st_uid, "root")
                self.put_file(f"var/lib/rime-greet/{fn}", f"var/lib/rime-greet/{fn}", 0o644,
                              user if user in self.uids else "root", user if user in self.gids else "root")
        g = "etc/greetd/config.toml"
        if s.is_file(g):
            shipped = os.environ.get("RIME_GREETD_DEFAULT", GREETD_DEFAULT)
            same = os.path.isfile(shipped) and open(shipped, "rb").read() == s.read(g)
            if same:
                self.copied.add(g)
                self.report["checked"].append({"path": "/" + g, "result": "the shipped default; nothing to carry"})
            else:
                self.report["checked"].append({"path": "/" + g, "result": "customised: NOT carried. NixOS's greeter "
                                               "config is the system's; a kiosk or autologin must be configured "
                                               "again (TODO(handover))"})
                self.put_file(g, "var/lib/rime/carry/archive/etc/greetd/config.toml", 0o600)
                self.copied.discard(g)

    # The keyfile sections whose values may name a file the profile needs to come up: an OpenVPN profile's ca=, cert=,
    # key=, ta=, static-key= (nmcli import writes them under ~/.cert/nm-openvpn, a hand-made one often under
    # /etc/openvpn), an 802.1X profile's ca-cert=, client-cert=, private-key=.
    PROFILE_FILE_SECTIONS = ("vpn", "802-1x")

    def profile_files(self, rel):
        """The files a carried NetworkManager profile names (p2-gates.md H3). A profile carried without them does not
        come up. Each is carried to the same path, with its mode and owner (by name), unless the new root already
        holds the same bytes there (a file in a home on the same disk). One named but missing is reported."""
        import configparser
        cp = configparser.ConfigParser(interpolation=None, strict=False, delimiters=("=",), comment_prefixes=("#", ";"))
        cp.optionxform = str
        try:
            cp.read_string(self.src.read(rel).decode("utf-8", "replace"))
        except configparser.Error as e:
            self.report["left_behind"].append({"path": "/" + rel, "why": f"not read for the files it names: {e}"})
            return
        old_users = {int(p[2]): p[0] for p in parse_colon(self.src.text("etc/passwd", ""), 7) if p[2].isdigit()}
        old_groups = {int(g[2]): g[0] for g in parse_colon(self.src.text("etc/group", ""), 4) if g[2].isdigit()}
        for sec in self.PROFILE_FILE_SECTIONS:
            if not cp.has_section(sec):
                continue
            for key, val in cp.items(sec):
                path = val.strip()
                if path.startswith("file://"):
                    path = path[len("file://"):]
                if not path.startswith("/") or "\n" in path:
                    continue
                frel = os.path.normpath(path).lstrip("/")
                if frel.startswith("..") or frel in self.copied:
                    continue
                if not self.src.is_file(frel):
                    self.report["left_behind"].append({"path": path, "why": f"named by /{rel} ([{sec}] {key}) but "
                                                       "not a file on the old machine: the profile will not come up "
                                                       "until it is put back"})
                    continue
                data = self.src.read(frel)
                try:
                    with open(self.d(frel), "rb") as f:
                        if f.read() == data:
                            self.copied.add(frel)
                            self.report["checked"].append({"path": path, "result": f"named by /{rel}: already on "
                                                           "this disk, the same bytes"})
                            continue
                except OSError:
                    pass
                st = os.lstat(self.src.p(frel))
                user, group = old_users.get(st.st_uid, "root"), old_groups.get(st.st_gid, "root")
                self.put_file(frel, frel, stat.S_IMODE(st.st_mode), user if user in self.uids else "root",
                              group if group in self.gids else "root", data=data)

    def switches(self):
        """bootc enabled a guest's wipe unit with `systemctl enable` (a wants link). NixOS cannot: it is a switch."""
        d = "etc/systemd/system"
        if not self.src.is_dir(d):
            return
        for wants in sorted(os.listdir(self.src.p(d))):
            m = re.fullmatch(r"user-runtime-dir@(\d+)\.service\.wants", wants)
            if not m:
                continue
            for fn in os.listdir(self.src.p(f"{d}/{wants}")):
                if fn == f"rime-guest-session@{m.group(1)}.service":
                    marker = f"var/lib/rime/switch/{fn}.on"
                    self.report["switches"].append({"unit": fn, "state": "on", "from": f"/{d}/{wants}/{fn}"})
                    self.copied.add(f"{d}/{wants}/{fn}")
                    if not self.plan_only:
                        write_atomic(self.d(marker), f"{now_utc()} carried\n", 0o644)

    def checks(self):
        s = self.src
        if s.lexists("etc/npmrc"):
            keys = sorted({ln.split("=")[0].strip() for ln in s.text("etc/npmrc", "").splitlines()
                           if "=" in ln and not ln.lstrip().startswith((";", "#"))})
            self.report["checked"].append({"path": "/etc/npmrc", "result": "not carried; keys set there: "
                                           + (", ".join(keys) or "none")})
            self.copied.add("etc/npmrc")
        if s.lexists("etc/hypr"):
            self.report["checked"].append({"path": "/etc/hypr", "result": "not carried: nothing reads /etc/hypr "
                                           "(rime-shell-firstrun)"})
            self.copied.add("etc/hypr")
        else:
            self.report["checked"].append({"path": "/etc/hypr", "result": "absent"})

    def left_behind(self):
        """Every other file of the old /etc (and the /var/lib this tool knows), unless it equals the image's
        default. Private keys are named, never read."""
        img = self.image_etc if os.path.isdir(self.image_etc) else None
        etc = self.src.p("etc")
        for dirpath, dirnames, filenames in os.walk(etc, followlinks=False):
            rel_dir = os.path.relpath(dirpath, self.src.root)
            if any(rel_dir == c or rel_dir.startswith(c + "/") for c in self.copied):
                dirnames[:] = []
                continue
            dirnames.sort()
            for fn in sorted(filenames) + [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]:
                rel = os.path.join(rel_dir, fn)
                if rel in self.copied:
                    continue
                full = self.src.p(rel)
                st = os.lstat(full)
                private = (re.search(r"(^|/)private(/|$)", rel_dir) is not None
                           or fn.endswith((".priv", ".key")) and not fn.startswith("ssh_host_"))
                if private and stat.S_ISREG(st.st_mode):
                    self.report["not_copied_private_keys"].append("/" + rel)
                    continue
                shipped = self.shipped_by_system(rel, st)
                if shipped == "same":
                    self.report["checked"].append({"path": "/" + rel, "result": "the new system ships the same "
                                                   "settings: nothing to carry"})
                    continue
                if img:
                    other = os.path.join(img, os.path.relpath(rel, "etc"))
                    try:
                        if stat.S_ISLNK(st.st_mode):
                            if os.path.islink(other) and os.readlink(other) == os.readlink(full):
                                continue
                        elif stat.S_ISREG(st.st_mode) and os.path.isfile(other) and \
                                open(other, "rb").read() == self.src.read(rel):
                            continue
                    except OSError:
                        pass
                why = "differs from the image default" if img else "no image default to compare with"
                host_local = next((w for d, w in HOST_LOCAL_WHY.items() if rel_dir == d), None)
                if shipped == "differs":
                    why = "the new system ships its own file of this name; this machine's version is not carried"
                elif host_local and stat.S_ISREG(st.st_mode):
                    why = host_local
                elif rel_dir.startswith("etc/systemd/system") and stat.S_ISREG(st.st_mode) and fn.endswith(
                        (".service", ".timer", ".socket", ".mount", ".path")):
                    why = "a unit written on this machine: not carried, port it to the system configuration"
                elif stat.S_ISLNK(st.st_mode):
                    why = f"symlink -> {os.readlink(full)}"
                self.report["left_behind"].append({"path": "/" + rel, "why": why})

    def shipped_by_system(self, rel, st):
        """"same" when the new system's /etc has this file with the same settings (comments and blank lines aside,
        for the configuration directories in HOST_LOCAL_WHY), "differs" when it has its own, else None."""
        if not stat.S_ISREG(st.st_mode):
            return None
        mine = os.path.join(self.system, "etc", os.path.relpath(rel, "etc"))
        try:
            with open(mine, "rb") as f:
                theirs = f.read()
        except OSError:
            return None
        ours = self.src.read(rel)
        if os.path.dirname(rel) in HOST_LOCAL_WHY:
            return "same" if settings_of(theirs) == settings_of(ours) else "differs"
        return "same" if theirs == ours else "differs"

    def write_report(self):
        rep = self.report
        txt = [f"rime-hostlayer carry {rep['at']}", f"from {rep['from']}  to {rep['to']}", "",
               f"CARRIED ({len(rep['carried'])})"]
        txt += [f"  {c['from']} -> {c['to']}" for c in rep["carried"]]
        txt += ["", "INTO host.toml", "  " + json.dumps(rep["host_toml"], sort_keys=True)]
        txt += ["", "ACCOUNTS", f"  people: {', '.join(rep['accounts'].get('people', []))}"]
        txt += [f"  {n}" for n in rep["accounts"].get("notes", [])]
        txt += ["", "SWITCHES"] + [f"  {s['unit']} {s['state']} (from {s['from']})" for s in rep["switches"]]
        txt += ["", "CHECKED"] + [f"  {c['path']}: {c['result']}" for c in rep["checked"]]
        txt += ["", f"PRIVATE KEYS NOT COPIED ({len(rep['not_copied_private_keys'])})"]
        txt += [f"  {p}" for p in rep["not_copied_private_keys"]]
        groups = {}
        for e in rep["left_behind"]:
            top = e["path"].split("/")[2] if e["path"].startswith("/etc/") else e["path"].split(":")[0]
            groups.setdefault(top, []).append(e)
        txt += ["", f"LEFT BEHIND ({len(rep['left_behind'])}; every entry is in report.json)"]
        for top in sorted(groups):
            es = groups[top]
            special = [e for e in es if not e["why"].startswith(("no image default", "differs"))]
            txt.append(f"  {top}: {len(es)}")
            txt += [f"    {e['path']}: {e['why']}" for e in special]
        text = "\n".join(txt) + "\n"
        if self.plan_only:
            sys.stdout.write(text)
            return
        write_atomic(self.d("var/lib/rime/carry/report.json"), json.dumps(rep, indent=1, sort_keys=True) + "\n",
                     0o600)
        write_atomic(self.d("var/lib/rime/carry/report.txt"), text, 0o600)
        write_atomic(self.d("var/lib/rime/carry/done"), rep["at"] + "\n", 0o644)
        sys.stdout.write(text)

    def run(self, force=False):
        self.force = force
        self.preflight()
        self.params = self.kernel_args()  # refuses before anything is written
        self.microcode = self.cpu_microcode()
        self.accounts()  # likewise
        self.host()
        self.files()
        self.switches()
        self.checks()
        self.left_behind()
        self.write_report()
        return 0


def cmd_carry(a):
    if os.path.realpath(a.src) == os.path.realpath(a.root):
        raise Refused("--from and --root are the same directory")
    return Carry(a).run(force=a.force)


# ── main ─────────────────────────────────────────────────────────────────────


def main(argv=None):
    ap = argparse.ArgumentParser(prog=PROG, description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("carry")
    c.add_argument("--from", dest="src", required=True)
    c.add_argument("--root", required=True)
    c.add_argument("--system", required=True)
    c.add_argument("--declared")
    c.add_argument("--image-etc")
    c.add_argument("--cmdline")
    # the machine's CPU (host.toml [boot] microcode); the default is the running one, which is the machine moved
    c.add_argument("--cpuinfo", default="/proc/cpuinfo")
    c.add_argument("--plan", action="store_true")
    c.add_argument("--force", action="store_true")
    h = sub.add_parser("host")
    h.add_argument("action", choices=["show", "get", "validate"])
    h.add_argument("key", nargs="?")
    h.add_argument("--file")
    g = sub.add_parser("generate-mounts")
    g.add_argument("normal")
    g.add_argument("early", nargs="?")
    g.add_argument("late", nargs="?")
    g.add_argument("--host-toml")
    ap_ = sub.add_parser("apply")
    ap_.add_argument("--root", default="")
    ap_.add_argument("--host-toml")
    ap_.add_argument("--no-kernel", action="store_true")
    s = sub.add_parser("subid")
    s.add_argument("action", choices=["save", "restore"])
    s.add_argument("--root", default="")
    w = sub.add_parser("switch")
    w.add_argument("action", choices=["list", "get", "on", "off"])
    w.add_argument("name", nargs="?")
    w.add_argument("--now", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "switch" and a.action != "list" and not a.name:
        ap.error("switch get|on|off needs a unit name")
    if a.cmd == "host" and a.action == "get" and not a.key:
        ap.error("host get needs a key")
    fn = {"carry": cmd_carry, "host": cmd_host, "generate-mounts": cmd_generate_mounts, "apply": cmd_apply,
          "subid": cmd_subid, "switch": cmd_switch}[a.cmd]
    try:
        return fn(a)
    except Refused as e:
        log(f"refusing: {e}")
        return EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
