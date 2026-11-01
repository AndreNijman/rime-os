# D6 end to end (docs/nixos/updates-and-release.md §6-§11, §18): the REAL release pipeline publishes, the REAL client
# installs. update-client.nix proves the client against a fixture publisher; release-github.nix proves the pipeline
# against its reference consumer. This test joins them: nothing between the two is a fixture.
#
# server  the production publish shape with THROWAWAY keys made at runtime (release-github.nix's stand-in):
#           server:5000       the GHCR stand-in (distribution); manifests and channel documents signed by cosign 2.5.2
#                             with a TEST Fulcio-shaped chain (mint-test-signing)
#           pages.test:8090   GitHub Pages of rime-cache: the narinfos, deployed by a post-receive hook of a bare repo
#           api.test:8081     fake-github (release API), github.test:8082 (downloads, 302) -> objects.test:8083 (NARs)
#           server:8080       a fake upstream cache (cache.nixos.org), signed with its own TEST key, holding nixpkgs'
#                             closures of the kernel 6.12, systemd, nix, skopeo, glibc, coreutils and bash
#         and runs `rime-nix-release closure | split | stage | publish --backend github` per release, from the
#         toplevels below (extraDependencies: the build job's output, here the server's store).
# client  boots release A from its own disk (systemd-boot, so reboots are real) and runs `rime update` against it.
#
# Releases (the client's own configuration plus one change each, evaluated with extendModules):
#   2026.11.01 A  what the client boots first                      -> up to date
#   2026.11.02 B  the nixpkgs 6.12 kernel, an UPSTREAM-ONLY path   -> installs for the next boot; reboot into it
#   rollback      back to A, reboot
#   2026.11.03 C  A + a new Shell build + a greetd unit change      -> applies live: no reboot, greetd untouched
#   then refusals: a manifest changed after signing (its channel document validly signed), and a channel document
#   changed after signing. Neither moves the profile.
{ pkgs, self }:

