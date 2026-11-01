"""rime-hostlayer's unit tests: run by the package's installCheckPhase against the installed program.

Each test builds a throwaway old root (a bootc-shaped /etc), a fake NixOS toplevel (sw/bin and the declared
accounts) and an empty new root under a temporary directory. Nothing here needs root; the VM tests
(nix/tests/hostlayer.nix) cover what does.
"""

import hashlib
import json
import os
import subprocess
import tempfile
import tomllib
import unittest

PROG = os.environ.get("RIME_HOSTLAYER", "rime-hostlayer")
GENERATOR = os.environ.get("RIME_HOSTLAYER_GENERATOR")

# A NixOS system's declared accounts, as nix/modules/hostlayer.nix writes them (a subset of rime-vm's).
DECLARED = {
    "users": {
        "root": {"uid": 0, "group": "root"},
        "nobody": {"uid": 65534, "group": "nogroup"},
        "messagebus": {"uid": 4, "group": "messagebus"},
        "nixbld1": {"uid": 30001, "group": "nixbld"},
        "greetd": {"uid": None, "group": "greetd"},
        "sshd": {"uid": None, "group": "sshd"},
    },
    "groups": {
        "root": {"gid": 0}, "wheel": {"gid": 1}, "nogroup": {"gid": 65534}, "messagebus": {"gid": 4},
        "nixbld": {"gid": 30000}, "scanner": {"gid": 59}, "lp": {"gid": 20}, "greetd": {"gid": None},
        "sshd": {"gid": None}, "shadow": {"gid": 318},
    },
}

PASSWD = """root:x:0:0:Super User:/root:/bin/bash
lp:x:4:7:lp:/var/spool/lpd:/usr/sbin/nologin
nobody:x:65534:65534:Kernel Overflow User:/:/usr/sbin/nologin
greetd:x:973:973:greetd daemon:/var/lib/greetd:/usr/sbin/nologin
ghrunner:x:960:960:GitHub Actions self-hosted runner:/var/lab/runner:/usr/sbin/nologin
andre:x:1000:1000::/var/home/andre:/bin/zsh
visitor:x:1001:1001::/var/home/visitor:/usr/bin/fish
"""
GROUP = """root:x:0:
lp:x:7:
wheel:x:10:andre,ghost
tss:x:59:clevis
nobody:x:65534:
greetd:x:973:
ghrunner:x:960:
andre:x:1000:
visitor:x:1001:
"""
SHADOW = """root:*::0:99999:7:::
andre:$6$rimetestsalt$hash:20000:0:99999:7:::
ghrunner:!:20000::::::
visitor:$6$rimetestsalt$hash:20000:0:99999:7:::
"""
GSHADOW = "root:::\nwheel:!::andre,ghost\nandre:!::\n"
SUBIDS = "andre:524288:65536\nghrunner:1000000:65536\n"


def tree_hash(top):
    h = hashlib.sha256()
    for dirpath, dirnames, filenames in os.walk(top):
        dirnames.sort()
        for fn in sorted(filenames):
            p = os.path.join(dirpath, fn)
            st = os.lstat(p)
            h.update(f"{os.path.relpath(p, top)} {st.st_mode:o}\n".encode())
            h.update(os.readlink(p).encode() if os.path.islink(p) else open(p, "rb").read())
    return h.hexdigest()


def write(path, text, mode=0o644):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if os.path.lexists(path):
        os.unlink(path)
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, mode)


