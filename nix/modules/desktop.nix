# Desktop: greetd, Hyprland/niri/labwc sessions, Rime Shell, portals, fonts, audio (PLAN.md 4.8).
# Skeleton stub: the option exists so hosts and tests can set it; the owning worker fills config.
#
# Sections marked "# shell" belong to the shell port (nix/pkgs/rime-shell.nix, nix/tests/shell.nix): Rime Shell,
# what it needs at run time, and the command a compositor session runs to start it. Session plumbing (greetd,
# rime-greet, the compositors, portals, the session files) belongs to the desktop port.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.desktop;

  # shell
  shellCfg = cfg.shell;
  shellPkg = shellCfg.package;

  # shell: what a compositor session execs once at start (bootc: /usr/libexec/rime-shell-autostart, exec'd from
  # files/desktop/hypr/rime/session.lua, the labwc autostart and the niri config). It runs rime-os's own
  # launcher unchanged and only supplies, by environment, what that launcher reads from Fedora paths:
  #   RIME_SHELL_DIR     the shell directory; the path contract in nix/pkgs/rime-shell.nix
  #   RIME_QML_DIR       the Rime.I18n module root (bootc: /usr/lib64/rime-shell/qml)
  #   RIME_SECRETS_START the login-keyring hand-over (bootc: /usr/libexec/rime-secrets-start)
  # PATH is set here, not inherited: the session's own PATH may not hold the system profile (session.lua sets
  # PATH to Fedora's directories), and the launcher needs pgrep, hypridle and quickshell from it.
  shellAutostart = pkgs.writeShellScriptBin "rime-shell-autostart" ''
    export RIME_SHELL_DIR="''${RIME_SHELL_DIR:-${shellPkg.shellDir}}"
    export RIME_QML_DIR="''${RIME_QML_DIR:-${shellPkg.qmlImportPath}}"
    export RIME_SECRETS_START="''${RIME_SECRETS_START:-${pkgs.rime.rime-libexec}/libexec/rime-secrets-start}"
    export QT_PLUGIN_PATH="${shellPkg.qtPluginPath}''${QT_PLUGIN_PATH:+:$QT_PLUGIN_PATH}"
    export PATH="${lib.makeBinPath shellPkg.runtimeDeps}:/run/wrappers/bin:/run/current-system/sw/bin''${PATH:+:$PATH}"
    # Run by bash, not executed: the file is 0644 in git (the image installs it with COPY --chmod=0755).
    exec ${pkgs.bash}/bin/bash ${pkgs.rime.rime-libexec}/libexec/rime-shell-autostart "$@"
  '';
in
{
  options.rime.desktop = {
    enable = lib.mkEnableOption "Rime's desktop (sessions, Shell, portals)";

    # shell
    shell = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = cfg.enable;
        defaultText = lib.literalExpression "config.rime.desktop.enable";
        description = "Rime Shell (the bar, launcher, notifications and lock screen) and what it needs at run time.";
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.rime.rime-shell;
        defaultText = lib.literalExpression "pkgs.rime.rime-shell";
        description = "The Rime Shell package.";
      };

      autostart = lib.mkOption {
        type = lib.types.package;
        readOnly = true;
        default = shellAutostart;
        defaultText = lib.literalMD "a wrapper around rime-os's `rime-shell-autostart`";
        description = ''
          The command a compositor session runs once at start to bring up hypridle and Rime Shell
          (`''${autostart}/bin/rime-shell-autostart`, also on PATH as `rime-shell-autostart`). Idempotent.
        '';
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable { })

    # shell
    (lib.mkIf shellCfg.enable {
      environment.systemPackages = [
        shellPkg
        shellCfg.autostart
        # The shell calls ~15 rime-os helpers (rime-switcher, rime-session-select, rime-greet-wallpaper, ...)
        # through ${shellPkg.shellDir}'s rewritten /run/current-system/sw/libexec paths.
        pkgs.rime.rime-libexec
        # Icons the shell resolves through Quickshell.iconPath (app icons fall back to hicolor/Adwaita).
        pkgs.adwaita-icon-theme
        pkgs.hicolor-icon-theme
      ]
      ++ shellPkg.runtimeDeps;

      # The path contract (nix/pkgs/rime-shell.nix): /run/current-system/sw/share/rime-shell must exist and
      # /run/current-system/sw/libexec must hold rime-os's helpers.
      # /share/hyprshade: the colour-filter picker lists /usr/share/hyprshade/shaders (rewritten to the profile;
      # QuickSettings.qml shaderPaths). Nothing else links it, so without this the system shaders vanish silently.
      environment.pathsToLink = [
        "/share/rime-shell"
        "/libexec"
        "/share/hyprshade"
      ];

      # `rime shell` (rimed/rime/src/main.rs) and the launcher read it; sessions that bypass the wrapper still agree.
      environment.sessionVariables.RIME_SHELL_DIR = shellPkg.shellDir;

      # The shell's text uses both family names: "JetBrains Mono" and "JetBrainsMono Nerd Font" (icon glyphs).
      fonts.packages = [
        pkgs.jetbrains-mono
        pkgs.nerd-fonts.jetbrains-mono
      ];

      # The lock screen authenticates with PamContext { config: "system-auth" } (src/windows/Lockscreen.qml).
      # Fedora ships /etc/pam.d/system-auth; NixOS does not, and an unknown service falls through to "other",
      # which denies: the user could never unlock. This declares it with NixOS's default (pam_unix) rules.
      security.pam.services.system-auth = { };

      # org.rimeos.shell.agent.policy is read from the system profile's share/polkit-1/actions.
      security.polkit.enable = lib.mkDefault true;

      # Quickshell.Services.UPower (battery, power profiles) talks to the upower daemon.
      services.upower.enable = lib.mkDefault true;
    })
  ];
}
