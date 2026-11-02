#!/usr/bin/env python3
"""rime-migrate: the bridge from a bootc Rime OS machine to Rime OS on NixOS (PLAN.md D1, D2, D3).

  check [--explain] [--json] [--backup DIR] [--disk DEV ...]
      Writes nothing. Names the machine's boot topology (A-F, PLAN.md D1 / out/understand-boot.md), the free space
      against the disk figure (H9), the home size, the backup destination, the signer the old system trusts (X3),
      and every reason the machine is refused, by name. --disk DEV probes an unbooted disk read-only instead of the
      running system (every mount read-only, btrfs with rescue=nologreplay, ext4 with noload).
  repair-e [--confirm]
      Topology E only (a failed boot-v2 trial, katana): lists the trial's leftovers on Rime's own ESP (\\EFI\\systemd\\,
      \\EFI\\Linux\\, \\loader\\) and its NVRAM entry, archives them under /var/lib/rime/boot-migrate/archive-*/,
      and with --confirm deletes them (the NVRAM entry through efivarfs). Never a Windows ESP, never BootOrder,
      never the entry that booted the machine, never the phase file.
  start --backup DIR --payload DIR --yes
      Topologies A and E only. Backup (off the disk, restore-tested, LUKS header), old side masked, a read-only
      @pre-nixos snapshot, new subvolumes @nixos (/), @nix (/nix), @home (/var/home, a reflink copy), the payload's
      closure installed and registered, the /etc carry (rime-hostlayer carry), the first activation, and the boot
      entry through the Rime boot hook in custom.cfg. The ostree entries stay; nothing is written to any ESP or NVRAM.
      The fsck containment gate (H8) runs before and after, and a failure removes what the bridge made.

Exit codes: 0 ok, 2 usage, 3 refused (by name: "REFUSED [name] ..."), 10 failed.
Never runs bootctl, grub2-install or efibootmgr, and never uses NIXOS_LUSTRATE.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import time
import uuid

PROG = "rime-migrate"
EXIT_OK, EXIT_USAGE, EXIT_REFUSED, EXIT_FAILED = 0, 2, 3, 10
GB = 10**9

# A11 disk floor as `rime update` is designed (docs/nixos/closure.md §6): one system installed, room for one mass
# rebuild (~10.5 GB unpacked) and boot. H9 adds what the old copies can pin: home and the old store, if rewritten.
FLOOR_BYTES = int(os.environ.get("RIME_MIGRATE_FLOOR_BYTES", 19 * GB))
RENAMED_SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main"
SIGNER_OVERRIDE = "usr/share/rime-os/trust/expected-signer"
STATE = "/var/lib/rime/nixos-bridge"
BOOT_MIGRATE_DIRS = ("var/lib/rime/boot-migrate", "var/lib/apex/boot-migrate")
ESP_TYPE = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
EFI_GLOBAL = "8be4df61-93ca-11d2-aa0d-00e098032b8c"
EFIVARS = os.environ.get("RIME_MIGRATE_EFIVARS", "/sys/firmware/efi/efivars")
ACCEPTED = ("A", "E")
SUBVOLS = ("@nixos", "@nix", "@home")
ROOT_LABEL = "rime-root"
OLD_LABELS = ("rime-root", "apex-root")
# Old-side units masked at start (D2): the update timer, the store compactor, bootupd's own update, boot-v2.
MASK_UNITS = ("rime-update-check.timer", "rime-update-check.service", "rime-store-compact.timer",
              "rime-store-compact.service", "bootloader-update.service", "rime-boot-migrate-confirm.service")
WAIT_UNITS = ("rime-store-compact.service", "rime-migrate-from-apex.service")
TRIAL_LEFTOVERS = ("EFI/systemd", "EFI/Linux", "loader")

TOPOLOGY_TEXT = {
    "A": "ostree + bootupd GRUB, shim in \\EFI\\fedora on Rime's own ESP (installer default)",
    "B": "ostree + GRUB in the Windows ESP (ISO partition mode)",
    "C": "a second Rime ESP made by the Windows installer (\\EFI\\rimeinst\\)",
    "D": "legacy \\EFI\\APEX\\SHIMX64.EFI on another disk's (Windows) ESP",
    "E": "a failed boot-v2 migration's leftovers (phase=failed) on top of A, B or D",
    "F": "composefs + unsigned systemd-boot, Type #1 entries on the ESP (a confirmed boot-v2 migration)",
    "unknown": "no boot topology Rime knows",
}


class Refused(Exception):
    def __init__(self, name, text):
        super().__init__(text)
        self.name = name


class Failed(Exception):
    pass


def log(msg):
    print(f"{PROG}: {msg}", file=sys.stderr, flush=True)


def run(cmd, check=True, capture=True, **kw):
    r = subprocess.run(cmd, check=False, text=True, stdout=subprocess.PIPE if capture else None,
                       stderr=subprocess.PIPE if capture else None, **kw)
    if check and r.returncode != 0:
        raise Failed(f"{' '.join(cmd)} exited {r.returncode}: {(r.stderr or '').strip()[-400:]}")
    return r


def read(path, default=None, limit=1 << 20):
    try:
        with open(path, "rb") as f:
            return f.read(limit).decode("utf-8", "replace")
    except OSError:
        return default


def du_bytes(path):
    """Allocated bytes under PATH on its own filesystem (hard links once)."""
    total, seen = 0, set()
    try:
        dev = os.lstat(path).st_dev
    except OSError:
        return 0
    for d, dirs, files in os.walk(path):
        try:
            if os.lstat(d).st_dev != dev:
                dirs[:] = []
                continue
        except OSError:
            continue
        for n in files + dirs:
            try:
                st = os.lstat(os.path.join(d, n))
            except OSError:
                continue
            if st.st_nlink > 1:
                if (st.st_dev, st.st_ino) in seen:
                    continue
                seen.add((st.st_dev, st.st_ino))
            total += st.st_blocks * 512
    return total


def find_ci(base, rel):
    """BASE/REL with every component matched case-insensitively (FAT), or None."""
    cur = base
    for part in [p for p in rel.replace("\\", "/").split("/") if p]:
        try:
            names = os.listdir(cur)
        except OSError:
            return None
        m = [n for n in names if n.lower() == part.lower()]
        if not m:
            return None
        cur = os.path.join(cur, m[0])
    return cur


# ── NVRAM (read-only parsing; the only write is repair-e's delete of one trial entry) ─────────────────────────


def parse_load_option(data):
    attrs, fplen = struct.unpack_from("<IH", data, 0)
    end = 6
    while end + 1 < len(data) and data[end:end + 2] != b"\0\0":
        end += 2
    desc = data[6:end].decode("utf-16-le", "replace")
    fp = data[end + 2:end + 2 + fplen]
    path = partuuid = None
    j = 0
    while j + 4 <= len(fp):
        t, st, ln = fp[j], fp[j + 1], struct.unpack_from("<H", fp, j + 2)[0]
        if ln < 4:
            break
        body = fp[j + 4:j + ln]
        if t == 4 and st == 1 and len(body) >= 38 and body[37] == 2:
            partuuid = str(uuid.UUID(bytes_le=bytes(body[20:36])))
        elif t == 4 and st == 4:
            path = body.decode("utf-16-le", "replace").rstrip("\0")
        elif t == 0x7F and st == 0xFF:
            break
        j += ln
    return {"label": desc, "path": path, "partuuid": partuuid, "active": bool(attrs & 1)}


def efivar(name):
    try:
        with open(os.path.join(EFIVARS, f"{name}-{EFI_GLOBAL}"), "rb") as f:
            return f.read()[4:]
    except OSError:
        return None


def nvram_facts():
    if not os.path.isdir(EFIVARS):
        return {"available": False}
    out = {"available": True, "entries": {}, "boot_current": None, "boot_order": [], "secure_boot": None}
    cur = efivar("BootCurrent")
    if cur and len(cur) >= 2:
        out["boot_current"] = "%04X" % struct.unpack_from("<H", cur)[0]
    order = efivar("BootOrder") or b""
    out["boot_order"] = ["%04X" % struct.unpack_from("<H", order, i)[0] for i in range(0, len(order) - 1, 2)]
    sb = efivar("SecureBoot")
    out["secure_boot"] = bool(sb[0]) if sb else None
    for fn in sorted(os.listdir(EFIVARS)):
        m = re.fullmatch(r"Boot([0-9A-F]{4})-" + EFI_GLOBAL, fn)
        if m:
            data = efivar(f"Boot{m.group(1)}")
            if data:
                try:
                    out["entries"][m.group(1)] = parse_load_option(data)
                except (struct.error, IndexError):
                    out["entries"][m.group(1)] = {"label": "?", "path": None, "partuuid": None, "unparsed": True}
    return out


# ── probing ─────────────────────────────────────────────────────────────────────────────────────────────────────


class Mounts:
    """Read-only mounts in a private directory, undone in reverse order."""

    def __init__(self):
        self.base = tempfile.mkdtemp(prefix="rime-migrate-")
        self.stack = []

    def ro(self, dev, fstype, extra=""):
        d = os.path.join(self.base, str(len(self.stack)))
        os.mkdir(d)
        opts = {"btrfs": "ro,rescue=nologreplay", "ext4": "ro,noload", "xfs": "ro,norecovery",
                "vfat": "ro"}.get(fstype, "ro")
        if extra:
            opts += "," + extra
        r = run(["mount", "-t", fstype, "-o", opts, dev, d], check=False)
        if r.returncode != 0:
            os.rmdir(d)
            return None
        self.stack.append(d)
        return d

    def close(self):
        for d in reversed(self.stack):
            run(["umount", d], check=False)
            try:
                os.rmdir(d)
            except OSError:
                pass
        self.stack = []
        try:
            os.rmdir(self.base)
        except OSError:
            pass


def lsblk(disks=()):
    r = run(["lsblk", "-J", "-b", "-l", "-p", "-o",
             "NAME,KNAME,TYPE,FSTYPE,UUID,LABEL,PARTTYPE,PARTUUID,PKNAME,SIZE,MOUNTPOINTS", *disks])
    devs = json.loads(r.stdout)["blockdevices"]
    for d in devs:
        d["parttype"] = (d.get("parttype") or "").lower()
        d["partuuid"] = (d.get("partuuid") or "").lower()
        d["mountpoints"] = [m for m in (d.get("mountpoints") or []) if m]
    return devs


def disk_of(devs, name):
    by = {d["name"]: d for d in devs}
    by.update({d["kname"]: d for d in devs})
    d = by.get(name)
    seen = 0
    while d is not None and d.get("type") != "disk" and d.get("pkname") and seen < 8:
        d = by.get(d["pkname"])
        seen += 1
    return d["name"] if d else None


def esp_facts(dev, path, disk):
    f = {"device": dev["name"], "disk": disk, "uuid": dev.get("uuid"), "partuuid": dev["partuuid"],
         "size": int(dev.get("size") or 0), "dirs": [], "loader_entries": [], "grub": {}, "mounted": path is not None}
    if path is None:
        return f
    efi = find_ci(path, "EFI")
    if efi:
        f["dirs"] = sorted(n for n in os.listdir(efi) if os.path.isdir(os.path.join(efi, n)))
        for n in f["dirs"]:
            g = find_ci(efi, f"{n}/grub.cfg")
            b = find_ci(efi, f"{n}/bootuuid.cfg")
            if g or b:
                m = re.search(r'BOOT_UUID="?([0-9A-Fa-f-]+)"?', read(b, "") if b else "")
                f["grub"][n.lower()] = {"grub_cfg": read(g, "", 1 << 16) if g else "",
                                        "boot_uuid": m.group(1).lower() if m else None}
        # shim in \EFI\BOOT with its fallback and no GRUB there hands over to \EFI\fedora (BOOTX64.CSV)
        f["fallback_csv"] = bool(find_ci(efi, "BOOT/fbx64.efi") and not find_ci(efi, "BOOT/grubx64.efi")
                                 and find_ci(efi, "fedora/BOOTX64.CSV"))
    le = find_ci(path, "loader/entries")
    if le:
        f["loader_entries"] = sorted(os.listdir(le))
    f["top"] = sorted(os.listdir(path))
    return f


def deployment_dirs(top):
    out = []
    base = os.path.join(top, "ostree/deploy")
    if os.path.isdir(base):
        for sr in sorted(os.listdir(base)):
            dd = os.path.join(base, sr, "deploy")
            if os.path.isdir(dd):
                out += [os.path.join(dd, n) for n in sorted(os.listdir(dd)) if re.fullmatch(r"[0-9a-f]{64}\.\d+", n)]
    return out


def signer_facts(usr_roots):
    """X3: does the old system know the post-rename signer? Its override file decides when present, else the
    identity compiled into its rime (or apex) CLI."""
    for root in usr_roots:
        ov = os.path.join(root, SIGNER_OVERRIDE)
        if os.path.isfile(ov):
            ids = [ln.strip() for ln in read(ov, "").splitlines() if ln.strip() and not ln.startswith("#")]
            return {"renamed_known": RENAMED_SIGNER in ids, "evidence": f"/{SIGNER_OVERRIDE}"}
        for b in ("usr/bin/rime", "usr/bin/apex"):
            p = os.path.join(root, b)
            if os.path.isfile(p):
                with open(p, "rb") as f:
                    data = f.read()
                return {"renamed_known": RENAMED_SIGNER.encode() in data, "evidence": f"/{b}"}
    return {"renamed_known": False, "evidence": "no rime CLI and no signer override found"}


def migration_facts(var_root, top):
    m = {"phase": None, "state_dir": None, "files": [], "composefs_leftover": False}
    for d in BOOT_MIGRATE_DIRS:
        p = os.path.join(var_root, d.split("/", 1)[1]) if var_root else None
        if p and os.path.isdir(p):
            m["state_dir"] = "/" + d
            m["files"] = sorted(os.listdir(p))
            m["phase"] = (read(os.path.join(p, "phase"), "") or "").strip() or None
            break
    if top:
        m["composefs_leftover"] = os.path.isdir(os.path.join(top, "composefs"))
    return m


def root_facts_from(top, dev, devs, src_fsroot="/"):
    st = os.statvfs(top)
    return {"device": dev["name"], "disk": disk_of(devs, dev["name"]), "fstype": dev.get("fstype"),
            "uuid": dev.get("uuid"), "label": dev.get("label"), "free_bytes": st.f_bavail * st.f_frsize,
            "size_bytes": st.f_blocks * st.f_frsize, "subvol": src_fsroot}


def luks_of(devs, root_dev):
    if root_dev.get("type") != "crypt":
        return None
    by = {d["name"]: d for d in devs}
    by.update({d["kname"]: d for d in devs})
    parent = by.get(root_dev.get("pkname"))
    return {"device": parent["name"] if parent else None, "uuid": parent.get("uuid") if parent else None,
            "mapper": root_dev["name"]}


def collect_live():
    """Facts about the running bootc system. Mounts the btrfs top level and the ESPs read-only, privately."""
    mounts = Mounts()
    try:
        cmdline = read("/proc/cmdline", "")
        devs = lsblk()
        sysroot = "/sysroot" if os.path.ismount("/sysroot") else "/"
        r = run(["findmnt", "-n", "-J", "-o", "SOURCE,FSTYPE,UUID,FSROOT", "--target", sysroot])
        fm = json.loads(r.stdout)["filesystems"][0]
        src = re.sub(r"\[.*\]$", "", fm["source"])
        real = os.path.realpath(src)
        rdev = next((d for d in devs if d["name"] == src or os.path.realpath(d["name"]) == real), None)
        if rdev is None:
            raise Failed(f"cannot find the root block device {src} in lsblk")
        top = mounts.ro(rdev["name"], fm["fstype"], "subvolid=5" if fm["fstype"] == "btrfs" else "")
        if top is None:
            raise Failed(f"cannot mount {rdev['name']} read-only")
        facts = {"mode": "live", "cmdline": cmdline.strip()}
        facts["root"] = root_facts_from(top, rdev, devs, fm.get("fsroot") or "/")
        facts["root"]["luks"] = luks_of(devs, rdev)
        facts["ostree"] = ostree_facts(top, booted=os.path.exists("/run/ostree-booted"),
                                       composefs_native="composefs=" in cmdline)
        b = json.loads(run(["findmnt", "-n", "-J", "-o", "SOURCE,FSTYPE,UUID,FSROOT", "--target", "/boot"]).stdout)
        bf = b["filesystems"][0]
        same = (bf.get("uuid") or "").lower() == (rdev.get("uuid") or "").lower()
        facts["boot"] = boot_facts("/boot", "top-level-dir" if same else "partition",
                                   (bf.get("uuid") or "").lower(), bf.get("fstype"))
        v = json.loads(run(["findmnt", "-n", "-J", "-o", "FSROOT", "--target", "/var"]).stdout)["filesystems"][0]
        facts["var_fsroot"] = v["fsroot"]
        facts["home"] = {"path": "/var/home", "bytes": du_bytes("/var/home")}
        facts["migration"] = migration_facts("/var", top)
        facts["signer"] = signer_facts(["/"])
        facts["keymap"] = keymap_of("/etc")
        facts["esps"] = esps(devs, mounts)
        facts["nvram"] = nvram_facts()
        return facts
    finally:
        mounts.close()


def collect_disks(disks):
    """Facts about unbooted disks (the --disk mode): everything mounted read-only, nothing written."""
    mounts = Mounts()
    try:
        devs = lsblk(disks)
        facts = {"mode": "disk", "disks": list(disks), "cmdline": ""}
        top = rdev = None
        for d in devs:
            if d.get("fstype") in ("btrfs", "ext4", "xfs") and d["parttype"] != ESP_TYPE:
                t = mounts.ro(d["name"], d["fstype"], "subvolid=5" if d["fstype"] == "btrfs" else "")
                if t and (os.path.isdir(os.path.join(t, "ostree")) or os.path.isdir(os.path.join(t, "composefs"))):
                    top, rdev = t, d
                    break
        if top is None:
            luks = [d for d in devs if d.get("fstype") == "crypto_LUKS"]
            facts["root"] = {"fstype": "crypto_LUKS" if luks else None, "device": luks[0]["name"] if luks else None,
                             "disk": disk_of(devs, luks[0]["name"]) if luks else None, "free_bytes": 0,
                             "luks": {"device": luks[0]["name"], "uuid": luks[0].get("uuid")} if luks else None,
                             "locked": bool(luks)}
            facts["ostree"] = {"deployments": [], "store_bytes": 0, "booted": False, "composefs_native": False}
            facts["boot"] = {"kind": "unknown"}
            facts["migration"] = {"phase": None, "files": [], "composefs_leftover": False}
            facts["signer"] = {"renamed_known": False, "evidence": "root not readable"}
            facts["home"] = {"bytes": 0}
        else:
            facts["root"] = root_facts_from(top, rdev, devs)
            facts["root"]["luks"] = None
            facts["ostree"] = ostree_facts(top, booted=False, composefs_native=False)
            sr = sorted(os.listdir(os.path.join(top, "ostree/deploy"))) if os.path.isdir(
                os.path.join(top, "ostree/deploy")) else []
            var = os.path.join(top, "ostree/deploy", sr[0], "var") if sr else os.path.join(top, "state/os/default/var")
            facts["var_fsroot"] = "/" + os.path.relpath(var, top)
            facts["home"] = {"path": facts["var_fsroot"] + "/home", "bytes": du_bytes(os.path.join(var, "home"))}
            facts["migration"] = migration_facts(var, top)
            deps = deployment_dirs(top)
            facts["signer"] = signer_facts(deps[:1] or [top])
            facts["keymap"] = keymap_of(os.path.join(deps[0], "etc")) if deps else "us"
            bdir = os.path.join(top, "boot")
            facts["boot"] = boot_facts(bdir, "top-level-dir", (rdev.get("uuid") or "").lower(), rdev.get("fstype"))
            if facts["migration"]["phase"] == "confirmed" or facts["migration"]["composefs_leftover"]:
                facts["ostree"]["composefs_native"] = facts["migration"]["phase"] == "confirmed"
        facts["esps"] = esps(devs, mounts)
        facts["nvram"] = {"available": False}
        return facts
    finally:
        mounts.close()


def ostree_facts(top, booted, composefs_native):
    deps = deployment_dirs(top)
    repo = os.path.join(top, "ostree/repo")
    return {"booted": booted, "composefs_native": composefs_native,
            "deployments": [os.path.relpath(d, top) for d in deps],
            "store_bytes": du_bytes(repo) if os.path.isdir(repo) else 0}


def boot_facts(bdir, kind, fs_uuid, fstype):
    g = os.path.join(bdir, "grub2/grub.cfg")
    text = read(g, None, 1 << 18)
    loader = os.path.join(bdir, "loader")
    return {"kind": kind, "uuid": fs_uuid, "fstype": fstype, "grub_cfg_present": text is not None,
            "sources_custom_cfg": bool(text and re.search(r"custom\.cfg", text)),
            "loader_link": os.readlink(loader) if os.path.islink(loader) else None,
            "custom_cfg_present": os.path.isfile(os.path.join(bdir, "grub2/custom.cfg"))}


def keymap_of(etc):
    m = re.search(r'^KEYMAP="?([^"\n]+)"?', read(os.path.join(etc, "vconsole.conf"), "") or "", re.M)
    return m.group(1) if m else "us"


def esps(devs, mounts):
    out = []
    for d in devs:
        if d["parttype"] == ESP_TYPE and d.get("fstype") == "vfat":
            p = mounts.ro(d["name"], "vfat")
            out.append(esp_facts(d, p, disk_of(devs, d["name"])))
    return out


# ── classification (pure: facts in, verdict out) ─────────────────────────────────────────────────────────────


def is_windows_esp(e):
    return any(n.lower() == "microsoft" for n in e.get("dirs", []))


def has_dir(e, name):
    return any(n.lower() == name.lower() for n in e.get("dirs", []))


def booted_loader(facts):
    """(esp, path) of the loader the firmware started, from NVRAM BootCurrent; (None, None) when unknown."""
    nv = facts.get("nvram") or {}
    if not nv.get("available") or not nv.get("boot_current"):
        return None, None
    ent = nv.get("entries", {}).get(nv["boot_current"])
    if not ent:
        return None, None
    esp = next((e for e in facts.get("esps", []) if ent.get("partuuid") and e.get("partuuid") == ent["partuuid"]),
               None)
    path = ent.get("path")
    if esp is None and path is None:
        # A firmware boot option with no file (the removable-media path): only certain when one ESP has \EFI\BOOT.
        cand = [e for e in facts.get("esps", []) if has_dir(e, "BOOT")]
        if len(cand) == 1:
            return cand[0], "\\EFI\\BOOT\\BOOTX64.EFI"
    if esp is not None and path is None:
        path = "\\EFI\\BOOT\\BOOTX64.EFI"
    return esp, path


def loader_dir(path):
    parts = [p for p in (path or "").replace("/", "\\").split("\\") if p]
    return parts[1].lower() if len(parts) >= 3 and parts[0].lower() == "efi" else None


def base_topology(facts):
    """The loader topology without the failed-trial overlay: A, B, C, D or unknown, plus why."""
    esps_ = facts.get("esps", [])
    root_disk = (facts.get("root") or {}).get("disk")
    esp, path = booted_loader(facts)
    if esp is not None:
        d = loader_dir(path)
        why = f"firmware booted {path} on {esp['device']} (BootCurrent {facts['nvram']['boot_current']})"
        if d == "rimeinst" or has_dir(esp, "rimeinst"):
            return "C", why
        if d == "apex":
            return "D", why
        if d in ("fedora", "boot"):
            return ("B", why + ", a Windows ESP") if is_windows_esp(esp) else ("A", why + ", Rime's own ESP")
        return "unknown", why + ": not a loader Rime installs"
    # No NVRAM answer: decide from what the ESPs hold, most specific first.
    if any(has_dir(e, "rimeinst") for e in esps_):
        return "C", "an ESP holds \\EFI\\rimeinst\\ (NVRAM not read)"
    if any(has_dir(e, "APEX") for e in esps_):
        return "D", "an ESP holds the legacy \\EFI\\APEX\\ loader (NVRAM not read)"
    own = [e for e in esps_ if has_dir(e, "fedora") and not is_windows_esp(e)]
    if own:
        same = [e for e in own if e.get("disk") == root_disk]
        return "A", f"Rime's own ESP {(same or own)[0]['device']} holds \\EFI\\fedora\\ (NVRAM not read)"
    if any(has_dir(e, "fedora") and is_windows_esp(e) for e in esps_):
        return "B", "\\EFI\\fedora\\ is only in a Windows ESP (NVRAM not read)"
    return "unknown", "no ESP holds a Rime loader"


def trial_leftovers(facts):
    """The boot-v2 trial's files on Rime's own ESPs (never a Windows ESP) and its NVRAM entries."""
    files = []
    for e in facts.get("esps", []):
        if is_windows_esp(e):
            continue
        for rel in TRIAL_LEFTOVERS:
            top = rel.split("/")[0]
            if rel.startswith("EFI/") and has_dir(e, rel.split("/")[1]):
                files.append({"esp": e["device"], "path": rel})
            elif rel == "loader" and any(n.lower() == top for n in e.get("top", [])):
                files.append({"esp": e["device"], "path": rel})
    nv = facts.get("nvram") or {}
    entries = [k for k, v in (nv.get("entries") or {}).items()
               if (v.get("path") or "").lower().replace("/", "\\").startswith("\\efi\\systemd\\")]
    return {"files": files, "nvram": sorted(entries)}


def classify(facts):
    """{topology, base, why}: E wins over its base, F over everything (it has no ostree GRUB to return to)."""
    mig = facts.get("migration") or {}
    ost = facts.get("ostree") or {}
    if ost.get("composefs_native") or mig.get("phase") == "confirmed":
        return {"topology": "F", "base": None, "why": "booted from composefs" if ost.get("composefs_native")
                else "boot-v2 phase=confirmed"}
    base, why = base_topology(facts)
    if mig.get("phase") == "failed":
        return {"topology": "E", "base": base, "why": f"boot-v2 phase=failed in {mig.get('state_dir')}; base {base}: {why}"}
    lo = trial_leftovers(facts)
    if lo["files"] and (mig.get("state_dir") or mig.get("composefs_leftover")):
        return {"topology": "E", "base": base, "why": f"boot-v2 leftovers without a phase file; base {base}: {why}"}
    return {"topology": base, "base": base, "why": why}


def chain_ok(facts):
    """Does the loader that boots this machine read the root's /boot/grub2/grub.cfg (where custom.cfg is sourced)?
    Returns (ok, why). bootupd's static EFI grub.cfg sources bootuuid.cfg and searches BOOT_UUID."""
    boot = facts.get("boot") or {}
    esp, path = booted_loader(facts)
    if esp is None:
        cands = [e for e in facts.get("esps", []) if not is_windows_esp(e) and "fedora" in e.get("grub", {})]
        if facts.get("mode") == "live" or not cands:
            return False, "the booted loader is not known (no NVRAM BootCurrent naming an ESP Rime can read)"
        esp, d = cands[0], "fedora"
    else:
        d = loader_dir(path) or "boot"
    g = esp.get("grub", {}).get(d)
    if not g and d == "boot" and has_dir(esp, "fedora") and esp.get("fallback_csv"):
        # The removable path \EFI\BOOT\BOOTX64.EFI is shim with its fallback (fbx64.efi) and no GRUB beside it:
        # fallback reads \EFI\fedora\BOOTX64.CSV and starts \EFI\fedora\shimx64.efi, whose GRUB reads
        # \EFI\fedora\grub.cfg. That is how a fresh install boots before the firmware has a "Fedora" entry.
        d = "fedora"
        g = esp.get("grub", {}).get(d)
    if not g:
        return False, f"{esp['device']} \\EFI\\{d}\\ has no grub.cfg Rime can read"
    if g.get("boot_uuid") != (boot.get("uuid") or "").lower():
        return False, (f"{esp['device']} \\EFI\\{d}\\bootuuid.cfg names {g.get('boot_uuid')}, "
                       f"not /boot's filesystem {boot.get('uuid')}")
    if "configfile" not in g.get("grub_cfg", ""):
        return False, f"{esp['device']} \\EFI\\{d}\\grub.cfg does not hand over to /boot/grub2/grub.cfg"
    if not boot.get("sources_custom_cfg"):
        return False, "/boot/grub2/grub.cfg does not source custom.cfg"
    return True, f"{esp['device']} \\EFI\\{d}\\grub.cfg -> fs {boot.get('uuid')} /boot/grub2/grub.cfg -> custom.cfg"


