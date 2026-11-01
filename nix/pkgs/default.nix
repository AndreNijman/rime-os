# Rime's own packages. Reached as pkgs.rime.<name> through the flake's overlays.default.
# One file per package; a worker owns its file and never this set (add a line here only for a NEW package).
{ callPackage, linuxPackages_latest, rime-shell-src }:

{
  rimed = callPackage ./rimed.nix { };
  rime-shell = callPackage ./rime-shell.nix { src = rime-shell-src; };
  rime-libexec = callPackage ./rime-libexec.nix { };
  # Not callPackage: that would wrap the kernel's own .override, which NixOS's boot.kernelPackages calls
  # (linuxPackagesFor -> kernel.override { features, randstructSeed, ... }).
  rime-kernel = import ./kernel { inherit linuxPackages_latest; };
}
