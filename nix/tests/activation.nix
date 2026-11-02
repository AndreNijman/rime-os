# Activation on a running, logged-in Rime machine (docs/nixos/p2-gates.md A1) and two host-layer handovers:
#
#   A1  greetd is not restarted by `switch-to-configuration switch`: a person logs in through the real Rime greeter
#       (rime-greet) into Hyprland; a switch to a generation whose greetd unit CHANGED keeps greetd's MainPID, the
#       person's logind session and their compositor. A built-in mutant generation (restartIfChanged forced on)
#       must restart greetd, so the check is seen to catch a restart.
#   firewall.d  on a fresh machine /etc/rime/firewall.d resolves before the first exception is ever written
#       (Rime Shell's firewall view reads it) and `rime-firewall allow` lands there.
#   switch --now  `rime-hostlayer switch off|on rime-firewall.service --now` takes the firewall down and up at once,
#       and a start while the switch is off is refused by the unit's condition.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-activation";
  node.pkgsReadOnly = false;

  nodes.machine =
    { lib, ... }:
    {
      imports = [
        self.nixosModules.default
        ../hosts/vm.nix
      ];
      virtualisation.qemu.options = [
        "-vga none"
        "-device virtio-gpu-pci,xres=1280,yres=800"
      ];
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;
      # The shipped profile turns the Rime boot hook on (nix/hosts/profile.nix), and the hook rightly refuses a
      # machine with no /boot, which this QEMU node is (no ESP, no Fedora GRUB): `switch` would then exit 1 at
      # "Failed to install bootloader" before A1 is even asked. A1 is about greetd, not the menu; the hook has its own
      # checks (boot-hook-unit, boot-hook-vm, the OVMF matrix).
      rime.boot.enable = lib.mkForce false;

      # The next generation: only greetd's unit differs (as in a real update that touches it).
      specialisation.changed.configuration = {
        systemd.services.greetd.environment.RIME_TEST_GENERATION = "changed";
      };
      # The mutant: the same change, with the restart A1 must never see.
      specialisation.mutant.configuration = {
        systemd.services.greetd = {
          environment.RIME_TEST_GENERATION = "mutant";
          restartIfChanged = lib.mkForce true;
        };
      };
    };

  testScript = { nodes, ... }: ''

    fw = "${nodes.machine.rime.security.firewall.package}/libexec/rime-firewall"

    def state():
        pid = machine.succeed("systemctl show -p MainPID --value greetd.service").strip()
        sessions = machine.succeed(
            "for s in $(loginctl list-sessions --no-legend | awk '$3==\"rime\"{print $1}'); do "
            "echo $s $(loginctl show-session $s -p State --value) $(loginctl show-session $s -p Leader --value); done"
        ).strip()
        comp = machine.succeed("(pgrep -u rime Hyprland || true) | sort | tr '\\n' ' '").strip()
        print(f"greetd MainPID {pid}; rime's sessions [{sessions}]; Hyprland {comp}")
        return pid, sessions, comp

    def switch_to(base, name):
        rc, out = machine.execute(f"{base}/specialisation/{name}/bin/switch-to-configuration switch 2>&1")
        print(out)
        assert rc == 0, f"switch to {name} exited {rc}"
        env = machine.succeed("systemctl show -p Environment --value greetd.service")
        assert f"RIME_TEST_GENERATION={name}" in env, f"greetd's unit did not change: {env}"
        return out

    machine.wait_for_unit("multi-user.target")

    with subtest("a fresh machine: /etc/rime/firewall.d resolves before the first exception"):
        machine.wait_for_unit("rime-firewall.service")
        print(machine.succeed("ls -la /etc/rime/ /var/lib/rime/"))
        machine.succeed("test -d /etc/rime/firewall.d")
        assert machine.succeed("ls -A /etc/rime/firewall.d").strip() == ""
        print(machine.succeed(fw + " allow http"))
        machine.succeed("test -s /etc/rime/firewall.d/http.conf")
        print(machine.succeed(fw + " deny http"))

    with subtest("rime-hostlayer switch --now: the firewall goes down and up at once"):
        machine.succeed("nft list table inet rime")
        print(machine.succeed("rime-hostlayer switch off rime-firewall.service --now 2>&1"))
        machine.fail("systemctl is-active rime-firewall.service")
        machine.fail("nft list table inet rime")
        assert machine.succeed("rime-hostlayer switch get rime-firewall.service").strip() == "off"
        # Switched off, a plain start is refused by the unit's condition (no table appears).
        machine.succeed("systemctl start rime-firewall.service")
        assert machine.succeed("systemctl show -p ConditionResult --value rime-firewall.service").strip() == "no"
        machine.fail("nft list table inet rime")
        print(machine.succeed("rime-hostlayer switch on rime-firewall.service --now 2>&1"))
        machine.succeed("systemctl is-active rime-firewall.service")
        machine.succeed("nft list table inet rime")

    with subtest("the test user logs in through greetd (rime-greet) into Hyprland"):
        machine.wait_for_unit("greetd.service")
        machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=180)
        machine.sleep(8)
        machine.send_chars("rime\n")   # no last user yet: focus starts on the username field
        machine.sleep(2)
        machine.send_chars("rime\n")
        machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
        machine.wait_until_succeeds("pgrep -u rime Hyprland")
        machine.sleep(5)
        machine.screenshot("before-switch")
        print(machine.succeed("systemctl show -p KillMode -p RestartIfChanged greetd.service 2>&1 || true"))

    base = machine.succeed("readlink -f /run/current-system").strip()

    with subtest("A1: a switch to a generation with a changed greetd unit leaves greetd and the session alone"):
        before = state()
        assert before[1] and before[2], "nobody is logged in"
        out = switch_to(base, "changed")
        machine.sleep(5)
        after = state()
        assert after == before, f"A1 FAILED: greetd or the session changed across the switch: {before} -> {after}"
        machine.succeed("ls /run/user/1000/hypr/*/.socket.sock")
        machine.screenshot("after-switch")

    with subtest("A1 mutant: with restartIfChanged forced on, the same check sees the restart"):
        before = state()
        switch_to(base, "mutant")
        machine.sleep(5)
        machine.wait_for_unit("greetd.service")
        after = state()
        assert after[0] != before[0], "the mutant restarted nothing: the A1 check cannot see a restart"
        print("mutant: greetd MainPID", before[0], "->", after[0], "| session", before[1] or "-", "->", after[1] or "-")
  '';
}
