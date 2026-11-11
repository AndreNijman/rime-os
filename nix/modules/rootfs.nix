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
# Where /boot comes from is a fact about the MACHINE, never the build: one published toplevel (D9) boots a machine
# moved from bootc and a fresh install alike, and updates it later (a build-time choice once shipped the fresh-install
# /boot to moved machines, which then stopped in emergency mode waiting for an ESP they do not have). /boot is NOT in
# fileSystems: the generator rime-rootfs-boot (rootfs/boot-generator.py) is its only owner and writes boot.mount at
# every boot from /var/lib/rime/host.toml [boot]:
#
#   layout = "bls"   a FRESH install (installer/rime-install-nixos.sh writes it): Rime's own ESP (FAT, label
#                    `espLabel`) at /boot; Fedora's shim + GRUB (bootupd's static config) live there and the boot hook
#                    writes BLS entries for GRUB's blscfg.
#   anything else    a machine MOVED from bootc (`rime migrate start` writes layout = "custom-cfg"): bootc left /boot
#                    either as a directory of the btrfs top level (the plain layout) or as its own ext4 partition (the
#                    installer's LUKS layout, installer/rime-install), recorded as [boot] device + fstype. The
#                    partition, or a bind of `topMount`/boot (the top level, subvolid=5, is mounted at `topMount` on
#                    every machine). The Rime boot hook (rime.boot, layout custom-cfg) writes its menu and kernels
#                    there and bootc's GRUB reads them; ostree's entries stay, so the old system still boots
#                    (emergency only, D2).
#
# `rime migrate finish` (nix/pkgs/rime-bridge/rime-migrate-finish.py, docs/nixos/bridge.md) removes the old side once
# NixOS has booted well three times: every machine has the helper on PATH (the `rime` CLI execs it by its
# /run/current-system/sw path; on a fresh install it refuses, there is no old side) and rime-migrate-good-boot.service,
# which runs only on a moved machine (`topMount`/ostree, the old side, is there) and counts every boot that reaches
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
      --replace-fail '@topMount@' '${cfg.topMount}' \
      --replace-fail '@espLabel@' '${cfg.espLabel}'
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
    espLabel = lib.mkOption {
      type = lib.types.str;
      default = "RIME-ESP";
      description = "FAT label of Rime's own EFI System Partition, mounted at /boot when host.toml [boot] layout = \"bls\".";
    };
    topMount = lib.mkOption {
      type = lib.types.str;
      default = "/mnt/rime-top";
      description = "Where the btrfs top level (subvolid=5) is mounted when /boot is bound from it (a moved machine's plain layout).";
    };
    bootGenerator = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = bootGenerator;
      description = "The rime-rootfs-boot generator this configuration installs (for the checks).";
    };
  };

  imports = [
    (lib.mkRemovedOptionModule [ "rime" "rootfs" "boot" ] ''
      Where /boot is comes from /var/lib/rime/host.toml [boot] at every boot (nix/modules/rootfs.nix), never from the
      build: one toplevel serves a moved machine and a fresh install.
    '')
  ];

  config = lib.mkIf cfg.enable {
    fileSystems = {
      "/" = sub "@nixos";
      "/nix" = sub "@nix" // { neededForBoot = true; };
      "/var/home" = sub "@home";
      # The top level: a moved machine's plain layout binds /boot from it, and finish reads the old side there.
      ${cfg.topMount} = { device = dev; fsType = "btrfs"; options = [ "subvolid=5" ]; };
    };
    boot.initrd.supportedFilesystems.btrfs = true;
    systemd.tmpfiles.rules = [ "L /home - - - - /var/home" ];
    # One toplevel opens a LUKS2 root when the command line names one (host.toml [boot] kernel_params).
    rime.luks.enable = lib.mkDefault true;
    systemd.generators.rime-rootfs-boot = "${bootGenerator}";
    # /boot is FAT (a fresh install's ESP) or ext4 (a moved machine's LUKS layout): both fscks and modules.
    boot.supportedFilesystems = { vfat = true; ext4 = true; };
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
      unitConfig = {
        # A moved machine only: its old side (ostree's repo and deployments) is in the top level until finish
        # removes it; a fresh install has none.
        RequiresMountsFor = cfg.topMount;
        ConditionPathExists = [ "${cfg.topMount}/ostree" "!/var/lib/rime/nixos-bridge/finish.json" ];
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.rime.rime-bridge}/bin/rime-migrate-finish --record-boot";
      };
    };
  };
}
