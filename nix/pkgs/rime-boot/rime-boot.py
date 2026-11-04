#!/usr/bin/env python3
"""rime-boot: Rime's boot.loader.external hook (PLAN.md D3 v1 chain, D9, decision A8).

The chain is Fedora's Microsoft-signed shim -> Fedora-signed GRUB -> a MOK-signed NixOS kernel. GRUB and shim are
installed by the installer (fresh machines) or are bootc's (migrated machines); this hook never installs, updates or
registers a boot loader. It never runs bootctl, grub-install, grub2-* or efibootmgr and never writes NVRAM.

What it does, on every `switch-to-configuration boot|switch` (NixOS calls it with the new toplevel as $1) and on
`rime-boot install` (rime gc / rime rollback, D6 section 8.7):

  1. reads the system profile's generations (/nix/var/nix/profiles/system-<N>-link), keeps the newest
     `configurationLimit` plus the profile's current generation, the booted one and every pinned one;
  2. copies each kept generation's kernel (realpath of <toplevel>/kernel, so a MOK-signed image that the kernel
     module links there is what boots), the early-microcode cpios its bootspec names (org.rimeos.microcode.v1,
     only the vendor(s) host.toml [boot] microcode asks for) and its initrd into <boot>/rime/nixos/ under
     content-addressed names (<sha256>-<store name>): a microcode file is stored ONCE for every generation that
     shares it, and each entry lists it as its own initrd before the generation's initrd;
     when the files do not fit above the floor, the oldest generations that are not current, booted or pinned
     are left out of the menu until they do (making room first, see "Space"); it refuses, writing nothing, only
     when the current, booted and pinned generations alone do not fit;
  3. writes the menu for the machine's layout:
       bls         fresh installs (the FAT ESP is /boot): one BLS entry per generation in <boot>/loader/entries,
                   read by Fedora GRUB's blscfg, plus <boot>/grub2/rime-nixos.cfg (default + timeout);
       custom-cfg  migrated bootc machines: every menu entry in <boot>/grub2/rime-nixos.cfg, sourced from GRUB's
                   custom.cfg. ostree's /boot/loader, loader.N and /boot/ostree are never touched;
     the profile's current generation is the default, the others are listed as "Rime (generation N, <date>)";
  4. garbage-collects files in <boot>/rime/nixos/ that no entry names, and stale rime-generation-*.conf entries.

Order (a power cut at any point leaves a bootable menu): kernel/initrd written to a temporary name, fsynced,
renamed, directory fsynced; then entries; then the menu file (tmp + fsync + rename); only then is anything removed.

Space: when the new files fit only after the files of generations no longer listed are removed, the hook first
writes a menu of the kept generations whose files are already on disk (bootable as it stands), removes what that
menu does not name, and only then copies; a power cut in between boots one of those generations.

Recovery in v1 is manual (D3, D9): the menu is always shown (timeout_style=menu, `timeout` seconds) and every kept
generation is listed, so a user whose new generation does not boot picks an older one. There is no boot counting:
bootupd's static GRUB config has no counting scripts, and GRUB cannot write its environment block on btrfs (the
/boot of a migrated machine), so a counter would work on one layout only. See nix/pkgs/rime-boot/README.md.

`rime-boot check` is read-only: it prints (JSON) which generation the menu boots by default and whether that is the
profile's current generation with every file in place (exit 0), or not (exit 1). Exit 2 = error, 3 = refused.
"""

import argparse
import errno
import hashlib
import json
import os
import re
import stat
import sys
import time

try:
    import tomllib
except ImportError:  # pragma: no cover - python < 3.11 is not shipped
    tomllib = None

EXIT_STALE = 1
EXIT_ERROR = 2
EXIT_REFUSED = 3

