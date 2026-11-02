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
import re
import shutil
import sys
import tempfile
import time
import unittest

HOOK = None  # the module under test, loaded in __main__
MiB = 1024 * 1024


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

    def toplevel(self, n, kernel=b"kernel-A", initrd=b"initrd-A", params=("console=ttyS0",), signed_kernel=None,
                 ucode=None):
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
        doc = {"org.nixos.bootspec.v1": {
            "init": os.path.join(top, "init"), "kernel": k, "initrd": i,
            "kernelParams": list(params), "label": "NixOS test", "system": "x86_64-linux", "toplevel": top,
        }, "org.nixos.specialisation.v1": {}}
        if ucode:  # nix/modules/boot.nix's org.rimeos.microcode.v1: {vendor: <store>/<vendor>-ucode.img}
            ext = {}
            for vendor, data in ucode.items():
                d = os.path.join(self.store, f"u{hashlib.md5(data).hexdigest()[:6]}-microcode-{vendor}")
                os.makedirs(d, exist_ok=True)
                ext[vendor] = os.path.join(d, f"{vendor}-ucode.img")
                with open(ext[vendor], "wb") as fh:
                    fh.write(data)
            doc["org.rimeos.microcode.v1"] = ext
        with open(os.path.join(top, "boot.json"), "w") as f:
            json.dump(doc, f)
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

    def host_toml(self, text):
        with open(self.host, "w") as fh:
            fh.write(text)

    def initrd_lines(self, text):
        return [l.strip() for l in text.splitlines() if l.strip().startswith("initrd ")]

    def blob_of(self, rel):
        return slurp(os.path.join(self.boot, rel.lstrip("/")))

    def virtual_capacity(self, mib):
        """Make the boot filesystem hold `mib` MiB of file data (free = capacity - what is under boot)."""
        def used():
            return sum(os.lstat(os.path.join(d, n)).st_size for d, _, ns in os.walk(self.boot) for n in ns)
        real = HOOK.free_bytes
        HOOK.free_bytes = lambda path: max(0, int(mib * MiB) - used())
        self.t.addCleanup(setattr, HOOK, "free_bytes", real)

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
        # GRUB globs `*` and `?` and treats a word starting with `#` as a comment: refused too.
        for bad in ("foo=*", "foo=a?b", "#x"):
            g = Fixture(self)
            try:
                g.generation(1, params=("console=ttyS0", bad))
                before = tree(g.boot)
                self.assertEqual(g.run("install"), 3, bad)
                self.assertIn(repr(bad), g.stderr)
                self.assertEqual(tree(g.boot), before)
            finally:
                g.cleanup()
        # host.toml kernel_params must be a list of strings: a bare string is not split into characters.
        for toml in ('kernel_params = "rd.luks.uuid=0b1c"', "kernel_params = [1]"):
            g = Fixture(self)
            try:
                g.generation(1)
                with open(g.host, "w") as h:
                    h.write("[boot]\n" + toml + "\n")
                before = tree(g.boot)
                self.assertEqual(g.run("install"), 3, toml)
                self.assertIn("kernel_params must be a list of strings", g.stderr)
                self.assertEqual(tree(g.boot), before)
            finally:
                g.cleanup()

    # D3 ESP rules (R20): Windows' ESP is refused by name, any letter case (FAT), with nothing written
    def test_refuses_a_windows_esp(self):
        f = self.f
        f.generation(1)
        for parts in (("EFI", "Microsoft", "Boot"), ("efi", "MICROSOFT", "boot")):
            g = Fixture(self)
            try:
                g.generation(1)
                d = os.path.join(g.boot, *parts)
                os.makedirs(d)
                with open(os.path.join(d, "bootmgfw.efi" if parts[0] == "EFI" else "BOOTMGFW.EFI"), "wb") as fh:
                    fh.write(b"MZ windows")
                before = tree(g.boot)
                self.assertEqual(g.run("install"), 3, parts)
                self.assertIn("this is a Windows EFI system partition", g.stderr)
                self.assertIn("Nothing was written", g.stderr)
                self.assertEqual(tree(g.boot), before)
            finally:
                g.cleanup()
        # The bootc installer's custom.cfg Windows ENTRY (a chainloader line) is not Windows' ESP: still written.
        f.make_ostree_boot()
        self.assertEqual(f.run("install"), 0, f.stderr)

    # PLAN.md D2 keymap gate / R19: host.toml [keyboard] console reaches the initrd as rd.vconsole.keymap=, and a
    # LUKS machine whose keymap the generation does not ship is refused by name (checks.keymap-luks is the VM proof).
    def keymap_fixture(self, toml, keymaps=("xkb/gb.map.gz",)):
        g = Fixture(self)
        top = g.generation(1)
        for rel in keymaps:
            km = os.path.join(top, "etc", "kbd", "keymaps", rel)
            os.makedirs(os.path.dirname(km), exist_ok=True)
            with open(km, "wb") as fh:
                fh.write(b"keymap")
        with open(g.host, "w") as h:
            h.write(toml)
        return g

    def entry_options(self, g):
        return [ln for ln in g.read("loader/entries/rime-generation-1.conf").splitlines() if ln.startswith("options ")][0]

    def test_console_keymap_reaches_the_initrd(self):
        cases = [
            # host.toml, keymaps the generation ships, expected rd.vconsole.keymap params
            ('[keyboard]\nconsole = "gb"\n', ("xkb/gb.map.gz",), ["rd.vconsole.keymap=gb"]),
            ('[keyboard]\nconsole = "sg"\n[[luks]]\nname = "root"\ndevice = "UUID=0b1c"\n',
             ("i386/qwertz/sg.map.gz",), ["rd.vconsole.keymap=sg"]),
            ('[keyboard]\nconsole = "us"\n[[luks]]\nname = "root"\ndevice = "UUID=0b1c"\n', (), []),
            ('[keyboard]\nxkb_layout = "gb"\n', ("xkb/gb.map.gz",), []),
            # [boot] kernel_params naming a keymap wins; it is not repeated
            ('[boot]\nkernel_params = ["rd.vconsole.keymap=uk"]\n[keyboard]\nconsole = "gb"\n', ("xkb/gb.map.gz",),
             ["rd.vconsole.keymap=uk"]),
            # not shipped, no LUKS: a warning, the console stays on us
            ('[keyboard]\nconsole = "gb"\n', (), []),
        ]
        for toml, keymaps, want in cases:
            g = self.keymap_fixture(toml, keymaps)
            try:
                self.assertEqual(g.run("install"), 0, (toml, g.stderr))
                got = [w for w in self.entry_options(g).split() if w.startswith("rd.vconsole.keymap=")]
                self.assertEqual(got, want, toml)
                if not keymaps and "gb" in toml:
                    self.assertIn("warning: [keyboard] console = 'gb' is not a keymap of generation 1", g.stderr)
            finally:
                g.cleanup()

    def test_luks_machine_without_its_keymap_is_refused(self):
        for toml in ('[keyboard]\nconsole = "ch"\n[[luks]]\nname = "root"\ndevice = "UUID=0b1c"\n',
                     '[boot]\nkernel_params = ["rd.luks.uuid=0b1c"]\n[keyboard]\nconsole = "ch"\n'):
            g = self.keymap_fixture(toml, ("xkb/gb.map.gz",))
            try:
                before = tree(g.boot)
                self.assertEqual(g.run("install"), 3, toml)
                self.assertIn("console = 'ch' is not a keymap of generation 1", g.stderr)
                self.assertIn("passphrase prompt would read the us layout", g.stderr)
                self.assertEqual(tree(g.boot), before)
            finally:
                g.cleanup()
        for bad in ("g b", "$x", "../gb"):
            g = self.keymap_fixture('[keyboard]\nconsole = "%s"\n' % bad)
            try:
                self.assertEqual(g.run("install"), 3, bad)
                self.assertIn("is not a keymap name", g.stderr)
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

    # ── early microcode as separate initrd files (org.rimeos.microcode.v1, host.toml [boot] microcode) ──
    def test_microcode_split_dedupe_and_vendor(self):
        f = self.f
        uc = {"intel": b"INTEL-UCODE", "amd": b"AMD-UCODE"}
        f.generation(1, kernel=b"K1", initrd=b"I1", ucode=uc)
        f.generation(2, kernel=b"K2", initrd=b"I2", ucode=uc)
        self.assertEqual(f.run("install"), 0, f.stderr)
        e2 = f.read("loader/entries/rime-generation-2.conf")
        got = [f.blob_of(l.split()[1]) for l in f.initrd_lines(e2)]
        self.assertEqual(got, [b"INTEL-UCODE", b"AMD-UCODE", b"I2"])  # one line per file, microcode first
        # each microcode file is stored once for both generations
        self.assertEqual(len(f.nixos_files()), 6)
        e1 = f.read("loader/entries/rime-generation-1.conf")
        self.assertEqual(f.initrd_lines(e1)[:2], f.initrd_lines(e2)[:2])
        self.assertEqual(f.run("check"), 0, f.stdout)
        # the CPU's vendor only: the other vendor's file leaves the ESP
        f.host_toml('[boot]\nmicrocode = "intel"\n')
        self.assertEqual(f.run("install"), 0, f.stderr)
        e2 = f.read("loader/entries/rime-generation-2.conf")
        self.assertEqual([f.blob_of(l.split()[1]) for l in f.initrd_lines(e2)], [b"INTEL-UCODE", b"I2"])
        self.assertEqual(len(f.nixos_files()), 5)
        f.host_toml('[boot]\nmicrocode = "none"\n')
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(len(f.initrd_lines(f.read("loader/entries/rime-generation-2.conf"))), 1)
        self.assertEqual(len(f.nixos_files()), 4)
        # anything else is refused by name, before a write
        f.host_toml('[boot]\nmicrocode = "arm"\n')
        before = tree(f.boot)
        self.assertEqual(f.run("install"), 3)
        self.assertIn("[boot] microcode must be one of both, intel, amd, none", f.stderr)
        self.assertEqual(tree(f.boot), before)

    def test_microcode_custom_cfg_one_line_and_check_sees_every_file(self):
        f = self.f
        f.cfg["layout"] = "custom-cfg"
        f.generation(1, kernel=b"K1", initrd=b"I1", ucode={"intel": b"INTEL-UCODE", "amd": b"AMD-UCODE"})
        self.assertEqual(f.run("install"), 0, f.stderr)
        lines = f.initrd_lines(f.read("grub2/rime-nixos.cfg"))
        self.assertEqual(len(lines), 1, lines)
        self.assertEqual([f.blob_of(p) for p in lines[0].split()[1:]], [b"INTEL-UCODE", b"AMD-UCODE", b"I1"])
        self.assertEqual(f.run("check"), 0, f.stdout)
        # check names a missing file even when it is the 2nd path on the line
        amd = lines[0].split()[2]
        os.unlink(os.path.join(f.boot, amd.lstrip("/")))
        self.assertEqual(f.run("check"), 1)
        self.assertEqual([os.path.basename(m) for m in json.loads(f.stdout)["missing"]], [os.path.basename(amd)])

    def test_generation_without_microcode_extension_has_one_initrd(self):
        f = self.f
        f.generation(1, kernel=b"K1", initrd=b"I1")
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(len(f.initrd_lines(f.read("loader/entries/rime-generation-1.conf"))), 1)

    # ── space: prune to fit, never run out (docs/nixos/boot-matrix.md "ESP budget") ──
    def big(self, n, **kw):
        """Generation n with a 1 MiB kernel and a 1 MiB initrd of its own."""
        return self.f.generation(n, kernel=bytes([n]) * MiB, initrd=bytes([100 + n]) * MiB, **kw)

    def test_prune_leaves_out_the_oldest_generations_that_do_not_fit(self):
        f = self.f
        f.cfg["reserveMiB"] = 1
        for n in range(1, 5):
            self.big(n)
        f.virtual_capacity(1 + 4.5)  # the floor + two generations
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.entries(), ["rime-generation-3.conf", "rime-generation-4.conf"])
        self.assertIn("generation 1 left out of the menu", f.stderr)
        self.assertIn("generation 2 left out of the menu", f.stderr)
        self.assertEqual(f.run("check"), 0, f.stdout)

    def test_prune_reclaims_through_a_bootable_menu(self):
        f = self.f
        f.cfg["reserveMiB"] = 1
        f.virtual_capacity(1 + 4.5)
        self.big(1)
        top2 = self.big(2)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertEqual(f.entries(), ["rime-generation-1.conf", "rime-generation-2.conf"])
        os.symlink(top2, f.cfg["bootedSystem"])  # running generation 2 (mandatory)
        self.big(3)
        self.big(4)
        # Making room removes only Rime's own files: another loader's entry, Fedora's shim directory and a file at
        # the filesystem's root survive it, byte for byte.
        foreign = {os.path.join(f.boot, "loader", "entries", "fedora-other.conf"): b"title someone else's\n",
                   os.path.join(f.boot, "EFI", "fedora", "shimx64.efi"): b"shim",
                   os.path.join(f.boot, "stray.bin"): b"stray"}
        for p, data in foreign.items():
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as fh:
                fh.write(data)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn("making room: menu of generation(s) 2", f.stderr)
        for p, data in foreign.items():
            self.assertEqual(slurp(p), data, p)
        self.assertEqual(f.entries(), ["fedora-other.conf", "rime-generation-2.conf", "rime-generation-4.conf"])
        self.assertIn("set default=rime-generation-4\n", f.read("grub2/rime-nixos.cfg"))
        self.assertEqual(len(f.nixos_files()), 4)
        self.assertEqual(f.run("check"), 0, f.stdout)

    def test_prune_keeps_a_file_a_kept_generation_shares_with_a_dropped_one(self):
        f = self.f
        f.cfg["reserveMiB"] = 1
        f.virtual_capacity(1 + 4.5)
        self.big(1)
        top2 = self.big(2)
        self.assertEqual(f.run("install"), 0, f.stderr)
        os.symlink(top2, f.cfg["bootedSystem"])
        self.big(3)
        f.generation(4, kernel=bytes([1]) * MiB, initrd=bytes([104]) * MiB)  # generation 1's kernel, a new initrd
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.assertIn("making room", f.stderr)
        self.assertEqual(f.entries(), ["rime-generation-2.conf", "rime-generation-4.conf"])
        self.assertEqual(f.run("check"), 0, f.stdout)  # the shared kernel was not removed while making room

    def interrupted_after_making_room(self, layout):
        f = self.f
        f.cfg["layout"] = layout
        f.cfg["reserveMiB"] = 1
        f.virtual_capacity(1 + 4.5)
        self.big(1)
        top2 = self.big(2)
        self.assertEqual(f.run("install"), 0, f.stderr)
        os.symlink(top2, f.cfg["bootedSystem"])
        self.big(3)
        self.big(4)
        real = HOOK.atomic_copy

        def dies(src, dst):
            raise OSError(5, "Input/output error (simulated power cut)")

        HOOK.atomic_copy = dies
        try:
            self.assertEqual(f.run("install"), 2)
        finally:
            HOOK.atomic_copy = real
        # what a power cut here would boot: a menu of generation 2 alone, every file it names present
        menu = f.read("grub2/rime-nixos.cfg")
        self.assertIn("set default=rime-generation-2\n", menu)
        if layout == "bls":
            self.assertEqual(f.entries(), ["rime-generation-2.conf"])
        else:
            self.assertEqual(re.findall(r"--id (rime-generation-\d+)", menu), ["rime-generation-2"])
        f.run("check")
        self.assertEqual(json.loads(f.stdout)["missing"], [])
        self.assertEqual(f.run("install"), 0, f.stderr)  # the next run finishes the job
        self.assertEqual(f.run("check"), 0, f.stdout)
        self.assertEqual(json.loads(f.stdout)["default_generation"], 4)

    def test_prune_interrupted_after_making_room_still_boots_bls(self):
        self.interrupted_after_making_room("bls")

    def test_prune_interrupted_after_making_room_still_boots_custom_cfg(self):
        self.interrupted_after_making_room("custom-cfg")

    def test_prune_refuses_when_current_booted_and_pinned_do_not_fit(self):
        f = self.f
        f.cfg["reserveMiB"] = 1
        f.virtual_capacity(1 + 3)
        self.big(1)
        self.big(2)
        os.makedirs(f.pins)
        os.symlink("/dev/null", os.path.join(f.pins, "pin-1"))
        before = tree(f.boot)
        self.assertEqual(f.run("install"), 3)
        self.assertIn("current, booted and pinned generation(s) 2, 1", f.stderr)
        self.assertIn("Nothing was written", f.stderr)
        self.assertEqual(tree(f.boot), before)

    def test_prune_refuses_to_make_room_when_nothing_kept_is_on_disk(self):
        f = self.f
        f.cfg["reserveMiB"] = 1
        f.virtual_capacity(1 + 2.5)
        self.big(1)
        self.assertEqual(f.run("install"), 0, f.stderr)
        self.big(2)
        os.unlink(os.path.join(f.profiles, "system-1-link"))  # deleted, so only 2 (not on disk) is kept
        before = tree(f.boot)
        self.assertEqual(f.run("install"), 3)
        self.assertIn("making room would leave nothing to boot", f.stderr)
        self.assertEqual(tree(f.boot), before)

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
