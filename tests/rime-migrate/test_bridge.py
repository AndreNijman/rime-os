"""rime-migrate's unit tests: the classifier and the refusals over facts dicts, the NVRAM load-option parser, and
the CLI's own guards. Run by the package's installCheckPhase against the installed program (RIME_MIGRATE).

The facts dicts are the shape collect_live()/collect_disks() produce. Each topology fixture is built from the
layouts in out/understand-boot.md; the katana one from the facts PLAN.md D3 records (BootCurrent 0000 = APEX-OS
Primary on the Windows ESP, Boot0006 the trial's systemd-boot, phase=failed). When RIME_KATANA_STATE names the
katana snapshot, its real boot-migrate state files are read at runtime too (never copied into the repo).
"""

import copy
import importlib.machinery
import importlib.util
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
import uuid

PROG = os.environ.get("RIME_MIGRATE", os.path.join(os.path.dirname(__file__), "..", "rime-bridge.py"))


def load():
    loader = importlib.machinery.SourceFileLoader("rime_bridge", PROG)
    spec = importlib.util.spec_from_loader("rime_bridge", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


B = load()
GB = 10**9
ROOT_UUID = "0b0b0b0b-0000-4000-8000-000000000001"
RIME_ESP = "11111111-1111-4111-8111-111111111111"
WIN_ESP = "22222222-2222-4222-8222-222222222222"
INST_ESP = "33333333-3333-4333-8333-333333333333"
STATIC_EFI = 'source ${config_directory}/bootuuid.cfg\nsearch --fs-uuid "${BOOT_UUID}" --set prefix\n' \
             'configfile $prefix/grub.cfg\n'


def esp(dev, disk, partuuid, dirs, grub=None, top=None):
    return {"device": dev, "disk": disk, "partuuid": partuuid, "uuid": "AAAA-0000", "size": 512 << 20,
            "dirs": dirs, "loader_entries": [], "grub": grub or {}, "mounted": True, "top": top or ["EFI"]}


def fedora_grub(boot_uuid=ROOT_UUID):
    return {"grub_cfg": STATIC_EFI, "boot_uuid": boot_uuid}


def usage(data_size, data_used, meta_size, meta_used, unallocated, reserve=GB // 2, data_ratio=1.0, meta_ratio=2.0):
    """A root's `btrfs filesystem usage -b` as parse_btrfs_usage returns it (bytes; Size/Used logical)."""
    return {"data_size": data_size, "data_used": data_used, "meta_size": meta_size, "meta_used": meta_used,
            "unallocated": unallocated, "global_reserve": reserve, "data_ratio": data_ratio, "meta_ratio": meta_ratio,
            "mixed": False}


def base_facts():
    """Topology A, plain, with room and the renamed signer: the case the bridge accepts."""
    return {
        "mode": "live",
        "root": {"device": "/dev/vda3", "disk": "/dev/vda", "fstype": "btrfs", "uuid": ROOT_UUID,
                 "label": "rime-root", "free_bytes": 60 * GB, "size_bytes": 80 * GB, "luks": None,
                 "usage": usage(30 * GB, 18 * GB, 2 * GB, GB, 48 * GB, reserve=GB // 10)},
        "boot": {"kind": "top-level-dir", "uuid": ROOT_UUID, "fstype": "btrfs", "grub_cfg_present": True,
                 "sources_custom_cfg": True, "loader_link": "loader.1", "custom_cfg_present": False},
        "ostree": {"booted": True, "composefs_native": False, "deployments": ["ostree/deploy/default/deploy/x.0"],
                   "store_bytes": 9 * GB},
        "home": {"path": "/var/home", "bytes": 1 * GB, "entries": 20000, "small_bytes": 10**7},
        "var_fsroot": "/ostree/deploy/default/var",
        "migration": {"phase": None, "state_dir": None, "files": [], "composefs_leftover": False},
        "signer": {"renamed_known": True, "evidence": "/usr/bin/rime"},
        "keymap": "us",
        "esps": [esp("/dev/vda2", "/dev/vda", RIME_ESP, ["BOOT", "fedora"], {"fedora": fedora_grub()})],
        "nvram": {"available": True, "boot_current": "0001", "boot_order": ["0001"], "secure_boot": False,
                  "entries": {"0001": {"label": "Fedora", "path": "\\EFI\\fedora\\shimx64.efi",
                                       "partuuid": RIME_ESP}}},
    }


def topo_b():
    f = base_facts()
    f["esps"] = [esp("/dev/vdb1", "/dev/vdb", WIN_ESP, ["BOOT", "Microsoft", "fedora"], {"fedora": fedora_grub()})]
    f["nvram"]["entries"]["0001"]["partuuid"] = WIN_ESP
    return f


def topo_c():
    f = base_facts()
    f["esps"].append(esp("/dev/vdb1", "/dev/vdb", WIN_ESP, ["Boot", "Microsoft"]))
    f["esps"].append(esp("/dev/vdb5", "/dev/vdb", INST_ESP, ["BOOT", "rimeinst"], {"rimeinst": fedora_grub()}))
    f["nvram"]["entries"]["0001"] = {"label": "Rime OS", "path": "\\EFI\\rimeinst\\shimx64.efi", "partuuid": INST_ESP}
    return f


def topo_d():
    f = base_facts()
    f["esps"].append(esp("/dev/nvme0n1p1", "/dev/nvme0n1", WIN_ESP, ["Boot", "Microsoft", "APEX"],
                         {"apex": fedora_grub()}))
    f["nvram"]["boot_current"] = "0000"
    f["nvram"]["boot_order"] = ["0000", "0001"]
    f["nvram"]["entries"]["0000"] = {"label": "APEX-OS Primary", "path": "\\EFI\\APEX\\SHIMX64.EFI",
                                     "partuuid": WIN_ESP}
    return f


def topo_e_katana():
    """katana: D's loader, phase=failed, the trial's files on Rime's own ESP and Boot0006 outside BootOrder."""
    f = topo_d()
    f["root"]["label"] = "apex-root"
    f["migration"] = {"phase": "failed", "state_dir": "/var/lib/rime/boot-migrate",
                      "files": ["bootnum", "failed-at", "phase"], "composefs_leftover": True}
    f["esps"][0]["dirs"] = ["BOOT", "Linux", "fedora", "systemd"]
    f["esps"][0]["top"] = ["EFI", "loader"]
    f["nvram"]["boot_order"] = ["0000", "0001", "0002", "0003", "0004", "0005"]
    f["nvram"]["entries"]["0006"] = {"label": "Rime OS", "path": "\\EFI\\systemd\\systemd-bootx64.efi",
                                     "partuuid": RIME_ESP}
    return f


def topo_f():
    f = base_facts()
    f["ostree"]["composefs_native"] = True
    f["migration"] = {"phase": "confirmed", "state_dir": "/var/lib/rime/boot-migrate", "files": ["phase"],
                      "composefs_leftover": True}
    f["esps"][0]["dirs"] = ["BOOT", "systemd", "Linux"]
    f["nvram"]["entries"]["0001"] = {"label": "Rime OS", "path": "\\EFI\\systemd\\systemd-bootx64.efi",
                                     "partuuid": RIME_ESP}
    return f


def names(refusals):
    return [n for n, _ in refusals]


class Classify(unittest.TestCase):
    def test_a_accepted(self):
        c, r, notes = B.evaluate(base_facts())
        self.assertEqual(c["topology"], "A")
        self.assertEqual(r, [])
        self.assertTrue(any("custom.cfg" in n for n in notes), notes)

    def test_b_refused_by_name(self):
        c, r, _ = B.evaluate(topo_b())
        self.assertEqual(c["topology"], "B")
        self.assertIn("topology-B", names(r))

    def test_c_refused_by_name(self):
        c, r, _ = B.evaluate(topo_c())
        self.assertEqual(c["topology"], "C")
        self.assertIn("topology-C", names(r))

    def test_d_refused_by_name(self):
        c, r, _ = B.evaluate(topo_d())
        self.assertEqual(c["topology"], "D")
        self.assertIn("topology-D", names(r))

    def test_e_katana_accepted_with_repair_pending(self):
        c, r, notes = B.evaluate(topo_e_katana())
        self.assertEqual((c["topology"], c["base"]), ("E", "D"))
        self.assertEqual(r, [])
        self.assertTrue(any("repair pending" in n and "Boot0006" in n for n in notes), notes)

    def test_f_refused_by_name(self):
        c, r, _ = B.evaluate(topo_f())
        self.assertEqual(c["topology"], "F")
        self.assertIn("topology-F", names(r))

    def test_unknown_loader_refused(self):
        f = base_facts()
        f["nvram"]["entries"]["0001"]["path"] = "\\EFI\\ubuntu\\shimx64.efi"
        c, r, _ = B.evaluate(f)
        self.assertEqual(c["topology"], "unknown")
        self.assertIn("topology-unknown", names(r))

    def test_offline_classification_without_nvram(self):
        for fx, want in ((base_facts, "A"), (topo_b, "B"), (topo_c, "C"), (topo_d, "D")):
            f = fx()
            f["mode"] = "disk"
            f["nvram"] = {"available": False}
            self.assertEqual(B.classify(f)["topology"], want, fx.__name__)

    def test_e_needs_failed_phase_or_leftovers(self):
        f = topo_e_katana()
        f["migration"] = {"phase": None, "state_dir": None, "files": [], "composefs_leftover": False}
        self.assertEqual(B.classify(f)["topology"], "D")

    def test_e_base_a(self):
        f = base_facts()
        f["migration"]["phase"] = "failed"
        f["migration"]["state_dir"] = "/var/lib/apex/boot-migrate"
        c, r, _ = B.evaluate(f)
        self.assertEqual((c["topology"], c["base"]), ("E", "A"))
        self.assertEqual(r, [])


class Refusals(unittest.TestCase):
    def test_renamed_signer_x3(self):
        f = base_facts()
        f["signer"] = {"renamed_known": False, "evidence": "/usr/bin/rime"}
        _, r, _ = B.evaluate(f)
        self.assertEqual(names(r), ["renamed-signer"])

    def test_free_space_h9(self):
        # data: floor + old store against unused data chunks + unallocated / data ratio; home is not charged
        f = base_facts()
        u = f["root"]["usage"]
        need = B.FLOOR_BYTES + f["ostree"]["store_bytes"]
        u["unallocated"] = 10 * GB
        u["data_used"] = u["data_size"] - (need - u["unallocated"]) + 1      # one byte short
        _, r, _ = B.evaluate(f)
        self.assertEqual(names(r), ["free-space"])
        self.assertIn("floor 19.0", r[0][1])
        u["data_used"] -= 1
        self.assertEqual(B.evaluate(f)[1], [])

    def test_space_unmeasured(self):
        f = base_facts()
        f["root"]["usage"] = None
        self.assertEqual(names(B.evaluate(f)[1]), ["space-unmeasured"])
        f["root"]["fstype"] = "ext4"     # not btrfs: refused for that, no H9 figure at all
        self.assertEqual(names(B.evaluate(f)[1]), ["root-not-btrfs"])

    def test_ext4_root(self):
        f = base_facts()
        f["root"]["fstype"] = "ext4"
        self.assertIn("root-not-btrfs", names(B.evaluate(f)[1]))

    def test_boot_partition_kinds(self):
        # installer LUKS-A: /boot on its own ext4 is bridged (test_bridge_d2.py); anything else is refused by name.
        f = base_facts()
        f["boot"].update(kind="partition", fstype="vfat")
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-partition-fstype"])
        f["boot"]["kind"] = "unknown"
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-unknown"])

    def test_luks_keymap(self):
        f = base_facts()
        f["root"]["luks"] = {"device": "/dev/vda3", "uuid": "x", "mapper": "/dev/mapper/luks-x"}
        f["keymap"] = "uk"
        self.assertIn("luks-keymap", names(B.evaluate(f)[1]))
        f["keymap"] = "us"
        self.assertEqual(B.evaluate(f)[1], [])

    def test_chain_bootuuid_mismatch(self):
        f = base_facts()
        f["esps"][0]["grub"]["fedora"]["boot_uuid"] = "deadbeef-0000-4000-8000-000000000000"
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_chain_custom_cfg_not_sourced(self):
        f = base_facts()
        f["boot"]["sources_custom_cfg"] = False
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_chain_through_shim_fallback(self):
        # A fresh install's first boots: the firmware started the removable path (\EFI\BOOT\BOOTX64.EFI = shim +
        # fbx64.efi, measured in the bridge OVMF lab: BootCurrent was the disk's auto-created entry).
        f = base_facts()
        f["nvram"]["boot_current"] = "0002"
        f["nvram"]["entries"]["0002"] = {"label": "UEFI Misc Device", "path": None, "partuuid": None}
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])   # no fallback facts: not proven
        f["esps"][0]["fallback_csv"] = True
        c, refusals, notes = B.evaluate(f)
        self.assertEqual((c["topology"], refusals), ("A", []))
        self.assertTrue(any("\\EFI\\fedora\\grub.cfg" in n for n in notes), notes)

    def test_chain_unknown_loader_live(self):
        f = base_facts()
        f["nvram"] = {"available": False}
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_katana_apex_chain_must_read_root(self):
        f = topo_e_katana()
        del f["esps"][1]["grub"]["apex"]
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_backup_rules(self):
        f = base_facts()
        same = {"path": "/var/backup", "exists": True, "disk": "/dev/vda", "free_bytes": 100 * GB}
        self.assertEqual(names(B.evaluate(f, same)[1]), ["backup-same-disk"])
        small = {"path": "/mnt/usb", "exists": True, "disk": "/dev/sdb", "free_bytes": GB}
        self.assertEqual(names(B.evaluate(f, small)[1]), ["backup-too-small"])
        ok = {"path": "/mnt/usb", "exists": True, "disk": "/dev/sdb", "free_bytes": 100 * GB}
        self.assertEqual(B.evaluate(f, ok)[1], [])
        self.assertEqual(names(B.evaluate(f, {"path": "/nope", "exists": False})[1]), ["backup-missing"])

    def test_backup_is_optional(self):
        # A move needs no space beyond the disk (Andre, 2026-11-10): with no --backup the restore point is @pre-nixos.
        f = base_facts()
        f["home"]["in_top_level"] = True
        c, r, n = B.evaluate(f)
        self.assertEqual(r, [])
        text = B.report(f, c, r, n, True)
        self.assertIn("backup: none (optional", text)
        self.assertIn("@pre-nixos", text)
        self.assertIn("does not survive the disk failing", text)

    def test_home_subvolume_needs_backup(self):
        # A snapshot of the top level does not hold a nested subvolume: with no off-disk copy that home would have
        # no restore point, so start is refused by name; --backup lifts it; unknown (offline facts) does not refuse.
        f = base_facts()
        f["home"]["in_top_level"] = False
        self.assertEqual(names(B.evaluate(f)[1]), ["home-subvolume-needs-backup"])
        ok = {"path": "/mnt/usb", "exists": True, "disk": "/dev/sdb", "free_bytes": 100 * GB}
        self.assertEqual(B.evaluate(f, ok)[1], [])
        f["home"].pop("in_top_level")
        self.assertEqual(B.evaluate(f)[1], [])

    def test_already_started(self):
        f = base_facts()
        f["existing_subvols"] = ["@nixos"]
        self.assertEqual(names(B.evaluate(f)[1]), ["already-started"])

    def test_report_names_every_refusal(self):
        f = topo_b()
        f["signer"]["renamed_known"] = False
        c, r, n = B.evaluate(f)
        text = B.report(f, c, r, n, True)
        self.assertIn("REFUSED [topology-B]", text)
        self.assertIn("REFUSED [renamed-signer]", text)
        self.assertIn("verdict: REFUSED [topology-B] [renamed-signer]", text)


# A real machine's `btrfs filesystem usage -b` of its root, verbatim (the L16 rehearsal, 2026-11-04: 1.6 TB btrfs,
# metadata DUP; its last line is a separate qgroup command's error, left in: the parser must not care). It carries no
# identifier beyond a device name.
L16_USAGE = """\
Overall:
    Device size:		     1617991565312
    Device allocated:		     1551594684416
    Device unallocated:		       66396880896
    Device missing:		                 0
    Device slack:		                 0
    Used:			     1236276977664
    Free (estimated):		      376761475072	(min: 343563034624)
    Free (statfs, df):		      376760426496
    Data ratio:			              1.00
    Metadata ratio:		              2.00
    Global reserve:		         536870912	(used: 0)
    Multiple profiles:		                no

Data,single: Size:1515070685184, Used:1204706091008 (79.51%)
   /dev/nvme0n1p5	1515070685184

Metadata,DUP: Size:18253611008, Used:15785246720 (86.48%)
   /dev/nvme0n1p5	36507222016

System,DUP: Size:8388608, Used:196608 (2.34%)
   /dev/nvme0n1p5	  16777216

Unallocated:
   /dev/nvme0n1p5	66396880896
ERROR: can't list qgroups: quotas not enabled
"""
# The rest of the L16's facts from the same rehearsal's `check --explain` (home 1294.2 GB by du, old store 20.9 GB).
# Its home's names and small-file bytes were NOT measured: the figures below are invented and kept far below the
# share estimate, so the result rests on the measured figures alone.
L16_HOME = 1294.19 * GB
L16_STORE = 20.9 * GB


def space(**kw):
    """base_facts() with the root's usage and home/store replaced: only H9 differs."""
    f = base_facts()
    f["root"]["usage"] = usage(**kw.pop("u"))
    f["home"].update(kw.pop("home", {}))
    if "store" in kw:
        f["ostree"]["store_bytes"] = kw.pop("store")
    assert not kw, kw
    return f


class SpaceH9(unittest.TestCase):
    """H9 models what start consumes: DATA (floor + old store; home is a reflink copy) and METADATA (home's, which the
    copy duplicates, x the metadata ratio, from metadata free - the global reserve + what unallocated data leaves)."""

    def test_parse_l16(self):
        u = B.parse_btrfs_usage(L16_USAGE)
        self.assertEqual(u, {"data_size": 1515070685184, "data_used": 1204706091008, "meta_size": 18253611008,
                             "meta_used": 15785246720, "unallocated": 66396880896, "global_reserve": 536870912,
                             "data_ratio": 1.0, "meta_ratio": 2.0, "mixed": False})
        self.assertIsNone(B.parse_btrfs_usage(""))
        self.assertIsNone(B.parse_btrfs_usage(L16_USAGE.replace("Metadata,DUP", "Metadata-gone")))

    def test_l16_passes(self):
        # The real L16 (it refused with "376.8 GB free, 1334.1 GB needed" when home counted as data).
        f = base_facts()
        f["root"]["usage"] = B.parse_btrfs_usage(L16_USAGE)
        f["home"].update(bytes=int(L16_HOME), entries=3_000_000, small_bytes=2 * GB)
        f["ostree"]["store_bytes"] = int(L16_STORE)
        _, r, _ = B.evaluate(f)
        self.assertEqual(r, [])
        p = B.space_plan(f)
        self.assertAlmostEqual(p["data_avail"] / GB, 376.76, places=2)      # = statfs free
        self.assertAlmostEqual(p["data_need"] / GB, 39.9, places=2)
        self.assertEqual(p["data_draw_raw"], 0)
        # home's du (uncompressed) exceeds the on-disk data used: its share is capped at ALL metadata used
        self.assertEqual(p["share"], 15785246720)
        self.assertEqual(p["home_meta"], p["share"])
        self.assertAlmostEqual(p["meta_need_raw"] / GB, 15.785246720 * 1.25 * 2, places=3)    # 39.46
        self.assertAlmostEqual(p["meta_avail_raw"] / GB, (18.253611008 - 15.785246720 - 0.536870912) * 2 + 66.396880896,
                               places=3)                                                     # 70.26
        text = B.report(f, B.classify(f), r, [], True)
        self.assertIn("space: data 376.8 GB free, 39.9 GB needed: floor 19.0 + old store 20.9", text)
        self.assertIn("space: metadata 70.26 GB on the device, 39.46 GB needed", text)

    def test_data_below_floor(self):
        f = space(u=dict(data_size=100 * GB, data_used=95 * GB, meta_size=2 * GB, meta_used=GB // 2, unallocated=10 * GB),
                  home={"bytes": 50 * GB}, store=GB)
        _, r, _ = B.evaluate(f)
        self.assertEqual(names(r), ["free-space"])     # 15 GB free, 20 GB needed; metadata fits
        self.assertIn("data: 15.0 GB free, 20.0 GB needed: floor 19.0 + old store 1.0", r[0][1])

    def tight(self, unallocated):
        # metadata 10 GB, 8 GB used, reserve 0.5, DUP: (10 - 8 - 0.5) x 2 = 3 GB raw free in chunks. Home is 300 GB of
        # 300 GB used: its share is all 8 GB, need 8 x 1.25 x 2 = 20 GB raw. Data has 200 GB free in its chunks.
        return space(u=dict(data_size=500 * GB, data_used=300 * GB, meta_size=10 * GB, meta_used=8 * GB,
                            unallocated=unallocated, reserve=GB // 2),
                     home={"bytes": 300 * GB, "entries": 1000, "small_bytes": 0}, store=9 * GB)

    def test_metadata_space_refused(self):
        # 3 + 16.5 = 19.5 GB raw < 20: refused by name, with its numbers. Counted once (no DUP), or with the reserve
        # free, the same disk would pass (18 >= 10; 20.5 >= 20).
        _, r, _ = B.evaluate(self.tight(int(16.5 * GB)))
        self.assertEqual(names(r), ["metadata-space"])
        self.assertIn("metadata: 19.50 GB on the device, 20.00 GB needed", r[0][1])
        self.assertIn("global reserve 0.50", r[0][1])

    def test_metadata_space_boundary(self):
        self.assertEqual(B.evaluate(self.tight(17 * GB))[1], [])                         # 3 + 17 = 20: fits
        self.assertEqual(names(B.evaluate(self.tight(17 * GB - 1))[1]), ["metadata-space"])

    def test_small_files_home(self):
        # 6 M names and 3 GB of inline files in an 8 GB home on a disk of 800 GB of games: the share (0.04 GB) is far
        # too small; per name 6 M x 1 KiB + 3 GB = 9.14 GB, x 1.25 x 2 = 22.9 GB raw > 1 + 15.
        f = space(u=dict(data_size=900 * GB, data_used=800 * GB, meta_size=5 * GB, meta_used=4 * GB,
                         unallocated=15 * GB, reserve=GB // 2),
                  home={"bytes": 8 * GB, "entries": 6_000_000, "small_bytes": 3 * GB}, store=9 * GB)
        _, r, _ = B.evaluate(f)
        self.assertEqual(names(r), ["metadata-space"])
        p = B.space_plan(f)
        self.assertEqual(p["home_meta"], p["per_name"])
        self.assertLess(p["share"], GB // 10)

    def test_unallocated_shared(self):
        # Data has 5 GB in its chunks and needs 25: it takes 20 of the 30 GB unallocated. Metadata (no free in its
        # chunks after the reserve) needs 5.5 x 1.25 x 2 = 13.75 GB raw from the 10 left: refused. Giving metadata all
        # 30 would pass it.
        f = space(u=dict(data_size=100 * GB, data_used=95 * GB, meta_size=6 * GB, meta_used=int(5.5 * GB),
                         unallocated=30 * GB, reserve=GB // 2),
                  home={"bytes": 95 * GB, "entries": 0, "small_bytes": 0}, store=6 * GB)
        p = B.space_plan(f)
        self.assertEqual(p["data_draw_raw"], 20 * GB)
        self.assertTrue(p["data_ok"])
        self.assertEqual(names(B.evaluate(f)[1]), ["metadata-space"])

    def test_both_refused(self):
        f = space(u=dict(data_size=100 * GB, data_used=99 * GB, meta_size=2 * GB, meta_used=int(1.9 * GB),
                         unallocated=GB, reserve=GB // 10),
                  home={"bytes": 99 * GB}, store=GB)
        self.assertEqual(names(B.evaluate(f)[1]), ["free-space", "metadata-space"])

    def test_mixed_block_groups(self):
        text = ("Overall:\n    Device unallocated:\t\t 100000000\n    Data ratio:\t\t 2.00\n"
                "    Metadata ratio:\t\t 2.00\n    Global reserve:\t\t 5000000\t(used: 0)\n\n"
                "Data+Metadata,DUP: Size:400000000, Used:100000000 (25.00%)\n"
                "System,DUP: Size:8388608, Used:16384 (0.20%)\n")
        u = B.parse_btrfs_usage(text)
        self.assertTrue(u["mixed"])
        f = base_facts()
        f["root"]["usage"] = u
        f["home"].update(bytes=50 * 10**6, entries=1000, small_bytes=0)
        f["ostree"]["store_bytes"] = 0
        old = B.FLOOR_BYTES
        try:
            B.FLOOR_BYTES = 200 * 10**6
            p = B.space_plan(f)     # pool 295 MB free + 100 MB raw / 2: 345 MB; data 200; metadata 1.28 MB
            self.assertEqual(p["data_avail"], 345 * 10**6)
            self.assertTrue(p["data_ok"] and p["meta_ok"])
            f["home"]["entries"] = 200_000   # 204.8 MB x 1.25 > the 145 MB data leaves
            self.assertEqual(names(B.evaluate(f)[1]), ["metadata-space"])
        finally:
            B.FLOOR_BYTES = old

    def test_explain_says_what_divergence_costs(self):
        f = base_facts()
        c, r, n = B.evaluate(f)
        text = B.report(f, c, r, n, True)
        self.assertIn("divergence:", text)
        self.assertIn("each file changed or added on NixOS takes new space", text)
        self.assertIn("@pre-nixos, ostree/ and the old var/home", text)
        self.assertIn("rime migrate finish", text)
        self.assertIn("rime migrate finish --dry-run", text)
        self.assertNotIn("planned", text)   # finish exists (on NixOS): the text says how to get the space back
        self.assertNotIn("divergence:", B.report(f, c, r, n, False))

    def test_du_tree(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "a/b"))
            with open(os.path.join(d, "a/small"), "wb") as fh:
                fh.write(b"x" * 100)
            with open(os.path.join(d, "a/b/big"), "wb") as fh:
                fh.write(b"y" * 10000)
            os.link(os.path.join(d, "a/small"), os.path.join(d, "a/b/small2"))
            t = B.du_tree(d)
            self.assertEqual(t["entries"], 5)           # a, a/b, small, big, small2 (every name)
            self.assertEqual(t["small_bytes"], 100)     # the hard link once; big is not under a sector
            self.assertEqual(B.du_bytes(d), t["bytes"])


class TrialLeftovers(unittest.TestCase):
    def test_katana_leftovers(self):
        lo = B.trial_leftovers(topo_e_katana())
        self.assertEqual(lo["nvram"], ["0006"])
        self.assertEqual(sorted(x["path"] for x in lo["files"]), ["EFI/Linux", "EFI/systemd", "loader"])
        self.assertTrue(all(x["esp"] == "/dev/vda2" for x in lo["files"]))

    def test_windows_esp_never_listed(self):
        f = topo_e_katana()
        f["esps"][1]["dirs"].append("systemd")
        self.assertTrue(all(x["esp"] == "/dev/vda2" for x in B.trial_leftovers(f)["files"]))


class WaitUnits(unittest.TestCase):
    """start waits for a running old-side oneshot, never for one that finished (RemainAfterExit=yes reads "active")."""

    def fake_run(self, states):
        calls = []

        def run(cmd, check=True):
            calls.append(cmd)
            return subprocess.CompletedProcess(cmd, 0, f"ActiveState={states.get(cmd[-1], 'inactive')}\n", "")
        return run, calls

    def with_run(self, states, **kw):
        run, calls = self.fake_run(states)
        old = B.run
        B.run = run
        try:
            B.wait_units(**kw)
        finally:
            B.run = old
        return calls

    def test_finished_oneshot_is_not_waited_for(self):
        calls = self.with_run({"rime-migrate-from-apex.service": "active"}, timeout=0, poll=0)
        self.assertTrue(all(c[:2] == ["systemctl", "show"] for c in calls))
        self.assertEqual(len(calls), len(B.WAIT_UNITS))

    def test_running_oneshot_times_out_by_name(self):
        with self.assertRaises(B.Refused) as cm:
            self.with_run({"rime-store-compact.service": "activating"}, timeout=0, poll=0)
        self.assertEqual(cm.exception.name, "old-side-busy")


class Chroot(unittest.TestCase):
    def test_host_chroot_by_absolute_path(self):
        seen = []

        def fake(argv, **kw):
            seen.append((argv, kw.get("env", {}).get("PATH", "")))
            return subprocess.CompletedProcess(argv, 0, "", "")
        old = B.subprocess.run
        B.subprocess.run = fake
        try:
            B.chroot("/t", "/nix/store/x/bin/true")
        finally:
            B.subprocess.run = old
        argv, path = seen[0]
        self.assertTrue(os.path.isabs(argv[0]) and argv[0].endswith("/chroot"), argv)
        self.assertNotIn(os.path.dirname(argv[0]), path.split(":"))


class FirstActivation(unittest.TestCase):
    """The chroot's activation must not leave the running (bootc) kernel's modprobe or firmware path in a store."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.root, self.t = os.path.join(self.d, "host"), os.path.join(self.d, "t")
        for base, val in ((self.root, ("/usr/sbin/modprobe\n", "\n")), (self.t, ("x\n", "x\n"))):
            for k, v in zip(B.HOST_KNOBS, val):
                os.makedirs(os.path.dirname(os.path.join(base, k)), exist_ok=True)
                with open(os.path.join(base, k), "w") as f:
                    f.write(v)
        os.makedirs(os.path.join(self.t, "run"))
        self.mounts, self.umounts = [], []
        self.old = (B.mount, B.run, B.chroot)
        B.mount = lambda *a: self.mounts.append(a)
        B.run = lambda cmd, **kw: self.umounts.append(cmd)

    def tearDown(self):
        B.mount, B.run, B.chroot = self.old
        shutil.rmtree(self.d)

    def activate_writes(self, base):
        def fake(t, *cmd, **kw):
            for k, v in zip(B.HOST_KNOBS, ("/nix/store/k-kmod-31/bin/modprobe", "/nix/store/f-firmware/lib/firmware")):
                with open(os.path.join(base, k), "w") as f:
                    f.write(v)
            return subprocess.CompletedProcess(cmd, 0, "", "")
        B.chroot = fake

    def host(self):
        return [open(os.path.join(self.root, k)).read() for k in B.HOST_KNOBS]

    def test_each_knob_is_shielded_and_the_host_keeps_its_values(self):
        # With the shields bound, the activation's writes land in the target's (scratch) files.
        self.activate_writes(self.t)
        r = B.first_activation(self.t, "/nix/store/s-system", root=self.root)
        self.assertEqual(r.returncode, 0)
        self.assertEqual(sorted(m[2] for m in self.mounts), sorted(os.path.join(self.t, k) for k in B.HOST_KNOBS))
        self.assertTrue(all(m[0] == "--bind" for m in self.mounts), self.mounts)
        self.assertEqual(sorted(u[1] for u in self.umounts), sorted(m[2] for m in self.mounts))
        self.assertEqual(self.host(), ["/usr/sbin/modprobe\n", "\n"])

    def test_a_write_that_reaches_the_host_is_put_back_and_fails_start(self):
        self.activate_writes(self.root)
        with self.assertRaises(B.Failed) as cm:
            B.first_activation(self.t, "/nix/store/s-system", root=self.root)
        self.assertIn("/proc/sys/kernel/modprobe", str(cm.exception))
        self.assertEqual(self.host(), ["/usr/sbin/modprobe\n", "\n"])


class Containment(unittest.TestCase):
    """H8: the bridge may add its own GRUB block and files; any other change to ostree's /boot or an ESP fails."""

    def snap(self):
        return {"boot": {"loader.1/entries/ostree-1.conf": "a", "grub2/grub.cfg": "b"},
                "custom_outside_block": B.custom_outside_block("if true; then echo user; fi\n"),
                "deployments": ["abc.0"], "esp:/dev/vda2": {"EFI/fedora/grub.cfg": "c"}}

    def test_unchanged(self):
        self.assertEqual(B.containment_diff(self.snap(), self.snap()), {})

    def test_changed_file_content_is_named(self):
        a = self.snap()
        a["boot"]["grub2/grub.cfg"] = "changed"
        self.assertEqual(B.containment_diff(self.snap(), a), {"boot": ["grub2/grub.cfg"]})

    def test_esp_file_added_is_named(self):
        a = self.snap()
        a["esp:/dev/vda2"]["EFI/systemd/systemd-bootx64.efi"] = "d"
        self.assertEqual(B.containment_diff(self.snap(), a), {"esp:/dev/vda2": ["EFI/systemd/systemd-bootx64.efi"]})

    def test_rime_block_is_ours_the_rest_is_not(self):
        user = "if true; then echo user; fi\n"
        block = "### BEGIN rime-boot ###\nsource $prefix/rime-nixos.cfg\n### END rime-boot ###\n"
        self.assertEqual(B.custom_outside_block(user + block), B.custom_outside_block(user))
        self.assertNotEqual(B.custom_outside_block(user + "set timeout=0\n" + block), B.custom_outside_block(user))


class LoadOption(unittest.TestCase):
    def option(self, desc, partuuid, path, attrs=1):
        hd = struct.pack("<IQQ", 1, 2048, 1048576) + uuid.UUID(partuuid).bytes_le + bytes([2, 2])
        fp = bytes([4, 1]) + struct.pack("<H", 4 + len(hd)) + hd
        p = (path + "\0").encode("utf-16-le")
        fp += bytes([4, 4]) + struct.pack("<H", 4 + len(p)) + p
        fp += bytes([0x7F, 0xFF, 4, 0])
        return struct.pack("<IH", attrs, len(fp)) + (desc + "\0").encode("utf-16-le") + fp

    def test_parse(self):
        o = B.parse_load_option(self.option("APEX-OS Primary", WIN_ESP, "\\EFI\\APEX\\SHIMX64.EFI"))
        self.assertEqual(o, {"label": "APEX-OS Primary", "path": "\\EFI\\APEX\\SHIMX64.EFI", "partuuid": WIN_ESP,
                             "active": True})

    def test_nvram_dir(self):
        import tempfile
        d = tempfile.mkdtemp()
        g = B.EFI_GLOBAL
        with open(f"{d}/Boot0006-{g}", "wb") as f:
            f.write(b"\7\0\0\0" + self.option("Rime OS", RIME_ESP, "\\EFI\\systemd\\systemd-bootx64.efi"))
        with open(f"{d}/BootCurrent-{g}", "wb") as f:
            f.write(b"\6\0\0\0" + struct.pack("<H", 0))
        with open(f"{d}/BootOrder-{g}", "wb") as f:
            f.write(b"\7\0\0\0" + struct.pack("<HH", 0, 1))
        old = B.EFIVARS
        B.EFIVARS = d
        try:
            nv = B.nvram_facts()
        finally:
            B.EFIVARS = old
        self.assertEqual(nv["boot_current"], "0000")
        self.assertEqual(nv["boot_order"], ["0000", "0001"])
        self.assertEqual(nv["entries"]["0006"]["path"], "\\EFI\\systemd\\systemd-bootx64.efi")


class Katana(unittest.TestCase):
    """The real katana boot-migrate state (root-only snapshot), read at runtime when RIME_KATANA_STATE is set."""

    def test_katana_state_is_e(self):
        st = os.environ.get("RIME_KATANA_STATE")
        if not st or not os.path.isdir(os.path.join(st, "var/lib/rime/boot-migrate")):
            self.skipTest("RIME_KATANA_STATE not set or unreadable: katana snapshot absent")
        m = B.migration_facts(os.path.join(st, "var"), None)
        self.assertEqual(m["phase"], "failed")
        f = topo_e_katana()
        f["migration"] = m
        self.assertEqual(B.classify(f)["topology"], "E")


class UndoBoot(unittest.TestCase):
    """H8 rollback, boot side: custom.cfg as it was; the hook's menu and kernels removed only when start made them."""

    def hook_writes(self, b):
        os.makedirs(os.path.join(b, "grub2"), exist_ok=True)
        os.makedirs(os.path.join(b, "rime/nixos"), exist_ok=True)
        with open(os.path.join(b, "rime/nixos/k.efi"), "w") as f:
            f.write("kernel")
        with open(os.path.join(b, "grub2/rime-nixos.cfg"), "w") as f:
            f.write("menuentry 'Rime' {}\n")
        with open(os.path.join(b, "grub2/custom.cfg"), "a") as f:
            f.write("### BEGIN rime-boot ###\nsource rime-nixos.cfg\n### END rime-boot ###\n")

    def test_made_by_start_is_removed(self):
        with tempfile.TemporaryDirectory() as b:
            os.makedirs(os.path.join(b, "grub2"))
            had = B.boot_had(b)
            self.hook_writes(b)
            B.undo_boot(b, None, had)
            self.assertEqual(sorted(os.listdir(b)), ["grub2"])
            self.assertEqual(os.listdir(os.path.join(b, "grub2")), [])

    def test_what_was_there_stays(self):
        with tempfile.TemporaryDirectory() as b:
            self.hook_writes(b)
            with open(os.path.join(b, "grub2/custom.cfg"), "w") as f:
                f.write("user entry\n")
            had = B.boot_had(b)
            self.hook_writes(b)
            B.undo_boot(b, "user entry\n", had)
            self.assertTrue(os.path.isfile(os.path.join(b, "rime/nixos/k.efi")))
            self.assertTrue(os.path.isfile(os.path.join(b, "grub2/rime-nixos.cfg")))
            with open(os.path.join(b, "grub2/custom.cfg")) as f:
                self.assertEqual(f.read(), "user entry\n")


class Cli(unittest.TestCase):
    def test_needs_root_or_refuses_usage(self):
        for args in (["--backup", "/x", "--payload", "/y"], ["--payload", "/y"]):   # --backup is optional
            r = subprocess.run([PROG, "start", *args], capture_output=True, text=True)
            if os.geteuid() != 0:
                self.assertEqual(r.returncode, 2)
                self.assertIn("needs root", r.stderr)

    def test_no_forbidden_tools(self):
        with open(PROG) as f:
            src = f.read()
        body = src.split('"""', 2)[2]  # everything after the module docstring, which names them to forbid them
        for word in ("bootctl", "efibootmgr", "grub2-install", "grub-install", "NIXOS_LUSTRATE", "grub2-reboot"):
            self.assertNotIn(word, body, word)
        self.assertNotIn("btrfs\", \"property\", \"set", src)


if __name__ == "__main__":
    unittest.main()
