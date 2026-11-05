# Rime Settings and the quick-settings integrations in every release variant (docs/nixos/desktops.md §6.3, §8.2).
# Evaluation only (no system is built):
#   - Rime Settings (org.rimeos.Settings, rime-settings) is in every variant: it ships with rime-shell;
#   - each desktop's integration is in a variant exactly when that desktop is: GNOME's extension, KDE's plasmoid,
#     COSMIC's applet and its panel default; a variant without the desktop has none of it;
#   - GNOME's dconf enabled-extensions names rime@rimeos.org (and still kimpanel@kde.org) in one database;
{ pkgs, self }:
let
  lib = pkgs.lib;
  variants = import ../release/variants.nix { inherit self lib; };
  ids = lib.attrNames (import ../desktops);
  d = builtins.unsafeDiscardStringContext;

  facts = _: v:
    let
      c = v.system.config;
      p = v.system.pkgs.rime;
      paths = map (x: d (toString x)) c.environment.systemPackages;
      has = drv: lib.elem (d (toString drv)) paths;
      dbs = c.programs.dconf.profiles.user.databases or [ ];
      extLists = lib.concatMap (db: lib.optional ((db.settings or { }) ? "org/gnome/shell")
        (db.settings."org/gnome/shell".enabled-extensions or [ ])) dbs;
    in
    {
      desktops = lib.filter (id: c.rime.desktops.${id}.enable) ids;
      settingsApp = has p.rime-shell;
      gnomeExtension = has p.rime-gnome-extension;
      plasmoid = has p.rime-plasma-applet;
      cosmicApplet = has p.rime-cosmic-applet;
      cosmicPanel = has p.rime-cosmic-applet.panelDefaults;
      gnomeExtensionLists = extLists;
    };

  all = lib.mapAttrs facts variants;

  problems = lib.concatLists (lib.mapAttrsToList (name: f:
    let
      on = id: lib.elem id f.desktops;
      want = cond: what: lib.optional (!cond) "${name}: ${what}";
    in
    want f.settingsApp "Rime Settings (rime-shell) is not in systemPackages"
    ++ want (f.gnomeExtension == on "gnome") "GNOME extension present=${lib.boolToString f.gnomeExtension}, GNOME on=${lib.boolToString (on "gnome")}"
    ++ want (f.plasmoid == on "kde") "KDE plasmoid present=${lib.boolToString f.plasmoid}, KDE on=${lib.boolToString (on "kde")}"
    ++ want (f.cosmicApplet == on "cosmic") "COSMIC applet present=${lib.boolToString f.cosmicApplet}, COSMIC on=${lib.boolToString (on "cosmic")}"
    ++ want (f.cosmicPanel == on "cosmic") "COSMIC panel default present=${lib.boolToString f.cosmicPanel}, COSMIC on=${lib.boolToString (on "cosmic")}"
    ++ lib.optionals (on "gnome") (
      want (lib.length f.gnomeExtensionLists == 1) "GNOME: ${toString (lib.length f.gnomeExtensionLists)} dconf databases set enabled-extensions (one list, or one hides the other)"
      ++ want (lib.any (l: lib.elem "rime@rimeos.org" l && lib.elem "kimpanel@kde.org" l) f.gnomeExtensionLists)
        "GNOME: enabled-extensions ${builtins.toJSON f.gnomeExtensionLists} lacks rime@rimeos.org or kimpanel@kde.org")
  ) all);

  report = pkgs.writeText "settings-integrations-eval.json" (builtins.toJSON all);
in
{
  settings-integrations-eval = pkgs.runCommand "settings-integrations-eval" { } ''
    cat ${report} > $out
    ${if problems == [ ] then ''
      echo "settings-integrations-eval: ${toString (lib.length (lib.attrNames all))} variants checked, 0 problems" | tee -a $out
    '' else ''
      cat <<'PROBLEMS'
      ${lib.concatStringsSep "\n" problems}
      PROBLEMS
      exit 1
    ''}
  '';
}