ENTRY_PREFIX = "rime-generation-"
MENU_FILE = "rime-nixos.cfg"
TMP_PREFIX = ".rime-boot-tmp-"
BLOCK_BEGIN = "### BEGIN rime-boot (written by rime-boot; do not edit inside this block) ###"
BLOCK_END = "### END rime-boot ###"
INCLUDE_BLOCK = (
    BLOCK_BEGIN + "\n"
    "if [ -f ${prefix}/" + MENU_FILE + " ]; then\n"
    "  source ${prefix}/" + MENU_FILE + "\n"
    "fi\n" + BLOCK_END + "\n"
)
LAYOUTS = ("bls", "custom-cfg")
# The Secure Boot key entry (README "The way back when Secure Boot refuses the kernel"). These markers and this text
# are the bootc helper's (rime-os files/system/libexec/rime-secureboot-key, `block`), byte for byte: a machine that
# moved from bootc keeps exactly one block whichever of the two wrote it last. boot-hook-unit pins its sha256.
SB_BEGIN = "### BEGIN rime-secureboot-key ###"
SB_END = "### END rime-secureboot-key ###"
SB_BLOCK = r"""### BEGIN rime-secureboot-key ###
# Rime OS: enrol Rime's Secure Boot key from this menu when Rime OS does not
# start ("bad shim signature"). Written by /usr/libexec/rime-secureboot-key,
# which replaces this block as a whole: edits inside it are lost.
if [ "$grub_platform" = "efi" ]; then
  if [ "$shim_lock" = "y" ]; then
    menuentry 'Enrol the Rime OS Secure Boot key (if Rime OS does not start)' --id rime-enrol-key {
      insmod part_gpt
      insmod fat
      insmod chain
      if search --no-floppy --file --set=rime_esp /EFI/rime/rime-mok.der; then
        echo "Enrol the Rime OS Secure Boot key"
        echo ""
        echo "Rime OS starts with Secure Boot on once its key is enrolled."
        echo "A blue key manager opens next. On it:"
        echo "  1. When it says \"Press any key\", press a key within 10 seconds."
        echo "  2. Choose \"Enroll key from disk\"."
        echo "  3. Choose EFI-SYSTEM (or RIMEESP), then EFI, rime, rime-mok.der."
        echo "  4. Choose \"Continue\", then \"Yes\"."
        echo "  5. Choose \"Reboot\". Rime OS then starts."
        echo "If you miss a step you come back to this menu. Choose this again."
        echo ""
        echo "Opening in 20 seconds. Press Esc to go back to the menu."
        if sleep --verbose --interruptible 20; then
          chainloader ($rime_esp)/EFI/fedora/mmx64.efi
        fi
      else
        echo "The Rime OS key file is not on this machine's EFI partition."
        echo "Turn Secure Boot off in the firmware settings to start Rime OS,"
        echo "then run: sudo systemctl start rime-secureboot-key.service"
        sleep --interruptible 30
      fi
    }
  fi
fi
### END rime-secureboot-key ###
"""
# Where the entry and MokManager look for the certificate, and the MokManager the entry chainloads: Fedora's, put on
# the ESP by the installer (nixos_esp) or by bootupd. Its presence is what says "this is the ESP shim boots from".
SB_CERT_REL = ("EFI", "rime", "rime-mok.der")
SB_MOKMANAGER_REL = ("EFI", "fedora", "mmx64.efi")
# host.toml [boot] microcode: which vendors' early-microcode cpios every entry carries. "both" (the default) is what
# a generic install needs; the installer writes the CPU's own vendor.
MICROCODE = {"both": ("intel", "amd"), "intel": ("intel",), "amd": ("amd",), "none": ()}
MICROCODE_EXT = "org.rimeos.microcode.v1"
# Kernel parameters are written into GRUB script (custom-cfg) and BLS `options` lines that blscfg expands like GRUB
# script. Anything that GRUB would expand, quote or split is refused by name instead of escaped: that includes `*` and
# `?` (GRUB's wildcard translator globs them against devices and files) and `#` (a word starting with it is a comment).
SAFE_PARAM = re.compile(r"^[A-Za-z0-9_./:=,+@%~^\-\[\]!]+$")


class Refusal(Exception):
    """A named refusal: nothing was written."""


class HookError(Exception):
    pass


def read_text(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def log(msg):
    print(f"rime-boot: {msg}", file=sys.stderr)


# ── configuration ──────────────────────────────────────────────────────────────

DEFAULTS = {
    "layout": "auto",
    "bootDir": "/boot",
    "profileDir": "/nix/var/nix/profiles",
    "profile": "system",
    "hostToml": "/var/lib/rime/host.toml",
    "pinDir": "/nix/var/nix/gcroots/rime",
    "bootedSystem": "/run/booted-system",
    "configurationLimit": 10,
    "timeout": 5,
    "reserveMiB": 32,
    "grubPath": None,
    "mountinfo": "/proc/self/mountinfo",
    # The DER certificate the kernel is signed with (nix/modules/boot.nix, rime.boot.mokCertificate), or None: no
    # Rime-signed kernel, nothing to enrol, custom.cfg's Secure Boot block left as it is.
    "mokCert": None,
}


def load_config(path):
    cfg = dict(DEFAULTS)
    if path:
        with open(path, encoding="utf-8") as f:
            cfg.update(json.load(f))
    if cfg["layout"] not in ("auto",) + LAYOUTS:
        raise HookError(f"layout {cfg['layout']!r} is not one of auto, bls, custom-cfg")
    if int(cfg["configurationLimit"]) < 1:
        raise HookError("configurationLimit must be at least 1")
    return cfg


def read_host(cfg):
    """The parsed /var/lib/rime/host.toml (D2 host layer), or {} when there is none."""
    p = cfg["hostToml"]
    if not p or not os.path.exists(p):
        return {}
    if tomllib is None:
        raise HookError("python has no tomllib; cannot read " + p)
    with open(p, "rb") as f:
        try:
            return tomllib.load(f)
        except tomllib.TOMLDecodeError as e:
            raise Refusal(f"{p} is not valid TOML ({e}); fix it or remove it") from e


def read_host_boot(cfg, host=None):
    """The [boot] table of host.toml, or {} when there is none."""
    boot = (read_host(cfg) if host is None else host).get("boot", {})
    if not isinstance(boot, dict):
        raise Refusal(f"{cfg['hostToml']}: [boot] must be a table")
    return boot


# ── console keymap (PLAN.md D2 keymap gate, R19, Q16) ──────────────────────────
#
# The initrd is shared by every machine and keeps KEYMAP=us. A machine whose host.toml [keyboard] console names
# another keymap gets rd.vconsole.keymap=<it> on every entry: the initrd's systemd-vconsole-setup loads it from the
# initrd's /etc/kbd/keymaps before the LUKS passphrase is asked for (checks.keymap-luks proves gb and ch). The
# initrd's keymap tree is the generation's /etc/kbd/keymaps (nixos/modules/config/console.nix: both are consoleEnv),
# so <toplevel>/etc/kbd/keymaps says whether the name will load. When it will not and the machine unlocks a LUKS
# volume at boot, the hook refuses by name: the passphrase would be read with the us layout (R19 lockout).
KEYMAP_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.+-]*$")
KEYMAP_SUFFIXES = (".map", ".map.gz", ".map.bz2", ".map.xz", ".map.zst")


def keymap_in_tree(root, name):
    """True when loadkeys would find NAME (as NAME.map[.gz|...]) anywhere under ROOT."""
    if not os.path.isdir(root):
        return False
    want = {name + suf for suf in KEYMAP_SUFFIXES}
    for _d, _dirs, files in os.walk(root, followlinks=True):
        if want.intersection(files):
            return True
    return False


