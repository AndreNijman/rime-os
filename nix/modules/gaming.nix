# Gaming: Steam, gamescope Gaming Mode, NTSYNC, scx schedulers, game environment (PLAN.md 4.6, 4.8).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.gaming;
in
{
  options.rime.gaming = {
    enable = lib.mkEnableOption "Rime's gaming stack";
  };

  config = lib.mkIf cfg.enable { };
}
