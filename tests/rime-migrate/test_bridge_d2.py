"""rime-migrate's D2 completion tests (bridge2): the installer's LUKS layout with /boot on its own partition, the
host.toml [boot] table start writes, the EFI grub.cfg shapes check reads, the signed payload manifest, the carry
preview check shows before start, key-export, and repair-e's archive of the trial's /composefs and /state.

Same loader as test_bridge.py: RIME_MIGRATE names the program under test (a mutant, in nix/tests/bridge.nix).
"""

import base64
import contextlib
import copy
import errno
import hashlib
import importlib.machinery
import importlib.util
import io
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
                 "luks": {"device": "/dev/vda3", "uuid": LUKS_UUID, "mapper": f"/dev/mapper/luks-{LUKS_UUID}"},
                 "usage": {"data_size": 30 * GB, "data_used": 18 * GB, "meta_size": 2 * GB, "meta_used": GB,
                           "unallocated": 48 * GB, "global_reserve": GB // 10, "data_ratio": 1.0, "meta_ratio": 2.0,
                           "mixed": False}},
        "boot": {"kind": "partition", "uuid": BOOT_UUID, "fstype": "ext4", "device": "/dev/vda2",
                 "grub_cfg_present": True, "sources_custom_cfg": True, "loader_link": "loader.0",
                 "custom_cfg_present": False},
        "ostree": {"booted": True, "composefs_native": False, "deployments": ["ostree/deploy/default/deploy/x.0"],
                   "store_bytes": 9 * GB},
        "home": {"path": "/var/home", "bytes": 1 * GB, "entries": 20000, "small_bytes": 10**7},
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

    def test_written_over_carrys_file_keeps_it(self):
        """start's write keeps every table carry wrote (the lab's first encrypted run lost rd.luks.* here: the file
        was opened for writing, so truncated, before it was read)."""
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d)
        ht = os.path.join(d, "host.toml")
        carried = ('schema = 1\n\n[host]\ntimezone = "Australia/Perth"\n\n[boot]\nkernel_params = ["rd.luks.uuid=%s"]\n\n'
                   '[[luks]]\nname = "luks-%s"\ndevice = "UUID=%s"\n' % (LUKS_UUID, LUKS_UUID, LUKS_UUID))
        with open(ht, "w") as f:
            f.write(carried)
        B.write_boot_table(ht, self.TABLE)
        with open(ht, "rb") as f:
            t = tomllib.load(f)
        self.assertEqual(t["boot"], dict(self.TABLE, kernel_params=[f"rd.luks.uuid={LUKS_UUID}"]))
        self.assertEqual(t["host"], {"timezone": "Australia/Perth"})
        self.assertEqual(t["luks"][0]["device"], f"UUID={LUKS_UUID}")
        self.assertEqual(oct(os.stat(ht).st_mode & 0o777), "0o644")
        self.assertEqual(os.listdir(d), ["host.toml"])

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


def sign_manifest(raw, seed=TEST_SEED, name="rime-test-1", prefix=b""):
    """manifest.json.sig as the release writes it: the key name and the signature over the manifest's exact bytes."""
    return f"{name}:" + base64.b64encode(ed_sign(seed, prefix + raw)[1]).decode()


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
        # S' = L - S makes [S']B = -(R + [k]A): the same y, the other x. Only the x comparison refuses it
        # (mutant ed25519-y-only).
        neg = (B._L - int.from_bytes(sig2[32:], "little")) % B._L
        self.assertFalse(B.ed25519_verify(pub2, h("72"), sig2[:32] + neg.to_bytes(32, "little")))

    def test_the_product_trusts_the_image_key_file(self):
        # The shipped program trusts the key file the bootc image carries (the readiness release), nothing compiled in.
        self.assertIsNone(B.TRUSTED_PAYLOAD_KEYS)
        self.assertEqual(B.PAYLOAD_KEY_FILE, "/usr/share/rime-os/trust/nixos-bridge-payload.pub")
        self.assertEqual(B.PAYLOAD_SIG, "manifest.json.sig")