def run(*args, env=None, check=None):
    e = dict(os.environ)
    e.update(env or {})
    r = subprocess.run([PROG, *args], capture_output=True, text=True, env=e)
    if check is not None:
        assert r.returncode == check, f"{args}: exit {r.returncode}\n{r.stdout}\n{r.stderr}"
    return r


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = self.tmp.name
        self.src, self.dest, self.sys = f"{t}/old", f"{t}/new", f"{t}/system"
        os.makedirs(self.dest)
        for b in ("bash", "zsh", "nologin", "fish"):
            write(f"{self.sys}/sw/bin/{b}", "")
        write(f"{self.sys}/etc/rime/hostlayer/declared-accounts.json", json.dumps(DECLARED))
        e = f"{self.src}/etc"
        write(f"{e}/passwd", PASSWD)
        write(f"{e}/group", GROUP)
        write(f"{e}/shadow", SHADOW, 0o600)
        write(f"{e}/gshadow", GSHADOW, 0o600)
        write(f"{e}/subuid", SUBIDS)
        write(f"{e}/subgid", SUBIDS)
        write(f"{e}/login.defs", "UID_MIN 1000\nGID_MIN 1000\n")
        write(f"{e}/machine-id", "e9cc8221c5e54fbcb8586980b2f2e386\n", 0o444)
        write(f"{e}/hostname", "apex\n")
        os.symlink("../usr/share/zoneinfo/Australia/Perth", f"{e}/localtime")
        write(f"{e}/locale.conf", "# comment\nLANG=en_US.UTF-8\n")
        write(f"{e}/vconsole.conf", "KEYMAP=uk\n")
        write(f"{e}/X11/xorg.conf.d/00-keyboard.conf",
              'Section "InputClass"\n Identifier "system-keyboard"\n Option "XkbLayout" "gb"\n'
              ' Option "XkbVariant" "extd"\nEndSection\n')
        write(f"{e}/fstab",
              "UUID=a41f / btrfs subvol=root 0 0\n"
              "UUID=a41fee21 /var/home/andre/Games ext4 defaults,noatime,nofail 0 2\n"
              "UUID=8c87 /var/lab ext4 defaults,noatime,nofail 0 2\n"
              "/dev/zram0 none swap defaults 0 0\n")
        write(f"{e}/NetworkManager/system-connections/Home Wi-Fi.nmconnection", "[connection]\nid=Home Wi-Fi\n",
              0o600)
        write(f"{e}/ssh/ssh_host_ed25519_key", "PRIVATE-TEST\n", 0o600)
        write(f"{e}/ssh/ssh_host_ed25519_key.pub", "ssh-ed25519 AAAA test\n")
        write(f"{e}/ssh/sshd_config", "# stock\n", 0o600)
        write(f"{e}/cups/printers.conf", "# printers\n", 0o600)
        write(f"{e}/cups/cupsd.conf", "# image default\n")
        write(f"{e}/cups/ppd/office.ppd", "*PPD\n")
        write(f"{e}/rime/guest-accounts", "# guests\nvisitor\n")
        write(f"{e}/rime/firewall.d/ssh.conf", "ssh\n")
        write(f"{e}/npmrc", "prefix=${HOME}/.local\n")
        write(f"{e}/pki/akmods/private/test.priv", "PRIVATE\n", 0o600)
        write(f"{e}/systemd/system/runner.service", "[Service]\n")
        os.makedirs(f"{e}/systemd/system/user-runtime-dir@1001.service.wants")
        os.symlink("/usr/lib/systemd/system/rime-guest-session@.service",
                   f"{e}/systemd/system/user-runtime-dir@1001.service.wants/rime-guest-session@1001.service")
        write(f"{self.src}/var/lib/bluetooth/70:1A/settings", "[General]\n", 0o600)
        write(f"{self.src}/var/lib/rime-greet/last-desktop", "hyprland")
        os.symlink("rime", f"{self.src}/var/lib/apex")

    def tearDown(self):
        self.tmp.cleanup()

    def carry(self, *extra, check=None):
        return run("carry", "--from", self.src, "--root", self.dest, "--system", self.sys, *extra, check=check)

    def read(self, rel):
        with open(os.path.join(self.dest, rel)) as f:
            return f.read()


