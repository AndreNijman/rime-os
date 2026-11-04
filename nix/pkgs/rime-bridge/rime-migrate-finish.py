#!/usr/bin/env python3
"""rime-migrate-finish: `rime migrate finish`, the reclaim step of the move from bootc to NixOS (PLAN.md D2).

`rime migrate start` leaves the old system whole beside the new one: a read-only snapshot @pre-nixos of the btrfs top
level, ostree/ (the repository, the deployments and the old /var with home as it was at the move) and the GRUB menu's
ostree entries, the emergency route back. @home shares every block with the old home, so the old side costs nothing at
the move; it costs more as home diverges on NixOS. finish removes it, on the moved NixOS system, once NixOS has proved
itself (docs/nixos/bridge.md "rime migrate finish").

  [--dry-run]   (the default without --yes) Writes nothing (the top level is mounted read-only in a private mount
                namespace). Checks every precondition and prints what --yes would delete, the space it should free
                and what stops working.
  --yes         Deletes the old side: the GRUB menu's ostree entries first (the old system stops being bootable before
                any of its data goes), then bootc's kernels in /boot/ostree, then ostree/ (moved aside into the
                top level's .rime-finish-old in one rename, then deleted) and @pre-nixos (a read-only snapshot,
                deleted where it is: btrfs refuses to move it). Every step is
                idempotent and recorded in /var/lib/rime/nixos-bridge/finish.json, so a run that was interrupted is
                completed by the next one. Never touches @nixos, @nix, @home, /boot/rime, /boot/grub2 (custom.cfg
                with the rime-boot block, the Secure Boot key entry and anything else in it), any ESP or the LUKS
                header: a containment check compares them before and after and a difference is a failure.
  --record-boot (rime-migrate-good-boot.service, once per boot after multi-user.target) Counts this boot as a good
                NixOS boot when / is @nixos.

Preconditions, each refused by name (exit 3, "REFUSED [name] ..."): booted NixOS (not ostree) from @nixos, /var/home
from @home on the same filesystem, the start record says done, GOOD_BOOTS good NixOS boots, nothing of the old side
mounted or in use, `btrfs device stats` all zero, the NixOS menu present in custom.cfg, no ostree entry this program
would leave behind (grub.cfg itself, or custom.cfg outside the rime-boot block).

The start record and the refusal it means are read with the bridge's own code (rime-migrate, the file beside this one:
`record_refusal`), as everywhere else. Exit codes: 0 ok, 2 usage, 3 refused, 10 failed.
Never runs bootctl, grub2-install or efibootmgr, never writes an ESP or NVRAM.
"""
import argparse
import datetime
import fcntl
import glob
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import re
import shutil
import struct
import sys


def _load_bridge():
    """The bridge program beside this one (rime-migrate in the package's bin/; rime-bridge.py in the source tree)."""
    here = os.path.dirname(os.path.realpath(__file__))
    for name in ("rime-migrate", "rime-bridge.py"):
        p = os.path.join(here, name)
        if os.path.isfile(p):
            loader = importlib.machinery.SourceFileLoader("rime_bridge", p)
            mod = importlib.util.module_from_spec(importlib.util.spec_from_loader("rime_bridge", loader))
            loader.exec_module(mod)
            return mod
    sys.exit(f"rime-migrate-finish: the bridge (rime-migrate) is not beside {here}")


B = _load_bridge()
Refused, Failed, run, read = B.Refused, B.Failed, B.run, B.read
EXIT_OK, EXIT_USAGE, EXIT_REFUSED, EXIT_FAILED = B.EXIT_OK, B.EXIT_USAGE, B.EXIT_REFUSED, B.EXIT_FAILED

PROG = "rime migrate finish"
# Three good boots (docs/nixos/bridge.md): the first boot finishes what the chroot activation could not, the second is
# the first one with nothing left over from the move, the third shows the second was not luck. Three is also how many
# tries Rime's bootc boot counter gave a deployment before it fell back (recover.rs).
GOOD_BOOTS = 3
GOOD_BOOTS_KEPT = 32
KEEP_TOP = ("@nixos", "@nix", "@home", "boot")   # never deleted, whatever a list below says
OLD_TOP = ("@pre-nixos", "ostree")               # the old side in the btrfs top level
TRASH = ".rime-finish-old"                       # where MOVE_ASIDE is moved before it is deleted
# Only the old tree moves aside. @pre-nixos is a read-only snapshot, and btrfs refuses (EROFS) to move a read-only
# subvolume into another directory (its `..` lives inside it; found by the OVMF run). It needs no move: deleting a
# subvolume removes its name in one step, and nothing boots it once the ostree entries are gone.
MOVE_ASIDE = ("ostree",)
STEPS = ("boot entries", "old kernels", "move aside", "nested subvolumes", "@pre-nixos", "old tree", "sync",
         "containment")
