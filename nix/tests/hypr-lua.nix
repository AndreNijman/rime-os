# hypr-lua: Hyprland is driven in Lua only, and Rime's display and input settings are checked before Hyprland sees them.
# The product session (greetd initial_session -> start-hyprland, Rime's Lua config seeded as firstrun does, the Shell
# started by session.lua), then, as the session user:
#
#   start     `hyprctl configerrors` is empty and the Shell's config probe raised nothing;
#   shell     the Shell's live writes land through `hyprctl eval`: window rounding (syncWindowCorners, read back against
#             the value the Shell logged), the kept window layout (settings.json windowLayout = master), focus mode's
#             gaps (0/6 and back to 5/10) and the accent border; after `hyprctl reload` the accent is pushed again;
#   display   rime-display-apply with scale 1.1 on the 1280x800 output: the model is snapped to 1.0666667 (128/120,
#             Hyprland's own search, Monitor.cpp), `hyprctl -j monitors` reports it, and Hyprland logged no "Invalid
#             scale" (so it showed no notification). Control: the same 1.1 pushed raw through eval DOES log it;
#   input     rime-input-apply with layout gb lands in `hyprctl -j devices`; then layout xx is refused with a note,
#             the devices and input.lua keep gb. Control: an input.lua with kb_layout "xx" written by hand makes
#             Hyprland fall back to us, so the devices check can tell;
#   probe     a hyprland.lua without Rime's modules, reloaded: the Shell logs "Hyprland config problem
#             (modules-missing)" and names rime-hypr-migrate; Rime's file back: "loaded again";
#   dispatch  rime-project-windows moves a real window (foot) to workspace 7 without following it (follow =
#             false), rime-agent-focus's hyprland_focus brings it to the front, and rime-project-windows
#             switches back: every call an hl.dsp form, each checked against `hyprctl -j` state;
#   firstrun  the per-login refresh of ~/.config/hypr/rime/ puts a changed session.lua back (control: it ran)
#             and leaves matugen's colors.lua byte for byte;
#   end       `hyprctl configerrors` is empty again, and Hyprland logged no XCursor shape fallback (session.lua
#             names the Adwaita cursor theme; without it every shape fell back, 951 lines in one session).
#
# Results (every reply the asserts read) are in the VM's /tmp/hl-out, copied out as hl-out.tar,
# with screenshots (start, display-snapped, input-refused, probe-modules-missing).png.
#   nix build path:.#checks.x86_64-linux.hypr-lua -L
{ pkgs, self }:

let
  user = "alice";
  shellDir = "/run/current-system/sw/share/rime-shell";
  libexec = "/run/current-system/sw/libexec";
