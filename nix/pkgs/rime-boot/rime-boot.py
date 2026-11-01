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
     module links there is what boots) and initrd into <boot>/rime/nixos/ under content-addressed names
     (<sha256>-<store name>), refusing BEFORE writing anything when the filesystem would drop below its floor;
  3. writes the menu for the machine's layout:
       bls         fresh installs (the FAT ESP is /boot): one BLS entry per generation in <boot>/loader/entries,
                   read by Fedora GRUB's blscfg, plus <boot>/grub2/rime-nixos.cfg (default + timeout);
       custom-cfg  migrated bootc machines: every menu entry in <boot>/grub2/rime-nixos.cfg, sourced from GRUB's
                   custom.cfg. ostree's /boot/loader, loader.N and /boot/ostree are never touched;
     the profile's current generation is the default, the others are listed as "Rime (generation N, <date>)";
  4. garbage-collects files in <boot>/rime/nixos/ that no entry names, and stale rime-generation-*.conf entries.

Order (a power cut at any point leaves a bootable menu): kernel/initrd written to a temporary name, fsynced,
renamed, directory fsynced; then entries; then the menu file (tmp + fsync + rename); only then is anything removed.

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


def read_host_boot(cfg):
    """The [boot] table of /var/lib/rime/host.toml (D2 host layer), or {} when there is none."""
    p = cfg["hostToml"]
    if not p or not os.path.exists(p):
        return {}
    if tomllib is None:
        raise HookError("python has no tomllib; cannot read " + p)
    with open(p, "rb") as f:
        try:
            data = tomllib.load(f)
        except tomllib.TOMLDecodeError as e:
            raise Refusal(f"{p} is not valid TOML ({e}); fix it or remove it") from e
    boot = data.get("boot", {})
    if not isinstance(boot, dict):
        raise Refusal(f"{p}: [boot] must be a table")
    return boot


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
        spec = {}
        if os.path.exists(spec_path):
            with open(spec_path, encoding="utf-8") as f:
                spec = json.load(f).get("org.nixos.bootspec.v1", {})
        self.spec = spec
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
    keep = set(sorted(raw, reverse=True)[: int(cfg["configurationLimit"])])
    keep.add(current)
    keep |= {n for n in pinned_numbers(cfg) if n in raw}
    booted = cfg.get("bootedSystem")
    if booted and os.path.exists(booted):
        b = os.path.realpath(booted)
        keep |= {n for n, (_, t) in raw.items() if t == b}
    gens = [Generation(n, *raw[n]) for n in sorted(keep, reverse=True)]
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
    return f"{sha256_file(path)[:32]}-{store_name(path)}{suffix}"


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


def bls_entry(g, files, grub_path, host_params):
    k, i = files[g.kernel], files[g.initrd]
    return (
        f"title {g.title()}\n"
        f"version {g.number}\n"
        f"linux {grub_path}/rime/nixos/{k}\n"
        f"initrd {grub_path}/rime/nixos/{i}\n"
        f"options {options_of(g, host_params)}\n"
    )


def menu_file(layout, gens, default, files, grub_path, host_params, timeout):
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
                f"  initrd {grub_path}/rime/nixos/{files[g.initrd]}",
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


# ── install ────────────────────────────────────────────────────────────────────

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
    host_boot = read_host_boot(cfg)
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
    grub_path = grub_path_of(cfg)

    rdir = os.path.join(boot, "rime", "nixos")
    files = {}
    for g in gens:
        files.setdefault(g.kernel, boot_file_name(g.kernel, "kernel"))
        files.setdefault(g.initrd, boot_file_name(g.initrd, "initrd"))

    # (e) the floor: refuse before the first write.
    need = 0
    to_copy = []
    seen = set()
    for src, name in sorted(files.items(), key=lambda kv: kv[1]):
        dst = os.path.join(rdir, name)
        if name in seen or (os.path.isfile(dst) and os.path.getsize(dst) == os.path.getsize(src)):
            continue
        seen.add(name)
        to_copy.append((src, dst))
        need += os.path.getsize(src)
    reserve = int(cfg["reserveMiB"]) * 1024 * 1024
    avail = free_bytes(boot if not os.path.isdir(rdir) else rdir)
    if to_copy and avail < need + reserve:
        mib = lambda b: f"{b / 1048576:.1f} MiB"  # noqa: E731
        raise Refusal(
            f"not enough space on {boot}: {mib(avail)} free, the new kernel and initrd files need {mib(need)} "
            f"plus the {mib(reserve)} floor (rime.boot.reserveMiB). Nothing was written; remove old generations "
            "(rime gc) and run the hook again"
        )

    log(f"layout {layout} ({why}); GRUB path of {boot}: {grub_path or '/'}")
    os.makedirs(rdir, exist_ok=True)
    for name in os.listdir(rdir):
        if name.startswith(TMP_PREFIX):
            os.unlink(os.path.join(rdir, name))
    for src, dst in to_copy:
        log(f"copy {src} -> {dst}")
        atomic_copy(src, dst)
    for g in gens:
        log(
            f"generation {g.number}{' (default)' if g is default else ''}: kernel {files[g.kernel]}"
            f"{' (signed)' if pe_signed(g.kernel) else ' (no signature)'}, initrd {files[g.initrd]}"
        )

    wanted_entries = set()
    if layout == "bls":
        edir = os.path.join(boot, "loader", "entries")
        os.makedirs(edir, exist_ok=True)
        for name in os.listdir(edir):
            if name.startswith(TMP_PREFIX):
                os.unlink(os.path.join(edir, name))
        for g in gens:
            name = g.entry_id + ".conf"
            wanted_entries.add(name)
            atomic_write(os.path.join(edir, name), bls_entry(g, files, grub_path, host_params))

    atomic_write(
        os.path.join(grub2, MENU_FILE),
        menu_file(layout, gens, default, files, grub_path, host_params, cfg["timeout"]),
    )
    custom = os.path.join(grub2, "custom.cfg")
    try:
        old = read_text(custom)
    except FileNotFoundError:
        old = None
    atomic_write(custom, with_include_block(old))

    # Only now remove what nothing names any more.
    if layout == "bls":
        edir = os.path.join(boot, "loader", "entries")
        for name in os.listdir(edir):
            if name.startswith(ENTRY_PREFIX) and name.endswith(".conf") and name not in wanted_entries:
                log(f"remove entry {name}")
                os.unlink(os.path.join(edir, name))
        fsync_dir(edir)
    referenced = set(files.values())
    for name in os.listdir(rdir):
        p = os.path.join(rdir, name)
        if name not in referenced and stat.S_ISREG(os.lstat(p).st_mode):
            log(f"remove unreferenced {p}")
            os.unlink(p)
    fsync_dir(rdir)
    try:
        fd = os.open(boot, os.O_RDONLY | os.O_DIRECTORY)
        os.syncfs(fd) if hasattr(os, "syncfs") else os.sync()
        os.close(fd)
    except OSError:
        os.sync()
    log(f"menu: {len(gens)} generation(s), default generation {default.number}")
    return 0


# ── check (read-only) ──────────────────────────────────────────────────────────

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
            refs = re.findall(r"^(?:linux|initrd) (\S+)$", body, re.M)
        else:
            blk = re.search(r"--id " + re.escape(f"{ENTRY_PREFIX}{n}") + r" \{\n(.*?)\n\}", text, re.S)
            refs = re.findall(r"^\s*(?:linux|initrd) (\S+)", blk.group(1), re.M) if blk else []
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
