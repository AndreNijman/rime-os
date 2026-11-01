# Security: firewall zones, sudo rules, PAM/keyring, sshd keys only, Secure Boot hooks (PLAN.md 4.7, D3, D4).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.security;
in
{
  options.rime.security = {
    enable = lib.mkEnableOption "Rime's security policy";
  };

  config = lib.mkIf cfg.enable { };
}