class CarryTest(Fixture):
    def test_accounts_are_carried_with_their_ids_and_ranges(self):
        before = tree_hash(self.src)
        self.carry(check=0)
        self.assertEqual(tree_hash(self.src), before, "carry wrote into its source")
        pw = {ln.split(":")[0]: ln.split(":") for ln in self.read("etc/passwd").splitlines()}
        self.assertEqual(pw["andre"][2:4], ["1000", "1000"])
        self.assertEqual(pw["ghrunner"][2:4], ["960", "960"])
        self.assertEqual(pw["greetd"][2], "973", "a dynamic NixOS account keeps the machine's id")
        self.assertEqual(pw["andre"][6], "/run/current-system/sw/bin/zsh")
        self.assertEqual(pw["ghrunner"][6], "/run/current-system/sw/bin/nologin")
        self.assertEqual(pw["root"][6], "/run/current-system/sw/bin/bash")
        # lp (uid 4) is messagebus's on NixOS: an old-image account, left behind and reported
        self.assertNotIn("lp", pw)
        self.assertEqual(self.read("etc/subuid"), SUBIDS)
        self.assertEqual(self.read("etc/subgid"), SUBIDS)
        gr = {ln.split(":")[0]: ln.split(":") for ln in self.read("etc/group").splitlines()}
        self.assertEqual(gr["wheel"][2:], ["1", "andre"], "wheel takes NixOS's gid, keeps andre, drops ghost")
        self.assertNotIn("tss", gr, "tss collides with NixOS's scanner and nobody needs it")
        self.assertIn("andre:$6$rimetestsalt$hash", self.read("etc/shadow"))
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        left = {e["path"]: e["why"] for e in rep["left_behind"]}
        self.assertIn("user:lp", left)
        self.assertIn("group:tss", left)
        self.assertIn("/etc/systemd/system/runner.service", left)
        self.assertIn("/etc/cups/cupsd.conf", left)
        self.assertEqual(rep["not_copied_private_keys"], ["/etc/pki/akmods/private/test.priv"])
        self.assertNotIn("PRIVATE", json.dumps(rep))

    def test_host_toml_holds_the_machine_facts(self):
        self.carry(check=0)
        with open(f"{self.dest}/var/lib/rime/host.toml", "rb") as f:
            host = tomllib.load(f)
        self.assertEqual(host["host"], {"hostname": "apex", "timezone": "Australia/Perth"})
        self.assertEqual(host["locale"], {"LANG": "en_US.UTF-8"})
        self.assertEqual(host["keyboard"], {"console": "uk", "xkb_layout": "gb", "xkb_variant": "extd"})
        self.assertEqual([m["where"] for m in host["mount"]], ["/var/home/andre/Games", "/var/lab"])
        self.assertEqual(host["mount"][0]["options"], "defaults,noatime,nofail")
        run("host", "validate", "--file", f"{self.dest}/var/lib/rime/host.toml", check=0)

    def test_files_and_state_move_where_nixos_reads_them(self):
        self.carry(check=0)
        self.assertEqual(self.read("etc/machine-id"), "e9cc8221c5e54fbcb8586980b2f2e386\n")
        nm = f"{self.dest}/etc/NetworkManager/system-connections/Home Wi-Fi.nmconnection"
        self.assertEqual(oct(os.stat(nm).st_mode & 0o777), "0o600")
        self.assertEqual(self.read("etc/ssh/ssh_host_ed25519_key"), "PRIVATE-TEST\n")
        self.assertFalse(os.path.exists(f"{self.dest}/etc/ssh/sshd_config"))
        self.assertEqual(self.read("var/lib/cups/printers.conf"), "# printers\n")
        self.assertTrue(os.path.exists(f"{self.dest}/var/lib/cups/ppd/office.ppd"))
        self.assertFalse(os.path.exists(f"{self.dest}/var/lib/cups/cupsd.conf"))
        self.assertTrue(os.path.exists(f"{self.dest}/var/lib/bluetooth/70:1A/settings"))
        self.assertIn("visitor", self.read("var/lib/rime/guest-accounts"))
        self.assertEqual(self.read("var/lib/rime/firewall.d/ssh.conf"), "ssh\n")
        self.assertEqual(self.read("var/lib/rime-greet/last-desktop"), "hyprland")
        self.assertTrue(os.path.exists(f"{self.dest}/var/lib/rime/switch/rime-guest-session@1001.service.on"),
                        "a guest bootc enabled is a switch that is on")
        rep = self.read("var/lib/rime/carry/report.txt")
        self.assertIn("/etc/npmrc: not carried; keys set there: prefix", rep)
        self.assertIn("/etc/hypr: absent", rep)

    def test_a_person_colliding_with_a_nixos_id_is_refused_by_name_and_nothing_is_written(self):
        write(f"{self.src}/etc/passwd", PASSWD + "builder:x:30001:30000::/var/home/builder:/bin/bash\n")
        write(f"{self.src}/etc/group", GROUP + "builder:x:30000:\n")
        before = tree_hash(self.src)
        r = self.carry(check=2)
        self.assertIn("user builder (uid 30001) collides with the NixOS user nixbld1", r.stderr)
        self.assertIn("group builder (gid 30000, primary group of builder) collides with the NixOS group nixbld",
                      r.stderr)
        self.assertEqual(os.listdir(self.dest), [], "a refused carry wrote something")
        self.assertEqual(tree_hash(self.src), before)

    def test_a_service_account_with_subordinate_ids_is_pinned(self):
        write(f"{self.src}/etc/passwd", PASSWD.replace("ghrunner:x:960:960", "ghrunner:x:30001:960"))
        r = self.carry(check=2)
        self.assertIn("user ghrunner (uid 30001) collides with the NixOS user nixbld1", r.stderr)

    def test_a_person_with_a_name_nixos_declares_is_refused(self):
        write(f"{self.src}/etc/passwd", PASSWD + "sshd:x:1002:1002::/var/home/sshd:/bin/bash\n")
        write(f"{self.src}/etc/group", GROUP + "sshd:x:1002:\n")
        r = self.carry(check=2)
        self.assertIn("user sshd (uid 1002) is a person on this machine", r.stderr)

    def test_an_unfinished_apex_migration_is_refused(self):
        os.unlink(f"{self.src}/var/lib/apex")
        os.makedirs(f"{self.src}/var/lib/apex")
        r = self.carry(check=2)
        self.assertIn("rime-migrate-from-apex has not finished", r.stderr)

    def test_a_second_carry_is_refused(self):
        self.carry(check=0)
        r = self.carry(check=2)
        self.assertIn("already carried", r.stderr)

    def test_plan_writes_nothing(self):
        r = self.carry("--plan", check=0)
        self.assertIn("CARRIED", r.stdout)
        self.assertEqual(os.listdir(self.dest), [])


