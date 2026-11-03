# The desktops' Quick Settings integrations against a fake rimed (docs/nixos/desktops.md §6, §8.2): no VM, a private
# dbus-daemon standing in for the system bus with rime-shell's tests/lib/fake-rimed.py on it (the same fake the
# Shell's own PowerProfileService test uses, so the contract is written once).
#
#   settings-quicksettings-gnome   the GNOME extension's model (rimed.js) under gjs
#   settings-quicksettings-kde     the Plasma applet's model (RimedModel.qml) under qmltestrunner, with
#                                  plasma-workspace's org.kde.plasma.workspace.dbus module
#   settings-quicksettings-cosmic  the COSMIC applet's model (crate rime-cosmic-model, zbus) under cargo test
#
# Each asserts what the integration would draw as of its last change notification, never a fresh read: the Gaming
# toggle follows Mode.Active changed elsewhere, "Game running" follows GameMode.Active, a click calls Mode.Set, the
# Update item shows for available/restart (click: Updates.Apply) and hides when up to date, and no property is read
# while nothing changes (no polling).
#
# Mutants (passthru.mutants, built by hand; each must FAIL): no-properties-changed = the integration without its
# PropertiesChanged handler.
{ pkgs, self }:

let
  shellSrc = pkgs.rime.rime-shell.src;
  py = pkgs.python3.withPackages (ps: [ ps.pygobject3 ]);
  typelibs = pkgs.lib.makeSearchPath "lib/girepository-1.0" [ pkgs.glib.out pkgs.gobject-introspection ];

  gnomeCheck = ext:
    pkgs.runCommand "settings-quicksettings-gnome"
      {
        nativeBuildInputs = [ pkgs.gjs pkgs.dbus py pkgs.systemd pkgs.coreutils ];
        GI_TYPELIB_PATH = typelibs;
        PYTHONDONTWRITEBYTECODE = "1";
      }
      ''
        . ${shellSrc}/tests/lib/fake-system-bus.sh
        trap 'fake_bus_stop || true' EXIT
        fake_bus_start "$TMPDIR/bus"
        fake_rimed_start
        rc=0
        gjs -m ${./settings-quicksettings-gnome.js} ${ext}/share/gnome-shell/extensions/rime@rimeos.org/rimed.js \
          | tee $TMPDIR/out.log || rc=$?
        grep -E '^RESULT: passed=[0-9]+ failed=0$' $TMPDIR/out.log >/dev/null || rc=1
        [ "$rc" = 0 ] || { echo "--- fake rimed calls"; fake_rimed_calls; exit 1; }
        mkdir -p $out
        cp $TMPDIR/out.log $out/gnome.log
        fake_bus_stop || true
      '';

  gnome = gnomeCheck pkgs.rime.rime-gnome-extension;

  kdeCheck = applet:
    pkgs.runCommand "settings-quicksettings-kde"
      {
        nativeBuildInputs = [ pkgs.dbus py pkgs.systemd pkgs.coreutils pkgs.kdePackages.qtdeclarative ];
        QML_IMPORT_PATH = pkgs.lib.concatMapStringsSep ":" (p: "${p}/${pkgs.kdePackages.qtbase.qtQmlPrefix}") [
          pkgs.kdePackages.plasma-workspace
          pkgs.kdePackages.qtdeclarative
        ];
        QT_QPA_PLATFORM = "offscreen";
        PYTHONDONTWRITEBYTECODE = "1";
      }
      ''
        . ${shellSrc}/tests/lib/fake-system-bus.sh
        trap 'fake_bus_stop || true' EXIT
        fake_bus_start "$TMPDIR/bus"
        fake_rimed_start
        mkdir $TMPDIR/t
        cp ${applet}/share/plasma/plasmoids/org.rimeos.quicksettings/contents/ui/RimedModel.qml $TMPDIR/t/
        cp ${./settings-quicksettings-kde.qml} $TMPDIR/t/tst_quicksettings.qml
        rc=0
        HOME=$TMPDIR qmltestrunner -input $TMPDIR/t/tst_quicksettings.qml 2>&1 | tee $TMPDIR/out.log || rc=$?
        grep -E '^Totals: [0-9]+ passed, 0 failed' $TMPDIR/out.log >/dev/null || rc=1
        [ "$rc" = 0 ] || { echo "--- fake rimed calls"; fake_rimed_calls; exit 1; }
        mkdir -p $out
        cp $TMPDIR/out.log $out/kde.log
        fake_bus_stop || true
      '';

  kde = kdeCheck pkgs.rime.rime-plasma-applet;

  # The COSMIC model: only the zbus crate is compiled (libcosmic is not), from the applet's own sources and vendor.
  cosmicApplet = pkgs.rime.rime-cosmic-applet;
  cosmicCheck = modelMutation:
    pkgs.stdenv.mkDerivation {
      name = "settings-quicksettings-cosmic";
      inherit (cosmicApplet) src cargoDeps;
      nativeBuildInputs = [
        pkgs.rustPlatform.cargoSetupHook
        pkgs.cargo
        pkgs.rustc
        pkgs.dbus
        py
        pkgs.systemd
        pkgs.coreutils
      ];
      PYTHONDONTWRITEBYTECODE = "1";
      postPatch = pkgs.lib.optionalString (modelMutation != null) ''
        sed -i -E ${pkgs.lib.escapeShellArg modelMutation} model/src/lib.rs
      '';
      buildPhase = ''
        runHook preBuild
        cargo test --offline --release -p rime-cosmic-model --no-run -j $NIX_BUILD_CORES
        . ${shellSrc}/tests/lib/fake-system-bus.sh
        trap 'fake_bus_stop || true' EXIT
        fake_bus_start "$TMPDIR/bus"
        fake_rimed_start
        rc=0
        cargo test --offline --release -p rime-cosmic-model -- --nocapture 2>&1 | tee $TMPDIR/out.log || rc=$?
        grep -E '^RESULT: passed=[0-9]+ failed=0$' $TMPDIR/out.log >/dev/null || rc=1
        [ "$rc" = 0 ] || { echo "--- fake rimed calls"; fake_rimed_calls; exit 1; }
        fake_bus_stop || true
        runHook postBuild
      '';
      installPhase = ''
        mkdir -p $out
        cp $TMPDIR/out.log $out/cosmic.log
      '';
    };
  cosmic = cosmicCheck null;
in
{
  settings-quicksettings-gnome = gnome // {
    mutants = {
      no-properties-changed = gnomeCheck (pkgs.rime.rime-gnome-extension.override {
        rimedMutation = "/g-properties-changed/d";
      });
    };
  };
  settings-quicksettings-kde = kde // {
    mutants = {
      no-properties-changed = kdeCheck (pkgs.rime.rime-plasma-applet.override {
        modelMutation = "s/function dbusPropertiesChanged/function dbusPropertiesChangedRemoved/";
      });
    };
  };
  settings-quicksettings-cosmic = cosmic // {
    # The applet itself and its panel default (built by hand; the VM check settings-app-cosmic runs them).
    inherit (cosmicApplet) panelDefaults;
    applet = cosmicApplet;
    mutants = {
      # The PropertiesChanged stream replaced by one that never yields.
      no-properties-changed = cosmicCheck
        "s/props\\.receive_properties_changed\\(\\)\\.await\\?\\.boxed\\(\\)/futures_util::stream::pending().boxed()/";
    };
  };
}
