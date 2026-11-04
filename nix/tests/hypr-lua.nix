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
#   end       `hyprctl configerrors` is empty again.
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
    environment.systemPackages = [ pkgs.jq ];

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
            log = shell_log()
            want = [l for l in log.splitlines() if "Rime window corners:" in l]
            assert want, "the Shell never synced the window corners"
            r = int(want[-1].rsplit("rounding", 1)[1].strip())
            rounding = option("shell-rounding", "decoration:rounding")
            assert rounding.get("int") == r, "rounding " + repr(rounding) + ", the Shell wrote " + str(r)
            border = option("shell-border", "general:col.active_border")
            assert "8d748c" not in json.dumps(border), "the accent never replaced the config's gradient: " + repr(border)
            wait_gaps("shell-gaps-start", 5, 10)
            # Focus mode's IPC handler lives in QuickSettings, which is built only
            # when the dashboard's home page is first shown: before that the
            # toggle is a no-op (measured in this VM: rc 0, gaps stay 5/10;
            # open-items.md). Show the page once, close it, then toggle.
            run("shell-dashboard-open", "qs -c ${shellDir} ipc call dashboard-home toggle")
            machine.sleep(2)
            run("shell-dashboard-close", "qs -c ${shellDir} ipc call dashboard-home toggle")
            machine.sleep(2)
            run("shell-focus-on", "qs -c ${shellDir} ipc call focus-toggle toggle")
            wait_gaps("shell-gaps-focus", 0, 6)
            run("shell-focus-off", "qs -c ${shellDir} ipc call focus-toggle toggle")
            wait_gaps("shell-gaps-restored", 5, 10)
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
            # none, so move the cursor to get one.
            for _ in range(20):
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

        with subtest("end: no config errors"):
            configerrors("end-configerrors")
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
