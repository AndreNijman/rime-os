# Rime Settings and Rime's quick settings in another desktop, in a VM (docs/nixos/desktops.md §8.2).
#
#   settings-app-gnome   GNOME enabled directly in the test machine (installing it is the desktops piece's test):
#                        boots to rime-greet, logs in to GNOME through the greeter (the correct password, typed),
#                        then:
#                          - the extension rime@rimeos.org is ACTIVE (`gnome-extensions info`) and gnome-shell
#                            logged no error for it;
#                          - it draws: the test sink org.rimeos.QuickSettingsTest receives its report (the hook in
#                            extension.js), which also proves rimed is on the bus from GNOME's side;
#                          - `gtk-launch org.rimeos.Settings` opens one window that GNOME matches to the
#                            org.rimeos.Settings app (Shell Introspect GetWindows: app-id + wm-class), screenshot;
#                          - no Rime Shell process runs in the GNOME session.
#
# Mutants (passthru.mutants, built by hand; each must FAIL):
#   wrong-app-id         rime-settings starts the app with QS_APP_ID=org.rimeos.Wrong, so GNOME cannot match the
#                        window to org.rimeos.Settings.desktop.
#   wrong-shell-version  metadata.json names shell-version 49 on GNOME 50: GNOME does not load the extension.
#
#                          - `rime mode set gaming` / `daily` flips the extension's Gaming toggle (its report);
#                          - Power -> performance in the app (its IPC test hook runs the Power page's own handler)
#                            reaches rimed's Power.Tier, `powerprofilesctl get` and the power-profiles property GNOME
#                            reads from inside the session.
# These need the desktops piece's rimed D-Bus contract (Mode.Active/Set, Updates, the power-profiles API) and its
# greeter session environment: on a branch without them the check fails (nix/ci/checks-not-in-ci.txt).
{ pkgs, self }:

let
  sink = pkgs.writers.writePython3 "rime-quicksettings-sink" {
    libraries = [ pkgs.python3Packages.pygobject3 ];
    flakeIgnore = [ "E501" ];
  } ''
    # Owns org.rimeos.QuickSettingsTest on the session bus and appends every Report(ss) to argv[1] as one line.
    import sys
    from gi.repository import Gio, GLib

    XML = """
    <node><interface name="org.rimeos.QuickSettingsTest">
      <method name="Report">
        <arg type="s" direction="in"/><arg type="s" direction="in"/>
      </method>
    </interface></node>"""
    out = sys.argv[1]


    def call(conn, sender, path, iface, method, params, inv):
        desktop, state = params.unpack()
        with open(out, "a") as f:
            f.write(desktop + " " + state + "\n")
        inv.return_value(None)


    def acquired(conn, name):
        info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
        conn.register_object("/org/rimeos/QuickSettingsTest", info, call, None, None)


    Gio.bus_own_name(Gio.BusType.SESSION, "org.rimeos.QuickSettingsTest",
                     Gio.BusNameOwnerFlags.NONE, acquired, None, None)
    GLib.MainLoop().run()
  '';

  # GNOME 50 answers Shell Introspect's GetWindows only in unsafe mode or to the owner of one of two portal backend
  # names (js/misc/introspect.js APP_ALLOWLIST; there is no `introspect` setting any more). This probe stops the GTK
  # portal backend (a D-Bus-activated user unit, which comes back on the next request), owns
  # org.freedesktop.impl.portal.desktop.gtk for the call and prints GetWindows' answer in GVariant text. Test only:
  # nothing in the shipped system does this.
  windowsProbe = pkgs.writers.writePython3 "rime-gnome-windows" {
    libraries = [ pkgs.python3Packages.pygobject3 ];
    flakeIgnore = [ "E501" ];
  } ''
    import subprocess
    import sys
    import time
    from gi.repository import Gio, GLib

    NAME = "org.freedesktop.impl.portal.desktop.gtk"
    conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    for _ in range(20):
        subprocess.run(["systemctl", "--user", "stop", "xdg-desktop-portal-gtk.service"], check=False)
        (r,) = conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                              GLib.Variant("(su)", (NAME, 4)), GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE, -1,
                              None).unpack()
        if r in (1, 4):
            break
        time.sleep(0.2)
    else:
        sys.exit(NAME + " is still owned by something else (RequestName " + str(r) + ")")
    # gnome-shell learns the new owner from a NameOwnerChanged watch: retry while it has not yet.
    for _ in range(50):
        try:
            w = conn.call_sync("org.gnome.Shell.Introspect", "/org/gnome/Shell/Introspect", "org.gnome.Shell.Introspect",
                               "GetWindows", None, None, Gio.DBusCallFlags.NONE, -1, None)
            print(w.print_(True))
            sys.exit(0)
        except GLib.Error as e:
            if "AccessDenied" not in str(e):
                raise
            time.sleep(0.1)
    sys.exit("GetWindows still denied")
  '';

  gnomeTest = { name, overlay ? null }:
    pkgs.testers.runNixOSTest {
      inherit name;
      node.pkgsReadOnly = false;

      nodes.machine = { lib, pkgs, ... }: {
        imports = [ self.nixosModules.default ];
        nixpkgs.overlays = lib.mkAfter (lib.optional (overlay != null) overlay);

        rime.services.enable = true;
        rime.desktop.enable = true;
        rime.desktops.gnome.enable = true;
        # powerprofilesctl, the client GNOME uses, by store path: power-profiles-daemon is never in a Rime system.
        virtualisation.additionalPaths = [ pkgs.power-profiles-daemon ];

        users.users.rime = {
          isNormalUser = true;
          uid = 1000;
          initialPassword = "rime";
          extraGroups = [ "wheel" "video" "audio" "networkmanager" ];
        };

        virtualisation = {
          memorySize = 6144;
          cores = 4;
          qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
        };
        # As nix/tests/desktop.nix: the greeter's sway renders with pixman, Qt Quick in software.
        environment.sessionVariables = {
          WLR_RENDERER = "pixman";
          QT_QUICK_BACKEND = "software";
        };
        systemd.services.rime-flathub-setup.enable = lib.mkForce false;

        # The greeter preselects GNOME (rime-greet reads the session stem from last-session).
        systemd.tmpfiles.rules = [ "f+ /var/lib/rime-greet/last-session 0644 greetd greetd - gnome" ];
        environment.systemPackages = [ pkgs.gtk3 ];
      };

      testScript = ''
        import json
        import shlex

        def user(cmd):
            env = ("export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
                   "WAYLAND_DISPLAY=wayland-0; ")
            return "su - rime -c " + shlex.quote(env + cmd)

        machine.wait_for_unit("multi-user.target")

        with subtest("the greeter offers GNOME and preselects it"):
            machine.succeed("grep -l '^Name=GNOME' /etc/rime/wayland-sessions/*.desktop")
            machine.succeed("test \"$(cat /var/lib/rime-greet/last-session)\" = gnome")
            machine.wait_for_unit("greetd.service")
            machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
            machine.sleep(8)
            machine.screenshot("greeter")

        with subtest("logs in to GNOME through rime-greet"):
            machine.send_chars("rime\n")
            machine.sleep(2)
            machine.send_chars("rime\n")
            # NixOS wraps gnome-shell: comm is ".gnome-shell-wr", argv0 "gnome-shell"; the ShellVersion call below is the proof.
            machine.wait_until_succeeds("pgrep -u rime gnome-shell", timeout=240)
            machine.wait_until_succeeds("test -S /run/user/1000/wayland-0", timeout=120)
            machine.wait_until_succeeds(user("gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell "
                                             "--method org.freedesktop.DBus.Properties.Get org.gnome.Shell ShellVersion"),
                                        timeout=120)
            print(machine.succeed("ps -u rime -o pid,comm,args --no-headers | cut -c1-200"))
            machine.sleep(10)
            machine.screenshot("gnome")

        with subtest("the extension rime@rimeos.org is ACTIVE and logged no error"):
            machine.wait_until_succeeds(user("gnome-extensions info rime@rimeos.org | grep -E 'State: ACTIVE'"),
                                               timeout=60)
            print(machine.succeed(user("gnome-extensions info rime@rimeos.org")))
            ext_log = machine.succeed("journalctl -b _UID=1000 --no-pager -o cat | grep -i -F 'rime@rimeos.org' || true")
            print("journal lines naming the extension:", ext_log)
            assert "rror" not in ext_log, ext_log

        with subtest("the extension draws: its report reaches the test sink"):
            machine.succeed(user("systemd-run --user --unit rime-qs-sink ${sink} /tmp/qs-reports.txt"))
            machine.wait_until_succeeds("grep '^gnome ' /tmp/qs-reports.txt", timeout=60)
            line = machine.succeed("grep '^gnome ' /tmp/qs-reports.txt | tail -n1").strip()
            print("report:", line)
            state = json.loads(line.split(" ", 1)[1])
            assert "gaming" in state and "update" in state, state

        def reports():
            return machine.succeed("grep '^gnome ' /tmp/qs-reports.txt | tail -n1").strip()

        with subtest("the extension follows rimed: rime mode set gaming flips the Gaming toggle"):
            machine.wait_for_unit("rimed.service")
            machine.wait_until_succeeds("grep '^gnome .*\"reactive\":true' /tmp/qs-reports.txt", timeout=60)
            machine.succeed("rime mode set gaming")
            machine.wait_until_succeeds("grep '^gnome ' /tmp/qs-reports.txt | tail -n1 | grep -F '\"gaming\":{\"checked\":true'", timeout=30)
            print("after gaming:", reports())
            machine.succeed("rime mode set daily")
            machine.wait_until_succeeds("grep '^gnome ' /tmp/qs-reports.txt | tail -n1 | grep -F '\"gaming\":{\"checked\":false'", timeout=30)
            print("after daily:", reports())

        with subtest("Rime Settings opens from gtk-launch as org.rimeos.Settings"):
            # In a user scope, as GNOME starts apps: a process left in su's PAM session is not in the active seat
            # session, and polkit would refuse it what the desktop allows the logged-in user.
            machine.succeed(user("systemd-run --user --scope --unit rime-settings-launch "
                                 "gtk-launch org.rimeos.Settings >/tmp/settings-launch.log 2>&1"))
            machine.wait_until_succeeds("pgrep -u rime -f 'rime-shell/settings.qml'", timeout=60)

            machine.wait_until_succeeds(user("${windowsProbe} | grep -F org.rimeos"), timeout=60)
            w = machine.succeed(user("${windowsProbe}"))
            print(w)
            assert "'wm-class': <'org.rimeos.Settings'>" in w, "no window with wm-class org.rimeos.Settings"
            assert "'app-id': <'org.rimeos.Settings.desktop'>" in w, "GNOME did not match the window to org.rimeos.Settings.desktop"
            assert w.count("org.rimeos.Settings'") == 1, "more than one Rime Settings window"
            machine.sleep(3)
            machine.screenshot("settings-app")
            print(machine.succeed("cat /tmp/settings-launch.log || true"))

        with subtest("Power -> performance in the app reaches rimed's tier, powerprofilesctl and GNOME's reading"):
            PPD = "org.freedesktop.UPower.PowerProfiles"
            PPCTL = "${pkgs.power-profiles-daemon}/bin/powerprofilesctl"
            # rime-settings runs "<qs> -n -p <settings.qml>": the same quickshell and path answer its IPC.
            fields = machine.succeed("pgrep -u rime -a -f 'rime-shell/settings.qml' | head -n1").split()
            qs, app = fields[1], fields[-1]
            machine.succeed("rime tier balanced")
            # PowerProfileService's owner check, as the user's session sends it: the power-profiles name, asked for
            # rimed's own object, answers only when rimed owns that name. Without this the subtest below would also
            # pass through the `rime tier` fallback, so it would not show which path the app took.
            owner = machine.succeed(user("busctl --system call org.freedesktop.UPower.PowerProfiles /org/rimeos/Rimed1 "
                                         "org.freedesktop.DBus.Properties Get ss org.rimeos.Rimed1.Power Tier")).strip()
            print("owner check:", owner)
            assert owner.startswith('v s "'), "rimed does not answer on the power-profiles name: " + owner
            # Show the Power page first: PowerProfileService picks its backend (rimed's power-profiles API) with one
            # owner check when the page appears, as it would before anyone could click.
            machine.succeed(user(f"{qs} -p {app} ipc call settings open power"))
            machine.sleep(2)
            answer = machine.succeed(user(f"{qs} -p {app} ipc call settings power performance"))
            print("app answered:", answer)
            machine.sleep(1)
            print(machine.succeed("journalctl -b --no-pager -o cat -u rimed.service -u polkit.service | tail -n 8"))
            machine.wait_until_succeeds("busctl get-property org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Power Tier | grep -F performance", timeout=30)
            assert machine.succeed(f"{PPCTL} get").strip() == "performance"
            gnome_reads = machine.succeed(user(f"gdbus call --system --dest {PPD} --object-path /org/freedesktop/UPower/PowerProfiles "
                                              f"--method org.freedesktop.DBus.Properties.Get {PPD} ActiveProfile"))
            assert "performance" in gnome_reads, gnome_reads

        with subtest("no Rime Shell in the GNOME session"):
            procs = machine.succeed("ps -u rime -o args=")
            assert "/shell.qml" not in procs, procs
            machine.fail("pgrep -u rime -x hyprland")
      '';
    };

  gnome = gnomeTest { name = "rime-settings-app-gnome"; };
in
{
  settings-app-gnome = gnome // {
    mutants = {
      wrong-app-id = gnomeTest {
        name = "rime-settings-app-gnome-wrong-app-id";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-shell = prev.rime.rime-shell.overrideAttrs (o: {
              # The package's own installCheck asserts the right id; the mutant is for the VM check alone to catch.
              doInstallCheck = false;
              postInstall = (o.postInstall or "") + ''
                # $out/bin/rime-settings is makeWrapper's; the script it runs is under share/.
                sed -i 's/QS_APP_ID=org.rimeos.Settings/QS_APP_ID=org.rimeos.Wrong/' $out/share/rime-shell/bin/rime-settings
                grep -F QS_APP_ID=org.rimeos.Wrong $out/share/rime-shell/bin/rime-settings
              '';
            });
          };
        };
      };
      wrong-shell-version = gnomeTest {
        name = "rime-settings-app-gnome-wrong-shell-version";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-gnome-extension = prev.rime.rime-gnome-extension.override { shellVersionOverride = "49"; };
          };
        };
      };
    };
  };
}
