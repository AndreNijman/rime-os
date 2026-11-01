# rime.rootfs: the filesystems of a Rime OS NixOS machine whose root is the btrfs a bootc install made (PLAN.md D2).
#
# The bridge (`rime migrate start`, nix/pkgs/rime-bridge) creates three top-level subvolumes inside the existing
# filesystem (and LUKS2 container) and relabels the filesystem `rime-root` when it was `apex-root`:
#
#   @nixos  /          the NixOS root (/etc, /var)
#   @nix    /nix       the store, a subvolume of its own so it can be snapshotted apart from /
#   @home   /var/home  a reflink copy of the old home; /home links to it, as on bootc
#
# Every subvolume is mounted compress-force=zstd:3 (D2). The top level (subvolid=5) is mounted at `topMount` so
# bootc's /boot, a directory there, can be bound to /boot: the Rime boot hook (rime.boot, layout custom-cfg)
# writes its menu and kernels there and GRUB reads them from btrfs. ostree's tree stays where it was, so its GRUB
# entries still boot the old system (emergency only, D2).
#
# TODO(handover): a LUKS-A machine has /boot on its own ext4 partition (installer/rime-install); that needs the
# /boot source from host.toml (a generator), not this static bind. The bridge refuses nothing for it yet because
# no LUKS VM has run (B10).
{ config, lib, ... }:

let
  cfg = config.rime.rootfs;
  dev = "/dev/disk/by-label/${cfg.label}";
  sub = s: {
    device = dev;
    fsType = "btrfs";
    options = [ "subvol=${s}" "compress-force=zstd:3" ];
  };
in
{
  options.rime.rootfs = {
    enable = lib.mkEnableOption "the @nixos/@nix/@home btrfs layout a migrated bootc machine has (D2)";
    label = lib.mkOption {
      type = lib.types.str;
      default = "rime-root";
      description = "Label of the btrfs that holds the subvolumes (the bridge relabels apex-root to this).";
    };
    topMount = lib.mkOption {
      type = lib.types.str;
      default = "/mnt/rime-top";
      description = "Where the btrfs top level (subvolid=5) is mounted, so its /boot directory can be bound.";
    };
  };

  config = lib.mkIf cfg.enable {
    fileSystems = {
      "/" = sub "@nixos";
      "/nix" = sub "@nix" // { neededForBoot = true; };
      "/var/home" = sub "@home";
      ${cfg.topMount} = { device = dev; fsType = "btrfs"; options = [ "subvolid=5" ]; };
      "/boot" = { device = "${cfg.topMount}/boot"; fsType = "none"; options = [ "bind" ]; depends = [ cfg.topMount ]; };
    };
    boot.initrd.supportedFilesystems.btrfs = true;
    systemd.tmpfiles.rules = [ "L /home - - - - /var/home" ];
  };
}
