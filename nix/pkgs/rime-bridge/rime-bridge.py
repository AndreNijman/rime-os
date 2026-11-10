#!/usr/bin/env python3
"""rime-migrate: the bridge from a bootc Rime OS machine to Rime OS on NixOS (PLAN.md D1, D2, D3).

  check [--explain] [--json] [--backup DIR] [--payload DIR] [--disk DEV ...]
      Writes nothing. Names the machine's boot topology (A-F, PLAN.md D1 / out/understand-boot.md), the data and
      metadata space start needs against what the root btrfs has (H9, space_plan; --explain says what divergence after
      the move costs), the home size (the backup's), the backup destination, the signer the old system trusts (X3),
      the boot chain it read (every grub.cfg by shape) and every reason the machine is refused, by name. --disk DEV
      probes an unbooted disk read-only instead of the running system (every mount read-only, btrfs with
      rescue=nologreplay, ext4 with noload; a LUKS root is read only when its mapping is already open).
      --explain --payload DIR also verifies the payload's signature and prints the left-behind report the /etc
      carry WOULD write (rime-hostlayer carry --plan, the payload's own copy, run by this system's python): what
      will not come over is shown BEFORE start, never only after.
  repair-e [--confirm]
      Topology E only (a failed boot-v2 trial, katana): lists the trial's leftovers on Rime's own ESP (\\EFI\\systemd\\,
      \\EFI\\Linux\\, \\loader\\), its NVRAM entry and its /composefs and /state in the btrfs top level. With
      --confirm the ESP files and the NVRAM entry are archived under /var/lib/rime/boot-migrate/archive-*/ and
      deleted (the entry through efivarfs), and /composefs and /state are MOVED into that archive (a rename inside
      the same btrfs: nothing is deleted, nothing is copied). Never a Windows ESP, never BootOrder, never the entry
      that booted the machine, never the phase file.
  key-export --to DIR
      The machine's keys, off the disk: the LUKS2 header (luksHeaderBackup, read back with luksDump) and the rime
      backup keys and recipients (/var/lib/rime-backup/{keys,recipients}). start runs it into its backup.
  start [--backup DIR] --payload DIR --yes
      Topologies A and E only. The payload's signature, old side masked, a read-only @pre-nixos snapshot (the restore
      point: it holds the old system and home as they were, and is restore-tested), new subvolumes @nixos (/), @nix (/nix), @home (/var/home, a reflink
      copy), the payload's closure installed and registered, the /etc carry (rime-hostlayer carry), [boot] in
      host.toml (layout custom-cfg, and the /boot partition when /boot is one), the first activation, and the boot
      entry through the Rime boot hook in custom.cfg. The ostree entries stay; nothing is written to any ESP or NVRAM.
      The fsck containment gate (H8) runs before and after, and a failure removes what the bridge made.
      Needs no space beyond the disk itself: --backup DIR is OPTIONAL, an extra copy off the disk (home restore-tested,
      key-export) taken first, for anyone who wants one. Without it the restore point is @pre-nixos, which does not
      survive the disk failing and which `rime migrate finish` deletes.

Exit codes: 0 ok, 2 usage, 3 refused (by name: "REFUSED [name] ..."), 10 failed.
Never runs bootctl, grub2-install or efibootmgr, and never uses NIXOS_LUSTRATE.
"""
import argparse
import base64
import datetime
import errno
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
import tarfile
import tempfile
import time
import uuid

PROG = "rime-migrate"
EXIT_OK, EXIT_USAGE, EXIT_REFUSED, EXIT_FAILED = 0, 2, 3, 10
GB = 10**9

# A11 disk floor as `rime update` is designed (docs/nixos/closure.md §6): one system installed, room for one mass
# rebuild (~10.5 GB unpacked) and boot. H9 (space_plan) adds the old store, if rewritten, to it as DATA, and checks the
# METADATA the reflink copy of home duplicates separately; home's data is shared, not charged (space_plan says why).
FLOOR_BYTES = int(os.environ.get("RIME_MIGRATE_FLOOR_BYTES", 19 * GB))
# H9's metadata estimate (space_plan): per name in home (inode item, inode ref, dir item + dir index, one file extent
# item, the extra extent back-reference, item headers, at leaf fill) and the margin over the larger of the two
# estimates. Both are checked against a real `cp -a --reflink=always` in bridge-refusals (docs/nixos/bridge.md H9).
ENTRY_META_BYTES = 1024
META_MARGIN = 1.25
RENAMED_SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main"
SIGNER_OVERRIDE = "usr/share/rime-os/trust/expected-signer"
STATE = "/var/lib/rime/nixos-bridge"
RUN_DIR = "/run/rime-migrate"  # start's mount points (tests point it at a temporary directory)
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
OLD_SIDE_TIMERS = ("rime-update-check.timer", "rime-store-compact.timer")
SYSTEMD_ETC = "/etc/systemd/system"
WAIT_UNITS = ("rime-store-compact.service", "rime-migrate-from-apex.service")
TRIAL_LEFTOVERS = ("EFI/systemd", "EFI/Linux", "loader")
# The trial's trees in the btrfs top level (rime-boot-migrate's composefs repository and its deployment state).
TRIAL_TOP = ("composefs", "state")
# A /boot partition the bridge hands to NixOS: ext4, what the installer's LUKS layout makes (label rime-boot).
BOOT_PART_FSTYPES = ("ext4",)
# rime backup's key store (rimed/rime-backup-core/src/keys.rs SYSTEM_ROOT): keys/<uid>.key, recipients/<uid>.pub.
BACKUP_KEYS_ROOT = "/var/lib/rime-backup"
# The payload's manifest is signed (docs/nixos/bridge.md "The payload is signed"). This program is the only verifier
# (the readiness release's `rime migrate`, rimed/rime/src/nixos_bridge.rs, execs it with no check of its own); the
# format is what payload-sign.sh writes and `openssl pkeyutl -verify -rawin` accepts:
#   PAYLOAD_KEY_FILE   one line "<name>:<base64 of the 32-byte ed25519 public key>", shipped in the bootc image
#   manifest.json.sig  one line "<name>:<base64 of the 64-byte ed25519 signature>" over the exact bytes of
#                      manifest.json, signed by the key of the same name
PAYLOAD_KEY_FILE = "/usr/share/rime-os/trust/nixos-bridge-payload.pub"
PAYLOAD_SIG = "manifest.json.sig"
# None = trust PAYLOAD_KEY_FILE, the product. The OVMF lab's build replaces this one line with its TEST key
# (nix/probe/ovmf/bridge/lab-bridge.nix); no flag or environment variable can do the same.
TRUSTED_PAYLOAD_KEYS = None

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
    return du_tree(path)["bytes"]


def du_tree(path):
    """One walk of PATH on its own filesystem: allocated bytes (hard links once), every name in it (files and
    directories: each one is a directory entry, and all but the second name of a hard link an inode, in the btrfs
    trees a reflink copy duplicates), and the bytes of regular files smaller than one 4 KiB sector (btrfs keeps such a
    file's data INLINE, in the metadata tree, so its copy costs metadata, not data). The metadata half of H9 reads the
    last two (space_plan)."""
    total, entries, small, seen = 0, 0, 0, set()
    try:
        dev = os.lstat(path).st_dev
    except OSError:
        return {"bytes": 0, "entries": 0, "small_bytes": 0}
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
            entries += 1
            if st.st_nlink > 1:
                if (st.st_dev, st.st_ino) in seen:
                    continue
                seen.add((st.st_dev, st.st_ino))
            total += st.st_blocks * 512
            if stat.S_ISREG(st.st_mode) and st.st_size < 4096:
                small += st.st_size
    return {"bytes": total, "entries": entries, "small_bytes": small}


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


# ── ed25519 (RFC 8032 §5.1.7 verification; the bootc side has no nix and no crypto library to lean on) ──────────


_P = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, _P - 2, _P) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)


def _ed_x(y, sign):
    xx = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P) % _P
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P:
        x = x * _SQRT_M1 % _P
    if (x * x - xx) % _P or (x == 0 and sign):
        return None
    return _P - x if x & 1 != sign else x


def _ed_add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % _P
    b = (p[1] + p[0]) * (q[1] + q[0]) % _P
    c = p[3] * 2 * _D * q[3] % _P
    d = p[2] * 2 * q[2] % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _ed_mul(k, p):
    q = (0, 1, 1, 0)
    while k:
        if k & 1:
            q = _ed_add(q, p)
        p = _ed_add(p, p)
        k >>= 1
    return q


