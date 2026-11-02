# The filesystems of a FRESH Rime OS NixOS install (PLAN.md D9 + D2), as installer/rime-install-nixos.sh lays the
# disk out:
#
#   GPT  1  EFI System Partition  FAT32, label RIME-ESP, mounted at /boot. Rime's own ESP, never another OS's (D3
#                                 ESP rules): Fedora's shim + GRUB (bootupd's static config) live here, and the
#                                 boot hook (rime.boot, layout bls) writes the BLS entries, kernels and initrds.
#        2  root                  btrfs, label rime-root, either directly (plain) or inside a LUKS2 volume (the
#                                 boot hook adds rd.luks.uuid= from host.toml; nix/modules/luks.nix opens it).
#
#   subvolumes (D2's names, so fresh and migrated machines read the same paths):
#     @nixos  /          the NixOS root (/etc, /var)
#     @nix    /nix       the store
#     @home   /var/home  homes; /home links to it, as on bootc
#   every one mounted compress-force=zstd:3 (D9).
#
# One generic toplevel serves plain and LUKS machines (D9 named two, rime-plain and rime-luks; the only difference
# would be the LUKS initrd support, which costs nothing when no rd.luks.uuid= is given, so there is one: `rime`).
# Everything per machine (LUKS UUID, keymap, hostname, timezone, locale) comes from /var/lib/rime/host.toml.
#
# OVERLAP (handover to the integrator): the bridge's nix/modules/rootfs.nix (rime.rootfs, branch nix/bridge) mounts
# the same three subvolumes of the same label for a MIGRATED machine, plus bootc's /boot bind. The two can become one
# module with a `boot = "esp" | "top-bind"` switch once both are on feat/nixos.
{ config, lib, ... }:

let
  cfg = config.rime.installTarget;
  dev = "/dev/disk/by-label/${cfg.rootLabel}";
  sub = s: {
    device = dev;
    fsType = "btrfs";
    options = [ "subvol=${s}" "compress-force=zstd:3" ];
  };
in
{
  options.rime.installTarget = {
    enable = lib.mkEnableOption "the fresh-install disk layout the Rime installer writes (D9)";
    rootLabel = lib.mkOption {
      type = lib.types.str;
      default = "rime-root";
      description = "Label of the btrfs holding @nixos, @nix and @home (installer/rime-install-nixos.sh writes it).";
    };
    espLabel = lib.mkOption {
      type = lib.types.str;
      default = "RIME-ESP";
      description = "FAT label of Rime's own EFI System Partition, mounted at /boot.";
    };
  };

  config = lib.mkIf cfg.enable {
    fileSystems = {
      "/" = sub "@nixos";
      "/nix" = sub "@nix" // { neededForBoot = true; };
      "/var/home" = sub "@home";
      "/boot" = {
        device = "/dev/disk/by-label/${cfg.espLabel}";
        fsType = "vfat";
        options = [ "umask=0077" ];
      };
    };
    boot.initrd.supportedFilesystems.btrfs = true;
    systemd.tmpfiles.rules = [ "L /home - - - - /var/home" ];

    # D3 v1 chain on a fresh install: BLS entries on the ESP for Fedora GRUB's blscfg.
    rime.boot.layout = lib.mkDefault "bls";
    # The generic toplevel opens a LUKS2 root when the command line names one (host.toml [boot] kernel_params).
    rime.luks.enable = lib.mkDefault true;
  };
}