class HostTomlTest(unittest.TestCase):
    def test_bad_values_are_refused(self):
        with tempfile.TemporaryDirectory() as t:
            for bad in ('[host]\nhostname = "a b"\n', '[host]\ntimezone = "../../etc/shadow"\n',
                        '[[mount]]\nwhat="/dev/x"\nwhere="/"\ntype="ext4"\noptions="nofail"\n',
                        '[keyboard]\nxkb_layout = "us\\"\\nOption"\n', 'nonsense = 1\n'):
                write(f"{t}/h.toml", bad)
                r = run("host", "validate", "--file", f"{t}/h.toml")
                self.assertEqual(r.returncode, 1, bad)

    def test_the_boot_hooks_table_is_valid(self):
        # The bridge writes [boot] layout = "custom-cfg" for the Rime boot hook; apply must not refuse that file.
        with tempfile.TemporaryDirectory() as t:
            write(f"{t}/h.toml", 'schema = 1\n[boot]\nlayout = "custom-cfg"\nkernel_params = ["quiet"]\n')
            r = run("host", "validate", "--file", f"{t}/h.toml")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            r = run("host", "show", "--file", f"{t}/h.toml")
            self.assertIn('[boot]\nlayout = "custom-cfg"\nkernel_params = ["quiet"]', r.stdout)
            for bad in ('[boot]\nlayout = "grub"\n', '[boot]\nkernel_params = "quiet"\n', '[boot]\nentry = 1\n'):
                write(f"{t}/h.toml", bad)
                self.assertEqual(run("host", "validate", "--file", f"{t}/h.toml").returncode, 1, bad)


