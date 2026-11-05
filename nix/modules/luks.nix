# LUKS root unlock on Rime's NixOS (PLAN.md D3, D9, D2): the boot path of an encrypted install.
#
# One toplevel serves every encrypted machine (D9's generic `rime-luks`), so the volume is NOT named in the closure:
# the boot hook (nix/pkgs/rime-boot) appends host.toml `[boot] kernel_params = [ "rd.luks.uuid=<uuid>" ]` to every
# menu entry, and systemd-cryptsetup-generator in the systemd initrd opens it as /dev/mapper/luks-<uuid>. The volume
# is named by its LUKS UUID, never by a label (two disks may carry the same label; bootc's installer labels every
# volume RIMEROOT). The passphrase is asked by Plymouth (desktop.nix) or, without it, on the console.
#
# This module only makes the initrd able to do that: dm-crypt and the cipher modules, systemd-cryptsetup and its
# generator, and the cryptsetup targets (nixpkgs' boot.initrd.luks.forceLuksSupportInInitrd, so nothing is written
# into the initrd's /etc/crypttab). `uuid` exists for a machine-specific configuration (and tests); `keymap` is the
# console keymap the passphrase is typed with (the tpmkeymap work builds the keymap gate on it).
#
# The systemd initrd is the only initrd at this nixpkgs pin (nix/probe/ovmf/system.nix), so there is nothing to assert.
# Proven by checks.luks-boot (nix/tests/bootmatrix.nix) and, through Fedora's shim and GRUB, by the OVMF boot matrix
# (nix/probe/ovmf/boothook, docs/nixos/boot-matrix.md).
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.luks;
in
{
  options.rime.luks = {
    enable = lib.mkEnableOption ''
      unlocking a LUKS2 root in the systemd initrd from `rd.luks.uuid=` (normally written per machine by the boot
      hook from host.toml [boot] kernel_params)'';

    uuid = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}");
      default = null;
      example = "4f6c8a1e-2b3d-4e5f-8a9b-0c1d2e3f4a5b";
      description = ''
        The LUKS UUID of the root volume, as `cryptsetup luksUUID` prints it. Set only in a configuration built for
        one machine: it adds `rd.luks.uuid=<uuid>` to the kernel command line of every generation. Generic release
        toplevels leave it null and get the same argument from host.toml through the boot hook.
      '';
    };

    keymap = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "[A-Za-z0-9_.-]+");
      default = null;
      example = "de";
      description = ''
        Console keymap for the passphrase prompt. Passed through to console.keyMap with console.earlySetup, so the
        initrd's vconsole setup loads it before systemd-cryptsetup asks. null: the system's console keymap as is.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      # dm_mod, dm_crypt, the cipher modules, systemd-cryptsetup + its generator and the cryptsetup targets in the
      # initrd, without a crypttab entry (the volume comes from the kernel command line).
      boot.initrd.luks.forceLuksSupportInInitrd = true;

      # The prompt is answered from the built-in keyboard: on a laptop that is i8042/atkbd, on a desktop USB HID,
      # already in nixpkgs' default initrd modules; nothing to add. cryptsetup itself for the running system
      # (luksDump, adding a key slot).
      environment.systemPackages = [ pkgs.cryptsetup ];
    }

    (lib.mkIf (cfg.uuid != null) {
      boot.kernelParams = [ "rd.luks.uuid=${cfg.uuid}" ];
    })

    (lib.mkIf (cfg.keymap != null) {
      console.keyMap = lib.mkDefault cfg.keymap;
      console.earlySetup = true;
    })
  ]);
}
