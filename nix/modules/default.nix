# nixosModules.default: every Rime module plus the overlay that provides pkgs.rime.*.
# This is the entry point; the per-area modules in the flake (nixosModules.base, ...) expect it.
{ overlay }:

{
  imports = [
    ./base.nix
    ./desktop.nix
    ./services.nix
    ./kernel.nix
    ./gaming.nix
    ./security.nix
    ./packages-catalogue.nix
  ];

  nixpkgs.overlays = [ overlay ];
}
