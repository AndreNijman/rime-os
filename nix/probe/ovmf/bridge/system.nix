# The bridge OVMF lab's NixOS system (README.md here): what `rime migrate start` installs into the VM's bootc disk.
# Built with --impure from the rime-os flake (run.sh does both passes):
#
#   imageJson  pass 1: the unsigned Rime kernel image to sign ({ imageHash, target, image })
#   toplevel   pass 2 (signatureDir + certificate given): the same system, its kernel carrying the detached MOK
#              signature (rime.kernel.signing, require = true: an unsigned kernel cannot slip in), so the NixOS entry
#              boots with Secure Boot ON through shim + Fedora's GRUB, exactly as a release kernel does. Here the MOK
#              is a THROWAWAY TEST key (run.sh keys), enrolled in the lab varstore next to the real Rime MOK
#              certificate that signs the published bootc kernel.
#
# The product modules (desktop with the greeter, services, security, hostlayer, the boot hook) plus rime.rootfs
# (the @nixos/@nix/@home layout the bridge makes, /boot from host.toml) and the Rime kernel (no NVIDIA in OVMF).
# Lab-only knobs, none of them product behaviour: the serial console the driver reads, Plymouth off so the LUKS
# passphrase prompt is on that console (the bootc disk's own kernel arguments do the same), and the VM graphics
# knobs every desktop VM test uses (pixman for the greeter's compositor, software Qt Quick).
{
  flake,
  signatureDir ? null,
  certificate ? null,
}:

let
  inherit (flake.inputs.nixpkgs) lib;
  signed = signatureDir != null;
  c =
    (lib.nixosSystem {
      modules = [
        flake.nixosModules.default
        (
          { modulesPath, ... }:
          {
            imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];
            nixpkgs.hostPlatform = "x86_64-linux";
            rime = {
              desktop.enable = true;
              services.enable = true;
              security.enable = true;
              boot.enable = true;
              rootfs.enable = true;
              kernel = {
                enable = true;
                nvidia.enable = false;
                signing =
                  if signed then
                    {
                      inherit signatureDir certificate;
                      require = true;
                    }
                  else
                    { imageSignature = lib.mkForce null; };
              };
            };
            boot.kernelParams = [
              "console=tty0"
              "console=ttyS0,115200"
              "plymouth.enable=0"
              "rd.plymouth=0"
            ];
            boot.initrd.availableKernelModules = [
              "virtio_pci"
              "virtio_blk"
              "ahci"
              "sd_mod"
            ];
            environment.sessionVariables = {
              WLR_RENDERER = "pixman";
              QT_QUICK_BACKEND = "software";
            };
            # The passphrase prompt is the real one (systemd-cryptsetup on the console); this initrd unit only TELLS
            # the driver on ttyS0 when it is asking, so it knows when to type (the boot matrix lab does the same).
            boot.initrd.systemd.services.rime-lab-ask-watch = {
              description = "Rime bridge lab: say on ttyS0 when a passphrase is asked";
              wantedBy = [ "initrd.target" ];
              unitConfig.DefaultDependencies = false;
              serviceConfig = {
                Type = "simple";
                ExecStart = "/bin/sh -c 'until ls /run/systemd/ask-password/ask.* >/dev/null 2>&1; do sleep 0.2; done; echo \"RIME-LAB: PASSPHRASE-ASKED $(cat /proc/uptime)\" > /dev/ttyS0'";
              };
            };
            # No network in the lab.
            systemd.services.rime-flathub-setup.enable = lib.mkForce false;
            system.stateVersion = "26.05";
            documentation.enable = false;
          }
        )
      ];
    }).config;
in
{
  toplevel = c.system.build.toplevel;
  imageJson = builtins.toJSON {
    inherit (c.system.build.rimeKernelSigning) imageHash target;
    image = "${c.system.build.rimeKernelImage}";
  };
}