def uses_luks(host, params):
    return bool(host.get("luks")) or any(p.startswith(("rd.luks.", "luks.")) for p in params)


def keymap_params(cfg, host, host_params, default):
    """[] or ["rd.vconsole.keymap=<host.toml [keyboard] console>"], appended to every entry."""
    kb = host.get("keyboard", {})
    name = kb.get("console") if isinstance(kb, dict) else None
    if not name or name == "us":
        return []
    if any(p.startswith("rd.vconsole.keymap=") for p in host_params):
        return []  # [boot] kernel_params names one itself: it wins
    if not isinstance(name, str) or not KEYMAP_NAME.match(name):
        raise Refusal(f"{cfg['hostToml']}: [keyboard] console = {name!r} is not a keymap name")
    tree = os.path.join(default.toplevel, "etc", "kbd", "keymaps")
    if keymap_in_tree(tree, name):
        return [f"rd.vconsole.keymap={name}"]
    if uses_luks(host, host_params + default.params):
        raise Refusal(
            f"{cfg['hostToml']}: [keyboard] console = {name!r} is not a keymap of generation {default.number} "
            f"({tree}), and this machine unlocks LUKS at boot: its passphrase prompt would read the us layout. "
            "Pick a keymap that generation ships (localectl list-keymaps), or set it to \"us\""
        )
    log(f"warning: [keyboard] console = {name!r} is not a keymap of generation {default.number}; "
        "the boot console stays on us")
    return []


def is_ostree_boot(boot_dir):
    """bootc/ostree owns this /boot: /boot/loader is ostree's symlink to loader.0|1, or /boot/ostree exists."""
    loader = os.path.join(boot_dir, "loader")
    return os.path.islink(loader) or os.path.isdir(os.path.join(boot_dir, "ostree"))


def resolve_layout(cfg, host_boot):
    boot_dir = cfg["bootDir"]
    layout, why = None, None
    if "layout" in host_boot:
        layout, why = host_boot["layout"], cfg["hostToml"]
        if layout not in LAYOUTS:
            raise Refusal(f"{cfg['hostToml']}: [boot] layout = {layout!r} is not one of bls, custom-cfg")
    elif cfg["layout"] != "auto":
        layout, why = cfg["layout"], "rime.boot.layout"
    elif is_ostree_boot(boot_dir):
        layout, why = "custom-cfg", f"{boot_dir}/loader or {boot_dir}/ostree is ostree's"
    else:
        layout, why = "bls", "no ostree loader in " + boot_dir
    if layout == "bls" and os.path.islink(os.path.join(boot_dir, "loader")):
        # D3: NixOS entries never go into ostree's loader directory (it is swapped on every bootc deploy, and
        # pruning there could delete ostree's own entries).
        raise Refusal(
            f"{boot_dir}/loader is a symlink (ostree's loader.N); refusing to write BLS entries there. "
            "This is a migrated bootc machine: use layout custom-cfg"
        )
    return layout, why


# ── generations ────────────────────────────────────────────────────────────────

class Generation:
    def __init__(self, number, link, toplevel):
        self.number = number
        self.link = link
        self.toplevel = toplevel
        self.date = time.strftime("%Y-%m-%d %H:%M", time.localtime(os.lstat(link).st_mtime))
        spec_path = os.path.join(toplevel, "boot.json")
        doc = {}
        if os.path.exists(spec_path):
            with open(spec_path, encoding="utf-8") as f:
                doc = json.load(f)
        spec = doc.get("org.nixos.bootspec.v1", {})
        self.spec = spec
        # nix/modules/boot.nix: {"intel": <cpio>, "amd": <cpio>}. A generation built before the split (or with the
        # extension off) has none: its initrd may carry microcode itself (boot.initrd.prepend) and is used as is.
        ucode = doc.get(MICROCODE_EXT) or {}
        self.microcode = {k: os.path.realpath(v) for k, v in ucode.items() if k in ("intel", "amd") and v}
        self.mandatory = False
        # The toplevel's own kernel link, not bootspec's `kernel`: bootspec names ${kernelPackages.kernel}/bzImage
        # (unsigned), while nix/modules/kernel.nix relinks <toplevel>/kernel to the MOK-signed image when the hosted
        # signing job's signature is set. Whatever <toplevel>/kernel is, is what boots.
        k = os.path.join(toplevel, "kernel")
        i = os.path.join(toplevel, "initrd")
        self.kernel = os.path.realpath(k) if os.path.exists(k) else spec.get("kernel")
        self.initrd = os.path.realpath(i) if os.path.exists(i) else spec.get("initrd")
        self.init = spec.get("init") or os.path.join(toplevel, "init")
        params = spec.get("kernelParams")
        if params is None:
            kp = os.path.join(toplevel, "kernel-params")
            params = read_text(kp).split() if os.path.exists(kp) else []
        self.params = list(params)
        if not self.kernel or not os.path.isfile(self.kernel):
            raise HookError(f"generation {number} ({toplevel}) has no kernel")
        if not self.initrd or not os.path.isfile(self.initrd):
            raise HookError(f"generation {number} ({toplevel}) has no initrd")

    def initrds(self, vendors):
        """The files this generation's entry loads as initrds, in order: the microcode cpios, then the initrd."""
        out = []
        for v in vendors:
            p = self.microcode.get(v)
            if p is None:
                continue
            if not os.path.isfile(p):
                raise HookError(f"generation {self.number}: its {v} microcode {p} does not exist")
            out.append(p)
        return out + [self.initrd]

    @property
    def entry_id(self):
        return f"{ENTRY_PREFIX}{self.number}"

    def title(self):
        return f"Rime (generation {self.number}, {self.date})"


