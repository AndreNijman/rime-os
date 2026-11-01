"""Unit tests of nix/pkgs/rime-boot/rime-boot.py against fake profiles, toplevels and boot filesystems.

Run by checks.x86_64-linux.boot-hook-unit (nix/tests/boot.nix) as
    python3 -I boot-hook-test.py <path to rime-boot>
and runnable by hand the same way. Every test builds its own tree under a temporary directory.
"""
import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import sys
import tempfile
import time
import unittest

HOOK = None  # the module under test, loaded in __main__


def load_hook(path):
    loader = importlib.machinery.SourceFileLoader("rime_boot", path)
    spec = importlib.util.spec_from_loader("rime_boot", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


def slurp(path, mode="rb"):
    with open(path, mode) as f:
        return f.read()


def sha(path):
    return hashlib.sha256(slurp(path)).hexdigest()


def tree(root):
    """{relative path: ('l', target) | ('f', sha256, inode, mtime_ns)} of everything under root."""
    out = {}
    for d, dirs, files in os.walk(root):
        for n in dirs + files:
            p = os.path.join(d, n)
            rel = os.path.relpath(p, root)
            if os.path.islink(p):
                out[rel] = ("l", os.readlink(p))
            elif os.path.isfile(p):
                st = os.stat(p)
                out[rel] = ("f", sha(p), st.st_ino, st.st_mtime_ns)
    return out


class Fixture:
    def __init__(self, t):
        self.t = t
        self.root = tempfile.mkdtemp(prefix="rime-boot-test-")
        self.store = os.path.join(self.root, "store")
        self.profiles = os.path.join(self.root, "profiles")
        self.boot = os.path.join(self.root, "boot")
        self.pins = os.path.join(self.root, "gcroots-rime")
        self.host = os.path.join(self.root, "host.toml")
        for d in (self.store, self.profiles, os.path.join(self.boot, "grub2")):
            os.makedirs(d)
        self.cfg = {
            "layout": "auto",
            "bootDir": self.boot,
            "profileDir": self.profiles,
            "profile": "system",
            "hostToml": self.host,
            "pinDir": self.pins,
            "bootedSystem": os.path.join(self.root, "booted-system"),
            "configurationLimit": 10,
            "timeout": 5,
            "reserveMiB": 0,
            "grubPath": "",
        }

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def blob(self, name, content):
        d = os.path.join(self.store, name)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, "bzImage" if "linux" in name else "initrd")
        with open(p, "wb") as f:
            f.write(content)
        return p

    def toplevel(self, n, kernel=b"kernel-A", initrd=b"initrd-A", params=("console=ttyS0",), signed_kernel=None):
        """A fake toplevel. `signed_kernel`: <toplevel>/kernel points at a different (signed) image than bootspec's
        `kernel`, the way nix/modules/kernel.nix relinks it."""
        top = os.path.join(self.store, f"top{n}-nixos-system")
        os.makedirs(top, exist_ok=True)
        k = self.blob(f"k{hashlib.md5(kernel).hexdigest()[:6]}-linux-7.3", kernel)
        i = self.blob(f"i{hashlib.md5(initrd).hexdigest()[:6]}-initrd-linux-7.3", initrd)
        target = k
        if signed_kernel is not None:
            target = self.blob(f"s{hashlib.md5(signed_kernel).hexdigest()[:6]}-linux-image-signed", signed_kernel)
        for name, dst in (("kernel", target), ("initrd", i)):
            p = os.path.join(top, name)
            if os.path.lexists(p):
                os.unlink(p)
            os.symlink(dst, p)
        with open(os.path.join(top, "init"), "w") as f:
            f.write("#!/bin/sh\n")
        with open(os.path.join(top, "boot.json"), "w") as f:
            json.dump({"org.nixos.bootspec.v1": {
                "init": os.path.join(top, "init"), "kernel": k, "initrd": i,
                "kernelParams": list(params), "label": "NixOS test", "system": "x86_64-linux", "toplevel": top,
            }, "org.nixos.specialisation.v1": {}}, f)
        return top

    def generation(self, n, current=True, **kw):
        top = self.toplevel(n, **kw)
        link = os.path.join(self.profiles, f"system-{n}-link")
        if os.path.lexists(link):
            os.unlink(link)
        os.symlink(top, link)
        if current:
            self.set_current(n)
        return top

    def set_current(self, n):
        p = os.path.join(self.profiles, "system")
        if os.path.lexists(p):
            os.unlink(p)
        os.symlink(f"system-{n}-link", p)

    def write_cfg(self):
        p = os.path.join(self.root, "cfg.json")
        with open(p, "w") as f:
            json.dump(self.cfg, f)
        return p

    def run(self, *args):
        err, out = io.StringIO(), io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(out):
            rc = HOOK.main(["--config", self.write_cfg(), *args])
        self.stderr, self.stdout = err.getvalue(), out.getvalue()
        return rc

    def read(self, rel):
        return slurp(os.path.join(self.boot, rel), "r")

    def entries(self):
        d = os.path.join(self.boot, "loader", "entries")
        return sorted(os.listdir(d)) if os.path.isdir(d) else []

    def nixos_files(self):
        d = os.path.join(self.boot, "rime", "nixos")
        return sorted(os.listdir(d)) if os.path.isdir(d) else []

    def make_ostree_boot(self):
        """bootc's /boot: loader -> loader.1 with ostree's entry, an ostree dir, and the bootc installer's Windows
        entry already in custom.cfg."""
        os.makedirs(os.path.join(self.boot, "loader.1", "entries"))
        os.makedirs(os.path.join(self.boot, "ostree", "default-0123"))
        with open(os.path.join(self.boot, "ostree", "default-0123", "vmlinuz"), "w") as f:
            f.write("bootc kernel")
        os.symlink("loader.1", os.path.join(self.boot, "loader"))
        with open(os.path.join(self.boot, "loader.1", "entries", "ostree-1.conf"), "w") as f:
            f.write("title Fedora Linux 45 (ostree:0)\nversion 1\nlinux /boot/ostree/default-0123/vmlinuz\n")
        self.windows = "menuentry 'Windows' --id rime-windows {\n  chainloader /EFI/Microsoft/Boot/bootmgfw.efi\n}\n"
        with open(os.path.join(self.boot, "grub2", "custom.cfg"), "w") as f:
            f.write(self.windows)


