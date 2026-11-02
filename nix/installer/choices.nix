# The systems the NixOS installer lets an owner choose, and the disk floor of each: what build-live-iso.sh stamps
# into the ISO as nixos-choices.json (NIXOS_CHOICES) and rime-install-nixos.sh reads (nixos_choose). Pure data.
#
# A4: an install ships `plain` unless the owner chooses gaming, which installs `plain-gaming` (nix/release/
# variants.nix names) and switches the gaming feature on the way `rime install steam` does, so every later
# `rime update` keeps selecting that same variant. Virtualization is not offered at install (A4): `rime install qemu`.
#
# The floors are nix/release/floors.nix's, the ONE place `rime update`'s space gate (update.toml on_disk_ratio) and
# `rime install steam` (catalogue.json floors) read them from: `disk_gb` for a whole disk, `partition_gb` for the
# root partition alone (partition mode, where the ESP already exists). Nothing in the installer states a number.
#
#   nix eval --json --file nix/installer/choices.nix > choices.json      (what NIXOS_CHOICES names)
#
# checks.installer-choices holds this to floors.nix's published figures and to the shipped catalogue's switch path.
let
  floors = import ../release/floors.nix;
  catalogue = import ../catalogue/catalogue.nix;
  # nix/modules/packages-catalogue.nix: switchDir and renderFeature's `switch` (checks.installer-choices compares).
  switch = feature: "/var/lib/rime/switch/rime-feature@${feature}.target.on";
  variant = name: features: {
    disk_gb = floors.floors.${name}.diskGB;
    partition_gb = floors.floors.${name}.partitionGB;
    inherit features;
  };
in
{
  schema = 1;
  variants = {
    plain = variant "plain" [ ];
    plain-gaming = variant "plain-gaming" [ "gaming" ];
  };
  features.gaming = {
    inherit (catalogue.features.gaming) title summary;
    switch = switch "gaming";
    # The requesting name the switch file holds, as `rime install steam` writes it.
    names = [ "steam" ];
  };
}
