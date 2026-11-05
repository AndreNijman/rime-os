# avahi-resolvconf (nix/modules/avahi-resolvconf.nix): avahi survives a DNS change that lands while it starts, and
# a dead avahi does not block the next start.
#
#   race   ten restarts of avahi-daemon.service, each with a poller that does exactly what openresolv's
#          libc.d/avahi-daemon hook does (send SIGHUP to the PID in the file named by /etc/resolvconf.conf's
#          `avahi_daemon_pidfile`, default /var/run/avahi-daemon/pid) the moment that file is non-empty: the hook at
#          the worst instant, every time, instead of by chance. Every restart must end with avahi active, the PID it
#          was sent still running, and avahi logging "Got SIGHUP, reloading." (the DNS change still reaches it).
#          Then a real `resolvconf -u` reaches the running avahi the same way.
#   stale  avahi killed with SIGKILL leaves its PID file; a dead instance's PID file planted as the avahi user (the
#          state the boot race left behind, desktops-install-kde 11-04) must not stop the next start.
#   eval   the shipped system (rime-vm) carries the module.
#
# Mutants (nix build path:.#checks.x86_64-linux.avahi-resolvconf.mutants.<name>; each must FAIL):
#   no-ready-gate        the hook back on avahi's own PID file (the race kills avahi)
#   unprivileged-cleanup the PID file's removal without `+` (it runs in avahi's sandbox and cannot unlink, as avahi
#                        itself cannot)
# The nixpkgs kernel boots here (no Rime kernel): it runs in hosted CI.
{ pkgs, self }:

let
  lib = pkgs.lib;
  vm = self.nixosConfigurations.rime-vm.config;

  # Exactly the hook's test and kill, as fast as Python can poll; one SIGHUP per run, then it exits.
  poller = pkgs.writeText "avahi-hook-poller.py" ''
    import os, signal, sys, time
    path, deadline = sys.argv[1], time.monotonic() + 30
    open(sys.argv[2], "w").close()  # polling from here on
    while time.monotonic() < deadline:
        try:
            with open(path) as f:
                pid = f.read().strip()
        except OSError:
            continue
        if pid:
            os.kill(int(pid), signal.SIGHUP)
            print(pid, flush=True)
            sys.exit(0)
    sys.exit("the PID file never appeared: " + path)
  '';

  mkTest = name: extra: pkgs.testers.runNixOSTest {
    inherit name;
    nodes.machine = { lib, ... }: {
      imports = [ ../modules/avahi-resolvconf.nix extra ];
      services.avahi.enable = true;
      networking.resolvconf.enable = true;
      environment.systemPackages = [ pkgs.python3 ];
    };
    testScript = ''
      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("avahi-daemon.service")
      hookfile = machine.succeed(
          "sh -c '. /etc/resolvconf.conf; : ''${avahi_daemon_pidfile:=/var/run/avahi-daemon/pid}; "
          "echo $avahi_daemon_pidfile'").strip()
      print("the hook signals the PID in", hookfile)

      with subtest("race: the hook at the worst instant, ten times"):
          for i in range(10):
              machine.succeed("systemctl stop avahi-daemon.service avahi-daemon.socket")
              cursor = machine.succeed("journalctl -o cat -u avahi-daemon.service | wc -l").strip()
              machine.succeed("rm -f /tmp/hup.pid /tmp/hup.ready")
              machine.succeed(f"python3 ${poller} {hookfile} /tmp/hup.ready > /tmp/hup.pid 2>&1 &")
              machine.wait_until_succeeds("test -e /tmp/hup.ready", timeout=30)
              machine.succeed("systemctl start avahi-daemon.service || true")
              machine.wait_until_succeeds("test -s /tmp/hup.pid", timeout=30)
              hup = machine.succeed("cat /tmp/hup.pid").strip()
              machine.sleep(1)
              state = machine.execute("systemctl is-active avahi-daemon.service")[1].strip()
              jl = machine.succeed(f"journalctl -o cat -u avahi-daemon.service | tail -n +{int(cursor) + 1}")
              assert state == "active", f"restart {i}: avahi is {state} after the hook's SIGHUP to {hup}:\n{jl}"
              machine.succeed(f"kill -0 {hup}")
              assert "Got SIGHUP, reloading." in jl, f"restart {i}: avahi never handled the SIGHUP:\n{jl}"
          machine.succeed("systemctl start avahi-daemon.socket")

      with subtest("a real resolvconf -u reaches the running avahi"):
          n0 = machine.succeed("journalctl -o cat -u avahi-daemon.service | grep -c 'Got SIGHUP, reloading.'").strip()
          machine.succeed("resolvconf -u")
          machine.wait_until_succeeds(
              f"test $(journalctl -o cat -u avahi-daemon.service | grep -c 'Got SIGHUP, reloading.') -gt {n0}",
              timeout=10)
          machine.succeed("systemctl is-active avahi-daemon.service")

      with subtest("stale: a dead instance's PID file does not block the next start"):
          machine.succeed("systemctl kill -s KILL avahi-daemon.service")
          machine.wait_until_fails("systemctl is-active avahi-daemon.service", timeout=10)
          machine.succeed("systemctl start avahi-daemon.service")
          machine.succeed("systemctl is-active avahi-daemon.service")
          machine.succeed("systemctl stop avahi-daemon.service avahi-daemon.socket")
          # The boot race's leftover: avahi's own file (owner avahi), naming a process that is gone.
          machine.succeed("install -o avahi -g avahi -m 0644 /dev/null /run/avahi-daemon/pid && "
                          "echo 99999 > /run/avahi-daemon/pid && ! kill -0 99999")
          rc, out = machine.execute("systemctl start avahi-daemon.service")
          jl = machine.succeed("journalctl -o cat -u avahi-daemon.service | tail -n 15")
          assert rc == 0, f"avahi did not start over a dead instance's PID file:\n{jl}"
          machine.succeed("systemctl is-active avahi-daemon.service")
    '';
  };
in
assert lib.assertMsg (lib.hasInfix "avahi_daemon_pidfile=/run/avahi-daemon/ready" vm.networking.resolvconf.extraConfig)
  "rime-vm does not carry nix/modules/avahi-resolvconf.nix (its resolvconf.conf has no avahi_daemon_pidfile)";
assert lib.assertMsg (vm.services.avahi.enable && vm.networking.resolvconf.enable)
  "rime-vm has no avahi or no openresolv: this check tests nothing the product ships";
mkTest "rime-avahi-resolvconf" { } // {
  mutants = {
    no-ready-gate = mkTest "rime-avahi-resolvconf-mutant-no-ready-gate" ({ lib, ... }: {
      networking.resolvconf.extraConfig = lib.mkForce "";
      systemd.services.avahi-daemon.serviceConfig.ExecStartPost = lib.mkForce [ ];
    });
    unprivileged-cleanup = mkTest "rime-avahi-resolvconf-mutant-unprivileged-cleanup" ({ lib, ... }: {
      # The ready file is still cleaned with full privileges (the race subtest needs it); only the PID file's
      # removal runs in avahi's sandbox, and nothing removes it after a stop.
      systemd.services.avahi-daemon.serviceConfig = {
        ExecStartPre = lib.mkForce [
          "+${pkgs.coreutils}/bin/rm -f /run/avahi-daemon/ready /run/avahi-daemon/ready.tmp"
          "-${pkgs.coreutils}/bin/rm -f /run/avahi-daemon/pid"
        ];
        ExecStopPost = lib.mkForce [ "+${pkgs.coreutils}/bin/rm -f /run/avahi-daemon/ready /run/avahi-daemon/ready.tmp" ];
      };
    });
  };
}
