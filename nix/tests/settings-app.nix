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
#                          - `rime mode set gaming` / `daily` flips the extension's Gaming toggle (its report);
#                          - Power -> performance in the app (its IPC test hook runs the Power page's own handler)
#                            reaches rimed's Power.Tier, `powerprofilesctl get` and the power-profiles property GNOME
#                            reads from inside the session;
#                          - with polkit refusing switch-profile (a test rule, while /run/rime-test-deny-power exists)
#                            Power -> performance leaves rimed's tier alone, and the Power page shows why ("Not
#                            allowed: ...") with the tier in force selected again; screenshot; the next press lands
#                            and clears the reason.
#
#   settings-app-kde     Plasma enabled directly, logged in through rime-greet, then:
#                          - the applet org.rimeos.quicksettings draws (its report) and is in the user's
#                            plasma-org.kde.plasma.desktop-appletsrc (the system tray adopted it);
#                          - `rime mode set gaming` / `daily` flips its Gaming state;
#                          - Rime Settings opens one window KWin knows as org.rimeos.Settings (resourceClass and
#                            desktopFileName, read by a KWin script that reports through the test sink);
#                          - Power -> performance in the app reaches rimed and powerdevil's currentProfile;
#                          - powerdevil's setProfile (what Plasma's battery applet calls) changes rimed's tier;
#                          - Update offered (/run/rime-update/state): pressing it (the sink's Act signal runs the
#                            applet's own click handler) raises Plasma's polkit agent; Escape refuses (rime-update
#                            .service not started), the test admin's password starts rime-update.service.
#
#   settings-app-cosmic  COSMIC enabled directly, logged in through rime-greet, then:
#                          - COSMIC Initial Setup opens and the screen reader stays OFF (no orca, a11y bus
#                            ScreenReaderEnabled false, the dconf key not true); turning it on starts orca (control);
#                          - cosmic-applet-rime runs in cosmic-panel and follows `rime mode set`;
#                          - Rime Settings opens; Power -> performance reaches rimed.
#
# Every check also asserts that no Rime Shell process runs in the desktop's session.
#
# Mutants (passthru.mutants, built by hand; each must FAIL):
#   gnome  wrong-app-id             rime-settings starts the app with QS_APP_ID=org.rimeos.Wrong, so GNOME cannot
#                                   match the window to org.rimeos.Settings.desktop.
#          wrong-shell-version      metadata.json names shell-version 49 on GNOME 50: GNOME does not load it.
#          power-refusal-silent     PowerProfileService writes through Quickshell's PowerProfiles.profile again (it
#                                   only logs a refusal): the refused pick stays and no reason shows.
#   kde    tray-not-adopted         the applet is not a notification-area applet, nor enabled by default.
#          wrong-app-id             as GNOME's.
#          apply-not-interactive    rimed checks Apply without AllowUserInteraction: no prompt, Apply refused.
#   cosmic panel-default-not-found  cosmic-panel's default without Rime's entry: the applet never starts.
#          screen-reader-unpatched  COSMIC Initial Setup as nixpkgs ships it (desktops.md §12.9).
#
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

    # Act(s) (called by the test) makes the sink emit the signal Act(s): Plasma's applet presses the control it names
    # (nix/integrations/kde main.qml, which listens only to this name's owner).
    XML = """
    <node><interface name="org.rimeos.QuickSettingsTest">
      <method name="Report">
        <arg type="s" direction="in"/><arg type="s" direction="in"/>
      </method>
      <method name="Act">
        <arg type="s" direction="in"/>
      </method>
      <signal name="Act"><arg type="s"/></signal>
    </interface></node>"""
    out = sys.argv[1]


    def call(conn, sender, path, iface, method, params, inv):
        if method == "Act":
            (action,) = params.unpack()
            conn.emit_signal(None, path, iface, "Act", GLib.Variant("(s)", (action,)))
            inv.return_value(None)
            return
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

        # A refusal on demand: while /run/rime-test-deny-power exists polkit says no to switching the power profile
        # (rimed asks polkit for every ActiveProfile write), as it would for someone not signed in at the seat.
        security.polkit.extraConfig = ''
          polkit.addRule(function (action, subject) {
            if (action.id != "org.freedesktop.UPower.PowerProfiles.switch-profile") return undefined;
            try { polkit.spawn(["${pkgs.coreutils}/bin/test", "-e", "/run/rime-test-deny-power"]); }
            catch (e) { return undefined; }
            return polkit.Result.NO;
          });
        '';
      };

      testScript = ''
        import json
        import shlex
        import time

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

        with subtest("a tier rimed refuses: the Power page says why and shows the tier in force again"):
            # PowerProfileService writes ActiveProfile itself (busctl) because Quickshell's own write only logs a
            # refusal: the page then showed the refused pick, or snapped back, and never said why.
            TIER = "busctl get-property org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Power Tier"
            def page():
                # quickshell prints a locale warning on stdout first: the answer is the JSON line.
                out = machine.succeed(user(f"{qs} -p {app} ipc call settings powerState"))
                return json.loads([l for l in out.splitlines() if l.startswith("{")][-1])
            machine.succeed("rime tier balanced")
            machine.wait_until_succeeds(user(f"{qs} -p {app} ipc call settings powerState") + " | grep -F '\"tier\":\"balanced\"'", timeout=30)
            machine.succeed("touch /run/rime-test-deny-power")
            t0 = time.monotonic()
            print("app answered:", machine.succeed(user(f"{qs} -p {app} ipc call settings power performance")))
            machine.wait_until_succeeds(user(f"{qs} -p {app} ipc call settings powerState") + " | grep -F 'Not allowed'", timeout=10)
            shown = page()
            print(f"refused after {time.monotonic() - t0:.1f} s (1 s polling); the page shows:", shown)
            machine.screenshot("settings-power-refused")
            assert shown["tier"] == "balanced", f"the page still shows the refused pick: {shown}"
            assert machine.succeed(TIER).strip() == 's "balanced"', machine.succeed(TIER)
            print(machine.succeed("journalctl -b --no-pager -o cat -u rimed.service -u polkit.service | tail -n 6"))

            machine.succeed("rm /run/rime-test-deny-power")
            print("app answered:", machine.succeed(user(f"{qs} -p {app} ipc call settings power power-saver")))
            machine.wait_until_succeeds(f"{TIER} | grep -F power-saver", timeout=30)
            machine.wait_until_succeeds(user(f"{qs} -p {app} ipc call settings powerState") + " | grep -F '\"tier\":\"power-saver\"'", timeout=10)
            assert page()["reason"] == "", f"the reason outlived the next press: {page()}"

        with subtest("no Rime Shell in the GNOME session"):
            procs = machine.succeed("ps -u rime -o args=")
            assert "/shell.qml" not in procs, procs
            machine.fail("pgrep -u rime -x hyprland")
      '';
    };

  # The test machine of the KDE and COSMIC checks: the desktop enabled directly (as gnomeTest), the greeter
  # preselecting its session, rimed's update checker off (the test writes the checker's state itself).
  desktopMachine = { id, session, overlay, extra ? { } }:
    { lib, pkgs, ... }: {
      imports = [ self.nixosModules.default extra ];
      nixpkgs.overlays = lib.mkAfter (lib.optional (overlay != null) overlay);

      rime.services.enable = true;
      rime.desktop.enable = true;
      rime.desktops.${id}.enable = true;
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
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      # As desktops-install.nix: Plasma reads its own environment scripts and gets its normal Qt Quick back.
      environment.etc."xdg/plasma-workspace/env/rime-test-qtquick.sh".text = "unset QT_QUICK_BACKEND\n";
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;
      # No release server here: the checker would overwrite the state the test writes (OnBootSec 2 min).
      systemd.timers.rime-update-check.enable = lib.mkForce false;

      systemd.tmpfiles.rules = [ "f+ /var/lib/rime-greet/last-session 0644 greetd greetd - ${session}" ];
      environment.systemPackages = [ pkgs.gtk3 ];
    };

  # Python shared by the KDE and COSMIC scripts: log in through rime-greet, the sink, Rime Settings and its Power
  # page (the same steps as gnomeTest's).
  commonScript = { process }: ''
    import json
    import shlex

    PPCTL = "${pkgs.power-profiles-daemon}/bin/powerprofilesctl"
    TIER = "busctl get-property org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Power Tier"

    # The compositor's socket name is read, not assumed: Plasma's is wayland-1 (measured, settings-app-kde).
    def user(cmd):
        env = ("export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
               "WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1); ")
        return "su - rime -c " + shlex.quote(env + cmd)

    def running(proc):
        return f"pgrep -u rime -f -- '^([^ ]*/)?\\.?{proc}(-wrapped)?( |$)'"

    def login():
        machine.wait_for_unit("multi-user.target")
        machine.wait_for_unit("greetd.service")
        machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
        machine.sleep(8)
        machine.screenshot("greeter")
        machine.send_chars("rime\n")
        machine.sleep(2)
        machine.send_chars("rime\n")
        machine.wait_until_succeeds(running("${process}"), timeout=300)

    def start_sink():
        machine.succeed(user("systemd-run --user --unit rime-qs-sink ${sink} /tmp/qs-reports.txt"))
        machine.wait_until_succeeds(user("busctl --user status org.rimeos.QuickSettingsTest"), timeout=30)

    def last_report(desktop):
        return machine.succeed(f"grep '^{desktop} ' /tmp/qs-reports.txt | tail -n1").strip()

    def gaming_follows(desktop):
        """rime mode set gaming / daily: the integration's Gaming toggle follows rimed (its report)."""
        machine.wait_for_unit("rimed.service")
        machine.succeed("rime mode set gaming")
        machine.wait_until_succeeds(f"grep '^{desktop} ' /tmp/qs-reports.txt | tail -n1 | grep -F '\"gaming\":{{\"checked\":true'", timeout=60)
        print("after gaming:", last_report(desktop))
        machine.succeed("rime mode set daily")
        machine.wait_until_succeeds(f"grep '^{desktop} ' /tmp/qs-reports.txt | tail -n1 | grep -F '\"gaming\":{{\"checked\":false'", timeout=60)
        print("after daily:", last_report(desktop))

    def open_settings():
        """Rime Settings from its desktop file, in a user scope (as a desktop starts apps); returns (qs, app)."""
        machine.succeed(user("systemd-run --user --scope --unit rime-settings-launch "
                             "gtk-launch org.rimeos.Settings >/tmp/settings-launch.log 2>&1"))
        machine.wait_until_succeeds("pgrep -u rime -f 'rime-shell/settings.qml'", timeout=60)
        fields = machine.succeed("pgrep -u rime -a -f 'rime-shell/settings.qml' | head -n1").split()
        return fields[1], fields[-1]

    def app_power(qs, app, tier):
        """The Power page's own handler (the IPC hook), after the page is shown (gnomeTest's sequence)."""
        owner = machine.succeed(user("busctl --system call org.freedesktop.UPower.PowerProfiles /org/rimeos/Rimed1 "
                                     "org.freedesktop.DBus.Properties Get ss org.rimeos.Rimed1.Power Tier")).strip()
        print("owner check:", owner)
        assert owner.startswith('v s "'), "rimed does not answer on the power-profiles name: " + owner
        machine.succeed(user(f"{qs} -p {app} ipc call settings open power"))
        machine.sleep(2)
        print("app answered:", machine.succeed(user(f"{qs} -p {app} ipc call settings power {tier}")))
        machine.wait_until_succeeds(f"{TIER} | grep -F {tier}", timeout=30)
        assert machine.succeed(f"{PPCTL} get").strip() == tier

    def no_rime_shell():
        procs = machine.succeed("ps -u rime -o args=")
        assert "/shell.qml" not in procs, procs
        machine.fail("pgrep -u rime -x hyprland")
  '';

  # KWin answers no window list over D-Bus; a KWin script (loaded through org.kde.kwin.Scripting) sends one to the
  # sink as a "kwin" report. Test only.
  kwinWindows = pkgs.writeText "rime-kwin-windows.js" ''
    const list = [];
    for (const w of workspace.windowList()) {
        list.push({ resourceClass: String(w.resourceClass), desktopFileName: String(w.desktopFileName),
                    caption: String(w.caption), normal: Boolean(w.normalWindow), dialog: Boolean(w.dialog) });
    }
    callDBus("org.rimeos.QuickSettingsTest", "/org/rimeos/QuickSettingsTest", "org.rimeos.QuickSettingsTest",
             "Report", "kwin", JSON.stringify(list));
  '';

  kdeTest = { name, overlay ? null, extra ? { } }:
    pkgs.testers.runNixOSTest {
      inherit name;
      node.pkgsReadOnly = false;
      nodes.machine = desktopMachine { id = "kde"; session = "plasma"; inherit overlay extra; };

      testScript = commonScript { process = "plasmashell"; } + ''

        APPLET = "org.rimeos.quicksettings"
        PD = ("busctl --user call org.kde.Solid.PowerManagement /org/kde/Solid/PowerManagement/Actions/PowerProfile "
              "org.kde.Solid.PowerManagement.Actions.PowerProfile")
        windows_runs = [0]

        def kwin_windows():
            """The window list, from inside KWin (kwinWindows), as a list of dicts."""
            windows_runs[0] += 1
            n = windows_runs[0]
            before = machine.succeed("grep -c '^kwin ' /tmp/qs-reports.txt || true").strip() or "0"
            sid = machine.succeed(user(f"busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss ${kwinWindows} rime-windows-{n}")).split()[1]
            machine.succeed(user(f"busctl --user call org.kde.KWin /Scripting/Script{sid} org.kde.kwin.Script run"))
            machine.wait_until_succeeds(f"test $(grep -c '^kwin ' /tmp/qs-reports.txt) -gt {before}", timeout=30)
            return json.loads(last_report("kwin").split(" ", 1)[1])

        def apply_state():
            return machine.succeed("systemctl show rime-update.service -p InvocationID -p ActiveState -p ExecMainStartTimestampMonotonic")

        with subtest("logs in to Plasma through rime-greet"):
            machine.succeed("grep -l '^Name=Plasma' /etc/rime/wayland-sessions/*.desktop")
            login()
            machine.wait_until_succeeds("ls /run/user/1000/wayland-? >/dev/null", timeout=120)
            print(machine.succeed("ls -la /run/user/1000"))
            machine.sleep(30)  # the panel, the tray, autostart
            print(machine.succeed("ps -u rime -o pid,args --no-headers | cut -c1-200"))
            machine.screenshot("plasma")

        with subtest("the tray loaded org.rimeos.quicksettings: its report reaches the test sink"):
            start_sink()
            machine.wait_until_succeeds("grep '^kde ' /tmp/qs-reports.txt", timeout=60)
            state = json.loads(last_report("kde").split(" ", 1)[1])
            print("report:", state)
            assert "gaming" in state and "update" in state, state
            # The tray's own record of what it hosts (plasmoidregistry adds an EnabledByDefault applet to extraItems).
            rc = machine.succeed("cat /home/rime/.config/plasma-org.kde.plasma.desktop-appletsrc")
            print(rc)
            assert APPLET in rc, "the system tray's configuration does not name " + APPLET

        with subtest("the applet follows rimed: rime mode set gaming flips its Gaming switch"):
            machine.wait_until_succeeds("grep '^kde .*\"reactive\":true' /tmp/qs-reports.txt", timeout=60)
            gaming_follows("kde")

        with subtest("Rime Settings opens from its desktop file as org.rimeos.Settings"):
            qs, app = open_settings()
            for _ in range(30):
                wins = kwin_windows()
                mine = [w for w in wins if w["resourceClass"] == "org.rimeos.Settings"]
                if mine:
                    break
                machine.sleep(2)
            print(wins)
            assert len(mine) == 1, f"expected one org.rimeos.Settings window: {wins}"
            assert mine[0]["desktopFileName"] == "org.rimeos.Settings", mine[0]
            machine.sleep(3)
            machine.screenshot("settings-app")

        with subtest("Power -> performance in the app reaches rimed's tier and powerdevil's reading"):
            machine.succeed("rime tier balanced")
            app_power(qs, app, "performance")
            machine.wait_until_succeeds(user(f"{PD} currentProfile") + " | grep -F performance", timeout=60)

        with subtest("Plasma writes a profile: powerdevil's setProfile (what the battery applet calls) moves rimed's tier"):
            # powerprofilescontrol.cpp setProfile -> powerdevil (a user service) -> ActiveProfile on the system bus.
            machine.succeed(user(f"{PD} setProfile s power-saver"))
            machine.wait_until_succeeds(f"{TIER} | grep -F power-saver", timeout=30)
            assert machine.succeed(f"{PPCTL} get").strip() == "power-saver"
            assert "* power-saver" in machine.succeed("rime tier 2>&1"), machine.succeed("rime tier 2>&1")
            machine.succeed(user(f"{PD} setProfile s balanced"))
            machine.wait_until_succeeds(f"{TIER} | grep -F balanced", timeout=30)
            print(machine.succeed("journalctl -b --no-pager -o cat -u rimed.service -u polkit.service | tail -n 12"))

        with subtest("an update is on offer: the applet shows it"):
            machine.succeed("install -d /run/rime-update",
                            "printf 'state=available\\nrelease=2026.11.20\\nrunning=2026.11.10\\n' > /run/rime-update/state.tmp",
                            "mv /run/rime-update/state.tmp /run/rime-update/state")
            machine.wait_until_succeeds("busctl get-property org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Updates State | grep -F available", timeout=30)
            machine.wait_until_succeeds("grep '^kde ' /tmp/qs-reports.txt | tail -n1 | grep -F '\"visible\":true'", timeout=60)
            print("report:", last_report("kde"))
            machine.wait_until_succeeds(running("polkit-kde-authentication-agent-1"), timeout=60)

        def press_update_and_wait_for_prompt(shot):
            machine.succeed(user("busctl --user call org.rimeos.QuickSettingsTest /org/rimeos/QuickSettingsTest org.rimeos.QuickSettingsTest Act s update"))
            for _ in range(30):
                wins = kwin_windows()
                prompt = [w for w in wins if "polkit" in w["resourceClass"] or "polkit" in w["desktopFileName"]]
                if prompt:
                    break
                machine.sleep(1)
            print(wins)
            assert prompt, f"no polkit prompt after pressing Update: {wins}"
            machine.sleep(2)
            machine.screenshot(shot)
            return prompt

        with subtest("Update pressed, the prompt cancelled: rimed refuses, nothing starts"):
            before = apply_state()
            press_update_and_wait_for_prompt("polkit-prompt-cancel")
            machine.send_key("esc")
            machine.wait_until_succeeds("journalctl -b _UID=1000 --no-pager -o cat | grep -F 'rime: Apply refused'", timeout=60)
            print(machine.succeed("journalctl -b _UID=1000 --no-pager -o cat | grep -F 'rime: Apply refused'"))
            machine.sleep(2)
            after = apply_state()
            print(before, after)
            assert before == after, f"rime-update.service changed after a cancelled prompt: {before} -> {after}"

        with subtest("Update pressed, the administrator's password typed: rimed starts rime-update.service"):
            before = apply_state()
            press_update_and_wait_for_prompt("polkit-prompt")
            machine.send_chars("rime\n")
            machine.wait_until_succeeds("journalctl -b --no-pager -o cat -u rime-update.service | grep .", timeout=60)
            after = apply_state()
            print(before, after)
            assert before != after, "rime-update.service was not started"
            print(machine.succeed("journalctl -b --no-pager -o cat -u rime-update.service | head -n 20"))
            machine.succeed("systemctl stop rime-update.service")

        with subtest("no Rime Shell in the Plasma session"):
            no_rime_shell()
      '';
    };

  cosmicTest = { name, overlay ? null, extra ? { } }:
    pkgs.testers.runNixOSTest {
      inherit name;
      node.pkgsReadOnly = false;
      nodes.machine = desktopMachine { id = "cosmic"; session = "cosmic"; inherit overlay extra; };

      testScript = commonScript { process = "cosmic-comp"; } + ''

        A11Y = "busctl --user get-property org.a11y.Bus /org/a11y/bus org.a11y.Status ScreenReaderEnabled"

        with subtest("logs in to COSMIC through rime-greet; COSMIC Initial Setup opens"):
            machine.succeed("grep -l '^Name=COSMIC' /etc/rime/wayland-sessions/*.desktop")
            login()
            machine.wait_until_succeeds("pgrep -u rime -f cosmic-initial-setup", timeout=180)
            machine.sleep(30)  # the panel, its applets, Initial Setup's accessibility subscription
            print(machine.succeed("ps -u rime -o pid,args --no-headers | cut -c1-200"))
            machine.screenshot("cosmic-initial-setup")

        with subtest("first login: no screen reader (desktops.md §12.9)"):
            procs = machine.succeed("ps -u rime -o args=")
            assert "orca" not in procs, "orca runs at the first COSMIC login:\n" + procs
            reader = machine.succeed(user(A11Y)).strip()
            print("ScreenReaderEnabled:", reader)
            assert reader == "b false", reader
            key = machine.succeed(user("dconf read /org/gnome/desktop/a11y/applications/screen-reader-enabled")).strip()
            print("dconf screen-reader-enabled:", repr(key))
            assert key != "true", key

        with subtest("the screen reader still works when asked for: on starts orca, off stops it"):
            machine.succeed(user("busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status ScreenReaderEnabled b true"))
            machine.wait_until_succeeds("pgrep -u rime -f '/bin/[.]?orca'", timeout=60)
            machine.succeed(user("busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status ScreenReaderEnabled b false"))
            machine.wait_until_fails("pgrep -u rime -f '/bin/[.]?orca'", timeout=60)

        with subtest("cosmic-panel runs Rime's applet; it reports and follows rimed"):
            machine.wait_until_succeeds("pgrep -u rime -f cosmic-applet-rime", timeout=120)
            print(machine.succeed("pgrep -u rime -a -f cosmic-applet-rime"))
            start_sink()
            gaming_follows("cosmic")
            state = json.loads(last_report("cosmic").split(" ", 1)[1])
            assert state["gaming"]["reactive"] is True, state

        with subtest("the panel with Rime's applet (Initial Setup closed), and Rime Settings"):
            for pid in machine.succeed("pgrep -u rime -f cosmic-initial-setup || true").split():
                machine.succeed(f"kill {pid}")
            machine.sleep(5)
            machine.screenshot("cosmic-panel")
            qs, app = open_settings()
            machine.sleep(5)
            machine.screenshot("settings-app")

        with subtest("Power -> performance in the app reaches rimed's tier"):
            machine.succeed("rime tier balanced")
            app_power(qs, app, "performance")

        with subtest("no Rime Shell in the COSMIC session"):
            no_rime_shell()
      '';
    };

  # The rimed of a mutant (no tests: the mutant is the source, not its suite), as desktops-install.nix.
  rimedMutant = rimed: mname: file: from: to:
    rimed.overrideAttrs (o: {
      pname = "rimed-mutant-${mname}";
      doCheck = false;
      postPatch = (o.postPatch or "") + ''
        grep -F ${pkgs.lib.escapeShellArg from} ${file} >/dev/null
        substituteInPlace ${file} --replace-fail ${pkgs.lib.escapeShellArg from} ${pkgs.lib.escapeShellArg to}
      '';
    });

  wrongAppId = _final: prev: {
    rime = prev.rime // {
      rime-shell = prev.rime.rime-shell.overrideAttrs (o: {
        doInstallCheck = false;
        postInstall = (o.postInstall or "") + ''
          sed -i 's/QS_APP_ID=org.rimeos.Settings/QS_APP_ID=org.rimeos.Wrong/' $out/share/rime-shell/bin/rime-settings
          grep -F QS_APP_ID=org.rimeos.Wrong $out/share/rime-shell/bin/rime-settings
        '';
      });
    };
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
      power-refusal-silent = gnomeTest {
        name = "rime-settings-app-gnome-power-refusal-silent";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-shell = prev.rime.rime-shell.overrideAttrs (o: {
              doInstallCheck = false;
              postInstall = (o.postInstall or "") + ''
                f=$out/share/rime-shell/src/services/system/PowerProfileService.qml
                grep -F 'root._ppdWrite(id)' $f
                sed -i -E 's/^( *)root\._ppdWrite\(id\)$/\1PowerProfiles.profile = (id === "performance" ? PowerProfile.Performance : id === "power-saver" ? PowerProfile.PowerSaver : PowerProfile.Balanced)/' $f
                ! grep -F 'root._ppdWrite(id)' $f
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

  settings-app-kde = kdeTest { name = "rime-settings-app-kde"; } // {
    mutants = {
      # The tray does not adopt the applet (not a notification-area applet, not enabled by default).
      tray-not-adopted = kdeTest {
        name = "rime-settings-app-kde-tray-not-adopted";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-plasma-applet = prev.rime.rime-plasma-applet.overrideAttrs (o: {
              doInstallCheck = false;
              postInstall = (o.postInstall or "") + ''
                m=$out/share/plasma/plasmoids/org.rimeos.quicksettings/metadata.json
                ${pkgs.jq}/bin/jq '.KPlugin.EnabledByDefault = false | .["X-Plasma-NotificationArea"] = "false"' $m > m.json
                mv m.json $m
              '';
            });
          };
        };
      };
      wrong-app-id = kdeTest { name = "rime-settings-app-kde-wrong-app-id"; overlay = wrongAppId; };
      # The KDE defect this check was written for: Apply checked without AllowUserInteraction, as Plasma's call
      # (which cannot set the message flag) asks. No prompt; Apply refused.
      apply-not-interactive = kdeTest {
        name = "rime-settings-app-kde-apply-not-interactive";
        extra = { pkgs, ... }: {
          rime.services.rimed.package = rimedMutant pkgs.rime.rimed "apply-not-interactive" "rimed/rimed/src/updates.rs"
            "authorize_interactive(conn, &hdr, ACTION_APPLY)" "authorize(conn, &hdr, ACTION_APPLY)";
        };
      };
    };
  };

  settings-app-cosmic = cosmicTest { name = "rime-settings-app-cosmic"; } // {
    mutants = {
      # cosmic-panel's default without Rime's entry: the applet never starts.
      panel-default-not-found = cosmicTest {
        name = "rime-settings-app-cosmic-panel-default-not-found";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-cosmic-applet = prev.rime.rime-cosmic-applet // {
              panelDefaults = pkgs.runCommand "rime-cosmic-panel-defaults-mutant" { } ''
                cp -r ${prev.rime.rime-cosmic-applet.panelDefaults} $out
                chmod -R u+w $out
                sed -i 's/"org.rimeos.CosmicAppletRime", //' $out/share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings
                ! grep -F org.rimeos.CosmicAppletRime $out/share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings
              '';
            };
          };
        };
      };
      # COSMIC Initial Setup as nixpkgs ships it: it turns the screen reader on.
      screen-reader-unpatched = cosmicTest {
        name = "rime-settings-app-cosmic-screen-reader-unpatched";
        overlay = _final: prev: { cosmic-initial-setup = prev.cosmic-initial-setup.passthru.unpatched; };
      };
    };
  };
}
