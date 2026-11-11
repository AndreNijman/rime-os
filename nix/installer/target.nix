# The filesystems of a FRESH Rime OS NixOS install (PLAN.md D9 + D2), as installer/rime-install-nixos.sh lays the
# disk out:
#
#   GPT  1  EFI System Partition  FAT32, label RIME-ESP, mounted at /boot. Rime's own ESP, never another OS's (D3
#                                 ESP rules): Fedora's shim + GRUB (bootupd's static config) live here, and the
#                                 boot hook (rime.boot, layout bls) writes the BLS entries, kernels and initrds.
#        2  root                  btrfs, label rime-root, either directly (plain) or inside a LUKS2 volume (the
#                                 boot hook adds rd.luks.uuid= from host.toml; nix/modules/luks.nix opens it).
#
# One generic toplevel serves plain and LUKS machines (D9 named two, rime-plain and rime-luks; the only difference
# would be the LUKS initrd support, which costs nothing when no rd.luks.uuid= is given, so there is one: `rime`).
# Everything per machine (LUKS UUID, keymap, hostname, timezone, locale) comes from /var/lib/rime/host.toml.
#
# The mounts themselves are rime.rootfs's (nix/modules/rootfs.nix), the ONE option set a fresh and a migrated machine
# share; a fresh install differs from a moved machine only in host.toml ([boot] layout = "bls", which the installer
# writes and rime.rootfs's /boot generator reads at boot). This file keeps the installer's names working:
#   rime.installTarget.enable     = rime.rootfs.enable
#   rime.installTarget.rootLabel  is rime.rootfs.label     (an alias: setting either sets both)
#   rime.installTarget.espLabel   is rime.rootfs.espLabel  (an alias)
{ config, lib, ... }:

{
  imports = [
    (lib.mkAliasOptionModule [ "rime" "installTarget" "rootLabel" ] [ "rime" "rootfs" "label" ])
    (lib.mkAliasOptionModule [ "rime" "installTarget" "espLabel" ] [ "rime" "rootfs" "espLabel" ])
  ];

  options.rime.installTarget.enable = lib.mkEnableOption "the fresh-install disk layout the Rime installer writes (D9)";

  config = lib.mkIf config.rime.installTarget.enable {
    rime.rootfs.enable = true;
  };
}
