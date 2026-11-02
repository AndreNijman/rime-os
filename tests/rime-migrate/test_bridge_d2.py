"""rime-migrate's D2 completion tests (bridge2): the installer's LUKS layout with /boot on its own partition, the
host.toml [boot] table start writes, the EFI grub.cfg shapes check reads, the signed payload manifest, the carry
preview check shows before start, key-export, and repair-e's archive of the trial's /composefs and /state.

Same loader as test_bridge.py: RIME_MIGRATE names the program under test (a mutant, in nix/tests/bridge.nix).
"""

import base64
import copy
import errno
import hashlib
import importlib.machinery
import importlib.util
import json
import pathlib
import os
import shutil
import sys
import tarfile
import tempfile
import tomllib
import unittest

PROG = os.environ.get("RIME_MIGRATE", os.path.join(os.path.dirname(__file__), "..", "rime-bridge.py"))


def load():
    loader = importlib.machinery.SourceFileLoader("rime_bridge_d2", PROG)
    spec = importlib.util.spec_from_loader("rime_bridge_d2", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


B = load()
GB = 10**9
ROOT_UUID = "3a7d2c10-7b4e-4a51-8d1c-a1a1a1a1a1a1"
LUKS_UUID = "5c0ffee0-1111-4222-8333-444455556666"
BOOT_UUID = "b007b007-0000-4000-8000-000000000b00"
RIME_ESP = "11111111-1111-4111-8111-111111111111"
WIN_ESP = "22222222-2222-4222-8222-222222222222"
# bootupd's grub-static-efi.cfg as the published image ships it (/usr/lib/bootupd/grub2-static/), byte for byte.
BOOTUPD_STATIC = """if [ -e (md/md-boot) ]; then
  # The search command might pick a RAID component rather than the RAID,
  # since the /boot RAID currently uses superblock 1.0.  See the comment in
  # the main grub.cfg.
  set prefix=md/md-boot
else
  if [ -f ${config_directory}/bootuuid.cfg ]; then
    source ${config_directory}/bootuuid.cfg
  fi
  if [ -n "${BOOT_UUID}" ]; then
    search --fs-uuid "${BOOT_UUID}" --set prefix --no-floppy
  else
    search --label boot --set prefix --no-floppy
  fi
fi
if [ -d ($prefix)/grub2 ]; then
  set prefix=($prefix)/grub2
  configfile $prefix/grub.cfg
else
  set prefix=($prefix)/boot/grub2
  configfile $prefix/grub.cfg
fi
boot
"""
GRUB2_STUB = ("search --no-floppy --root-dev-only --fs-uuid --set=dev {uuid}\nset prefix=($dev)/grub2\n"
              "export $prefix\nconfigfile $prefix/grub.cfg\n")
# A shape Rime does not know (what katana's \EFI\APEX\grub.cfg might be: a full menu with its own entries).
UNKNOWN_SHAPE = ("set timeout=5\ninsmod part_gpt\ninsmod btrfs\nmenuentry 'APEX-OS' {\n  search --label apex-root "
                 "--set root\n  linux /boot/vmlinuz root=LABEL=apex-root\n  initrd /boot/initramfs.img\n}\n")


def esp(dev, disk, partuuid, dirs, grub=None):
    return {"device": dev, "disk": disk, "partuuid": partuuid, "uuid": "AAAA-0000", "size": 1 << 30,
            "dirs": dirs, "loader_entries": [], "grub": grub or {}, "mounted": True, "top": ["EFI"]}


def luks_a():
    """The installer's LUKS layout (installer/rime-install): p1 ESP, p2 ext4 /boot (label rime-boot), p3 LUKS2 with
    btrfs rime-root inside; bootuuid.cfg names the ext4 /boot."""
    return {
        "mode": "live",
        "root": {"device": f"/dev/mapper/luks-{LUKS_UUID}", "disk": "/dev/vda", "fstype": "btrfs", "uuid": ROOT_UUID,
                 "label": "rime-root", "free_bytes": 60 * GB, "size_bytes": 80 * GB,
                 "luks": {"device": "/dev/vda3", "uuid": LUKS_UUID, "mapper": f"/dev/mapper/luks-{LUKS_UUID}"}},
        "boot": {"kind": "partition", "uuid": BOOT_UUID, "fstype": "ext4", "device": "/dev/vda2",
                 "grub_cfg_present": True, "sources_custom_cfg": True, "loader_link": "loader.0",
                 "custom_cfg_present": False},
        "ostree": {"booted": True, "composefs_native": False, "deployments": ["ostree/deploy/default/deploy/x.0"],
                   "store_bytes": 9 * GB},
        "home": {"path": "/var/home", "bytes": 1 * GB},
        "var_fsroot": "/ostree/deploy/default/var",
        "migration": {"phase": None, "state_dir": None, "files": [], "composefs_leftover": False, "trial_top": []},
        "signer": {"renamed_known": True, "evidence": "/usr/bin/rime"},
        "keymap": "us",
        "esps": [esp("/dev/vda1", "/dev/vda", RIME_ESP, ["BOOT", "fedora"],
                     {"fedora": {"grub_cfg": BOOTUPD_STATIC, "boot_uuid": BOOT_UUID}})],
        "nvram": {"available": True, "boot_current": "0001", "boot_order": ["0001"], "secure_boot": True,
                  "entries": {"0001": {"label": "Fedora", "path": "\\EFI\\fedora\\shimx64.efi",
                                       "partuuid": RIME_ESP}}},
    }


def names(refusals):
    return [n for n, _ in refusals]


class LuksA(unittest.TestCase):
    def test_luks_a_is_bridged(self):
        c, refusals, notes = B.evaluate(luks_a())
        self.assertEqual((c["topology"], refusals), ("A", []))
        self.assertTrue(any(f"partition UUID={BOOT_UUID}" in n for n in notes), notes)

    def test_only_an_ext4_boot_partition(self):
        # the installer makes ext4; rime.rootfs ships ext4 support only, so anything else is refused by name
        f = luks_a()
        f["boot"]["fstype"] = "xfs"
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-partition-fstype"])

    def test_boot_partition_without_uuid_refused(self):
        f = luks_a()
        f["boot"]["uuid"] = ""
        self.assertIn("boot-partition-fstype", names(B.evaluate(f)[1]))

    def test_chain_must_reach_the_boot_partition(self):
        # bootuuid.cfg naming the btrfs root (the plain layout) does not reach the ext4 /boot: refused, by name.
        f = luks_a()
        f["esps"][0]["grub"]["fedora"]["boot_uuid"] = ROOT_UUID
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_boot_table(self):
        self.assertEqual(B.boot_table(luks_a()),
                         {"layout": "custom-cfg", "device": f"UUID={BOOT_UUID}", "fstype": "ext4"})
        plain = luks_a()
        plain["boot"]["kind"] = "top-level-dir"
        self.assertEqual(B.boot_table(plain), {"layout": "custom-cfg"})

    def test_explain_names_the_partition(self):
        f = luks_a()
        c, r, n = B.evaluate(f)
        text = B.report(f, c, r, n, True)
        self.assertIn("/boot: partition /dev/vda2 ext4", text)
        self.assertIn("in LUKS /dev/vda3", text)


class BootTable(unittest.TestCase):
    TABLE = {"layout": "custom-cfg", "device": f"UUID={BOOT_UUID}", "fstype": "ext4"}

    def test_merged_into_carrys_boot_table(self):
        # carry writes [boot] kernel_params (rd.luks.*) before [[luks]]; start's keys go INTO that table.
        carried = ('schema = 1\n\n[host]\nhostname = "lab"\n\n[boot]\nkernel_params = ["rd.luks.uuid=%s"]\n\n'
                   '[[luks]]\nname = "luks-%s"\ndevice = "UUID=%s"\n' % (LUKS_UUID, LUKS_UUID, LUKS_UUID))
        out = B.merge_boot_table(carried, self.TABLE)
        self.assertEqual(out.count("[boot]"), 1, out)
        t = tomllib.loads(out)
        self.assertEqual(t["boot"], dict(self.TABLE, kernel_params=[f"rd.luks.uuid={LUKS_UUID}"]))
        self.assertEqual(t["luks"][0]["name"], f"luks-{LUKS_UUID}")
        self.assertEqual(t["host"], {"hostname": "lab"})

    def test_replaces_not_duplicates(self):
        out = B.merge_boot_table('[boot]\nlayout = "bls"\ndevice = "UUID=old"\n', self.TABLE)
        self.assertEqual(tomllib.loads(out)["boot"], self.TABLE)

    def test_no_boot_table_yet(self):
        out = B.merge_boot_table('schema = 1\n[host]\nhostname = "x"', {"layout": "custom-cfg"})
        self.assertEqual(tomllib.loads(out), {"schema": 1, "host": {"hostname": "x"}, "boot": {"layout": "custom-cfg"}})
        self.assertEqual(tomllib.loads(B.merge_boot_table("", {"layout": "custom-cfg"})),
                         {"boot": {"layout": "custom-cfg"}})


class GrubShape(unittest.TestCase):
    def test_bootupd_static(self):
        shape, target, missing, cmds = B.grub_shape(BOOTUPD_STATIC)
        self.assertEqual((shape, target, missing["bootupd-static"]), ("bootupd-static", None, []))
        self.assertIn("configfile", cmds)

    def test_grub2_stub(self):
        shape, target, _, _ = B.grub_shape(GRUB2_STUB.format(uuid=BOOT_UUID.upper()))
        self.assertEqual((shape, target), ("grub2-stub", BOOT_UUID))

    def test_stub_reaching_boot_is_verified(self):
        f = luks_a()
        f["esps"][0]["grub"]["fedora"] = {"grub_cfg": GRUB2_STUB.format(uuid=BOOT_UUID), "boot_uuid": None}
        self.assertEqual(B.evaluate(f)[1], [])
        f["esps"][0]["grub"]["fedora"]["grub_cfg"] = GRUB2_STUB.format(uuid=ROOT_UUID)
        self.assertEqual(names(B.evaluate(f)[1]), ["boot-chain-unverified"])

    def test_unknown_shape_is_explained(self):
        # PLAN open question 7: katana's real \EFI\APEX\grub.cfg is not known until P3. Whatever it holds, check must
        # say what it read and why it cannot verify the chain, never guess.
        f = luks_a()
        f["esps"].append(esp("/dev/nvme0n1p1", "/dev/nvme0n1", WIN_ESP, ["Boot", "Microsoft", "APEX"],
                             {"apex": {"grub_cfg": UNKNOWN_SHAPE, "boot_uuid": None}}))
        f["nvram"]["boot_current"] = "0000"
        f["nvram"]["entries"]["0000"] = {"label": "APEX-OS Primary", "path": "\\EFI\\APEX\\SHIMX64.EFI",
                                         "partuuid": WIN_ESP}
        f["migration"].update(phase="failed", state_dir="/var/lib/rime/boot-migrate")   # katana: E on base D
        c, refusals, notes = B.evaluate(f)
        self.assertEqual((c["topology"], c["base"]), ("E", "D"))
        self.assertEqual(names(refusals), ["boot-chain-unverified"])
        why = dict(refusals)["boot-chain-unverified"]
        for want in ("/dev/nvme0n1p1 \\EFI\\apex\\grub.cfg", "a shape Rime does not know", "commands set, insmod, "
                     "menuentry, search, linux, initrd", "bootupd-static needs source bootuuid.cfg",
                     "missing: search --fs-uuid <UUID>, configfile", "it begins: set timeout=5"):
            self.assertIn(want, why)
        ch = B.chain_read(f)
        self.assertEqual((ch["shape"], ch["dir"], ch["lines"]), ("unknown", "apex", 8))
        text = B.report(f, c, refusals, notes, True)
        self.assertIn("boot chain: NVRAM Boot0000 -> \\EFI\\APEX\\SHIMX64.EFI; reads /dev/nvme0n1p1 \\EFI\\apex\\grub.cfg "
                      "shape=unknown -> fs None", text)
        self.assertIn("boot-chain-unverified", text.split("boot chain:")[1].splitlines()[0])


# ── the signed payload ────────────────────────────────────────────────────────────────────────────────────────────

def ed_encode(p):
    zi = pow(p[2], B._P - 2, B._P)
    x, y = p[0] * zi % B._P, p[1] * zi % B._P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def ed_sign(seed, msg):
    """RFC 8032 §5.1.6 signing, for the tests only (the bridge never signs)."""
    h = hashlib.sha512(seed).digest()
    a = (int.from_bytes(h[:32], "little") & ((1 << 254) - 8)) | (1 << 254)
    pub = ed_encode(B._ed_mul(a, B._ED_B))
    r = int.from_bytes(hashlib.sha512(h[32:] + msg).digest(), "little") % B._L
    big_r = ed_encode(B._ed_mul(r, B._ED_B))
    k = int.from_bytes(hashlib.sha512(big_r + pub + msg).digest(), "little") % B._L
    return pub, big_r + ((r + k * a) % B._L).to_bytes(32, "little")


TEST_SEED = hashlib.sha256(b"rime-bridge TEST key, throwaway, DO NOT TRUST").digest()
TEST_PUB = ed_sign(TEST_SEED, b"")[0]
TEST_KEY = "rime-test-1:" + base64.b64encode(TEST_PUB).decode()


def sign_manifest(raw, seed=TEST_SEED, name="rime-test-1", domain=True):
    return f"{name}:" + base64.b64encode(ed_sign(seed, (B.PAYLOAD_DOMAIN if domain else b"") + raw)[1]).decode()


class Ed25519(unittest.TestCase):
    def test_rfc8032_vectors(self):
        h = bytes.fromhex
        # RFC 8032 §7.1 TEST 1 and TEST 2
        self.assertEqual(ed_sign(h("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"), b"")[0].hex(),
                         "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        self.assertTrue(B.ed25519_verify(
            h("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"), b"",
            h("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd2"
              "5bf5f0595bbe24655141438e7a100b")))
        pub2 = h("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
        sig2 = h("92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c"
                 "387b2eaeb4302aeeb00d291612bb0c00")
        self.assertTrue(B.ed25519_verify(pub2, h("72"), sig2))
        self.assertFalse(B.ed25519_verify(pub2, h("73"), sig2))
        self.assertFalse(B.ed25519_verify(pub2, h("72"), sig2[:63] + bytes([sig2[63] ^ 1])))
        # S >= L (malleability) is refused even though [S]B would still match
        s = int.from_bytes(sig2[32:], "little") + B._L
        self.assertFalse(B.ed25519_verify(pub2, h("72"), sig2[:32] + s.to_bytes(32, "little")))

    def test_trusted_key_is_the_cache_key(self):
        self.assertEqual([k.split(":")[0] for k in B.TRUSTED_PAYLOAD_KEYS], ["rime-cache-1"])
        self.assertEqual(len(B.parse_key(B.TRUSTED_PAYLOAD_KEYS[0])[1]), 32)


class SignedManifest(unittest.TestCase):
    RAW = b'{"toplevel": "/nix/store/' + b"a" * 32 + b'-nixos-system"}'

    def refused(self, sig, trusted=(TEST_KEY,), raw=None):
        with self.assertRaises(B.Refused) as cm:
            B.verify_manifest_signature(self.RAW if raw is None else raw, sig, trusted)
        return cm.exception.name

    def test_signed_by_trusted_key(self):
        self.assertEqual(B.verify_manifest_signature(self.RAW, sign_manifest(self.RAW), (TEST_KEY,)), "rime-test-1")

    def test_unsigned(self):
        self.assertEqual(self.refused(None), "payload-unsigned")
        self.assertEqual(self.refused("\n"), "payload-unsigned")

    def test_tampered(self):
        sig = sign_manifest(self.RAW)
        self.assertEqual(self.refused(sig, raw=self.RAW.replace(b"aaaa", b"aaab", 1)), "payload-signature-bad")
        self.assertEqual(self.refused(sig, raw=self.RAW + b" "), "payload-signature-bad")

    def test_untrusted_key(self):
        other = hashlib.sha256(b"someone else").digest()
        self.assertEqual(self.refused(sign_manifest(self.RAW, other, "rime-other-1")), "payload-key-untrusted")
        # the right name with another key's signature is a bad signature, not a trusted one
        self.assertEqual(self.refused(sign_manifest(self.RAW, other)), "payload-signature-bad")
        # the production default trusts rime-cache-1 only
        self.assertEqual(self.refused(sign_manifest(self.RAW), B.TRUSTED_PAYLOAD_KEYS), "payload-key-untrusted")

    def test_domain_separated(self):
        # A signature over the bare manifest (what a narinfo-style signature would be) is not a payload signature.
        self.assertEqual(self.refused(sign_manifest(self.RAW, domain=False)), "payload-signature-bad")

    def test_garbage(self):
        self.assertEqual(self.refused("rime-test-1:not base64!"), "payload-signature-bad")
        self.assertEqual(self.refused("rime-test-1:" + base64.b64encode(b"short").decode()), "payload-signature-bad")


def make_payload(d, sign=True, preview=None):
    os.makedirs(d, exist_ok=True)
    for n, data in (("store.tar.zst", b"store"), ("registration", b"reg")):
        with open(os.path.join(d, n), "wb") as f:
            f.write(data)
    m = {"toplevel": "/nix/store/" + "b" * 32 + "-nixos-system-rime", "nix": "/nix/store/" + "c" * 32 + "-nix-2.31",
         "store": {"file": "store.tar.zst", "sha256": hashlib.sha256(b"store").hexdigest()},
         "registration": {"file": "registration", "sha256": hashlib.sha256(b"reg").hexdigest()}}
    if preview:
        m["preview"] = {"file": "preview.tar", "sha256": B.sha256_file(preview)}
        shutil.copy(preview, os.path.join(d, "preview.tar"))
    raw = json.dumps(m, indent=1).encode()
    with open(os.path.join(d, "manifest.json"), "wb") as f:
        f.write(raw)
    if sign:
        with open(os.path.join(d, "manifest.sig"), "w") as f:
            f.write(sign_manifest(raw) + "\n")
    return m


class Payload(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.d)

    def test_signed_payload_verifies(self):
        make_payload(self.d)
        man = B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(man["signed_by"], "rime-test-1")

    def test_unsigned_payload_refused(self):
        make_payload(self.d, sign=False)
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-unsigned")

    def test_store_swapped_after_signing(self):
        # the manifest still verifies, the file it names does not: the sha256 binds the store to the signature
        make_payload(self.d)
        with open(os.path.join(self.d, "store.tar.zst"), "wb") as f:
            f.write(b"evil")
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-corrupt")

    def test_manifest_edited_after_signing(self):
        make_payload(self.d)
        p = os.path.join(self.d, "manifest.json")
        m = json.loads(pathlib.Path(p).read_text())
        m["toplevel"] = "/nix/store/" + "d" * 32 + "-nixos-system-evil"
        with open(p, "w") as f:
            json.dump(m, f, indent=1)
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-signature-bad")

    def test_production_key_refuses_the_lab_key(self):
        make_payload(self.d)
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d)
        self.assertEqual(cm.exception.name, "payload-key-untrusted")


