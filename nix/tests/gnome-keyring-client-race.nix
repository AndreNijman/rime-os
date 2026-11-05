# gnome-keyring-client-race (nix/modules/gnome-keyring-client-race.nix): reading a property of the login keyring
# never aborts the daemon that the login password unlocked.
#
#   reads  a user logs in on the console (pam_gnome_keyring starts `gnome-keyring-daemon --login` with the password),
#          the session runs `gnome-keyring-daemon --start --components=secrets` as every Rime session does
#          (rime-secrets-start), then 6000 `busctl --user get-property … collection/login … Locked`, each from a new
#          connection, the read checks.desktops-install-kde makes once. Unpatched, the daemon dies at a random read:
#          10 of 10 daemons died within 318-4308 reads on a private bus (`artifacts/ppd-keyring/keyring/`), so 6000
#          reads find it nearly every run. The check fails on any failed read, a new or missing daemon, or the
#          "assertion 'client' failed" line in the journal.
#   eval   the shipped system (rime-vm) carries the patched gnome-keyring.
#
# Mutant (nix build path:.#checks.x86_64-linux.gnome-keyring-client-race.mutants.unpatched; must FAIL):
#   unpatched   nixpkgs' gnome-keyring 50.0 as it is (the module not imported)
# Probabilistic in one direction only: the patched daemon never dies here (0 of 50,000 reads on a private bus), the
# unpatched one survives 6000 reads in about 1 run of 20. The nixpkgs kernel boots here: it runs in hosted CI.
{ pkgs, self }:

let
  lib = pkgs.lib;
  vmPkgs = self.nixosConfigurations.rime-vm.pkgs;
  reads = 6000;

  mkTest = name: patched: pkgs.testers.runNixOSTest {
    inherit name;
    node.pkgsReadOnly = false;
    nodes.machine = { ... }: {
      imports = lib.optional patched ../modules/gnome-keyring-client-race.nix;
      services.gnome.gnome-keyring.enable = true;
      users.users.alice = { isNormalUser = true; password = "keyring-test"; };
      virtualisation.memorySize = 1024;
    };
    testScript = ''
      def as_alice(cmd):
          return ("su - alice -c " + repr("export XDG_RUNTIME_DIR=/run/user/1000 "
                  "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; " + cmd))

      machine.wait_for_unit("multi-user.target")
      machine.wait_until_tty_matches("1", "login: ")
      machine.send_chars("alice\n")
      machine.wait_until_tty_matches("1", "Password: ")
      machine.send_chars("keyring-test\n")
      machine.wait_until_succeeds("pgrep -u alice -f -- 'gnome-keyring-daemon --daemonize --login'", timeout=60)
      machine.wait_for_unit("dbus.socket", "alice")
      # rime-secrets-start: the session's `--start` keeps the PAM daemon and puts it on the bus.
      machine.succeed(as_alice("gnome-keyring-daemon --start --components=secrets"))
      machine.wait_until_succeeds(as_alice("busctl --user status org.freedesktop.secrets"), timeout=30)
      owner = machine.succeed(as_alice("busctl --user status org.freedesktop.secrets | grep -E '^(PID|CommandLine)='"))
      print(owner)
      assert "--login" in owner, "the secrets service is not the daemon the login password unlocked: " + owner
      pid = owner.split("PID=")[1].split()[0]

      with subtest("${toString reads} reads of the login keyring's Locked, each from a new connection"):
          rc, out = machine.execute(as_alice(
              "for i in $(seq 1 ${toString reads}); do "
              "busctl --user get-property org.freedesktop.secrets /org/freedesktop/secrets/collection/login "
              "org.freedesktop.Secret.Collection Locked > /tmp/locked || { echo \"read $i failed\"; exit 1; }; "
              "done; cat /tmp/locked"), timeout=900)
          print(out)
          jl = machine.succeed("journalctl -o cat --no-pager | grep -E \"gnome-keyring|assertion 'client'|invoke_get_property\" | tail -n 20 || true")
          print(jl)
          assert rc == 0, f"a read failed: {out.strip()}\n{jl}"
          assert out.strip().endswith("b false"), f"the login keyring is locked: {out}"
          assert "assertion 'client' failed" not in jl, jl
          machine.succeed(f"kill -0 {pid}")
          now = machine.succeed(as_alice("busctl --user status org.freedesktop.secrets | grep -E '^PID='")).strip()
          assert now == f"PID={pid}", f"the secrets service changed hands ({now}, was {pid}): the daemon died"
    '';
  };
in
assert lib.assertMsg (vmPkgs.gnome-keyring.passthru.rimeClientRacePatch or false)
  "rime-vm's gnome-keyring does not carry nix/modules/gnome-keyring-client-race.patch";
mkTest "rime-gnome-keyring-client-race" true // {
  mutants.unpatched = mkTest "rime-gnome-keyring-client-race-mutant-unpatched" false;
}