LINK_RE = re.compile(r"^(?P<profile>.+)-(?P<n>[0-9]+)-link$")


def list_generations(cfg):
    pdir, profile = cfg["profileDir"], cfg["profile"]
    gens = {}
    try:
        names = os.listdir(pdir)
    except FileNotFoundError:
        names = []
    for name in names:
        m = LINK_RE.match(name)
        if not m or m.group("profile") != profile:
            continue
        link = os.path.join(pdir, name)
        top = os.path.realpath(link)
        if not os.path.isdir(top):
            continue
        gens[int(m.group("n"))] = (link, top)
    return gens


def current_generation_number(cfg):
    p = os.path.join(cfg["profileDir"], cfg["profile"])
    try:
        target = os.readlink(p)
    except OSError:
        return None
    m = LINK_RE.match(os.path.basename(target))
    return int(m.group("n")) if m else None


def pinned_numbers(cfg):
    out = set()
    d = cfg["pinDir"]
    if d and os.path.isdir(d):
        for name in os.listdir(d):
            m = re.match(r"^pin-([0-9]+)$", name)
            if m:
                out.add(int(m.group(1)))
    return out


def plan_generations(cfg, toplevel_arg):
    """Returns (kept generations newest first, default generation)."""
    raw = list_generations(cfg)
    current = current_generation_number(cfg)
    if not raw:
        raise HookError(f"no generations of profile {cfg['profile']} in {cfg['profileDir']}")
    if current is None or current not in raw:
        current = max(raw)
        log(f"profile link {cfg['profile']} names no generation; defaulting to the newest, {current}")
    if toplevel_arg:
        top = os.path.realpath(toplevel_arg)
        if raw[current][1] != top:
            log(f"note: {top} is not the profile's current generation {current}; the menu defaults to generation {current}")
    must = {current} | {n for n in pinned_numbers(cfg) if n in raw}
    booted = cfg.get("bootedSystem")
    if booted and os.path.exists(booted):
        b = os.path.realpath(booted)
        must |= {n for n, (_, t) in raw.items() if t == b}
    keep = set(sorted(raw, reverse=True)[: int(cfg["configurationLimit"])]) | must
    gens = [Generation(n, *raw[n]) for n in sorted(keep, reverse=True)]
    for g in gens:
        g.mandatory = g.number in must
    default = next(g for g in gens if g.number == current)
    return gens, default


# ── files ──────────────────────────────────────────────────────────────────────

def sha256_file(path, _cache={}):
    if path in _cache:
        return _cache[path]
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    _cache[path] = h.hexdigest()
    return _cache[path]


def store_name(path):
    """`/nix/store/<hash>-linux-7.3/bzImage` -> `linux-7.3-bzImage` (for humans; the sha256 prefix is the identity)."""
    parts = path.split("/")
    if len(parts) > 3 and parts[1] == "nix" and parts[2] == "store":
        pkg = parts[3].split("-", 1)[1] if "-" in parts[3] else parts[3]
        rest = "-".join(parts[4:])
        name = f"{pkg}-{rest}" if rest else pkg
    else:
        name = os.path.basename(path)
    name = re.sub(r"[^A-Za-z0-9._+-]", "_", name)
    return name[:96]


def boot_file_name(path, kind):
    suffix = ".efi" if kind == "kernel" else ".img"
    name = store_name(path)
    return f"{sha256_file(path)[:32]}-{name}{'' if name.endswith(suffix) else suffix}"


def pe_signed(path):
    """True when the PE image carries an Authenticode certificate table (sbsign/sbattach). Not enforced here:
    shim decides; this only reports."""
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
        if head[:2] != b"MZ":
            return False
        off = int.from_bytes(head[0x3C:0x40], "little")
        if head[off:off + 4] != b"PE\0\0":
            return False
        opt = off + 24
        magic = int.from_bytes(head[opt:opt + 2], "little")
        dd = opt + (112 if magic == 0x20B else 96)
        size = int.from_bytes(head[dd + 4 * 8 + 4: dd + 4 * 8 + 8], "little")
        return size > 0
    except OSError:
        return False


def fsync_dir(d):
    try:
        fd = os.open(d, os.O_RDONLY | os.O_DIRECTORY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError as e:  # vfat may refuse fsync on a directory; the file fsync + rename still hold
        if e.errno not in (errno.EINVAL, errno.EROFS, errno.ENOTSUP):
            raise
    finally:
        os.close(fd)


def atomic_write(path, data, mode=0o644):
    """Write tmp, fsync, rename over `path`, fsync the directory. Skips the write if `path` already holds `data`
    (idempotent: a second run changes nothing)."""
    if isinstance(data, str):
        data = data.encode()
    try:
        with open(path, "rb") as f:
            if f.read() == data:
                return False
    except FileNotFoundError:
        pass
    d = os.path.dirname(path)
    tmp = os.path.join(d, TMP_PREFIX + os.path.basename(path))
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.rename(tmp, path)
    fsync_dir(d)
    return True


def atomic_copy(src, dst):
    d = os.path.dirname(dst)
    tmp = os.path.join(d, TMP_PREFIX + os.path.basename(dst))
    with open(src, "rb") as fi, open(tmp, "wb") as fo:
        while True:
            b = fi.read(1 << 20)
            if not b:
                break
            fo.write(b)
        fo.flush()
        os.fsync(fo.fileno())
    os.rename(tmp, dst)
    fsync_dir(d)


# ── GRUB paths ─────────────────────────────────────────────────────────────────

def _unescape_mountinfo(s):
    return re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), s)


