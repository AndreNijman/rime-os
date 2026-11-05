# rime.rootfs: the filesystems of every Rime OS NixOS machine, migrated or freshly installed (PLAN.md D2, D9).
# ONE option set for both (it replaced rime.installTarget's own copy; nix/installer/target.nix only maps onto it).
#
# The root is one btrfs, labelled `label`, directly or inside a LUKS2 volume, with three top-level subvolumes:
#
#   @nixos  /          the NixOS root (/etc, /var)
#   @nix    /nix       the store, a subvolume of its own so it can be snapshotted apart from /
#   @home   /var/home  homes; /home links to it, as on bootc
#
# Every subvolume is mounted compress-force=zstd:3 (D2). The bridge (`rime migrate start`, nix/pkgs/rime-bridge) makes
# them inside the bootc machine's existing filesystem and relabels it `rime-root` when it was `apex-root`; the
# installer (installer/rime-install-nixos.sh) makes them on a fresh disk.
#
# Where /boot comes from is the one difference, `boot`:
#
#   "bootc"  a MIGRATED machine. bootc left /boot either as a directory of the btrfs top level (the plain layout)
#            or as its own ext4 partition (the installer's LUKS layout, installer/rime-install). The bridge records
#            which in /var/lib/rime/host.toml ([boot] device + fstype for a partition), and the generator
#            rime-rootfs-boot (rootfs/boot-generator.py) writes boot.mount from it at every boot: the partition,
#            or a bind of `topMount`/boot (the top level, subvolid=5, is mounted at `topMount`). The Rime boot hook
#            (rime.boot, layout custom-cfg) writes its menu and kernels there and bootc's GRUB reads them; ostree's
#            entries stay, so the old system still boots (emergency only, D2). /boot is NOT in fileSystems: the
#            generator is its only owner, because the source is a per-machine fact, not a build-time one.
#   "esp"    a FRESH install: Rime's own ESP (FAT, label `espLabel`) at /boot; Fedora's shim + GRUB (bootupd's static
#            config) live there and the boot hook writes BLS entries for GRUB's blscfg (layout bls).
#
# `rime migrate finish` (nix/pkgs/rime-bridge/rime-migrate-finish.py, docs/nixos/bridge.md) removes the old side once
# NixOS has booted well three times: a migrated machine ("bootc") gets the helper on PATH (the `rime` CLI execs it by
# its /run/current-system/sw path) and rime-migrate-good-boot.service, which counts every boot that reaches
# multi-user.target from @nixos in /var/lib/rime/nixos-bridge/good-boots.json.
#
# LUKS: the initrd can open a LUKS2 root (rime.luks) in both cases; which volume comes from the kernel command line
# (host.toml [boot] kernel_params, rd.luks.*: the installer writes them, the bridge carries them from bootc's).
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.rootfs;
  dev = "/dev/disk/by-label/${cfg.label}";
  sub = s: {
    device = dev;
    fsType = "btrfs";
    options = [ "subvol=${s}" "compress-force=zstd:3" ];
  };
  bootGenerator = pkgs.runCommand "rime-rootfs-boot" { } ''
    install -Dm755 ${./rootfs/boot-generator.py} $out
    substituteInPlace $out \
      --replace-fail '#!/usr/bin/env python3' '#!${pkgs.python3}/bin/python3 -I' \
      --replace-fail '@topMount@' '${cfg.topMount}'
  '';
in
{
  options.rime.rootfs = {
    enable = lib.mkEnableOption "the @nixos/@nix/@home btrfs layout of a Rime OS machine (D2)";
    label = lib.mkOption {
      type = lib.types.str;
      default = "rime-root";
      description = "Label of the btrfs that holds the subvolumes (the bridge relabels apex-root to this).";
    };
    boot = lib.mkOption {
      type = lib.types.enum [ "bootc" "esp" ];
      default = "bootc";
      description = ''
        Where /boot is: "bootc" for a migrated machine (bootc's /boot, a top-level directory or its own partition,
        as host.toml [boot] records; mounted by the rime-rootfs-boot generator), "esp" for a fresh install (Rime's
        ESP, label espLabel).
      '';
    };
    espLabel = lib.mkOption {
      type = lib.types.str;
      default = "RIME-ESP";
      description = "FAT label of Rime's own EFI System Partition, mounted at /boot when boot = \"esp\".";
    };
    topMount = lib.mkOption {
      type = lib.types.str;
      default = "/mnt/rime-top";
      description = "Where the btrfs top level (subvolid=5) is mounted when boot = \"bootc\", so its /boot can be bound.";
    };
    bootGenerator = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = bootGenerator;
      description = "The rime-rootfs-boot generator this configuration installs (for the checks).";
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      fileSystems = {
        "/" = sub "@nixos";
        "/nix" = sub "@nix" // { neededForBoot = true; };
        "/var/home" = sub "@home";
      };
      boot.initrd.supportedFilesystems.btrfs = true;
      systemd.tmpfiles.rules = [ "L /home - - - - /var/home" ];
      # One toplevel opens a LUKS2 root when the command line names one (host.toml [boot] kernel_params).
      rime.luks.enable = lib.mkDefault true;
    }
    (lib.mkIf (cfg.boot == "bootc") {
      fileSystems.${cfg.topMount} = { device = dev; fsType = "btrfs"; options = [ "subvolid=5" ]; };
      systemd.generators.rime-rootfs-boot = "${bootGenerator}";
      # A /boot partition is ext4 (the installer's LUKS layout); its fsck and module must be there.
      boot.supportedFilesystems.ext4 = true;
      # Only finish on PATH, not the bootc-side bridge beside it (it loads that by its own store path).
      environment.systemPackages = [
        (pkgs.runCommand "rime-migrate-finish" { } ''
          mkdir -p $out/bin
          ln -s ${pkgs.rime.rime-bridge}/bin/rime-migrate-finish $out/bin/rime-migrate-finish
        '')
      ];
      systemd.services.rime-migrate-good-boot = {
        description = "Count a good NixOS boot for rime migrate finish";
        wantedBy = [ "multi-user.target" ];
        after = [ "multi-user.target" ];
        unitConfig.ConditionPathExists = "!/var/lib/rime/nixos-bridge/finish.json";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${pkgs.rime.rime-bridge}/bin/rime-migrate-finish --record-boot";
        };
      };
    })
    (lib.mkIf (cfg.boot == "esp") {
      fileSystems."/boot" = {
        device = "/dev/disk/by-label/${cfg.espLabel}";
        fsType = "vfat";
        options = [ "umask=0077" ];
      };
      # D3 v1 chain on a fresh install: BLS entries on the ESP for Fedora GRUB's blscfg.
      rime.boot.layout = lib.mkDefault "bls";
    })
  ]);
}
