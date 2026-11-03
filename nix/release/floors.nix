# The disk floors (A11, docs/nixos/closure.md §6, §9-§11) and the on-disk estimate the update gate counts with
# (docs/nixos/updates-and-release.md §11.5), in ONE place. Pure data and integer arithmetic, no pkgs.
#
# Read by:
#   nix/modules/update.nix              `on_disk_ratio` -> /etc/rime/update.toml (the free-space gate of rime update)
#   nix/modules/packages-catalogue.nix  `floors` -> /etc/rime/catalogue.json (`rime install steam` refuses below the
#                                       floor of the system it would select)
#   nix/tests/release-plan.nix          checks.release-plan: the arithmetic below, and every published number
#   nix/installer/choices.nix           the installer's disk floors: `floors.<variant>.diskGB` for a whole disk,
#                                       `.partitionGB` for an existing partition (plain, and plain-gaming when gaming
#                                       is chosen), stamped into the ISO as nixos-choices.json and read by
#                                       installer/rime-install-nixos.sh (checks.installer-choices)
#
# Decision (2026-11-02, wave 3 "updates"): the gate counts ON-DISK bytes on a store mounted compress-force=zstd, as
# the unpacked NAR size times `ratioPermille`/1000 (rounded up), plus the 1 GB reserve. The ratio is the largest
# measured (plain 0.524 in closure.md §10; 0.520, 0.500, 0.502 and 0.486 for the four variants in §12) plus a margin,
# so the estimate errs high. Any other store filesystem counts the unpacked size (ratio 1); on ext4 a closure really
# takes 3.6-4.5 % more than its NAR size (§12), about 0.48 GB for every variant, which the 1 GB reserve absorbs
# (checks.update-floors holds the gate to that).
#
# A floor is what the disk must hold for the gate to accept one mass rebuild (every path new, the D6 §11.3 worst
# case) beside the installed system: partitions + the installed system on disk + the gate's need for a whole second
# closure + 1 GB for home and logs. `storeBytes` is the part on the root filesystem (what `rime install` compares
# with the size of the filesystem holding /nix); `diskGB` adds the installer's partitions and rounds up to a GB;
# `partitionGB` is `storeBytes` rounded up to a GB (the root partition when the ESP is not counted).
let
  GB = 1000000000;
  ceilDiv = a: b: (a + b - 1) / b;

  # closure.md §12 (wave 4 "updates2", 2026-11-03): all four release variant toplevels (`.#rime-release-toplevels` at
  # this tree) installed in a VM by checks.update-floors.measure (nixos-install --system, the installer's copy and
  # profile step) onto btrfs mounted compress-force=zstd:3; `onDisk` is `df` used after `nix store optimise` (a running
  # Rime optimises every path it adds: base.nix auto-optimise-store), `unpacked` the closure's NAR size from the
  # installed store's own database. checks.update-floors fails when a figure here is more than 3 % from what the VM
  # measures, so a release whose closure moves must be re-measured here.
  #
  # Before this, the two virtualization systems were ESTIMATED (plain's measurement + the NAR delta at the gate's
  # ratio); measured, they are smaller on disk than estimated (plain-virtualization 5,564,084,224 B against
  # 5,800,374,254 B), and their published floors drop by 1 GB. Decision: publish the measurement.
  # `virtualizationDelta` stays as a record: the measured NAR delta of plain-virtualization over plain (unchanged).
  virtualizationDelta = 650429872;
  measured = {
    plain = { unpacked = 10433745168; onDisk = 5423476736; estimated = false; };
    plain-gaming = { unpacked = 12631031056; onDisk = 6309990400; estimated = false; };
    plain-virtualization = { unpacked = 11084175040; onDisk = 5564084224; estimated = false; };
    plain-gaming-virtualization = { unpacked = 13281460928; onDisk = 6455635968; estimated = false; };
  };

  ratioPermille = 550;
  reserve = GB; # the gate's reserve (§11.5)
  home = GB; # home, logs, /var: the same allowance bootc's 12 GB figure carries (closure.md §6)

  # installer/rime-install-nixos.sh: NIXOS_ESP_MIB=2048, and sgdisk's 1 MiB start alignment plus the backup GPT.
  espBytes = 2048 * 1024 * 1024;
  gptBytes = 2 * 1024 * 1024;

  onDiskEstimate = unpacked: ceilDiv (unpacked * ratioPermille) 1000;

  floor = m: rec {
    storeBytes = m.onDisk + onDiskEstimate m.unpacked + reserve + home;
    diskBytes = storeBytes + espBytes + gptBytes;
    diskGB = ceilDiv diskBytes GB;
    # The root partition alone (the installer's "into a partition" mode, where the ESP already exists): the
    # "14 GB plain, 16 GB gaming" of the D5 handover is this figure.
    partitionGB = ceilDiv storeBytes GB;
  };
in
{
  inherit GB measured ratioPermille reserve home espBytes gptBytes onDiskEstimate virtualizationDelta;
  ratio = ratioPermille / 1000.0;
  # Per release variant (nix/release/variants.nix names). A variant with no floor of its own (one not measured yet)
  # takes the largest floor of the variants whose features it contains: rimed (variant::floor_for) does that.
  floors = builtins.mapAttrs (_: floor) measured;
  # The published figures, stated as numbers so a change to a measurement shows up as a failing check
  # (checks.release-plan recomputes them), not as a silent new floor.
  published = {
    plain = 16;
    plain-gaming = 18;
    plain-virtualization = 16;
    plain-gaming-virtualization = 18;
  };
  publishedPartition = {
    plain = 14;
    plain-gaming = 16;
    plain-virtualization = 14;
    plain-gaming-virtualization = 16;
  };
}
