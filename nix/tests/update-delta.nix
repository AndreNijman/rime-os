# G3 (docs/nixos/delta.md §7-§8): `rime update` rebuilds changed paths from zstd deltas against the previous release,
# and falls back to the whole NAR on ANY refusal. update-e2e.nix's server (the real pipeline, throwaway keys), plus
# `rime-nix-release deltas` between stage and publish, as the build job runs it.
#
# Releases (the client's configuration plus one versioned payload, as a package update looks):
#   2026.11.01 A  rime-delta-payload-1 (8 MiB of incompressible bytes)       the client boots this
#   2026.11.02 B  rime-delta-payload-2 (the same bytes, 3 small edits)       published WITH deltas against A
#
# Every subtest runs `rime update --download-only` (the client keeps booting A), checks what arrived and what the
# client fetched (fake-github's download log), then deletes B's new paths so the next subtest downloads them again:
#   deltas used          the payload is rebuilt from its .delta; its whole NAR is never fetched
#   tampered delta       the .delta asset changed on the server  -> refused, the payload downloaded whole
#   wrong base           the client's copy of payload-1 changed  -> refused, the payload downloaded whole
#   window over the cap  delta_window_log_max = 26               -> not tried (no .delta fetched), downloaded whole
#   --no-deltas          nothing but whole NARs
#
# `mutant` (a sed program over rimed's delta.rs) is for nix/release/delta/lab/update-delta-mutants.sh: a mutant
# rimed must FAIL this test.
#
# `pair = { old; new; }` (two toplevels; nix/release/delta/lab/g3-vm.sh passes the weekly pair of delta.md §3) turns
# this into G3's measurement instead: the server publishes `old`, then `new` with deltas; the client gets `old`'s
# closure from the cache (setup, not measured) and runs `rime update --download-only` for `new` four times, with and
# without deltas, on the plain virtual link and on one shaped to 100 Mbit/s. Bytes, wall clock and CPU are printed
# as one JSON line per run ("G3 RESULT").
{ pkgs, self, mutant ? null, pair ? null }:

let
  lib = pkgs.lib;
  release = import ../release { inherit pkgs self; };
  tool = release.rime-nix-release;
  REPO = "server:5000/andrenijman/rime-os";

  # 8 MiB of seeded random bytes; version 2 changes three short runs. A whole NAR of it does not compress; its delta
  # against version 1 is a few KB.
  payload = v: pkgs.runCommand "rime-delta-payload-${v}" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    mkdir -p $out/share/rime-delta-payload
    python3 - $out/share/rime-delta-payload/blob ${v} <<'PY'
    import random, sys
    data = bytearray(random.Random(20261102).randbytes(8 << 20))
    if sys.argv[2] == "2":
        for off in (4096, 3 << 20, (8 << 20) - 8192):
            data[off:off + 64] = b"rime-delta-payload version 2 ".ljust(64, b".")
    open(sys.argv[1], "wb").write(data)
    PY
    echo ${v} > $out/share/rime-delta-payload/version
  '';

  versioned =
    { config, lib, ... }:
    {
      options.rimeTest.payload = lib.mkOption { type = lib.types.str; default = "1"; };
      # Through /etc: a package with only share/rime-delta-payload would not be linked into the system path, so
      # nothing would refer to it and it would not be in the closure.
      config.environment.etc."rime-delta-payload".source = payload config.rimeTest.payload;
    };

  rimedMutant = pkgs.rime.rimed.overrideAttrs (old: {
    pname = "rimed-mutant";
    doCheck = false;
    postPatch = (old.postPatch or "") + ''
      cp rimed/rime/src/nixos/delta.rs /tmp/delta.rs.orig
      sed -i ${lib.escapeShellArg mutant} rimed/rime/src/nixos/delta.rs
      if cmp -s /tmp/delta.rs.orig rimed/rime/src/nixos/delta.rs; then echo "mutant: sed changed nothing"; exit 1; fi
    '';
  });

  g3 = pair != null;
  # A plan for a toplevel with no configuration at hand (the pair): no components, which only `rime update`'s live
  # classification reads, and the G3 runs stop at --download-only.
  rawPlan = name: top: pkgs.writeText "rime-delta-plan-${name}.json" (builtins.toJSON {
    variants.plain = {
      toplevel = builtins.unsafeDiscardStringContext (toString top);
      publishable = false;
      note = "TEST: update-delta G3";
      components = { };
    };
    catalogue = [ ];
  });

  hosts = nodes: "${nodes.server.networking.primaryIPAddress} pages.test api.test github.test objects.test";
