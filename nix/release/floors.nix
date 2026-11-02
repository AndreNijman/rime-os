# The disk floors (A11, docs/nixos/closure.md §6, §9-§11) and the on-disk estimate the update gate counts with
# (docs/nixos/updates-and-release.md §11.5), in ONE place. Pure data and integer arithmetic, no pkgs.
#
# Read by:
#   nix/modules/update.nix              `on_disk_ratio` -> /etc/rime/update.toml (the free-space gate of rime update)
#   nix/modules/packages-catalogue.nix  `floors` -> /etc/rime/catalogue.json (`rime install steam` refuses below the
#                                       floor of the system it would select)
#   nix/tests/release-plan.nix          checks.release-plan: the arithmetic below, and every published number
#   the installer                       the disk floor and the gaming choice (installer2: read `diskGB` from here)
#
# Decision (2026-11-02, wave 3 "updates"): the gate counts ON-DISK bytes on a store mounted compress-force=zstd, as
# the unpacked NAR size times `ratioPermille`/1000 (rounded up), plus the 1 GB reserve. The ratio is the larger of the
# two measured (plain 0.524, plain-gaming 0.503, closure.md §10) plus a margin, so the estimate errs high. Any other
# store filesystem counts the unpacked size (ratio 1).
#
# A floor is what the disk must hold for the gate to accept one mass rebuild (every path new, the D6 §11.3 worst
# case) beside the installed system: partitions + the installed system on disk + the gate's need for a whole second
# closure + 1 GB for home and logs. `storeBytes` is the part on the root filesystem (what `rime install` compares
# with the size of the filesystem holding /nix); `diskGB` adds the installer's partitions and rounds up to a GB.
let
  GB = 1000000000;
  ceilDiv = a: b: (a + b - 1) / b;

  # closure.md §10: both toplevels measured in one run (2026-11-02), btrfs compress-force=zstd:3, `df` used after
  # `nix store optimise`. `unpacked` is the closure's NAR size.
  #
  # Virtualization was not installed and measured on disk. Its systems are ESTIMATED: the closure's NAR-size delta,
  # measured on 2026-11-02 (wave 3 "updates", the four release variants built at once: plain-virtualization adds 89
  # paths, 658,470,728 B, and drops 29, 8,040,856 B; the same 650,429,872 B net over plain-gaming), on top of the
  # measured system, with the on-disk part counted at the gate's own ratio.
  virtualizationDelta = 650429872;
  measured = rec {
    plain = { unpacked = 10382141376; onDisk = 5442637824; };
    plain-gaming = { unpacked = 12579425640; onDisk = 6327009280; };
    plain-virtualization = withVirtualization plain;
    plain-gaming-virtualization = withVirtualization plain-gaming;
  };
  withVirtualization = m: {
    unpacked = m.unpacked + virtualizationDelta;
    onDisk = m.onDisk + onDiskEstimate virtualizationDelta;
    estimated = true;
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
    plain-virtualization = 17;
    plain-gaming-virtualization = 19;
  };
}
