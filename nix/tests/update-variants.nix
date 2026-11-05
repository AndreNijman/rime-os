# checks.update-variants: `rime update` against a release carrying the REAL published variants, with the machine's
# store on btrfs mounted compress-force=zstd:3 (docs/nixos/updates-and-release.md §11.5, §11.6).
#
# server  an OCI registry and RustFS (an S3 store standing in for R2), as checks.update-client. One release,
#         2026.11.10, names the four toplevels of nix/release/variants.nix (`.#rime-release-toplevels`, built from
#         this commit: plain, plain-gaming, plain-virtualization, plain-gaming-virtualization), published with
#         THROWAWAY keys by fixture-publish.py through update-variants-publish.py (the variants table).
# client  a small test system (the update-client shape: systemd-boot on its own disk) whose update.toml base is
#         `plain`. Before the first update its whole /nix moves onto a second disk formatted btrfs and mounted
#         compress-force=zstd:3, the mount the installer and the bridge give @nix, with /nix/store bound read-only
#         over itself as NixOS does: from then on every path rime update downloads is written there, compressed,
#         and /proc/self/mounts names btrfs for /nix/store. (The test framework cannot boot a btrfs root: its disk
#         images are ext4. The store the gate measures is the store Nix writes to.)
#
# 1. The free-space gate's compressed branch (§11.5), by name: with free space just below ceil(0.55 x U) + 1 GB
#    (U = the release's missing NAR bytes, from `rime update --plan`) the update is refused naming the compressed
#    estimate; with free space between that and U + 1 GB, a figure the uncompressed (1.0) branch refuses, the update
#    installs. What the download then really took on disk is compared with the estimate.
# 2. Variant selection with the real toplevels (§11.6): the switch files `rime install steam` / `rime install qemu`
#    write move the machine plain -> plain-gaming -> plain -> plain-virtualization -> plain ->
#    plain-gaming-virtualization -> plain, each `rime update --no-live` ending with the profile on exactly that
#    variant's toplevel and that variant's own boot hook (nix/pkgs/rime-boot) having written the BLS entry of the
#    generation the update created (rime-generation-<N>.conf, N from the profile link, naming its init). None of
#    them is booted: they mount Rime's installed layout by label and boot through Fedora's shim and GRUB
#    (checks.installer-nixos and the OVMF installer lab boot that chain), so --no-live installs each for the next boot.
#
# Mutants (passthru.mutants.<name>, built by hand, each must FAIL):
#   ratio-one            update.toml's on_disk_ratio 1.0 (the gate counting unpacked bytes on this store)
#   compress-not-forced  the store mounted compress=zstd:3 instead of compress-force (rimed must count 1.0 there)
#   swapped-variants     the release publishes plain-gaming with plain-virtualization's toplevel and back
#   stale-entry          the newest generation's boot entry removed after each return to plain, as if the boot hook
#                        had not run (an older generation's entry still names plain's init)
#
# Not in hosted CI (nix/ci/checks-not-in-ci.txt): the variants carry the Rime kernel, which no binary cache holds yet.
{ pkgs, self }:

let
  lib = pkgs.lib;
  keys = pkgs.callPackage ../release/fixture-keys.nix { };
  repo = "server:5000/rime/os";
  publisher = pkgs.callPackage ../release/fixture-publish.nix { inherit keys repo; };
  release = import ../release { inherit pkgs self; };
  tops = lib.mapAttrs (_: v: v.system.config.system.build.toplevel) release.variants;
  s3key = "rimetestkey";
  s3secret = "rimetestsecret-TEST-only";

  variantsPublish = pkgs.runCommand "rime-variants-publish-TEST" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
    mkdir -p $out/bin
    makeWrapper ${pkgs.python3}/bin/python3 $out/bin/rime-variants-publish \
      --add-flags "-I ${./update-variants-publish.py} ${publisher}/share/fixture-publish.py" \
      --prefix PATH : ${lib.makeBinPath [ pkgs.skopeo pkgs.openssl pkgs.nix pkgs.rustfs-cli ]} \
      --set FIXTURE_KEYS ${keys} \
      --set FIXTURE_REPO ${repo} \
      --set FIXTURE_CACHE /srv/rime-cache \
      --set FIXTURE_BUCKET local/rime-cache
  '';

  mkTest = mutant: pkgs.testers.runNixOSTest {
    name = "rime-update-variants${lib.optionalString (mutant != "") "-mutant-${mutant}"}";
    node.pkgsReadOnly = false;
    # The cache: 14 GB of NARs compressed once; the client downloads about 10 GB of them.
    globalTimeout = 3 * 3600;

    nodes.server =
      { pkgs, ... }:
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
        environment.systemPackages = [ publisher variantsPublish pkgs.rustfs-cli ];
        # The release job's output: every variant's closure is valid in the server's store.
        system.extraDependencies = builtins.attrValues tops;
        virtualisation.memorySize = 4096;
        virtualisation.cores = 4;
        # The file:// cache and RustFS's copy of it (about 4.5 GB each, zstd).
        virtualisation.diskSize = 20480;
      };

    nodes.client =
      { config, ... }:
      {
        imports = [ self.nixosModules.default ];

        # The update-client shape: a real boot menu on a real disk, so `switch-to-configuration boot` runs a boot
        # loader hook against a real ESP.
        virtualisation.useBootLoader = true;
        boot.growPartition = true;
        virtualisation.useEFIBoot = true;
        boot.loader.systemd-boot.enable = true;
        boot.loader.efi.canTouchEfiVariables = true;
        system.switch.enable = true;
        nix.enable = true;
        environment.etc."machine-id".text = "a7c1f00dba5e4f6a9d3c2b1e0f9a8b7c\n";
        virtualisation.memorySize = 3072;
        virtualisation.cores = 4;
        virtualisation.diskSize = 8192;
        # The disk /nix moves to (btrfs). The real plain-gaming-virtualization system is about 7 GB on it.
        virtualisation.emptyDiskImages = [ 30720 ];
        virtualisation.containers.registries.settings.registry = [ { location = "server:5000"; insecure = true; } ];
        environment.systemPackages = [ pkgs.btrfs-progs pkgs.compsize ];

        rime.services.enable = true;
        # The catalogue names each feature's switch file: without /etc/rime/catalogue.json rimed reads no switch at
        # all (variant::switched_on), as on a system built without it. Every published variant carries it.
        rime.catalogue.enable = true;
        rime.update = {
          repository = repo;
          cacheUrl = "http://server:9000/rime-cache";
          fulcioRoot = "${keys}/ca.pem";
        };
      };

    testScript = ''
      import json, math, re

      TOPS = json.loads('${builtins.toJSON (lib.mapAttrs (_: t: "${t}") tops)}')
      P, PG = TOPS["plain"], TOPS["plain-gaming"]
      PV, PGV = TOPS["plain-virtualization"], TOPS["plain-gaming-virtualization"]
      MUTANT = "${mutant}"
      GB = 1000 * 1000 * 1000
      RATIO = 0.55

      def update(*args):
          code, out = client.execute("rime update --skip-firmware " + " ".join(args) + " 2>&1")
          print(out)
          return code, out

      def grab(pattern, out):
          m = re.search(pattern, out)
          assert m is not None, f"no {pattern!r} in: {out}"
          return m.group(1)

      def profile():
          return client.succeed("readlink -f /nix/var/nix/profiles/system").strip()

      def free():
          client.succeed("sync", "btrfs filesystem sync /nix")
          a, s = client.succeed("stat -f -c '%a %S' /nix/store").split()
          return int(a) * int(s)

      def fill_to(target):
          """Leave `target` bytes free on the store's filesystem (statvfs f_bavail, what the gate reads)."""
          client.succeed("rm -f /nix/rime-test-fill", "btrfs filesystem sync /nix")
          n = free() - target
          for _ in range(4):
              assert n > 0, f"cannot leave {target} B free"
              client.succeed(
                  "rm -f /nix/rime-test-fill",
                  "btrfs filesystem sync /nix",
                  f"fallocate -l {n} /nix/rime-test-fill",
                  "btrfs filesystem sync /nix",
              )
              got = free()
              if abs(got - target) <= 100 * 1000 * 1000:
                  return got
              n += got - target
          raise Exception(f"could not set free space to {target} B (have {got} B)")

      start_all()
      server.wait_for_unit("docker-registry.service")
      server.wait_for_unit("rustfs.service")
      server.wait_for_open_port(9000)
      server.succeed("rc alias set local http://127.0.0.1:9000 ${s3key} ${s3secret}")
      server.succeed("rc bucket create local/rime-cache")
      server.succeed("rc anonymous set download local/rime-cache")
      client.wait_for_unit("multi-user.target")
      client.wait_for_unit("growpart.service")
      client.succeed("resize2fs /dev/disk/by-label/nixos")

      with subtest("the store moves onto btrfs mounted compress-force=zstd:3"):
          opts = "compress=zstd:3" if MUTANT == "compress-not-forced" else "compress-force=zstd:3"
          client.succeed(
              "mkfs.btrfs -q -f -L rime-nix /dev/vdb",
              f"mkdir -p /run/newnix && mount -o {opts} /dev/vdb /run/newnix",
              "systemctl stop nix-daemon.socket nix-daemon.service",
              "tar -C /nix --exclude=./var/nix/daemon-socket -cf - . | tar -C /run/newnix -xpf -",
              "mkdir -p /run/newnix/var/nix/daemon-socket",
              "umount /run/newnix",
              f"mount -o {opts} /dev/vdb /nix",
              "mount --bind /nix/store /nix/store",
              "mount -o remount,ro,bind /nix/store",
              "systemctl start nix-daemon.socket",
          )
          m = client.succeed("awk '$2 == \"/nix/store\"' /proc/self/mounts").strip().splitlines()[-1].split()
          print(m)
          assert m[2] == "btrfs" and ("compress-force=zstd:3" in m[3]) == (MUTANT != "compress-not-forced"), m
          # The store database moved with it: the running system is valid in it.
          client.succeed("nix-store --verify-path $(readlink -f /run/current-system)")
          print(client.succeed("df -h /nix /"))

      if MUTANT == "ratio-one":
          client.succeed(
              "cp --remove-destination $(readlink -f /etc/rime/update.toml) /etc/rime/update.toml",
              "sed -i 's/^on_disk_ratio = .*/on_disk_ratio = 1.0/' /etc/rime/update.toml",
          )
      print(client.succeed("grep on_disk_ratio /etc/rime/update.toml"))

      # The real variant's boot hook writes Fedora GRUB's menu (D3); on a fresh install GRUB is on the ESP. The hook
      # refuses an ESP without it, so the test ESP gets the directory GRUB would read.
      client.succeed("mkdir -p /boot/grub2")

      with subtest("the release: the four real variants, signed"):
          server.succeed(f"rime-fixture-publish cache {P} {PG} {PV} {PGV}", timeout=7200)
          swap = "--swap plain-gaming,plain-virtualization" if MUTANT == "swapped-variants" else ""
          d = server.succeed(
              f"rime-variants-publish --release 2026.11.10 --variant plain={P} --variant plain-gaming={PG}"
              f" --variant plain-virtualization={PV} --variant plain-gaming-virtualization={PGV} {swap}"
          ).strip()
          server.succeed(f"rime-fixture-publish channel --serial 1 --entry edge={d}=2026.11.10")
          before = profile()

      with subtest("the gate on a compressed store: refused below ceil(0.55 x U) + 1 GB, by name"):
          code, out = update("--plan")
          assert code == 0, out
          U = int(float(grab(r"([0-9.]+) GB unpacked\)", out)) * GB)
          need = math.ceil(U * RATIO) + GB
          need_unpacked = U + GB
          print(f"U = {U} B: the gate needs {need} B on this store, {need_unpacked} B uncompressed")
          assert need_unpacked - need > 2 * GB, "the window between the two branches is too small to test"
          got = fill_to(need - 600 * 1000 * 1000)
          code, out = update("--no-live")
          assert code != 0, "the update was not refused"
          assert "Nothing was changed" in out and "on this compressed disk" in out, out
          est = float(grab(r"about ([0-9.]+) GB on this compressed disk", out)) * GB
          assert abs(est - U * RATIO) < 0.02 * GB, (est, U)
          assert profile() == before, "the refused update moved the profile"

      with subtest("the gate on a compressed store: passes where the uncompressed branch would refuse"):
          got = fill_to((need + need_unpacked) // 2)
          assert need < got < need_unpacked, (need, got, need_unpacked)
          code, out = update("--no-live")
          assert code == 0, out
          assert profile() == P, f"the profile is {profile()}, not the real plain {P}"
          after = free()
          spent = got - after
          print(f"the download took {spent} B on disk for {U} B of NAR: ratio {spent / U:.3f} (gate {RATIO})")
          assert spent < U * RATIO, "the gate's compressed estimate was below what the download took"
          client.succeed("rm -f /nix/rime-test-fill", "btrfs filesystem sync /nix")
          print(client.succeed("compsize -b /nix/store | tail -n +2"))

      def switch(feature, pkg, on):
          f = f"/var/lib/rime/switch/rime-feature@{feature}.target.on"
          client.succeed(f"mkdir -p /var/lib/rime/switch && echo {pkg} > {f}" if on else f"rm -f {f}")

      def lands(name, top):
          code, out = update("--no-live")
          assert code == 0, out
          if name != "plain":
              assert f"installing the {name} system" in out, out
          got = profile()
          assert got == top, f"{name}: the profile is {got}, not the real {name} {top}"
          # The variant's own boot hook ran and wrote the entry of THIS generation (nix/pkgs/rime-boot, BLS: one
          # rime-generation-<N>.conf per generation). Any generation's entry would not do: on every return to plain
          # an older generation's entry already names plain's init, whether the hook ran or not.
          link = client.succeed("readlink /nix/var/nix/profiles/system").strip()
          n = re.fullmatch(r"system-([0-9]+)-link", link)
          assert n is not None, f"the system profile points at {link!r}, not a generation link"
          entry = f"/boot/loader/entries/rime-generation-{n.group(1)}.conf"
          if MUTANT == "stale-entry" and name == "plain":
              # As if the hook had not run: this generation's entry is missing, older ones remain.
              client.succeed(f"rm -f {entry}")
          client.succeed(f"grep -F 'init={top}/init' {entry}")

      with subtest("gaming switched on: the real plain-gaming, and back"):
          switch("gaming", "steam", True)
          lands("plain-gaming", PG)
          client.succeed(f"test -e {PG}/sw/bin/steam")
          switch("gaming", "steam", False)
          lands("plain", P)

      with subtest("virtual machines switched on: the real plain-virtualization, and back"):
          switch("virtualization", "qemu", True)
          lands("plain-virtualization", PV)
          client.succeed(f"test -e {PV}/sw/bin/qemu-system-x86_64")
          switch("virtualization", "qemu", False)
          lands("plain", P)

      with subtest("both switched on: the real plain-gaming-virtualization, and back"):
          switch("gaming", "steam", True)
          switch("virtualization", "qemu", True)
          lands("plain-gaming-virtualization", PGV)
          switch("gaming", "steam", False)
          switch("virtualization", "qemu", False)
          lands("plain", P)
          print(client.succeed("df -B1 /nix; compsize -b /nix/store | tail -n +2"))
    '';
  };
in
mkTest ""
// {
  mutants = lib.genAttrs [ "ratio-one" "compress-not-forced" "swapped-variants" "stale-entry" ] mkTest;
}
