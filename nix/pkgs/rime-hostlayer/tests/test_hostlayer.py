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
        write(f"{e}/machine-id", "0f1a70000000400080000000c0ffee01\n", 0o444)
        write(f"{e}/hostname", "apex\n")
        os.symlink("../usr/share/zoneinfo/Australia/Perth", f"{e}/localtime")
        write(f"{e}/locale.conf", "# comment\nLANG=en_US.UTF-8\n")
        write(f"{e}/vconsole.conf", "KEYMAP=uk\n")
        write(f"{e}/X11/xorg.conf.d/00-keyboard.conf",
              'Section "InputClass"\n Identifier "system-keyboard"\n Option "XkbLayout" "gb"\n'
              ' Option "XkbVariant" "extd"\nEndSection\n')
        write(f"{e}/fstab",
              "UUID=a41f / btrfs subvol=root 0 0\n"
              "UUID=f1a70001 /var/home/andre/Games ext4 defaults,noatime,nofail 0 2\n"
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
        self.assertEqual(self.read("etc/machine-id"), "0f1a70000000400080000000c0ffee01\n")
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

    def test_the_files_a_vpn_profile_names_are_carried_with_it(self):
        # An OpenVPN profile as written by hand (certificates under /etc/openvpn) and as nmcli import writes one
        # (under the person's home, already on the disk the new system boots from); one names a file that is gone.
        e = f"{self.src}/etc"
        write(f"{e}/openvpn/client/ca.crt", "CA-TEST\n", 0o644)
        write(f"{e}/openvpn/client/me.key", "KEY-TEST\n", 0o600)
        write(f"{self.src}/var/home/andre/.cert/nm-openvpn/work-ta.pem", "TA-TEST\n", 0o600)
        write(f"{self.dest}/var/home/andre/.cert/nm-openvpn/work-ta.pem", "TA-TEST\n", 0o600)
        write(f"{e}/NetworkManager/system-connections/Work.nmconnection",
              "[connection]\nid=Work\ntype=vpn\n\n[vpn]\n"
              "service-type=org.freedesktop.NetworkManager.openvpn\nremote=vpn.example\n"
              "ca=/etc/openvpn/client/ca.crt\nkey=/etc/openvpn/client/me.key\n"
              "ta=/var/home/andre/.cert/nm-openvpn/work-ta.pem\ncert=/etc/openvpn/client/gone.crt\n", 0o600)
        self.carry(check=0)
        self.assertEqual(self.read("etc/openvpn/client/ca.crt"), "CA-TEST\n")
        self.assertEqual(self.read("etc/openvpn/client/me.key"), "KEY-TEST\n")
        self.assertEqual(oct(os.stat(f"{self.dest}/etc/openvpn/client/me.key").st_mode & 0o777), "0o600")
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        carried = {c["from"] for c in rep["carried"]}
        self.assertIn("/etc/openvpn/client/ca.crt", carried)
        self.assertIn("/etc/openvpn/client/me.key", carried)
        self.assertNotIn("/var/home/andre/.cert/nm-openvpn/work-ta.pem", carried, "a home file was rewritten")
        checked = {c["path"]: c["result"] for c in rep["checked"]}
        self.assertIn("already on this disk", checked["/var/home/andre/.cert/nm-openvpn/work-ta.pem"])
        left = {l["path"]: l["why"] for l in rep["left_behind"]}
        self.assertIn("named by /etc/NetworkManager/system-connections/Work.nmconnection", left["/etc/openvpn/client/gone.crt"])
        # Carried with the profile, so neither "left behind" nor a private key "not copied".
        self.assertNotIn("/etc/openvpn/client/ca.crt", left)
        self.assertNotIn("/etc/openvpn/client/me.key", rep["not_copied_private_keys"])
        self.assertNotIn("KEY-TEST", json.dumps(rep))

    def test_a_profile_naming_a_path_the_system_manages_does_not_carry_it(self):
        # An 802.1X profile trusting the distribution's CA bundle: NixOS's configuration provides that path (the
        # toplevel's /etc, or a link into /etc/static already in the new root). The old bytes are never written over it.
        e = f"{self.src}/etc"
        write(f"{e}/pki/tls/certs/ca-bundle.crt", "FEDORA-BUNDLE\n")
        write(f"{e}/ssl/certs/ca-certificates.crt", "FEDORA-BUNDLE-2\n")
        write(f"{self.sys}/etc/pki/tls/certs/ca-bundle.crt", "NIXOS-BUNDLE\n")
        os.makedirs(f"{self.dest}/etc/ssl/certs")
        os.symlink("/etc/static/ssl/certs/ca-certificates.crt", f"{self.dest}/etc/ssl/certs/ca-certificates.crt")
        write(f"{e}/NetworkManager/system-connections/Eduroam.nmconnection",
              "[connection]\nid=Eduroam\ntype=wifi\n\n[802-1x]\neap=peap;\n"
              "ca-cert=/etc/pki/tls/certs/ca-bundle.crt\nclient-cert=/etc/ssl/certs/ca-certificates.crt\n", 0o600)
        self.carry(check=0)
        self.assertFalse(os.path.lexists(f"{self.dest}/etc/pki/tls/certs/ca-bundle.crt"), "a managed path was carried")
        self.assertEqual(os.readlink(f"{self.dest}/etc/ssl/certs/ca-certificates.crt"),
                         "/etc/static/ssl/certs/ca-certificates.crt", "the /etc/static link was replaced")
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        carried = {c["to"] for c in rep["carried"]}
        self.assertNotIn("/etc/pki/tls/certs/ca-bundle.crt", carried)
        self.assertNotIn("/etc/ssl/certs/ca-certificates.crt", carried)
        checked = {c["path"]: c["result"] for c in rep["checked"]}
        self.assertIn("set by the new system's configuration (in the new system's /etc)",
                      checked["/etc/pki/tls/certs/ca-bundle.crt"])
        self.assertIn("set by the new system's configuration (a link into /etc/static)",
                      checked["/etc/ssl/certs/ca-certificates.crt"])
        left = {l["path"] for l in rep["left_behind"]}
        self.assertNotIn("/etc/pki/tls/certs/ca-bundle.crt", left)

    def test_a_profile_naming_a_link_to_a_managed_path_gets_the_link(self):
        # A VPN profile's ca= is a link to the distribution's CA bundle, which the new system provides. The bundle's old
        # bytes are not carried, but the named path must still exist: the link is carried, pointing at the new copy.
        e = f"{self.src}/etc"
        write(f"{e}/ssl/certs/ca-certificates.crt", "FEDORA-BUNDLE\n")
        os.makedirs(f"{e}/openvpn/client")
        os.symlink("/etc/ssl/certs/ca-certificates.crt", f"{e}/openvpn/client/ca.crt")
        write(f"{self.sys}/etc/ssl/certs/ca-certificates.crt", "NIXOS-BUNDLE\n")
        write(f"{e}/NetworkManager/system-connections/V.nmconnection",
              "[connection]\nid=V\ntype=vpn\n\n[vpn]\nca=/etc/openvpn/client/ca.crt\n", 0o600)
        self.carry(check=0)
        self.assertEqual(os.readlink(f"{self.dest}/etc/openvpn/client/ca.crt"), "/etc/ssl/certs/ca-certificates.crt")
        self.assertFalse(os.path.lexists(f"{self.dest}/etc/ssl/certs/ca-certificates.crt"), "a managed path was carried")
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        links = {c["to"]: c.get("link") for c in rep["carried"]}
        self.assertEqual(links.get("/etc/openvpn/client/ca.crt"), "/etc/ssl/certs/ca-certificates.crt")
        self.assertNotIn("/etc/openvpn/client/ca.crt", {l["path"] for l in rep["left_behind"]})
        # A second carry finds the same link and leaves it.
        self.carry("--force", check=0)
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        self.assertIn("the same link", {c["path"]: c["result"] for c in rep["checked"]}["/etc/openvpn/client/ca.crt"])
        self.assertEqual(os.readlink(f"{self.dest}/etc/openvpn/client/ca.crt"), "/etc/ssl/certs/ca-certificates.crt")

    def test_a_symlinked_file_a_profile_names_is_followed_inside_the_old_root(self):
        # A cert kept as a link (relative, absolute, and through a linked directory) is carried as the bytes it pointed
        # at, to the path the profile names; a link out of the root is read against the root; a dangling or looping
        # link is reported as missing, never as carried.
        e = f"{self.src}/etc"
        write(f"{e}/openvpn/keys/ca-2026.crt", "CA-LINKED\n", 0o644)
        write(f"{e}/openvpn/keys/me-2026.key", "KEY-LINKED\n", 0o600)
        os.makedirs(f"{e}/openvpn/client")
        os.symlink("../keys/ca-2026.crt", f"{e}/openvpn/client/ca.crt")
        os.symlink("/etc/openvpn/keys/me-2026.key", f"{e}/openvpn/client/me.key")
        os.symlink("keys", f"{e}/openvpn/current")
        os.symlink("/../../etc/openvpn/keys/ca-2026.crt", f"{e}/openvpn/client/up.crt")
        os.symlink("nowhere.crt", f"{e}/openvpn/client/dangling.crt")
        os.symlink("loop-b.crt", f"{e}/openvpn/client/loop-a.crt")
        os.symlink("loop-a.crt", f"{e}/openvpn/client/loop-b.crt")
        write(f"{e}/NetworkManager/system-connections/Linked.nmconnection",
              "[connection]\nid=Linked\ntype=vpn\n\n[vpn]\nservice-type=org.freedesktop.NetworkManager.openvpn\n"
              "ca=/etc/openvpn/client/ca.crt\nkey=/etc/openvpn/client/me.key\n"
              "ta=/etc/openvpn/current/ca-2026.crt\ncert=/etc/openvpn/client/up.crt\n"
              "static-key=/etc/openvpn/client/dangling.crt\n\n[802-1x]\nca-cert=/etc/openvpn/keys/ca-2026.crt\n", 0o600)
        write(f"{e}/NetworkManager/system-connections/Loop.nmconnection",
              "[connection]\nid=Loop\ntype=vpn\n\n[vpn]\nca=/etc/openvpn/client/loop-a.crt\n", 0o600)
        self.carry(check=0)
        for rel, want in (("etc/openvpn/client/ca.crt", "CA-LINKED\n"), ("etc/openvpn/client/me.key", "KEY-LINKED\n"),
                          ("etc/openvpn/current/ca-2026.crt", "CA-LINKED\n"), ("etc/openvpn/client/up.crt", "CA-LINKED\n"),
                          ("etc/openvpn/keys/ca-2026.crt", "CA-LINKED\n")):
            self.assertFalse(os.path.islink(f"{self.dest}/{rel}"), rel)
            self.assertEqual(self.read(rel), want, rel)
        self.assertEqual(oct(os.stat(f"{self.dest}/etc/openvpn/client/me.key").st_mode & 0o777), "0o600")
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        moves = {c["to"]: c["from"] for c in rep["carried"]}
        self.assertEqual(moves["/etc/openvpn/client/ca.crt"], "/etc/openvpn/keys/ca-2026.crt")
        self.assertEqual(moves["/etc/openvpn/client/me.key"], "/etc/openvpn/keys/me-2026.key")
        left = {}
        for l in rep["left_behind"]:
            left.setdefault(l["path"], []).append(l["why"])
        for p in ("/etc/openvpn/client/dangling.crt", "/etc/openvpn/client/loop-a.crt"):
            self.assertTrue(any("not a file on the old machine" in w for w in left[p]), (p, left[p]))
            self.assertNotIn(p, moves)
        self.assertNotIn("/etc/openvpn/client/ca.crt", left)
        self.assertNotIn("KEY-LINKED", json.dumps(rep))

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

    def test_luks_and_keymap_kernel_arguments_reach_host_toml_boot(self):
        cmdline = f"{self.tmp.name}/cmdline"
        uuid = "0b1c2d3e-0000-4000-8000-0000000000aa"
        write(cmdline, f"BOOT_IMAGE=(hd0,gpt2)/ostree/default-x/vmlinuz root=UUID=a41f rw rootflags=subvol=root "
                       f"ostree=/ostree/boot.1/default/x/0 rd.luks.uuid=luks-{uuid} rd.luks.options=tpm2-device=auto,"
                       f"discard rd.vconsole.keymap=uk rhgb quiet nvidia-drm.modeset=1\n")
        self.carry("--cmdline", cmdline, check=0)
        with open(f"{self.dest}/var/lib/rime/host.toml", "rb") as f:
            host = tomllib.load(f)
        self.assertEqual(host["boot"], {"kernel_params": [
            f"rd.luks.uuid=luks-{uuid}", "rd.luks.options=tpm2-device=auto,discard", "rd.vconsole.keymap=uk"]})
        self.assertEqual(host["luks"], [{"name": f"luks-{uuid}", "device": f"UUID={uuid}",
                                         "options": "tpm2-device=auto,discard", "tpm2": True}])
        rep = self.read("var/lib/rime/carry/report.txt")
        self.assertIn("not carried (the image's own, or port them to the system configuration): rhgb quiet "
                      "nvidia-drm.modeset=1", rep)
        self.assertNotIn("ostree=", rep.split("kernel command line", 1)[1].split("\n", 1)[0])
        run("host", "validate", "--file", f"{self.dest}/var/lib/rime/host.toml", check=0)

    def test_a_luks_argument_the_boot_menu_cannot_hold_is_refused(self):
        cmdline = f"{self.tmp.name}/cmdline"
        write(cmdline, "rd.luks.options=$weird\n")
        r = self.carry("--cmdline", cmdline, check=2)
        self.assertIn("rd.luks.options=$weird", r.stderr)
        self.assertEqual(os.listdir(self.dest), [], "a refused carry wrote something")

    def test_no_cmdline_carries_no_kernel_arguments(self):
        self.carry(check=0)
        with open(f"{self.dest}/var/lib/rime/host.toml", "rb") as f:
            self.assertNotIn("boot", tomllib.load(f))
        self.assertIn("kernel command line: not given (--cmdline)", self.read("var/lib/rime/carry/report.txt"))

    def test_host_local_tuning_is_reported_or_matched_to_the_system(self):
        e = f"{self.src}/etc"
        write(f"{e}/udev/rules.d/60-bfq-nvme.rules", 'ACTION=="add", KERNEL=="nvme0n1", ATTR{queue/scheduler}="bfq"\n')
        write(f"{e}/sysctl.d/90-writeback.conf", "# old comment\nvm.dirty_bytes = 1073741824\n")
        write(f"{e}/sysctl.d/91-edited.conf", "vm.swappiness = 10\n")
        # The new system ships 90-writeback.conf with the same setting (another comment) and its own 91-edited.conf.
        write(f"{self.sys}/etc/sysctl.d/90-writeback.conf", "# Rime's\nvm.dirty_bytes=1073741824\n")
        write(f"{self.sys}/etc/sysctl.d/91-edited.conf", "vm.swappiness = 60\n")
        self.carry(check=0)
        rep = json.loads(self.read("var/lib/rime/carry/report.json"))
        left = {e["path"]: e["why"] for e in rep["left_behind"]}
        checked = {c["path"]: c["result"] for c in rep["checked"]}
        self.assertIn("NixOS builds /etc/udev/rules.d", left["/etc/udev/rules.d/60-bfq-nvme.rules"])
        self.assertIn("the new system ships the same settings", checked["/etc/sysctl.d/90-writeback.conf"])
        self.assertNotIn("/etc/sysctl.d/90-writeback.conf", left)
        self.assertIn("ships its own file of this name", left["/etc/sysctl.d/91-edited.conf"])
        self.assertIn("/etc/udev/rules.d/60-bfq-nvme.rules: a udev rule written on this machine",
                      self.read("var/lib/rime/carry/report.txt"))

    def test_plan_writes_nothing(self):
        r = self.carry("--plan", check=0)
        self.assertIn("CARRIED", r.stdout)
        self.assertEqual(os.listdir(self.dest), [])


class HostTomlTest(unittest.TestCase):
    def test_bad_values_are_refused(self):
        with tempfile.TemporaryDirectory() as t:
            for bad in ('[host]\nhostname = "a b"\n', '[host]\ntimezone = "../../etc/shadow"\n',
                        '[[mount]]\nwhat="/dev/x"\nwhere="/"\ntype="ext4"\noptions="nofail"\n',
                        '[keyboard]\nxkb_layout = "us\\"\\nOption"\n', 'nonsense = 1\n',
                        '[boot]\nlayout = "grub"\n', '[boot]\nkernel_params = "rd.luks.uuid=x"\n',
                        '[boot]\nkernel_params = ["a b"]\n', '[boot]\nkernel_params = ["a=$(id)"]\n',
                        '[boot]\nlayout = "systemd-boot"\n', '[boot]\nentries = 3\n',
                        '[boot]\nmicrocode = "arm"\n', '[boot]\nmicrocode = true\n'):
                write(f"{t}/h.toml", bad)
                r = run("host", "validate", "--file", f"{t}/h.toml")
                self.assertEqual(r.returncode, 1, bad)

    def test_the_boot_table_rime_boot_reads_is_valid(self):
        with tempfile.TemporaryDirectory() as t:
            write(f"{t}/h.toml", 'schema = 1\n[boot]\nlayout = "custom-cfg"\n'
                                 'kernel_params = ["rd.luks.uuid=luks-0b1c", "rd.vconsole.keymap=uk"]\n')
            run("host", "validate", "--file", f"{t}/h.toml", check=0)
            r = run("host", "show", "--file", f"{t}/h.toml", check=0)
            self.assertIn('[boot]\nlayout = "custom-cfg"\nkernel_params = ["rd.luks.uuid=luks-0b1c", '
                          '"rd.vconsole.keymap=uk"]\n', r.stdout)


    def test_the_boot_partition_the_bridge_records(self):
        # A migrated LUKS-A machine: /boot is ext4 on its own partition. The bridge writes [boot] device + fstype;
        # rime.rootfs's generator mounts it. `host show` (render_host_toml) must keep both, validate must accept them.
        with tempfile.TemporaryDirectory() as t:
            write(f"{t}/h.toml", '[boot]\nlayout = "custom-cfg"\ndevice = "UUID=b007b007-0000-4000-8000-000000000b00"\n'
                  'fstype = "ext4"\n')
            r = run("host", "validate", "--file", f"{t}/h.toml")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            r = run("host", "show", "--file", f"{t}/h.toml")
            self.assertIn('device = "UUID=b007b007-0000-4000-8000-000000000b00"\nfstype = "ext4"', r.stdout)
            for bad in ('[boot]\ndevice = "UUID=x y"\n', '[boot]\nfstype = "Ext4;rm"\n', '[boot]\ndevice = 3\n'):
                write(f"{t}/h.toml", bad)
                self.assertEqual(run("host", "validate", "--file", f"{t}/h.toml").returncode, 1, bad)

    def test_the_boot_microcode_the_hook_reads_is_valid(self):
        # nix/pkgs/rime-boot reads [boot] microcode (the installer writes the CPU's vendor); every value the hook
        # takes is valid here and is written back by `host show`.
        with tempfile.TemporaryDirectory() as t:
            for v in ("both", "intel", "amd", "none"):
                write(f"{t}/h.toml", f'[boot]\nlayout = "bls"\nmicrocode = "{v}"\nkernel_params = ["quiet"]\n')
                r = run("host", "validate", "--file", f"{t}/h.toml")
                self.assertEqual(r.returncode, 0, v + r.stdout + r.stderr)
                r = run("host", "show", "--file", f"{t}/h.toml", check=0)
                self.assertIn(f'[boot]\nlayout = "bls"\nmicrocode = "{v}"\nkernel_params = ["quiet"]\n', r.stdout)

    def test_the_boot_table_the_hook_reads_is_valid(self):
        # nix/pkgs/rime-boot reads [boot] layout + kernel_params (rd.luks.uuid= of a LUKS root, D9); the host layer
        # must accept it, or one [boot] table drops every other fact of the machine ("not applied").
        with tempfile.TemporaryDirectory() as t:
            write(f"{t}/h.toml", '[host]\nhostname = "lab"\n[boot]\nlayout = "custom-cfg"\n'
                  'kernel_params = ["rd.luks.uuid=5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6", "quiet"]\n')
            r = run("host", "validate", "--file", f"{t}/h.toml")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

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

    def test_a_network_mount_waits_for_the_network_and_hangs_from_remote_fs(self):
        out = self.generate('[[mount]]\nwhat = "nas:/export"\nwhere = "/mnt/nas"\ntype = "nfs"\n'
                            'options = "_netdev,nofail"\npassno = 2\n'
                            '[[mount]]\nwhat = "//nas/share"\nwhere = "/mnt/share"\ntype = "cifs"\n'
                            'options = "credentials=/etc/smb"\n'
                            '[[mount]]\nwhat = "/dev/disk/by-label/iscsi"\nwhere = "/mnt/iscsi"\ntype = "ext4"\n'
                            'options = "_netdev,nofail"\n')
        nfs = open(f"{out}/mnt-nas.mount").read()
        self.assertIn("After=network-online.target\nWants=network-online.target\n", nfs)
        self.assertNotIn("local-fs.target", nfs)
        self.assertNotIn("systemd-fsck", nfs, "a network filesystem is never fsck'd")
        self.assertTrue(os.path.islink(f"{out}/remote-fs.target.wants/mnt-nas.mount"))
        # No nofail: the boot waits for it, as remote-fs.target does for an fstab line without nofail.
        self.assertIn("Before=remote-fs.target", open(f"{out}/mnt-share.mount").read())
        self.assertTrue(os.path.islink(f"{out}/remote-fs.target.requires/mnt-share.mount"))
        # _netdev on a block device makes it a network mount too (iSCSI).
        self.assertTrue(os.path.islink(f"{out}/remote-fs.target.wants/mnt-iscsi.mount"))
        self.assertFalse(os.path.exists(f"{out}/local-fs.target.wants"))
        self.assertFalse(os.path.exists(f"{out}/local-fs.target.requires"))

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
