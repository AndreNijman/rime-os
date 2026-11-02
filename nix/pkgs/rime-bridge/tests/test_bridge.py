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
ROOT_UUID = "087699bd-0000-4000-8000-000000000001"
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


def base_facts():
    """Topology A, plain, with room and the renamed signer: the case the bridge accepts."""
    return {
        "mode": "live",
        "root": {"device": "/dev/vda3", "disk": "/dev/vda", "fstype": "btrfs", "uuid": ROOT_UUID,
                 "label": "rime-root", "free_bytes": 60 * GB, "size_bytes": 80 * GB, "luks": None},
        "boot": {"kind": "top-level-dir", "uuid": ROOT_UUID, "fstype": "btrfs", "grub_cfg_present": True,
                 "sources_custom_cfg": True, "loader_link": "loader.1", "custom_cfg_present": False},
        "ostree": {"booted": True, "composefs_native": False, "deployments": ["ostree/deploy/default/deploy/x.0"],
                   "store_bytes": 9 * GB},
        "home": {"path": "/var/home", "bytes": 1 * GB},
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
        f = base_facts()
        need = B.FLOOR_BYTES + f["home"]["bytes"] + f["ostree"]["store_bytes"]
        f["root"]["free_bytes"] = need - 1
        _, r, _ = B.evaluate(f)
        self.assertEqual(names(r), ["free-space"])
        f["root"]["free_bytes"] = need
        self.assertEqual(B.evaluate(f)[1], [])

    def test_free_space_counts_home_and_store(self):
        f = base_facts()
        f["root"]["free_bytes"] = B.FLOOR_BYTES + 1
        _, r, _ = B.evaluate(f)
        self.assertIn("free-space", names(r))

    def test_ext4_root(self):
        f = base_facts()
        f["root"]["fstype"] = "ext4"
        self.assertIn("root-not-btrfs", names(B.evaluate(f)[1]))

    def test_boot_partition_refused(self):
        # installer LUKS-A: /boot on its own ext4. rime.rootfs cannot mount it yet, so it is refused by name.
        f = base_facts()
        f["boot"]["kind"] = "partition"
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-partition"])
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
        r = subprocess.run([PROG, "start", "--backup", "/x", "--payload", "/y"], capture_output=True, text=True)
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