class KeyFile(unittest.TestCase):
    """payload_keys(): the image's nixos-bridge-payload.pub, in the readiness release's format."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.f = os.path.join(self.d, "nixos-bridge-payload.pub")

    def tearDown(self):
        shutil.rmtree(self.d)

    def key(self, text):
        with open(self.f, "w") as f:
            f.write(text)
        try:
            return B.payload_keys(self.f), None
        except B.Refused as e:
            return None, e.name

    def test_good(self):
        self.assertEqual(self.key(TEST_KEY + "\n"), ((TEST_KEY,), None))

    def test_missing(self):
        with self.assertRaises(B.Refused) as cm:
            B.payload_keys(self.f)
        self.assertEqual(cm.exception.name, "payload-key-missing")

    def test_bad(self):
        self.assertEqual(self.key("rime-test-1:" + base64.b64encode(b"x" * 31).decode())[1], "payload-key-bad")
        self.assertEqual(self.key(TEST_KEY + "\n" + TEST_KEY + "\n")[1], "payload-key-bad")
        self.assertEqual(self.key("no colon here")[1], "payload-key-bad")

    def test_payload_checked_against_the_key_file(self):
        pl = os.path.join(self.d, "pl")
        make_payload(pl)
        with open(self.f, "w") as f:
            f.write(TEST_KEY + "\n")
        old = B.PAYLOAD_KEY_FILE
        B.PAYLOAD_KEY_FILE = self.f
        try:
            self.assertEqual(B.verify_payload(pl)["signed_by"], "rime-test-1")
            os.unlink(self.f)
            with self.assertRaises(B.Refused) as cm:
                B.verify_payload(pl)
            self.assertEqual(cm.exception.name, "payload-key-missing")
        finally:
            B.PAYLOAD_KEY_FILE = old


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
        self.assertEqual(self.refused(sign_manifest(self.RAW), ("rime-other-1:" + TEST_KEY.split(":")[1],)),
                         "payload-key-untrusted")

    def test_exact_bytes_only(self):
        # The signature covers manifest.json's bytes and nothing else: one with any prefix (the domain-separated form
        # this branch first used) is not the readiness release's format and is refused.
        self.assertEqual(self.refused(sign_manifest(self.RAW, prefix=b"rime-bridge-payload-v1\n")),
                         "payload-signature-bad")

    def test_one_line(self):
        sig = sign_manifest(self.RAW)
        self.assertEqual(B.verify_manifest_signature(self.RAW, sig + "\n", (TEST_KEY,)), "rime-test-1")
        self.assertEqual(self.refused(sig + "\n" + sig + "\n"), "payload-signature-bad")

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
        with open(os.path.join(d, "manifest.json.sig"), "w") as f:
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

    def test_signed_toplevel_must_be_a_store_path(self):
        # Defence behind the signature: a signed manifest whose toplevel is not a /nix/store path is still refused
        # (start chroots into it and runs its activate) (mutant manifest-paths-unchecked).
        make_payload(self.d)
        p = os.path.join(self.d, "manifest.json")
        m = json.loads(pathlib.Path(p).read_text())
        m["toplevel"] = "/etc/../nix/store/" + "b" * 32 + "-x"
        raw = json.dumps(m, indent=1).encode()
        pathlib.Path(p).write_bytes(raw)
        pathlib.Path(self.d, "manifest.json.sig").write_text(sign_manifest(raw) + "\n")
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-corrupt")

    def test_no_key_file_refuses_every_payload(self):
        # The product with no nixos-bridge-payload.pub (a release before the key ships): nothing is installed.
        make_payload(self.d)
        old, B.PAYLOAD_KEY_FILE = B.PAYLOAD_KEY_FILE, os.path.join(self.d, "absent.pub")
        try:
            with self.assertRaises(B.Refused) as cm:
                B.verify_payload(self.d)
        finally:
            B.PAYLOAD_KEY_FILE = old
        self.assertEqual(cm.exception.name, "payload-key-missing")

    def test_old_signature_file_name_is_unsigned(self):
        make_payload(self.d)
        os.rename(os.path.join(self.d, "manifest.json.sig"), os.path.join(self.d, "manifest.sig"))
        with self.assertRaises(B.Refused) as cm:
            B.verify_payload(self.d, (TEST_KEY,))
        self.assertEqual(cm.exception.name, "payload-unsigned")


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

    def test_masks_survive_and_specials_never_crash(self):
        """A NixOS etc/ holds unit masks: a dereferencing tar writes them as the /dev/null character device, a plain
        one as a link to /dev/null. Both come back as links to /dev/null; a fifo, another device and an absolute link
        elsewhere are left out and counted (the lab's first SB run crashed here with SpecialFileError)."""
        def member(name, kind, **kw):
            m = tarfile.TarInfo(name)
            m.type = kind
            for k, v in kw.items():
                setattr(m, k, v)
            return m
        with tarfile.open(self.tar, "a") as tf:
            tf.addfile(member("system/etc/systemd/system/apex-flathub-setup.service", tarfile.CHRTYPE, devmajor=1, devminor=3))
            tf.addfile(member("system/etc/systemd/system/other.service", tarfile.SYMTYPE, linkname="/dev/null"))
            tf.addfile(member("system/etc/fifo", tarfile.FIFOTYPE))
            tf.addfile(member("system/etc/zero", tarfile.CHRTYPE, devmajor=1, devminor=5))
            tf.addfile(member("system/etc/abs", tarfile.SYMTYPE, linkname="/etc/passwd"))
        out = os.path.join(self.d, "out")
        os.makedirs(out)
        masks, skipped = B.preview_extract(self.tar, out)
        self.assertEqual(sorted(masks), ["system/etc/systemd/system/apex-flathub-setup.service",
                                         "system/etc/systemd/system/other.service"])
        self.assertEqual(sorted(skipped), ["system/etc/abs", "system/etc/fifo", "system/etc/zero"])
        for n in masks:
            self.assertEqual(os.readlink(os.path.join(out, n)), "/dev/null")
        for n in skipped:
            self.assertFalse(os.path.lexists(os.path.join(out, n)))
        self.assertTrue(os.path.isfile(os.path.join(out, "rime-hostlayer")))
        man = make_payload(self.payload, preview=self.tar)
        rc, text = B.carry_preview(self.payload, man, src="/nonexistent-src", cmdline="/nonexistent-cmdline")
        self.assertEqual(rc, 0, text)
        self.assertIn("preview: 2 unit mask(s) kept as links to /dev/null, 3 special file(s) left out", text)

    def test_mask_never_written_through_a_link(self):
        """A mask whose directory is a link out of the extraction dir is left out, never created there."""
        outside = os.path.join(self.d, "outside")
        os.makedirs(outside)
        with tarfile.open(self.tar, "a") as tf:
            m = tarfile.TarInfo("system/escape/x.service")
            m.type, m.devmajor, m.devminor = tarfile.CHRTYPE, 1, 3
            tf.addfile(m)
        out = os.path.join(self.d, "out")
        os.makedirs(os.path.join(out, "system"))
        os.symlink(outside, os.path.join(out, "system/escape"))
        masks, skipped = B.preview_extract(self.tar, out)
        self.assertIn("system/escape/x.service", skipped)
        self.assertEqual(os.listdir(outside), [])

    def _with_fstab(self, text):
        tree = os.path.join(self.d, "fstab-tree")
        os.makedirs(os.path.join(tree, "system/etc"))
        with open(os.path.join(tree, "system/etc/fstab"), "w") as f:
            f.write(text)
        with tarfile.open(self.tar, "a") as tf:
            tf.add(os.path.join(tree, "system/etc/fstab"), "system/etc/fstab")
        return make_payload(self.payload, preview=self.tar)

    def test_fresh_install_boot_refused(self):
        """The payload of 2026.11.10.x: its system was the fresh-install toplevel, /boot = Rime's ESP in its fstab;
        a moved machine has no such partition and its first NixOS boot stopped in emergency mode."""
        man = self._with_fstab("# This is a generated file.\n"
                               "/dev/disk/by-label/rime-root / btrfs x-initrd.mount,subvol=@nixos,compress-force=zstd:3 0 0\n"
                               "/dev/disk/by-label/RIME-ESP /boot vfat umask=0077 0 2\n")
        with self.assertRaises(B.Refused) as cm:
            B.carry_preview(self.payload, man, src="/nonexistent-src", cmdline="/nonexistent-cmdline")
        self.assertEqual(cm.exception.name, "payload-boot-from-build")
        self.assertIn("RIME-ESP", str(cm.exception))

    def test_boot_spelled_otherwise_refused(self):
        """mount reads `/boot/`, `//boot`, `/./boot` as /boot (review of the first fix: only `/boot` was matched)."""
        for where in ("/boot/", "//boot", "/./boot", "/boot//"):
            self.assertEqual(B.fstab_mounts_boot(f"LABEL=RIME-ESP {where} vfat umask=0077 0 2\n"), "LABEL=RIME-ESP", where)
        for where in ("/bootx", "/boot/efi", "/boot\\040x", "/mnt/boot"):
            self.assertIsNone(B.fstab_mounts_boot(f"LABEL=X {where} vfat defaults 0 2\n"), where)
        self.assertIsNone(B.fstab_mounts_boot("#LABEL=RIME-ESP /boot vfat umask=0077 0 2\n/boot\n"))

    def test_boot_from_host_accepted(self):
        """A system whose fstab has no /boot line (rime.rootfs's generator mounts it from host.toml) is shown."""
        man = self._with_fstab("# /dev/disk/by-label/RIME-ESP /boot vfat umask=0077 0 2\n"
                               "/dev/disk/by-label/rime-root / btrfs x-initrd.mount,subvol=@nixos 0 0\n"
                               "/dev/disk/by-label/rime-root /mnt/rime-top btrfs subvolid=5 0 0\n"
                               "/dev/disk/by-label/rime-root /boot/efi btrfs subvolid=5 0 0\n")
        rc, text = B.carry_preview(self.payload, man, src="/nonexistent-src", cmdline="/nonexistent-cmdline")
        self.assertEqual(rc, 0, text)
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



# ── the commands: check, start and key-export call their gates (not only the helpers do) ─────────────────────────

class Commands(unittest.TestCase):
    """cmd_start verifies the payload's signature before it mounts or writes anything; cmd_check refuses when the
    carry preview fails; cmd_key_export refuses the disk whose keys it exports. Mutants start-unverified,
    check-carry-refusal-dropped, key-export-same-disk (nix/tests/bridge.nix)."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.keyfile = os.path.join(self.d, "nixos-bridge-payload.pub")
        pathlib.Path(self.keyfile).write_text(TEST_KEY + "\n")
        self.saved = {n: getattr(B, n) for n in ("gather", "collect_live", "backup_facts", "PAYLOAD_KEY_FILE",
                                                  "mount", "run", "STATE", "BACKUP_KEYS_ROOT", "wait_units",
                                                  "RUN_DIR")}
        B.PAYLOAD_KEY_FILE = self.keyfile
        B.STATE = os.path.join(self.d, "state")
        B.RUN_DIR = os.path.join(self.d, "run")
        B.BACKUP_KEYS_ROOT = os.path.join(self.d, "no-backup-keys")
        self.facts = luks_a()
        self.facts["existing_subvols"] = []
        self.facts["backup"] = {"path": os.path.join(self.d, "backup"), "exists": True, "disk": "/dev/vdc",
                                "free_bytes": 100 * GB}
        B.gather = lambda a: copy.deepcopy(self.facts)

    def tearDown(self):
        for n, v in self.saved.items():
            setattr(B, n, v)
        shutil.rmtree(self.d)

    def forbid_writes(self):
        def touched(*a, **k):
            raise AssertionError(f"start touched the machine before the payload was verified: {a}")
        B.mount = B.run = B.wait_units = touched

    def start(self, payload):
        a = B.argparse.Namespace(cmd="start", yes=True, backup=self.facts["backup"]["path"], payload=payload, disk=[])
        with self.assertRaises(B.Refused) as cm, contextlib.redirect_stdout(io.StringIO()):
            B.cmd_start(a)
        self.assertFalse(os.path.exists(B.STATE), "start wrote its state before the payload was verified")
        return cm.exception.name

    def test_start_refuses_an_unsigned_payload(self):
        p = os.path.join(self.d, "payload")
        make_payload(p, sign=False)
        self.forbid_writes()
        self.assertEqual(self.start(p), "payload-unsigned")

    def test_start_refuses_a_payload_signed_by_another_key(self):
        p = os.path.join(self.d, "payload")
        make_payload(p, sign=False)
        raw = pathlib.Path(p, "manifest.json").read_bytes()
        other = hashlib.sha256(b"another TEST key").digest()
        pathlib.Path(p, "manifest.json.sig").write_text(sign_manifest(raw, seed=other, name="rime-other-1") + "\n")
        self.forbid_writes()
        self.assertEqual(self.start(p), "payload-key-untrusted")

    def test_start_refuses_a_tampered_payload(self):
        p = os.path.join(self.d, "payload")
        make_payload(p)
        with open(os.path.join(p, "manifest.json"), "ab") as f:
            f.write(b" ")
        self.forbid_writes()
        self.assertEqual(self.start(p), "payload-signature-bad")

    def test_start_refuses_a_payload_without_the_left_behind_report(self):
        # Signed and intact, but no preview.tar: start must not begin without showing what stays behind (D2).
        p = os.path.join(self.d, "payload")
        make_payload(p)
        self.forbid_writes()
        self.assertEqual(self.start(p), "payload-no-preview")

    def preview(self, rc):
        tree = os.path.join(self.d, f"tree{rc}")
        os.makedirs(os.path.join(tree, "system/etc"))
        pathlib.Path(tree, "greetd-config.toml").write_text("[terminal]\nvt = 1\n")
        pathlib.Path(tree, "rime-hostlayer").write_text(
            f"import sys\nprint('LEFT BEHIND (1; every entry is in report.json)')\nsys.exit({rc})\n")
        tar = os.path.join(self.d, f"preview{rc}.tar")
        with tarfile.open(tar, "w") as tf:
            for n in sorted(os.listdir(tree)):
                tf.add(os.path.join(tree, n), n)
        p = os.path.join(self.d, f"payload{rc}")
        make_payload(p, preview=tar)
        return p

    def check(self, payload):
        a = B.argparse.Namespace(cmd="check", explain=True, json=False, backup=None, disk=[], payload=payload)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = B.cmd_check(a)
        return rc, out.getvalue()

    def test_check_shows_the_left_behind_report(self):
        rc, out = self.check(self.preview(0))
        self.assertEqual(rc, B.EXIT_OK, out)
        self.assertIn("LEFT BEHIND", out)

    def test_check_refuses_when_the_carry_plan_fails(self):
        rc, out = self.check(self.preview(4))
        self.assertEqual(rc, B.EXIT_REFUSED, out)
        self.assertIn("carry-refused", out)
        self.assertIn("LEFT BEHIND", out)

    def key_export(self, disk):
        B.collect_live = lambda: luks_a()
        B.backup_facts = lambda p, devs=None: {"path": p, "exists": True, "disk": disk, "free_bytes": 100 * GB}
        B.run = FakeRun(LUKS_UUID)
        with contextlib.redirect_stdout(io.StringIO()):
            return B.cmd_key_export(B.argparse.Namespace(cmd="key-export", to=self.d))

    def test_key_export_refuses_the_disk_it_exports_from(self):
        with self.assertRaises(B.Refused) as cm:
            self.key_export("/dev/vda")
        self.assertEqual(cm.exception.name, "backup-same-disk")
        self.assertFalse([n for n in os.listdir(self.d) if n.startswith("rime-keys-")])

    def test_key_export_to_another_disk(self):
        self.assertEqual(self.key_export("/dev/vdc"), B.EXIT_OK)
        out = [n for n in os.listdir(self.d) if n.startswith("rime-keys-")]
        self.assertEqual(len(out), 1)
        self.assertTrue(os.path.isfile(os.path.join(self.d, out[0], "luks-header.img")))


class StartRecord(unittest.TestCase):
    """The start record (STATE/state.json): every step after it exists is inside the rollback, so a start that fails in
    the backup, while masking the old side or at the first fsck gate unmasks what it masked and records `rolled back`;
    check and start both refuse over a record that neither finished nor rolled back. Mutants start-early-steps-outside-
    rollback, check-ignores-start-record (nix/tests/bridge.nix)."""

    NAMES = ("gather", "PAYLOAD_KEY_FILE", "mount", "run", "STATE", "RUN_DIR", "wait_units", "do_backup",
             "mask_old_side", "unmask_old_side", "fsck_gate", "containment_snapshot", "carry_preview", "boot_had",
             "undo_boot", "test_restore_point", "chroot", "first_activation")

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.saved = {n: getattr(B, n) for n in self.NAMES}
        keyfile = os.path.join(self.d, "nixos-bridge-payload.pub")
        pathlib.Path(keyfile).write_text(TEST_KEY + "\n")
        B.PAYLOAD_KEY_FILE = keyfile
        B.STATE = os.path.join(self.d, "state")
        B.RUN_DIR = os.path.join(self.d, "run")
        self.facts = luks_a()
        self.facts["existing_subvols"] = []
        self.facts["backup"] = {"path": os.path.join(self.d, "backup"), "exists": True, "disk": "/dev/vdc",
                                "free_bytes": 100 * GB}
        B.gather = lambda a: copy.deepcopy(self.facts)
        self.calls = []
        B.mount = lambda *a: self.calls.append(("mount",) + a)
        B.run = lambda cmd, **k: self.calls.append(("run", tuple(cmd)))
        B.wait_units = lambda *a, **k: None
        B.carry_preview = lambda payload, man, *a, **k: (0, "LEFT BEHIND (0)")
        B.boot_had = lambda bdir: {}
        B.undo_boot = lambda *a: self.calls.append(("undo_boot",))
        B.do_backup = lambda *a: os.path.join(self.d, "backup", "b")
        B.containment_snapshot = lambda *a: {}
        self.unmasked = []
        B.unmask_old_side = lambda state: self.unmasked.append(list(state.get("masked", [])))
        self.payload = os.path.join(self.d, "payload")
        make_payload(self.payload)

    def tearDown(self):
        for n, v in self.saved.items():
            setattr(B, n, v)
        shutil.rmtree(self.d)

    def masks(self, state, save=lambda: None):
        state.setdefault("masked", []).append("rime-update-check.timer")

    def start(self):
        a = B.argparse.Namespace(cmd="start", yes=True, backup=(self.facts.get("backup") or {}).get("path"),
                                 payload=self.payload, disk=[])
        with contextlib.redirect_stdout(io.StringIO()):
            return B.cmd_start(a)

    def record(self):
        with open(os.path.join(B.STATE, "state.json")) as f:
            return json.load(f)

    def fails_in(self, where):
        def boom(*a, **k):
            raise B.Failed(f"TEST failure in {where}")
        return boom

    def assert_rolled_back(self, masked):
        rec = self.record()
        self.assertEqual(rec["steps"][-1], "rolled back", rec["steps"])
        self.assertIn("TEST failure", rec["failed"])
        self.assertEqual(self.unmasked, [masked])
        self.assertIsNone(B.record_refusal(B.start_record()))

    def test_a_failed_backup_rolls_back(self):
        B.do_backup = self.fails_in("backup")
        B.mask_old_side = self.masks
        with self.assertRaises(B.Failed):
            self.start()
        self.assert_rolled_back([])

    def test_a_failure_while_masking_unmasks_what_was_masked(self):
        def half(state, save=lambda: None):
            self.masks(state)
            raise B.Failed("TEST failure half way through the masks")
        B.mask_old_side = half
        with self.assertRaises(B.Failed):
            self.start()
        self.assert_rolled_back(["rime-update-check.timer"])

    def test_a_failed_first_fsck_gate_rolls_back(self):
        B.mask_old_side = self.masks
        B.fsck_gate = self.fails_in("fsck gate before")
        with self.assertRaises(B.Failed):
            self.start()
        self.assert_rolled_back(["rime-update-check.timer"])

    def no_backup(self):
        del self.facts["backup"]
        self.facts["home"]["in_top_level"] = True
        B.do_backup = self.fails_in("do_backup (there is no --backup)")
        B.mask_old_side = self.masks
        B.fsck_gate = lambda *a, **k: {}

    def test_the_installed_system_is_checked_for_a_boot_mount(self):
        # The preview is not the bytes start installs (review of the first fix): after `nix-env --set`, the installed
        # toplevel's own etc/fstab is read in the target, and a /boot line there rolls the start back by name.
        self.no_backup()
        B.test_restore_point = lambda *a: {"sampled": 0}
        man = json.load(open(os.path.join(self.payload, "manifest.json")))
        chroots = []

        def chroot(t, *cmd, **kw):
            chroots.append(cmd)
            out = ""
            if cmd[:2] == (f"{man['toplevel']}/sw/bin/sh", "-c"):
                out = "/dev/disk/by-label/RIME-ESP /boot/ vfat umask=0077 0 2\n"
            return B.subprocess.CompletedProcess(cmd, 0, out, "")
        B.chroot = chroot
        B.first_activation = lambda *a, **k: self.fail("activated a system that mounts /boot from its build")
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(B.Refused) as cm:
            self.start()
        self.assertEqual(cm.exception.name, "payload-boot-from-build")
        rec = self.record()
        self.assertEqual(rec["steps"][-2:], ["install closure", "rolled back"], rec["steps"])
        self.assertIn("RIME-ESP", rec["failed"])
        sets = [i for i, c in enumerate(chroots) if "--set" in c]
        checks = [i for i, c in enumerate(chroots) if c[:2] == (f"{man['toplevel']}/sw/bin/sh", "-c")]
        self.assertEqual(len(checks), 1)
        self.assertLess(sets[0], checks[0], "the check ran before the system was installed")

    def test_start_without_backup_tests_the_snapshot_as_its_restore_point(self):
        # No drive: start goes on with no backup step; the read-only @pre-nixos snapshot is restore-tested right after
        # it is taken, and a failed restore test removes it and rolls back like any other failure.
        self.no_backup()
        tested = []

        def restore_test(snap, var_fsroot, live_home):
            tested.append((snap, var_fsroot, live_home))
            raise B.Failed("TEST failure in the restore test")
        B.test_restore_point = restore_test
        with self.assertRaises(B.Failed):
            self.start()
        self.assert_rolled_back(["rime-update-check.timer"])
        rec = self.record()
        self.assertEqual(rec["steps"][0], "old side", rec["steps"])
        self.assertNotIn("backup", rec["steps"])
        self.assertIsNone(rec["backup"])
        top = os.path.join(B.RUN_DIR, "top")
        self.assertEqual(tested, [(os.path.join(top, "@pre-nixos"), self.facts["var_fsroot"],
                                   os.path.join(top, self.facts["var_fsroot"].lstrip("/"), "home"))])
        cmds = [c[1] for c in self.calls if c[0] == "run"]
        snap = ("btrfs", "subvolume", "snapshot", "-r", top, os.path.join(top, "@pre-nixos"))
        self.assertIn(snap, cmds)
        self.assertIn(("btrfs", "subvolume", "delete", os.path.join(top, "@pre-nixos")), cmds)
        self.assertLess(cmds.index(snap), len(cmds) - 1)

    def test_start_without_backup_refuses_a_home_subvolume(self):
        self.no_backup()
        self.facts["home"]["in_top_level"] = False
        before = self.calls[:]
        self.assertEqual(self.start(), B.EXIT_REFUSED)
        self.assertEqual(self.calls, before, "start touched the machine")
        self.assertIsNone(B.start_record())

    def write_record(self, rec):
        os.makedirs(B.STATE, exist_ok=True)
        with open(os.path.join(B.STATE, "state.json"), "w") as f:
            f.write(rec if isinstance(rec, str) else json.dumps(rec))

    def check(self):
        a = B.argparse.Namespace(cmd="check", explain=True, json=False, backup=None, disk=[], payload=None)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = B.cmd_check(a)
        return rc, out.getvalue()

    def test_check_and_start_refuse_over_an_unfinished_start(self):
        rc, out = self.check()
        self.assertEqual(rc, B.EXIT_OK, out)  # positive control: no record
        for rec, name in (({"steps": ["backup", "old side"]}, "start-unfinished"),
                          ({"steps": ["backup", "done"], "done": "2026-11-03"}, "already-moved"),
                          ("{not json", "start-record-unreadable")):
            self.write_record(rec)
            rc, out = self.check()
            self.assertEqual(rc, B.EXIT_REFUSED, out)
            self.assertIn(name, out)
            B.mask_old_side = self.masks
            before = self.calls[:]
            self.assertEqual(self.start(), B.EXIT_REFUSED)
            self.assertEqual(self.calls, before, "start touched the machine over an existing record")

    def test_a_start_that_rolled_back_allows_another(self):
        self.write_record({"steps": ["backup", "rolled back"], "failed": "TEST"})
        rc, out = self.check()
        self.assertEqual(rc, B.EXIT_OK, out)

    def test_a_start_interrupted_by_ctrl_c_rolls_back_and_allows_another(self):
        # topology-e: Ctrl-C on the console during the closure install. KeyboardInterrupt is a BaseException, so the
        # rollback runs; str(KeyboardInterrupt()) is "", so `failed` used to be "", and check/start then refused
        # start-unfinished over a start that HAD rolled back, while `rime` (key present) allowed every bootc verb.
        B.mask_old_side = self.masks

        def interrupted(*a, **k):
            raise KeyboardInterrupt
        B.fsck_gate = interrupted
        with self.assertRaises(KeyboardInterrupt):
            self.start()
        rec = self.record()
        self.assertEqual(rec["steps"][-1], "rolled back", rec["steps"])
        self.assertEqual(rec["failed"], "KeyboardInterrupt")
        self.assertEqual(self.unmasked, [["rime-update-check.timer"]])
        self.assertIsNone(B.record_refusal(B.start_record()))
        rc, out = self.check()
        self.assertEqual(rc, B.EXIT_OK, out)

    def test_the_record_rule_is_the_readiness_release_rule(self):
        # The same table as rimed/rime/src/nixos_bridge.rs `classify` and rime-update-check's predicate (readiness
        # release): `failed` PRESENT (any value) with "rolled back" last allows; `done` set (not null) wins over
        # everything; steps that are not a list name no step.
        allow = ({"steps": ["backup", "rolled back"], "failed": ""},       # a record #137's bridge wrote on Ctrl-C
                 {"steps": ["backup", "rolled back"], "failed": None},
                 {"steps": ["rolled back"], "failed": "x", "done": None})
        for rec in allow:
            self.assertIsNone(B.record_refusal(rec), rec)
        refuse = (({"steps": ["backup", "rolled back"], "failed": "x", "done": "D"}, "already-moved"),
                  ({"steps": ["done"], "done": ""}, "already-moved"),
                  ({"steps": ["backup", "rolled back"]}, "start-unfinished"),
                  ({"steps": ["backup", "old side"], "failed": "x"}, "start-unfinished"),
                  ({"steps": {"rolled back": 1}, "failed": "x"}, "start-unfinished"),
                  ({"steps": "rolled back", "failed": "x"}, "start-unfinished"),
                  ({}, "start-unfinished"))
        for rec, name in refuse:
            got = B.record_refusal(rec)
            self.assertIsNotNone(got, rec)
            self.assertEqual(got[0], name, rec)


class OldSide(unittest.TestCase):
    """mask_old_side / unmask_old_side: start masks the old side's timers and services; a rollback puts it back."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.saved = (B.run, B.SYSTEMD_ETC)
        B.SYSTEMD_ETC = self.d
        self.calls = []
        self.failed_units = set()   # units `systemctl is-failed` reports failed
        self.refuse = {}            # (verb, unit) -> the exit code of that systemctl (the unit is left as it was)
        # Only the update timer is running (the compactor's timer, say, was stopped by hand).
        self.active = {"rime-update-check.timer"}
        self.records = []           # every start record mask_old_side saved, in order
        self.on_stop = None

        def run(cmd, check=True, capture=True, **kw):
            self.calls.append(list(cmd))
            verb, unit = cmd[1], cmd[-1]
            if cmd[:3] == ["systemctl", "is-active", "--quiet"]:
                rc = 0 if unit in self.active else 3
            elif cmd[:3] == ["systemctl", "is-failed", "--quiet"]:
                rc = 0 if unit in self.failed_units else 1
            else:
                rc = self.refuse.get((verb, unit), 0)
                if verb == "stop" and not rc:
                    self.active.discard(unit)
                if verb == "start" and not rc:
                    self.active.add(unit)
            if verb == "stop" and self.on_stop:
                self.on_stop(unit)
            if check and rc:
                raise B.Failed(f"{' '.join(cmd)} exited {rc}")
            return B.subprocess.CompletedProcess(cmd, rc, "", "")
        B.run = run

    def tearDown(self):
        B.run, B.SYSTEMD_ETC = self.saved
        shutil.rmtree(self.d)

    def save(self, state):
        self.records.append(json.loads(json.dumps(state)))

    def mask(self, state):
        with contextlib.redirect_stdout(io.StringIO()):
            B.mask_old_side(state, lambda: self.save(state))

    def unmask(self, state):
        with contextlib.redirect_stderr(io.StringIO()) as err:
            B.unmask_old_side(state)
        return err.getvalue()

    def test_a_unit_failed_before_start_is_left_failed_by_a_rollback(self):
        # The health gate must still see a failure start did not cause.
        self.failed_units.add("bootloader-update.service")
        state = {}
        self.mask(state)
        self.calls.clear()
        self.unmask(state)
        self.assertEqual(state["failed_before"], ["bootloader-update.service"])
        reset = next(c for c in self.calls if c[1] == "reset-failed")
        self.assertNotIn("bootloader-update.service", reset)
        self.assertIn("rime-update-check.timer", reset)

    def test_a_timer_that_does_not_stop_is_not_masked_under(self):
        self.refuse[("stop", "rime-update-check.timer")] = 1
        state = {}
        with self.assertRaises(B.Failed):
            self.mask(state)
        self.assertEqual(os.listdir(self.d), [])
        self.assertNotIn(["systemctl", "daemon-reload"], self.calls)

    def test_a_timer_the_image_does_not_have_does_not_stop_the_move(self):
        # `systemctl stop` exits 5 for a unit that is not loaded; it is not running, so nothing is masked under it.
        self.refuse[("stop", "rime-store-compact.timer")] = 5
        state = {}
        self.mask(state)
        self.assertEqual(sorted(state["masked"]), sorted(B.MASK_UNITS))
        self.assertEqual(state["timers_running"], ["rime-update-check.timer"])

    def test_the_record_names_a_running_timer_before_it_is_stopped(self):
        # A power loss right after the stop: the saved record must already say the timer was running.
        state = {}
        self.on_stop = lambda u: self.stops.append((u, list(self.records[-1]["timers_running"]) if self.records else []))
        self.stops = []
        self.mask(state)
        self.assertIn(("rime-update-check.timer", ["rime-update-check.timer"]), self.stops)

    def test_the_record_names_each_mask_before_it_is_made(self):
        # A power loss while saving the record: every link on disk is named by the last record that was saved.
        state, count = {}, []

        def save():
            count.append(1)
            if len(count) == len(B.OLD_SIDE_TIMERS) + 3:
                raise KeyboardInterrupt  # the machine stops while the record is written
            self.save(state)
        with self.assertRaises(KeyboardInterrupt), contextlib.redirect_stdout(io.StringIO()):
            B.mask_old_side(state, save)
        links = set(os.listdir(self.d))
        self.assertEqual(len(links), 2)
        self.assertLessEqual(links, set(self.records[-1]["masked"]))

    def test_a_rollback_whose_systemctl_fails_finishes_and_says_so(self):
        state = {}
        self.mask(state)
        self.refuse[("start", "rime-update-check.timer")] = 1
        self.calls.clear()
        err = self.unmask(state)
        self.assertEqual(os.listdir(self.d), [])
        self.assertTrue(any(c[1] == "reset-failed" for c in self.calls))
        self.assertEqual(state["old_side_left"], ["systemctl start rime-update-check.timer exited 1"])
        self.assertIn("systemctl start rime-update-check.timer", err)

    def test_a_second_start_is_refused_while_one_runs(self):
        saved = B.RUN_DIR
        B.RUN_DIR = os.path.join(self.d, "run")
        try:
            first = B.start_lock()
            with self.assertRaises(B.Refused) as e:
                B.start_lock()
            self.assertEqual(e.exception.name, "start-running")
            first.close()
            B.start_lock().close()
        finally:
            B.RUN_DIR = saved

    def test_timers_stop_before_their_services_are_masked(self):
        # Masking the service a running timer triggers fails the timer ("Unit to trigger vanished").
        state = {}
        self.mask(state)
        stops = [i for i, c in enumerate(self.calls) if c[:2] == ["systemctl", "stop"]]
        reload = self.calls.index(["systemctl", "daemon-reload"])
        self.assertEqual(len(stops), 2)
        self.assertLess(max(stops), reload)
        self.assertEqual(sorted(state["masked"]), sorted(B.MASK_UNITS))
        self.assertEqual(state["timers_running"], ["rime-update-check.timer"])
        self.assertEqual(os.readlink(os.path.join(self.d, "rime-update-check.service")), "/dev/null")

    def test_a_rollback_leaves_nothing_failed_and_restarts_the_running_timers(self):
        state = {}
        self.mask(state)
        self.calls.clear()
        B.unmask_old_side(state)
        self.assertEqual(os.listdir(self.d), [])
        self.assertIn(["systemctl", "reset-failed", *state["masked"]], self.calls)
        self.assertIn(["systemctl", "start", "rime-update-check.timer"], self.calls)
        self.assertNotIn(["systemctl", "start", "rime-store-compact.timer"], self.calls)
        self.assertLess(self.calls.index(["systemctl", "daemon-reload"]),
                        self.calls.index(["systemctl", "start", "rime-update-check.timer"]))

    def test_a_mask_made_by_someone_else_is_kept(self):
        os.symlink("/dev/null", os.path.join(self.d, "bootloader-update.service"))
        state = {}
        self.mask(state)
        B.unmask_old_side(state)
        self.assertEqual(os.listdir(self.d), ["bootloader-update.service"])
        self.assertNotIn("bootloader-update.service", next(c for c in self.calls if c[1] == "reset-failed"))


class RestorePoint(unittest.TestCase):
    """test_restore_point: the restore test start runs on @pre-nixos when there is no off-disk backup."""

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.snap = os.path.join(self.d, "@pre-nixos")
        self.var = "/ostree/deploy/default/var"
        self.snap_home = os.path.join(self.snap, self.var.lstrip("/"), "home")
        self.live = os.path.join(self.d, "live-home")
        for i in range(10):
            for base in (self.snap_home, self.live):
                p = os.path.join(base, "u", f"d{i % 3}", f"f{i}")
                os.makedirs(os.path.dirname(p), exist_ok=True)
                pathlib.Path(p).write_bytes(f"file {i}\n".encode() * 50)
                os.utime(p, ns=(10**18, 10**18 + i))
        self.saved = B.same_subvolume

    def tearDown(self):
        B.same_subvolume = self.saved
        shutil.rmtree(self.d)

    def restore_test(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return B.test_restore_point(self.snap, self.var, self.live)

    def test_equal_home_passes(self):
        got = self.restore_test()
        self.assertEqual((got["files"], got["sampled"], got["equal"], got["changed_since"]), (10, 10, 10, 0))

    def test_files_changed_since_are_not_failures(self):
        # The machine is in use while start runs: a file written after the snapshot has another size or mtime.
        p = os.path.join(self.live, "u", "d0", "f0")
        pathlib.Path(p).write_bytes(b"new content")
        os.unlink(os.path.join(self.live, "u", "d1", "f1"))
        got = self.restore_test()
        self.assertEqual((got["equal"], got["changed_since"]), (8, 2))

    def test_a_snapshot_that_reads_back_different_fails(self):
        p = os.path.join(self.snap_home, "u", "d2", "f2")
        st = os.stat(p)
        pathlib.Path(p).write_bytes(pathlib.Path(p).read_bytes().replace(b"file", b"FILE"))
        os.utime(p, ns=(st.st_atime_ns, st.st_mtime_ns))
        with self.assertRaisesRegex(B.Failed, "1 of 10 files differ"):
            self.restore_test()

    def test_nothing_compared_fails(self):
        for root, _, fs in os.walk(self.live):
            for n in fs:
                os.utime(os.path.join(root, n), ns=(1, 1))
        with self.assertRaisesRegex(B.Failed, "none of 10 sampled files could be compared"):
            self.restore_test()

    def test_a_snapshot_without_home_fails(self):
        shutil.rmtree(self.snap_home)
        with self.assertRaisesRegex(B.Failed, "does not hold home"):
            self.restore_test()

    def test_an_empty_placeholder_home_fails(self):
        # A nested subvolume shows as an empty directory in a snapshot of its parent, in another subvolume's place.
        B.same_subvolume = lambda top, path: not path.rstrip("/").endswith("/home")
        with self.assertRaisesRegex(B.Failed, "does not hold home"):
            self.restore_test()

    def test_an_empty_home_passes(self):
        for h in (self.snap_home, self.live):
            shutil.rmtree(h)
            os.makedirs(h)
        self.assertEqual(self.restore_test()["files"], 0)

    def test_an_empty_snapshot_of_a_full_home_fails(self):
        shutil.rmtree(self.snap_home)
        os.makedirs(self.snap_home)
        with self.assertRaisesRegex(B.Failed, "holds an empty home, and home is not empty"):
            self.restore_test()

    def test_a_subvolume_nested_in_home_fails(self):
        # Its place in the snapshot is an empty placeholder (inode 2): the snapshot does not hold that part of home.
        B.same_subvolume = lambda top, path: not path.rstrip("/").endswith("/u/d1")
        with self.assertRaisesRegex(B.Failed, "1 subvolume.s. nested in it are empty placeholders"):
            self.restore_test()

    def test_a_link_to_a_directory_elsewhere_is_not_a_placeholder(self):
        # A container layer in home holds absolute links to directories (etc/ssl/certs -> /etc/pki/tls/certs).
        # Followed, they land on another filesystem; the link itself is a name in the snapshot's own subvolume.
        for base in (self.snap_home, self.live):
            os.symlink("/proc", os.path.join(base, "u", "elsewhere"))
        link = os.path.join(self.snap_home, "u", "elsewhere")
        self.assertIs(B.same_subvolume(self.snap, link), False)  # the real check, followed, says "elsewhere"
        got = self.restore_test()
        self.assertEqual((got["files"], got["equal"]), (10, 10))

    def test_a_file_written_while_it_is_read_is_changed_since(self):
        live = os.path.join(self.live, "u", "d2", "f5")
        real = B.sha256_file

        def sha(p):
            h = real(p)
            if p == live:
                pathlib.Path(p).write_bytes(b"written while read\n")
                return "0" * 64
            return h
        B.sha256_file, saved = sha, B.sha256_file
        try:
            got = self.restore_test()
        finally:
            B.sha256_file = saved
        self.assertEqual((got["equal"], got["changed_since"]), (9, 1))

    def test_a_differing_file_name_is_escaped(self):
        name = os.path.join(self.snap_home, "u", "d0", "evil\x1b]0;x\x07")
        for base in (self.snap_home, self.live):
            p = os.path.join(base, os.path.relpath(name, self.snap_home))
            pathlib.Path(p).write_bytes(b"A" if base == self.snap_home else b"B")
            os.utime(p, ns=(10**18, 10**18))
        with self.assertRaises(B.Failed) as e:
            self.restore_test()
        self.assertNotIn("\x1b", str(e.exception))
        self.assertIn("evil\\x1b]0;x\\x07", str(e.exception))


class Undo(unittest.TestCase):
    """`undo`: takes back a finished move from the old (ostree) side, shaped like the first real move (2026-11-11): a
    pre-#164 record (no timers_running), the top level relabelled from apex-root, six units masked, @nixos holding the
    nested subvolumes a NixOS boot makes, custom.cfg with the hook's block AND the Secure Boot key block. Mutants
    undo-* (nix/tests/bridge.nix)."""

    NAMES = ("run", "mount", "STATE", "RUN_DIR", "SYSTEMD_ETC", "undo_facts", "mounted_subvols")
    SB = "### BEGIN rime-secureboot-key ###\nmenuentry 'Enrol the Rime OS Secure Boot key' {\n}\n### END rime-secureboot-key ###\n"
    HOOK = ("### BEGIN rime-boot (written by rime-boot; do not edit inside this block) ###\n"
            "if [ -f ${prefix}/rime-nixos.cfg ]; then\n  source ${prefix}/rime-nixos.cfg\nfi\n### END rime-boot ###\n")
    SUBS = ["@pre-nixos", "@nixos", "@nix", "@home", "@nixos/srv", "@nixos/var/lib/portables",
            "@nixos/var/lib/machines", "@nixos/var/tmp"]

    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.saved = {n: getattr(B, n) for n in self.NAMES}
        B.STATE = os.path.join(self.d, "state")
        B.RUN_DIR = os.path.join(self.d, "run")
        B.SYSTEMD_ETC = os.path.join(self.d, "etc-systemd")
        os.makedirs(B.SYSTEMD_ETC)
        self.top = os.path.join(B.RUN_DIR, "top")
        for s in self.SUBS:
            os.makedirs(os.path.join(self.top, s), exist_ok=True)
        pathlib.Path(self.top, "@home/alex").mkdir()
        pathlib.Path(self.top, "@home/alex/notes.txt").write_text("before the move\n")
        g = os.path.join(self.top, "boot/grub2")
        os.makedirs(g)
        self.user = "menuentry 'Windows' {\n  chainloader /EFI/Microsoft/Boot/bootmgfw.efi\n}\n"
        pathlib.Path(g, "custom.cfg").write_text(self.user + self.HOOK + "\n" + self.SB)
        pathlib.Path(g, "rime-nixos.cfg").write_text("menuentry 'Rime (NixOS)' {}\n")
        pathlib.Path(g, "grub.cfg").write_text("ostree's\n")
        os.makedirs(os.path.join(self.top, "boot/rime/nixos"))
        pathlib.Path(self.top, "boot/rime/nixos/bzImage").write_text("k")
        pathlib.Path(self.top, "boot/loader").mkdir()
        self.masked = list(B.MASK_UNITS)
        for u in self.masked:
            os.symlink("/dev/null", os.path.join(B.SYSTEMD_ETC, u))
        # `done` a little in the future: everything the fixture made is from "before the move finished".
        self.done = (datetime_now() + 30)
        self.rec = {"started": iso(self.done - 600), "done": iso(self.done), "relabelled_from": "apex-root",
                    "masked": self.masked, "steps": ["old side", "fsck gate before", "snapshot @pre-nixos",
                                                     "home (reflink copy)", "install closure", "carry /etc (rime-hostlayer)",
                                                     "first activation and boot entry", "fsck gate after", "done"],
                    "topology": {"topology": "A"}, "backup": None}
        self.write_record()
        self.facts = {"mode": "live", "root": {"device": "/dev/nvme0n1p3", "label": "rime-root", "luks": None},
                      "boot": {"kind": "top-level-dir"}}
        B.undo_facts = lambda: copy.deepcopy(self.facts)
        B.mounted_subvols = lambda *a, **k: []
        self.calls, self.mounts = [], []
        self.fail_delete = None
        self.enabled = {"rime-update-check.timer", "rime-store-compact.timer"}
        B.mount = lambda *a: self.mounts.append(a)
        B.run = self.fake_run

    def tearDown(self):
        for n, v in self.saved.items():
            setattr(B, n, v)
        shutil.rmtree(self.d)

    def fake_run(self, cmd, check=True, capture=True, **kw):
        cmd = list(cmd)
        self.calls.append(cmd)
        out, rc = "", 0
        if cmd[:3] == ["btrfs", "subvolume", "list"]:
            have = [s for s in self.SUBS if os.path.isdir(os.path.join(self.top, s))]
            out = "".join(f"ID {256 + i} gen 9 top level 5 path {s}\n" for i, s in enumerate(have))
        elif cmd[:3] == ["btrfs", "subvolume", "delete"]:
            rel = os.path.relpath(cmd[3], self.top)
            if rel == self.fail_delete:
                rc = 1
            else:
                kids = [s for s in self.SUBS if s.startswith(rel + "/") and os.path.isdir(os.path.join(self.top, s))]
                if kids:
                    rc = 1  # btrfs refuses a subvolume that still holds one
                else:
                    shutil.rmtree(cmd[3])
        elif cmd[:3] == ["systemctl", "is-enabled", "--quiet"]:
            # as systemd answers: a masked unit is `masked` (exit 1), whatever its wants links say
            masked = os.path.islink(os.path.join(B.SYSTEMD_ETC, cmd[3]))
            rc = 0 if cmd[3] in self.enabled and not masked else 1
        if check and rc:
            raise B.subprocess.CalledProcessError(rc, cmd)
        return B.subprocess.CompletedProcess(cmd, rc, out, "")

    def write_record(self):
        os.makedirs(B.STATE, exist_ok=True)
        with open(os.path.join(B.STATE, "state.json"), "w") as f:
            json.dump(self.rec, f)

    def record(self):
        with open(os.path.join(B.STATE, "state.json")) as f:
            return json.load(f)

    def undo(self, yes=True):
        a = B.argparse.Namespace(cmd="undo", yes=yes, disk=[], backup=None, payload=None)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = B.cmd_undo(a)
        return rc, out.getvalue()

    def deleted(self):
        return [os.path.relpath(c[3], self.top) for c in self.calls if c[:3] == ["btrfs", "subvolume", "delete"]]

    def assert_untouched(self):
        self.assertEqual(self.record(), self.rec)
        for s in self.SUBS:
            self.assertTrue(os.path.isdir(os.path.join(self.top, s)), s)
        self.assertIn(self.HOOK, pathlib.Path(self.top, "boot/grub2/custom.cfg").read_text())
        self.assertTrue(os.path.exists(os.path.join(self.top, "boot/grub2/rime-nixos.cfg")))
        self.assertEqual(self.deleted(), [])

    def test_plan_changes_nothing(self):
        rc, out = self.undo(yes=False)
        self.assertEqual(rc, B.EXIT_OK)
        self.assertIn("nothing was changed", out)
        self.assertIn("rime-root -> apex-root", out)
        self.assertEqual(self.mounts[0][:2], ("-o", "ro,subvolid=5"), "a plan mounts the top level read-only")
        self.assert_untouched()
        self.assertTrue(all(os.path.islink(os.path.join(B.SYSTEMD_ETC, u)) for u in self.masked))

    def test_undo_of_the_l16_shaped_move(self):
        rc, out = self.undo()
        self.assertEqual(rc, B.EXIT_OK, out)
        self.assertEqual(self.mounts[0][:2], ("-o", "subvolid=5"))
        # every subvolume, nested ones before the one holding them, the restore point last
        order = self.deleted()
        self.assertEqual(sorted(order), sorted(self.SUBS))
        self.assertEqual(order[-1], "@pre-nixos")
        for kid in ("@nixos/srv", "@nixos/var/lib/portables", "@nixos/var/lib/machines", "@nixos/var/tmp"):
            self.assertLess(order.index(kid), order.index("@nixos"))
        for s in self.SUBS:
            self.assertFalse(os.path.exists(os.path.join(self.top, s)), s)
        # GRUB: the hook's block, menu file and kernels gone; the user's entry and the Secure Boot block byte for byte
        self.assertEqual(pathlib.Path(self.top, "boot/grub2/custom.cfg").read_text(), self.user + "\n" + self.SB)
        self.assertFalse(os.path.lexists(os.path.join(self.top, "boot/grub2/rime-nixos.cfg")))
        self.assertFalse(os.path.lexists(os.path.join(self.top, "boot/rime")))
        self.assertTrue(os.path.isdir(os.path.join(self.top, "boot/loader")))
        self.assertEqual(pathlib.Path(self.top, "boot/grub2/grub.cfg").read_text(), "ostree's\n")
        # label, masks, timers
        self.assertIn(["btrfs", "filesystem", "label", self.top, "apex-root"], self.calls)
        for u in self.masked:
            self.assertFalse(os.path.lexists(os.path.join(B.SYSTEMD_ETC, u)), u)
        for u in B.OLD_SIDE_TIMERS:
            self.assertIn(["systemctl", "start", u], self.calls)
        # the record: rolled back, so every bootc verb and a new start are allowed again
        rec = self.record()
        self.assertIsNone(rec["done"])
        self.assertEqual(rec["done_before_undo"], self.rec["done"])
        self.assertEqual(rec["failed"], B.UNDONE)
        self.assertEqual(rec["steps"][-2:], ["undo", "rolled back"])
        self.assertIsNone(B.record_refusal(B.start_record()))
        self.assertEqual(B.undo_record_refusal(rec)[0], "nothing-to-undo")
        self.assertIn(["umount", self.top], self.calls)

    def test_only_enabled_timers_start_for_an_old_record(self):
        self.enabled = {"rime-update-check.timer"}
        self.undo()
        self.assertIn(["systemctl", "start", "rime-update-check.timer"], self.calls)
        self.assertNotIn(["systemctl", "start", "rime-store-compact.timer"], self.calls)

    def test_a_record_naming_its_timers_is_followed(self):
        self.rec["timers_running"] = ["rime-store-compact.timer"]
        self.write_record()
        self.undo()
        self.assertIn(["systemctl", "start", "rime-store-compact.timer"], self.calls)
        self.assertNotIn(["systemctl", "start", "rime-update-check.timer"], self.calls)

    def test_a_stop_half_way_still_reads_as_moved_and_continues(self):
        self.fail_delete = "@home"
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(B.Failed) as e:
            self.undo()
        self.assertIn("run `rime migrate undo --yes` again", str(e.exception))
        rec = self.record()
        self.assertEqual(rec["done"], self.rec["done"], "the record said not-moved before the undo finished")
        self.assertEqual(rec["steps"][-1], "undo")
        self.assertEqual(B.record_refusal(rec)[0], "already-moved")
        self.assertTrue(os.path.isdir(os.path.join(self.top, "@pre-nixos")), "restore point deleted before the rest")
        self.assertTrue(all(os.path.islink(os.path.join(B.SYSTEMD_ETC, u)) for u in self.masked))
        self.fail_delete, self.calls = None, []
        rc, out = self.undo()
        self.assertEqual(rc, B.EXIT_OK, out)
        self.assertEqual(self.deleted(), ["@home", "@pre-nixos"])
        self.assertEqual(self.record()["steps"][-3:], ["undo", "undo", "rolled back"])
        self.assertIsNone(B.record_refusal(B.start_record()))

    def refused(self, name):
        rc, out = self.undo()
        self.assertEqual(rc, B.EXIT_REFUSED, out)
        self.assertIn(f"REFUSED [{name}]", out)
        self.assert_untouched()
        return out

    def test_refused_when_nixos_was_used(self):
        nb = os.path.join(self.top, "@nixos/var/lib/rime/nixos-bridge")
        os.makedirs(nb)
        pathlib.Path(nb, "good-boots.json").write_text(json.dumps({"boots": [{"boot_id": "a"}]}))
        self.refused("nixos-used")

    def test_refused_when_home_changed_after_the_move(self):
        self.rec["done"] = iso(datetime_now() - 3600)
        self.write_record()
        out = self.refused("home-changed-since-move")
        self.assertIn("@home", out)

    def test_a_deleted_file_counts_as_a_change(self):
        home = os.path.join(self.top, "@home")
        B.time.sleep(0.02)
        t = datetime_now()
        B.time.sleep(0.02)
        self.assertIsNone(B.newest_change(home, t))
        p = pathlib.Path(home, "alex/notes.txt")
        os.utime(p, (0, 0))  # mtime can be set back; ctime cannot, and the utime itself is a change
        self.assertEqual(B.newest_change(home, t), str(p))
        p.unlink()
        self.assertEqual(B.newest_change(home, t), os.path.join(home, "alex"))

    def test_refused_after_finish(self):
        nb = os.path.join(self.top, "@nixos/var/lib/rime/nixos-bridge")
        os.makedirs(nb)
        pathlib.Path(nb, "finish.json").write_text("{}")
        self.refused("finished")

    def test_refused_while_a_subvolume_is_mounted(self):
        B.mounted_subvols = lambda *a, **k: [("@home", "/mnt/x")]
        self.refused("subvolume-in-use")

    def test_refused_off_ostree(self):
        self.facts = {"mode": "not-ostree"}
        with self.assertRaises(B.Refused) as e:
            self.undo()
        self.assertEqual(e.exception.name, "not-live")

    def test_records_undo_does_not_take(self):
        os.unlink(os.path.join(B.STATE, "state.json"))
        with self.assertRaises(B.Refused) as e:
            self.undo()
        self.assertEqual(e.exception.name, "nothing-to-undo")
        self.rec.update(done=None, failed="", steps=["old side", "rolled back"])
        self.write_record()
        with self.assertRaises(B.Refused) as e:
            self.undo()
        self.assertEqual(e.exception.name, "nothing-to-undo")
        self.rec.update(steps=["old side", "install closure"])
        del self.rec["failed"]
        self.write_record()
        with self.assertRaises(B.Refused) as e:
            self.undo()
        self.assertEqual(e.exception.name, "start-unfinished")

    def test_mounted_subvols_reads_mountinfo(self):
        mi = os.path.join(self.d, "mountinfo")
        pathlib.Path(mi).write_text(
            "29 1 0:26 /ostree/deploy/default/deploy/abc.0 /sysroot rw - btrfs /dev/nvme0n1p3 rw\n"
            "30 1 0:26 /@home/alex /mnt/a\\040b rw - btrfs /dev/nvme0n1p3 rw\n"
            "31 1 0:26 /@homework /mnt/c rw - btrfs /dev/nvme0n1p3 rw\n"
            "32 1 0:27 /@nixos /mnt/d rw - btrfs /dev/sdb1 rw\n")
        self.assertEqual(self.saved["mounted_subvols"]({"/dev/nvme0n1p3"}, B.UNDO_SUBVOLS, mi), [("@home", "/mnt/a\\040b")])


def datetime_now():
    return B.time.time()


def iso(t):
    return B.datetime.datetime.fromtimestamp(t, B.datetime.timezone.utc).isoformat()


if __name__ == "__main__":
    unittest.main()