def _ed_decode(b):
    if len(b) != 32:
        return None
    y = int.from_bytes(b, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    if y >= _P:
        return None
    x = _ed_x(y, sign)
    return None if x is None else (x, y, 1, x * y % _P)


_ED_B = _ed_decode((4 * pow(5, _P - 2, _P) % _P).to_bytes(32, "little"))


def ed25519_verify(public, message, signature):
    """True when SIGNATURE (64 bytes) is PUBLIC's (32 bytes) ed25519 signature of MESSAGE: [S]B == R + [k]A."""
    if len(public) != 32 or len(signature) != 64:
        return False
    a, r = _ed_decode(public), _ed_decode(signature[:32])
    s = int.from_bytes(signature[32:], "little")
    if a is None or r is None or s >= _L:
        return False
    k = int.from_bytes(hashlib.sha512(signature[:32] + public + message).digest(), "little") % _L
    lhs, rhs = _ed_mul(s, _ED_B), _ed_add(r, _ed_mul(k, a))
    return (lhs[0] * rhs[2] - rhs[0] * lhs[2]) % _P == 0 and (lhs[1] * rhs[2] - rhs[1] * lhs[2]) % _P == 0


def parse_key(text):
    """A nix-style key "name:base64" -> (name, bytes), or (None, None)."""
    name, _, b64 = (text or "").strip().partition(":")
    try:
        raw = base64.b64decode(b64, validate=True)
    except ValueError:
        return None, None
    return (name, raw) if name and b64 else (None, None)


def payload_keys(path=None):
    """The keys a payload may be signed by: TRUSTED_PAYLOAD_KEYS when a lab build set it, else PAYLOAD_KEY_FILE.
    Refused by name: payload-key-missing (no key file: this system cannot check any payload), payload-key-bad."""
    if TRUSTED_PAYLOAD_KEYS is not None and path is None:
        return tuple(TRUSTED_PAYLOAD_KEYS)
    path = path or PAYLOAD_KEY_FILE
    text = read(path)
    if text is None:
        raise Refused("payload-key-missing", f"this system has no payload signing key ({path}), so no payload can be "
                      "checked; take the release that ships it first")
    name, raw = parse_key(text) if len(text.strip().splitlines()) == 1 else (None, None)
    if name is None or len(raw) != 32:
        raise Refused("payload-key-bad", f"{path} is not one \"<name>:<base64 ed25519 public key>\" line")
    return (text.strip(),)


def verify_manifest_signature(manifest_bytes, sig_text, trusted):
    """The payload manifest's signature check. Raises Refused by name: payload-unsigned, payload-key-untrusted,
    payload-signature-bad. Returns the key name that signed it."""
    if sig_text is None or not sig_text.strip():
        raise Refused("payload-unsigned", f"the payload has no {PAYLOAD_SIG}: a payload nobody signed is never "
                      "installed")
    name, sig = parse_key(sig_text) if len(sig_text.strip().splitlines()) == 1 else (None, None)
    if name is None or len(sig) != 64:
        raise Refused("payload-signature-bad", f"{PAYLOAD_SIG} is not one \"<key name>:<base64 ed25519 signature>\" "
                      "line")
    keys = dict(parse_key(k) for k in trusted)
    if name not in keys:
        raise Refused("payload-key-untrusted", f"the manifest is signed by {name!r}, not by a key this system trusts "
                      f"({', '.join(k.split(':')[0] for k in trusted)})")
    if not ed25519_verify(keys[name], manifest_bytes, sig):
        raise Refused("payload-signature-bad", f"{PAYLOAD_SIG} does not verify against {name}: the manifest was "
                      "changed after it was signed, or the signature belongs to another file")
    return name


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
    m = {"phase": None, "state_dir": None, "files": [], "composefs_leftover": False, "trial_top": []}
    for d in BOOT_MIGRATE_DIRS:
        p = os.path.join(var_root, d.split("/", 1)[1]) if var_root else None
        if p and os.path.isdir(p):
            m["state_dir"] = "/" + d
            m["files"] = sorted(os.listdir(p))
            m["phase"] = (read(os.path.join(p, "phase"), "") or "").strip() or None
            break
    if top:
        m["composefs_leftover"] = os.path.isdir(os.path.join(top, "composefs"))
        m["trial_top"] = [n for n in TRIAL_TOP if os.path.lexists(os.path.join(top, n))]
    return m


def root_facts_from(top, dev, devs, src_fsroot="/"):
    st = os.statvfs(top)
    r = {"device": dev["name"], "disk": disk_of(devs, dev["name"]), "fstype": dev.get("fstype"),
         "uuid": dev.get("uuid"), "label": dev.get("label"), "free_bytes": st.f_bavail * st.f_frsize,
         "size_bytes": st.f_blocks * st.f_frsize, "subvol": src_fsroot}
    if dev.get("fstype") == "btrfs":
        r["usage"] = btrfs_usage(top)
    return r


def btrfs_usage(top):
    """`btrfs filesystem usage -b` of the filesystem mounted at TOP (read-only ioctls; a read-only mount answers too),
    parsed by parse_btrfs_usage. None when the command fails or prints no Data/Metadata line: H9 then refuses
    [space-unmeasured] instead of guessing."""
    r = run(["btrfs", "filesystem", "usage", "-b", top], check=False)
    return parse_btrfs_usage(r.stdout or "")


def parse_btrfs_usage(text):
    """The figures H9 needs from `btrfs filesystem usage -b` (bytes): Size and Used of each block-group type (logical,
    summed over profiles), the Data and Metadata ratios (raw bytes per logical byte: DUP and RAID1 are 2), the device
    space no chunk holds yet (Device unallocated, raw) and the global reserve (logical metadata the kernel keeps for
    itself). Mixed block groups (`Data+Metadata`, only on tiny filesystems) are one pool, `mixed` set."""
    u = {"data_size": 0, "data_used": 0, "meta_size": 0, "meta_used": 0, "mixed": False}
    seen = set()
    for key, rx in (("unallocated", r"^\s*Device unallocated:\s+(\d+)"), ("global_reserve", r"^\s*Global reserve:\s+(\d+)"),
                    ("data_ratio", r"^\s*Data ratio:\s+([\d.]+)"), ("meta_ratio", r"^\s*Metadata ratio:\s+([\d.]+)")):
        m = re.search(rx, text, re.M)
        if m is None:
            return None
        u[key] = float(m.group(1)) if "ratio" in key else int(m.group(1))
    for m in re.finditer(r"^(Data|Metadata|Data\+Metadata),[^:]*:\s*Size:(\d+),\s*Used:(\d+)", text, re.M):
        kind, size, used = m.group(1), int(m.group(2)), int(m.group(3))
        seen.add(kind)
        if kind == "Data+Metadata":
            u["mixed"] = True
            for k in ("data", "meta"):
                u[k + "_size"] += size
                u[k + "_used"] += used
        else:
            k = "data" if kind == "Data" else "meta"
            u[k + "_size"] += size
            u[k + "_used"] += used
    if not ({"Data", "Metadata"} <= seen or "Data+Metadata" in seen) or u["data_ratio"] <= 0 or u["meta_ratio"] <= 0:
        return None
    return u


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
        facts["boot"]["device"] = re.sub(r"\[.*\]$", "", bf.get("source") or "")
        v = json.loads(run(["findmnt", "-n", "-J", "-o", "FSROOT", "--target", "/var"]).stdout)["filesystems"][0]
        facts["var_fsroot"] = v["fsroot"]
        facts["home"] = dict(du_tree("/var/home"), path="/var/home")
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
        top = rdev = bootp = None
        for d in devs:
            if d.get("fstype") in ("btrfs", "ext4", "xfs") and d["parttype"] != ESP_TYPE:
                t = mounts.ro(d["name"], d["fstype"], "subvolid=5" if d["fstype"] == "btrfs" else "")
                if not t:
                    continue
                if top is None and (os.path.isdir(os.path.join(t, "ostree"))
                                    or os.path.isdir(os.path.join(t, "composefs"))):
                    top, rdev = t, d
                elif bootp is None and is_boot_partition(t):
                    bootp = (t, d)
        if top is None:
            luks = [d for d in devs if d.get("fstype") == "crypto_LUKS"
                    and not any(x.get("pkname") == d["name"] for x in devs)]
            facts["root"] = {"fstype": "crypto_LUKS" if luks else None, "device": luks[0]["name"] if luks else None,
                             "disk": disk_of(devs, luks[0]["name"]) if luks else None, "free_bytes": 0,
                             "luks": {"device": luks[0]["name"], "uuid": luks[0].get("uuid")} if luks else None,
                             "locked": bool(luks)}
            facts["ostree"] = {"deployments": [], "store_bytes": 0, "booted": False, "composefs_native": False}
            facts["boot"] = boot_part_facts(bootp) if bootp else {"kind": "unknown"}
            facts["migration"] = {"phase": None, "files": [], "composefs_leftover": False}
            facts["signer"] = {"renamed_known": False, "evidence": "root not readable"}
            facts["home"] = {"bytes": 0}
        else:
            facts["root"] = root_facts_from(top, rdev, devs)
            facts["root"]["luks"] = luks_of(devs, rdev)
            facts["ostree"] = ostree_facts(top, booted=False, composefs_native=False)
            sr = sorted(os.listdir(os.path.join(top, "ostree/deploy"))) if os.path.isdir(
                os.path.join(top, "ostree/deploy")) else []
            var = os.path.join(top, "ostree/deploy", sr[0], "var") if sr else os.path.join(top, "state/os/default/var")
            facts["var_fsroot"] = "/" + os.path.relpath(var, top)
            facts["home"] = dict(du_tree(os.path.join(var, "home")), path=facts["var_fsroot"] + "/home")
            if rdev.get("fstype") == "btrfs":
                facts["home"].update(home_layout(top, facts["var_fsroot"]))
            facts["migration"] = migration_facts(var, top)
            deps = deployment_dirs(top)
            facts["signer"] = signer_facts(deps[:1] or [top])
            facts["keymap"] = keymap_of(os.path.join(deps[0], "etc")) if deps else "us"
            bdir = os.path.join(top, "boot")
            if bootp and disk_of(devs, bootp[1]["name"]) == facts["root"]["disk"] and not is_boot_partition(bdir):
                # The installer's LUKS layout: /boot on its own partition beside the (opened) LUKS root.
                facts["boot"] = boot_part_facts(bootp)
            else:
                facts["boot"] = boot_facts(bdir, "top-level-dir", (rdev.get("uuid") or "").lower(),
                                           rdev.get("fstype"))
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


def is_boot_partition(path):
    """An ostree /boot: GRUB's config and ostree's loader link (or entries) at its top."""
    return os.path.isfile(os.path.join(path, "grub2/grub.cfg")) and (
        os.path.islink(os.path.join(path, "loader")) or os.path.isdir(os.path.join(path, "loader/entries")))


def boot_part_facts(bootp):
    path, d = bootp
    f = boot_facts(path, "partition", (d.get("uuid") or "").lower(), d.get("fstype"))
    f["device"] = d["name"]
    return f


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
    return {"files": files, "nvram": sorted(entries), "top": list((facts.get("migration") or {}).get("trial_top", []))}


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


UUID_RX = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
# The EFI-side grub.cfg shapes Rime knows, by the directives that make each one hand over to /boot/grub2/grub.cfg.
#   bootupd-static  bootupd's grub-static-efi.cfg (every A install): sources bootuuid.cfg beside it, searches the
#                   filesystem BOOT_UUID names, configfile <that fs>/grub2/grub.cfg (or /boot/grub2/grub.cfg)
#   grub2-stub      the stub GRUB's own installer and grub2-mkconfig write: searches a literal fs UUID, configfile
GRUB_SHAPES = {
    "bootupd-static": (("source bootuuid.cfg", r"^\s*source\s+\S*bootuuid\.cfg"),
                       ("search --fs-uuid ${BOOT_UUID}", r"^\s*search\b[^\n]*--fs-uuid\s+\"?\$\{?BOOT_UUID\}?"),
                       ("configfile .../grub.cfg", r"^\s*configfile\s+\S*grub\.cfg")),
    "grub2-stub": (("search --fs-uuid <UUID>", r"^\s*search\b[^\n]*--fs-uuid\b[^\n]*\b" + UUID_RX),
                   ("configfile .../grub.cfg", r"^\s*configfile\s+\S*grub\.cfg")),
}


def grub_shape(text):
    """(shape, target_uuid or None, {shape: [missing directive, ...]}, [GRUB commands in order of first use]).
    The target of a bootupd-static config is BOOT_UUID from bootuuid.cfg (the caller has it)."""
    text = text or ""
    missing = {name: [label for label, rx in need if not re.search(rx, text, re.M)]
               for name, need in GRUB_SHAPES.items()}
    cmds = []
    for c in re.findall(r"^\s*([A-Za-z_][\w-]*)", text, re.M):
        if c not in cmds:
            cmds.append(c)
    if not missing["bootupd-static"]:
        return "bootupd-static", None, missing, cmds
    if not missing["grub2-stub"]:
        line = next(ln for ln in text.splitlines() if re.search(GRUB_SHAPES["grub2-stub"][0][1], ln))
        return "grub2-stub", re.search(UUID_RX, line).group(0).lower(), missing, cmds
    return "unknown", None, missing, cmds


def chain_read(facts):
    """What check reads of the boot chain, so --explain can say exactly that: the ESP and EFI directory the firmware
    starts, the grub.cfg there and its shape, the filesystem it hands over to. {ok, why, esp, dir, shape, ...}"""
    boot = facts.get("boot") or {}
    out = {"ok": False, "esp": None, "dir": None, "shape": None, "target": None, "boot_uuid": boot.get("uuid")}
    esp, path = booted_loader(facts)
    if esp is None:
        cands = [e for e in facts.get("esps", []) if not is_windows_esp(e) and "fedora" in e.get("grub", {})]
        if facts.get("mode") == "live" or not cands:
            out["why"] = "the booted loader is not known (no NVRAM BootCurrent naming an ESP Rime can read)"
            return out
        esp, d = cands[0], "fedora"
        out["how"] = "no NVRAM: the only Rime ESP with \\EFI\\fedora\\grub.cfg"
    else:
        d = loader_dir(path) or "boot"
        out["how"] = f"NVRAM Boot{facts['nvram']['boot_current']} -> {path}"
    g = esp.get("grub", {}).get(d)
    if not g and d == "boot" and has_dir(esp, "fedora") and esp.get("fallback_csv"):
        # The removable path \EFI\BOOT\BOOTX64.EFI is shim with its fallback (fbx64.efi) and no GRUB beside it:
        # fallback reads \EFI\fedora\BOOTX64.CSV and starts \EFI\fedora\shimx64.efi, whose GRUB reads
        # \EFI\fedora\grub.cfg. That is how a fresh install boots before the firmware has a "Fedora" entry.
        d = "fedora"
        g = esp.get("grub", {}).get(d)
        out["how"] += " (shim fallback -> \\EFI\\fedora\\)"
    out.update(esp=esp["device"], dir=d)
    where = f"{esp['device']} \\EFI\\{d}\\grub.cfg"
    if not g:
        out["why"] = f"{esp['device']} \\EFI\\{d}\\ has no grub.cfg Rime can read"
        return out
    text = g.get("grub_cfg", "")
    shape, target, missing, cmds = grub_shape(text)
    if shape == "bootupd-static":
        target = g.get("boot_uuid")
    out.update(shape=shape, target=target, commands=cmds, sha256=hashlib.sha256(text.encode()).hexdigest(),
               lines=len(text.splitlines()))
    if shape == "unknown":
        head = " | ".join(ln.strip() for ln in text.splitlines() if ln.strip())[:240]
        out["why"] = (f"{where} ({out['lines']} lines, sha256 {out['sha256'][:16]}) has a shape Rime does not know, so it "
                      "cannot tell which filesystem GRUB reads next: commands " + (", ".join(cmds) or "none") + "; "
                      + "; ".join(f"{n} needs " + ", ".join(dict(GRUB_SHAPES[n]).keys()) + f" (missing: {', '.join(m)})"
                                  for n, m in missing.items()) + f"; it begins: {head}")
        return out
    if target != (boot.get("uuid") or "").lower():
        src = "bootuuid.cfg BOOT_UUID" if shape == "bootupd-static" else "its search line"
        out["why"] = f"{where} ({shape}) hands over to fs {target} ({src}), not /boot's filesystem {boot.get('uuid')}"
        return out
    if not boot.get("sources_custom_cfg"):
        out["why"] = "/boot/grub2/grub.cfg does not source custom.cfg"
        return out
    out["ok"] = True
    out["why"] = f"{where} ({shape}) -> fs {target} /grub2/grub.cfg -> custom.cfg"
    return out


def chain_ok(facts):
    """Does the loader that boots this machine read /boot's grub2/grub.cfg (where custom.cfg is sourced)?
    Returns (ok, why); chain_read has the detail."""
    c = chain_read(facts)
    return c["ok"], c["why"]


def boot_table(facts):
    """host.toml [boot] keys the bridge writes: the boot hook's layout, and where /boot comes from when it is its
    own partition (rime.rootfs's generator mounts it; without it NixOS would bind the btrfs top level's empty /boot)."""
    t = {"layout": "custom-cfg"}
    b = facts.get("boot") or {}
    if b.get("kind") == "partition":
        t["device"] = f"UUID={b['uuid']}"
        t["fstype"] = b.get("fstype")
    return t


def toml_str(v):
    return '"' + v.replace("\\", "\\\\").replace('"', '\\"') + '"'


def merge_boot_table(text, table):
    """TEXT (a host.toml carry wrote) with TABLE's keys set in its ONE [boot] table: replaced where carry wrote them,
    added after the header otherwise, a new [boot] at the end when there is none. Never a second [boot]."""
    lines = text.splitlines()
    hdr = next((i for i, ln in enumerate(lines) if re.fullmatch(r"\s*\[boot\]\s*(#.*)?", ln)), None)
    mine = [f"{k} = {toml_str(v)}" for k, v in table.items()]
    if hdr is None:
        return (text.rstrip("\n") + "\n\n" if text.strip() else "") + "[boot]\n" + "\n".join(mine) + "\n"
    end = next((i for i in range(hdr + 1, len(lines)) if lines[i].lstrip().startswith("[")), len(lines))
    keep = [ln for ln in lines[hdr + 1:end]
            if not re.match(r"\s*(" + "|".join(map(re.escape, table)) + r")\s*=", ln)]
    return "\n".join(lines[:hdr + 1] + mine + keep + lines[end:]) + "\n"


def write_boot_table(path, table):
    """Merge TABLE into the [boot] of the host.toml at PATH (the one carry just wrote) and replace the file. The text
    is read BEFORE anything is opened for writing: opening it with "w" first truncates what carry wrote, and the
    machine loses its rd.luks.* arguments (no unlock), timezone, locale, keyboard, mounts and [[luks]] (the bridge
    OVMF lab's first encrypted run). Returns the text written."""
    text = merge_boot_table(read(path, "") or "", table)
    tmp = path + ".rime-bridge.tmp"
    with open(tmp, "w") as f:
        f.write(text)
        f.flush()
        os.fsync(f.fileno())
    os.chmod(tmp, 0o644)
    os.replace(tmp, path)
    return text


DIVERGENCE = (
    "  divergence: @home shares every data block with the old system's home and @pre-nixos shares everything, so "
    "start costs them no data space. After the move, each file changed or added on NixOS takes new space while the "
    "old side keeps its old blocks: the data free above shrinks as home diverges, gradually. The space comes back "
    "with `rime migrate finish`, run on NixOS once it has booted well three times: it removes the old side "
    "(@pre-nixos, ostree/ and the old var/home) and the GRUB menu's ostree entries, and `rime migrate finish "
    "--dry-run` first says what it would delete and the space it should free. Until then those entries still boot "
    "the old system, with home as it was at the move.")


def space_plan(facts):
    """H9: what `start` consumes on the root btrfs, data and metadata apart, against what the filesystem has for each.
    None when the root is not btrfs; {"unmeasured": True} when `btrfs filesystem usage` could not be read.

    DATA. start installs the closure into @nix and must leave room for one mass rebuild (FLOOR_BYTES), and the old
    ostree store is counted again in case it is ever rewritten (a compaction or an update unsharing its extents from
    @pre-nixos; start masks both). Home is NOT charged: `cp -a --reflink=always` makes @home share every data extent
    with the old var/home, and the read-only @pre-nixos snapshot shares everything, so neither costs data when it is
    made. What costs data later is DIVERGENCE: every file changed or added on NixOS takes new space while the old side
    keeps the old blocks, gradually, and it comes back when the old side is removed. Charging the whole home (the
    first H9) refused every machine whose home fills more than about half its disk. Available: unused space in data
    chunks + unallocated / data ratio (= statfs free on a single-profile filesystem).

    METADATA. The reflink copy is not free in metadata: every name in home gets a new inode, directory entries and
    file extent items in @home, every shared extent one more back-reference, and a file under one sector keeps its
    data inline in the metadata tree, so it is copied there. Metadata ENOSPC is btrfs's real failure mode (a full
    metadata space with free data). Home's metadata is estimated two ways, the larger kept:
      share     metadata used x min(1, home bytes / data used): home's part of all metadata by its size. Capped at 1:
                du counts uncompressed bytes and Used on-disk ones, and home's metadata cannot exceed all of it.
      per name  names x ENTRY_META_BYTES + bytes of files under 4 KiB: a home of many small files on a disk whose
                data is mostly large files, where the share is too small.
    Need = that x META_MARGIN, times the metadata ratio on the device (DUP writes every block twice). Available on
    the device = (unused in metadata chunks - the global reserve, which the kernel keeps for itself) x ratio + the
    unallocated space DATA did not take (metadata chunks are allocated from it). Mixed block groups (tiny
    filesystems): one pool, the per-name estimate only (the pool's Used is data and metadata together)."""
    root = facts.get("root") or {}
    if root.get("fstype") != "btrfs":
        return None
    u = root.get("usage")
    if not u:
        return {"unmeasured": True}
    home = facts.get("home") or {}
    hbytes, entries, small = int(home.get("bytes", 0)), int(home.get("entries", 0)), int(home.get("small_bytes", 0))
    store = int((facts.get("ostree") or {}).get("store_bytes", 0))
    data_ratio = float(u["data_ratio"])
    meta_ratio = float(u["meta_ratio"])
    reserve = int(u["global_reserve"])
    unalloc = int(u["unallocated"])
    p = {"floor": FLOOR_BYTES, "store": store, "home": hbytes, "entries": entries, "small_bytes": small,
         "data_ratio": data_ratio, "meta_ratio": meta_ratio, "reserve": reserve, "unallocated": unalloc,
         "mixed": bool(u.get("mixed")), "margin": META_MARGIN}
    data_need = FLOOR_BYTES + store
    per_name = entries * ENTRY_META_BYTES + small
    if p["mixed"]:
        pool_free = max(0, u["data_size"] - u["data_used"] - reserve)
        p.update(share=0, per_name=per_name, home_meta=per_name, data_need=data_need,
                 data_free=pool_free, data_avail=pool_free + unalloc / data_ratio)
        p["meta_need"] = int(per_name * META_MARGIN)
        p["meta_avail"] = max(0, p["data_avail"] - data_need)
        p["meta_need_raw"], p["meta_avail_raw"] = p["meta_need"] * data_ratio, p["meta_avail"] * data_ratio
        p["data_ok"] = data_need <= p["data_avail"]
        p["meta_ok"] = p["meta_need"] <= p["meta_avail"]
        return p
    data_free = max(0, u["data_size"] - u["data_used"])
    data_avail = data_free + unalloc / data_ratio
    data_draw_raw = max(0, data_need - data_free) * data_ratio
    used = int(u["data_used"])
    share = int(u["meta_used"] * (min(1.0, hbytes / used) if used else (1.0 if hbytes else 0.0)))
    home_meta = max(share, per_name)
    meta_need = int(home_meta * META_MARGIN)
    meta_unused = max(0, u["meta_size"] - u["meta_used"])
    meta_free = max(0, meta_unused - reserve)
    meta_need_raw = meta_need * meta_ratio
    meta_avail_raw = meta_free * meta_ratio + max(0, unalloc - data_draw_raw)
    p.update(data_need=data_need, data_free=data_free, data_avail=data_avail, data_draw_raw=data_draw_raw,
             share=share, per_name=per_name, home_meta=home_meta, meta_need=meta_need, meta_unused=meta_unused,
             meta_free=meta_free, meta_need_raw=meta_need_raw, meta_avail_raw=meta_avail_raw)
    p["data_ok"] = not data_need > data_avail
    p["meta_ok"] = not meta_need_raw > meta_avail_raw
    return p


def space_lines(p):
    """H9's two figures as check prints them and the refusals repeat them: (data, metadata), each None when there is
    no figure (the root is not btrfs, or its usage could not be read)."""
    if p is None or p.get("unmeasured"):
        return None, None
    d = (f"{p['data_avail'] / GB:.1f} GB free, {p['data_need'] / GB:.1f} GB needed: floor {p['floor'] / GB:.1f} + "
         f"old store {p['store'] / GB:.1f} (home is a reflink copy: no data at start)")
    est = (f"home's metadata {p['home_meta'] / GB:.2f} GB (share {p['share'] / GB:.2f}, per name "
           f"{p['per_name'] / GB:.2f}: {p['entries']} names) x margin {p['margin']:g}")
    if p["mixed"]:
        m = (f"{p['meta_avail'] / GB:.2f} GB left in the mixed data+metadata pool after data, "
             f"{p['meta_need'] / GB:.2f} GB needed: {est}")
    else:
        m = (f"{p['meta_avail_raw'] / GB:.2f} GB on the device, {p['meta_need_raw'] / GB:.2f} GB needed: {est} x ratio "
             f"{p['meta_ratio']:g}; on the device = (unused in metadata chunks {p['meta_unused'] / GB:.2f} - global "
             f"reserve {p['reserve'] / GB:.2f}) x {p['meta_ratio']:g} + unallocated {p['unallocated'] / GB:.2f} "
             f"less {p['data_draw_raw'] / GB:.2f} data takes")
    return d, m


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
        b = facts.get("boot") or {}
        if b.get("kind") == "partition":
            # The installer's LUKS layout (installer/rime-install): /boot is ext4 on its own partition. start records
            # it in host.toml [boot] device/fstype and rime.rootfs's generator mounts it at /boot on NixOS.
            if b.get("fstype") not in BOOT_PART_FSTYPES or not b.get("uuid"):
                refusals.append(("boot-partition-fstype", f"/boot is a {b.get('fstype')} partition (uuid "
                                 f"{b.get('uuid')}): the bridge hands only {'/'.join(BOOT_PART_FSTYPES)} with a "
                                 "UUID to NixOS"))
            else:
                notes.append(f"/boot is its own {b['fstype']} partition UUID={b['uuid']}: host.toml [boot] device "
                             "records it and NixOS mounts it at /boot (rime.rootfs)")
        elif b.get("kind") != "top-level-dir":
            refusals.append(("boot-unknown", "/boot is neither a directory of the root btrfs nor a partition"))
    plan = space_plan(facts)
    data_line, meta_line = space_lines(plan)
    if plan is not None and plan.get("unmeasured"):
        refusals.append(("space-unmeasured", "`btrfs filesystem usage -b` of the root could not be read, so the data "
                         "and metadata the move needs cannot be checked (H9)"))
    elif plan is not None:
        if not plan["data_ok"]:
            refusals.append(("free-space", f"data: {data_line} (H9)"))
        if not plan["meta_ok"]:
            refusals.append(("metadata-space", f"metadata: {meta_line} (H9)"))
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
    home = facts.get("home") or {}
    if home.get("nested_subvolumes"):
        refusals.append(("home-nested-subvolume", "home holds btrfs subvolumes of its own ("
                         + ", ".join(shown(p) for p in home["nested_subvolumes"]) + "): neither the copy of home nor "
                         "a backup crosses into them, so their files would be left behind: move their files into "
                         "ordinary directories (or delete them) first"))
    if backup is None and home.get("in_top_level") is False:
        refusals.append(("home-subvolume-needs-backup", "home is its own btrfs subvolume, so the @pre-nixos snapshot "
                         "(the restore point when there is no --backup) would not hold it: give --backup DIR off "
                         "this disk"))
    elif backup is None and root.get("fstype") == "btrfs" and "home" in facts and (
            home.get("in_top_level") is None or home.get("nested_subvolumes") is None):
        notes.append("could not read how home sits in the btrfs subvolumes: start checks it again and its restore "
                     "test refuses a home the @pre-nixos snapshot does not hold")
    if t == "E":
        lo = trial_leftovers(facts)
        if lo["files"] or lo["nvram"] or lo["top"]:
            notes.append("topology E repair pending: " + ", ".join(
                [f"{x['esp']}:{x['path']}" for x in lo["files"]] + [f"Boot{n}" for n in lo["nvram"]]
                + [f"/{n} (btrfs top level)" for n in lo["top"]]))
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
        lines.append(f"  /boot: {b.get('kind')}" + (f" {b.get('device')} {b.get('fstype')}" if b.get("kind") == "partition"
                                                     else "") + f" uuid={b.get('uuid')} loader->{b.get('loader_link')} "
                     f"custom.cfg sourced={b.get('sources_custom_cfg')}")
        ch = chain_read(facts)
        lines.append(f"  boot chain: {ch.get('how', 'not read')}; reads {ch.get('esp')} \\EFI\\{ch.get('dir')}\\grub.cfg"
                     f" shape={ch.get('shape')} -> fs {ch.get('target')}; /boot fs {ch.get('boot_uuid')}: "
                     + ("verified" if ch["ok"] else "boot-chain-unverified"))
        for e in facts.get("esps", []):
            for d, g in sorted(e.get("grub", {}).items()):
                sh, tg, _, cm = grub_shape(g.get("grub_cfg"))
                lines.append(f"    {e['device']} \\EFI\\{d}\\grub.cfg: {sh}, target "
                             f"{g.get('boot_uuid') if sh == 'bootupd-static' else tg}, commands {','.join(cm) or 'none'}")
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
    plan = space_plan(facts)
    data_line, meta_line = space_lines(plan)
    if plan is None:
        lines.append("space: the root is not btrfs (no figure)")
    elif plan.get("unmeasured"):
        lines.append("space: `btrfs filesystem usage -b` could not be read (no figure)")
    else:
        lines.append(f"space: data {data_line}")
        lines.append(f"space: metadata {meta_line}")
    if explain and plan is not None and not plan.get("unmeasured"):
        lines.append(DIVERGENCE)
    # With --backup, start's first step copies all of home off the disk: its destination is checked against this.
    lines.append(f"home: {facts.get('home', {}).get('bytes', 0) / GB:.2f} GB")
    if facts.get("backup"):
        b = facts["backup"]
        lines.append(f"backup: {b['path']} on {b.get('disk')}, {b.get('free_bytes', 0) / GB:.1f} GB free")
    else:
        lines.append(f"backup: none (optional: --backup DIR, off this disk). {RESTORE_POINT}")
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


RESTORE_POINT = ("The restore point is the read-only @pre-nixos snapshot start takes of the old system and home, on "
                 "this disk, restore-tested after it is taken. It costs no space at the move, it does not survive "
                 "the disk failing, and `rime migrate finish` deletes it.")


def gather(a):
    facts = collect_disks(a.disk) if a.disk else collect_live()
    if facts.get("root", {}).get("fstype") == "btrfs" and not a.disk:
        facts["existing_subvols"] = existing_subvols(facts["root"]["device"])
        if facts.get("var_fsroot") and isinstance(facts.get("home"), dict):
            facts["home"].update(home_in_top_level(facts["root"]["device"], facts["var_fsroot"]))
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


def home_in_top_level(dev, var_fsroot):
    """home_layout of the live system: its btrfs top level mounted read-only for the look."""
    m = Mounts()
    try:
        top = m.ro(dev, "btrfs", "subvolid=5")
        return home_layout(top, var_fsroot) if top else {"in_top_level": None, "nested_subvolumes": None}
    finally:
        m.close()


def home_layout(top, var_fsroot):
    """How home sits in the btrfs subvolumes under TOP (the top level, mounted): in_top_level is True when home is in
    the top level's own subvolume (so a snapshot of the top level holds it), False when it, /var or anything between
    is a subvolume of its own; nested_subvolumes lists the subvolumes inside home (a snapshot holds an empty
    placeholder for each, and `cp --one-file-system` does not enter them). None when it cannot be read."""
    home_rel = var_fsroot.strip("/") + "/home"
    in_top = same_subvolume(top, os.path.join(top, home_rel))
    paths = subvolume_paths(top)
    if paths is None:
        return {"in_top_level": in_top, "nested_subvolumes": None}
    if any(home_rel == p or home_rel.startswith(p + "/") for p in paths):
        in_top = False
    return {"in_top_level": in_top, "nested_subvolumes": sorted("/" + p for p in paths if p.startswith(home_rel + "/"))}


def subvolume_paths(top):
    """Every subvolume's path relative to the top level TOP (`btrfs subvolume list`), None when it cannot be listed."""
    r = run(["btrfs", "subvolume", "list", top], check=False)
    if r.returncode:
        return None
    return [p for p in (ln.partition(" path ")[2] for ln in r.stdout.splitlines()) if p]


# The empty directory a btrfs snapshot holds in place of a nested subvolume is inode 2 (no other file or directory
# is). Measured on 7.2: its st_dev is not the snapshot's either, but the inode is what btrfs defines.
BTRFS_EMPTY_SUBVOL_INO = 2


def same_subvolume(top, path):
    """PATH is in TOP's btrfs subvolume: the same st_dev (every subvolume has its own) and PATH is not the placeholder
    a snapshot leaves for a nested subvolume. None when either cannot be read."""
    try:
        a, b = os.stat(top), os.stat(path)
    except OSError:
        return None
    return a.st_dev == b.st_dev and b.st_ino != BTRFS_EMPTY_SUBVOL_INO


def shown(path):
    """PATH for a message: control and other unprintable characters escaped, so a file name cannot write to the
    terminal of whoever runs this."""
    return "".join(c if c.isprintable() else (f"\\x{ord(c):02x}" if ord(c) < 0x100 else f"\\u{ord(c):04x}")
                   for c in str(path))


def spread(items, k):
    """Up to K of ITEMS spread evenly over all of them, the first and the last included."""
    n = len(items)
    if n <= k:
        return list(items)
    if k == 1:
        return [items[0]]
    return [items[i * (n - 1) // (k - 1)] for i in range(k)]


def start_record():
    """The record a start leaves (STATE/state.json): None when there is none, {"unreadable": why} when it cannot be
    read as JSON."""
    p = os.path.join(STATE, "state.json")
    try:
        with open(p) as f:
            rec = json.load(f)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as e:
        return {"unreadable": str(e)}
    return rec if isinstance(rec, dict) else {"unreadable": "not a JSON object"}


def record_refusal(rec):
    """The refusal a start record means, or None. The same rule, read the same way, as the bootc side's `rime` for its
    own verbs (rimed/rime/src/nixos_bridge.rs `classify`) and rime-update-check (readiness release): `done` set (not
    null) is a finished move; otherwise a start that undid itself (`failed` PRESENT, whatever its text, and the last
    step "rolled back") allows another; any other record, and an unreadable one, refuses by name. (`failed` is the
    exception's text, which is "" for a Ctrl-C: reading it as a truth value refused a start that had rolled back.)"""
    p = os.path.join(STATE, "state.json")
    if rec is None:
        return None
    if "unreadable" in rec:
        return ("start-record-unreadable", f"{p} cannot be read ({rec['unreadable']}); look at it before anything else")
    steps = rec.get("steps")
    steps = steps if isinstance(steps, list) else []
    last = steps[-1] if steps else "none"
    if rec.get("done") is not None:
        return ("already-moved", f"{p}: this machine was moved to NixOS on {rec['done']}; boot the Rime (NixOS) entry")
    if "failed" in rec and last == "rolled back":
        return None
    return ("start-unfinished", f"{p}: a start reached step {last!r} and neither finished nor rolled itself back; "
            "nothing new is started over it")


def cmd_check(a):
    facts = gather(a)
    c, refusals, notes = evaluate(facts, facts.get("backup"))
    rec = start_record()
    if record_refusal(rec):
        refusals.append(record_refusal(rec))
    preview = None
    if getattr(a, "payload", None):
        try:
            man = verify_payload(a.payload)
            if a.explain and not a.disk:
                rc, text = carry_preview(a.payload, man)
                preview = {"rc": rc, "text": text}
                if rc:
                    refusals.append(("carry-refused", f"rime-hostlayer carry --plan exited {rc} (its report below)"))
        except Refused as e:
            refusals.append((e.name, str(e)))
    if a.json:
        print(json.dumps({"facts": facts, "classification": c, "space": space_plan(facts), "refusals": [{"name": n, "text": t} for n, t in refusals],
                          "notes": notes, "chain": chain_read(facts), "carry_preview": preview,
                          "start_record": rec}, indent=1, sort_keys=True))
    else:
        sys.stdout.write(report(facts, c, refusals, notes, a.explain))
        if preview is not None:
            sys.stdout.write("\nthe /etc carry start would run (rime-hostlayer carry --plan; nothing written). "
                             "LEFT BEHIND is what does not come over:\n"
                             + "".join(f"  | {ln}\n" for ln in preview["text"].splitlines()))
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


def is_subvolume(path):
    """A btrfs subvolume's root directory has inode 256."""
    try:
        return os.lstat(path).st_ino == 256 and stat.S_ISDIR(os.lstat(path).st_mode)
    except OSError:
        return False


def archive_trial_top(top, arch_rel):
    """repair-e: MOVE the trial's /composefs and /state out of the btrfs top level TOP into TOP/ARCH_REL (the archive
    directory under /var/lib/rime/boot-migrate). Decided: archive, never delete (docs/nixos/bridge.md): the trial's
    /state holds the /etc and /var it ran with, which may hold what the user wrote while it ran, and a rename in the
    same btrfs costs no space and no time. Across a subvolume boundary (EXDEV) the tree is reflink-copied, compared
    and only then removed. Returns [{name, to, how}]."""
    moved = []
    for name in TRIAL_TOP:
        src = os.path.join(top, name)
        if not os.path.lexists(src):
            continue
        dst = os.path.join(top, arch_rel, name)
        os.makedirs(os.path.dirname(dst), mode=0o700, exist_ok=True)
        if os.path.lexists(dst):
            raise Failed(f"{dst} already exists")
        try:
            os.rename(src, dst)
            how = "renamed"
        except OSError as e:
            if e.errno != errno.EXDEV:
                raise
            run(["cp", "-a", "--reflink=auto", src, dst])
            if tree_sums(src) != tree_sums(dst):
                raise Failed(f"the archive copy of /{name} does not match the original; the original is kept")
            if is_subvolume(src):
                run(["btrfs", "subvolume", "delete", src])
            else:
                shutil.rmtree(src)
            how = "copied (reflink), compared, original removed"
        moved.append({"name": name, "to": "/" + os.path.join(arch_rel, name), "how": how})
    return moved


def mount_top_rw(facts):
    """The root btrfs top level (subvolid=5), read-write, in this process's private mount namespace."""
    root = facts["root"]
    dev = root["luks"]["mapper"] if root.get("luks") else root["device"]
    top = "/run/rime-migrate/top"
    os.makedirs(top, exist_ok=True)
    if not os.path.ismount(top):
        mount("-o", "subvolid=5", dev, top)
    return top


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
    for n in lo["top"]:
        print(f"  /{n} in the btrfs top level (moved into the archive, not deleted)")
    if not lo["files"] and not lo["nvram"] and not lo["top"]:
        print("  nothing: the repair has already run")
        return EXIT_OK
    if not a.confirm:
        print("nothing written; run again with --confirm to archive these (the ESP files and the NVRAM entry are then "
              "deleted, /composefs and /state are moved into the archive)")
        return EXIT_OK
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    arch = os.path.join("/var/lib/rime/boot-migrate", f"archive-{ts}")
    os.makedirs(arch, mode=0o700)
    manifest = {"at": ts, "topology": c, "files": [], "nvram": {}, "phase": phase_before.get("phase"), "top": []}
    for n in lo["nvram"]:
        with open(os.path.join(EFIVARS, f"Boot{n}-{EFI_GLOBAL}"), "rb") as f:
            raw = f.read()
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
                        manifest["files"].append({"path": os.path.relpath(p, arch), "sha256": sha256_file(p)})
            for x in [x for x in lo["files"] if x["esp"] == dev]:
                # The archive is read back before the ESP copy goes.
                src = find_ci(mnt, x["path"])
                for d, _, fs in os.walk(src):
                    for fn in fs:
                        rel = os.path.relpath(os.path.join(d, fn), mnt)
                        arc = os.path.join(arch, "esp", os.path.basename(dev), x["path"],
                                           os.path.relpath(os.path.join(d, fn), src))
                        if sha256_file(arc) != sha256_file(os.path.join(mnt, rel)):
                            raise Failed(f"archive of {rel} does not match the ESP")
                shutil.rmtree(src) if os.path.isdir(src) else os.unlink(src)
            os.sync()
        finally:
            run(["umount", mnt], check=False)
            os.rmdir(mnt)
    if lo["top"]:
        top = mount_top_rw(facts)
        var_rel = facts["var_fsroot"].strip("/")
        manifest["top"] = archive_trial_top(top, os.path.join(var_rel, os.path.relpath(os.path.realpath(arch), "/var")))
        for m in manifest["top"]:
            print(f"moved /{m['name']} -> {m['to']} ({m['how']})")
        os.sync()
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


# ── key-export (D2 backup rule: the LUKS header and the backup keys leave the disk) ───────────────────────────────

KEYS_NOT_EXPORTED = (
    "the LUKS recovery key: the installer showed it once and never stored it (installer/rime-install shreds its "
    "copy), so no program can export it; it is wherever it was written down",
    "a TPM2 enrolment: it is bound to this machine's TPM and cannot leave it (the LUKS header above holds the "
    "keyslot, the TPM holds the secret)",
)


def export_keys(dest, facts):
    """key-export into DEST (created, 0700): the LUKS2 header (luksHeaderBackup, then luksDump of the BACKUP FILE,
    which must name the volume's UUID) and rime backup's keys/ and recipients/. Returns the manifest it writes."""
    os.makedirs(dest, mode=0o700)
    man = {"luks": None, "backup_keys": {}, "not_exported": list(KEYS_NOT_EXPORTED)}
    luks = (facts.get("root") or {}).get("luks")
    if luks and luks.get("device"):
        hdr = os.path.join(dest, "luks-header.img")
        run(["cryptsetup", "luksHeaderBackup", luks["device"], "--header-backup-file", hdr])
        dump = run(["cryptsetup", "luksDump", hdr]).stdout
        if luks.get("uuid") and luks["uuid"].lower() not in dump.lower():
            raise Failed(f"the LUKS header backup does not name the volume's UUID {luks['uuid']}")
        with open(os.path.join(dest, "luks-dump.txt"), "w") as f:
            f.write(dump)
        man["luks"] = {"device": luks["device"], "uuid": luks.get("uuid"), "file": "luks-header.img",
                       "sha256": sha256_file(hdr)}
        print(f"keys: LUKS header of {luks['device']} written and read back (luksDump)", flush=True)
    for sub in ("keys", "recipients"):
        src = os.path.join(BACKUP_KEYS_ROOT, sub)
        if os.path.isdir(src):
            dst = os.path.join(dest, "rime-backup", sub)
            shutil.copytree(src, dst, symlinks=True)
            man["backup_keys"].update(tree_sums(os.path.join(dest, "rime-backup"), [sub]))
    print(f"keys: rime backup {len(man['backup_keys'])} key file(s) copied", flush=True)
    with open(os.path.join(dest, "keys.json"), "w") as f:
        json.dump(man, f, indent=1, sort_keys=True)
    with open(os.path.join(dest, "README.txt"), "w") as f:
        f.write("Rime OS keys, exported by rime migrate key-export.\n\n"
                "luks-header.img  restore with: cryptsetup luksHeaderRestore <device> --header-backup-file luks-header.img\n"
                "rime-backup/     copy back to /var/lib/rime-backup/ (root, 0700) to read backups made on this machine\n\n"
                "NOT in this directory:\n" + "".join(f"  - {t}\n" for t in KEYS_NOT_EXPORTED))
    os.sync()
    return man


def cmd_key_export(a):
    facts = collect_live()
    b = backup_facts(a.to)
    if not b.get("exists"):
        raise Refused("backup-missing", f"{a.to} is not a directory")
    if b.get("disk") and b["disk"] == (facts.get("root") or {}).get("disk"):
        raise Refused("backup-same-disk", f"{a.to} is on {b['disk']}, the disk whose keys these are")
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    dest = os.path.join(a.to, f"rime-keys-{ts}")
    man = export_keys(dest, facts)
    print(f"exported to {dest}: LUKS header {'yes' if man['luks'] else 'no (the root is not in LUKS)'}, "
          f"{len(man['backup_keys'])} backup key file(s); not exportable: the recovery key, a TPM2 binding")
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


def custom_outside_block(text):
    """sha256 of custom.cfg without the boot hook's own block: everything else in it belongs to the user."""
    return hashlib.sha256(re.sub(r"### BEGIN rime-boot.*?### END rime-boot ###\n?", "", text,
                                 flags=re.S).encode()).hexdigest()


def containment_diff(before, after):
    """H8: what changed between two containment snapshots ({} = nothing it must not touch)."""
    diff = sorted(k for k in set(before) | set(after) if before.get(k) != after.get(k))
    return {k: sorted(x for x in set(before.get(k) or {}) ^ set(after.get(k) or {}) | {
        x for x in set(before.get(k) or {}) & set(after.get(k) or {}) if before[k][x] != after[k][x]})[:5]
            if isinstance(before.get(k), dict) or isinstance(after.get(k), dict)
            else [before.get(k), after.get(k)] for k in diff}


def containment_snapshot(top, facts):
    """H8: what the bridge must leave byte for byte: ostree's /boot (loader, entries, kernels, grub.cfg and
    bootuuid.cfg) and every file on every ESP. custom.cfg is compared outside the Rime block only."""
    boot = os.path.join(top, "boot") if facts["boot"]["kind"] == "top-level-dir" else "/boot"
    snap = {"boot": {k: v for k, v in tree_sums(boot).items()
                     if not k.startswith(("rime/", "grub2/rime-nixos.cfg", "grub2/custom.cfg", "lost+found"))}}
    snap["custom_outside_block"] = custom_outside_block(read(os.path.join(boot, "grub2/custom.cfg"), ""))
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
    sample = spread(files, 64)
    bad = [p for p in sample if sha256_file(p) != sha256_file(os.path.join(d, "home", os.path.relpath(p, var_home)))]
    if bad:
        raise Failed(f"backup restore test failed for {len(bad)} of {len(sample)} files: "
                     + ", ".join(shown(p) for p in bad[:3]))
    print(f"backup: restore test {len(sample)} of {len(files)} files read back equal", flush=True)
    export_keys(os.path.join(d, "keys"), facts)
    with open(os.path.join(d, "RESTORE-TESTED"), "w") as f:
        f.write(f"{len(sample)} of {len(files)} files compared equal at {ts}\n")
    os.sync()
    return d


def test_restore_point(snap, var_fsroot, live_home, sample=64):
    """The restore test when there is no off-disk backup: home in the read-only snapshot SNAP is real (in the
    snapshot's own subvolume, not an empty placeholder left by a nested subvolume) and reads back equal to the live
    home. The live home keeps changing while start runs (the machine is in use), so a sampled file is compared only
    when its size and mtime are still the snapshot's, before and after it is read; others count as changed since,
    not as failures. At least one file must compare equal when home has any, and an empty home in the snapshot
    passes only when the live home is empty too. A subvolume nested in home (an empty placeholder in the snapshot)
    fails it: the snapshot does not hold that part of home."""
    snap_home = os.path.join(snap, var_fsroot.lstrip("/"), "home")
    if not os.path.isdir(snap_home) or not same_subvolume(snap, snap_home):
        raise Failed(f"the restore point {snap} does not hold home ({snap_home})")
    files, nested = [], []
    for root, dirs, fs in os.walk(snap_home):
        # os.walk lists a link to a directory with the directories (and does not enter it). The link is a name in
        # the snapshot's own subvolume, never a placeholder; followed, it lands wherever it points (an absolute
        # link in a container layer, etc/ssl/certs -> /etc/pki/tls/certs, reaches the running system's /etc).
        for d in [d for d in dirs if not os.path.islink(os.path.join(root, d))
                  and not same_subvolume(snap, os.path.join(root, d))]:
            dirs.remove(d)
            nested.append(os.path.join(root, d))
        for n in fs:
            p = os.path.join(root, n)
            if stat.S_ISREG(os.lstat(p).st_mode):
                files.append(p)
    if nested:
        raise Failed(f"the restore point {snap} does not hold all of home: {len(nested)} subvolume(s) nested in it "
                     "are empty placeholders there: " + ", ".join(shown(p) for p in nested[:3]))
    files.sort()
    picked = spread(files, sample)
    equal, changed, bad = 0, 0, []
    for p in picked:
        live = os.path.join(live_home, os.path.relpath(p, snap_home))
        a = os.lstat(p)
        try:
            b = os.lstat(live)
        except FileNotFoundError:
            changed += 1
            continue
        if not stat.S_ISREG(b.st_mode) or (a.st_size, a.st_mtime_ns) != (b.st_size, b.st_mtime_ns):
            changed += 1
            continue
        same = sha256_file(p) == sha256_file(live)
        try:
            c = os.lstat(live)
        except FileNotFoundError:
            c = None
        if c is None or (c.st_size, c.st_mtime_ns, c.st_ino) != (b.st_size, b.st_mtime_ns, b.st_ino):
            changed += 1  # written (or replaced) while it was read: not a verdict on the snapshot
        elif same:
            equal += 1
        else:
            bad.append(p)
    if bad:
        raise Failed(f"restore test of {snap}: {len(bad)} of {len(picked)} files differ from home: "
                     + ", ".join(shown(p) for p in bad[:3]))
    if files and not equal:
        raise Failed(f"restore test of {snap}: none of {len(picked)} sampled files could be compared")
    if not files and has_regular_file(live_home):
        raise Failed(f"the restore point {snap} holds an empty home, and home is not empty")
    print(f"restore point: {snap} holds home; {equal} of {len(picked)} sampled files ({len(files)} in all) read back "
          f"equal, {changed} changed since the snapshot", flush=True)
    return {"path": snap, "files": len(files), "sampled": len(picked), "equal": equal, "changed_since": changed}


def has_regular_file(top):
    """TOP holds at least one regular file (it stops at the first)."""
    for root, _, fs in os.walk(top):
        for n in fs:
            try:
                if stat.S_ISREG(os.lstat(os.path.join(root, n)).st_mode):
                    return True
            except OSError:
                continue
    return False


def mask_old_side(state):
    # Recorded as it goes: a failure half way through still unmasks exactly what was masked (unmask_old_side).
    # The timers stop FIRST: masking the service a running timer triggers fails that timer ("Unit to trigger
    # vanished", result 'resources'), and after a rollback a failed unit holds the next update (the first real move
    # left both timers failed until a reboot). The ones that were running are recorded, so a rollback starts them.
    masked = state.setdefault("masked", [])
    running = state.setdefault("timers_running", [])
    for u in OLD_SIDE_TIMERS:
        if u not in running and run(["systemctl", "is-active", "--quiet", u], check=False).returncode == 0:
            running.append(u)
        run(["systemctl", "stop", u], check=False)
    for u in MASK_UNITS:
        p = os.path.join(SYSTEMD_ETC, u)
        if os.path.lexists(p):
            continue
        os.symlink("/dev/null", p)
        masked.append(u)
    run(["systemctl", "daemon-reload"], check=False)
    print(f"old side: masked {', '.join(masked) or 'nothing new'}", flush=True)


def unmask_old_side(state):
    for u in state.get("masked", []):
        p = os.path.join(SYSTEMD_ETC, u)
        if os.path.islink(p) and os.readlink(p) == "/dev/null":
            os.unlink(p)
    run(["systemctl", "daemon-reload"], check=False)
    # Leave the old system as start found it: nothing failed, and the timers that were running, running again.
    if state.get("masked"):
        run(["systemctl", "reset-failed", *state["masked"]], check=False)
    for u in state.get("timers_running", []):
        run(["systemctl", "start", u], check=False)


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


def verify_payload(payload, trusted=None):
    """The payload: manifest.json signed (PAYLOAD_SIG, by a key payload_keys() trusts), then every file it names by
    sha256. Refused by name: payload-key-missing, payload-key-bad, payload-missing, payload-unsigned,
    payload-key-untrusted, payload-signature-bad, payload-corrupt."""
    trusted = payload_keys() if trusted is None else trusted
    mp = os.path.join(payload, "manifest.json")
    try:
        with open(mp, "rb") as f:
            raw = f.read()
    except OSError as e:
        raise Refused("payload-missing", f"{mp}: {e.strerror}")
    signer = verify_manifest_signature(raw, read(os.path.join(payload, PAYLOAD_SIG)), trusted)
    man = json.loads(raw)
    for key in ("store", "registration", "preview"):
        if key not in man:
            if key == "preview":
                continue
            raise Refused("payload-corrupt", f"the manifest names no {key}")
        p = os.path.join(payload, man[key]["file"])
        if os.path.basename(man[key]["file"]) != man[key]["file"] or not os.path.isfile(p):
            raise Refused("payload-corrupt", f"{key} file {man[key]['file']!r} is not a file in {payload}")
        got = sha256_file(p)
        if got != man[key]["sha256"]:
            raise Refused("payload-corrupt", f"{p}: sha256 {got}, manifest says {man[key]['sha256']}")
    if not re.fullmatch(r"/nix/store/[0-9a-z]{32}-[^/]+", man["toplevel"]) or not re.fullmatch(
            r"/nix/store/[0-9a-z]{32}-[^/]+", man["nix"]):
        raise Refused("payload-corrupt", "manifest toplevel/nix are not store paths")
    man["signed_by"] = signer
    print(f"payload: manifest signed by {signer}; store, registration" + (", preview" if "preview" in man else "")
          + " match their sha256", flush=True)
    return man


def preview_extract(tar, dest):
    """Extract preview.tar into DEST with tarfile's "data" filter, except for what a NixOS etc/ really holds and that
    filter refuses: a unit MASK (a link to /dev/null, or the /dev/null character device a dereferencing tar writes for
    one) is made again as a link to /dev/null AFTER everything else, as a leaf whose directory lies inside DEST, so no
    other member can be written through it. Any other device, fifo or absolute link is left out and counted.
    Returns (masks, skipped)."""
    masks, skipped = [], []

    def keep(m, path):
        if (m.ischr() and (m.devmajor, m.devminor) == (1, 3)) or (m.issym() and m.linkname == "/dev/null"):
            masks.append(m.name)
            return None
        if m.isdev() or m.isfifo():
            skipped.append(m.name)
            return None
        try:
            return tarfile.data_filter(m, path)
        except tarfile.FilterError:
            skipped.append(m.name)
            return None

    with tarfile.open(tar) as tf:
        tf.extractall(dest, filter=keep)
    real = os.path.realpath(dest)
    for name in masks:
        target = os.path.join(dest, name)
        parent = os.path.realpath(os.path.dirname(target))
        if os.path.commonpath([parent, real]) != real or os.path.lexists(target):
            skipped.append(name)
            continue
        os.makedirs(parent, exist_ok=True)
        os.symlink("/dev/null", target)
    return masks, skipped


def carry_preview(payload, man, src="/", cmdline="/proc/cmdline"):
    """The left-behind report BEFORE start: the payload's own rime-hostlayer (preview.tar: the program, the NixOS
    system's etc/ and sw/bin names, the greeter's shipped config) run as `carry --plan` by this system's python,
    against an empty root, as start will run it. Writes nothing outside a temporary directory. Returns (rc, text)."""
    if "preview" not in man:
        raise Refused("payload-no-preview", "this payload carries no preview.tar, so the carry cannot be shown first")
    tmp = tempfile.mkdtemp(prefix="rime-migrate-preview-")
    try:
        masks, skipped = preview_extract(os.path.join(payload, man["preview"]["file"]), tmp)
        note = (f"preview: {len(masks)} unit mask(s) kept as links to /dev/null"
                + (f", {len(skipped)} special file(s) left out: {', '.join(skipped[:5])}" if skipped else "") + "\n")
        root = os.path.join(tmp, "root")
        os.makedirs(root)
        env = {"PATH": "/usr/bin:/usr/sbin", "LANG": "C.UTF-8",
               "RIME_GREETD_DEFAULT": os.path.join(tmp, "greetd-config.toml")}
        r = run([sys.executable, "-I", "-B", os.path.join(tmp, "rime-hostlayer"), "carry", "--from", src, "--root",
                 root, "--system", os.path.join(tmp, "system"), "--cmdline", cmdline, "--plan"], check=False, env=env)
        return r.returncode, note + (r.stdout or "") + (r.stderr or "")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def chroot(t, *cmd, env=None, stdin=None, check=True):
    e = {"PATH": "/run/current-system/sw/bin:/nix/var/nix/profiles/system/sw/bin", "HOME": "/root", "LANG": "C.UTF-8"}
    e.update(env or {})
    # The host's chroot by absolute path: with env= given, the child's PATH (the target's) is what exec searches.
    r = subprocess.run([shutil.which("chroot") or "/usr/sbin/chroot", t, *cmd], env=e, stdin=stdin, text=True, stdout=subprocess.PIPE,
                       stderr=subprocess.STDOUT)
    sys.stdout.write("".join(f"  | {ln}\n" for ln in r.stdout.splitlines()[-40:]))
    if r.returncode and check:
        raise Failed(f"chroot {' '.join(cmd)} exited {r.returncode}")
    return r


# NixOS activation writes knobs of the RUNNING kernel (nixpkgs modprobe.nix: /proc/sys/kernel/modprobe; udev.nix:
# /sys/module/firmware_class/parameters/path). In start's chroot the running kernel is the bootc machine's, so its
# module autoload would exec a /nix/store path that machine does not have until it reboots (found on a host whose
# kernel.modprobe had been left pointing into a store). Each knob gets a scratch file bound over it inside the target
# for the activation, and the host's values are read again afterwards.
HOST_KNOBS = ("proc/sys/kernel/modprobe", "sys/module/firmware_class/parameters/path")


def first_activation(t, toplevel, root="/"):
    before = {k: read(os.path.join(root, k)) for k in HOST_KNOBS}
    shields = []
    try:
        for k in HOST_KNOBS:
            dst = os.path.join(t, k)
            if before[k] is None or not os.path.exists(dst):
                continue
            src = os.path.join(t, "run", "rime-knob-" + k.replace("/", "-"))
            with open(src, "w") as f:
                f.write(before[k])
            mount("--bind", src, dst)
            shields.append(dst)
        # As nixos-enter does: activation in a chroot may fail parts that need the running system (it runs again
        # at the first boot); its rc is recorded, not fatal.
        r = chroot(t, f"{toplevel}/activate", check=False,
                   env={"IN_NIXOS_ENTER": "1", "LOCALE_ARCHIVE": f"{toplevel}/sw/lib/locale/locale-archive"})
    finally:
        for dst in reversed(shields):
            run(["umount", dst], check=False)
    changed = []
    for k in HOST_KNOBS:
        p = os.path.join(root, k)
        if before[k] is not None and read(p) != before[k]:
            changed.append("/" + k)
            try:
                with open(p, "w") as f:
                    f.write(before[k] if before[k].strip() else "\n")
            except OSError as e:
                log(f"could not put {p} back: {e}")
    if changed:
        raise Failed(f"the first activation changed the running kernel's {', '.join(changed)} (put back)")
    return r


def mount(*args):
    run(["mount", *args])


def boot_had(bdir):
    """What of the boot hook's own files /boot held before start (a rollback removes only what start made)."""
    return {"rime": os.path.lexists(os.path.join(bdir, "rime")),
            "rime-nixos.cfg": os.path.lexists(os.path.join(bdir, "grub2/rime-nixos.cfg"))}


def undo_boot(bdir, old_custom, had):
    """Rollback of the boot side: custom.cfg back as it was, and the hook's menu file and kernel directory gone when
    start made them (they are outside every subvolume the rollback deletes)."""
    p = os.path.join(bdir, "grub2/custom.cfg")
    if old_custom is not None:
        with open(p, "w") as f:
            f.write(old_custom)
    elif os.path.exists(p):
        os.unlink(p)
    menu = os.path.join(bdir, "grub2/rime-nixos.cfg")
    if not had.get("rime-nixos.cfg") and os.path.lexists(menu):
        os.unlink(menu)
    kdir = os.path.join(bdir, "rime")
    if not had.get("rime") and os.path.isdir(kdir) and not os.path.islink(kdir):
        shutil.rmtree(kdir)


def cmd_start(a):
    if not a.yes:
        raise Refused("not-confirmed", "start needs --yes (read `rime migrate check --explain` first)")
    facts = gather(a)
    c, refusals, notes = evaluate(facts, facts.get("backup"))
    if facts.get("mode") != "live":
        raise Refused("not-live", "start runs on the booted bootc system only")
    if c["topology"] == "E":
        lo = trial_leftovers(facts)
        if lo["files"] or lo["nvram"] or lo["top"]:
            refusals.append(("e-repair-pending", "run `rime migrate repair-e --confirm` first (D3)"))
    if record_refusal(start_record()):
        refusals.append(record_refusal(start_record()))
    if refusals:
        sys.stdout.write(report(facts, c, refusals, notes, True))
        return EXIT_REFUSED
    man = verify_payload(a.payload)
    # The left-behind report comes first, always (D2): a payload with no preview.tar is refused by name
    # (payload-no-preview, raised by carry_preview) instead of starting without showing what stays behind.
    rc, text = carry_preview(a.payload, man)
    sys.stdout.write("the /etc carry (preview; the real one runs below):\n"
                     + "".join(f"  | {ln}\n" for ln in text.splitlines()))
    if rc:
        raise Refused("carry-refused", f"rime-hostlayer carry --plan exited {rc}; nothing was written")
    root = facts["root"]
    dev = root["luks"]["mapper"] if root.get("luks") else root["device"]
    state = {"started": datetime.datetime.now(datetime.timezone.utc).isoformat(), "topology": c, "steps": []}
    os.makedirs(STATE, mode=0o700, exist_ok=True)

    def step(s):
        state["steps"].append(s)
        with open(os.path.join(STATE, "state.json"), "w") as f:
            json.dump(state, f, indent=1, sort_keys=True)
        print(f"== {s}", flush=True)

    os.makedirs(RUN_DIR, exist_ok=True)
    top = os.path.join(RUN_DIR, "top")
    os.makedirs(top, exist_ok=True)
    mount("-o", "subvolid=5", dev, top)
    # rime-migrate-from-apex (and a store compaction) must have finished before anything is copied (D2).
    wait_units()
    made = []
    bdir = os.path.join(top, "boot") if facts["boot"]["kind"] == "top-level-dir" else "/boot"
    old_custom = read(os.path.join(bdir, "grub2/custom.cfg"))
    had = boot_had(bdir)
    # Every step after the record exists is inside the rollback (wave 3): a failure in the backup, while masking the
    # old side or at the first fsck gate used to leave a record with neither `failed` nor `rolled back`, units masked,
    # and the bootc verbs refused for good. Now it unmasks what it masked and records `rolled back` like any other.
    try:
        if a.backup:
            step("backup")
            state["backup"] = do_backup(a.backup, facts, os.path.join(top, facts["var_fsroot"].lstrip("/"), "home"))
        else:
            state["backup"] = None
        step("old side")
        wait_units()
        mask_old_side(state)
        step("fsck gate before")
        state["fsck_before"] = fsck_gate(top, facts, "before")
        before = containment_snapshot(top, facts)
        step("snapshot @pre-nixos")
        run(["btrfs", "subvolume", "snapshot", "-r", top, os.path.join(top, "@pre-nixos")])
        made.append("@pre-nixos")
        if not a.backup:
            # No copy off the disk: the snapshot is the restore point, so it is tested like a backup would be.
            state["restore_point"] = test_restore_point(os.path.join(top, "@pre-nixos"), facts["var_fsroot"],
                                                        os.path.join(top, facts["var_fsroot"].lstrip("/"), "home"))
            if (root.get("luks") or os.path.isdir(BACKUP_KEYS_ROOT)):
                print("keys: no --backup, so nothing left this disk; `sudo rime migrate key-export --to DIR` "
                      "(a few MB, e.g. a USB stick) copies the LUKS header and rime backup's keys off it at any time",
                      flush=True)
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
        t = os.path.join(RUN_DIR, "target")
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
        # The old kernel command line goes with it: rd.luks.*, the console keymap and resume become host.toml
        # [boot] kernel_params (a LUKS root does not unlock without them).
        with open("/proc/cmdline") as src, open(os.path.join(t, "run/rime-cmdline"), "w") as dst:
            dst.write(src.read())
        chroot(t, f"{man['toplevel']}/sw/bin/rime-hostlayer", "carry", "--from", "/run/rime-src", "--root", "/",
               "--system", man["toplevel"], "--cmdline", "/run/rime-cmdline")
        # The boot-v2 record (phase=failed on topology E, and repair-e's archive) carries over as it is: the
        # NixOS side must never re-run that migration or forget it failed (D1, D3).
        bm = os.path.realpath("/var/lib/rime/boot-migrate")
        if os.path.isdir(bm):
            # reflink: repair-e's archive (the trial's /composefs and /state) can be large; same btrfs, no copy
            run(["cp", "-a", "--reflink=auto", bm + "/.", os.path.join(t, "var/lib/rime/boot-migrate")])
            state["boot_migrate_carried"] = sorted(os.listdir(bm))
        ht = os.path.join(t, "var/lib/rime/host.toml")
        state["boot_table"] = boot_table(facts)
        write_boot_table(ht, state["boot_table"])
        chroot(t, f"{man['toplevel']}/sw/bin/rime-hostlayer", "host", "validate", "--file", "/var/lib/rime/host.toml")
        step("first activation and boot entry")
        open(os.path.join(t, "etc/NIXOS"), "a").close()
        if os.path.lexists(os.path.join(t, "etc/mtab")):
            os.unlink(os.path.join(t, "etc/mtab"))
        os.symlink("/proc/mounts", os.path.join(t, "etc/mtab"))
        # switch-to-configuration boot below is fatal; the activation's rc is not (first_activation).
        r = first_activation(t, man["toplevel"])
        state["activate_rc"] = r.returncode
        print(f"first activation rc={r.returncode}", flush=True)
        chroot(t, "/nix/var/nix/profiles/system/bin/switch-to-configuration", "boot",
               env={"NIXOS_INSTALL_BOOTLOADER": "1"})
        for p in reversed(("run/rime-src/etc", "run/rime-src/usr", "run/rime-src/var")):
            run(["umount", os.path.join(t, p)], check=False)
        run(["umount", "-R", os.path.join(t, "run")], check=False)
        os.sync()
        step("fsck gate after")
        state["fsck_after"] = fsck_gate(top, facts, "after")
        after = containment_snapshot(top, facts)
        detail = containment_diff(before, after)
        if detail:
            raise Failed(f"fsck containment: the bridge changed what it must not: {json.dumps(detail)}")
        print("containment: ostree /boot, custom.cfg outside the Rime block, deployments and every ESP unchanged",
              flush=True)
    except BaseException as e:
        log(f"failed: {e}; removing what the bridge made")
        run(["umount", "-R", os.path.join(RUN_DIR, "target")], check=False)
        undo_boot(bdir, old_custom, had)
        for s in reversed(made):
            run(["btrfs", "subvolume", "delete", os.path.join(top, s)], check=False)
        if state.get("relabelled_from"):
            run(["btrfs", "filesystem", "label", top, state["relabelled_from"]], check=False)
        unmask_old_side(state)
        state["failed"] = str(e) or type(e).__name__   # a Ctrl-C's text is "": the record names it
        step("rolled back")
        raise
    state["done"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    step("done")
    report_path = os.path.join(RUN_DIR, "target", "var/lib/rime/carry/report.txt")
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
    c.add_argument("--payload")
    r = sub.add_parser("repair-e")
    r.add_argument("--confirm", action="store_true")
    k = sub.add_parser("key-export")
    k.add_argument("--to", required=True)
    s = sub.add_parser("start")
    s.add_argument("--backup")
    s.add_argument("--payload", required=True)
    s.add_argument("--yes", action="store_true")
    s.set_defaults(disk=[])
    a = ap.parse_args(argv)
    if os.geteuid() != 0:
        print(f"{PROG}: needs root (sudo rime migrate {a.cmd})", file=sys.stderr)
        return EXIT_USAGE
    in_private_namespace(argv)
    try:
        return {"check": cmd_check, "repair-e": cmd_repair_e, "key-export": cmd_key_export,
                "start": cmd_start}[a.cmd](a)
    except Refused as e:
        print(f"REFUSED [{e.name}] {e}")
        return EXIT_REFUSED
    except Failed as e:
        print(f"{PROG}: FAILED: {e}", file=sys.stderr)
        return EXIT_FAILED


if __name__ == "__main__":
    sys.exit(main())
