# Rime's terminal: Ghostty by default, Alacritty still installed as the alternative.
#
#   - xdg-terminal-exec (what "open a terminal" means for launchers, .desktop Terminal=true apps and Rime's own
#     helpers) tries Ghostty first, then Alacritty; $TERMINAL is ghostty.
#   - Every user's Alacritty setup carries over once: rime-terminal-migrate (nix/pkgs/rime-terminal.nix) runs as
#     a user oneshot at each login, seeds Rime's Ghostty theme when it is missing and, the first time only, writes
#     ~/.config/ghostty/config.ghostty from their Alacritty config. It never edits an existing Ghostty config or
#     any Alacritty file, and never fails the login.
#
# Ghostty reads no system-wide config file (only ~/.config/ghostty/...), so Rime's look reaches users as the
# `rime-os` theme in their own config directory rather than through /etc.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.terminal;
  pkg = pkgs.rime.rime-terminal;
in
{
  options.rime.terminal = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = config.rime.desktop.enable;
      defaultText = lib.literalExpression "config.rime.desktop.enable";
      description = "Ghostty as the default terminal, Alacritty as the alternative, and the Alacritty carry-over.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.ghostty;
      defaultText = lib.literalExpression "pkgs.ghostty";
      description = "The Ghostty package.";
    };

    migrate = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Run rime-terminal-migrate at each login: seed ~/.config/ghostty/themes/rime-os when missing and, once per
        user, write ~/.config/ghostty/config.ghostty from their Alacritty settings (report:
        ~/.local/state/rime/terminal-migration.txt).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      cfg.package
      pkgs.alacritty
      pkg
    ];

    # com.mitchellh.ghostty.desktop is Ghostty's own desktop file id (its Exec line runs a single GTK instance).
    xdg.terminal-exec = {
      enable = true;
      settings.default = [ "com.mitchellh.ghostty.desktop" "Alacritty.desktop" ];
    };
    environment.sessionVariables.TERMINAL = "ghostty";

    systemd.user.services.rime-terminal-migrate = lib.mkIf cfg.migrate {
      description = "Carry Alacritty settings over to Ghostty (once) and seed Rime's Ghostty theme";
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = "!@system";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkg}/libexec/rime-terminal-migrate --theme ${pkg}/share/rime/ghostty/rime-os";
        # the script exits 0 on every error by design; this only bounds a hung filesystem
        TimeoutStartSec = "30s";
      };
    };
  };
}