def need_bytes(facts):
    return FLOOR_BYTES + int((facts.get("home") or {}).get("bytes", 0)) + int(
        (facts.get("ostree") or {}).get("store_bytes", 0))


def evaluate(facts, backup=None):
    """The verdict: (classification, refusals [(name, text)], notes [text]). Nothing here writes."""
    c = classify(facts)
    t = c["topology"]
    refusals, notes = [], []
    if t not in ACCEPTED:
        refusals.append((f"topology-{t}", f"topology {t} ({TOPOLOGY_TEXT[t]}) is not bridged in v1 (PLAN.md A10): "
                         "back up and reinstall"))
    root = facts.get("root") or {}
    if root.get("locked"):
        refusals.append(("root-locked", "the root is inside LUKS and was not opened (--disk mode)"))
    elif root.get("fstype") != "btrfs":
        refusals.append(("root-not-btrfs", f"the root filesystem is {root.get('fstype')}, not btrfs (D2)"))
    if root.get("luks") and facts.get("keymap", "us") != "us":
        refusals.append(("luks-keymap", f"LUKS with the console keymap {facts.get('keymap')}: not proven to unlock "
                         "under NixOS yet (D2)"))
    if not (facts.get("signer") or {}).get("renamed_known"):
        refusals.append(("renamed-signer", "this system never took the release that knows the renamed signer "
                         f"{RENAMED_SIGNER} ({(facts.get('signer') or {}).get('evidence')}); update bootc first"))
    if t in ACCEPTED:
        ok, why = chain_ok(facts)
        (notes if ok else refusals).append(why if ok else ("boot-chain-unverified", why))
        if (facts.get("boot") or {}).get("kind") not in ("top-level-dir", "partition"):
            refusals.append(("boot-unknown", "/boot is neither a directory of the root btrfs nor a partition"))
    need = need_bytes(facts)
    free = int(root.get("free_bytes", 0))
    if root.get("fstype") == "btrfs" and free < need:
        refusals.append(("free-space", f"{free / GB:.1f} GB free, {need / GB:.1f} GB needed: floor "
                         f"{FLOOR_BYTES / GB:.1f} + home {facts['home']['bytes'] / GB:.1f} + old store "
                         f"{facts['ostree']['store_bytes'] / GB:.1f} (H9)"))
    for s in facts.get("existing_subvols", []):
        refusals.append(("already-started", f"subvolume {s} already exists"))
    if backup is not None:
        b = backup
        if not b.get("exists"):
            refusals.append(("backup-missing", f"backup destination {b['path']} is not a directory"))
        elif b.get("disk") and b.get("disk") == root.get("disk"):
            refusals.append(("backup-same-disk", f"backup destination {b['path']} is on {b['disk']}, the disk "
                             "being migrated"))
        elif int(b.get("free_bytes", 0)) < int((facts.get("home") or {}).get("bytes", 0)) + GB:
            refusals.append(("backup-too-small", f"backup destination has {b['free_bytes'] / GB:.1f} GB free, home "
                             f"is {facts['home']['bytes'] / GB:.1f} GB"))
    if t == "E":
        lo = trial_leftovers(facts)
        if lo["files"] or lo["nvram"]:
            notes.append("topology E repair pending: " + ", ".join(
                [f"{x['esp']}:{x['path']}" for x in lo["files"]] + [f"Boot{n}" for n in lo["nvram"]]))
    return c, refusals, notes