def grub_path_of(cfg):
    """The path GRUB sees for bootDir, relative to the root of the filesystem that holds it: "" when /boot is the
    ESP (vfat mounted at /boot), "/boot" when /boot is a directory at the top of bootc's btrfs (topology E).
    From /proc/self/mountinfo: the mount's root within its filesystem (field 4; for btrfs, relative to subvolid 5,
    which is what GRUB reads) plus the path of bootDir under the mount point."""
    if cfg.get("grubPath") is not None:
        return cfg["grubPath"].rstrip("/")
    boot = os.path.realpath(cfg["bootDir"])
    best = None
    with open(cfg["mountinfo"], encoding="utf-8") as f:
        for line in f:
            fields = line.split()
            root, mnt = _unescape_mountinfo(fields[3]), _unescape_mountinfo(fields[4])
            if boot == mnt or boot.startswith(mnt.rstrip("/") + "/"):
                if best is None or len(mnt) >= len(best[1]):
                    best = (root, mnt)
    if best is None:
        raise HookError(f"no mount holds {boot}")
    root, mnt = best
    rel = os.path.relpath(boot, mnt)
    p = os.path.normpath(os.path.join(root, rel if rel != "." else ""))
    return "" if p == "/" else p.rstrip("/")


# ── menu text ──────────────────────────────────────────────────────────────────

def check_params(params, where):
    for p in params:
        if not SAFE_PARAM.match(p):
            raise Refusal(
                f"kernel parameter {p!r} from {where} has a character GRUB would expand, quote or split "
                "($, quotes, backslash, ;, braces, |, &, <, >, *, ?, #, whitespace); refusing to write it"
            )


def options_of(g, host_params):
    return " ".join([f"init={g.init}"] + g.params + host_params)


def bls_entry(g, files, grub_path, host_params, vendors):
    # One `initrd` line per file: Fedora GRUB's blscfg loads them in order and the kernel gets one concatenated
    # image, microcode first (nix/probe/ovmf/mok, boots U1/U2).
    return (
        f"title {g.title()}\n"
        f"version {g.number}\n"
        f"linux {grub_path}/rime/nixos/{files[g.kernel]}\n"
        + "".join(f"initrd {grub_path}/rime/nixos/{files[i]}\n" for i in g.initrds(vendors))
        + f"options {options_of(g, host_params)}\n"
    )


def menu_file(layout, gens, default, files, grub_path, host_params, timeout, vendors):
    lines = [
        "# Rime boot menu, written by rime-boot (boot.loader.external). Do not edit: every",
        "# switch-to-configuration boot rewrites it. Sourced from custom.cfg (bootupd's 41_custom.cfg).",
        f"# layout {layout}; default generation {default.number} ({default.toplevel})",
        "set timeout_style=menu",
        f"set timeout={int(timeout)}",
    ]
    if layout == "custom-cfg":
        # The default first, then the others newest first, so "down" from the default reaches the previous one.
        order = [default] + [g for g in gens if g is not default]
        for g in order:
            lines += [
                f"menuentry '{g.title()}' --id {g.entry_id} {{",
                f"  linux {grub_path}/rime/nixos/{files[g.kernel]} {options_of(g, host_params)}",
                "  initrd " + " ".join(f"{grub_path}/rime/nixos/{files[i]}" for i in g.initrds(vendors)),
                "}",
            ]
    lines.append(f"set default={default.entry_id}")
    return "\n".join(lines) + "\n"


def with_include_block(old):
    """custom.cfg with exactly one rime-boot block; everything else (e.g. the bootc installer's Windows entry) kept
    byte for byte."""
    if old is None:
        return INCLUDE_BLOCK
    pat = re.compile(re.escape(BLOCK_BEGIN) + r".*?" + re.escape(BLOCK_END) + r"\n?", re.S)
    if pat.search(old):
        return pat.sub(lambda _m: INCLUDE_BLOCK, old, count=1)
    sep = "" if old.endswith("\n") or old == "" else "\n"
    return old + sep + INCLUDE_BLOCK


def with_secureboot_block(old):
    """custom.cfg with exactly one Secure Boot key block, at the end: the bootc helper's custom_cfg_with_block, line
    for line. Every line from a BEGIN marker line through the next END marker line goes (an unterminated BEGIN takes
    the rest of the file), then trailing empty lines, then one empty line and the block are added. Everything else
    is kept as it is. The result is a fixed point of both this and the helper."""
    lines = []
    if old:
        recs = old.split("\n")
        if recs[-1] == "":
            recs.pop()
        skip = False
        for line in recs:
            if line == SB_BEGIN:
                skip = True
            elif line == SB_END:
                skip = False
            elif not skip:
                lines.append(line)
        while lines and lines[-1] == "":
            lines.pop()
    head = "".join(line + "\n" for line in lines) + ("\n" if lines else "")
    return head + SB_BLOCK


def custom_cfg(old, secureboot):
    """The new custom.cfg: the include block in place (or appended), and with `secureboot` the Secure Boot block
    moved to the end. The include block is put back once more afterwards: a BEGIN marker with no END (a damaged file)
    makes the bootc rule drop everything after it, and the menu must never lose the line that sources it."""
    new = with_include_block(old)
    if secureboot:
        new = with_include_block(with_secureboot_block(new))
    return new


def mounted_fstype(cfg, path):
    """The filesystem type mounted exactly at `path` (from mountinfo), or None when `path` is not a mount point."""
    want = os.path.realpath(path)
    found = None
    try:
        with open(cfg["mountinfo"], encoding="utf-8") as f:
            for line in f:
                fields = line.split()
                if "-" not in fields:
                    continue
                sep = fields.index("-")
                if _unescape_mountinfo(fields[4]) == want and sep + 1 < len(fields):
                    found = fields[sep + 1]  # the last mount at a path is the one that shows
    except OSError:
        return None
    return found


