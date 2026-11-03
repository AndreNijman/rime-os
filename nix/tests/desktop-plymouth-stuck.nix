# checks.desktop-plymouth-stuck: a boot splash that stops answering does not hold the boot or the greeter.
#
# Hosted run 41450212626's desktop check never got a root shell: in its console log plymouth-read-write.service
# ("Tell Plymouth To Write Out Runtime Data", Before=sysinit.target) started after switch-root and never finished, so
# sysinit.target, and with it the test backdoor, never came. Freezing plymouthd (SIGSTOP) early in stage 2 gives the
# same console log here, every time (docs/nixos/ci.md, failures). desktop.nix (the module) now bounds the splash's client
# units and stops plymouthd when one of them does not finish.
#
# This check freezes plymouthd before plymouth-read-write runs, then asserts: sysinit.target is reached within its
# bound, the module stopped plymouthd and said so, and rime-greet comes up on VT 1 (plymouthd held the display).
# Mutants (passthru, must FAIL): "no-kill" (the units are bounded but plymouthd is not stopped) and "no-bound" (neither:
# the units as plymouth ships them, except plymouth-quit-wait's TimeoutSec=0 for all three).
{ pkgs, self }:

let
  inherit (pkgs) lib;
  mk = mutant: let
    # The mutants take the module's fix back out of the three units.
    undone = lib.mkIf (mutant != null) ({ ExecStopPost = lib.mkForce [ ]; }
      // lib.optionalAttrs (mutant == "no-bound") { TimeoutStartSec = lib.mkForce "infinity"; });
  in pkgs.testers.runNixOSTest {
    name = "rime-desktop-plymouth-stuck" + lib.optionalString (mutant != null) "-mutant-${mutant}";
    node.pkgsReadOnly = false;
    # The no-bound mutant never reaches sysinit.target, so no root shell ever connects: end it in 15 min, not 60.
    globalTimeout = 900;
    passthru.mutants = lib.optionalAttrs (mutant == null) (lib.genAttrs [ "no-kill" "no-bound" ] mk);

    nodes.machine = { lib, pkgs, ... }: {
      imports = [ self.nixosModules.default ];
      rime.desktop.enable = true;
      virtualisation = {
        memorySize = 4096;
        cores = 4;
        qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
      };
      # As in checks.desktop: the greeter host renders with pixman on QEMU's GPU.
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;

      # The fault: plymouthd (started in the initrd, still running) stops answering just before it is first asked.
      systemd.services.rime-test-freeze-plymouthd = {
        unitConfig.DefaultDependencies = false;
        wantedBy = [ "sysinit.target" ];
        before = [ "plymouth-read-write.service" ];
        after = [ "systemd-journald.service" ];
        serviceConfig.Type = "oneshot";
        script = ''
          pid=$(cat /run/plymouth/pid)
          kill -STOP "$pid"
          echo "rime-test: plymouthd (pid $pid) frozen" > /dev/kmsg
        '';
      };

      systemd.services.plymouth-read-write.serviceConfig = undone;
      systemd.services.plymouth-quit.serviceConfig = undone;
      systemd.services.plymouth-quit-wait.serviceConfig = undone;
    };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      with subtest("plymouthd was frozen before plymouth-read-write asked it anything"):
          print(machine.succeed("journalctl -k -b --no-pager | grep -E 'rime-test: plymouthd|rime: plymouthd'"))
          machine.succeed("journalctl -k -b --no-pager | grep -F 'rime-test: plymouthd (pid'")

      with subtest("sysinit.target within the bound, and the module stopped plymouthd"):
          t = int(machine.succeed("systemctl show -P ActiveEnterTimestampMonotonic sysinit.target").strip())
          print(f"sysinit.target reached {t / 1e6:.1f} s after boot")
          assert 0 < t < 60_000_000, f"sysinit.target at {t / 1e6:.1f} s: the splash held the boot"
          machine.succeed("journalctl -k -b --no-pager | grep -E 'rime: plymouthd \\(pid [0-9]+\\) did not answer plymouth-read-write'")
          machine.fail("pgrep -x plymouthd")

      with subtest("rime-greet comes up on VT 1"):
          machine.wait_for_unit("greetd.service", timeout=120)
          machine.wait_until_succeeds("pgrep -u greetd -x sway", timeout=120)
          machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=120)
          machine.succeed("test \"$(cat /sys/class/tty/tty0/active)\" = tty1")
    '';
  };
in
mk null
