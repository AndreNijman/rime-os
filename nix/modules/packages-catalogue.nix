# Packages catalogue: what `rime install <name>` offers and what every install ships (PLAN.md 4.5, D5).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.catalogue;
in
{
  options.rime.catalogue = {
    enable = lib.mkEnableOption "Rime's package catalogue";
  };

  config = lib.mkIf cfg.enable { };
}