let
  lib = pkgs.lib;
  release = import ../release { inherit pkgs self; };
  tool = release.rime-nix-release;
  REPO = "server:5000/andrenijman/rime-os";

  tamper = pkgs.writeShellScriptBin "update-e2e-tamper" ''
    export PATH=${lib.makeBinPath [ pkgs.skopeo release.cosign pkgs.openssl pkgs.coreutils ]}:$PATH
    export RIME_COSIGN=${release.cosign}/bin/cosign PYTHONPATH=${tool}/lib/rime-nix-release
    exec ${pkgs.python3}/bin/python3 ${./update-e2e-tamper.py} "$@"
  '';

  # The one module every release differs by (as update-client.nix). Defaults = release A.
  delta =
    { config, lib, pkgs, ... }:
    {
      options.rimeTest = {
        shell = lib.mkOption { type = lib.types.str; default = "A"; };
        kernel = lib.mkOption { type = lib.types.bool; default = false; };
      };
      config = {
        # A Shell build of its own per release: the client classifies by store name (rime-shell-*).
        environment.systemPackages = [
          (pkgs.symlinkJoin {
            name = "rime-shell-test-${config.rimeTest.shell}";
            paths = [ pkgs.rime.rime-shell ];
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
      };
    };

  hosts = nodes: "${nodes.server.networking.primaryIPAddress} pages.test api.test github.test objects.test";
in
pkgs.testers.runNixOSTest {
  name = "rime-update-e2e";
  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;
  # Three publishes of a real NixOS closure, three downloads, three reboots.
  globalTimeout = 2 * 3600;

  nodes.server =
    { nodes, ... }:
    {
      nix.package = pkgs.nixVersions.latest;
      nix.settings = {
        experimental-features = [ "nix-command" ];
        substituters = lib.mkForce [ ];
      };
      networking.extraHosts = hosts nodes;
      virtualisation.memorySize = 4096;
      virtualisation.diskSize = 20480;
      virtualisation.cores = 4;
      networking.firewall.allowedTCPPorts = [ 5000 8080 8081 8082 8083 8090 ];
      services.dockerRegistry = {
        enable = true;
        listenAddress = "0.0.0.0";
        port = 5000;
      };
      systemd.services.upstream-cache = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "upstream";
          ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080 --directory /var/lib/upstream";
        };
      };
      systemd.services.pages = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "pages";
          ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8090 --directory /var/lib/pages";
        };
      };
      systemd.services.fake-github = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "fake-github";
          ExecStart = "${tool}/bin/fake-github --dir /var/lib/fake-github --repo AndreNijman/rime-cache"
            + " --token rime-test-cache-token --api 0.0.0.0:8081 --download 0.0.0.0:8082 --objects 0.0.0.0:8083"
            + " --objects-url http://objects.test:8083";
        };
      };
      environment.systemPackages = [ tool release.cosign tamper pkgs.curl pkgs.skopeo pkgs.openssl pkgs.git ];
      environment.etc = lib.mapAttrs' (n: p: lib.nameValuePair "rime-e2e/plan-${n}.json" { source = p; })
        nodes.client.system.build.rimePlans;
      # The build job's output: every release's closure is valid in the server's store, none is on the client's disk.
      system.extraDependencies = builtins.attrValues nodes.client.system.build.rimeReleases;
    };

  nodes.client =
    { config, lib, pkgs, nodes, extendModules, ... }:
    let
      ext = m: (extendModules { modules = [ m ]; }).config;
      cfgs = {
        A = config;
        B = ext { rimeTest.kernel = true; };
        C = ext { rimeTest.shell = "C"; };
      };
      str = p: builtins.unsafeDiscardStringContext (toString p);
      components = import ../release/components.nix { inherit lib; };
      # What nix/release/default.nix's rime-release-plan holds, for one release of this machine.
      plan = name: c: pkgs.writeText "rime-e2e-plan-${name}.json" (builtins.toJSON {
        variants.plain = {
          toplevel = str c.system.build.toplevel;
          publishable = false;
          note = "TEST: update-e2e";
          components = lib.mapAttrs (_: map str) (components c pkgs);
        };
        catalogue = [ ];
      });
    in
    {
      imports = [ self.nixosModules.default delta ];

      system.build.rimeReleases = lib.mapAttrs (_: c: c.system.build.toplevel) cfgs;
      system.build.rimePlans = lib.mapAttrs plan cfgs;
      system.build.rimeKernelB = cfgs.B.boot.kernelPackages.kernel;
      # What the fake upstream serves: closures of plain nixpkgs packages, as cache.nixos.org would. The 6.12 kernel
      # is only there, so release B cannot install from Rime's cache alone.
      system.build.rimeUpstreamRoots = [
        cfgs.B.boot.kernelPackages.kernel
        config.systemd.package
        config.nix.package
        pkgs.skopeo
        pkgs.stdenv.cc.libc
        pkgs.coreutils
        pkgs.bashInteractive
      ];

      networking.extraHosts = hosts nodes;
      virtualisation.useBootLoader = true;
      boot.growPartition = true;
      virtualisation.useEFIBoot = true;
      boot.loader.systemd-boot.enable = true;
      boot.loader.efi.canTouchEfiVariables = true;
      system.switch.enable = true;
      nix.enable = true;
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
        repository = REPO;
        cacheUrl = "http://pages.test:8090";
        upstreamCacheUrl = "http://server:8080";
        # The TEST Fulcio root is minted when the test starts and copied here before the first update.
        fulcioRoot = "/var/lib/rime-test/fulcio-root.pem";
        keepGenerations = 2;
      };
    };

  testScript =
    { nodes, ... }:
    let
      b = nodes.client.system.build;
    in
    ''
      import json, os

      SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main"
      ISSUER = "https://token.actions.githubusercontent.com"
      REPO = "${REPO}"
      PAGES = "http://pages.test:8090"
      API = "http://api.test:8081"
      DOWNLOAD = "http://github.test:8082"
      UPSTREAM = "http://server:8080"
      GIT = "file:///var/lib/rime-cache.git"
      TOKEN = "rime-test-cache-token"
      NIXPKGS = "1c89083e451e891152365b799595cf7790a43325"
      SHELL_SHA = "5" * 40
      A, B, C = "${b.rimeReleases.A}", "${b.rimeReleases.B}", "${b.rimeReleases.C}"
      KERNEL_B = "${b.rimeKernelB}"
      UPSTREAM_ROOTS = "${lib.concatStringsSep " " (map toString b.rimeUpstreamRoots)}"
      CLIENT_IP = "${nodes.client.networking.primaryIPAddress}"

      def hash_of(p):
          return os.path.basename(p)[:32]

      def publish(name, top, os_sha, rid):
          d = f"/root/{name}"
          server.succeed(
              f"mkdir -p {d}",
              f"rime-nix-release closure --variant plain={top} --out {d}/closure.json",
              f"rime-nix-release split --closure {d}/closure.json --upstream {UPSTREAM} --out {d}/split.json",
              f"rime-nix-release stage --closure {d}/closure.json --split {d}/split.json --out {d}/artifact",
          )
          rc, log = server.execute(
              f"RIME_CACHE_TOKEN={TOKEN} rime-nix-release publish --backend github --artifact {d}/artifact"
              f" --plan /etc/rime-e2e/plan-{name}.json --secret-key-file /root/cache.sec"
              f" --upstream {UPSTREAM} --upstream-key \"$(cat /root/up.pub)\""
              f" --gh-repo AndreNijman/rime-cache --gh-remote {GIT} --gh-api {API} --gh-uploads {API} --gh-download {DOWNLOAD}"
              f" --public {PAGES} --cache-pubkey \"$(cat /root/cache.pub)\" --cache-url-hint {PAGES}"
              f" --repo {REPO} --plain-http --fulcio-root /root/sign/fulcio-root.pem"
              f" --rime-os {os_sha} --rime-shell {SHELL_SHA} --nixpkgs {NIXPKGS} --release {rid}"
              f" --allow-test-variants --test-signing /root/sign --out {d}/out 2>&1")
          print(log)
          assert rc == 0, f"publish {name} failed"
          st = json.loads(server.succeed(f"cat {d}/out/published.json"))
          assert st["state"] == "published" and st["release"] == rid, st
          split = json.loads(server.succeed(f"cat {d}/split.json"))
          man = json.loads(server.succeed(f"cat {d}/out/manifest.json"))
          server.succeed(f"rm -rf {d}/artifact")  # two full copies of the Rime set do not fit twice over
          return st["manifest_digest"], split, man

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

      def boot_id():
          return client.succeed("cat /proc/sys/kernel/random/boot_id").strip()

      def greetd_pid():
          return client.succeed("systemctl show -P MainPID greetd.service").strip()

      def served_to_client(unit):
          """Paths the client requested from a cache, from python http.server's access log."""
          return server.succeed(f"journalctl -u {unit} --no-pager -o cat | grep '^{CLIENT_IP} ' || true")

      def reboot():
          client.shutdown()
          client.start()
          client.wait_for_unit("multi-user.target")

      start_all()
      for u in ("docker-registry.service", "upstream-cache.service", "pages.service", "fake-github.service"):
          server.wait_for_unit(u)
      for p in (5000, 8080, 8081, 8082, 8083, 8090):
          server.wait_for_open_port(p)
      client.wait_for_unit("multi-user.target")
      client.wait_for_unit("greetd.service")
      assert link("/run/current-system") == A
      client.wait_for_unit("growpart.service")
      client.succeed("resize2fs /dev/disk/by-label/nixos")

      with subtest("throwaway keys: upstream-test-1, rime-test-cache-1, a TEST Fulcio root; the client trusts the root"):
          server.succeed(
              "nix key generate-secret --key-name upstream-test-1 > /root/up.sec",
              "nix key convert-secret-to-public < /root/up.sec > /root/up.pub",
              "nix key generate-secret --key-name rime-test-cache-1 > /root/cache.sec",
              "nix key convert-secret-to-public < /root/cache.sec > /root/cache.pub",
              f"mint-test-signing /root/ca /root/sign {SIGNER} {ISSUER}",
          )
          server.copy_from_machine("/root/sign/fulcio-root.pem", "")
          client.copy_from_host(str(server.out_dir / "fulcio-root.pem"), "/var/lib/rime-test/fulcio-root.pem")
          os.remove(server.out_dir / "fulcio-root.pem")

      with subtest("the fake upstream cache, and rime-cache as bootstrapped (main: nix-cache-info, CNAME, .nojekyll)"):
          server.succeed(f"nix copy --to 'file:///var/lib/upstream?compression=zstd' {UPSTREAM_ROOTS}")
          server.succeed(f"nix store sign --store file:///var/lib/upstream --key-file /root/up.sec -r {UPSTREAM_ROOTS}")
          server.succeed(f"test -e /var/lib/upstream/{hash_of(KERNEL_B)}.narinfo")
          server.succeed(
              "git init --quiet --bare -b main /var/lib/rime-cache.git",
              "printf '#!/bin/sh\\nexec git --work-tree=/var/lib/pages checkout -f main\\n' > /var/lib/rime-cache.git/hooks/post-receive",
              "chmod +x /var/lib/rime-cache.git/hooks/post-receive",
              "git init --quiet -b main /root/seed",
              "printf 'StoreDir: /nix/store\\nWantMassQuery: 1\\nPriority: 50\\n' > /root/seed/nix-cache-info",
              "echo cache.rimeos.com > /root/seed/CNAME && touch /root/seed/.nojekyll && echo TEST > /root/seed/README.md",
              "git -C /root/seed add -A && git -C /root/seed -c user.name=T -c user.email=t@invalid commit --quiet -m seed",
              "git -C /root/seed push --quiet /var/lib/rime-cache.git main",
          )
          client.succeed(f"curl -fsS {PAGES}/nix-cache-info | grep 'Priority: 50'")

      with subtest("release A, published by the pipeline: the machine is up to date; trust --gate verifies it"):
          _, split_a, man_a = publish("A", A, "a" * 40, "2026.11.01")
          assert A in split_a["rime"] and split_a["upstream"], split_a
          assert man_a["cache"]["upstream"]["url_hint"] == UPSTREAM, man_a["cache"]
          client.fail("rime trust --gate")  # nothing recorded for the running system yet
          code, out = update()
          assert code == 0 and "up to date" in out, out
          client.succeed("rime trust --gate")
          assert "state=none" in checker()

      before = profile()

      with subtest("release B (kernel, upstream-only): installs for the next boot from BOTH caches; reboot into it"):
          digest_b, split_b, man_b = publish("B", B, "b" * 40, "2026.11.02")
          assert KERNEL_B in split_b["upstream"] and KERNEL_B not in split_b["rime"], "the 6.12 kernel is not upstream-only"
          origins = {e["path"]: e.get("origin") for e in man_b["paths"]}
          assert origins[KERNEL_B] == "upstream" and origins[B] == "rime", (origins[KERNEL_B], origins[B])
          server.fail(f"git -C /var/lib/rime-cache.git cat-file -e main:{hash_of(KERNEL_B)}.narinfo")
          st = checker()
          assert "state=available" in st and f"digest={digest_b}" in st, st
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
          assert st["state"] == "active" and st["remaining"] == "reboot", st
          up_log, pages_log = served_to_client("upstream-cache"), served_to_client("pages")
          assert f"GET /{hash_of(KERNEL_B)}.narinfo" in up_log, up_log
          assert f"GET /{hash_of(B)}.narinfo" in pages_log, pages_log
          assert f"/{hash_of(KERNEL_B)}.narinfo" not in pages_log, "the upstream-only kernel was asked of Rime's cache"
          reboot()
          assert link("/run/booted-system") == B
          assert "6.12" in client.succeed("uname -r")
          client.succeed("rime trust --gate")

      with subtest("rollback: the generation before the booted one boots next; reboot returns to A"):
          print(client.succeed("rime rollback"))
          assert link("/nix/var/nix/profiles/system") == A
          reboot()
          assert link("/run/booted-system") == A
          assert "6.12" not in client.succeed("uname -r")

      with subtest("release C (Shell only): applies live with no reboot, greetd untouched"):
          publish("C", C, "c" * 40, "2026.11.03")
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
          client.succeed("rime trust --gate")

      before = profile()

      with subtest("refused: a manifest changed after signing, though the channel document naming it is validly signed"):
          print(server.succeed(f"update-e2e-tamper manifest --repo {REPO} --fulcio-root /root/sign/fulcio-root.pem"
                               " --test-signing /root/sign --release 2026.11.04"))
          refused("tampered manifest", "does not verify")
          assert link("/run/current-system") == C

      with subtest("refused: a channel document changed after signing"):
          print(server.succeed(f"update-e2e-tamper channels --repo {REPO}"))
          refused("tampered channel document", "does not verify")
          assert link("/run/current-system") == C
    '';
}
