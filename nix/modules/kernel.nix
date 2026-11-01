# Kernel and GPU: Rime's kernel (pkgs.rime.rime-kernel), NVIDIA, msi-ec, sysctls (PLAN.md 4.6, D7).
# Skeleton: off by default so VMs and tests boot nixpkgs' default (cached) kernel.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.kernel;
in
{
  options.rime.kernel = {
    enable = lib.mkEnableOption "Rime's own kernel instead of nixpkgs' default";
  };

  config = lib.mkIf cfg.enable {
    boot.kernelPackages = pkgs.linuxPackagesFor pkgs.rime.rime-kernel;
  };
}