def secureboot_esp(cfg, boot, layout):
    """(the ESP to put the certificate on, None) or (None, why not). As careful as the bootc helper: the ESP the hook
    already writes (bls: /boot IS Rime's own ESP; the hook has refused a Windows ESP there before getting here), or
    for custom-cfg only a FAT filesystem mounted at <boot>/efi that is not Windows' ESP. Never a guess between
    several. Either way it must hold Fedora's MokManager, which the entry chainloads from the same partition."""
    if layout == "bls":
        esp = boot
    else:
        esp = os.path.join(boot, "efi")
        fstype = mounted_fstype(cfg, esp)
        if fstype != "vfat":
            return None, (f"{esp} is not a mounted FAT filesystem ({fstype or 'not mounted'}), and on a {layout} "
                          "machine that is the only ESP this hook will write")
        if windows_boot_manager(esp):
            return None, f"{esp} holds Windows Boot Manager: Rime never writes Windows' ESP"
    if not os.path.isfile(os.path.join(esp, *SB_MOKMANAGER_REL)):
        return None, f"{esp} has no {'/'.join(SB_MOKMANAGER_REL)}: not the ESP this machine boots shim from"
    return esp, None


def place_secureboot_key(cfg, boot, layout):
    """Put the Rime certificate on the ESP at EFI/rime/rime-mok.der (tmp + fsync + rename, read back; nothing written
    when an identical file is there). True = the menu entry may be written; False = the Secure Boot block in
    custom.cfg is left exactly as it is, and the log says why. Never raises: a machine without the entry is what
    every machine was before it existed, so this never fails a switch."""
    src = cfg.get("mokCert")
    if not src:
        log("secure boot key: the kernel is not signed with a Rime key (rime.boot.mokCertificate unset): "
            "nothing to enrol, no boot menu entry")
        return False
    try:
        with open(src, "rb") as f:
            cert = f.read()
    except OSError as e:
        log(f"secure boot key: cannot read the certificate {src}: {e}; no boot menu entry")
        return False
    if not cert.startswith(b"\x30"):
        log(f"secure boot key: {src} is not a DER certificate; no boot menu entry")
        return False
    esp, why = secureboot_esp(cfg, boot, layout)
    if esp is None:
        log(f"secure boot key: {why}; certificate not placed, boot menu entry not written")
        return False
    dst = os.path.join(esp, *SB_CERT_REL)
    try:
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        if atomic_write(dst, cert):
            log(f"secure boot key: certificate written to {dst}")
        with open(dst, "rb") as f:
            if f.read() != cert:
                log(f"secure boot key: {dst} does not match {src} after writing it; boot menu entry not written")
                return False
    except OSError as e:
        log(f"secure boot key: could not place {dst}: {e}; boot menu entry not written")
        return False
    return True


# ── install ────────────────────────────────────────────────────────────────────

def windows_boot_manager(boot):
    """The path of Windows Boot Manager under boot (EFI/Microsoft/Boot/bootmgfw.efi, any case: FAT), or None."""
    d = boot
    for part in ("efi", "microsoft", "boot", "bootmgfw.efi"):
        try:
            names = os.listdir(d)
        except (NotADirectoryError, FileNotFoundError):
            return None
        match = [n for n in names if n.lower() == part]
        if not match:
            return None
        d = os.path.join(d, match[0])
    return d if os.path.isfile(d) else None


def free_bytes(path):
    st = os.statvfs(path)
    return st.f_bavail * st.f_frsize


