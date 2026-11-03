# Other desktops (docs/nixos/desktops.md §2, §4): GNOME, KDE Plasma and COSMIC from nixpkgs' own modules, with Rime's
# policy on top. The table is nix/desktops/default.nix; this module turns each entry into rime.desktops.<id>.enable.
#
# A release builds every variant with at most one of these on (D3, nix/release/variants.nix); a machine selects one
# with `rime install gnome` (the switch file rime-feature@gnome.target.on). Nothing here is on by default.
#
# Shared rules, applied whether or not a desktop is on, so every variant behaves the same:
#   - power-profiles-daemon never runs (desktop.nix forces it off): rimed serves its D-Bus API (§7), and COSMIC's
#     module would otherwise turn system76-power on, a second writer of the files rimed writes.
#   - blueman-applet starts only in Rime's sessions: GNOME, Plasma and COSMIC have their own Bluetooth UI, and two
#     Bluetooth agents fight over pairing. Rime's sessions do not process /etc/xdg/autostart at all.
#   - kwallet-pam's autostart runs only in Plasma (it has no ShowIn and would start kwalletd in every session).
{ config, lib, pkgs, ... }:
let
  table = import ../desktops;
  cfg = config.rime.desktops;
  enabled = lib.filterAttrs (id: _: cfg.${id}.enable) table;
  secondPowerDaemons = lib.unique (
    lib.filter (n: lib.elem n [ "power-profiles-daemon" "system76-power" ]) (
      map (p: p.pname or (builtins.parseDrvName (p.name or "")).name) (
        config.environment.systemPackages ++ config.services.dbus.packages ++ config.systemd.packages
      )
    )
  );

  # An autostart entry with one key added, written under /etc/xdg/autostart, which comes before the system profile's
  # etc/xdg in XDG_CONFIG_DIRS: the autostart spec reads the first file of a name and ignores the rest.
  autostartOverride =
    name: src: line:
    pkgs.runCommand "rime-autostart-${name}" { } ''
      if grep -qE '^(Only|Not)ShowIn=' ${src}; then
        echo "${src} already restricts where it shows: re-check this override" >&2
        exit 1
      fi
      sed '/^\[Desktop Entry\]$/a ${line}' ${src} > $out
      grep -qx '${line}' $out
    '';
in
{
  options.rime.desktops = lib.mapAttrs (
    id: d:
    {
      enable = lib.mkEnableOption "the ${d.title} desktop beside Rime's sessions (${d.summary})";
    }
  ) table;

  imports = lib.mapAttrsToList (
    id: d:
    { config, lib, pkgs, ... }@args:
    {
      config = lib.mkIf config.rime.desktops.${id}.enable (d.config args);
    }
  ) table;

  config = lib.mkMerge [
    {
      assertions = [
        {
          assertion = lib.length (lib.attrNames enabled) <= 1;
          message = "Rime adds one other desktop at a time (docs/nixos/desktops.md D3): ${lib.concatStringsSep ", " (lib.attrNames enabled)} are all on";
        }
        {
          # D6: rimed owns org.freedesktop.UPower.PowerProfiles and net.hadess.PowerProfiles. A second daemon on the
          # system (or its D-Bus policy) would take the name or write the same sysfs files.
          assertion = secondPowerDaemons == [ ];
          message = "rimed serves the power-profiles API; these packages would run a second power daemon: ${lib.concatStringsSep ", " secondPowerDaemons}";
        }
      ];

      rime.desktop.extraSessions = lib.concatLists (lib.mapAttrsToList (_: d: d.sessions pkgs) enabled);

      hardware.system76.power-daemon.enable = lib.mkForce false;
    }

    (lib.mkIf config.services.blueman.enable {
      environment.etc."xdg/autostart/blueman.desktop".source =
        autostartOverride "blueman" "${pkgs.blueman}/etc/xdg/autostart/blueman.desktop"
          "NotShowIn=GNOME;KDE;COSMIC;";
    })

    (lib.mkIf (cfg.kde.enable or false) {
      environment.etc."xdg/autostart/pam_kwallet_init.desktop".source =
        autostartOverride "pam-kwallet-init" "${pkgs.kdePackages.kwallet-pam}/etc/xdg/autostart/pam_kwallet_init.desktop"
          "OnlyShowIn=KDE;";
    })
  ];
}
