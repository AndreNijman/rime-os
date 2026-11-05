# The boot-hook OVMF lab's NixOS generations (README.md here). Built with --impure from the rime-os flake:
#   nix build --impure --expr '(import ./systems.nix { flake = builtins.getFlake "path:/work/wt-boothook";
#                                signature = /path/to/kernel.sig; certificate = /path/to/test-mok.crt; }).<layout>.<gen>'
#
# For each layout (bls, custom) three generations of one minimal NixOS with Rime's boot hook (nix/modules/boot.nix):
#   gen1  good: stage 2 prints RIME-BOOTHOOK markers, then does what fw_cfg opt/rime/action says
#         (install:<toplevel> = nix-env --set + <toplevel>/bin/switch-to-configuration boot, run INSIDE the VM),
#         then powers off;
#   gen2  BAD: its root filesystem label does not exist, so the initrd times out on sysroot and falls into
#         emergency mode; an initrd unit prints "RIME-BOOTHOOK: BAD generation 2 ..." there and powers off;
#   gen3  good again (gen1 with a different /etc/rime-lab-generation).
# fw_cfg rollback:<N> = nix-env --switch-generation N + the profile's switch-to-configuration boot (D6 rollback).
# Every generation's <toplevel>/kernel is nixpkgs' linuxPackages_latest bzImage with a detached TEST-MOK signature
# attached (sbattach), linked the way nix/modules/kernel.nix links the Rime kernel once CI has signed it. bootspec
# still names the unsigned image: the hook must copy <toplevel>/kernel, or Secure Boot refuses the boot.
{ flake, signature ? null, certificate ? null }:

let
  nixpkgs = flake.inputs.nixpkgs;
  inherit (nixpkgs) lib;

  common = generation: { config, pkgs, modulesPath, ... }:
    let
      kernel = config.boot.kernelPackages.kernel;
      signedImage = pkgs.runCommand "linux-${kernel.version}-bzImage-TEST-MOK-signed"
        { nativeBuildInputs = [ pkgs.sbsigntool ]; } ''
          cp ${kernel}/${config.system.boot.loader.kernelFile} $out
          chmod u+w $out
          sbattach --attach ${signature} $out
          sbverify --list $out
        '';
    in
    {
      imports = [
        (modulesPath + "/profiles/minimal.nix")
        (modulesPath + "/profiles/qemu-guest.nix")
        (flake.outPath + "/nix/modules/boot.nix")
      ];
      nixpkgs.hostPlatform = "x86_64-linux";
      nixpkgs.overlays = [ flake.overlays.default ];

      rime.boot.enable = true;
      # The TEST MOK's certificate (run.sh prep): the hook places it on the ESP with the Secure Boot recovery entry,
      # as a Rime machine gets rime.kernel.signing.certificate (drive.py sbrecovery enrols it from that entry).
      rime.boot.mokCertificate = lib.mkIf (certificate != null) certificate;

      # The same kernel the OVMF probe proved (nixpkgs' mainline at the A1 pin).
      boot.kernelPackages = pkgs.linuxPackages_latest;
      boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" "ahci" "sd_mod" ];
      boot.kernelModules = [ "qemu_fw_cfg" ];
      boot.kernelParams = [ "console=ttyS0,115200" ];

      system.systemBuilderCommands = lib.mkIf (signature != null) (lib.mkAfter ''
        ln -sf -- ${signedImage} $out/kernel
      '');

      networking.hostName = "rime-boothook-lab";
      networking.useDHCP = false;
      documentation.enable = false;
      services.udisks2.enable = false;
      users.users.root.initialHashedPassword = "!";
      system.stateVersion = "26.05";
      environment.etc."rime-lab-generation".text = "${toString generation}\n";
    };

  # Stage-2 driver of good generations: markers on ttyS0, then the action QEMU passed in fw_cfg, then power off.
  driver = { config, pkgs, ... }: {
    systemd.services.rime-boothook-lab = {
      description = "Rime boot-hook lab: stage-2 marker and driver";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-user-sessions.service" ];
      path = [ config.nix.package pkgs.coreutils pkgs.util-linux pkgs.mokutil pkgs.kmod pkgs.gnugrep pkgs.gnused
        pkgs.findutils pkgs.systemd ];
      serviceConfig = { Type = "oneshot"; StandardOutput = "tty"; StandardError = "tty"; TTYPath = "/dev/ttyS0"; };
      script = ''
        say() { echo "RIME-BOOTHOOK: $*"; }
        say "stage-2 reached: generation=$(cat /etc/rime-lab-generation) system=$(readlink -f /run/current-system)"
        say "cmdline: $(cat /proc/cmdline)"
        say "sb-state: $(mokutil --sb-state 2>&1 | tr '\n' ' ')"
        say "profile: $(readlink /nix/var/nix/profiles/system) -> $(readlink -f /nix/var/nix/profiles/system)"
        say "boot mount: $(findmnt -no SOURCE,FSROOT,FSTYPE /boot | tr '\n' ' ')"
        dmesg | grep -E "Loaded X.509 cert 'Rime boot-hook lab TEST MOK" | sed 's/^/RIME-BOOTHOOK: /' || true
        if [ -L /boot/loader ]; then
          say "ostree: loader -> $(readlink /boot/loader); $(sha256sum /boot/loader.1/entries/ostree-1.conf)"
          say "custom.cfg windows entry: $(grep -c "menuentry 'Windows (lab)'" /boot/grub2/custom.cfg)"
        fi
        modprobe qemu_fw_cfg || true
        A=/sys/firmware/qemu_fw_cfg/by_name/opt/rime/action/raw
        action=none
        [ -r "$A" ] && action=$(cat "$A")
        say "action: $action"
        case "$action" in
          install:/nix/store/*)
            t=''${action#install:}
            rc=0
            out=$( { nix-env -p /nix/var/nix/profiles/system --set "$t" && "$t/bin/switch-to-configuration" boot; } 2>&1 ) || rc=$?
            printf '%s\n' "$out" | sed 's/^/RIME-BOOTHOOK: hook| /'
            say "install rc=$rc profile now $(readlink /nix/var/nix/profiles/system)"
            ;;
          rollback:[0-9]*)
            # D6 rollback: the profile's current generation moves back, then the boot entry is rewritten the way
            # nixos-rebuild --rollback boot does it (the profile's own switch-to-configuration boot).
            rc=0
            out=$( { nix-env -p /nix/var/nix/profiles/system --switch-generation "''${action#rollback:}" \
                     && /nix/var/nix/profiles/system/bin/switch-to-configuration boot; } 2>&1 ) || rc=$?
            printf '%s\n' "$out" | sed 's/^/RIME-BOOTHOOK: hook| /'
            say "rollback rc=$rc profile now $(readlink /nix/var/nix/profiles/system)"
            ;;
        esac
        say "rime-boot check: $(/run/current-system/sw/bin/rime-boot check 2>&1 | tr -s ' \n' ' ')"
        if [ -d /boot/loader/entries ] && [ ! -L /boot/loader ]; then say "entries: $(ls /boot/loader/entries | tr '\n' ' ')"; fi
        grep -E '^(menuentry|set default|set timeout)' /boot/grub2/rime-nixos.cfg | sed 's/^/RIME-BOOTHOOK: menu| /'
        say "files: $(ls /boot/rime/nixos | tr '\n' ' ')"
        sync
        say "DONE, powering off"
        systemctl --no-block poweroff
      '';
    };
  };

  # The bad generation: no stage 2. The initrd waits 15 s for a root that does not exist, emergency.target follows,
  # and this unit prints the verdict and powers off (the root account is locked, so emergency mode has no shell).
  bad = { lib, ... }: {
    fileSystems."/".device = lib.mkForce "/dev/disk/by-label/rime-lab-missing";
    fileSystems."/".options = [ "x-systemd.device-timeout=15s" ];
    boot.initrd.systemd.services.rime-boothook-lab-bad = {
      description = "Rime boot-hook lab: the bad generation reached emergency mode";
      wantedBy = [ "emergency.target" ];
      unitConfig.DefaultDependencies = false;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "/bin/sh -c 'echo \"RIME-BOOTHOOK: BAD generation 2 reached initrd emergency mode (root by-label/rime-lab-missing)\" > /dev/ttyS0'";
        ExecStartPost = "/bin/systemctl --no-block poweroff";
      };
    };
  };

  layouts = {
    # Fresh install (D9): the FAT ESP is /boot, root ext4.
    bls = {
      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-root"; fsType = "ext4"; };
      fileSystems."/boot" = { device = "/dev/disk/by-label/RIMEESP"; fsType = "vfat"; options = [ "umask=0077" ]; };
    };
    # Migrated bootc machine (D2, topology E shape): one btrfs, NixOS root in @nixos, bootc's /boot a directory at
    # the top level, bound to /boot.
    custom = {
      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-btrfs"; fsType = "btrfs"; options = [ "subvol=@nixos" ]; };
      fileSystems."/mnt/rime-top" = { device = "/dev/disk/by-label/rime-lab-btrfs"; fsType = "btrfs"; options = [ "subvolid=5" ]; };
      fileSystems."/boot" = { device = "/mnt/rime-top/boot"; fsType = "none"; options = [ "bind" ]; depends = [ "/mnt/rime-top" ]; };
    };
  };

  system = layout: generation: extra:
    (lib.nixosSystem { modules = [ (common generation) layouts.${layout} ] ++ extra; }).config.system.build;

  gens = layout: {
    gen1 = (system layout 1 [ driver ]).toplevel;
    gen2 = (system layout 2 [ bad ]).toplevel;
    gen3 = (system layout 3 [ driver ]).toplevel;
  };
in
{
  bls = gens "bls";
  custom = gens "custom";
  unsignedKernel = (system "bls" 1 [ ]).kernel;
}