def install(cfg, toplevel_arg):
    boot = cfg["bootDir"]
    if not os.path.isdir(boot):
        raise Refusal(f"{boot} does not exist or is not a directory (is the ESP / boot filesystem mounted?)")
    grub2 = os.path.join(boot, "grub2")
    if not os.path.isdir(grub2):
        raise Refusal(
            f"{grub2} does not exist: Fedora's GRUB (bootupd's static config) is not installed on this boot "
            "filesystem, so nothing would read the menu. rime-boot never installs GRUB"
        )
    windows = windows_boot_manager(boot)
    if windows:
        # D3 ESP rules (R20): Rime writes only its own ESP or bootc's /boot, never Windows' ESP (a shared disk,
        # topologies B and D; katana's nvme0n1p1). A fresh install mounts Rime's own ESP at /boot.
        raise Refusal(
            f"{boot} holds Windows Boot Manager ({windows}): this is a Windows EFI system partition, and Rime "
            "writes its menu and kernels only to its own ESP. Nothing was written; mount Rime's ESP at "
            f"{boot} and run the hook again"
        )
    host = read_host(cfg)
    host_boot = read_host_boot(cfg, host)
    layout, why = resolve_layout(cfg, host_boot)
    host_params = host_boot.get("kernel_params", [])
    if not isinstance(host_params, list) or not all(isinstance(p, str) for p in host_params):
        # A bare string would otherwise be split into one "parameter" per character.
        raise Refusal(f"{cfg['hostToml']}: [boot] kernel_params must be a list of strings, "
                      f"e.g. [\"rd.luks.uuid=...\"], not {host_params!r}")
    check_params(host_params, cfg["hostToml"])
    gens, default = plan_generations(cfg, toplevel_arg)
    for g in gens:
        check_params(g.params, f"generation {g.number}")
    host_params = host_params + keymap_params(cfg, host, host_params, default)
    grub_path = grub_path_of(cfg)

    vendors = microcode_vendors(cfg, host_boot)
    rdir = os.path.join(boot, "rime", "nixos")
    reserve = int(cfg["reserveMiB"]) * 1024 * 1024
    mib = lambda b: f"{b / 1048576:.1f} MiB"  # noqa: E731
    on_disk = files_on_disk(rdir)
    avail = free_bytes(boot if not os.path.isdir(rdir) else rdir)

    # (e) the floor, before the first write: leave out the oldest optional generations until the files fit, in
    # free space as it is or once the files no kept generation names are removed.
    kept, dropped = list(gens), []
    while True:
        files, to_copy, need, reclaim = space_plan(kept, vendors, rdir, on_disk)
        if not to_copy or avail >= need + reserve:
            room = "free"
            break
        if avail + reclaim >= need + reserve:
            room = "reclaim"
            break
        optional = [g for g in kept if not g.mandatory]
        if not optional:
            raise Refusal(
                f"not enough space on {boot}: {mib(avail)} free ({mib(reclaim)} more once unlisted files are "
                f"removed), the new kernel, microcode and initrd files of the current, booted and pinned "
                f"generation(s) {', '.join(str(g.number) for g in kept)} need {mib(need)} plus the {mib(reserve)} "
                "floor (rime.boot.reserveMiB). Nothing was written; unpin a generation or make the boot "
                "filesystem bigger, and run the hook again"
            )
        oldest = min(optional, key=lambda g: g.number)
        kept.remove(oldest)
        dropped.append(oldest.number)
    for n in dropped:
        log(f"generation {n} left out of the menu: {boot} cannot hold its files as well (free {mib(avail)}, "
            f"floor {mib(reserve)}); it stays in the profile")

    log(f"layout {layout} ({why}); GRUB path of {boot}: {grub_path or '/'}; microcode {'+'.join(vendors) or 'none'}")
    os.makedirs(rdir, exist_ok=True)
    for name in os.listdir(rdir):
        if name.startswith(TMP_PREFIX):
            os.unlink(os.path.join(rdir, name))

    # Decided once per run (write_menus runs twice when making room): the certificate first, then the entry.
    secureboot = None
    if room == "reclaim":
        # Make room without ever leaving a menu that names a missing file: list only kept generations whose files
        # are all on disk, remove everything else, then copy.
        present = [g for g in kept if all(on_disk.get(files[p]) == os.path.getsize(p)
                                          for p in [g.kernel] + g.initrds(vendors))]
        if not present:
            raise Refusal(
                f"not enough space on {boot}: {mib(avail)} free and no kept generation's files are on it yet, "
                "so making room would leave nothing to boot. Nothing was written; free space on the boot "
                "filesystem and run the hook again"
            )
        interim = default if default in present else present[0]
        log(f"making room: menu of generation(s) {', '.join(str(g.number) for g in present)} "
            f"(default {interim.number}) while the unlisted files are removed")
        secureboot = place_secureboot_key(cfg, boot, layout)
        write_menus(cfg, boot, grub2, layout, present, interim, files, grub_path, host_params, vendors, secureboot)
        # Keep every file a KEPT generation names (some may be on disk for a generation not yet complete): exactly
        # the `reclaim` bytes space_plan counted go.
        collect_garbage(boot, rdir, layout, present, set(files.values()))
        avail = free_bytes(rdir)
        if avail < need + reserve:
            raise Refusal(
                f"not enough space on {boot} after removing unlisted files: {mib(avail)} free, the new files need "
                f"{mib(need)} plus the {mib(reserve)} floor. The menu lists generation(s) "
                f"{', '.join(str(g.number) for g in present)}, all bootable; nothing new was copied"
            )

    for src, dst in to_copy:
        log(f"copy {src} -> {dst}")
        atomic_copy(src, dst)
    for g in kept:
        log(
            f"generation {g.number}{' (default)' if g is default else ''}: kernel {files[g.kernel]}"
            f"{' (signed)' if pe_signed(g.kernel) else ' (no signature)'}, initrd "
            + " + ".join(files[i] for i in g.initrds(vendors))
        )
    if secureboot is None:
        secureboot = place_secureboot_key(cfg, boot, layout)
    write_menus(cfg, boot, grub2, layout, kept, default, files, grub_path, host_params, vendors, secureboot)
    # Only now remove what nothing names any more.
    collect_garbage(boot, rdir, layout, kept, {files[p] for g in kept for p in [g.kernel] + g.initrds(vendors)})
    try:
        fd = os.open(boot, os.O_RDONLY | os.O_DIRECTORY)
        os.syncfs(fd) if hasattr(os, "syncfs") else os.sync()
        os.close(fd)
    except OSError:
        os.sync()
    log(f"menu: {len(kept)} generation(s), default generation {default.number}")
    return 0


def microcode_vendors(cfg, host_boot):
    v = host_boot.get("microcode", "both")
    if v not in MICROCODE:
        raise Refusal(f"{cfg['hostToml']}: [boot] microcode must be one of {', '.join(MICROCODE)}, not {v!r}")
    return MICROCODE[v]


def files_on_disk(rdir):
    """{name: size} of the regular files rime-boot keeps in rdir (temporary files excluded)."""
    out = {}
    if os.path.isdir(rdir):
        for name in os.listdir(rdir):
            p = os.path.join(rdir, name)
            st = os.lstat(p)
            if stat.S_ISREG(st.st_mode) and not name.startswith(TMP_PREFIX):
                out[name] = st.st_size
    return out


def space_plan(gens, vendors, rdir, on_disk):
    """(files {src: name}, to_copy [(src, dst)], bytes to copy, bytes freed by removing what gens do not name)."""
    files = {}
    for g in gens:
        for p in [g.kernel] + g.initrds(vendors):
            files.setdefault(p, boot_file_name(p, "kernel" if p == g.kernel else "initrd"))
    need, to_copy, seen = 0, [], set()
    for src, name in sorted(files.items(), key=lambda kv: kv[1]):
        if name in seen or on_disk.get(name) == os.path.getsize(src):
            continue
        seen.add(name)
        to_copy.append((src, os.path.join(rdir, name)))
        need += os.path.getsize(src)
    referenced = set(files.values())
    reclaim = sum(size for name, size in on_disk.items() if name not in referenced)
    return files, to_copy, need, reclaim


