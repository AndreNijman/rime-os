# Rime in COSMIC's panel (docs/nixos/desktops.md §6, D10): cosmic-applet-rime, a libcosmic panel applet showing Gaming
# and Update, read from rimed over the system bus by the zbus-only crate rime-cosmic-model (model/), which
# checks.settings-quicksettings-cosmic runs alone against the fake rimed.
#
# passthru.panelDefaults puts the applet in the panel: share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings is
# cosmic-panel's own default (from its source, data/default_schema) with org.rimeos.CosmicAppletRime before the power
# applet. cosmic-config reads system defaults from $XDG_DATA_DIRS/cosmic (NixOS links /share/cosmic); nixpkgs'
# cosmic-panel installs none, so this is the only copy. It is a default: a user who already has a panel config (they
# changed the panel) keeps theirs and adds Rime in COSMIC Settings -> Desktop -> Panel -> Applets.
{
  lib,
  rustPlatform,
  libcosmicAppHook,
  pkg-config,
  dbus,
  cosmic-panel,
  runCommand,
}:

let
  appletId = "org.rimeos.CosmicAppletRime";
  # The applet the Rime entry goes before in the panel's right wing (the session/power menu stays last).
  before = "com.system76.CosmicAppletPower";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./model
      ./applet
      ./data
    ];
  };

  cargoDeps = rustPlatform.fetchCargoVendor {
    inherit src;
    name = "cosmic-applet-rime-vendor";
    hash = "sha256-5gMzKjENn9Jght92tKsdbZlIMUUD1B1N7QWDDvzmzqY=";
  };

  panelDefaults =
    runCommand "rime-cosmic-panel-defaults"
      {
        schema = "${cosmic-panel.src}/data/default_schema";
        inherit appletId before;
      }
      ''
        d=$out/share/cosmic
        mkdir -p $d
        cp -r $schema/com.system76.CosmicPanel $schema/com.system76.CosmicPanel.Panel $d/
        chmod -R u+w $d
        f=$d/com.system76.CosmicPanel.Panel/v1/plugins_wings
        grep -F "\"$before\"" $f >/dev/null || {
          echo "cosmic-panel's default panel no longer lists $before:"; cat $f; exit 1; }
        sed -i "s/\"$before\"/\"$appletId\", \"$before\"/" $f
        grep -F "\"$appletId\", \"$before\"" $f >/dev/null
      '';
in
rustPlatform.buildRustPackage {
  pname = "cosmic-applet-rime";
  version = "1";
  inherit src cargoDeps;

  nativeBuildInputs = [
    pkg-config
    libcosmicAppHook
  ];
  buildInputs = [ dbus ];

  cargoBuildFlags = [
    "-p"
    "cosmic-applet-rime"
  ];
  # The model's tests need the fake rimed on a private bus: checks.settings-quicksettings-cosmic runs them.
  doCheck = false;

  postInstall = ''
    install -Dm0644 data/${appletId}.desktop $out/share/applications/${appletId}.desktop
  '';

  passthru = {
    inherit appletId panelDefaults;
  };

  meta = {
    description = "Rime's Gaming Mode and updates in COSMIC's panel";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "cosmic-applet-rime";
  };
}