class GeneratorTest(unittest.TestCase):
    def generate(self, toml):
        t = tempfile.mkdtemp()
        write(f"{t}/host.toml", toml)
        os.makedirs(f"{t}/out")
        cmd = [GENERATOR] if GENERATOR else [PROG, "generate-mounts"]
        r = subprocess.run(cmd + [f"{t}/out", f"{t}/early", f"{t}/late"], capture_output=True, text=True,
                           env=dict(os.environ, RIME_HOST_TOML=f"{t}/host.toml"))
        self.assertEqual(r.returncode, 0, r.stderr)
        return f"{t}/out"

    def test_a_nofail_mount_is_wanted_and_does_not_hold_the_boot(self):
        out = self.generate('schema = 1\n[[mount]]\nwhat = "UUID=8c87"\nwhere = "/var/lab"\ntype = "ext4"\n'
                            'options = "defaults,noatime,nofail"\nfreq = 0\npasso = 2\n'.replace("passo", "passno"))
        unit = open(f"{out}/var-lab.mount").read()
        self.assertIn("What=/dev/disk/by-uuid/8c87\n", unit)
        self.assertIn("Options=defaults,noatime,nofail\n", unit)
        self.assertNotIn("Before=local-fs.target", unit)
        self.assertIn("Requires=systemd-fsck@dev-disk-by\\x2duuid-8c87.service", unit)
        self.assertTrue(os.path.islink(f"{out}/local-fs.target.wants/var-lab.mount"))
        self.assertFalse(os.path.exists(f"{out}/local-fs.target.requires"))

    def test_a_mount_without_nofail_is_required_as_fstab_would(self):
        out = self.generate('[[mount]]\nwhat = "/dev/vdb"\nwhere = "/srv/data dir"\ntype = "ext4"\n'
                            'options = "defaults"\n'.replace("/srv/data dir", "/srv/data-dir"))
        self.assertTrue(os.path.islink(f"{out}/local-fs.target.requires/srv-data\\x2ddir.mount"))
        self.assertIn("Before=local-fs.target", open(f"{out}/srv-data\\x2ddir.mount").read())

    def test_a_broken_host_toml_writes_nothing_and_exits_0(self):
        out = self.generate("[[mount\n")
        self.assertEqual(os.listdir(out), [])
        out = self.generate('[[mount]]\nwhat = "/dev/vdb"\nwhere = "/"\ntype = "ext4"\noptions = "defaults"\n')
        self.assertEqual(os.listdir(out), [])


class ApplyTest(unittest.TestCase):
    def test_apply_writes_the_facts_and_leaves_configured_files(self):
        with tempfile.TemporaryDirectory() as r:
            write(f"{r}/var/lib/rime/host.toml",
                  '[host]\nhostname = "apex"\ntimezone = "Australia/Perth"\n[locale]\nLANG = "en_US.UTF-8"\n'
                  '[keyboard]\nconsole = "uk"\nxkb_layout = "gb"\n')
            write(f"{r}/etc/zoneinfo/Australia/Perth", "TZif")
            os.makedirs(f"{r}/etc/static")
            write(f"{r}/etc/static/locale.conf", "LANG=C\n")
            os.symlink("/etc/static/locale.conf", f"{r}/etc/locale.conf")
            run("apply", "--root", r, check=0)
            self.assertEqual(open(f"{r}/etc/hostname").read(), "apex\n")
            self.assertEqual(os.readlink(f"{r}/etc/localtime"), "/etc/zoneinfo/Australia/Perth")
            self.assertEqual(os.readlink(f"{r}/etc/locale.conf"), "/etc/static/locale.conf")
            self.assertEqual(open(f"{r}/etc/vconsole.conf").read(), "KEYMAP=uk\n")
            self.assertIn('Option "XkbLayout" "gb"', open(f"{r}/etc/X11/xorg.conf.d/00-keyboard.conf").read())
            # a locale.conf that no configuration manages is the machine's: written from host.toml
            os.unlink(f"{r}/etc/locale.conf")
            run("apply", "--root", r, check=0)
            self.assertEqual(open(f"{r}/etc/locale.conf").read(), "LANG=en_US.UTF-8\n")
            # without xkb_* the file it wrote goes; a file it did not write stays
            write(f"{r}/var/lib/rime/host.toml", '[host]\nhostname = "apex"\n')
            run("apply", "--root", r, check=0)
            self.assertFalse(os.path.exists(f"{r}/etc/X11/xorg.conf.d/00-keyboard.conf"))
            # ... unless the system configuration has a keyboard: then that one (rime-keymap-env reads it)
            write(f"{r}/etc/rime/hostlayer/00-keyboard.conf", 'Option "XkbLayout" "us"\n')
            run("apply", "--root", r, check=0)
            self.assertIn('Option "XkbLayout" "us"', open(f"{r}/etc/X11/xorg.conf.d/00-keyboard.conf").read())
            write(f"{r}/var/lib/rime/host.toml", '[keyboard]\nxkb_layout = "de"\n')
            run("apply", "--root", r, check=0)
            kb = open(f"{r}/etc/X11/xorg.conf.d/00-keyboard.conf").read()
            self.assertIn('Option "XkbLayout" "de"', kb)
            self.assertNotIn('"us"', kb)

    def test_a_machine_with_no_host_toml_gets_the_default_name(self):
        with tempfile.TemporaryDirectory() as r:
            os.makedirs(f"{r}/etc")
            run("apply", "--root", r, check=0)
            self.assertEqual(open(f"{r}/etc/hostname").read(), "rime\n")


