# D6, the client (docs/nixos/updates-and-release.md §8-§11, docs/nixos/update-client.md): `rime update` on a
# NixOS machine against releases published by a server, end to end, in VMs.
#
# server: an OCI registry (distribution) for the signed channel document and release manifests, and RustFS (an S3
#         store, standing in for R2: nixpkgs removed MinIO in 2026-10) serving the binary cache over plain HTTP.
#         nix/release/fixture-publish.py publishes with THROWAWAY keys (nix/release/fixture-keys.nix).
# client: boots generation A from its own disk (systemd-boot, so the boot menu and a reboot are real) and trusts the
#         test CA, identity and registry through rime.update, the same options a real system sets.
#
# Releases, all evaluated from the client's own configuration (extendModules) plus one change each:
#   2026.11.01 A  what the client boots first
#   2026.11.02 B  another kernel (nixpkgs 6.12)                      -> installs for the next boot, reboot into it
#   2026.11.03 C  A + a new Shell build + a greetd unit change          -> applies live: no reboot, greetd untouched
#   2026.11.04-6  D  C + an /etc file; used for every refusal, then installed live
#   2026.11.07 E  C + another /etc file                              -> one more generation, so gc has work to do
{ pkgs, self }:

let
  keys = pkgs.callPackage ../release/fixture-keys.nix { };
  repo = "server:5000/rime/os";
  publisher = pkgs.callPackage ../release/fixture-publish.nix { inherit keys repo; };
  s3key = "rimetestkey";
  s3secret = "rimetestsecret-TEST-only";

  # The one module every release differs by. Defaults = release A.
  delta =
    { config, lib, pkgs, ... }:
    {
      options.rimeTest = {
        shell = lib.mkOption { type = lib.types.str; default = "A"; };
        kernel = lib.mkOption { type = lib.types.bool; default = false; };
        etc = lib.mkOption { type = lib.types.str; default = "A"; };
      };
      config = {
        # A Shell build of its own per release: the client classifies by store name (rime-shell-*), so this is a
        # Shell change and nothing else.
        environment.systemPackages = [
          (pkgs.symlinkJoin {
            name = "rime-shell-test-${config.rimeTest.shell}";
            paths = [ pkgs.rime.rime-shell ];
            # A marker on PATH (system-path links bin/, not all of share/).
            postBuild = ''
              mkdir -p $out/bin
              printf '#!/bin/sh\necho ${config.rimeTest.shell}\n' > $out/bin/rime-test-shell
              chmod +x $out/bin/rime-test-shell
            '';
          })
        ];
        # A change to greetd's own unit, which a live switch must NOT act on (restartIfChanged = false).
        systemd.services.greetd.environment.RIME_TEST_SHELL = config.rimeTest.shell;
        boot.kernelPackages = lib.mkIf config.rimeTest.kernel pkgs.linuxPackages_6_12;
        environment.etc."rime-test-release".text = config.rimeTest.etc;
      };
    };