RECORD_GLOB = "ostree/deploy/*/var/lib/rime/nixos-bridge/state.json"
FS_IOC_GETFLAGS, FS_IOC_SETFLAGS, FS_IMMUTABLE_FL = 0x80086601, 0x40086602, 0x10


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def gb(n):
    return "?" if n is None else f"{n / 10**9:.2f} GB"


# ── the system, behind one object the tests replace ────────────────────────────────────────────────────────────────


def unescape(s):
    return re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), s)


def parse_mountinfo(text):
    out = []
    for line in (text or "").splitlines():
        pre, sep, post = line.partition(" - ")
        f, g = pre.split(), post.split()
        if not sep or len(f) < 5 or len(g) < 2:
            continue
        out.append({"root": unescape(f[3]), "target": unescape(f[4]), "fstype": g[0], "source": unescape(g[1]),
                    "options": f[5] if len(f) > 5 else ""})
    return out


class System:
    """Everything finish reads or runs on the machine. Paths are attributes; the tests point them at a fixture."""

    def __init__(self):
        self.mountinfo = "/proc/self/mountinfo"
        self.cmdline = "/proc/cmdline"
        self.nixos_marker = "/etc/NIXOS"
        self.ostree_booted = "/run/ostree-booted"
        self.proc = "/proc"
        self.sys_block = "/sys/block"
        self.swaps = "/proc/swaps"
        self.boot = "/boot"
        self.state = B.STATE
        self.boot_id = "/proc/sys/kernel/random/boot_id"
        self.booted_system = "/run/booted-system"
        self.top_dir = "/run/rime-migrate-finish/top"
        self.self_pid = os.getpid()

    def mounts(self):
        return parse_mountinfo(read(self.mountinfo, ""))

    def mount_top(self, source, writable):
        os.makedirs(self.top_dir, exist_ok=True)
        if not os.path.ismount(self.top_dir):
            run(["mount", "-t", "btrfs", "-o", "subvolid=5" + ("" if writable else ",ro"), source, self.top_dir])
        return self.top_dir

    def device_stats(self, path):
        """Nonzero `btrfs device stats` counters ({} = healthy); None when they cannot be read."""
        r = run(["btrfs", "device", "stats", path], check=False)
        if not r.stdout:
            return None
        bad = {}
        for m in re.finditer(r"^(\S+)\s+(-?\d+)\s*$", r.stdout, re.M):
            if int(m.group(2)) != 0:
                bad[m.group(1)] = int(m.group(2))
        return bad

    def subvolumes(self, top):
        """{path relative to the top level: uuid} of every subvolume."""
        r = run(["btrfs", "subvolume", "list", "-u", top])
        out = {}
        for line in r.stdout.splitlines():
            m = re.match(r"^ID \d+ .*? uuid (\S+) path (.+)$", line)
            if m:
                out[m.group(2)] = m.group(1)
        return out

    def subvol_delete(self, path):
        run(["btrfs", "subvolume", "delete", path])

    def subvol_sync(self, top):
        run(["btrfs", "subvolume", "sync", top])

    def usage(self, top):
        r = run(["btrfs", "filesystem", "usage", "-b", top], check=False)
        u = B.parse_btrfs_usage(r.stdout or "") or {}
        m = re.search(r"^\s*Free \(estimated\):\s+(\d+)", r.stdout or "", re.M)
        if m:
            u["free_estimated"] = int(m.group(1))
        return u

    def estimate(self, top, kept):
        """Bytes only the old side holds (`btrfs filesystem du`: what the whole top level holds, less what the kept
        trees hold). du counts uncompressed bytes: on a compressed disk the space that comes back is smaller."""
        def footprint(paths):
            r = run(["btrfs", "filesystem", "du", "-s", "--raw", *paths], check=False)
            if r.returncode != 0:
                return None
            n = 0
            for m in re.finditer(r"^\s*(\d+)\s+(\d+)\s+(\d+|-)\s+\S", r.stdout, re.M):
                n += int(m.group(2)) + (int(m.group(3)) if m.group(3) != "-" else 0)
            return n
        whole = footprint([top])
        k = footprint([os.path.join(top, p) for p in kept if os.path.isdir(os.path.join(top, p))])
        return None if whole is None or k is None else max(0, whole - k)

    def esp_sums(self):
        snap = {}
        m = B.Mounts()
        try:
            for d in B.lsblk():
                if d["parttype"] == B.ESP_TYPE and d.get("fstype") == "vfat":
                    p = m.ro(d["name"], "vfat")
                    snap[d["name"]] = B.tree_sums(p) if p else None
        finally:
            m.close()
        return snap

    def luks_headers(self):
        """sha256 of `cryptsetup luksDump` of every LUKS device (the header as cryptsetup reads it)."""
        out = {}
        if not shutil.which("cryptsetup"):
            return out
        for d in B.lsblk():
            if d.get("fstype") == "crypto_LUKS":
                r = run(["cryptsetup", "luksDump", d["name"]], check=False)
                out[d["name"]] = hashlib.sha256((r.stdout or "").encode()).hexdigest()
        return out

    def clear_immutable(self, root, files=False):
        """ostree marks each deployment's root directory immutable (FS_IMMUTABLE_FL): rm cannot remove it until the
        flag is cleared. Directories only, unless FILES."""
        dev = os.lstat(root).st_dev
        for d, dirs, fs in os.walk(root):
            dirs[:] = [x for x in dirs if os.lstat(os.path.join(d, x)).st_dev == dev]
            for p in [d] + ([os.path.join(d, f) for f in fs] if files else []):
                if os.path.islink(p):
                    continue
                try:
                    fd = os.open(p, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
                except OSError:
                    continue
                try:
                    buf = bytearray(struct.pack("i", 0))
                    fcntl.ioctl(fd, FS_IOC_GETFLAGS, buf)
                    flags = struct.unpack("i", buf)[0]
                    if flags & FS_IMMUTABLE_FL:
                        fcntl.ioctl(fd, FS_IOC_SETFLAGS, struct.pack("i", flags & ~FS_IMMUTABLE_FL))
                except OSError:
                    pass
                finally:
                    os.close(fd)

    def remove_tree(self, path):
        run(["rm", "-rf", "--one-file-system", path])

    def after_step(self, name):
        """Called after every --yes step (the tests interrupt a run here)."""


# ── what the machine is ───────────────────────────────────────────────────────────────────────────────────────────


def mount_at(mounts, target):
    """The mount at TARGET that is visible (the last one mounted there)."""
    hits = [m for m in mounts if m["target"] == target]
    return hits[-1] if hits else None


def on_old_side(rel):
    rel = rel.strip("/")
    return any(rel == p or rel.startswith(p + "/") for p in OLD_TOP + (TRASH,))


def never(rel):
    """The last guard before anything in the top level is moved or deleted: a kept tree, or the top level itself, is a
    failure whatever list named it."""
    parts = [p for p in os.path.normpath(rel).split("/") if p not in ("", ".")]
    if parts and parts[0] == TRASH:
        parts = parts[1:]
    if not parts or parts[0] in KEEP_TOP or ".." in parts:
        raise Failed(f"refusing to delete {rel!r}: {KEEP_TOP} and the top level itself are never deleted")


def bls_files(boot):
    """Every BLS entry file under /boot/loader*/entries once (/boot/loader is a link to loader.0 or loader.1)."""
    seen = {}
    for p in sorted(glob.glob(os.path.join(boot, "loader*", "entries", "*.conf"))):
        seen.setdefault(os.path.realpath(p), None)
    return sorted(seen)


def ostree_entries(boot):
    """The BLS entries ostree wrote (/boot/loader*/entries/*.conf whose options name ostree= or whose kernel is
    ostree's): [(path, title)]."""
    out = []
    for p in bls_files(boot):
        text = read(p, "") or ""
        linux = re.search(r"^\s*linux\s+(\S+)", text, re.M)
        if re.search(r"^\s*options\s.*\bostree=", text, re.M) or (
                linux and re.match(r"^(/boot)?/ostree/", linux.group(1))):
            t = re.search(r"^\s*title\s+(.+)$", text, re.M)
            out.append((p, t.group(1).strip() if t else os.path.basename(p)))
    return out


def rime_menu(boot):
    """The NixOS menu GRUB reads (custom.cfg's rime-boot block sourcing rime-nixos.cfg, at least one entry whose kernel
    is on /boot), or the reason there is none."""
    custom = read(os.path.join(boot, "grub2/custom.cfg"))
    if custom is None or not re.search(r"### BEGIN rime-boot.*?### END rime-boot ###", custom, re.S):
        return None, "custom.cfg has no rime-boot block"
    menu = read(os.path.join(boot, "grub2/rime-nixos.cfg"))
    if menu is None:
        return None, "rime-nixos.cfg is missing"
    kernels = re.findall(r"^\s*linux\s+(\S+)", menu, re.M)
    # GRUB's root is the btrfs top level when /boot is a directory there (plain A: /boot/rime/...) and the partition
    # itself when /boot is one (the encrypted installer layout: /rime/...); rime-boot writes whichever applies.
    ok = [k for k in kernels if any(k.startswith(p) and os.path.isfile(os.path.join(boot, "rime", k[len(p):]))
                                    for p in ("/boot/rime/", "/rime/")) and "/../" not in k]
    if not ok:
        return None, "rime-nixos.cfg names no kernel that is on /boot"
    return len(ok), None


def custom_outside(text):
    return re.sub(r"### BEGIN rime-boot.*?### END rime-boot ###\n?", "", text or "", flags=re.S)


def good_boots(sx):
    try:
        with open(os.path.join(sx.state, "good-boots.json")) as f:
            d = json.load(f)
        return [b for b in d.get("boots", []) if isinstance(b, dict) and b.get("boot_id")]
    except (OSError, ValueError, AttributeError):
        return []


def load_json(p):
    try:
        with open(p) as f:
            d = json.load(f)
        return d if isinstance(d, dict) else None
    except (OSError, ValueError):
        return None


def write_json(p, data):
    os.makedirs(os.path.dirname(p), mode=0o700, exist_ok=True)
    tmp = p + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=1, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.rename(tmp, p)
    fd = os.open(os.path.dirname(p), os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def in_use(sx, prefixes):
    """Processes whose cwd, root, executable, open files or mappings are under PREFIXES, loop devices backed by a file
    there and swap files there: [what]."""
    def under(p):
        p = re.sub(r" \(deleted\)$", "", p or "")
        return any(p == x or p.startswith(x + "/") for x in prefixes)

    hits = []
    for pid in sorted((d for d in os.listdir(sx.proc) if d.isdigit()), key=int):
        if int(pid) == sx.self_pid:
            continue
        base = os.path.join(sx.proc, pid)
        comm = (read(os.path.join(base, "comm"), "?") or "?").strip()
        paths = []
        for n in ("cwd", "root", "exe"):
            try:
                paths.append(os.readlink(os.path.join(base, n)))
            except OSError:
                pass
        try:
            for fd in os.listdir(os.path.join(base, "fd")):
                try:
                    paths.append(os.readlink(os.path.join(base, "fd", fd)))
                except OSError:
                    pass
        except OSError:
            pass
        for line in (read(os.path.join(base, "maps"), "", limit=1 << 24) or "").splitlines():
            f = line.split(None, 5)
            if len(f) == 6:
                paths.append(f[5])
        hit = next((p for p in paths if under(p)), None)
        if hit:
            hits.append(f"pid {pid} ({comm}) uses {hit}")
    for bf in glob.glob(os.path.join(sx.sys_block, "loop*", "loop", "backing_file")):
        p = (read(bf, "") or "").strip()
        if under(p):
            hits.append(f"{bf.split('/')[-3]} is backed by {p}")
    for line in (read(sx.swaps, "") or "").splitlines()[1:]:
        if line.split() and under(line.split()[0]):
            hits.append(f"swap on {line.split()[0]}")
    return hits


# ── preconditions ─────────────────────────────────────────────────────────────────────────────────────────────────


def preconditions(sx, writable):
    """(refusals [(name, text)], facts). Mounts the top level (read-only unless WRITABLE) only when the booted system
    is the moved one."""
    refusals, facts = [], {}
    fin = load_json(os.path.join(sx.state, "finish.json")) or {}
    facts["finish"] = fin
    if fin.get("done") is not None:
        return [("already-finished", f"{sx.state}/finish.json: the old side was removed on {fin['done']}; there is "
                 "nothing left to finish")], facts
    cmdline = read(sx.cmdline, "") or ""
    if os.path.exists(sx.ostree_booted) or re.search(r"(^|\s)ostree=", cmdline):
        refusals.append(("booted-ostree", "this is the old (bootc/ostree) system; finish runs on Rime OS on NixOS, "
                         "after `rime migrate start` (boot the Rime entry in the GRUB menu)"))
    elif not os.path.exists(sx.nixos_marker):
        refusals.append(("not-nixos", f"{sx.nixos_marker} is missing: this is not Rime OS on NixOS"))
    mounts = sx.mounts()
    facts["mounts"] = mounts
    root, home = mount_at(mounts, "/"), mount_at(mounts, "/var/home")
    if not root or root["fstype"] != "btrfs" or root["root"] != "/@nixos":
        refusals.append(("root-not-nixos-subvol", "/ is not the @nixos subvolume ("
                         + (f"{root['fstype']} {root['source']}[{root['root']}]" if root else "no / mount") + ")"))
    if not home or home["fstype"] != "btrfs" or home["root"] != "/@home" or not root \
            or home["source"] != root["source"]:
        refusals.append(("home-not-from-home-subvol", "/var/home is not the @home subvolume of the root filesystem ("
                         + (f"{home['fstype']} {home['source']}[{home['root']}]" if home else "not a mount") + ")"))
    if refusals:
        return refusals, facts
    source = root["source"]
    facts["source"] = source

    boots = good_boots(sx)
    facts["good_boots"] = boots
    if len(boots) < GOOD_BOOTS:
        refusals.append(("too-few-good-boots", f"NixOS has booted well {len(boots)} time(s) since the move; finish "
                         f"needs {GOOD_BOOTS} (a boot counts once it reaches multi-user.target from @nixos: "
                         "rime-migrate-good-boot.service). Use this system, reboot, and run finish again"))

    # Mounts of the old side: the top level is mounted by the system itself (rime.rootfs topMount, for /boot's bind),
    # which is allowed; a mount OF something under the old side, or anything mounted below it, is not.
    prefixes, mounted = [], []
    for m in mounts:
        if m["source"] == source and m["fstype"] == "btrfs":
            if m["root"] == "/":
                prefixes += [os.path.join(m["target"], p) for p in OLD_TOP + (TRASH,)]
            elif on_old_side(m["root"]):
                mounted.append(f"{m['target']} ({m['root']})")
            elif m["root"] == "/boot":
                prefixes.append(os.path.join(m["target"], "ostree"))
            elif m["root"].startswith("/boot/ostree"):
                mounted.append(f"{m['target']} ({m['root']})")
    bm = mount_at(mounts, sx.boot if sx.boot.startswith("/") else "/boot")
    if bm and bm["source"] != source:
        prefixes.append(os.path.join(bm["target"], "ostree"))
    for m in mounts:
        if any(m["target"].startswith(p + "/") or m["target"] == p for p in prefixes):
            mounted.append(f"{m['target']} ({m['fstype']} {m['source']})")
    if mounted:
        refusals.append(("old-side-mounted", "the old side is mounted: " + "; ".join(sorted(set(mounted))[:5])))
    users = in_use(sx, prefixes)
    if users:
        refusals.append(("old-side-in-use", "the old side is in use: " + "; ".join(users[:5])
                         + (f" (+{len(users) - 5} more)" if len(users) > 5 else "")))

    stats = sx.device_stats("/")
    if stats is None:
        refusals.append(("btrfs-stats-unreadable", "`btrfs device stats /` printed nothing"))
    elif stats:
        refusals.append(("btrfs-errors", "btrfs device stats are not zero ("
                         + ", ".join(f"{k} {v}" for k, v in sorted(stats.items())) + "): find out why first "
                         "(`sudo btrfs device stats /`, a scrub); finish deletes nothing on a filesystem that reports "
                         "errors"))

    n, why = rime_menu(sx.boot)
    facts["rime_menu"] = n
    if not n:
        refusals.append(("nixos-menu-missing", f"{why}: without it, removing the ostree entries leaves GRUB nothing to "
                         "boot"))
    if re.search(r"\bostree=", read(os.path.join(sx.boot, "grub2/grub.cfg"), "") or ""):
        refusals.append(("grub-cfg-ostree-inline", "/boot/grub2/grub.cfg names ostree= itself (not bootupd's static "
                         "file): finish never rewrites grub.cfg, and its entries would point at deleted files"))
    if re.search(r"\bostree\b", custom_outside(read(os.path.join(sx.boot, "grub2/custom.cfg"), ""))):
        refusals.append(("custom-cfg-ostree-entry", "custom.cfg names ostree outside the rime-boot block: finish never "
                         "edits custom.cfg (it is yours); remove that entry first"))

    top = sx.mount_top(source, writable)
    facts["top"] = top
    found = sorted(glob.glob(os.path.join(top, RECORD_GLOB)))
    saved = load_json(os.path.join(sx.state, "start-record.json"))
    if len(found) > 1:
        refusals.append(("start-record-ambiguous", "more than one start record: " + ", ".join(
            "/" + os.path.relpath(p, top) for p in found)))
        return refusals, facts
    if found:
        try:
            with open(found[0]) as f:
                rec = json.load(f)
            rec = rec if isinstance(rec, dict) else {"unreadable": "not a JSON object"}
        except (OSError, ValueError) as e:
            rec = {"unreadable": str(e)}
        facts["record_path"] = found[0]
    elif fin.get("started") and saved:
        rec = saved          # an interrupted finish already removed ostree/; it saved the record first
        facts["record_path"] = os.path.join(sx.state, "start-record.json")
    else:
        rec = None
    facts["record"] = rec
    r = B.record_refusal(rec) if rec is not None else ("start-record-missing", "")
    if r is None or r[0] != "already-moved":
        name = r[0] if r and r[0] != "already-moved" else "start-record-not-done"
        text = {"start-record-missing": f"no start record ({RECORD_GLOB} in the top level): this machine was not "
                "moved by `rime migrate start`, or the record is gone; finish deletes nothing it cannot account for",
                }.get(name, f"the start record does not say done ({r[1] if r else 'a start that rolled back'})")
        refusals.append((name, text))
    return refusals, facts


# ── the plan ──────────────────────────────────────────────────────────────────────────────────────────────────────


def plan(sx, facts, measure=True):
    top = facts["top"]
    p = {"entries": ostree_entries(sx.boot), "kernels": os.path.isdir(os.path.join(sx.boot, "ostree")),
         "trees": [], "nested": []}
    subs = sx.subvolumes(top)
    for n in OLD_TOP:
        for base in (n, os.path.join(TRASH, n)):
            if os.path.lexists(os.path.join(top, base)):
                p["trees"].append(base)
    if os.path.lexists(os.path.join(top, TRASH)) and not p["trees"]:
        p["trees"].append(TRASH)
    p["nested"] = sorted((s for s in subs if on_old_side(s) and s.strip("/") not in (
        "@pre-nixos", os.path.join(TRASH, "@pre-nixos"))), key=lambda s: (-s.count("/"), s))
    p["usage"] = sx.usage(top)
    p["estimate"] = sx.estimate(top, KEEP_TOP) if measure and p["trees"] else (0 if not p["trees"] else None)
    p["kernels_bytes"] = B.du_bytes(os.path.join(sx.boot, "ostree")) if p["kernels"] else 0
    return p


def plan_text(facts, p):
    rec = facts.get("record") or {}
    out = [f"rime migrate finish: removes the old (bootc) system this machine was moved from on {rec.get('done', '?')}",
           "",
           "will be deleted, in this order:"]
    if p["entries"]:
        out.append(f"  1. the GRUB menu's {len(p['entries'])} ostree entr{'y' if len(p['entries']) == 1 else 'ies'}:")
        out += [f"       {t}  ({os.path.basename(f)})" for f, t in p["entries"]]
    else:
        out.append("  1. the GRUB menu's ostree entries: none left")
    out.append(f"  2. bootc's kernels in /boot/ostree ({gb(p['kernels_bytes'])})" if p["kernels"]
               else "  2. bootc's kernels in /boot/ostree: gone already")
    names = {"@pre-nixos": "@pre-nixos (the read-only snapshot start took of the old system)",
             "ostree": "ostree/ (the old system: its repository, its deployments and its /var, with home as it was "
                       "at the move)"}
    if p["trees"]:
        out.append("  3. in the btrfs top level:")
        out += [f"       {names.get(os.path.basename(t), t)}" for t in p["trees"]]
        out += [f"       nested subvolume {s}" for s in p["nested"]]
    else:
        out.append("  3. in the btrfs top level: nothing left")
    u = p.get("usage") or {}
    out += ["", "space:",
            f"  should free about {gb(p['estimate'])} of data (btrfs filesystem du: what only the old side holds; "
            "uncompressed bytes, so on a compressed disk somewhat less comes back)" if p["estimate"] is not None
            else "  should free: not measured",
            f"  now: {gb(u.get('data_used'))} data used, {gb(u.get('free_estimated'))} free (estimated)",
            "", "what stops working:",
            "  the GRUB menu's ostree entries: the old system, and home as it was at the move, can no longer be booted",
            "  from this disk. The backup `rime migrate start` took (off this disk) is the only copy left; keep it.",
            "", "what stays (never touched; compared before and after):",
            "  @nixos, @nix, @home; /boot/rime (NixOS's kernels); /boot/grub2 (custom.cfg with the rime-boot block,",
            "  the Secure Boot key entry and anything else in it, rime-nixos.cfg, grub.cfg); every ESP (shim, GRUB,",
            "  MokManager); the LUKS header and every key."]
    return "\n".join(out) + "\n"


# ── --yes ─────────────────────────────────────────────────────────────────────────────────────────────────────────


def containment(sx, top):
    """What finish must leave byte for byte."""
    subs = sx.subvolumes(top)
    other_top = sorted(n for n in os.listdir(top) if n not in OLD_TOP + (TRASH,))
    entries = {e for e, _ in ostree_entries(sx.boot)}
    loaders = {os.path.relpath(p, sx.boot): B.sha256_file(p)
               for p in bls_files(sx.boot) if p not in entries}
    return {"grub2": B.tree_sums(os.path.join(sx.boot, "grub2")) if os.path.isdir(os.path.join(sx.boot, "grub2")) else {},
            "rime": B.tree_sums(os.path.join(sx.boot, "rime")) if os.path.isdir(os.path.join(sx.boot, "rime")) else {},
            "other_entries": loaders,
            "kept_subvolumes": {k: subs.get(k) for k in ("@nixos", "@nix", "@home")},
            "top_level": other_top, "esp": sx.esp_sums(), "luks": sx.luks_headers()}


def cmd_yes(sx, facts):
    top = facts["top"]
    path = os.path.join(sx.state, "finish.json")
    fin = facts["finish"] if facts["finish"].get("started") else {"started": now(), "steps": []}
    fin.pop("failed", None)
    fin.setdefault("steps", [])
    # The start record lives in the old /var (ostree/): saved on @nixos before anything is deleted.
    if not os.path.exists(os.path.join(sx.state, "start-record.json")):
        write_json(os.path.join(sx.state, "start-record.json"), facts["record"])
    p = plan(sx, facts, measure=False)
    fin.setdefault("usage_before", p["usage"])
    write_json(path, fin)
    before = containment(sx, top)

    def step(name):
        if name not in fin["steps"]:
            fin["steps"].append(name)
        write_json(path, fin)
        print(f"== {name}", flush=True)
        sx.after_step(name)

    try:
        # 1. The way back first: once the old system cannot be booted, a half-deleted one never is.
        removed = fin.setdefault("removed_entries", [])
        dirs = set()
        for f, t in ostree_entries(sx.boot):
            os.unlink(f)
            dirs.add(os.path.dirname(f))
            removed.append(t)
        for d in dirs:
            fd = os.open(d, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        step("boot entries")
        kdir = os.path.join(sx.boot, "ostree")
        if os.path.lexists(kdir):
            sx.clear_immutable(kdir)
            sx.remove_tree(kdir)
        step("old kernels")
        # 2. One rename each out of the top level: the old side is never half there under its own name.
        for n in MOVE_ASIDE:
            src, dst = os.path.join(top, n), os.path.join(top, TRASH, n)
            never(n)
            if os.path.lexists(src):
                os.makedirs(os.path.join(top, TRASH), mode=0o700, exist_ok=True)
                os.rename(src, dst)
        step("move aside")
        if os.path.lexists(os.path.join(top, TRASH)):
            sx.clear_immutable(os.path.join(top, TRASH))   # a subvolume's directory entry is in its parent
        subs = sx.subvolumes(top)
        trash_pre = os.path.join(TRASH, "@pre-nixos")
        for s in sorted((s for s in subs if s.startswith(TRASH + "/") and s != trash_pre),
                        key=lambda s: (-s.count("/"), s)):
            never(s)
            if os.path.lexists(os.path.join(top, s)):
                sx.subvol_delete(os.path.join(top, s))
        step("nested subvolumes")
        for pre in ("@pre-nixos", trash_pre):        # deleted where it is (read-only: see MOVE_ASIDE)
            never(pre)
            if os.path.lexists(os.path.join(top, pre)):
                sx.subvol_delete(os.path.join(top, pre))
        step("@pre-nixos")
        t = os.path.join(top, TRASH)
        if os.path.lexists(t):
            sx.clear_immutable(t)
            try:
                sx.remove_tree(t)
            except Failed:
                sx.clear_immutable(t, files=True)
                sx.remove_tree(t)
        step("old tree")
        sx.subvol_sync(top)
        step("sync")
        after = containment(sx, top)
        diff = B.containment_diff(before, after)
        if diff:
            raise Failed(f"containment: finish changed what it must not: {json.dumps(diff)}")
        left = [f for f, _ in ostree_entries(sx.boot)] + [n for n in OLD_TOP + (TRASH,)
                                                           if os.path.lexists(os.path.join(top, n))]
        if os.path.lexists(kdir):
            left.append(kdir)
        if left:
            raise Failed("the old side is still there after every step: " + ", ".join(left))
        step("containment")
    except (Failed, OSError) as e:
        # an OSError (a rename or unlink the kernel refused) is a failure like any other: recorded, exit 10, re-runnable
        fin["failed"] = str(e)
        write_json(path, fin)
        if isinstance(e, Failed):
            raise
        raise Failed(f"{e.strerror or e}: {e.filename or ''}{' -> ' + e.filename2 if e.filename2 else ''}") from e
    fin["usage_after"] = sx.usage(top)
    fin["done"] = now()
    write_json(path, fin)
    ub, ua = fin.get("usage_before") or {}, fin["usage_after"] or {}
    print(f"done: the old system is gone. Data used {gb(ub.get('data_used'))} -> {gb(ua.get('data_used'))}; "
          f"free (estimated) {gb(ub.get('free_estimated'))} -> {gb(ua.get('free_estimated'))}. "
          f"Removed {len(fin['removed_entries'])} ostree GRUB entr{'y' if len(fin['removed_entries']) == 1 else 'ies'}; "
          "the Rime entries and everything else in custom.cfg stay.", flush=True)
    return EXIT_OK


# ── --record-boot ─────────────────────────────────────────────────────────────────────────────────────────────────


def record_boot(sx):
    root = mount_at(sx.mounts(), "/")
    if os.path.exists(sx.ostree_booted) or not os.path.exists(sx.nixos_marker) or not root \
            or root["fstype"] != "btrfs" or root["root"] != "/@nixos":
        print("not counted: / is not @nixos on Rime OS on NixOS")
        return EXIT_OK
    bid = (read(sx.boot_id, "") or "").strip()
    if not bid:
        return EXIT_OK
    boots = good_boots(sx)
    if not any(b["boot_id"] == bid for b in boots):
        try:
            system = os.readlink(sx.booted_system)
        except OSError:
            system = None
        boots.append({"boot_id": bid, "at": now(), "system": system})
        write_json(os.path.join(sx.state, "good-boots.json"), {"boots": boots[-GOOD_BOOTS_KEPT:]})
    n = len(boots)
    done = (load_json(os.path.join(sx.state, "finish.json")) or {}).get("done") is not None
    if done:
        print(f"good NixOS boot {n}; the old side was removed already")
    elif n >= GOOD_BOOTS:
        print(f"good NixOS boot {n}: `sudo rime migrate finish` can remove the old system now "
              "(`rime migrate finish --dry-run` shows what it would delete)")
    else:
        print(f"good NixOS boot {n} of the {GOOD_BOOTS} `rime migrate finish` needs")
    return EXIT_OK


# ── main ──────────────────────────────────────────────────────────────────────────────────────────────────────────


def in_private_namespace(argv):
    """Re-run in a private mount namespace: the top-level mount goes away when this exits."""
    if os.environ.get("RIME_MIGRATE_FINISH_NS") == "1":
        return
    env = dict(os.environ, RIME_MIGRATE_FINISH_NS="1")
    os.execvpe("unshare", ["unshare", "--mount", "--propagation", "private", "--",
                           sys.executable, "-I", os.path.realpath(__file__), *argv], env)


def main(argv=None, sx=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    ap = argparse.ArgumentParser(prog=PROG, description=__doc__.split("\n\n")[0])
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--dry-run", action="store_true", help="write nothing; say what --yes would do (the default)")
    g.add_argument("--yes", action="store_true", help="delete the old side")
    g.add_argument("--record-boot", action="store_true", help=argparse.SUPPRESS)
    a = ap.parse_args(argv)
    if sx is None:
        if os.geteuid() != 0:
            print(f"{PROG}: needs root (sudo rime migrate finish{' --yes' if a.yes else ''})", file=sys.stderr)
            return EXIT_USAGE
        if TOOLS:
            os.environ["PATH"] = TOOLS + ":" + os.environ.get("PATH", "")
        if a.record_boot:
            return record_boot(System())
        in_private_namespace(argv)
        sx = System()
    elif a.record_boot:
        return record_boot(sx)
    try:
        refusals, facts = preconditions(sx, writable=a.yes)
        if refusals:
            for name, text in refusals:
                print(f"REFUSED [{name}] {text}")
            return EXIT_REFUSED
        if a.yes:
            return cmd_yes(sx, facts)
        sys.stdout.write(plan_text(facts, plan(sx, facts)))
        print("nothing was changed (dry run). To delete the old side: sudo rime migrate finish --yes")
        return EXIT_OK
    except Refused as e:
        print(f"REFUSED [{e.name}] {e}")
        return EXIT_REFUSED
    except Failed as e:
        print(f"{PROG}: FAILED: {e} (run it again: every step it finished is recorded and skipped)", file=sys.stderr)
        return EXIT_FAILED


TOOLS = ""   # the package writes the store paths of mount, btrfs, rm and cryptsetup here

if __name__ == "__main__":
    sys.exit(main())
