# The host layer (PLAN.md D2) on a synthetic bootc root (nix/tests/hostlayer-fixture.nix), carried into the
# rime-vm host at its first activation:
#   - accounts: ids, subordinate ranges and password hashes kept exactly; zsh rewritten to the system's;
#     a Fedora account on a NixOS static id left behind and named; a PERSON on one refused by name;
#   - machine-id, ssh host keys (as sshd serves them), NetworkManager connections, bluetooth, printers;
#   - host.toml: hostname, timezone, locale, console + X11 keyboard applied; the fstab's extra disks become
#     generated nofail mounts, and their absence does not stop the boot;
#   - the person logs in through greetd with the carried password, the greeter remembering them;
#   - the runtime switches: `rime-user guest enable` (what used to be `systemctl enable`) on a new account,
#     whose session is then wiped at logout; the carried guest's switch; rimed's Switch interface under polkit.
{ pkgs, self }:

let
  fixture = import ./hostlayer-fixture.nix { inherit pkgs; };
  common = import ./hostlayer-common.nix { inherit pkgs self; };
in
pkgs.testers.runNixOSTest {
  name = "rime-hostlayer";
  node.pkgsReadOnly = false;

  nodes.machine =
    { lib, ... }:
    {
      imports = [
        (common.node {
          from = "/var/lib/rime-test/src";
          # Out of the store, with the owners and modes a store path cannot hold.
          prepare = ''
            rm -rf /var/lib/rime-test/src
            cp -r ${fixture}/root /var/lib/rime-test/src
            chmod -R u+w /var/lib/rime-test/src
            while read -r mode uid gid path; do
              chown "$uid:$gid" "/var/lib/rime-test/src/$path"
              chmod "$mode" "/var/lib/rime-test/src/$path"
            done < ${fixture}/perms
          '';
        })
      ];
      # The guest logs in on tty2 through getty, whose screen the test driver reads from /dev/vcs2. Rime's
      # kmscon console (desktop.nix, checks.polish proves it) draws on DRM and leaves vcs2 empty, so this node
      # keeps the stock getty on the other VTs.
      services.kmscon.enable = lib.mkForce false;
      # A test-only grant, so rimed's Switch can be shown to work for someone polkit allows (and only them).
      security.polkit.extraConfig = ''
        polkit.addRule(function (action, subject) {
          if (action.id == "org.rimeos.rimed.manage-switches" && subject.user == "rime") {
            return polkit.Result.YES;
          }
        });
      '';
    };

  testScript = common.checks + ''
    SRC = "/var/lib/rime-test/src"
    FIX = "${fixture}"
    BUS = "busctl --system call org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Switch"

    def as_user(user, cmd):
        return "su - " + user + " -s /bin/sh -c " + shlex.quote(cmd)

    machine.wait_for_unit("multi-user.target")

    with subtest("the carry ran once, at the first activation"):
        carry_ok(machine)
        report = machine.succeed("cat /var/lib/rime/carry/report.txt")
        machine.succeed("stat -c '%a %U' /var/lib/rime/carry/report.json | grep -x '600 root'")

    with subtest("accounts: ids, ranges and hashes exact; the login shell is the system's"):
        check_accounts(machine, SRC, ["andre", "visitor", "ghrunner"])
        # A system account NixOS declares without a number keeps the old one (and its files their owner).
        machine.succeed("getent passwd greetd | cut -d: -f3 | grep -x 973")
        old_hash = machine.succeed(f"awk -F: '$1==\"andre\"{{print $2}}' {SRC}/etc/shadow").strip()
        assert machine.succeed("getent shadow andre | cut -d: -f2").strip() == old_hash, "andre's hash changed"
        assert "wheel" in machine.succeed("id -nG andre").split(), "andre lost wheel"
        # A Fedora account on a NixOS static id is not carried, and the report names it.
        machine.succeed("getent passwd messagebus | grep -x 'messagebus:x:4:4:.*'")
        machine.fail("getent passwd fedorabus")
        assert "fedorabus" in report, report
        # NixOS's own declared people still get their auto ranges; the carried ones survive another activation.
        machine.succeed("grep '^rime:' /etc/subuid")
        machine.succeed("/run/current-system/activate")  # every activation snippet again, users included
        check_accounts(machine, SRC, ["andre", "ghrunner"])

    with subtest("a person on a NixOS static id is refused, by name, and nothing is written"):
        rc, out = machine.execute(f"{HL} carry --from {FIX}/collision --root /tmp/croot --system /run/current-system 2>&1")
        print(rc, out)
        assert rc == 2, f"carry exited {rc}"
        assert "refusing:" in out and "builder" in out and "messagebus" in out, out
        machine.fail("test -e /tmp/croot/etc/passwd")
        machine.fail("test -e /tmp/croot/var/lib/rime/carry/done")

    with subtest("files: machine-id, ssh host keys, NetworkManager, bluetooth, printers, Rime state"):
        check_files(machine, SRC)
        machine.succeed("stat -c '%a %U' '/etc/NetworkManager/system-connections/Fixture Wired.nmconnection' | grep -x '600 root'")
        machine.succeed("test -s '/var/lib/bluetooth/AA:BB:CC:DD:EE:FF/settings'")
        machine.succeed("grep -q 'Printer Fixture' /var/lib/cups/printers.conf")
        machine.succeed("grep -qx visitor /var/lib/rime/guest-accounts")
        machine.succeed("test \"$(readlink /etc/rime/guest-accounts)\" = /var/lib/rime/guest-accounts")
        machine.succeed("stat -c '%a' /var/lib/rime/sing-box/config.json | grep -x 600")
        machine.succeed("test -f /etc/sing-box/config.json")
        machine.succeed("grep -qx 'tcp 2222' /var/lib/rime/firewall.d/fixture-ssh.conf")
        machine.wait_for_unit("rime-firewall.service")
        fw = machine.succeed(f"{SW}/libexec/rime-firewall status 2>&1")
        print(fw)
        assert "fixture-ssh" in fw, "rime-firewall does not read the carried exception"
        machine.succeed("stat -c '%U' /var/lib/rime-greet/last-user | grep -x greetd")

    with subtest("the report: what was only checked, left behind, and the private key named, never copied"):
        print(report)
        assert "/etc/npmrc: not carried; keys set there: registry" in report, report
        assert "fixture-runner.service: a unit written on this machine" in report, report
        assert "/etc/pki/akmods/private/fixture_signing.priv" in report, report
        assert "/etc/motd" not in report, "the image's own default was reported as left behind"
        machine.fail("find / -xdev -name fixture_signing.priv -not -path '/var/lib/rime-test/*' | grep .")
        machine.fail("test -e /etc/npmrc")

    with subtest("host.toml reaches the system"):
        print(machine.succeed(f"{HL} host show"))
        machine.succeed(f"{HL} host validate")
        assert machine.succeed("hostname").strip() == "fixturebox"
        assert machine.succeed("cat /etc/hostname").strip() == "fixturebox"
        machine.succeed("readlink -f /etc/localtime | grep 'Australia/Perth$'")
        assert machine.succeed("date +%Z").strip() == "AWST"
        machine.succeed("grep -x 'LC_TIME=C.UTF-8' /etc/locale.conf")
        machine.succeed("grep -x 'KEYMAP=uk' /etc/vconsole.conf")
        kb = machine.succeed("cat /etc/X11/xorg.conf.d/00-keyboard.conf")
        assert '"XkbLayout" "gb"' in kb and '"XkbOptions" "caps:escape"' in kb, kb
        luks = json.loads(machine.succeed(f"{HL} host get luks"))
        assert luks[0]["name"] == "luks-fixture" and luks[0]["tpm2"] is True, luks

    with subtest("the old disks: generated nofail mounts, and the boot went on without them"):
        check_mounts(machine)
        machine.succeed("grep -x 'What=/dev/disk/by-uuid/f1a70001-0000-4000-8000-00000000f1a7' "
                        "/run/systemd/generator/var-home-andre-Games.mount")
        machine.succeed("grep -x 'Options=defaults,nofail' /run/systemd/generator/var-home-andre-Games.mount")

    with subtest("the greeter types in host.toml's keymap, not the configuration's us"):
        # The password is typed at the greeter: on a gb machine its sway must read gb (installer2 OVMF run).
        machine.wait_until_succeeds("pgrep -u greetd -x sway")
        env = machine.succeed("tr '\\0' '\\n' < /proc/$(pgrep -u greetd -x sway | head -n1)/environ | grep '^XKB_' || true")
        print("greeter sway XKB environment:", env)
        assert "XKB_DEFAULT_LAYOUT=gb" in env.split() and "XKB_DEFAULT_OPTIONS=caps:escape" in env.split(), env

    with subtest("andre logs in through greetd with the carried password"):
        greeter_login(machine, "andre", "rime")
        machine.succeed("test \"$(stat -c %U /var/home/andre)\" = andre")

    with subtest("rime-user makes a disposable guest on NixOS, and its logout wipes it"):
        machine.succeed(f"{SW}/libexec/rime-user add kid")
        machine.succeed("echo kid:rime | chpasswd")
        uid = machine.succeed("id -u kid").strip()
        unit = f"rime-guest-session@{uid}.service"
        out = machine.succeed(f"{SW}/libexec/rime-user guest enable kid 2>&1")
        print(out)
        machine.succeed(f"test -f /var/lib/rime/switch/{unit}.on")
        machine.succeed("grep -qx kid /var/lib/rime/guest-accounts")
        assert machine.succeed(f"{HL} switch get {unit}").strip() == "on"
        home = machine.succeed("getent passwd kid | cut -d: -f6").strip()
        machine.succeed("chvt 2")
        machine.wait_until_tty_matches("2", "login: ")
        machine.send_chars("kid\n")
        machine.wait_until_tty_matches("2", "Password: ")
        machine.send_chars("rime\n")
        machine.wait_until_succeeds(f"systemctl is-active {unit}", timeout=60)
        machine.succeed(f"su kid -s /bin/sh -c 'echo guest-was-here > {home}/note.txt'")
        machine.succeed(f"test -f {home}/note.txt")
        # Log out: end the tty2 session, as `exit` would (typing it races zsh's first start against
        # rime-shell-firstrun seeding ~/.zshrc). Not terminate-user: that stops user@ with no UserStopDelaySec
        # while the session is still closing, and the wipe's live-session fence then refuses (as designed).
        # A terminate sent while login(1) is still opening the session (pam, the keyring) can be lost, so wait
        # for kid's shell on tty2 first, and ask again until logind has removed the session.
        machine.wait_until_succeeds("pgrep -u kid -t tty2 >/dev/null", timeout=60)
        print(machine.succeed("loginctl list-sessions --no-legend"))
        user_sessions = ("loginctl list-sessions --no-legend | awk '$3==\"kid\"{print $1}' | while read -r s; do "
                         "if [ \"$(loginctl show-session $s -p Class --value)\" = user ]; then echo $s; fi; done")
        assert machine.succeed(user_sessions).split(), "no login session of kid's to end"
        machine.wait_until_succeeds(
            f"for s in $({user_sessions}); do loginctl terminate-session $s; done; "
            f"sleep 2; test -z \"$({user_sessions})\"", timeout=60)
        machine.wait_until_succeeds(f"! systemctl is-active {unit}", timeout=120)
        machine.wait_until_succeeds(f"test ! -e {home}/note.txt", timeout=30)
        journal = machine.succeed(f"journalctl -b -u {unit} --no-pager")
        print(journal)
        assert "cleared the home directory" in journal, journal
        machine.succeed("chvt 1")
        # Every other login also pulls the unit in; the switch keeps it off for them.
        machine.succeed("systemctl show -p ConditionResult rime-guest-session@1000.service | grep -x ConditionResult=no")
        machine.fail("systemctl is-active rime-guest-session@1000.service")
        # The carried guest's switch came from bootc's wants link.
        machine.succeed("test -f /var/lib/rime/switch/rime-guest-session@1001.service.on")
        machine.succeed(f"{SW}/libexec/rime-user guest disable kid")
        machine.succeed(f"test ! -e /var/lib/rime/switch/{unit}.on")

    with subtest("rimed's Switch: polkit decides, and only registered units are switches"):
        print(machine.succeed(f"{BUS} List"))
        rc, out = machine.execute(as_user("andre", f"{BUS} Set sb rime-firewall.service false 2>&1"))
        print("andre (no grant):", rc, out)
        assert rc != 0, "rimed let a caller polkit refused flip a switch"
        machine.succeed("test ! -e /var/lib/rime/switch/rime-firewall.service.off")
        rc, out = machine.execute(as_user("rime", f"{BUS} Set sb sshd.service false 2>&1"))
        assert rc != 0 and "not a runtime switch" in out, out
        machine.succeed(as_user("rime", f"{BUS} Set sb rime-firewall.service false"))
        machine.succeed("test -f /var/lib/rime/switch/rime-firewall.service.off")
        assert "false" in machine.succeed(f"{BUS} Get s rime-firewall.service")
        machine.succeed("systemctl restart rime-firewall.service")
        machine.succeed("systemctl show -p ConditionResult rime-firewall.service | grep -x ConditionResult=no")
        machine.succeed(as_user("rime", f"{BUS} Set sb rime-firewall.service true"))
        machine.succeed("test ! -e /var/lib/rime/switch/rime-firewall.service.off")
        machine.succeed("systemctl restart rime-firewall.service")
        machine.succeed("systemctl is-active rime-firewall.service")
  '';
}
