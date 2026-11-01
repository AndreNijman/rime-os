# The bridge OVMF lab's NixOS system (README.md here): what `rime migrate start` installs into the VM's bootc disk.
# Built with --impure from the rime-os flake:
#   nix build --impure --expr '(import ./system.nix { flake = builtins.getFlake "path:/work/wt-bridge"; })'
#
# The product modules (desktop with the greeter, services, security, hostlayer, the boot hook) plus rime.rootfs
# (the @nixos/@nix/@home layout the bridge makes), on nixpkgs' kernel as rime-vm does (Secure Boot is off in this
# lab, so the kernel need not carry the MOK signature). One lab-only unit prints markers on ttyS0 once the greeter
# is up: the booted system, the mounts, machine-id and the home checksum the driver compares with the old side.
{ flake }:

let
  inherit (flake.inputs.nixpkgs) lib;
in
(lib.nixosSystem {
  modules = [
    flake.nixosModules.default
    ({ modulesPath, pkgs, ... }: {
      imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];
      nixpkgs.hostPlatform = "x86_64-linux";
      rime = {
        desktop.enable = true;
        services.enable = true;
        security.enable = true;
        boot.enable = true;
        rootfs.enable = true;
      };
      boot.kernelParams = [ "console=tty0" "console=ttyS0,115200" ];
      boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" "ahci" "sd_mod" ];
      system.stateVersion = "26.05";
      documentation.enable = false;

      systemd.services.rime-bridge-lab = {
        description = "Rime bridge lab: markers on ttyS0 once the greeter runs";
        wantedBy = [ "multi-user.target" ];
        after = [ "greetd.service" "systemd-user-sessions.service" ];
        path = with pkgs; [ coreutils util-linux findutils gnugrep systemd procps ];
        serviceConfig = { Type = "oneshot"; StandardOutput = "tty"; StandardError = "tty"; TTYPath = "/dev/ttyS0"; };
        script = ''
          say() { echo "RIME-BRIDGE: $*"; }
          for _ in $(seq 60); do systemctl is-active -q greetd.service && break; sleep 1; done
          sleep 5
          say "nixos booted system=$(readlink -f /run/current-system)"
          say "greetd=$(systemctl is-active greetd.service) greeter=$(pgrep -a -u greeter | head -3 | tr '\n' ' ')"
          say "mounts: $(findmnt -no TARGET,SOURCE,FSROOT,OPTIONS / /nix /var/home /boot | tr '\n' ';')"
          say "machine-id=$(cat /etc/machine-id) hostname=$(hostname)"
          say "carry: $(head -c 300 /var/lib/rime/carry/report.txt 2>/dev/null | tr '\n' ' ')"
          for h in /var/home/*; do
            [ -f "$h/.rime-lab/sums" ] || continue
            ( cd "$h" && sha256sum -c --quiet .rime-lab/sums >/dev/null 2>&1 ) && r=ok || r=FAIL
            say "home $h checksums=$r owner=$(stat -c %U "$h")"
          done
          say "READY"
        '';
      };
    })
  ];
}).config.system.build.toplevel
