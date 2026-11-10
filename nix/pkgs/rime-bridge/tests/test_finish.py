"""rime-migrate-finish's unit tests: every precondition refused by name, --dry-run writes nothing, --yes deletes the
old side and nothing else, an interrupted run is completed by the next, a second run is refused. Run against a fixture
machine (FakeSystem: a temporary directory laid out as a moved machine is, with btrfs's subvolume calls emulated), by
the package's installCheckPhase against the installed program (RIME_MIGRATE_FINISH; the bridge beside it).
"""

import contextlib
import errno
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
PROG = os.environ.get("RIME_MIGRATE_FINISH", os.path.join(HERE, "..", "rime-migrate-finish.py"))


def load():
    loader = importlib.machinery.SourceFileLoader("rime_migrate_finish", PROG)
    mod = importlib.util.module_from_spec(importlib.util.spec_from_loader("rime_migrate_finish", loader))
    loader.exec_module(mod)
    return mod


F = load()
SRC = "/dev/vda3"
RIME_BOOT = ("### BEGIN rime-boot (written by rime-boot; do not edit inside this block) ###\n"
             "if [ -f ${prefix}/rime-nixos.cfg ]; then\n  source ${prefix}/rime-nixos.cfg\nfi\n### END rime-boot ###\n")
SB_KEY = ("### BEGIN rime-secureboot-key ###\nmenuentry 'Enroll the Rime Secure Boot key' --id rime-secureboot-key {\n"
          "  chainloader /EFI/fedora/mmx64.efi\n}\n### END rime-secureboot-key ###\n")
WINDOWS = "menuentry 'Windows' {\n  search --fs-uuid 1234-ABCD --set root\n  chainloader /EFI/Microsoft/Boot/bootmgfw.efi\n}\n"
STATIC = ('source ${config_directory}/bootuuid.cfg\nsearch --fs-uuid "${BOOT_UUID}" --set prefix\n'
          "blscfg\nsource $prefix/custom.cfg\n")
DONE = {"started": "2026-11-04T10:00:00+00:00", "done": "2026-11-04T10:20:00+00:00",
        "steps": ["backup", "old side", "fsck gate before", "snapshot @pre-nixos", "home (reflink copy)",
                  "install closure", "carry /etc (rime-hostlayer)", "first activation and boot entry",
                  "fsck gate after", "done"]}


def jload(p):
    with open(p) as f:
        return json.load(f)


def slurp(p):
    with open(p, "rb") as f:
        return f.read()


