# The terminal greeting: fastfetch with the Rime symbol, in the image protocol the terminal speaks, recoloured
# from the wallpaper (docs/branding.md "Terminal greeting").
#
#   rime-ridge-files greeting.sh           the greeting itself, POSIX sh: files/desktop/shell/rime-greeting.sh with
#                                          nix/branding/ridge.patch (the bootc file keeps the spark). rime-libexec
#                                          installs it at share/rime/shell/greeting.sh. zsh reaches it from the
#                                          seeded ~/.zshrc; bash from here (bootc: /etc/bashrc).
#   rime-ridge-files config.jsonc          /etc/fastfetch/config.jsonc: files/system/fastfetch/config.jsonc with the
#                                          same patch: Rime's modules, Rime Blue keys, and the "RIME OS" label for
#                                          terminals with no image protocol.
#   pkgs.rime.rime-greeting                libexec/rime-term-graphics (protocol choice) and libexec/rime-logo-render
#                                          (Rime Shell's matugen post_hook: ~/.cache/rime/logo/rime-mark-*.png).
#   pkgs.rime.rime-branding                share/rime/branding/png/symbol/rime-symbol-{dark,light}-512.png, the logo
#                                          before any wallpaper, and the adaptive template rime-logo-render fills.
#
# Its own module (not desktop.nix) so the greeting can be switched on and off on its own, and so its files have
# one owner on the profile.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.greeting;
  greeting = "/run/current-system/sw/share/rime/shell/greeting.sh";
in
{
  options.rime.greeting = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = config.rime.desktop.enable;
      defaultText = lib.literalExpression "config.rime.desktop.enable";
      description = "The fastfetch greeting with the Rime logo in every new interactive terminal.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      pkgs.fastfetch
      pkgs.rime.rime-greeting
      pkgs.rime.rime-branding
    ];
    # rime-term-graphics and rime-logo-render are reached on the profile as libexec/<name>, rime-branding's PNGs as
    # share/rime/branding (base.nix links /libexec and /share/rime; listed here too so the greeting does not depend
    # on that).
    environment.pathsToLink = [ "/libexec" "/share/rime" ];

    environment.etc."fastfetch/config.jsonc".source = "${pkgs.rime.rime-ridge-files}/etc/fastfetch/config.jsonc";

    # bash: last in /etc/bashrc's interactive part, as bootc appended it to /etc/bashrc. The file checks for itself
    # that the shell is interactive, on a terminal, and not already greeted (RIME_GREETED).
    programs.bash.interactiveShellInit = lib.mkAfter ''
      [ -r ${greeting} ] && . ${greeting}
    '';
  };
}
