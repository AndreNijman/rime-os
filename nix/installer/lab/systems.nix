# The installer lab's systems (installer/lab/run.sh; docs/nixos/installer.md): the toplevels the lab ISO installs.
# Each is a release variant (nix/release/variants.nix: `plain`, and `plain-gaming` when gaming is chosen; both built
# on nixosConfigurations.rime, the generic fresh-install system, nix/installer/target.nix) plus only what an
# OVMF VM needs, with the REAL Rime kernel signed the way the release kernel is (a detached signature of
# system.build.rimeKernelImage, attached by nix/pkgs/kernel/sign.nix), here by a THROWAWAY TEST MOK. Built with
# --impure from the rime-os flake:
#
#   image      pass 1: the unsigned kernel image to sign ({ imageHash, target, image })
#   toplevel   pass 2 (signatureDir + certificate given; rime.kernel.signing.require = true, so an unsigned kernel
#              cannot slip in)
#
# Nothing here names a user, a disk, a LUKS volume, a hostname or a timezone: the installer writes all of that
# (useradd, host.toml), exactly as on hardware. Kernel arguments the lab needs on the serial line (console=ttyS0,
# loglevel, plymouth off for the console passphrase prompt) come from host.toml [boot] kernel_params, which the
# lab passes to the installer as RIME_EXTRA_KARGS.
{
  flake,
  signatureDir ? null,
  certificate ? null,
  variant ? "plain",
}:

let
  nixpkgs = flake.inputs.nixpkgs;
  inherit (nixpkgs) lib;
  signed = signatureDir != null;

  lab =
    { lib, modulesPath, ... }:
    {
      imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

      rime.kernel = {
        # No NVIDIA in OVMF; the rest of the module tree is the shipped one.
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

      boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" ];

      # VM-only graphics knobs (as nix/tests/vm-boot.nix): pixman for the greeter's sway, software Qt Quick.
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      # No network in the lab.
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;
    };

  base = (import ../../release/variants.nix { self = flake; }).${variant}.system;
  system = extra: (base.extendModules { modules = [ lab ] ++ extra; }).config;

  c = system [ ];
in
{
  toplevel = c.system.build.toplevel;
  image = {
    inherit (c.system.build.rimeKernelSigning) imageHash target;
    image = c.system.build.rimeKernelImage;
  };
  imageJson = builtins.toJSON {
    inherit (c.system.build.rimeKernelSigning) imageHash target;
    image = "${c.system.build.rimeKernelImage}";
  };
}