in
pkgs.testers.runNixOSTest {
  name = "rime-update-delta" + lib.optionalString (mutant != null) "-mutant" + lib.optionalString g3 "-g3";
  node.pkgsReadOnly = false;
  globalTimeout = if g3 then 6 * 3600 else 3600;

  nodes.server =
    { nodes, ... }:
    {
      nix.package = pkgs.nixVersions.latest;
      nix.settings = {
        experimental-features = [ "nix-command" ];
        substituters = lib.mkForce [ ];
      };
      networking.extraHosts = hosts nodes;
      virtualisation.memorySize = if g3 then 6144 else 4096;
      virtualisation.diskSize = if g3 then 40960 else 20480;
      virtualisation.cores = if g3 then 8 else 4;
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
      environment.systemPackages = [ tool release.cosign pkgs.curl pkgs.skopeo pkgs.openssl pkgs.git pkgs.jq ];
      environment.etc = lib.mapAttrs' (n: p: lib.nameValuePair "rime-delta/plan-${n}.json" { source = p; })
        (nodes.client.system.build.rimePlans
          // lib.optionalAttrs g3 { OLD = rawPlan "OLD" pair.old; NEW = rawPlan "NEW" pair.new; });
      system.extraDependencies = builtins.attrValues nodes.client.system.build.rimeReleases
        ++ lib.optionals g3 [ pair.old pair.new ];
    };

  nodes.client =
    { config, lib, pkgs, nodes, extendModules, ... }:
    let
      ext = m: (extendModules { modules = [ m ]; }).config;
      cfgs = {
        A = config;
        B = ext { rimeTest.payload = "2"; };
      };
      str = p: builtins.unsafeDiscardStringContext (toString p);
      components = import ../release/components.nix { inherit lib; };
      plan = name: c: pkgs.writeText "rime-delta-plan-${name}.json" (builtins.toJSON {
        variants.plain = {
          toplevel = str c.system.build.toplevel;
          publishable = false;
          note = "TEST: update-delta";
          components = lib.mapAttrs (_: map str) (components c pkgs);
        };
        catalogue = [ ];
      });
    in
    {
      imports = [ self.nixosModules.default versioned ];

      system.build.rimeReleases = lib.mapAttrs (_: c: c.system.build.toplevel) cfgs;
      system.build.rimePlans = lib.mapAttrs plan cfgs;
      system.build.rimePayloads = { A = payload "1"; B = payload "2"; };
      system.build.rimeUpstreamRoots = [ config.systemd.package config.nix.package pkgs.stdenv.cc.libc pkgs.coreutils ];

      networking.extraHosts = hosts nodes;
      # The store is the disk's own (as update-e2e.nix): B's paths are not on it until `rime update` brings them.
      virtualisation.useBootLoader = true;
      virtualisation.useEFIBoot = true;
      boot.loader.systemd-boot.enable = true;
      boot.loader.efi.canTouchEfiVariables = true;
      boot.growPartition = true;
      system.switch.enable = true;
      nix.enable = true;
      # G3: a mid-range machine (4 CPUs, 8 GB), and a kernel that is not the pair's, so the new release's kernel
      # is not already on the client's disk as part of its own system.
      virtualisation.memorySize = if g3 then 8192 else 2048;
      virtualisation.cores = if g3 then 4 else 2;
      virtualisation.diskSize = if g3 then 40960 else 12288;
      boot.kernelPackages = lib.mkIf g3 pkgs.linuxPackages_6_12;
      virtualisation.containers.registries.settings.registry = [ { location = "server:5000"; insecure = true; } ];

      rime.services.enable = true;
      rime.services.rimed.package = lib.mkIf (mutant != null) rimedMutant;
      rime.update = {
        repository = REPO;
        cacheUrl = "http://pages.test:8090";
        upstreamCacheUrl = "http://server:8080";
        fulcioRoot = "/var/lib/rime-test/fulcio-root.pem";
        keepGenerations = 2;
        deltas.url = "http://github.test:8082/AndreNijman/rime-cache/releases/download";
      };
    };

  testScript =
    { nodes, ... }:
    let
      b = nodes.client.system.build;

  # G3 on the weekly pair: setup, then four measured runs (docs/nixos/delta.md §5).
  g3Script = ''
      import time

      OLD, NEW = "${toString pair.old}", "${toString pair.new}"

      def busy_s():
          """CPU seconds the whole client spent (rime, curl, zstd, nix, the nix-daemon), from /proc/stat."""
          f = client.succeed("head -1 /proc/stat").split()[1:]
          user, nice, system, idle, iowait, irq, softirq = (int(x) for x in f[:7])
          return (user + nice + system + irq + softirq) / 100.0, iowait / 100.0

      def rx_bytes():
          return int(client.succeed("cat /sys/class/net/eth1/statistics/rx_bytes").strip())

      def asset_sizes():
          st = json.loads(server.succeed("cat /var/lib/fake-github/state.json"))
          return {f"{r['tag_name']}/{a['name']}": a["size"] for r in st["releases"] for a in r["assets"]}

      with subtest("G3 setup: publish the old release; the client gets its closure (not measured)"):
          man_old = publish("OLD", OLD, "d" * 40, "2026.10.23")
          for k in ("cache.pub", "up.pub"):
              server.copy_from_machine(f"/root/{k}", "")
              client.copy_from_host(str(server.out_dir / k), f"/root/{k}")
          t0 = time.monotonic()
          # Rime's paths come from the release cache, the split's upstream paths (glibc, ...) from the fake upstream.
          client.succeed(f"nix-store --realise {OLD} --option substituters '{PAGES} {UPSTREAM}'"
                         " --option trusted-public-keys \"$(cat /root/cache.pub) $(cat /root/up.pub)\""
                         " > /tmp/g3-setup.log 2>&1 || { tail -20 /tmp/g3-setup.log; false; }")
          print(f"G3 setup: the old closure copied to the client in {time.monotonic() - t0:.0f} s")
          client.succeed(f"nix-store --check-validity {OLD}")

      with subtest("G3 setup: publish the new release with deltas against the old"):
          t0 = time.monotonic()
          # man_b and old: the names the shared helpers (new_paths) read
          man_b = publish("NEW", NEW, "e" * 40, "2026.10.30", deltas=True)
          print(f"G3 setup: deltas + publish of the new release in {time.monotonic() - t0:.0f} s")
          d = man_b["delta"]
          print(f"G3 delta table: {len(d['files'])} deltas, {d['delta_size']} B")
          closure = [e["path"] for e in man_b["paths"]]
          invalid = set(client.succeed("nix-store --check-validity --print-invalid " + " ".join(closure)).split())
          missing = [p for p in closure if p in invalid]
          old = {e["path"] for e in man_old["paths"]}
          print(f"G3: {len(missing)} paths of the new release are not on the client"
                f" ({len([p for p in closure if p not in old])} not in the old release)")
          sizes = {e["path"]: e for e in man_b["paths"]}
          print("G3: whole download of those, by the manifest:",
                sum(sizes[p]["file_size"] for p in missing), "B; NAR", sum(sizes[p]["nar_size"] for p in missing), "B")
          server.succeed(f": > {DLOG}")

      def forget():
          client.succeed("nix-store --delete " + " ".join(missing) + " >/dev/null")
          client.fail(f"nix-store --check-validity {NEW}")

      def run(label, *args):
          client.succeed("sync; echo 3 > /proc/sys/vm/drop_caches")
          server.succeed("sync; echo 3 > /proc/sys/vm/drop_caches")
          host_load = open("/proc/loadavg").read().split()[0]
          (c0, w0), r0 = busy_s(), rx_bytes()
          t0 = time.monotonic()
          code, out = update(*args)
          wall = time.monotonic() - t0
          (c1, w1), r1 = busy_s(), rx_bytes()
          assert code == 0, out
          client.succeed(f"nix-store --check-validity {NEW}")
          client.succeed("nix-store --verify-path " + " ".join(missing[:40]))
          got, sizes_by = fetched(), asset_sizes()
          m = re.search(r"downloaded in ([0-9.]+) s", out)
          res = {
              "run": label, "wall_s": round(wall, 1), "download_phase_s": float(m.group(1)) if m else None,
              "cpu_s": round(c1 - c0, 1), "iowait_s": round(w1 - w0, 1), "rx_bytes": r1 - r0,
              "github_files": len(got), "github_bytes": sum(sizes_by.get(g, 0) for g in got),
              "delta_files": len([g for g in got if g.endswith(".delta")]),
              "host_load": host_load,
              "summary": [l for l in out.splitlines() if "rebuilt from deltas" in l or "to download" in l],
              "refused": len([l for l in out.splitlines() if "delta refused" in l]),
          }
          print("G3 RESULT " + json.dumps(res))
          forget()
          return res

      with subtest("G3: four runs, with and without deltas, on the plain link and at 100 Mbit/s"):
          r = [run("deltas"), run("whole", "--no-deltas")]
          server.succeed("tc qdisc replace dev eth1 root tbf rate 100mbit burst 256kb latency 400ms")
          r += [run("deltas-100mbit"), run("whole-100mbit", "--no-deltas")]
          server.succeed("tc qdisc del dev eth1 root")
          print("G3 RESULTS " + json.dumps(r))
  '';
    in
    ''
      import json, os, re

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
      A, B = "${b.rimeReleases.A}", "${b.rimeReleases.B}"
      PAY_A, PAY_B = "${b.rimePayloads.A}", "${b.rimePayloads.B}"
      UPSTREAM_ROOTS = "${lib.concatStringsSep " " (map toString b.rimeUpstreamRoots)}"
      CLIENT_IP = "${nodes.client.networking.primaryIPAddress}"
      DLOG = "/var/lib/fake-github/downloads.log"

      def hash_of(p):
          return os.path.basename(p)[:32]

      def publish(name, top, os_sha, rid, deltas=False):
          d = f"/root/{name}"
          server.succeed(
              f"mkdir -p {d}",
              f"rime-nix-release closure --variant plain={top} --out {d}/closure.json",
              f"rime-nix-release split --closure {d}/closure.json --upstream {UPSTREAM} --out {d}/split.json",
              f"rime-nix-release stage --closure {d}/closure.json --split {d}/split.json --out {d}/artifact",
          )
          if deltas:
              # As the build job: the previous release comes from the channel, its signature verified.
              print(server.succeed(
                  f"rime-nix-release deltas --artifact {d}/artifact --repo {REPO} --plain-http"
                  f" --fulcio-root /root/sign/fulcio-root.pem --signer {SIGNER} --issuer {ISSUER}"
                  f" --upstream {UPSTREAM} --jobs 2 2>&1"))
          rc, log = server.execute(
              f"RIME_CACHE_TOKEN={TOKEN} rime-nix-release publish --backend github --artifact {d}/artifact"
              f" --plan /etc/rime-delta/plan-{name}.json --secret-key-file /root/cache.sec"
              f" --upstream {UPSTREAM} --upstream-key \"$(cat /root/up.pub)\""
              f" --gh-repo AndreNijman/rime-cache --gh-remote {GIT} --gh-api {API} --gh-uploads {API} --gh-download {DOWNLOAD}"
              f" --public {PAGES} --cache-pubkey \"$(cat /root/cache.pub)\" --cache-url-hint {PAGES}"
              f" --repo {REPO} --plain-http --fulcio-root /root/sign/fulcio-root.pem"
              f" --rime-os {os_sha} --rime-shell {SHELL_SHA} --nixpkgs {NIXPKGS} --release {rid}"
              f" --allow-test-variants --test-signing /root/sign --out {d}/out 2>&1")
          print(log)
          assert rc == 0, f"publish {name} failed"
          man = json.loads(server.succeed(f"cat {d}/out/manifest.json"))
          server.succeed(f"rm -rf {d}/artifact")
          return man

      def update(*args):
          code, out = client.execute("rime update --skip-firmware --download-only " + " ".join(args) + " 2>&1")
          print(out)
          return code, out

      def fetched():
          """What the client asked github.test for since the last call (TAG/NAME per line)."""
          out = server.succeed(f"cat {DLOG} 2>/dev/null; : > {DLOG}")
          return [l.split()[2] for l in out.splitlines() if l.startswith(CLIENT_IP + " GET ")]

      def nar_name(path):
          info = server.succeed(f"cat /var/lib/pages/{hash_of(path)}.narinfo")
          m = re.search(r"^URL: (.+)$", info, re.M)
          assert m, info
          return m.group(1).rsplit("/", 1)[1]

      def new_paths():
          return [e["path"] for e in man_b["paths"] if e["path"] not in old]

      def forget_b():
          """Delete B's new paths from the client, so the next subtest downloads them again."""
          client.succeed("nix-store --delete " + " ".join(new_paths()) + " >/dev/null 2>&1")
          client.fail(f"nix-store --check-validity {PAY_B}")

      def downloaded_ok():
          client.succeed(f"nix-store --check-validity {B} {PAY_B}")
          client.succeed(f"nix-store --verify-path {PAY_B}")
          assert client.succeed(f"cat {PAY_B}/share/rime-delta-payload/version").strip() == "2"
          assert link("/run/current-system") == A, "--download-only installed something"

      def link(p):
          return client.succeed(f"readlink -f {p}").strip()

      start_all()
      for u in ("docker-registry.service", "upstream-cache.service", "pages.service", "fake-github.service"):
          server.wait_for_unit(u)
      for p in (5000, 8080, 8081, 8082, 8083, 8090):
          server.wait_for_open_port(p)
      client.wait_for_unit("multi-user.target")
      assert link("/run/current-system") == A
      client.wait_for_unit("growpart.service")
      client.succeed("resize2fs /dev/disk/by-label/nixos")
      client.succeed(f"nix-store --check-validity {PAY_A}")
      client.fail(f"nix-store --check-validity {PAY_B}")

      with subtest("throwaway keys, the fake upstream, rime-cache bootstrapped"):
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
          server.succeed(f"nix copy --to 'file:///var/lib/upstream?compression=zstd' {UPSTREAM_ROOTS}")
          server.succeed(f"nix store sign --store file:///var/lib/upstream --key-file /root/up.sec -r {UPSTREAM_ROOTS}")
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
    ''
    + (if g3 then g3Script else ''
      with subtest("release A (no previous release: no deltas); release B with deltas against A"):
          man_a = publish("A", A, "a" * 40, "2026.11.01", deltas=True)
          assert not (man_a.get("delta") or {}).get("files"), man_a.get("delta")
          old = {e["path"] for e in man_a["paths"]}
          man_b = publish("B", B, "b" * 40, "2026.11.02", deltas=True)
          files = man_b["delta"]["files"]
          print(json.dumps(man_b["delta"], indent=1))
          idx = {e["path"]: i for i, e in enumerate(man_b["paths"])}
          by_path = {man_b["paths"][f["path"]]["path"]: f for f in files}
          assert PAY_B in by_path, "no delta for the payload"
          dp = by_path[PAY_B]
          assert dp["base"] == PAY_A and dp["file"].startswith("2026.11.02") and dp["window_log"] == 27, dp
          pay_entry = man_b["paths"][idx[PAY_B]]
          assert dp["file_size"] * 50 < pay_entry["nar_size"], ("the payload delta is not small", dp, pay_entry)
          PAY_DELTA = dp["file"].split("/", 1)[1]
          PAY_NAR = nar_name(PAY_B)
          server.succeed(f": > {DLOG}")

      with subtest("deltas used: the payload is rebuilt from its delta, its whole NAR never fetched"):
          code, out = update()
          assert code == 0, out
          assert re.search(r"rime: \d+ paths? rebuilt from deltas", out), out
          assert "delta refused" not in out, out
          got = fetched()
          print(got)
          assert any(g.endswith("/" + PAY_DELTA) for g in got), got
          assert not any(g.endswith("/" + PAY_NAR) for g in got), "the payload's whole NAR was fetched"
          downloaded_ok()
          forget_b()

      with subtest("a tampered delta is refused and the payload downloaded whole"):
          asset = server.succeed(
              f"jq -r '.releases[].assets[] | select(.name == \"{PAY_DELTA}\") | .id' /var/lib/fake-github/state.json").strip()
          f = f"/var/lib/fake-github/files/{asset}"
          server.succeed(f"cp {f} /root/delta.orig",
                         f"printf '\\x55' | dd of={f} bs=1 seek=100 conv=notrunc status=none",
                         f"! cmp -s {f} /root/delta.orig")
          code, out = update()
          assert code == 0, out
          assert f"delta refused, downloaded whole instead: {PAY_B}: the delta file is not the one the manifest names" in out, out
          got = fetched()
          assert any(g.endswith("/" + PAY_DELTA) for g in got) and any(g.endswith("/" + PAY_NAR) for g in got), got
          downloaded_ok()
          forget_b()
          server.succeed(f"cp /root/delta.orig {f}")

      with subtest("a wrong base (the client's copy of payload-1 changed) is refused; the payload downloaded whole"):
          blob = f"{PAY_A}/share/rime-delta-payload/blob"
          client.succeed("mount -o remount,rw,bind /nix/store",
                         f"cp {blob} /root/blob.orig",
                         f"printf 'X' | dd of={blob} bs=1 seek=4242 conv=notrunc status=none")
          code, out = update()
          assert code == 0, out
          assert f"delta refused, downloaded whole instead: {PAY_B}: the local {PAY_A} is not the base" in out, out
          got = fetched()
          assert any(g.endswith("/" + PAY_NAR) for g in got), got
          downloaded_ok()
          forget_b()
          client.succeed(f"cp /root/blob.orig {blob}", "mount -o remount,ro,bind /nix/store",
                         f"nix-store --verify-path {PAY_A}")

      with subtest("a delta over the window cap is not tried: no .delta fetched, the payload downloaded whole"):
          client.fail("grep '^\\[' /etc/rime/update.toml")  # no [table]: an appended key lands at the top level
          client.succeed("cp -L /etc/rime/update.toml /root/update.toml",
                         "rm /etc/rime/update.toml",
                         "cp /root/update.toml /etc/rime/update.toml",
                         "echo 'delta_window_log_max = 26' >> /etc/rime/update.toml")
          code, out = update()
          assert code == 0, out
          assert "delta not tried" in out and "decoder window" in out, out
          got = fetched()
          assert not any(g.endswith("/" + PAY_DELTA) for g in got), got
          assert any(g.endswith("/" + PAY_NAR) for g in got), got
          downloaded_ok()
          forget_b()
          client.succeed("cp /root/update.toml /etc/rime/update.toml")

      with subtest("--no-deltas: whole NARs only"):
          code, out = update("--no-deltas")
          assert code == 0, out
          assert "deltas not used: --no-deltas" in out, out
          got = fetched()
          assert not any(g.endswith(".delta") for g in got), got
          assert any(g.endswith("/" + PAY_NAR) for g in got), got
          downloaded_ok()
    '');
}
