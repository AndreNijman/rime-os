# checks.update-floors: the disk floors of nix/release/floors.nix against the REAL published variants, installed in a
# VM (docs/nixos/closure.md §12, docs/nixos/updates-and-release.md §11.5).
#
# `measure` (a VM test, passthru.measure) installs each of the four release variant toplevels of
# nix/release/variants.nix with nixos-install (--system, the same copy and profile step the installer runs; no boot
# loader, no channel) onto a fresh disk, twice: ext4, and btrfs mounted compress-force=zstd:3 as D2/D9 mount @nix.
# It records `df -B1` used after the install and again after `nix store optimise` (base.nix sets auto-optimise-store,
# so a running machine is the optimised figure), compsize on btrfs, and the closure's NAR size and path count from the
# installed store's own database. summary.tsv is the whole result.
#
# The check itself is cheap: it reads summary.tsv and fails when
#   1. a published floor (floors.nix `published` / `publishedPartition`) no longer holds for the system as built:
#      installed on disk + the gate's need for one whole new closure (ceil(NAR x ratio) + reserve) + home, plus the
#      installer's ESP and GPT for the whole disk;
#   2. the gate's compressed ratio (floors.nix `ratioPermille`) is below what the btrfs install measured (the gate
#      would then promise room it does not have);
#   3. ext4 needs more than the NAR size plus the gate's 1 GB reserve (the gate's 1.0 branch would then promise
#      too much on an ext4 store; ext4 does need 3.6-4.5 % more than the NAR size, which the reserve absorbs);
#   4. floors.nix's recorded measurement is more than 3 % away from this one (the numbers in the docs are stale:
#      re-measure and edit floors.nix).
#
# Mutants (passthru.mutants.<name>, built by hand, each must FAIL, none re-measures):
#   published-low   the published plain-virtualization disk floor 1 GB lower
#   ratio-low       the gate's ratio 0.45
#   stale-measure   floors.nix's plain on-disk figure 10 % lower
#   reserve-small   the gate's reserve 0.4 GB (below ext4's measured overhead: the 1.0 branch would promise too much)
#
# Not in hosted CI (nix/ci/checks-not-in-ci.txt): the variants carry the Rime kernel, which no binary cache holds yet,
# and the four installs write about 50 GB through a 40 GB disk image.
{ pkgs, self }:

let
  lib = pkgs.lib;
  release = import ../release { inherit pkgs self; };
  floors = import ../release/floors.nix;
  tops = lib.mapAttrs (_: v: v.system.config.system.build.toplevel) release.variants;

  measure = pkgs.testers.runNixOSTest {
    name = "rime-update-floors-measure";
    # Eight installs of 10-13 GB each through the host store share.
    globalTimeout = 5 * 3600;

    nodes.machine =
      { config, ... }:
      {
        virtualisation.memorySize = 4096;
        virtualisation.cores = 4;
        # One 40 GiB disk, formatted afresh for every install.
        virtualisation.emptyDiskImages = [ 40960 ];
        # The four toplevels are valid in the VM's store (registered, read through the host store share): the
        # install copies them from there, as the installer copies from its cache.
        virtualisation.additionalPaths = builtins.attrValues tops;
        nix.settings.experimental-features = [ "nix-command" ];
        environment.systemPackages = [ config.system.build.nixos-install pkgs.btrfs-progs pkgs.compsize ];
      };

    testScript = ''
      import json

      TOPS = json.loads('${builtins.toJSON (lib.mapAttrs (_: t: "${t}") tops)}')
      DISK = "/dev/vdb"

      machine.wait_for_unit("multi-user.target")

      def used():
          machine.succeed("sync")
          return int(machine.succeed("df -B1 --output=used /mnt | tail -1").strip())

      rows = []
      for fs in ["btrfs", "ext4"]:
          for v in sorted(TOPS):
              t = TOPS[v]
              with subtest(f"{v} on {fs}"):
                  if fs == "btrfs":
                      mk, opts = f"mkfs.btrfs -q -f {DISK}", "-o compress-force=zstd:3"
                  else:
                      mk, opts = f"mkfs.ext4 -q -F {DISK}", ""
                  machine.succeed("wipefs -a " + DISK, mk, "mkdir -p /mnt", f"mount {opts} {DISK} /mnt")
                  empty = used()
                  machine.succeed(
                      f"nixos-install --system {t} --root /mnt --no-bootloader --no-root-passwd --no-channel-copy >&2",
                      timeout=7200,
                  )
                  if fs == "btrfs":
                      machine.succeed("btrfs filesystem sync /mnt")
                  installed = used()
                  machine.succeed("nix store optimise --store 'local?root=/mnt' >&2", timeout=7200)
                  if fs == "btrfs":
                      machine.succeed("btrfs filesystem sync /mnt")
                  optimised = used()
                  info = json.loads(machine.succeed(
                      f"nix path-info --json --json-format 1 --recursive --store 'local?root=/mnt' {t}"))
                  nar = sum(i["narSize"] for i in info.values())
                  comp = ""
                  if fs == "btrfs":
                      comp = machine.succeed("compsize -b /mnt/nix/store | awk '$1 == \"TOTAL\" { print $3 }'").strip()
                  # The profile points at the variant: this is the system the disk would boot.
                  assert machine.succeed("readlink -f /mnt/nix/var/nix/profiles/system").strip() == t
                  machine.succeed("umount /mnt")
                  row = [v, fs, str(len(info)), str(nar), str(empty), str(installed), str(optimised), comp or "-", t]
                  print("\t".join(row))
                  rows.append(row)

      head = ["variant", "fs", "paths", "nar_bytes", "df_used_empty", "df_used_installed", "df_used_optimised",
              "compsize_disk", "toplevel"]
      machine.succeed("cat > /tmp/summary.tsv <<'EOF'\n" + "\n".join("\t".join(r) for r in [head] + rows) + "\nEOF")
      machine.copy_from_machine("/tmp/summary.tsv", "")
    '';
  };

  check = name: fl: pkgs.runCommand "rime-update-floors${lib.optionalString (name != "") "-mutant-${name}"}"
    {
      nativeBuildInputs = [ pkgs.python3 ];
      floors = pkgs.writeText "floors.json" (builtins.toJSON {
        inherit (fl) ratioPermille reserve home espBytes gptBytes GB published publishedPartition;
        measured = lib.mapAttrs (_: m: { inherit (m) unpacked onDisk; }) fl.measured;
      });
      summary = "${measure}/summary.tsv";
    }
    ''
      python3 -I ${./update-floors-check.py} "$summary" "$floors" | tee $out
    '';

  mutate = f: lib.recursiveUpdate floors f;
in
(check "" floors).overrideAttrs (o: {
  passthru = (o.passthru or { }) // {
    inherit measure;
    mutants = {
      published-low = check "published-low" (mutate { published.plain-virtualization = floors.published.plain-virtualization - 1; });
      ratio-low = check "ratio-low" (mutate { ratioPermille = 450; });
      stale-measure = check "stale-measure" (mutate {
        measured.plain.onDisk = floors.measured.plain.onDisk * 9 / 10;
      });
      reserve-small = check "reserve-small" (mutate { reserve = 400000000; });
    };
  };
})
