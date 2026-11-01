# Rime services: rimed and its helper daemons, libexec scripts, update/live engine, timers (PLAN.md 4.3, 4.4).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.services;
in
{
  options.rime.services = {
    enable = lib.mkEnableOption "Rime's system services (rimed and helpers)";
  };

  config = lib.mkIf cfg.enable { };
}
