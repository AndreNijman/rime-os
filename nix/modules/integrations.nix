# Rime in each desktop's own quick settings (docs/nixos/desktops.md §6.3): the integration packages from
# nix/integrations, installed only with their desktop (rime.desktops.<id>.enable, nix/modules/desktops.nix).
#
#   gnome   rime@rimeos.org in share/gnome-shell/extensions; enabled system-wide by the GNOME entry's dconf
#           enabled-extensions list in nix/desktops (one key: a second dconf database would not add to it).
#   kde     the applet org.rimeos.quicksettings in share/plasma/plasmoids; the system tray adopts it by itself
#           (X-Plasma-NotificationAreaCategory + EnabledByDefault).
#   cosmic  cosmic-applet-rime (desktop entry org.rimeos.CosmicAppletRime) and the panel default that lists it
#           (share/cosmic/com.system76.CosmicPanel.Panel, linked by the COSMIC module's pathsToLink /share/cosmic).
#
# Rime Settings itself (org.rimeos.Settings, rime-settings) ships with rime-shell in every variant.
{ config, lib, pkgs, ... }:
let
  cfg = config.rime.desktops;
in
{
  config = lib.mkMerge [
    (lib.mkIf cfg.gnome.enable {
      environment.systemPackages = [ pkgs.rime.rime-gnome-extension ];
    })
    (lib.mkIf cfg.kde.enable {
      environment.systemPackages = [ pkgs.rime.rime-plasma-applet ];
    })
    (lib.mkIf cfg.cosmic.enable {
      environment.systemPackages = [
        pkgs.rime.rime-cosmic-applet
        pkgs.rime.rime-cosmic-applet.panelDefaults
      ];
    })
  ];
}
