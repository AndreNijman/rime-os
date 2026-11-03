# Rime's other desktops (docs/nixos/desktops.md §2): one entry per desktop, read by
#
#   nix/modules/desktops.nix      options rime.desktops.<id>.enable; applies `config` and the session packages
#   nix/catalogue/catalogue.nix   one class 1 feature per entry (group "desktop") plus a toggle per id and alias
#   nix/release/variants.nix      the release variants: every feature subset with at most one desktop (D3)
#   nix/release/floors.nix        the disk floor of each published desktop variant (base + the measured delta, D14)
#
# Pure data at the top level (no pkgs): the catalogue imports this file and reads only title/summary/aliases.
#
#   title         what `rime` prints ("GNOME is installed")
#   summary       one line for `rime search` / `rime info`
#   aliases       more catalogue names for the same desktop
#   sessions      pkgs -> the session packages for rime.desktop.extraSessions (desktop.nix mkForces the display
#                 manager's sessionPackages, so a desktop module's own entry never reaches the greeter)
#   sessionIds    the session file ids those packages provide (= passthru.providedSessions; checks.desktops-eval)
#   desktopNames  the session file's DesktopNames, which rime-greet passes as XDG_CURRENT_DESKTOP
#   delta         measured closure NAR bytes the desktop adds to its base system (2026-11-03, measure-sizes.txt);
#                 floors.nix turns it into the variant's disk floor. An entry without one fails checks.release-plan.
#   config        a NixOS module taking { config, lib, pkgs }, applied only while rime.desktops.<id>.enable is true
#
# Adding a desktop is one entry here plus its measured delta (the Xfce example in desktops.md §2).
{
  gnome = {
    title = "GNOME";
    summary = "the GNOME 50 desktop (GNOME Shell, Files, Settings), at the login screen beside Rime's sessions";
    aliases = [ ];
    sessions = pkgs: [ pkgs.gnome-session.sessions ];
    sessionIds = [ "gnome" ];
    desktopNames = [ "GNOME" ];
    delta = 1110685288;
    config =
      { pkgs, lib, ... }:
      {
        services.desktopManager.gnome.enable = true;
        # D15: no network sharing surfaces on a rime-firewall machine; Rime is the app store; no first-run wizard
        # (the Rime installer already made the account).
        services.gnome.gnome-initial-setup.enable = false;
        services.gnome.gnome-remote-desktop.enable = false;
        services.gnome.rygel.enable = false;
        services.gnome.gnome-user-share.enable = false;
        environment.gnome.excludePackages = with pkgs; [ gnome-tour epiphany gnome-software ];
        # fcitx5 is Rime's one input method (i18n.inputMethod); GNOME shows its candidates through kimpanel.
        environment.systemPackages = [ pkgs.gnomeExtensions.kimpanel ];
        programs.dconf.profiles.user.databases = [
          {
            # rime@rimeos.org: Gaming and Update in Quick Settings (nix/integrations/gnome, nix/modules/integrations.nix).
            settings."org/gnome/shell".enabled-extensions = [
              pkgs.gnomeExtensions.kimpanel.extensionUuid
              pkgs.rime.rime-gnome-extension.passthru.uuid
            ];
          }
        ];
      };
  };

  kde = {
    title = "KDE Plasma";
    summary = "the KDE Plasma 6 desktop, at the login screen beside Rime's sessions";
    aliases = [ "plasma" ];
    sessions = pkgs: [ pkgs.kdePackages.plasma-workspace.sessions ];
    sessionIds = [ "plasma" ];
    desktopNames = [ "KDE" ];
    delta = 2424402376;
    config =
      { config, pkgs, ... }:
      {
        services.desktopManager.plasma6.enable = true;
        environment.plasma6.excludePackages = [ pkgs.kdePackages.discover ];
        # fcitx5 (Rime's i18n.inputMethod) as KWin's Wayland input method: KWin starts it, and Plasma's
        # Virtual Keyboard settings show it as chosen. A user's own ~/.config/kwinrc still wins.
        environment.etc."xdg/kwinrc".text = ''
          [Wayland]
          InputMethod=${config.i18n.inputMethod.package}/share/applications/org.fcitx.Fcitx5.desktop
        '';
        # D12: gnome-keyring is the one org.freedesktop.secrets; KWallet keeps its own API only.
        environment.etc."xdg/kwalletrc".text = ''
          [org.freedesktop.secrets]
          apiEnabled=false
        '';
      };
  };

  cosmic = {
    title = "COSMIC";
    summary = "the COSMIC desktop from System76, at the login screen beside Rime's sessions";
    aliases = [ ];
    sessions = pkgs: [ pkgs.cosmic-session ];
    sessionIds = [ "cosmic" ];
    desktopNames = [ "COSMIC" ];
    delta = 2519964376;
    config =
      { lib, ... }:
      {
        services.desktopManager.cosmic.enable = true;
        # rimed owns the CPU policy (scx, cpusets, tiers); system76-scheduler would fight it.
        services.system76-scheduler.enable = lib.mkForce false;
      };
  };
}