in
pkgs.testers.runNixOSTest {
  name = "rime-hypr-lua";
  node.pkgsReadOnly = false;

  nodes.machine = { lib, config, ... }: {
    imports = [ self.nixosModules.default ];
    rime.desktop.enable = true;

    users.users.${user} = {
      isNormalUser = true;
      uid = 1000;
      initialPassword = "rime-test";
      extraGroups = [ "wheel" "video" "audio" ];
    };

    # As shell-kept.nix: straight into the product session, Rime's config seeded the way firstrun seeds it.
    services.greetd.settings.initial_session = {
      user = user;
      command = "${config.programs.hyprland.package}/bin/start-hyprland";
    };
    systemd.services.rime-test-seed-hypr = {
      wantedBy = [ "greetd.service" ];
      before = [ "greetd.service" ];
      serviceConfig.Type = "oneshot";
      script = ''
        h=/home/${user}
        install -d -o ${user} -g users $h/.config $h/.config/hypr $h/.local $h/.local/share $h/.local/share/hyprland \
          $h/.config/rime-shell $h/.config/rime-shell/src $h/.config/rime-shell/src/user_data
        cp -r /run/current-system/sw/share/rime/hypr/. $h/.config/hypr/
        grep -rl '@KB_' $h/.config/hypr | xargs -r sed -i -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g'
        echo 'hl.config({ debug = { disable_logs = false } })' >> $h/.config/hypr/hyprland.lua
        cp $h/.config/hypr/hyprland.lua $h/.config/hypr/hyprland.lua.rime
        # The kept window layout the Shell must put back with `hyprctl eval` at start.
        echo '{ "windowLayout": "master" }' > $h/.config/rime-shell/src/user_data/settings.json
        chown -R ${user}:users $h/.config $h/.local
        chmod -R u+w $h/.config/hypr   # cp -r keeps the store's read-only modes; firstrun mkdirs and renders, so its tree is writable
      '';
    };

    environment.sessionVariables = {
      RIME_PACING_LOG = "1";
      RIME_RELEASE_JSON = "/nonexistent";
      RIME_RELEASE_OPENER = "/bin/false";
    };
    environment.systemPackages = [ pkgs.jq pkgs.foot ];

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
    };
    hardware.graphics.enable = true;
    systemd.services.rime-flathub-setup.enable = lib.mkForce false;
  };

  testScript = ''
    import json, re, shlex, time

    # Deliberately no XDG_CURRENT_DESKTOP: the tools must find Hyprland from
    # HYPRLAND_INSTANCE_SIGNATURE, because pgrep -x Hyprland cannot see NixOS's
    # wrapped binary (.Hyprland-wrapped). Run 3 failed exactly there.
    def u(cmd):
        return "su - ${user} -c " + shlex.quote(
            "export XDG_RUNTIME_DIR=/run/user/1000; "
            "export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -n1); "
            "export WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1); " + cmd)

    def run(name, cmd):
        """Run as the session user, keep stdout+stderr in /tmp/hl-out/<name>, return (rc, text)."""
        rc, out = machine.execute(u(cmd + " 2>&1"))
        machine.succeed("mkdir -p /tmp/hl-out && cat > /tmp/hl-out/" + name + " <<'HLEOF'\n" + out + "\nrc=" + str(rc) + "\nHLEOF")
        print("── " + name + " (rc " + str(rc) + ")\n" + out)
        return rc, out

    def j(name, cmd):
        rc, out = run(name, cmd)
        assert rc == 0, name + ": exit " + str(rc)
        return json.loads(out)

    def configerrors(name):
        errs = [l for l in j(name, "hyprctl -j configerrors") if l.strip()]
        assert errs == [], name + ": configerrors " + repr(errs)

    def option(name, opt):
        return j(name, "hyprctl -j getoption " + opt)

    def first_num(o):
        if "int" in o:
            return int(o["int"])
        v = o.get("css", o.get("custom", ""))
        return int(str(v).split()[0])

    def wait_gaps(name, want_in, want_out):
        for _ in range(40):
            gin = first_num(option(name + "-in", "general:gaps_in"))
            gout = first_num(option(name + "-out", "general:gaps_out"))
            if (gin, gout) == (want_in, want_out):
                return
            time.sleep(0.5)
        raise AssertionError(name + ": gaps " + str((gin, gout)) + ", want " + str((want_in, want_out)))

    def hyprlog():
        return machine.succeed(u("cat /run/user/1000/hypr/*/hyprland.log"))

    def shell_log():
        return machine.execute(u("qs -c ${shellDir} log 2>&1"))[1]

    def dump():
        print(machine.execute("ps -eo pid,ppid,etimes,args | grep -v ' \\[' | tail -n 60; "
                              "journalctl -b --no-pager -o short-monotonic | tail -n 120")[1])
        print(machine.execute(u("hyprctl configerrors; tail -n 200 /run/user/1000/hypr/*/hyprland.log; "
                                "qs -c ${shellDir} log 2>&1 | tail -n 200"))[1])

    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
    try:
        machine.wait_until_succeeds(u("qs -c ${shellDir} ipc show | grep -q dashboard-home"), timeout=180)
        machine.sleep(10)

        with subtest("start: no config errors, the Shell's config probe raised nothing"):
            configerrors("start-configerrors")
            machine.screenshot("start")
            log = shell_log()
            machine.succeed("cat > /tmp/hl-out/start-shell.log <<'HLEOF'\n" + log + "\nHLEOF")
            assert "Hyprland config problem" not in log, "the probe complained on Rime's own config"

        with subtest("shell: live writes land through hyprctl eval"):
            layout = option("shell-layout", "general:layout")
            assert layout.get("str") == "master", "kept layout not applied: " + repr(layout)
            # The Shell writes this line once it has applied its settings, a few
            # seconds after it answers IPC: on a slow runner the first read can be early.
            want = []
            for _ in range(60):
                want = [l for l in shell_log().splitlines() if "Rime window corners:" in l]
                if want:
                    break
                time.sleep(1)
            assert want, "the Shell never synced the window corners (waited 60 s)"
            r = int(want[-1].rsplit("rounding", 1)[1].strip())
            rounding = option("shell-rounding", "decoration:rounding")
            assert rounding.get("int") == r, "rounding " + repr(rounding) + ", the Shell wrote " + str(r)
            border = option("shell-border", "general:col.active_border")
            assert "8d748c" not in json.dumps(border), "the accent never replaced the config's gradient: " + repr(border)
            wait_gaps("shell-gaps-start", 5, 10)
            # Focus Mode before the dashboard has ever been shown (rime-shell's
            # FocusMode singleton; it used to live in QuickSettings, built with the
            # dashboard, and the first presses did nothing).
            run("shell-focus-on", "qs -c ${shellDir} ipc call focus-toggle toggle")
            wait_gaps("shell-gaps-focus", 0, 6)
            # The next press at once, no waiting out the 250 ms settle window: a
            # press inside it is queued now, not dropped (it once was:
            # vm-run10-locked.log).
            run("shell-focus-off", "qs -c ${shellDir} ipc call focus-toggle toggle")
            wait_gaps("shell-gaps-restored", 5, 10)
            # Two presses back to back (the second lands while the first's gaps
            # read is out): they cancel out, gaps stay the user's, Focus Mode off.
            run("shell-focus-double", "qs -c ${shellDir} ipc call focus-toggle toggle; "
                "qs -c ${shellDir} ipc call focus-toggle toggle")
            # Exactly "off": not "on", and not "off queued" (quickshell may print a locale warning too).
            machine.wait_until_succeeds(u("qs -c ${shellDir} ipc call focus-toggle state 2>&1 | grep -qx off"), timeout=10)
            wait_gaps("shell-gaps-double", 5, 10)
            # A reload drops runtime values; the Shell pushes the accent again on configreloaded.
            run("shell-reload", "hyprctl reload")
            machine.sleep(4)
            border2 = option("shell-border-after-reload", "general:col.active_border")
            assert border2 == border, "after reload " + repr(border2) + ", before " + repr(border)
            configerrors("shell-configerrors")

        with subtest("display: a non-clean scale is snapped the way Hyprland would, with no notification"):
            mons = j("display-monitors-before", "hyprctl -j monitors")
            m = mons[0]
            assert (m["width"], m["height"]) == (1280, 800), "unexpected mode " + repr(m)
            model = {"outputs": [{"name": m["name"], "enabled": True, "x": 0, "y": 0, "scale": 1.1,
                                  "transform": "normal", "adaptive_sync": False,
                                  "mode": {"width": 1280, "height": 800, "refresh": round(m["refreshRate"])}}]}
            machine.succeed(u("cat > ~/.config/rime-shell/display.json <<'HLEOF'\n" + json.dumps(model) + "\nHLEOF"))
            before = hyprlog().count("Invalid scale passed to monitor")
            rc, out = run("display-apply", "${libexec}/rime-display-apply apply")
            assert rc == 0, "rime-display-apply apply: exit " + str(rc)
            assert "1.07" in out, "no snap note: " + out
            machine.sleep(2)
            mons = j("display-monitors-after", "hyprctl -j monitors")
            assert abs(mons[0]["scale"] - 128 / 120) < 1e-4, "scale " + repr(mons[0]["scale"])
            after = hyprlog().count("Invalid scale passed to monitor")
            assert after == before, "Hyprland logged an invalid scale (and so notified)"
            mlua = run("display-monitors-lua", "cat ~/.config/hypr/rime/monitors.lua")[1]
            assert "scale = 1.06666" in mlua, "monitors.lua does not carry the snapped scale"
            configerrors("display-configerrors")
            machine.screenshot("display-snapped")
            # Control: the unsnapped value straight to Hyprland is what notifies, so the log check can fail.
            run("display-control", "hyprctl eval 'hl.monitor({ output = \"" + m["name"] + "\", mode = \"1280x800@"
                + str(round(m["refreshRate"])) + "\", position = \"0x0\", scale = 1.1 })'")
            # A rule is applied on the next rendered frame (MonitorRuleManager.cpp: the
            # render.preChecks listener runs ensureMonitorStatus); an idle screen draws
            # none, so move the cursor to get one. On katana the log line shows after 9-10
            # rounds (~5-7 s) and one sandboxed build needed more than 20: 60 rounds, so a
            # slower hosted runner does not fail this control on pace alone.
            for _ in range(60):
                run("display-control-frame", "hyprctl dispatch 'hl.dsp.cursor.move({ x = 100, y = 100 })'; "
                    "hyprctl dispatch 'hl.dsp.cursor.move({ x = 400, y = 300 })'")
                if hyprlog().count("Invalid scale passed to monitor") > after:
                    break
                time.sleep(0.5)
            j("display-monitors-control", "hyprctl -j monitors")
            assert hyprlog().count("Invalid scale passed to monitor") > after, "control: raw 1.1 logged nothing"
            run("display-reset", "hyprctl eval 'hl.monitor({ output = \"" + m["name"] + "\", mode = \"preferred\", position = \"0x0\", scale = 1.0 })'")

        def keyboard_layouts(name):
            devs = j(name, "hyprctl -j devices")
            return sorted({k["layout"] for k in devs["keyboards"]})

        with subtest("input: a layout xkbcommon cannot build is refused and the last one kept"):
            machine.succeed(u("echo '{\"keyboard\": {\"layout\": \"gb\"}}' > ~/.config/rime-shell/input.json"))
            rc, out = run("input-apply-gb", "${libexec}/rime-input-apply")
            assert rc == 0, "rime-input-apply gb: exit " + str(rc)
            machine.sleep(3)
            assert keyboard_layouts("input-devices-gb") == ["gb"], "gb did not land"
            machine.succeed(u("echo '{\"keyboard\": {\"layout\": \"xx\"}}' > ~/.config/rime-shell/input.json"))
            rc, out = run("input-apply-xx", "${libexec}/rime-input-apply")
            assert "was refused" in out and "xx" in out, "no refusal note: " + out
            machine.sleep(3)
            assert keyboard_layouts("input-devices-xx") == ["gb"], "the refused layout moved the keyboard"
            lua = run("input-lua-xx", "cat ~/.config/hypr/rime/input.lua")[1]
            assert re.search(r'kb_layout\s*=\s*"gb"', lua), "input.lua lost gb"
            configerrors("input-configerrors")
            machine.screenshot("input-refused")
            # Control: kb_layout "xx" reaching Hyprland makes it fall back to us, which the devices check sees.
            machine.succeed(u("sed -i -E 's/kb_layout( *)= *\"gb\"/kb_layout\\1= \"xx\"/' ~/.config/hypr/rime/input.lua"))
            run("input-control-reload", "hyprctl reload")
            machine.sleep(3)
            assert keyboard_layouts("input-devices-control") != ["gb"], "control: a bad layout still read as gb"
            machine.succeed(u("echo '{\"keyboard\": {\"layout\": \"gb\"}}' > ~/.config/rime-shell/input.json"))
            run("input-restore", "${libexec}/rime-input-apply")
            machine.sleep(3)

        with subtest("probe: a hyprland.lua without Rime's modules is said out loud"):
            machine.succeed(u("printf -- '-- hand-written, no Rime modules\\n' > ~/.config/hypr/hyprland.lua"))
            run("probe-reload-bare", "hyprctl reload")
            machine.wait_until_succeeds(u("qs -c ${shellDir} log 2>&1 | grep -q 'Hyprland config problem (modules-missing)'"), timeout=30)
            log = shell_log()
            assert "rime-hypr-migrate" in log, "the problem line does not name the fix"
            # The notice is a notify-send to this shell's own server; its toast is
            # the right panel opening after the problem line. Shoot while it is up.
            def toast_after_problem():
                l = shell_log()
                i = l.rfind("Hyprland config problem (modules-missing)")
                return i >= 0 and "Rime pacing: right-panel Opening" in l[i:]
            for _ in range(20):
                if toast_after_problem():
                    break
                time.sleep(0.5)
            machine.screenshot("probe-modules-missing")
            assert toast_after_problem(), "the problem was logged but no notice toast opened"
            machine.succeed(u("cp ~/.config/hypr/hyprland.lua.rime ~/.config/hypr/hyprland.lua"))
            run("probe-reload-rime", "hyprctl reload")
            machine.wait_until_succeeds(u("qs -c ${shellDir} log 2>&1 | grep -q \"Hyprland's Lua config is loaded again\""), timeout=30)
            machine.succeed("cat > /tmp/hl-out/probe-shell.log <<'HLEOF'\n" + shell_log() + "\nHLEOF")

        with subtest("dispatch: the window helpers move and focus a real window through hl.dsp"):
            run("dispatch-spawn", "hyprctl dispatch 'hl.dsp.exec_cmd(\"foot -a rime-dispatch-test\")'")
            addr = None
            for _ in range(60):
                mine = [c for c in j("dispatch-clients", "hyprctl -j clients") if c.get("class") == "rime-dispatch-test"]
                if mine:
                    addr = mine[0]["address"]
                    break
                time.sleep(0.5)
            assert addr, "foot never mapped a window"
            ws0 = j("dispatch-ws-before", "hyprctl -j activeworkspace")["id"]
            rc, out = run("dispatch-place", "${libexec}/rime-project-windows place " + addr + " 7")
            assert rc == 0, "rime-project-windows place: exit " + str(rc) + ": " + out
            machine.sleep(1)
            c = [c for c in j("dispatch-clients-placed", "hyprctl -j clients") if c["address"] == addr][0]
            assert c["workspace"]["id"] == 7, "the window is on " + repr(c["workspace"])
            assert j("dispatch-ws-placed", "hyprctl -j activeworkspace")["id"] == ws0, "place followed the window"
            # rime-agent-focus has no verb that takes a window directly (it maps a session to one), so its
            # hyprland_focus is called as the module, with the script's own interpreter.
            rc, out = run("dispatch-focus",
                "py=$(head -n1 ${libexec}/rime-agent-focus | cut -c3-); $py -c '"
                "import importlib.machinery as M, importlib.util as U, sys\n"
                "s = U.spec_from_loader(\"f\", M.SourceFileLoader(\"f\", sys.argv[1]))\n"
                "f = U.module_from_spec(s); s.loader.exec_module(f)\n"
                "print(f.hyprland_focus(sys.argv[2]))' ${libexec}/rime-agent-focus " + addr)
            assert rc == 0 and out.strip().endswith("True"), "hyprland_focus: " + out
            machine.sleep(1)
            assert j("dispatch-active-window", "hyprctl -j activewindow").get("address") == addr, "focus missed the window"
            assert j("dispatch-ws-focused", "hyprctl -j activeworkspace")["id"] == 7, "focus did not bring its workspace"
            rc, out = run("dispatch-workspace", "${libexec}/rime-project-windows workspace " + str(ws0))
            assert rc == 0, "rime-project-windows workspace: exit " + str(rc) + ": " + out
            machine.sleep(1)
            assert j("dispatch-ws-back", "hyprctl -j activeworkspace")["id"] == ws0, "workspace switch did nothing"
            run("dispatch-close", "hyprctl dispatch 'hl.dsp.window.close({ window = \"address:" + addr + "\" })'")

        with subtest("firstrun: the per-login refresh leaves matugen's colors.lua alone"):
            colors = "~/.config/hypr/rime/colors.lua"
            # matugen may not have run in a VM with no wallpaper change; a file in its place is what
            # a refresh would overwrite if it overwrote it.
            machine.succeed(u("test -f " + colors + " || printf -- '-- matugen stand-in\\n' > " + colors))
            machine.succeed(u("printf -- '-- rime-test marker\\n' >> " + colors))
            before = machine.succeed(u("sha256sum " + colors)).split()[0]
            machine.succeed(u("printf -- '-- stale\\n' >> ~/.config/hypr/rime/session.lua"))
            run("firstrun-run", "systemctl --user start rime-shell-firstrun.service; "
                "systemctl --user show -p Result,ExecMainStatus rime-shell-firstrun.service")
            after = machine.succeed(u("sha256sum " + colors)).split()[0]
            assert after == before, "firstrun rewrote colors.lua"
            # Control: the refresh did run, so "unchanged" is not just "never touched".
            rc, _ = machine.execute(u("grep -q -- '-- stale' ~/.config/hypr/rime/session.lua"))
            assert rc != 0, "firstrun did not refresh session.lua, so the colors.lua check proves nothing"
            run("firstrun-reload", "hyprctl reload")
            machine.sleep(3)
            configerrors("firstrun-configerrors")

        with subtest("end: no config errors, no cursor fallbacks"):
            configerrors("end-configerrors")
            hl = hyprlog()
            shapes = len(re.findall(r"XCursor (failed to find a shape|couldn't find shape)", hl))
            run("end-cursor", "hyprctl -j getoption cursor:enable_hyprcursor; "
                "tr '\\0' '\\n' < /proc/$(pgrep -u 1000 -x .Hyprland-wrapp | head -n1)/environ | grep '^XCURSOR' || true")
            assert "XCursor librarypath is empty" not in hl, "Hyprland found no cursor theme directory"
            assert shapes == 0, str(shapes) + " XCursor shape fallbacks in hyprland.log"
            # The config probe: "loaded again" only for the one problem -> ok the probe subtest made
            # (every other reload in this run, from a good config, says nothing), and no answer with an
            # empty detail, which is what a probe killed by a restart while it ran used to print.
            log = shell_log()
            again = log.count("Hyprland's Lua config is loaded again")
            assert again == 1, str(again) + " 'loaded again' lines for one problem -> ok transition"
            empty = re.findall(r"could not ask Hyprland about its config: *$", log, re.M)
            assert not empty, str(len(empty)) + " probe answers with no detail (a probe killed mid-run)"
    except Exception:
        dump()
        raise
    finally:
        machine.execute("mkdir -p /tmp/hl-out && cp /run/user/1000/hypr/*/hyprland.log /tmp/hl-out/hyprland.log")
        machine.execute("cat > /tmp/hl-out/shell-final.log <<'HLEOF'\n" + shell_log() + "\nHLEOF")
        machine.execute("tar -C /tmp -cf /tmp/hl-out.tar hl-out 2>/dev/null")
        machine.copy_from_machine("/tmp/hl-out.tar")
  '';
}
