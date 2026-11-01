# Base system: Nix settings, networking, locale and user defaults every Rime machine has.
{ config, lib, ... }:

let
  cfg = config.rime.base;
in
{
  options.rime.base = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Rime's base system settings.";
    };
  };

  config = lib.mkIf cfg.enable {
    # First release Rime installs on. Never bump it for an existing machine.
    # Below mkDefault so the NixOS test framework and hosts can set their own without a conflict.
    system.stateVersion = lib.mkOverride 1200 "26.11";

    networking.hostName = lib.mkOverride 1200 "rime";
    networking.networkmanager.enable = true;

    # null = the user sets it at runtime (timedatectl / Settings), as on bootc.
    time.timeZone = lib.mkDefault null;
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    # Accounts are created and changed at runtime (installer, Settings, `rime user`), not declared.
    users.mutableUsers = true;

    # Rime ships NVIDIA, Steam and other unfree packages (same setting as the flake's own pkgs).
    nixpkgs.config.allowUnfree = true;

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      auto-optimise-store = true;
    };
  };
}