class HookTest(unittest.TestCase):
    def setUp(self):
        self.f = Fixture(self)

    def tearDown(self):
        self.f.cleanup()

    # (a) fresh install: BLS entries, content-addressed files, default = the profile's current generation
    def test_bls_entries_default_and_files(self):
        f = self.f
        f.generation(1, kernel=b"K1", initrd=b"I1")
        top2 = f.generation(2, kernel=b"K2", initrd=b"I2", params=("console=ttyS0", "quiet"))
        self.assertEqual(f.run("install", top2), 0, f.stderr)
        self.assertEqual(f.entries(), ["rime-generation-1.conf", "rime-generation-2.conf"])
        e2 = f.read("loader/entries/rime-generation-2.conf")
        self.assertRegex(e2, r"^title Rime \(generation 2, \d{4}-\d\d-\d\d \d\d:\d\d\)\n")
        self.assertIn(f"options init={top2}/init console=ttyS0 quiet\n", e2)
        menu = f.read("grub2/rime-nixos.cfg")
        self.assertIn("set default=rime-generation-2\n", menu)
        self.assertIn("set timeout_style=menu\nset timeout=5\n", menu)
        self.assertNotIn("menuentry", menu)  # blscfg lists the entries in this layout
        self.assertEqual(f.read("grub2/custom.cfg").count("source ${prefix}/rime-nixos.cfg"), 1)
        # content-addressed: <sha256[:32]>-..., and the bytes are the source's
        names = f.nixos_files()
        self.assertEqual(len(names), 4)
        for line in e2.splitlines():
            if line.startswith(("linux ", "initrd ")):
                name = os.path.basename(line.split()[1])
                data = slurp(os.path.join(f.boot, "rime", "nixos", name))
                self.assertTrue(name.startswith(hashlib.sha256(data).hexdigest()[:32] + "-"), name)
                self.assertIn(data, (b"K2", b"I2"))
        self.assertIn("linux /rime/nixos/", e2)  # grubPath "" = ESP root

    def test_default_follows_profile_rollback(self):
        f = self.f
        f.generation(1, kernel=b"K1")
        f.generation(2, kernel=b"K2")
        f.set_current(1)  # what `rime rollback` / `nix-env --switch-generation 1` does before running the hook
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn("set default=rime-generation-1\n", f.read("grub2/rime-nixos.cfg"))
        self.assertEqual(f.entries(), ["rime-generation-1.conf", "rime-generation-2.conf"])

    def test_signed_kernel_link_wins_over_bootspec(self):
        f = self.f
        top = f.generation(1, kernel=b"UNSIGNED", signed_kernel=b"MZ-signed-image")
        self.assertEqual(f.run("install", top), 0, f.stderr)
        e = f.read("loader/entries/rime-generation-1.conf")
        kname = [l.split()[1] for l in e.splitlines() if l.startswith("linux ")][0]
        self.assertEqual(slurp(os.path.join(f.boot, kname.lstrip("/"))), b"MZ-signed-image")

    # GC: entries and files of deleted generations go; foreign entries stay
    def test_gc_removes_only_rime_leftovers(self):
        f = self.f
        f.generation(1, kernel=b"K1", initrd=b"I1")
        f.generation(2, kernel=b"K2", initrd=b"I2")
        os.makedirs(os.path.join(f.boot, "loader", "entries"), exist_ok=True)
        foreign = os.path.join(f.boot, "loader", "entries", "fedora-other.conf")
        with open(foreign, "w") as fh:
            fh.write("title someone else's\n")
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(len(f.nixos_files()), 4)
        os.unlink(os.path.join(f.profiles, "system-1-link"))  # nix-env --delete-generations 1
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.entries(), ["fedora-other.conf", "rime-generation-2.conf"])
        self.assertEqual(slurp(foreign, "r"), "title someone else's\n")
        left = [slurp(os.path.join(f.boot, "rime", "nixos", n)) for n in f.nixos_files()]
        self.assertEqual(sorted(left), [b"I2", b"K2"])

    def test_limit_keeps_current_booted_and_pinned(self):
        f = self.f
        for n in range(1, 7):
            f.generation(n, kernel=b"K%d" % n)
        f.cfg["configurationLimit"] = 2
        os.makedirs(f.pins)
        os.symlink("/dev/null", os.path.join(f.pins, "pin-1"))
        os.symlink(os.path.realpath(os.path.join(f.profiles, "system-2-link")), f.cfg["bootedSystem"])
        f.set_current(3)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.entries(), [f"rime-generation-{n}.conf" for n in (1, 2, 3, 5, 6)])
        self.assertIn("set default=rime-generation-3\n", f.read("grub2/rime-nixos.cfg"))

    def test_idempotent_second_run_changes_nothing(self):
        f = self.f
        f.generation(1)
        f.generation(2, kernel=b"K2")
        self.assertEqual(f.run("install"), 0, f.stderr)
        before = tree(f.boot)
        time.sleep(0.02)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(tree(f.boot), before)

    # (e) the floor: refuse by name, write nothing
    def test_space_floor_refuses_and_writes_nothing(self):
        f = self.f
        f.generation(1)
        f.cfg["reserveMiB"] = 1 << 40  # a floor no filesystem has
        before = tree(f.boot)
        self.assertEqual(f.run("install"), 3)
        self.assertIn("REFUSED: not enough space on", f.stderr)
        self.assertIn("Nothing was written", f.stderr)
        self.assertEqual(tree(f.boot), before)

    def test_floor_does_not_block_when_nothing_new_is_copied(self):
        f = self.f
        f.generation(1)
        self.assertEqual(f.run("install"), 0, f.stderr)
        f.cfg["reserveMiB"] = 1 << 40
        f.set_current(1)
        self.assertEqual(f.run("install"), 0, f.stderr)  # a rollback needs no new files

    def test_failure_mid_copy_leaves_the_old_menu(self):
        f = self.f
        f.generation(1, kernel=b"K1", initrd=b"I1")
        self.assertEqual(f.run("install"), 0, f.stderr)
        before_menu = f.read("grub2/rime-nixos.cfg")
        before_entries = f.entries()
        f.generation(2, kernel=b"K2", initrd=b"I2")
        real, calls = HOOK.atomic_copy, []

        def flaky(src, dst):
            calls.append(src)
            if len(calls) == 2:
                raise OSError(28, "No space left on device (simulated)")
            real(src, dst)

        HOOK.atomic_copy = flaky
        try:
            self.assertEqual(f.run("install"), 2)
        finally:
            HOOK.atomic_copy = real
        self.assertEqual(f.read("grub2/rime-nixos.cfg"), before_menu)
        self.assertEqual(f.entries(), before_entries)
        self.assertEqual(f.run("install"), 0, f.stderr)  # the next run recovers (stale temp files swept)
        self.assertNotIn(".rime-boot-tmp-", " ".join(f.nixos_files()))
        self.assertIn("set default=rime-generation-2\n", f.read("grub2/rime-nixos.cfg"))

    # (b) migrated bootc machine: custom.cfg, ostree untouched
    def test_custom_cfg_on_ostree_boot(self):
        f = self.f
        f.make_ostree_boot()
        f.cfg["grubPath"] = "/boot"
        f.generation(1, kernel=b"K1")
        f.generation(2, kernel=b"K2")
        f.generation(3, kernel=b"K3")
        f.set_current(2)
        ostree_before = {k: v for k, v in tree(f.boot).items() if k.startswith(("loader", "ostree"))}
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn("layout custom-cfg", f.stderr)
        ostree_after = {k: v for k, v in tree(f.boot).items() if k.startswith(("loader", "ostree"))}
        self.assertEqual(ostree_after, ostree_before)
        menu = f.read("grub2/rime-nixos.cfg")
        ids = [l.split("--id ")[1].split()[0] for l in menu.splitlines() if l.startswith("menuentry")]
        self.assertEqual(ids, ["rime-generation-2", "rime-generation-3", "rime-generation-1"])  # default first
        self.assertIn("menuentry 'Rime (generation 1, ", menu)
        self.assertIn("  linux /boot/rime/nixos/", menu)
        self.assertIn("set default=rime-generation-2\n", menu)
        custom = f.read("grub2/custom.cfg")
        self.assertTrue(custom.startswith(f.windows), custom)  # the Windows entry, byte for byte
        self.assertEqual(custom.count(HOOK.BLOCK_BEGIN), 1)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.read("grub2/custom.cfg"), custom)

    def test_host_toml_layout_wins_and_ostree_refuses_bls(self):
        f = self.f
        f.generation(1)
        f.cfg["layout"] = "bls"
        with open(f.host, "w") as h:
            h.write('[boot]\nlayout = "custom-cfg"\n')
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn("menuentry 'Rime (generation 1", f.read("grub2/rime-nixos.cfg"))
        self.assertEqual(f.entries(), [])
        g = Fixture(self)
        try:
            g.make_ostree_boot()
            g.generation(1)
            with open(g.host, "w") as h:
                h.write('[boot]\nlayout = "bls"\n')
            before = tree(g.boot)
            self.assertEqual(g.run("install"), 3)
            self.assertIn("refusing to write BLS entries there", g.stderr)
            self.assertEqual(tree(g.boot), before)
        finally:
            g.cleanup()

    def test_host_kernel_params_and_unsafe_params(self):
        f = self.f
        f.generation(1)
        with open(f.host, "w") as h:
            h.write('[boot]\nkernel_params = ["rd.luks.uuid=0b1c", "rd.vconsole.keymap=gb"]\n')
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn(" console=ttyS0 rd.luks.uuid=0b1c rd.vconsole.keymap=gb\n",
                      f.read("loader/entries/rime-generation-1.conf"))
        g = Fixture(self)
        try:
            g.generation(1, params=("console=ttyS0", "foo=$bar"))
            before = tree(g.boot)
            self.assertEqual(g.run("install"), 3)
            self.assertIn("'foo=$bar'", g.stderr)
            self.assertEqual(tree(g.boot), before)
        finally:
            g.cleanup()

    def test_refuses_without_grub(self):
        f = self.f
        f.generation(1)
        os.rmdir(os.path.join(f.boot, "grub2"))
        self.assertEqual(f.run("install"), 3)
        self.assertIn("rime-boot never installs GRUB", f.stderr)

    # D6 8.7: read-only --check
    def test_check(self):
        f = self.f
        f.generation(1, kernel=b"K1")
        f.generation(2, kernel=b"K2")
        self.assertEqual(f.run("check"), 1)  # no menu yet
        self.assertEqual(f.run("install"), 0, f.stderr)
        before = tree(f.boot)
        self.assertEqual(f.run("check"), 0, f.stdout)
        out = json.loads(f.stdout)
        self.assertEqual((out["default_generation"], out["profile_generation"], out["up_to_date"]), (2, 2, True))
        self.assertEqual(out["default_toplevel"], os.path.realpath(os.path.join(f.profiles, "system-2-link")))
        self.assertEqual(tree(f.boot), before)
        f.generation(3, kernel=b"K3")  # profile moved, hook not run (interrupted update)
        self.assertEqual(f.run("check"), 1)
        self.assertFalse(json.loads(f.stdout)["up_to_date"])
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.run("check"), 0)
        for n in f.nixos_files():
            os.unlink(os.path.join(f.boot, "rime", "nixos", n))
        self.assertEqual(f.run("check"), 1)
        self.assertTrue(json.loads(f.stdout)["missing"])

    def test_grub_path_from_mountinfo(self):
        f = self.f
        f.cfg["grubPath"] = None
        mi = os.path.join(f.root, "mountinfo")
        f.cfg["mountinfo"] = mi
        boot = os.path.realpath(f.boot)
        cases = [
            ("24 1 0:22 / / rw - ext4 /dev/vda2 rw\n30 24 0:30 / %s rw - vfat /dev/vda1 rw\n" % boot, ""),
            ("24 1 0:22 / / rw - ext4 /dev/vda2 rw\n31 24 0:31 /boot %s rw - btrfs /dev/vda2 rw\n" % boot, "/boot"),
            ("24 1 0:22 / %s rw - btrfs /dev/vda2 rw\n" % os.path.dirname(boot), "/boot"),
            ("24 1 0:22 /@nixos / rw - btrfs /dev/vda2 rw\n", "/@nixos" + boot),
        ]
        for text, want in cases:
            with open(mi, "w") as fh:
                fh.write(text)
            self.assertEqual(HOOK.grub_path_of(f.cfg), want, text)


if __name__ == "__main__":
    HOOK = load_hook(sys.argv.pop(1))
    unittest.main(verbosity=2)