def backup_facts(path, devs=None):
    b = {"path": path, "exists": os.path.isdir(path)}
    if not b["exists"]:
        return b
    st = os.statvfs(path)
    b["free_bytes"] = st.f_bavail * st.f_frsize
    r = run(["findmnt", "-n", "-J", "-o", "SOURCE,FSTYPE", "--target", path], check=False)
    if r.returncode == 0:
        src = re.sub(r"\[.*\]$", "", json.loads(r.stdout)["filesystems"][0]["source"])
        devs = devs if devs is not None else lsblk()
        real = os.path.realpath(src)
        d = next((x for x in devs if x["name"] == src or os.path.realpath(x["name"]) == real), None)
        b["device"] = d["name"] if d else src
        b["disk"] = disk_of(devs, d["name"]) if d else None
    return b


def report(facts, c, refusals, notes, explain):
    lines = [f"topology: {c['topology']}" + (f" (on {c['base']})" if c["topology"] == "E" else "")
             + f" - {TOPOLOGY_TEXT[c['topology']]}"]
    if explain:
        lines.append(f"  why: {c['why']}")
        r = facts.get("root") or {}
        lines.append(f"  root: {r.get('device')} {r.get('fstype')} label={r.get('label')} uuid={r.get('uuid')}"
                     + (f" in LUKS {r['luks'].get('device')}" if r.get("luks") else ""))
        b = facts.get("boot") or {}
        lines.append(f"  /boot: {b.get('kind')} uuid={b.get('uuid')} loader->{b.get('loader_link')} "
                     f"custom.cfg sourced={b.get('sources_custom_cfg')}")
        for e in facts.get("esps", []):
            lines.append(f"  ESP {e['device']} on {e['disk']}: EFI/{{{','.join(e['dirs'])}}}"
                         + (" (Windows ESP)" if is_windows_esp(e) else ""))
        nv = facts.get("nvram") or {}
        if nv.get("available"):
            lines.append(f"  NVRAM: BootCurrent {nv.get('boot_current')} BootOrder {','.join(nv.get('boot_order'))} "
                         f"SecureBoot {nv.get('secure_boot')}")
            for k, v in sorted(nv.get("entries", {}).items()):
                lines.append(f"    Boot{k} {v.get('label')!r} {v.get('path')} part {v.get('partuuid')}")
        m = facts.get("migration") or {}
        lines.append(f"  boot-v2: phase={m.get('phase')} state={m.get('state_dir')} composefs/={m.get('composefs_leftover')}")
        lines.append(f"  signer: renamed signer known={facts.get('signer', {}).get('renamed_known')} "
                     f"({facts.get('signer', {}).get('evidence')})")
    r = facts.get("root") or {}
    lines.append(f"space: {int(r.get('free_bytes', 0)) / GB:.1f} GB free, {need_bytes(facts) / GB:.1f} GB needed "
                 f"(floor {FLOOR_BYTES / GB:.1f} + home {facts.get('home', {}).get('bytes', 0) / GB:.1f} + old store "
                 f"{facts.get('ostree', {}).get('store_bytes', 0) / GB:.1f})")
    lines.append(f"home: {facts.get('home', {}).get('bytes', 0) / GB:.2f} GB")
    if facts.get("backup"):
        b = facts["backup"]
        lines.append(f"backup: {b['path']} on {b.get('disk')}, {b.get('free_bytes', 0) / GB:.1f} GB free")
    for n in notes:
        lines.append(f"note: {n}")
    for name, text in refusals:
        lines.append(f"REFUSED [{name}] {text}")
    lines.append("verdict: " + ("REFUSED " + " ".join(f"[{n}]" for n, _ in refusals) if refusals else
                                f"topology {c['topology']} can be bridged (sudo rime migrate start)"))
    return "\n".join(lines) + "\n"