class Preview(unittest.TestCase):
    """check --explain --payload: the payload's own rime-hostlayer runs `carry --plan` by THIS system's python."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        tree = os.path.join(self.d, "tree")
        os.makedirs(os.path.join(tree, "system/sw/bin"))
        os.makedirs(os.path.join(tree, "system/etc/rime/hostlayer"))
        with open(os.path.join(tree, "greetd-config.toml"), "w") as f:
            f.write("[terminal]\nvt = 1\n")
        # A stand-in rime-hostlayer that reports how it was run.
        with open(os.path.join(tree, "rime-hostlayer"), "w") as f:
            f.write("import json, os, sys\nprint('LEFT BEHIND (1; every entry is in report.json)')\n"
                    "print('ARGV ' + json.dumps(sys.argv[1:]))\n"
                    "print('GREETD ' + os.environ.get('RIME_GREETD_DEFAULT', ''))\n"
                    "print('ROOT-EMPTY ' + str(os.listdir(sys.argv[sys.argv.index('--root') + 1]) == []))\n")
        self.tar = os.path.join(self.d, "preview.tar")
        with tarfile.open(self.tar, "w") as tf:
            for n in sorted(os.listdir(tree)):
                tf.add(os.path.join(tree, n), n)
        self.payload = os.path.join(self.d, "payload")

    def tearDown(self):
        shutil.rmtree(self.d)

    def test_plan_only_with_the_old_cmdline(self):
        man = make_payload(self.payload, preview=self.tar)
        rc, text = B.carry_preview(self.payload, man, src="/nonexistent-src", cmdline="/nonexistent-cmdline")
        self.assertEqual(rc, 0, text)
        argv = json.loads(text.split("ARGV ", 1)[1].splitlines()[0])
        self.assertEqual(argv[0], "carry")
        self.assertIn("--plan", argv)
        self.assertEqual(argv[argv.index("--from") + 1], "/nonexistent-src")
        self.assertEqual(argv[argv.index("--cmdline") + 1], "/nonexistent-cmdline")
        self.assertTrue(argv[argv.index("--system") + 1].endswith("/system"))
        self.assertIn("greetd-config.toml", text.split("GREETD ", 1)[1].splitlines()[0])
        self.assertIn("ROOT-EMPTY True", text)
        self.assertIn("LEFT BEHIND", text)

    def test_no_preview_refused_by_name(self):
        man = make_payload(self.payload)
        with self.assertRaises(B.Refused) as cm:
            B.carry_preview(self.payload, man)
        self.assertEqual(cm.exception.name, "payload-no-preview")

    def test_preview_tar_is_signed_too(self):
        make_payload(self.payload, preview=self.tar)
        with open(os.path.join(self.payload, "preview.tar"), "ab") as f:
            f.write(b"\0" * 512)
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.payload, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-corrupt")


# ── key-export ───────────────────────────────────────────────────────────────────────────────────────────────────

class FakeRun:
    """B.run with cryptsetup faked (luksHeaderBackup writes a file, luksDump prints a UUID); the rest runs for real."""

    def __init__(self, dump_uuid):
        self.calls, self.dump_uuid, self.real = [], dump_uuid, B.run

    def __call__(self, cmd, check=True, capture=True, **kw):
        self.calls.append(list(cmd))
        if cmd[0] == "cryptsetup" and cmd[1] == "luksHeaderBackup":
            with open(cmd[cmd.index("--header-backup-file") + 1], "wb") as f:
                f.write(b"LUKS\xba\xbe" + b"\0" * 64)
            return B.subprocess.CompletedProcess(cmd, 0, "", "")
        if cmd[0] == "cryptsetup" and cmd[1] == "luksDump":
            return B.subprocess.CompletedProcess(cmd, 0, f"LUKS header information\nVersion: 2\nUUID: {self.dump_uuid}\n", "")
        if cmd[0] == "tar":
            return B.subprocess.CompletedProcess(cmd, 0, "", "")
        return self.real(cmd, check=check, capture=capture, **kw)


class KeyExport(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.keys = os.path.join(self.d, "rime-backup")
        os.makedirs(os.path.join(self.keys, "keys"))
        os.makedirs(os.path.join(self.keys, "recipients"))
        with open(os.path.join(self.keys, "keys/1000.key"), "w") as f:
            f.write("AGE-SECRET-KEY-TEST\n")
        with open(os.path.join(self.keys, "recipients/1000.pub"), "w") as f:
            f.write("age1test\n")
        self.saved = (B.run, B.BACKUP_KEYS_ROOT)
        B.BACKUP_KEYS_ROOT = self.keys

    def tearDown(self):
        B.run, B.BACKUP_KEYS_ROOT = self.saved
        shutil.rmtree(self.d)

    def test_header_and_keys(self):
        B.run = FakeRun(LUKS_UUID)
        dest = os.path.join(self.d, "out")
        man = B.export_keys(dest, luks_a())
        self.assertIn(["cryptsetup", "luksHeaderBackup", "/dev/vda3", "--header-backup-file",
                       os.path.join(dest, "luks-header.img")], B.run.calls)
        self.assertIn(["cryptsetup", "luksDump", os.path.join(dest, "luks-header.img")], B.run.calls)
        self.assertEqual(man["luks"]["uuid"], LUKS_UUID)
        self.assertEqual(sorted(man["backup_keys"]), ["keys/1000.key", "recipients/1000.pub"])
        self.assertEqual(pathlib.Path(dest, "rime-backup/keys/1000.key").read_text(), "AGE-SECRET-KEY-TEST\n")
        self.assertEqual(os.stat(dest).st_mode & 0o777, 0o700)
        readme = pathlib.Path(dest, "README.txt").read_text()
        self.assertIn("recovery key", readme)
        self.assertIn("luksHeaderRestore", readme)
        self.assertEqual(json.loads(pathlib.Path(os.path.join(dest, "keys.json")).read_text())["luks"]["sha256"],
                         B.sha256_file(os.path.join(dest, "luks-header.img")))

    def test_header_read_back_must_name_the_volume(self):
        B.run = FakeRun("00000000-0000-4000-8000-000000000000")
        with self.assertRaises(B.Failed):
            B.export_keys(os.path.join(self.d, "out"), luks_a())

    def test_plain_root_has_no_header(self):
        B.run = FakeRun(LUKS_UUID)
        f = luks_a()
        f["root"]["luks"] = None
        man = B.export_keys(os.path.join(self.d, "out"), f)
        self.assertIsNone(man["luks"])
        self.assertFalse(any(c[0] == "cryptsetup" for c in B.run.calls))

    def test_start_backup_exports_the_keys(self):
        B.run = FakeRun(LUKS_UUID)
        home = os.path.join(self.d, "home")
        os.makedirs(os.path.join(home, "alex"))
        with open(os.path.join(home, "alex/note.txt"), "w") as f:
            f.write("hi\n")
        dest = os.path.join(self.d, "backup")
        os.makedirs(dest)
        out = B.do_backup(dest, luks_a(), home)
        self.assertTrue(os.path.isfile(os.path.join(out, "keys/luks-header.img")))
        self.assertTrue(os.path.isfile(os.path.join(out, "keys/rime-backup/recipients/1000.pub")))
        self.assertTrue(os.path.isfile(os.path.join(out, "RESTORE-TESTED")))


# ── repair-e: the trial's /composefs and /state ─────────────────────────────────────────────────────────────────

class ArchiveTrialTop(unittest.TestCase):
    ARCH = "ostree/deploy/default/var/lib/rime/boot-migrate/archive-T"

    def setUp(self):
        self.top = tempfile.mkdtemp()
        for rel, data in (("composefs/images/aa", b"image"), ("composefs/repo/objects/x", b"obj"),
                          ("state/deploy/abc/etc/hostname", b"trial\n"), ("state/deploy/abc/var/user-file", b"mine")):
            p = os.path.join(self.top, rel)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as f:
                f.write(data)
        os.symlink("images/aa", os.path.join(self.top, "composefs/link"))
        os.makedirs(os.path.join(self.top, "ostree/deploy/default/var/lib/rime/boot-migrate"))
        self.before = B.tree_sums(self.top, ["composefs", "state"])
        self.saved = os.rename

    def tearDown(self):
        os.rename = self.saved
        shutil.rmtree(self.top)

    def check_moved(self, moved, how):
        self.assertEqual([(m["name"], m["how"]) for m in moved], [("composefs", how), ("state", how)])
        self.assertFalse(os.path.lexists(os.path.join(self.top, "composefs")))
        self.assertFalse(os.path.lexists(os.path.join(self.top, "state")))
        after = B.tree_sums(os.path.join(self.top, self.ARCH), ["composefs", "state"])
        self.assertEqual(after, self.before)
        self.assertEqual(moved[1]["to"], "/" + self.ARCH + "/state")

    def test_moved_not_deleted(self):
        self.check_moved(B.archive_trial_top(self.top, self.ARCH), "renamed")

    def test_across_subvolumes_copied_then_removed(self):
        def exdev(src, dst):
            raise OSError(errno.EXDEV, "Invalid cross-device link")
        os.rename = exdev
        self.check_moved(B.archive_trial_top(self.top, self.ARCH), "copied (reflink), compared, original removed")

    def test_nothing_there(self):
        shutil.rmtree(os.path.join(self.top, "composefs"))
        shutil.rmtree(os.path.join(self.top, "state"))
        self.assertEqual(B.archive_trial_top(self.top, self.ARCH), [])

    def test_existing_archive_is_never_overwritten(self):
        os.makedirs(os.path.join(self.top, self.ARCH, "composefs"))
        with self.assertRaises(B.Failed):
            B.archive_trial_top(self.top, self.ARCH)
        self.assertTrue(os.path.isdir(os.path.join(self.top, "composefs/images")))

    def test_leftover_trees_keep_repair_pending(self):
        f = luks_a()
        f["migration"] = {"phase": "failed", "state_dir": "/var/lib/rime/boot-migrate", "files": ["phase"],
                          "composefs_leftover": True, "trial_top": ["composefs", "state"]}
        self.assertEqual(B.trial_leftovers(f)["top"], ["composefs", "state"])
        c, refusals, notes = B.evaluate(f)
        self.assertEqual(c["topology"], "E")
        self.assertTrue(any("/composefs (btrfs top level)" in n and "/state" in n for n in notes), notes)


if __name__ == "__main__":
    unittest.main()
