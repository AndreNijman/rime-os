# OVMF Secure Boot probe (PLAN.md PR 5, D3 gate P0): the NixOS side.
#
# A minimal NixOS system whose kernel and initrd are booted by Fedora's signed
# shim and GRUB in OVMF. No NixOS boot loader module is used (D3: neither the
# GRUB nor the systemd-boot module; the probe rig writes the menu entries the way
# the future boot.loader.external hook will).
#
# Build (inside the rime-nix container, from this directory):
#   nix build -f system.nix --arg nixpkgs 'builtins.fetchTarball …' kernel initrd toplevel rootfs
# run-all.sh does this with the pinned revision.
{ nixpkgs }:
let
  eval = import (nixpkgs + "/nixos/lib/eval-config.nix") {
    system = "x86_64-linux";
    modules = [ module ];
  };

  module = { config, pkgs, lib, modulesPath, ... }: {
    imports = [
      (modulesPath + "/profiles/minimal.nix")
      (modulesPath + "/profiles/qemu-guest.nix")
    ];

    # The kernel under test. linuxPackages_latest is the mainline kernel the
    # pin carries (Rime's CachyOS kernel is packaged later, P2).
    boot.kernelPackages = pkgs.linuxPackages_latest;

    # D3: no NixOS loader module. The probe's run-all.sh plays the external hook.
    boot.loader.grub.enable = false;
    boot.loader.systemd-boot.enable = false;

    # The initrd is systemd's: at this pin the scripted initrd is gone and
    # boot.initrd.systemd.enable is an error to set.
    boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" "virtio_scsi" "ahci" "sd_mod" "ext4" ];

    fileSystems."/" = { device = "/dev/disk/by-label/nixos-probe"; fsType = "ext4"; };

    networking.hostName = "rime-ovmf-probe";
    networking.useDHCP = false;
    documentation.enable = false;
    services.udisks2.enable = false;

    # Stage-2 marker: proves the system got past the initrd's switch-root into
    # the NixOS system, records what the firmware and shim handed over, then
    # powers off so a run ends by itself.
    systemd.services.rime-ovmf-probe = {
      description = "Rime OVMF probe: stage-2 marker";
      # No After=multi-user.target: the target already orders itself after
      # what it wants, so that would be a cycle systemd breaks by dropping us.
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-user-sessions.service" ];
      path = [ pkgs.coreutils pkgs.util-linux pkgs.mokutil pkgs.gnugrep pkgs.systemd ];
      serviceConfig = { Type = "oneshot"; StandardOutput = "tty"; TTYPath = "/dev/ttyS0"; };
      script = ''
        echo "RIME-OVMF-PROBE: stage-2 reached: $(uname -r) system=$(readlink -f /run/current-system)"
        echo "RIME-OVMF-PROBE: cmdline: $(cat /proc/cmdline)"
        echo "RIME-OVMF-PROBE: mokutil --sb-state: $(mokutil --sb-state 2>&1 | tr '\n' ' ')"
        echo "RIME-OVMF-PROBE: MokListRT certs:"
        mokutil --list-enrolled 2>&1 | grep -E 'Subject:|SHA1 Fingerprint' | sed 's/^/RIME-OVMF-PROBE:   /' || true
        echo "RIME-OVMF-PROBE: kernel keyring lines:"
        dmesg | grep -Ei 'secure ?boot|MokList|integrity: Load|Lockdown' | sed 's/^/RIME-OVMF-PROBE:   /' || true
        echo "RIME-OVMF-PROBE: DONE, powering off"
        systemctl --no-block poweroff
      '';
    };

    boot.kernelParams = [ "console=ttyS0,115200" ];
    users.users.root.initialHashedPassword = "!";
    system.stateVersion = "26.05";
  };

  inherit (eval) config pkgs;
  toplevel = config.system.build.toplevel;
in
{
  inherit toplevel;
  kernel = config.boot.kernelPackages.kernel;
  initrd = config.system.build.initialRamdisk;

  # The root filesystem: ext4, label nixos-probe, holding the toplevel's
  # closure. No VM needed (mkfs.ext4 -d). Stage 2 runs $toplevel/init.
  rootfs = pkgs.callPackage (nixpkgs + "/nixos/lib/make-ext4-fs.nix") {
    storePaths = [ toplevel ];
    volumeLabel = "nixos-probe";
    compressImage = false;
    populateImageCommands = ''
      mkdir -p ./files/etc ./files/boot
    '';
  };

  # What the hook needs to write an entry: init path and the kernel params.
  entry = pkgs.writeText "rime-ovmf-probe-entry.json" (builtins.toJSON {
    init = "${toplevel}/init";
    kernel = "${config.boot.kernelPackages.kernel}/${config.system.boot.loader.kernelFile}";
    initrd = "${config.system.build.initialRamdisk}/${config.system.boot.loader.initrdFile}";
    params = config.boot.kernelParams;
    kernelVersion = config.boot.kernelPackages.kernel.modDirVersion;
    label = config.system.nixos.label;
  });
}