class SubidTest(unittest.TestCase):
    def test_carried_ranges_survive_a_rewrite_by_nixos(self):
        with tempfile.TemporaryDirectory() as r:
            write(f"{r}/etc/passwd", "andre:x:1000:1000::/h:/bin/sh\nghrunner:x:960:960::/h:/bin/sh\n"
                                     "rime:x:1001:100::/h:/bin/sh\n")
            write(f"{r}/etc/subuid", SUBIDS)
            write(f"{r}/etc/subgid", SUBIDS)
            run("subid", "save", "--root", r, check=0)
            # what update-users-groups does: only declared users' ranges, one of them overlapping andre's
            write(f"{r}/etc/subuid", "rime:100000:65536\n")
            write(f"{r}/etc/subgid", "rime:500000:65536\n")
            res = run("subid", "restore", "--root", r, check=0)
            self.assertEqual(open(f"{r}/etc/subuid").read(), "rime:100000:65536\n" + SUBIDS)
            self.assertEqual(open(f"{r}/etc/subgid").read(), "rime:500000:65536\nghrunner:1000000:65536\n")
            self.assertIn("andre's range 524288:65536 overlaps rime's 500000:65536", res.stderr)


class SwitchTest(unittest.TestCase):
    def test_markers_follow_the_registry(self):
        with tempfile.TemporaryDirectory() as t:
            write(f"{t}/reg", "rime-guest-session@.service off\nrime-firewall.service on\n")
            env = {"RIME_SWITCH_REGISTRY": f"{t}/reg", "RIME_SWITCH_DIR": f"{t}/sw"}
            self.assertEqual(run("switch", "get", "rime-guest-session@1001.service", env=env, check=0).stdout,
                             "off\n")
            run("switch", "on", "rime-guest-session@1001.service", env=env, check=0)
            self.assertTrue(os.path.exists(f"{t}/sw/rime-guest-session@1001.service.on"))
            self.assertEqual(run("switch", "get", "rime-guest-session@1001.service", env=env, check=0).stdout,
                             "on\n")
            run("switch", "off", "rime-firewall.service", env=env, check=0)
            self.assertTrue(os.path.exists(f"{t}/sw/rime-firewall.service.off"))
            run("switch", "on", "rime-firewall.service", env=env, check=0)
            self.assertFalse(os.path.exists(f"{t}/sw/rime-firewall.service.off"))
            for bad in ("sshd.service", "../etc.service", "rime-guest-session@../x.service", "a b"):
                r = run("switch", "on", bad, env=env)
                self.assertEqual(r.returncode, 2, bad)


if __name__ == "__main__":
    unittest.main()