# ── private mount namespace ─────────────────────────────────────────────────────────────────────────────────────


def in_private_namespace(argv):
    """Re-run this command in a private mount namespace: every mount it makes goes away when it exits."""
    if os.environ.get("RIME_MIGRATE_NS") == "1":
        return
    env = dict(os.environ, RIME_MIGRATE_NS="1")
    os.execvpe("unshare", ["unshare", "--mount", "--propagation", "private", "--",
                           sys.executable, "-I", os.path.abspath(__file__), *argv], env)


def gather(a):
    facts = collect_disks(a.disk) if a.disk else collect_live()
    if facts.get("root", {}).get("fstype") == "btrfs" and not a.disk:
        facts["existing_subvols"] = existing_subvols(facts["root"]["device"])
    if getattr(a, "backup", None):
        facts["backup"] = backup_facts(a.backup)
    return facts


def existing_subvols(dev):
    m = Mounts()
    try:
        top = m.ro(dev, "btrfs", "subvolid=5")
        return [s for s in SUBVOLS + ("@pre-nixos",) if top and os.path.exists(os.path.join(top, s))]
    finally:
        m.close()


def cmd_check(a):
    facts = gather(a)
    c, refusals, notes = evaluate(facts, facts.get("backup"))
    if a.json:
        print(json.dumps({"facts": facts, "classification": c, "refusals": [{"name": n, "text": t} for n, t in refusals],
                          "notes": notes}, indent=1, sort_keys=True))
    else:
        sys.stdout.write(report(facts, c, refusals, notes, a.explain))
    return EXIT_REFUSED if refusals else EXIT_OK