in
pkgs.testers.runNixOSTest {
  name = "rime-update-client";
  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.server =
    { nodes, pkgs, ... }:
    {
      nix.enable = true;
      nix.settings.experimental-features = [ "nix-command" ];
      services.dockerRegistry = {
        enable = true;
        listenAddress = "0.0.0.0";
        openFirewall = true;
      };
      services.rustfs = {
        enable = true;
        environmentFile = toString (
          pkgs.writeText "rustfs-TEST.env" ''
            RUSTFS_ACCESS_KEY=${s3key}
            RUSTFS_SECRET_KEY=${s3secret}
          ''
        );
      };
      networking.firewall.allowedTCPPorts = [ 9000 ];
      virtualisation.containers.enable = true; # /etc/containers/policy.json for skopeo
      environment.systemPackages = [ publisher pkgs.rustfs-cli ];
      # Every release's closure is valid in the server's store; none of them is on the client's disk.
      system.extraDependencies = [ nodes.client.system.build.toplevel ] ++ builtins.attrValues nodes.client.system.build.rimeReleases;
      virtualisation.memorySize = 2048;
      virtualisation.diskSize = 12288;
    };

  nodes.client =
    { config, lib, extendModules, ... }:
    let
      release = m: (extendModules { modules = [ m ]; }).config.system.build.toplevel;
    in
    {
      imports = [ self.nixosModules.default delta ];

      system.build.rimeReleases = {
        B = release { rimeTest.kernel = true; };
        C = release { rimeTest.shell = "C"; };
        D = release { rimeTest = { shell = "C"; etc = "D"; }; };
        E = release { rimeTest = { shell = "C"; etc = "E"; }; };
      };

      # A real boot menu on a real disk: the store is the disk's, not the host's (useBootLoader).
      virtualisation.useBootLoader = true;
      virtualisation.useEFIBoot = true;
      boot.loader.systemd-boot.enable = true;
      boot.loader.efi.canTouchEfiVariables = true;
      system.switch.enable = true;
      nix.enable = true;
      # Kept across reboots (dbus-broker needs a valid one; the rollout slot is derived from it).
      environment.etc."machine-id".text = "a7c1f00dba5e4f6a9d3c2b1e0f9a8b7c\n";
      virtualisation.memorySize = 2048;
      virtualisation.diskSize = 12288;
      virtualisation.containers.registries.settings.registry = [ { location = "server:5000"; insecure = true; } ];

      rime.services.enable = true;
      services.greetd = {
        enable = true;
        settings.default_session.command = "${config.services.greetd.package}/bin/agreety --cmd /bin/sh";
      };

      rime.update = {
        repository = repo;
        cacheUrl = "http://server:9000/rime-cache";
        fulcioRoot = "${keys}/ca.pem";
        keepGenerations = 2;
      };
    };

  testScript =
    { nodes, ... }:
    let
      r = nodes.client.system.build.rimeReleases;
    in
    ''
      import json

      A = "${nodes.client.system.build.toplevel}"
      B, C, D, E = "${r.B}", "${r.C}", "${r.D}", "${r.E}"
      serial = 0

      def publish(cmd):
          return server.succeed(f"rime-fixture-publish {cmd}").strip()

      def release(rid, top, *extra):
          return publish(f"release --release {rid} --toplevel {top} " + " ".join(extra))

      def channel(digest, rid, *extra, at=None):
          global serial
          serial += 1
          return publish(f"channel --serial {serial if at is None else at} --entry edge={digest}={rid} " + " ".join(extra))

      def checker():
          client.succeed("systemctl start rime-update-check.service")
          return client.succeed("cat /run/rime-update/state")

      def update(*args):
          code, out = client.execute("rime update --skip-firmware " + " ".join(args) + " 2>&1")
          print(out)
          return code, out

      def refused(what, needle):
          code, out = update()
          assert code != 0, f"{what}: rime update exited 0"
          assert needle in out, f"{what}: expected {needle!r}"
          assert profile() == before, f"{what}: the profile moved"

      def link(p):
          return client.succeed(f"readlink -f {p}").strip()

      def profile():
          return client.succeed("readlink /nix/var/nix/profiles/system").strip()

      def gens():
          out = client.succeed("ls /nix/var/nix/profiles | sed -n 's/^system-\\([0-9]*\\)-link$/\\1/p' | sort -n")
          return [int(x) for x in out.split()]

      def gen_of(n):
          return link(f"/nix/var/nix/profiles/system-{n}-link")

      def boot_id():
          return client.succeed("cat /proc/sys/kernel/random/boot_id").strip()

      def greetd_pid():
          return client.succeed("systemctl show -P MainPID greetd.service").strip()

      def reboot():
          client.shutdown()
          client.start()
          client.wait_for_unit("multi-user.target")

      start_all()
      server.wait_for_unit("docker-registry.service")
      server.wait_for_unit("rustfs.service")
      server.wait_for_open_port(9000)
      server.succeed("rc alias set local http://127.0.0.1:9000 ${s3key} ${s3secret}")
      server.succeed("rc bucket create local/rime-cache")
      server.succeed("rc anonymous set download local/rime-cache")
      client.wait_for_unit("multi-user.target")
      client.wait_for_unit("greetd.service")
      assert link("/run/current-system") == A

      with subtest("greetd is not restarted by a switch (restartIfChanged = false)"):
          client.succeed("grep -x 'X-RestartIfChanged=false' /etc/systemd/system/greetd.service")

      with subtest("the publisher: every closure into the cache, signed with the test cache key"):
          publish(f"cache {A} {B} {C} {D} {E}")
          client.succeed("curl -sf http://server:9000/rime-cache/nix-cache-info")

      a = release("2026.11.01", A)

      with subtest("an unsigned channel document is refused by name"):
          channel(a, "2026.11.01", "--unsigned")
          before = profile()
          refused("unsigned", "is not signed")

      with subtest("release A: the machine is up to date and records it; trust --gate verifies it"):
          client.fail("rime trust --gate")  # nothing recorded for the running system yet
          channel(a, "2026.11.01")
          code, out = update()
          assert code == 0 and "up to date" in out, out
          client.succeed("rime trust --gate")
          # The update notice the Shell reads: nothing newer.
          assert "state=none" in checker()

      with subtest("release B (kernel): installs for the next boot, nothing live; reboot into it"):
          b = release("2026.11.02", B)
          channel(b, "2026.11.02")
          st = checker()
          assert "state=available" in st and f"digest={b}" in st, st
          code, out = update("--plan")
          assert code == 0 and "applies at the next restart" in out, out
          assert profile() == before
          bid = boot_id()
          code, out = update()
          assert code == 0 and "installs for the next restart" in out, out
          assert link("/run/current-system") == A, "a kernel change was activated live"
          assert boot_id() == bid
          assert link("/nix/var/nix/profiles/system") == B
          st = json.loads(client.succeed("cat /run/rime-live/status.json"))
          print(st)
          assert st["product"] == "nixos" and st["target"]["toplevel"] == B and st["staged_for_boot"], st
          reboot()
          assert link("/run/booted-system") == B
          assert "6.12" in client.succeed("uname -r")
          client.succeed("rime trust --gate")
          print(client.succeed("rime pin"))
          gen_b = gens()[-1]
          assert gen_of(gen_b) == B

      with subtest("rollback: the generation before the booted one boots next; reboot returns to A"):
          print(client.succeed("rime rollback"))
          assert link("/nix/var/nix/profiles/system") == A
          reboot()
          assert link("/run/booted-system") == A
          assert "6.12" not in client.succeed("uname -r")

      with subtest("release C (Shell only): busy defers it, then it applies live with no reboot"):
          c = release("2026.11.03", C)
          channel(c, "2026.11.03")
          print(client.succeed(f"{C}/bin/switch-to-configuration dry-activate 2>&1"))
          # Gaming Mode: a process in the game cgroup holds the live part back.
          client.succeed("mkdir -p /sys/fs/cgroup/rime-game && (sleep 600 >/dev/null 2>&1 & echo $! > /sys/fs/cgroup/rime-game/cgroup.procs)")
          code, out = update("--plan")
          assert code == 0 and "a game is running" in out, out
          client.succeed("kill $(cat /sys/fs/cgroup/rime-game/cgroup.procs) && sleep 1 && rmdir /sys/fs/cgroup/rime-game")
          bid, pid = boot_id(), greetd_pid()
          code, out = update()
          assert code == 0 and "is active now" in out, out
          assert link("/run/current-system") == C
          assert link("/run/booted-system") == A
          assert boot_id() == bid, "the machine rebooted"
          assert greetd_pid() == pid, "greetd was restarted by the switch"
          assert client.succeed("rime-test-shell").strip() == "C"
          st = json.loads(client.succeed("cat /run/rime-live/status.json"))
          print(st)
          assert st["state"] == "active" and st["booted"]["toplevel"] == A and st["target"]["toplevel"] == C, st
          assert st["generations"]["booted"] == 1, st
          client.succeed("rime trust --gate")

      before = profile()

      with subtest("a tampered manifest (changed after signing) is refused"):
          channel(release("2026.11.04", D, "--tamper"), "2026.11.04")
          refused("tampered", "does not verify")

      with subtest("a manifest signed by another identity is refused"):
          channel(release("2026.11.04", D, "--identity", "wrong"), "2026.11.04")
          refused("wrong identity", "build-image.yml")

      with subtest("an older channel document served again is a replay"):
          channel(a, "2026.11.01", at=1)
          refused("replay", "replay")

      with subtest("a channel pointing back at an older release installs nothing (no downgrade)"):
          channel(a, "2026.11.01")
          code, out = update()
          assert code == 0 and "older than the installed" in out, out
          assert profile() == before

      with subtest("min_from: a release that needs a newer starting point is refused"):
          channel(release("2026.11.05", D, "--min-from", "2026.11.04"), "2026.11.05")
          refused("min_from", "can only be installed over 2026.11.04")

      with subtest("a manifest announcing a cache key the cache did not sign with: nothing is installed"):
          channel(release("2026.11.05", D, "--cache-key", "wrong-cache"), "2026.11.05")
          refused("wrong cache key", "did not match its signature")
          client.fail(f"nix-store --check-validity {D}")

      with subtest("release D and E install live (two more generations)"):
          channel(release("2026.11.06", D), "2026.11.06")
          code, out = update()
          assert code == 0 and "is active now" in out, out
          channel(release("2026.11.07", E), "2026.11.07")
          code, out = update()
          assert code == 0 and "is active now" in out, out
          assert link("/run/current-system") == E

      with subtest("gc keeps the booted, pinned, current and previous generations and removes the rest"):
          g = gens()
          print(g)
          assert len(g) == 5, g  # A, B, C, D, E (the rollback moved the profile back, it made no generation)
          booted = 1
          assert gen_of(booted) == A
          print(client.succeed("rime gc"))
          left = gens()
          print(left)
          assert left == [booted, gen_b, g[-2], g[-1]], left
          client.succeed(f"nix-store --check-validity {A} {B} {D} {E}")
          assert link("/run/booted-system") == A

      with subtest("update --pin holds the machine; --unpin releases it"):
          print(client.succeed("rime update --pin 2026.11.07"))
          code, out = update()
          assert code == 0 and "held at 2026.11.07" in out, out
          print(client.succeed("rime update --unpin"))
          client.fail("rime update --pin 2026.10.01")

      with subtest("the new generation boots"):
          reboot()
          assert link("/run/booted-system") == E
          client.succeed("rime trust --gate")
    '';
}