def w(path, text="", mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def mountinfo(lines):
    return "".join(f"{i + 20} 1 0:30 {root} {target} rw,relatime shared:1 - {fs} {src} rw\n"
                   for i, (root, target, fs, src) in enumerate(lines))


GOOD_MOUNTS = [("/@nixos", "/", "btrfs", SRC), ("/@nix", "/nix", "btrfs", SRC), ("/@home", "/var/home", "btrfs", SRC),
               ("/", "/mnt/rime-top", "btrfs", SRC), ("/boot", "/boot", "btrfs", SRC)]


class Interrupt(BaseException):
    """What a power cut or a Ctrl-C looks like to the program: nothing after it runs."""


class FakeSystem(F.System):
    """A moved machine in a directory: R/top is the btrfs top level (subvolumes are directories listed in self.subs),
    R/top/boot is /boot (the plain layout), R/top/@nixos/var/lib/rime/nixos-bridge is the state directory."""

    def __init__(self, root):
        super().__init__()
        self.r = root
        top = os.path.join(root, "top")
        self.top = top
        self.mountinfo = os.path.join(root, "proc-self-mountinfo")
        self.cmdline = os.path.join(root, "cmdline")
        self.nixos_marker = os.path.join(root, "etc/NIXOS")
        self.ostree_booted = os.path.join(root, "run/ostree-booted")
        self.proc = os.path.join(root, "proc")
        self.sys_block = os.path.join(root, "sys-block")
        self.swaps = os.path.join(root, "swaps")
        self.boot = os.path.join(top, "boot")
        self.state = os.path.join(top, "@nixos/var/lib/rime/nixos-bridge")
        self.boot_id = os.path.join(root, "boot_id")
        self.booted_system = os.path.join(root, "booted-system")
        self.self_pid = 1
        self.subs = {"@pre-nixos": "u-pre", "@nixos": "u-nixos", "@nix": "u-nix", "@home": "u-home",
                     "ostree/deploy/default/var/lib/containers/storage/btrfs/subvolumes/aaa": "u-c1",
                     "ostree/deploy/default/var/lib/containers/storage/btrfs/subvolumes/aaa/inner": "u-c2"}
        self.stats = {}
        self.interrupt_at = None
        self.hook = None
        self.mounted = []
        self.deleted = []
        self.commits = []
        # read-only subvolumes: btrfs refuses (EROFS) to move one into another directory, as the OVMF run found
        self.readonly = {"@pre-nixos"}
        self.build()

    def build(self):
        r, top = self.r, self.top
        w(self.mountinfo, mountinfo(GOOD_MOUNTS))
        w(self.cmdline, "init=/nix/store/x-nixos-system/init root=fstab quiet\n")
        w(self.nixos_marker)
        w(self.boot_id, "11111111-2222-3333-4444-555555555555\n")
        for i in range(F.GOOD_BOOTS):
            pass
        w(os.path.join(self.state, "good-boots.json"), json.dumps(
            {"boots": [{"boot_id": f"b{i}", "at": "x", "system": None} for i in range(F.GOOD_BOOTS)]}))
        # the old side
        var = os.path.join(top, "ostree/deploy/default/var")
        w(os.path.join(var, "lib/rime/nixos-bridge/state.json"), json.dumps(DONE))
        w(os.path.join(var, "home/alex/notes.txt"), "old notes\n")
        w(os.path.join(var, "lib/containers/storage/btrfs/subvolumes/aaa/inner/layer.bin"), "layer")
        w(os.path.join(top, "ostree/deploy/default/deploy/abc.0/usr/bin/sh"), "sh")
        w(os.path.join(top, "ostree/repo/objects/ab/cdef.file"), "object")
        w(os.path.join(top, "@pre-nixos/ostree/repo/objects/ab/cdef.file"), "object")
        # kept
        w(os.path.join(top, "@home/alex/notes.txt"), "new notes\n")
        w(os.path.join(top, "@nix/store/x-nixos-system/init"), "init")
        w(os.path.join(top, "@nixos/etc/NIXOS"))
        w(os.path.join(top, "@nixos/var/lib/rime/boot-migrate/phase"), "failed\n")
        w(os.path.join(top, "user-subvol-left-alone/data"), "mine")
        # /boot
        b = self.boot
        w(os.path.join(b, "grub2/grub.cfg"), STATIC)
        w(os.path.join(b, "grub2/custom.cfg"), RIME_BOOT + SB_KEY + WINDOWS)
        w(os.path.join(b, "grub2/rime-nixos.cfg"),
          "menuentry 'Rime (generation 1)' --id rime-generation-1 {\n  linux /boot/rime/nixos/k-bzImage.efi init=x\n"
          "  initrd /boot/rime/nixos/i-initrd.img\n}\nset default=rime-generation-1\n")
        w(os.path.join(b, "rime/nixos/k-bzImage.efi"), "kernel")
        w(os.path.join(b, "rime/nixos/i-initrd.img"), "initrd")
        w(os.path.join(b, "ostree/default-abc/vmlinuz-7.2.9"), "old kernel")
        for n, title in ((1, "Fedora Linux 45 (ostree:0)"), (2, "Fedora Linux 45 (ostree:1)")):
            w(os.path.join(b, f"loader.1/entries/ostree-{n}.conf"),
              f"title {title}\nversion {n}\nlinux /boot/ostree/default-abc/vmlinuz-7.2.9\n"
              f"initrd /boot/ostree/default-abc/initramfs-7.2.9.img\noptions root=UUID=x rw ostree=/ostree/boot.1/default/abc/{n}\n")
        w(os.path.join(b, "loader.0/entries/ostree-old.conf"),
          "title Old\nlinux /ostree/default-abc/vmlinuz\noptions root=UUID=x rw\n")
        w(os.path.join(b, "loader.1/entries/memtest.conf"), "title Memtest\nlinux /memtest86.efi\n")
        os.symlink("loader.1", os.path.join(b, "loader"))
        # a process using /var/home (fine) and kernel threads
        for pid, cwd in (("1", "/"), ("400", "/var/home/alex")):
            d = os.path.join(self.proc, pid)
            os.makedirs(os.path.join(d, "fd"))
            os.symlink(cwd, os.path.join(d, "cwd"))
            os.symlink("/", os.path.join(d, "root"))
            w(os.path.join(d, "comm"), "fish\n")
            w(os.path.join(d, "maps"), "00400000-00452000 r-xp 00000000 08:02 173521 /nix/store/x-fish/bin/fish\n")
        os.makedirs(self.sys_block)
        w(self.swaps, "Filename Type Size Used Priority\n")

    # btrfs, emulated
    def mount_top(self, source, writable):
        self.mounted.append((source, writable))
        return self.top

    def device_stats(self, path):
        return dict(self.stats)

    def subvolumes(self, top):
        return dict(self.subs)

    def subvol_delete(self, path):
        rel = os.path.relpath(path, self.top)
        if self.interrupt_at == ("delete", os.path.basename(rel)):
            self.interrupt_at = None
            raise Interrupt()
        if any(s.startswith(rel + "/") for s in self.subs):
            raise F.Failed(f"subvolume {rel} still has subvolumes in it")
        shutil.rmtree(path)
        self.subs.pop(rel, None)
        self.deleted.append(rel)

    def subvol_sync(self, top):
        pass

    def commit(self, path):
        # what GRUB would read after a power cut from here on: the ostree entries left under /boot at this commit
        self.commits.append((os.path.relpath(path, self.top), [os.path.basename(f) for f, _ in F.ostree_entries(path)]))

    def usage(self, top):
        n = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(top) for f in fs)
        return {"data_used": n, "free_estimated": 10**9 - n}

    def estimate(self, top, kept):
        return 12345

    def esp_sums(self):
        return {"/dev/vda1": {"EFI/fedora/shimx64.efi": "aa", "EFI/fedora/grubx64.efi": "bb",
                              "EFI/fedora/mmx64.efi": "cc"}}

    def luks_headers(self):
        return {}

    def remove_tree(self, path):
        if self.interrupt_at == ("rm", os.path.basename(path)):
            self.interrupt_at = None
            # half of it gone, then the power goes
            for d, dirs, fs in os.walk(path):
                for f in fs[: len(fs) // 2 + 1]:
                    os.unlink(os.path.join(d, f))
            raise Interrupt()
        rel = os.path.relpath(path, self.top)
        inside = [s for s in self.subs if s == rel or s.startswith(rel + "/")]
        if inside:
            raise F.Failed(f"rm --one-file-system cannot remove {rel}: subvolumes {inside} in it")
        shutil.rmtree(path)

    def after_step(self, name):
        if self.hook:
            self.hook(self, name)
        if self.interrupt_at == ("step", name):
            self.interrupt_at = None
            raise Interrupt()

    # rename keeps the emulated subvolume list in step, as btrfs computes paths
    def renamed(self):
        pass


_rename = os.rename


def fake_rename(sx):
    """os.rename on the top level as btrfs does it: the emulated subvolume list follows a rename, and a read-only
    subvolume cannot be moved into another directory (EROFS)."""
    def rename(src, dst):
        if src.startswith(sx.top + "/") and dst.startswith(sx.top + "/"):
            a, b = os.path.relpath(src, sx.top), os.path.relpath(dst, sx.top)
            if a in sx.readonly and os.path.dirname(a) != os.path.dirname(b):
                raise OSError(errno.EROFS, os.strerror(errno.EROFS), src, None, dst)
            _rename(src, dst)
            for s in list(sx.subs):
                if s == a or s.startswith(a + "/"):
                    sx.subs[b + s[len(a):]] = sx.subs.pop(s)
            sx.readonly = {(b + r[len(a):]) if r == a or r.startswith(a + "/") else r for r in sx.readonly}
        else:
            _rename(src, dst)
    return rename


def run(sx, *argv):
    out = io.StringIO()
    err = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = F.main(list(argv), sx=sx)
    return rc, out.getvalue() + err.getvalue()


def snapshot(root):
    out = {}
    for d, dirs, fs in os.walk(root):
        for n in fs + [x for x in dirs if os.path.islink(os.path.join(d, x))]:
            p = os.path.join(d, n)
            out[os.path.relpath(p, root)] = ("->" + os.readlink(p)) if os.path.islink(p) else slurp(p)
        out[os.path.relpath(d, root) + "/"] = None
    return out


class FinishTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="finish-test-")
        self.sx = FakeSystem(self.tmp)
        sx = self.sx

        F.os.rename = fake_rename(sx)
        self.addCleanup(setattr, F.os, "rename", _rename)
        self.addCleanup(shutil.rmtree, self.tmp)

    def refused(self, name):
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 3, out)
        self.assertIn(f"REFUSED [{name}]", out)
        return out

    def assert_finished(self):
        top, b = self.sx.top, self.sx.boot
        for n in ("@pre-nixos", "ostree", F.TRASH):
            self.assertFalse(os.path.lexists(os.path.join(top, n)), n)
        self.assertFalse(os.path.lexists(os.path.join(b, "ostree")))
        self.assertEqual(F.ostree_entries(b), [])
        self.assertTrue(os.path.isfile(os.path.join(b, "loader.1/entries/memtest.conf")))
        with open(os.path.join(b, "grub2/custom.cfg")) as f:
            self.assertEqual(f.read(), RIME_BOOT + SB_KEY + WINDOWS)
        with open(os.path.join(top, "@home/alex/notes.txt")) as f:
            self.assertEqual(f.read(), "new notes\n")
        for k in ("@nixos", "@nix", "@home"):
            self.assertIn(k, self.sx.subs)
        self.assertTrue(os.path.isfile(os.path.join(b, "rime/nixos/k-bzImage.efi")))
        self.assertTrue(os.path.isfile(os.path.join(top, "user-subvol-left-alone/data")))
        fin = jload(os.path.join(self.sx.state, "finish.json"))
        self.assertIsNotNone(fin.get("done"))
        self.assertEqual(fin["steps"], list(F.STEPS))

    # ── the preconditions, each by name ──

    def test_booted_from_ostree_refused(self):
        w(self.sx.ostree_booted)
        self.refused("booted-ostree")

    def test_ostree_cmdline_refused(self):
        w(self.sx.cmdline, "BOOT_IMAGE=(hd0,gpt3)/boot/ostree/default-abc/vmlinuz ostree=/ostree/boot.1/default/abc/0\n")
        self.refused("booted-ostree")

    def test_booted_from_ostree_refused_even_with_nixos_mounts(self):
        """The ostree check stands on its own: an ostree boot with @nixos-looking mounts is still refused by it."""
        w(self.sx.ostree_booted)
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 3)
        self.assertIn("REFUSED [booted-ostree]", out)
        self.assertTrue(os.path.isdir(os.path.join(self.sx.top, "ostree")))

    def test_not_nixos_refused(self):
        os.unlink(self.sx.nixos_marker)
        self.refused("not-nixos")

    def test_root_not_nixos_refused(self):
        w(self.sx.mountinfo, mountinfo([("/ostree/deploy/default/deploy/abc.0", "/", "btrfs", SRC)] + GOOD_MOUNTS[1:]))
        self.refused("root-not-nixos-subvol")

    def test_home_not_from_home_refused(self):
        for bad in ([("/ostree/deploy/default/var/home", "/var/home", "btrfs", SRC)],
                    [("/@home", "/var/home", "btrfs", "/dev/sdb1")],
                    [("/", "/var/home", "ext4", "/dev/sdb1")],
                    []):
            w(self.sx.mountinfo, mountinfo([GOOD_MOUNTS[0], GOOD_MOUNTS[1]] + bad + GOOD_MOUNTS[3:]))
            out = self.refused("home-not-from-home-subvol")
            self.assertTrue(os.path.isdir(os.path.join(self.sx.top, "ostree")), out)

    def test_home_not_from_home_refused_on_yes(self):
        w(self.sx.mountinfo, mountinfo([GOOD_MOUNTS[0], ("/ostree/deploy/default/var/home", "/var/home", "btrfs", SRC)]))
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 3)
        self.assertIn("REFUSED [home-not-from-home-subvol]", out)
        self.assertTrue(os.path.isdir(os.path.join(self.sx.top, "ostree")))

    def test_record_missing_refused(self):
        os.unlink(os.path.join(self.sx.top, "ostree/deploy/default/var/lib/rime/nixos-bridge/state.json"))
        self.refused("start-record-missing")

    def test_record_not_done_refused(self):
        p = os.path.join(self.sx.top, "ostree/deploy/default/var/lib/rime/nixos-bridge/state.json")
        w(p, json.dumps({"started": "x", "steps": ["backup", "old side"]}))
        self.refused("start-unfinished")
        w(p, json.dumps({"started": "x", "failed": "", "steps": ["backup", "rolled back"]}))
        self.refused("start-record-not-done")
        w(p, "{not json")
        self.refused("start-record-unreadable")
        w(p, json.dumps(dict(DONE, done="")))   # done set, even empty, is done (record_refusal, as everywhere)
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 0, out)

    def test_two_records_refused(self):
        w(os.path.join(self.sx.top, "ostree/deploy/other/var/lib/rime/nixos-bridge/state.json"), json.dumps(DONE))
        self.refused("start-record-ambiguous")

    def test_too_few_good_boots(self):
        w(os.path.join(self.sx.state, "good-boots.json"), json.dumps({"boots": [{"boot_id": "a"}, {"boot_id": "b"}]}))
        out = self.refused("too-few-good-boots")
        self.assertIn(f"needs {F.GOOD_BOOTS}", out)

    def test_good_boots_is_three(self):
        self.assertEqual(F.GOOD_BOOTS, 3)

    def test_old_side_mounted(self):
        for extra in (("/@pre-nixos", "/mnt/old", "btrfs", SRC),
                      ("/ostree/deploy/default/var", "/mnt/oldvar", "btrfs", SRC),
                      ("/", "/mnt/rime-top/ostree/deploy/default/var/lib/x", "tmpfs", "tmpfs")):
            w(self.sx.mountinfo, mountinfo(GOOD_MOUNTS + [extra]))
            self.refused("old-side-mounted")

    def test_old_side_in_use(self):
        d = os.path.join(self.sx.proc, "777")
        os.makedirs(os.path.join(d, "fd"))
        os.symlink("/", os.path.join(d, "cwd"))
        os.symlink("/mnt/rime-top/ostree/deploy/default/var/log/x.log", os.path.join(d, "fd", "3"))
        w(os.path.join(d, "comm"), "tail\n")
        out = self.refused("old-side-in-use")
        self.assertIn("pid 777 (tail)", out)
        os.unlink(os.path.join(d, "fd", "3"))
        w(os.path.join(d, "maps"), "7f00-7f01 r--p 0 08:02 1 /mnt/rime-top/@pre-nixos/usr/lib64/libc.so.6\n")
        self.refused("old-side-in-use")
        os.unlink(os.path.join(d, "maps"))
        w(os.path.join(self.sx.sys_block, "loop0/loop/backing_file"), "/mnt/rime-top/ostree/deploy/default/var/x.img\n")
        self.refused("old-side-in-use")
        shutil.rmtree(os.path.join(self.sx.sys_block, "loop0"))
        w(self.sx.swaps, "Filename Type Size Used Priority\n/boot/ostree/swapfile file 1 0 -2\n")
        self.refused("old-side-in-use")

    def test_own_top_mount_and_home_are_not_in_use(self):
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 0, out)

    def test_btrfs_errors_refused(self):
        self.sx.stats = {"[/dev/vda3].corruption_errs": 2}
        out = self.refused("btrfs-errors")
        self.assertIn("corruption_errs 2", out)

    def test_nixos_menu_missing_refused(self):
        w(os.path.join(self.sx.boot, "grub2/custom.cfg"), WINDOWS)
        self.refused("nixos-menu-missing")
        w(os.path.join(self.sx.boot, "grub2/custom.cfg"), RIME_BOOT)
        os.unlink(os.path.join(self.sx.boot, "rime/nixos/k-bzImage.efi"))
        self.refused("nixos-menu-missing")

    def test_menu_on_a_boot_partition_counts(self):
        # The encrypted installer layout: /boot is its own partition, so GRUB's paths start at /rime/ (bridge2 L).
        w(os.path.join(self.sx.boot, "grub2/rime-nixos.cfg"),
          "menuentry 'Rime (generation 1)' --id rime-generation-1 {\n  linux /rime/nixos/k-bzImage.efi init=x\n"
          "  initrd /rime/nixos/i-initrd.img\n}\n")
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 0, out)
        os.unlink(os.path.join(self.sx.boot, "rime/nixos/k-bzImage.efi"))
        self.refused("nixos-menu-missing")

    def test_ostree_entries_finish_would_leave_refused(self):
        w(os.path.join(self.sx.boot, "grub2/custom.cfg"), RIME_BOOT + "menuentry 'old' {\n  linux /ostree/x ostree=/y\n}\n")
        self.refused("custom-cfg-ostree-entry")
        w(os.path.join(self.sx.boot, "grub2/custom.cfg"), RIME_BOOT)
        w(os.path.join(self.sx.boot, "grub2/grub.cfg"), "menuentry 'x' {\n linux /vmlinuz ostree=/ostree/boot.1/a\n}\n")
        self.refused("grub-cfg-ostree-inline")

    # ── --dry-run ──

    def test_dry_run_writes_nothing(self):
        before = snapshot(self.tmp)
        for argv in (["--dry-run"], []):
            rc, out = run(self.sx, *argv)
            self.assertEqual(rc, 0, out)
            self.assertEqual(snapshot(self.tmp), before)
        self.assertEqual([w_ for _, w_ in self.sx.mounted], [False, False])
        self.assertIn("Fedora Linux 45 (ostree:0)", out)
        self.assertIn("Fedora Linux 45 (ostree:1)", out)
        self.assertIn("@pre-nixos", out)
        self.assertIn("ostree/", out)
        self.assertIn("nested subvolume ostree/deploy/default/var/lib/containers/storage/btrfs/subvolumes/aaa", out)
        self.assertIn("should free about 0.00 GB", out)
        self.assertIn("what stops working", out)
        self.assertIn("can no longer be booted", out)
        self.assertIn("nothing was changed", out)

    def test_dry_run_says_whether_an_off_disk_backup_exists(self):
        # start without --backup (the default since no drive is needed): after finish @home is the only copy of home.
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 0, out)
        self.assertIn("took no backup off this disk, so @home is then the only copy of home", out)
        self.assertNotIn("only copy left; keep it", out)
        p = os.path.join(self.sx.top, "ostree/deploy/default/var/lib/rime/nixos-bridge/state.json")
        w(p, json.dumps(dict(DONE, backup="/run/media/usb/rime-migrate-backup-20261104T100000Z")))
        rc, out = run(self.sx, "--dry-run")
        self.assertEqual(rc, 0, out)
        self.assertIn("(off this disk, /run/media/usb/rime-migrate-backup-20261104T100000Z) is the only copy left", out)

    # ── --yes ──

    def test_yes_finishes(self):
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        self.assertEqual(self.sx.mounted, [(SRC, True)])
        self.assert_finished()
        # nested subvolumes went deepest first, @pre-nixos as a subvolume
        nested = [d for d in self.sx.deleted if "containers" in d]
        self.assertEqual(len(nested), 2)
        self.assertTrue(nested[0].endswith("aaa/inner"), self.sx.deleted)
        self.assertIn("@pre-nixos", self.sx.deleted)
        saved = jload(os.path.join(self.sx.state, "start-record.json"))
        self.assertEqual(saved["done"], DONE["done"])
        fin = jload(os.path.join(self.sx.state, "finish.json"))
        self.assertEqual(sorted(fin["removed_entries"]), ["Fedora Linux 45 (ostree:0)", "Fedora Linux 45 (ostree:1)",
                                                          "Old"])
        self.assertLess(fin["usage_after"]["data_used"], fin["usage_before"]["data_used"])

    def test_read_only_snapshot_is_not_moved(self):
        """@pre-nixos is read-only: it is deleted where it is, only ostree/ moves aside (btrfs: EROFS otherwise)."""
        seen = {}

        def hook(sx, name):
            if name == "move aside":
                seen["pre"] = os.path.isdir(os.path.join(sx.top, "@pre-nixos"))
                seen["ostree"] = os.path.isdir(os.path.join(sx.top, F.TRASH, "ostree"))
        self.sx.hook = hook
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        self.assertEqual(seen, {"pre": True, "ostree": True})
        self.assert_finished()

    def test_os_error_is_recorded_failure(self):
        """A rename the kernel refuses is a recorded failure (exit 10, re-runnable), never a bare traceback."""
        self.sx.subs["ostree"] = "u-ostree"
        self.sx.readonly.add("ostree")
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, F.EXIT_FAILED, out)
        self.assertIn("FAILED", out)
        fin = jload(os.path.join(self.sx.state, "finish.json"))
        self.assertIn("Read-only file system", fin.get("failed", ""))
        self.assertIsNone(fin.get("done"))
        self.assertEqual(F.rime_menu(self.sx.boot), (1, None))
        self.sx.readonly.discard("ostree")
        del self.sx.subs["ostree"]
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        self.assert_finished()

    def test_boot_entries_go_first(self):
        """The old system stops being bootable before any of its data is deleted."""
        seen = {}

        def hook(sx, name):
            if name == "boot entries":
                seen["entries"] = F.ostree_entries(sx.boot)
                seen["ostree"] = os.path.isdir(os.path.join(sx.top, "ostree"))
                seen["pre"] = os.path.isdir(os.path.join(sx.top, "@pre-nixos"))
            if name == "boot entries":
                seen["commits"] = list(sx.commits)
        self.sx.hook = hook
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        # ...and the removal is committed (GRUB replays no btrfs log or ext4 journal) before the step is recorded
        self.assertEqual(seen, {"entries": [], "ostree": True, "pre": True, "commits": [("boot", [])]})

    def test_second_run_refused(self):
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        before = snapshot(self.tmp)
        for argv in (["--yes"], ["--dry-run"]):
            rc, out = run(self.sx, *argv)
            self.assertEqual(rc, 3, out)
            self.assertIn("REFUSED [already-finished]", out)
            self.assertEqual(out.count("REFUSED"), 1, out)
        self.assertEqual(snapshot(self.tmp), before)

    def test_interrupted_at_every_step_then_rerun_completes(self):
        points = [("step", s) for s in F.STEPS[:-1]] + [("delete", "inner"), ("delete", "@pre-nixos"),
                                                         ("rm", F.TRASH), ("rm", "ostree")]
        for point in points:
            with self.subTest(point=point):
                shutil.rmtree(self.tmp)
                os.makedirs(self.tmp)
                self.sx = FakeSystem(self.tmp)
                self.setUp_rename()
                self.sx.interrupt_at = point
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(Interrupt):
                        F.main(["--yes"], sx=self.sx)
                # NixOS still boots: its menu, kernels and subvolumes are all there
                self.assertEqual(F.rime_menu(self.sx.boot), (1, None))
                for k in ("@nixos", "@nix", "@home"):
                    self.assertIn(k, self.sx.subs)
                fin = jload(os.path.join(self.sx.state, "finish.json"))
                self.assertIsNone(fin.get("done"))
                rc, out = run(self.sx, "--yes")
                self.assertEqual(rc, 0, f"{point}: {out}")
                self.assert_finished()

    def setUp_rename(self):
        F.os.rename = fake_rename(self.sx)

    def test_kept_trees_never_deleted(self):
        for rel in ("@home", "@nixos", "@nix", "boot", "", ".", F.TRASH, os.path.join(F.TRASH, "@home"),
                    "ostree/../@home"):
            with self.assertRaises(F.Failed, msg=rel):
                F.never(rel)
        for rel in ("@pre-nixos", "ostree", os.path.join(F.TRASH, "ostree/deploy/x")):
            F.never(rel)

    def test_home_survives(self):
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 0, out)
        self.assertTrue(os.path.isfile(os.path.join(self.sx.top, "@home/alex/notes.txt")))
        self.assertNotIn("@home", self.sx.deleted)

    def test_containment_failure_is_a_failure(self):
        """Anything finish must not touch, changed during a run: exit 10, not done; the next run (once it is
        put back) completes."""
        def hook(sx, name):
            if name == "old tree":
                with open(os.path.join(sx.boot, "grub2/custom.cfg"), "a") as f:
                    f.write("# changed\n")
        self.sx.hook = hook
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 10, out)
        self.assertIn("containment", out)
        fin = jload(os.path.join(self.sx.state, "finish.json"))
        self.assertIsNone(fin.get("done"))
        self.assertIn("containment", fin["failed"])

    def test_entries_left_behind_are_a_failure(self):
        real = F.ostree_entries
        calls = {"n": 0}

        def stale(boot):
            # an entry that comes back after step 1 (a bug, or ostree running somewhere): caught at the end
            calls["n"] += 1
            if calls["n"] > 3:
                w(os.path.join(boot, "loader.1/entries/ostree-9.conf"), "title X\noptions ostree=/a\n")
            return real(boot)
        F.ostree_entries = stale
        self.addCleanup(setattr, F, "ostree_entries", real)
        rc, out = run(self.sx, "--yes")
        self.assertEqual(rc, 10, out)
        self.assertIsNone(jload(os.path.join(self.sx.state, "finish.json")).get("done"))

    # ── --record-boot ──

    def test_record_boot_counts_once_per_boot(self):
        os.unlink(os.path.join(self.sx.state, "good-boots.json"))
        for i, bid in enumerate(("a", "a", "b", "c")):
            w(self.sx.boot_id, bid + "\n")
            rc, out = run(self.sx, "--record-boot")
            self.assertEqual(rc, 0)
        self.assertEqual([b["boot_id"] for b in F.good_boots(self.sx)], ["a", "b", "c"])
        self.assertIn("can remove the old system now", out)

    def test_record_boot_not_from_nixos(self):
        os.unlink(os.path.join(self.sx.state, "good-boots.json"))
        w(self.sx.mountinfo, mountinfo([("/ostree/deploy/default/deploy/abc.0", "/", "btrfs", SRC)]))
        rc, out = run(self.sx, "--record-boot")
        self.assertIn("not counted", out)
        self.assertEqual(F.good_boots(self.sx), [])
        w(self.sx.mountinfo, mountinfo(GOOD_MOUNTS))
        w(self.sx.ostree_booted)
        run(self.sx, "--record-boot")
        self.assertEqual(F.good_boots(self.sx), [])

    def test_record_boot_capped(self):
        w(os.path.join(self.sx.state, "good-boots.json"),
          json.dumps({"boots": [{"boot_id": str(i)} for i in range(F.GOOD_BOOTS_KEPT)]}))
        run(self.sx, "--record-boot")
        self.assertEqual(len(F.good_boots(self.sx)), F.GOOD_BOOTS_KEPT)

    # ── the program ──

    def test_parse_mountinfo_escapes(self):
        m = F.parse_mountinfo("36 35 98:0 /my\\040dir /mnt/a\\040b rw - btrfs /dev/vda3 rw\n")
        self.assertEqual(m[0]["root"], "/my dir")
        self.assertEqual(m[0]["target"], "/mnt/a b")

    def test_non_root_and_usage(self):
        if os.geteuid() == 0:
            self.skipTest("runs as root")
        r = subprocess.run(["python3", "-I", "-B", PROG, "--dry-run"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("needs root", r.stderr)
        r = subprocess.run(["python3", "-I", "-B", PROG, "--dry-run", "--yes"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)

    def test_no_forbidden_tools(self):
        text = slurp(PROG).decode()
        for word in ("bootctl", "efibootmgr", "grub2-install", "grub-install", "NIXOS_LUSTRATE", "grub2-reboot",
                     "grub2-mkconfig", "cryptsetup luksFormat", "luksKillSlot", "luksRemoveKey"):
            self.assertNotIn(f'"{word}"', text)


if __name__ == "__main__":
    unittest.main()
