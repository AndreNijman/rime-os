# The "previous release" side of a delta pair (docs/nixos/delta.md): rime-vm, unchanged, evaluated against an OLDER
# nixpkgs revision, as if Rime had released it then. Measurement only; nothing ships this.
#
#   nix build --impure --expr 'import ./nix/release/delta/pair.nix { rimeFlake = "path:/work/wt-g2"; nixpkgsRev = "<rev>"; }'
#
# Rime's modules target nixpkgs 1c89083. On an older nixpkgs a few options do not exist yet and one assertion
# (Hyprland >= 0.57) fails. `compat` declares the missing options as inert sinks and drops the assertions, so the
# old system keeps the packages that revision really had (Hyprland 0.56.2 stays 0.56.2: nothing from the newer
# nixpkgs is overlaid, which would make the pair look closer than it is). Each sink costs at most a few /etc lines.
{ rimeFlake, nixpkgsRev, host ? "vm" }:
let
  self = builtins.getFlake rimeFlake;
  old = builtins.getFlake "github:NixOS/nixpkgs/${nixpkgsRev}";
  # Whether the old nixpkgs's module declares `name` (a plain text test: declaring an option twice is an error, and
  # asking `options` from inside a module that declares options recurses).
  hasOption = file: name: old.lib.hasInfix "${name} = lib.mkOption" (builtins.readFile "${old}/nixos/modules/${file}");
  compat =
    { lib, options, ... }:
    let
      sink = lib.mkOption {
        type = lib.types.anything;
        default = { };
        description = "delta pair compat sink (option newer than this nixpkgs)";
      };
    in
    {
      # services.avahi.settings arrived between 55bfb4d (10-14) and 3ec6363 (10-24).
      options.services.avahi = lib.optionalAttrs (!hasOption "services/networking/avahi-daemon.nix" "settings") {
        settings = sink;
      };
      config.assertions = lib.mkForce [ ];
      # Packages renamed or added after this revision: use what the old revision had under its old name.
      config.nixpkgs.overlays = [
        (final: prev: {
          hyprland-guiutils = prev.hyprland-guiutils or prev.hyprland-qtutils;
        })
      ];
    };
in
(old.lib.nixosSystem {
  modules = [
    self.nixosModules.default
    (self.outPath + "/nix/hosts/${host}.nix")
    compat
  ];
}).config.system.build.toplevel
