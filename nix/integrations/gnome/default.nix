# Rime's GNOME Shell extension, rime@rimeos.org (docs/nixos/desktops.md §6, D10): Gaming and Update in Quick
# Settings, read from rimed over the system bus. Installed to share/gnome-shell/extensions and enabled system-wide by
# the GNOME entry in nix/desktops (dconf org/gnome/shell/enabled-extensions).
#
# `shell-version` in metadata.json is the major version of the gnome-shell this system carries. GNOME refuses to load
# an extension whose list does not name its major ("OUT OF DATE"), so a nixpkgs bump to GNOME 51 fails this build
# (`knownMajors` below) until the extension has been checked against 51's quickSettings.js and the major added.
{
  lib,
  stdenvNoCC,
  gnome-shell,
  gjs,
  # Test only: the version written into metadata.json (the shell-version mutant).
  shellVersionOverride ? null,
  # Test only: a sed expression applied to rimed.js (the PropertiesChanged mutant).
  rimedMutation ? null,
}:

let
  major = lib.versions.major gnome-shell.version;
  # The GNOME Shell majors this extension's code was read against (quickSettings.js addExternalIndicator,
  # QuickToggle/QuickMenuToggle `subtitle`, SystemIndicator._addIndicator: 50.4 at nixpkgs 1c89083).
  knownMajors = [ "50" ];
  shellVersion = if shellVersionOverride != null then shellVersionOverride else major;
in
assert lib.assertMsg (lib.elem major knownMajors)
  "rime@rimeos.org was checked against GNOME Shell ${lib.concatStringsSep ", " knownMajors}; this system has ${gnome-shell.version}. Read its js/ui/quickSettings.js and add the major to knownMajors.";

stdenvNoCC.mkDerivation {
  pname = "gnome-shell-extension-rime";
  version = "1";
  src = ./.;

  nativeBuildInputs = [ gjs ];

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    d=$out/share/gnome-shell/extensions/rime@rimeos.org
    install -d $d
    install -m0644 'rime@rimeos.org/extension.js' 'rime@rimeos.org/rimed.js' $d/
    sed 's/@SHELL_VERSION@/${shellVersion}/' 'rime@rimeos.org/metadata.json.in' > $d/metadata.json
    ${lib.optionalString (rimedMutation != null) "sed -i -E ${lib.escapeShellArg rimedMutation} $d/rimed.js"}
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    d=$out/share/gnome-shell/extensions/rime@rimeos.org
    # metadata is valid JSON naming this uuid and exactly the major GNOME will compare against.
    gjs -c "
      const m = JSON.parse(new TextDecoder().decode(imports.gi.GLib.file_get_contents('$d/metadata.json')[1]));
      if (m.uuid !== 'rime@rimeos.org') throw new Error('uuid ' + m.uuid);
      if (JSON.stringify(m['shell-version']) !== JSON.stringify(['${shellVersion}'])) throw new Error('shell-version');
    "
    # extension.js imports resource:///org/gnome/shell/..., which resolve only inside GNOME Shell: it is loaded for
    # real by checks.settings-app-gnome (State ACTIVE); rimed.js runs in checks.settings-quicksettings-gnome.
  '';

  passthru = {
    uuid = "rime@rimeos.org";
    inherit major knownMajors;
  };

  meta = {
    description = "Rime's Gaming Mode and updates in GNOME's Quick Settings";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
