# Rime's own packages. Reached as pkgs.rime.<name> through the flake's overlays.default.
# One file per package; a worker owns its file and never this set (add a line here only for a NEW package).
{ callPackage, pkgs, rime-shell-src }:

{
  rimed = callPackage ./rimed.nix { };
  rime-shell = callPackage ./rime-shell.nix { src = rime-shell-src; };
  rime-libexec = callPackage ./rime-libexec.nix { };
  # Not callPackage: that would wrap the kernel's own .override, which NixOS's boot.kernelPackages calls
  # (linuxPackagesFor -> kernel.override { features, randstructSeed, ... }).
  rime-kernel = import ./kernel { inherit pkgs; };
  # The config the kernel is built from is the committed nix/pkgs/kernel/config; this regenerates it.
  rime-kernel-config = callPackage ./kernel/config.nix { };
  # Rime's shipped sched-ext schedulers (bootc: Containerfile.core scx-builder).
  rime-scx = callPackage ./kernel/scx.nix { };
}
