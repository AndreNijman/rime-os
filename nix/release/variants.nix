# The toplevels a release publishes (docs/nixos/updates-and-release.md §3.1 `variants`, §11.6).
#
# Every variant is the generic installed system (`nixosConfigurations.rime`, D9: one toplevel for plain and LUKS
# machines) with a set of the catalogue's offered FEATURES on. A feature is a prebuilt system part a machine selects
# with `rime install` (steam -> gaming, qemu -> virtualization): the switch file /var/lib/rime/switch/
# rime-feature@<feature>.target.on. The update client installs the variant named by its base (`plain`, update.toml)
# plus every switched-on feature, sorted and joined with "-" (rimed/rime/src/nixos/variant.rs `name`):
#
#   plain                          no feature (A4: what an install ships unless gaming is chosen)
#   plain-gaming                   Steam, Gaming Mode, gamescope, 32-bit graphics
#   plain-virtualization           QEMU/KVM, libvirt, OVMF, swtpm, virtiofsd
#   plain-gaming-virtualization    both
#
# checks.release-plan fails when an offered feature set has no variant here, or a variant's system does not carry
# exactly its features. The shipped profile (nix/hosts/profile.nix) turns gaming ON for rime-vm and the test
# machines, so every variant states both options with mkForce.
#
# `publishable = false` makes `rime-nix-release publish` refuse the variant unless --allow-test-variants is given.
{ self, lib ? self.inputs.nixpkgs.lib }:
let
  catalogue = import ../catalogue/catalogue.nix;
  features = lib.sort (a: b: a < b) (lib.attrNames (lib.filterAttrs (_: f: f.offered) catalogue.features));
  base = "plain";

  # Every subset of the offered features, each sorted.
  subsets = lib.foldr (f: acc: acc ++ map (s: [ f ] ++ s) acc) [ [ ] ] features;
  name = set: lib.concatStringsSep "-" ([ base ] ++ lib.sort (a: b: a < b) set);

  variant = set: {
    system = self.nixosConfigurations.rime.extendModules {
      modules = [
        {
          rime.gaming.enable = lib.mkForce (lib.elem "gaming" set);
          rime.virtualization.enable = lib.mkForce (lib.elem "virtualization" set);
          # The toplevel says which variant it is (nixos-system-rime-plain-gaming-...); the generic system sets no
          # host name, which would name every variant "unnamed".
          system.name = "rime-${name set}";
        }
      ];
    };
    inherit features;
    selected = set;
    publishable = true;
    note = if set == [ ] then "the generic installed system" else "with ${lib.concatStringsSep " and " set}";
  };
in
lib.listToAttrs (map (set: lib.nameValuePair (name set) (variant set)) subsets)