# ── repair-e (X1) ────────────────────────────────────────────────────────────────────────────────────────────────

FS_IOC_GETFLAGS, FS_IOC_SETFLAGS, FS_IMMUTABLE_FL = 0x80086601, 0x40086602, 0x10


def efivar_delete(name):
    p = os.path.join(EFIVARS, f"{name}-{EFI_GLOBAL}")
    fd = os.open(p, os.O_RDONLY)
    try:
        buf = bytearray(struct.pack("i", 0))
        fcntl.ioctl(fd, FS_IOC_GETFLAGS, buf)
        flags = struct.unpack("i", buf)[0] & ~FS_IMMUTABLE_FL
        fcntl.ioctl(fd, FS_IOC_SETFLAGS, struct.pack("i", flags))
    except OSError:
        pass
    finally:
        os.close(fd)
    os.unlink(p)


def cmd_repair_e(a):
    facts = collect_live()
    c = classify(facts)
    if c["topology"] != "E":
        raise Refused("not-topology-E", f"this machine is topology {c['topology']}; repair-e is for topology E only")
    lo = trial_leftovers(facts)
    nv = facts["nvram"]
    protected = set(nv.get("boot_order", [])) | {nv.get("boot_current")}
    for n in lo["nvram"]:
        if n in protected:
            raise Refused("trial-entry-in-use", f"Boot{n} (the trial's systemd-boot) is in BootOrder or booted this "
                          "machine; refusing to delete it")
    phase_before = facts["migration"]
    print(f"topology E (on {c['base']}); the failed trial left:")
    for x in lo["files"]:
        print(f"  {x['esp']}: \\{x['path'].replace('/', chr(92))}\\")
    for n in lo["nvram"]:
        e = nv["entries"][n]
        print(f"  NVRAM Boot{n} {e['label']!r} {e['path']} (not in BootOrder)")
    if not lo["files"] and not lo["nvram"]:
        print("  nothing: the repair has already run")
        return EXIT_OK
    if not a.confirm:
        print("nothing written; run again with --confirm to archive and delete these")
        return EXIT_OK
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    arch = os.path.join("/var/lib/rime/boot-migrate", f"archive-{ts}")
    os.makedirs(arch, mode=0o700)
    manifest = {"at": ts, "topology": c, "files": [], "nvram": {}, "phase": phase_before.get("phase")}
    for n in lo["nvram"]:
        raw = open(os.path.join(EFIVARS, f"Boot{n}-{EFI_GLOBAL}"), "rb").read()
        with open(os.path.join(arch, f"Boot{n}.efivar"), "wb") as f:
            f.write(raw)
        manifest["nvram"][n] = nv["entries"][n] | {"sha256": hashlib.sha256(raw).hexdigest()}
    devs = {e["device"]: e for e in facts["esps"]}
    for dev in sorted({x["esp"] for x in lo["files"]}):
        if is_windows_esp(devs[dev]):
            raise Refused("windows-esp", f"{dev} is a Windows ESP")
        mnt = tempfile.mkdtemp(prefix="rime-migrate-esp-")
        run(["mount", "-t", "vfat", "-o", "rw,umask=0077", dev, mnt])
        try:
            for x in [x for x in lo["files"] if x["esp"] == dev]:
                src = find_ci(mnt, x["path"])
                dst = os.path.join(arch, "esp", os.path.basename(dev), x["path"])
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copytree(src, dst) if os.path.isdir(src) else shutil.copy2(src, dst)
                for d, _, fs in os.walk(dst):
                    for fn in fs:
                        p = os.path.join(d, fn)
                        manifest["files"].append({"path": os.path.relpath(p, arch),
                                                  "sha256": hashlib.sha256(open(p, "rb").read()).hexdigest()})
            for x in [x for x in lo["files"] if x["esp"] == dev]:
                # The archive is read back before the ESP copy goes.
                src = find_ci(mnt, x["path"])
                for d, _, fs in os.walk(src):
                    for fn in fs:
                        rel = os.path.relpath(os.path.join(d, fn), mnt)
                        arc = os.path.join(arch, "esp", os.path.basename(dev), x["path"],
                                           os.path.relpath(os.path.join(d, fn), src))
                        if hashlib.sha256(open(arc, "rb").read()).digest() != hashlib.sha256(
                                open(os.path.join(mnt, rel), "rb").read()).digest():
                            raise Failed(f"archive of {rel} does not match the ESP")
                shutil.rmtree(src) if os.path.isdir(src) else os.unlink(src)
            os.sync()
        finally:
            run(["umount", mnt], check=False)
            os.rmdir(mnt)
    with open(os.path.join(arch, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=1, sort_keys=True)
    for n in lo["nvram"]:
        efivar_delete(f"Boot{n}")
        print(f"deleted NVRAM Boot{n}")
    after = migration_facts("/var", None)
    if after.get("phase") != phase_before.get("phase"):
        raise Failed(f"phase changed from {phase_before.get('phase')} to {after.get('phase')}")
    print(f"archived to {arch}; phase={after.get('phase')} kept")
    return EXIT_OK


# ── start ─────────────────────────────────────────────────────────────────────────────────────────────────────────


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def tree_sums(base, rels=None):
    """sha256 of every regular file and the target of every link under BASE (or of the RELS within it)."""
    out = {}
    roots = [os.path.join(base, r) for r in rels] if rels else [base]
    for r in roots:
        if os.path.islink(r):
            out[os.path.relpath(r, base)] = "->" + os.readlink(r)
            continue
        if os.path.isfile(r):
            out[os.path.relpath(r, base)] = sha256_file(r)
            continue
        for d, dirs, fs in os.walk(r):
            for n in fs + [x for x in dirs if os.path.islink(os.path.join(d, x))]:
                p = os.path.join(d, n)
                out[os.path.relpath(p, base)] = "->" + os.readlink(p) if os.path.islink(p) else sha256_file(p)
    return out


def containment_snapshot(top, facts):
    """H8: what the bridge must leave byte for byte: ostree's /boot (loader, entries, kernels, grub.cfg and
    bootuuid.cfg) and every file on every ESP. custom.cfg is compared outside the Rime block only."""
    boot = os.path.join(top, "boot") if facts["boot"]["kind"] == "top-level-dir" else "/boot"
    snap = {"boot": {k: v for k, v in tree_sums(boot).items()
                     if not k.startswith(("rime/", "grub2/rime-nixos.cfg", "grub2/custom.cfg", "lost+found"))}}
    cc = read(os.path.join(boot, "grub2/custom.cfg"), "")
    snap["custom_outside_block"] = hashlib.sha256(re.sub(
        r"### BEGIN rime-boot.*?### END rime-boot ###\n?", "", cc, flags=re.S).encode()).hexdigest()
    snap["deployments"] = deployment_dirs(top)
    m = Mounts()
    try:
        for d in lsblk():
            if d["parttype"] == ESP_TYPE and d.get("fstype") == "vfat":
                p = m.ro(d["name"], "vfat")
                snap["esp:" + d["name"]] = tree_sums(p) if p else None
    finally:
        m.close()
    return snap


def fsck_gate(top, facts, stage):
    """H8: ostree fsck clean, btrfs device stats zero and a read-only scrub clean. Raises Failed on any error."""
    out = {}
    r = run(["ostree", "fsck", "--repo", "/sysroot/ostree/repo" if os.path.isdir("/sysroot/ostree/repo")
             else os.path.join(top, "ostree/repo")], check=False)
    out["ostree_fsck"] = {"rc": r.returncode, "tail": (r.stdout + r.stderr).strip().splitlines()[-3:]}
    r2 = run(["btrfs", "device", "stats", "-c", top], check=False)
    out["device_stats"] = {"rc": r2.returncode, "nonzero": [ln for ln in r2.stdout.splitlines()
                                                             if not ln.rstrip().endswith(" 0")]}
    r3 = run(["btrfs", "scrub", "start", "-B", "-r", top], check=False)
    out["scrub"] = {"rc": r3.returncode, "tail": (r3.stdout + r3.stderr).strip().splitlines()[-4:]}
    print(f"fsck gate ({stage}): ostree fsck rc={r.returncode}, device stats rc={r2.returncode}, "
          f"scrub rc={r3.returncode}", flush=True)
    if r.returncode or r2.returncode or r3.returncode:
        raise Failed(f"fsck containment gate failed {stage}: {json.dumps(out)}")
    return out


def do_backup(dest, facts, var_home):
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    d = os.path.join(dest, f"rime-migrate-backup-{ts}")
    os.makedirs(d, mode=0o700)
    print(f"backup: home -> {d}/home", flush=True)
    run(["cp", "-a", "--one-file-system", var_home + "/.", os.path.join(d, "home")], capture=False)
    os.makedirs(os.path.join(d, "system"), mode=0o700)
    run(["tar", "-C", "/", "-cf", os.path.join(d, "system/etc.tar"), "etc"], check=False)
    with open(os.path.join(d, "system/facts.json"), "w") as f:
        json.dump(facts, f, indent=1, sort_keys=True)
    # The restore test: a sample of files read back from the destination, compared with the source.
    files = []
    for root, _, fs in os.walk(var_home):
        for n in fs:
            p = os.path.join(root, n)
            if os.path.isfile(p) and not os.path.islink(p):
                files.append(p)
    files.sort()
    step = max(1, len(files) // 64)
    sample = files[::step][:64]
    bad = [p for p in sample if sha256_file(p) != sha256_file(os.path.join(d, "home", os.path.relpath(p, var_home)))]
    if bad:
        raise Failed(f"backup restore test failed for {len(bad)} of {len(sample)} files: {bad[:3]}")
    print(f"backup: restore test {len(sample)} of {len(files)} files read back equal", flush=True)
    luks = facts["root"].get("luks")
    if luks:
        run(["cryptsetup", "luksHeaderBackup", luks["device"], "--header-backup-file",
             os.path.join(d, "luks-header.img")])
        print("backup: LUKS header written", flush=True)
    with open(os.path.join(d, "RESTORE-TESTED"), "w") as f:
        f.write(f"{len(sample)} of {len(files)} files compared equal at {ts}\n")
    os.sync()
    return d


def mask_old_side(state):
    masked = []
    for u in MASK_UNITS:
        p = os.path.join("/etc/systemd/system", u)
        if os.path.lexists(p):
            continue
        os.symlink("/dev/null", p)
        masked.append(u)
    state["masked"] = masked
    run(["systemctl", "daemon-reload"], check=False)
    for u in ("rime-update-check.timer", "rime-store-compact.timer"):
        run(["systemctl", "stop", u], check=False)
    print(f"old side: masked {', '.join(masked) or 'nothing new'}", flush=True)


def unmask_old_side(state):
    for u in state.get("masked", []):
        p = os.path.join("/etc/systemd/system", u)
        if os.path.islink(p) and os.readlink(p) == "/dev/null":
            os.unlink(p)
    run(["systemctl", "daemon-reload"], check=False)


def unit_busy(show):
    """True while a unit's job is still running (`systemctl show -p ActiveState` text). Both WAIT_UNITS are oneshot,
    rime-migrate-from-apex.service with RemainAfterExit=yes: "active" there means it FINISHED, not that it runs."""
    props = dict(line.split("=", 1) for line in show.splitlines() if "=" in line)
    return props.get("ActiveState") in ("activating", "deactivating", "reloading")


def wait_units(timeout=3600, poll=5):
    for u in WAIT_UNITS:
        deadline = time.monotonic() + timeout
        said = False
        while unit_busy(run(["systemctl", "show", "-p", "ActiveState", u], check=False).stdout):
            if not said:
                print(f"waiting for {u} to finish", flush=True)
                said = True
            if time.monotonic() > deadline:
                raise Refused("old-side-busy", f"{u} still running after {timeout} s; start again once it has finished")
            time.sleep(poll)


def verify_payload(payload):
    man = json.load(open(os.path.join(payload, "manifest.json")))
    for key in ("store", "registration"):
        p = os.path.join(payload, man[key]["file"])
        got = sha256_file(p)
        if got != man[key]["sha256"]:
            raise Refused("payload-corrupt", f"{p}: sha256 {got}, manifest says {man[key]['sha256']}")
    if not re.fullmatch(r"/nix/store/[0-9a-z]{32}-[^/]+", man["toplevel"]) or not re.fullmatch(
            r"/nix/store/[0-9a-z]{32}-[^/]+", man["nix"]):
        raise Refused("payload-corrupt", "manifest toplevel/nix are not store paths")
    # TODO(handover): the manifest is checked against sha256s only. Production signs it (rime-cache-1 narinfo key or
    # the nix-release.yml Sigstore identity, D6) and the readiness release ships the public half.
    return man


def chroot(t, *cmd, env=None, stdin=None):
    e = {"PATH": "/run/current-system/sw/bin:/nix/var/nix/profiles/system/sw/bin", "HOME": "/root", "LANG": "C.UTF-8"}
    e.update(env or {})
    r = subprocess.run(["chroot", t, *cmd], env=e, stdin=stdin, text=True, stdout=subprocess.PIPE,
                       stderr=subprocess.STDOUT)
    sys.stdout.write("".join(f"  | {ln}\n" for ln in r.stdout.splitlines()[-40:]))
    if r.returncode:
        raise Failed(f"chroot {' '.join(cmd)} exited {r.returncode}")
    return r


def mount(*args):
    run(["mount", *args])


def cmd_start(a):
    if not a.yes:
        raise Refused("not-confirmed", "start needs --yes (read `rime migrate check --explain` first)")
    facts = gather(a)
    c, refusals, notes = evaluate(facts, facts.get("backup"))
    if facts.get("mode") != "live":
        raise Refused("not-live", "start runs on the booted bootc system only")
    if not a.backup:
        refusals.append(("backup-missing", "start needs --backup DIR off the disk being migrated"))
    if c["topology"] == "E":
        lo = trial_leftovers(facts)
        if lo["files"] or lo["nvram"]:
            refusals.append(("e-repair-pending", "run `rime migrate repair-e --confirm` first (D3)"))
    if refusals:
        sys.stdout.write(report(facts, c, refusals, notes, True))
        return EXIT_REFUSED
    man = verify_payload(a.payload)
    root = facts["root"]
    dev = root["luks"]["mapper"] if root.get("luks") else root["device"]
    state = {"started": datetime.datetime.now(datetime.timezone.utc).isoformat(), "topology": c, "steps": []}
    os.makedirs(STATE, mode=0o700, exist_ok=True)

    def step(s):
        state["steps"].append(s)
        with open(os.path.join(STATE, "state.json"), "w") as f:
            json.dump(state, f, indent=1, sort_keys=True)
        print(f"== {s}", flush=True)

    os.makedirs("/run/rime-migrate", exist_ok=True)
    top = "/run/rime-migrate/top"
    os.makedirs(top, exist_ok=True)
    mount("-o", "subvolid=5", dev, top)
    step("backup")
    state["backup"] = do_backup(a.backup, facts, os.path.join(top, facts["var_fsroot"].lstrip("/"), "home"))
    step("old side")
    wait_units()
    mask_old_side(state)
    step("fsck gate before")
    state["fsck_before"] = fsck_gate(top, facts, "before")
    before = containment_snapshot(top, facts)
    made = []
    old_custom = read(os.path.join(top, "boot/grub2/custom.cfg") if facts["boot"]["kind"] == "top-level-dir"
                      else "/boot/grub2/custom.cfg")
    try:
        step("snapshot @pre-nixos")
        run(["btrfs", "subvolume", "snapshot", "-r", top, os.path.join(top, "@pre-nixos")])
        made.append("@pre-nixos")
        for s in SUBVOLS:
            run(["btrfs", "subvolume", "create", os.path.join(top, s)])
            made.append(s)
        if root.get("label") != ROOT_LABEL:
            if any(d.get("label") == ROOT_LABEL for d in lsblk()):
                raise Refused("label-taken", f"another filesystem is already labelled {ROOT_LABEL}")
            run(["btrfs", "filesystem", "label", top, ROOT_LABEL])
            state["relabelled_from"] = root.get("label")
        step("home (reflink copy)")
        src_home = os.path.join(top, facts["var_fsroot"].lstrip("/"), "home")
        run(["cp", "-a", "--reflink=always", "--one-file-system", src_home + "/.", os.path.join(top, "@home")])
        step("install closure")
        t = "/run/rime-migrate/target"
        os.makedirs(t, exist_ok=True)
        mount("-o", "subvol=@nixos,compress-force=zstd:3", dev, t)
        for sub, where in (("@nix", "nix"), ("@home", "var/home")):
            os.makedirs(os.path.join(t, where), exist_ok=True)
            mount("-o", f"subvol={sub},compress-force=zstd:3", dev, os.path.join(t, where))
        os.makedirs(os.path.join(t, "boot"), exist_ok=True)
        mount("--bind", os.path.join(top, "boot") if facts["boot"]["kind"] == "top-level-dir" else "/boot",
              os.path.join(t, "boot"))
        run(["tar", "--zstd", "-xpf", os.path.join(a.payload, man["store"]["file"]), "-C", t], capture=False)
        for d in ("dev", "proc", "sys", "run", "etc", "tmp", "var/lib/rime", "run/rime-src"):
            os.makedirs(os.path.join(t, d), exist_ok=True)
        os.chmod(os.path.join(t, "tmp"), 0o1777)
        if not os.path.lexists(os.path.join(t, "home")):
            os.symlink("var/home", os.path.join(t, "home"))
        mount("--rbind", "/dev", os.path.join(t, "dev"))
        mount("-t", "proc", "proc", os.path.join(t, "proc"))
        mount("--rbind", "/sys", os.path.join(t, "sys"))
        mount("-t", "tmpfs", "-o", "mode=0755", "tmpfs", os.path.join(t, "run"))
        nix = man["nix"]
        with open(os.path.join(a.payload, man["registration"]["file"])) as reg:
            chroot(t, f"{nix}/bin/nix-store", "--load-db", stdin=reg)
        chroot(t, f"{nix}/bin/nix-store", "--verify", "--check-contents")
        chroot(t, f"{nix}/bin/nix-env", "-p", "/nix/var/nix/profiles/system", "--set", man["toplevel"])
        step("carry /etc (rime-hostlayer)")
        os.makedirs(os.path.join(t, "run/rime-src"), exist_ok=True)
        for d in ("etc", "usr", "var"):
            p = os.path.join(t, "run/rime-src", d)
            os.makedirs(p, exist_ok=True)
            mount("--bind", "/" + d, p)
            mount("-o", "remount,bind,ro", p)
        chroot(t, f"{man['toplevel']}/sw/bin/rime-hostlayer", "carry", "--from", "/run/rime-src", "--root", "/",
               "--system", man["toplevel"])
        # The boot-v2 record (phase=failed on topology E, and repair-e's archive) carries over as it is: the
        # NixOS side must never re-run that migration or forget it failed (D1, D3).
        bm = os.path.realpath("/var/lib/rime/boot-migrate")
        if os.path.isdir(bm):
            run(["cp", "-a", bm + "/.", os.path.join(t, "var/lib/rime/boot-migrate")])
            state["boot_migrate_carried"] = sorted(os.listdir(bm))
        ht = os.path.join(t, "var/lib/rime/host.toml")
        with open(ht, "a") as f:
            f.write('\n[boot]\nlayout = "custom-cfg"\n')
        step("first activation and boot entry")
        open(os.path.join(t, "etc/NIXOS"), "a").close()
        if os.path.lexists(os.path.join(t, "etc/mtab")):
            os.unlink(os.path.join(t, "etc/mtab"))
        os.symlink("/proc/mounts", os.path.join(t, "etc/mtab"))
        chroot(t, f"{man['toplevel']}/activate", env={"IN_NIXOS_ENTER": "1",
                                                     "LOCALE_ARCHIVE": f"{man['toplevel']}/sw/lib/locale/locale-archive"})
        chroot(t, "/nix/var/nix/profiles/system/bin/switch-to-configuration", "boot",
               env={"NIXOS_INSTALL_BOOTLOADER": "1"})
        for p in reversed(("run/rime-src/etc", "run/rime-src/usr", "run/rime-src/var")):
            run(["umount", os.path.join(t, p)], check=False)
        run(["umount", "-R", os.path.join(t, "run")], check=False)
        os.sync()
        step("fsck gate after")
        state["fsck_after"] = fsck_gate(top, facts, "after")
        after = containment_snapshot(top, facts)
        diff = sorted(k for k in set(before) | set(after) if before.get(k) != after.get(k))
        if diff:
            detail = {k: sorted(x for x in set(before.get(k) or {}) ^ set(after.get(k) or {}))[:5] if isinstance(
                before.get(k), dict) else [before.get(k), after.get(k)] for k in diff}
            raise Failed(f"fsck containment: the bridge changed what it must not: {json.dumps(detail)}")
        print("containment: ostree /boot, custom.cfg outside the Rime block, deployments and every ESP unchanged",
              flush=True)
    except BaseException as e:
        log(f"failed: {e}; removing what the bridge made")
        run(["umount", "-R", "/run/rime-migrate/target"], check=False)
        p = os.path.join(top, "boot/grub2/custom.cfg") if facts["boot"]["kind"] == "top-level-dir" \
            else "/boot/grub2/custom.cfg"
        if old_custom is not None:
            with open(p, "w") as f:
                f.write(old_custom)
        elif os.path.exists(p):
            os.unlink(p)
        for s in reversed(made):
            run(["btrfs", "subvolume", "delete", os.path.join(top, s)], check=False)
        if state.get("relabelled_from"):
            run(["btrfs", "filesystem", "label", top, state["relabelled_from"]], check=False)
        unmask_old_side(state)
        state["failed"] = str(e)
        step("rolled back")
        raise
    state["done"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    step("done")
    report_path = os.path.join("/run/rime-migrate/target", "var/lib/rime/carry/report.txt")
    print(f"done: NixOS {man['toplevel']} is in the GRUB menu; the ostree entries stay. Left-behind report: "
          f"/var/lib/rime/carry/report.txt on the new system ({'present' if os.path.exists(report_path) else 'missing'})")
    return EXIT_OK


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    ap = argparse.ArgumentParser(prog=PROG, description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("--explain", action="store_true")
    c.add_argument("--json", action="store_true")
    c.add_argument("--backup")
    c.add_argument("--disk", action="append", default=[])
    r = sub.add_parser("repair-e")
    r.add_argument("--confirm", action="store_true")
    s = sub.add_parser("start")
    s.add_argument("--backup", required=True)
    s.add_argument("--payload", required=True)
    s.add_argument("--yes", action="store_true")
    s.set_defaults(disk=[])
    a = ap.parse_args(argv)
    if os.geteuid() != 0:
        print(f"{PROG}: needs root (sudo rime migrate {a.cmd})", file=sys.stderr)
        return EXIT_USAGE
    in_private_namespace(argv)
    try:
        return {"check": cmd_check, "repair-e": cmd_repair_e, "start": cmd_start}[a.cmd](a)
    except Refused as e:
        print(f"REFUSED [{e.name}] {e}")
        return EXIT_REFUSED
    except Failed as e:
        print(f"{PROG}: FAILED: {e}", file=sys.stderr)
        return EXIT_FAILED


if __name__ == "__main__":
    sys.exit(main())
