# The desktops' Quick Settings integrations against a fake rimed (docs/nixos/desktops.md §6, §8.2): no VM, a private
# dbus-daemon standing in for the system bus with rime-shell's tests/lib/fake-rimed.py on it (the same fake the
# Shell's own PowerProfileService test uses, so the contract is written once).
#
#   settings-quicksettings-gnome   the GNOME extension's model (rimed.js) under gjs
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
in
{
  settings-quicksettings-gnome = gnome // {
    mutants = {
      no-properties-changed = gnomeCheck (pkgs.rime.rime-gnome-extension.override {
        rimedMutation = "/g-properties-changed/d";
      });
    };
  };
}
