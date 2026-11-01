# The manifest's `components` (§3.1): live-engine component slugs (rimed_core::live::Component::slug) mapped to the
# store paths that make up each, taken from the system's own configuration rather than guessed from store names.
#
# Deviation from the design (docs §3.1): the design has CI run the client's Rust classifier. That classifier works on
# /usr file paths and has no NixOS form yet (D7). Until it has, the paths come from the NixOS options that select
# them; `rime-nix-release manifest` drops any that are not in the variant's closure. TODO(handover): D7 replaces
# this with the shared classifier and the client and CI then agree by construction.
{ lib }:
config: pkgs:
let
  when = cond: xs: if cond then xs else [ ];
  desktop = config.rime.desktop.enable or false;
  nvidia = lib.elem "nvidia" (config.services.xserver.videoDrivers or [ ]);
in
lib.filterAttrs (_: v: v != [ ]) {
  shell = when desktop [ pkgs.rime.rime-shell ];
  shell-runtime = when desktop [ pkgs.quickshell ];
  compositor = when config.programs.hyprland.enable [ config.programs.hyprland.package ];
  rime-daemon = when (config.rime.services.enable or false) [ config.rime.services.rimed.package ];
  rime-tools = [ pkgs.rime.rime-libexec ];
  systemd = [ config.systemd.package ];
  libraries = [ pkgs.stdenv.cc.libc ];
  kernel = [ config.boot.kernelPackages.kernel ];
  nvidia = when nvidia [ config.hardware.nvidia.package ];
  firmware-files = [ config.hardware.firmware ]; # one buildEnv: the option applies pkgs.buildEnv to the list
  etc-defaults = [ config.system.build.etc ];
}