def write_menus(cfg, boot, grub2, layout, gens, default, files, grub_path, host_params, vendors, secureboot):
    """Entries (bls), the menu file and custom.cfg's include block, each written atomically. `secureboot`: also put
    the Secure Boot key block at the end of custom.cfg (place_secureboot_key placed the certificate); otherwise any
    block already there is kept as it is."""
    if layout == "bls":
        edir = os.path.join(boot, "loader", "entries")
        os.makedirs(edir, exist_ok=True)
        for name in os.listdir(edir):
            if name.startswith(TMP_PREFIX):
                os.unlink(os.path.join(edir, name))
        for g in gens:
            atomic_write(os.path.join(edir, g.entry_id + ".conf"), bls_entry(g, files, grub_path, host_params, vendors))
    atomic_write(
        os.path.join(grub2, MENU_FILE),
        menu_file(layout, gens, default, files, grub_path, host_params, cfg["timeout"], vendors),
    )
    custom = os.path.join(grub2, "custom.cfg")
    try:
        old = read_text(custom)
    except FileNotFoundError:
        old = None
    atomic_write(custom, custom_cfg(old, secureboot))


def collect_garbage(boot, rdir, layout, gens, referenced):
    """Remove the BLS entries of generations not in gens and the files of rdir not in referenced (call it only
    after the menu that names gens is written)."""
    if layout == "bls":
        edir = os.path.join(boot, "loader", "entries")
        wanted = {g.entry_id + ".conf" for g in gens}
        for name in os.listdir(edir):
            if name.startswith(ENTRY_PREFIX) and name.endswith(".conf") and name not in wanted:
                log(f"remove entry {name}")
                os.unlink(os.path.join(edir, name))
        fsync_dir(edir)
    if os.path.isdir(rdir):
        for name in os.listdir(rdir):
            p = os.path.join(rdir, name)
            if name not in referenced and stat.S_ISREG(os.lstat(p).st_mode):
                log(f"remove unreferenced {p}")
                os.unlink(p)
        fsync_dir(rdir)


# ── check (read-only) ──────────────────────────────────────────────────────────

def boot_refs(text):
    """Every file a menu entry loads: the kernel (first word of `linux`) and every path on every `initrd` line."""
    refs = []
    for kind, rest in re.findall(r"^\s*(linux|initrd) (.+?)\s*$", text, re.M):
        words = rest.split()
        refs += words[:1] if kind == "linux" else words
    return refs


def check(cfg):
    boot = cfg["bootDir"]
    host_boot = read_host_boot(cfg)
    layout, _ = resolve_layout(cfg, host_boot)
    menu = os.path.join(boot, "grub2", MENU_FILE)
    out = {"layout": layout, "menu": menu, "default_generation": None, "default_toplevel": None,
           "profile_generation": current_generation_number(cfg), "missing": [], "up_to_date": False}
    try:
        text = read_text(menu)
    except FileNotFoundError:
        out["missing"].append(menu)
        print(json.dumps(out, indent=2))
        return EXIT_STALE
    m = re.search(r"^set default=" + re.escape(ENTRY_PREFIX) + r"([0-9]+)$", text, re.M)
    if m:
        n = int(m.group(1))
        out["default_generation"] = n
        gens = list_generations(cfg)
        if n in gens:
            out["default_toplevel"] = gens[n][1]
        if layout == "bls":
            entry = os.path.join(boot, "loader", "entries", f"{ENTRY_PREFIX}{n}.conf")
            try:
                body = read_text(entry)
            except FileNotFoundError:
                out["missing"].append(entry)
                body = ""
            refs = boot_refs(body)
        else:
            blk = re.search(r"--id " + re.escape(f"{ENTRY_PREFIX}{n}") + r" \{\n(.*?)\n\}", text, re.S)
            refs = boot_refs(blk.group(1)) if blk else []
            if not blk:
                out["missing"].append(f"menuentry {ENTRY_PREFIX}{n}")
        for r in refs:
            local = os.path.join(boot, "rime", "nixos", os.path.basename(r))
            if not os.path.isfile(local):
                out["missing"].append(local)
        if not refs and not out["missing"]:
            out["missing"].append(f"kernel/initrd lines of {ENTRY_PREFIX}{n}")
    out["up_to_date"] = (out["default_generation"] is not None
                         and out["default_generation"] == out["profile_generation"] and not out["missing"])
    print(json.dumps(out, indent=2))
    return 0 if out["up_to_date"] else EXIT_STALE


def main(argv=None):
    ap = argparse.ArgumentParser(prog="rime-boot", description=__doc__.split("\n")[0])
    ap.add_argument("--config", help="JSON written by nix/modules/boot.nix")
    sub = ap.add_subparsers(dest="cmd")
    pi = sub.add_parser("install", help="write the boot menu (what switch-to-configuration runs)")
    pi.add_argument("toplevel", nargs="?")
    sub.add_parser("check", help="read-only: which generation the menu boots by default")
    args = ap.parse_args(argv)
    try:
        cfg = load_config(args.config)
        if args.cmd == "check":
            return check(cfg)
        if args.cmd in (None, "install"):
            return install(cfg, getattr(args, "toplevel", None))
    except Refusal as e:
        log(f"REFUSED: {e}")
        return EXIT_REFUSED
    except (HookError, OSError, ValueError, KeyError) as e:
        log(f"ERROR: {e}")
        return EXIT_ERROR
    return EXIT_ERROR


if __name__ == "__main__":
    sys.exit(main())
