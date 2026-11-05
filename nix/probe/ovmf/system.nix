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
#
# KERNEL=rime (kernsign, PLAN.md D3 + D7): with `rimeFlake` (the rime-os flake, builtins.getFlake), the same probe
# system boots Rime's REAL kernel through nixosModules.default + rime.kernel (CachyOS 7.2.9, every module's
# kmodigest certificate inserted), on the flake's nixpkgs. `signatureDir` holds `<image hash>.sig` made by
# nix/pkgs/kernel/sign-detached.sh with the rig's throwaway key, and `certificate` is that key's certificate:
# rime.kernel.signing attaches the signature and verifies it in the build (nix/pkgs/kernel/sign.nix), exactly the
# path a release takes with the Rime MOK. Only the signature and the public certificate enter the store.
{
  nixpkgs ? rimeFlake.inputs.nixpkgs,
  rimeFlake ? null,
  signatureDir ? null,
  certificate ? null,
}:
let
  rime = rimeFlake != null;
  eval = import (nixpkgs + "/nixos/lib/eval-config.nix") {
    system = "x86_64-linux";
    modules = [ module ] ++ (if rime then [ rimeFlake.nixosModules.default rimeModule ] else [ ]);
  };

  rimeModule = { lib, ... }: {
    rime.kernel.enable = true;
    rime.kernel.signing = {
      # null = no signature yet (the first pass, which only builds the unsigned image to sign).
      inherit signatureDir;
    } // lib.optionalAttrs (certificate != null) { inherit certificate; };
  };

  module = { config, pkgs, lib, modulesPath, ... }: {
    imports = [
      (modulesPath + "/profiles/minimal.nix")
      (modulesPath + "/profiles/qemu-guest.nix")
    ];

    # The kernel under test. linuxPackages_latest is the mainline kernel the
    # pin carries; with rimeFlake, rime.kernel sets Rime's own (KERNEL=rime).
    boot.kernelPackages = lib.mkIf (!rime) pkgs.linuxPackages_latest;

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
      # KERNEL=rime also proves multi-user.target is reached. A oneshot would hold the target back (targets
      # order themselves after what they want), so that variant runs as a simple service and waits for it.
      serviceConfig = { Type = if rime then "simple" else "oneshot"; StandardOutput = "tty"; TTYPath = "/dev/ttyS0"; };
      script = lib.optionalString rime ''
        for _ in $(seq 1 240); do
          systemctl is-active --quiet multi-user.target && break
          sleep 0.5
        done
        # A serial getty's vhangup() kills the ttyS0 descriptor systemd handed over: open it again.
        exec > /dev/ttyS0 2>&1
        echo "RIME-OVMF-PROBE: multi-user.target $(systemctl is-active multi-user.target)"
        echo "RIME-OVMF-PROBE: kernel image: $(readlink -f /run/current-system/kernel)"
      '' + ''
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

    # The KERNEL=rime marker prints after multi-user.target, when the serial getty already owns ttyS0 (its
    # vhangup() silenced the first run's marker): no getty on the serial console there.
    systemd.services."serial-getty@ttyS0" = lib.mkIf rime { enable = false; };

    boot.kernelParams = [ "console=ttyS0,115200" ];
    users.users.root.initialHashedPassword = "!";
    system.stateVersion = "26.05";
  };

  inherit (eval) config pkgs;
  toplevel = config.system.build.toplevel;
  # The image the toplevel boots: nixpkgs' kernel package, or rime.kernel's (signed when signatureDir has one).
  bootKernel = if rime then config.system.build.rimeKernelBootImage else config.boot.kernelPackages.kernel;
in
{
  inherit toplevel;
  kernel = bootKernel;
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
    kernel = "${bootKernel}/${config.system.boot.loader.kernelFile}";
    initrd = "${config.system.build.initialRamdisk}/${config.system.boot.loader.initrdFile}";
    params = config.boot.kernelParams;
    kernelVersion = config.boot.kernelPackages.kernel.modDirVersion;
    label = config.system.nixos.label;
  });
}
// (if rime then {
  # KERNEL=rime, first pass: the unsigned image to sign, and its hash (the .sig file name).
  unsignedImage = config.system.build.rimeKernelImage;
  signing = pkgs.writeText "rime-ovmf-probe-signing.json" (builtins.toJSON config.system.build.rimeKernelSigning);
} else { })
