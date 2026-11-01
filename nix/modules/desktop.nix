# Desktop: greetd, Hyprland/niri/labwc sessions, Rime Shell, portals, fonts, audio (PLAN.md 4.8).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.desktop;
in
{
  options.rime.desktop = {
    enable = lib.mkEnableOption "Rime's desktop (sessions, Shell, portals)";
  };

  config = lib.mkIf cfg.enable { };
}
