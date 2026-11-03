# shell-kept: the Shell's kept panel windows (rime-shell #53, smooth motion) on Hyprland, the product session.
# greetd's initial_session logs the user straight into start-hyprland with Rime's own Lua config (share/rime/hypr,
# seeded as firstrun does), and session.lua starts the Shell through rime-shell-autostart. shell-kept-driver.py
# runs inside the VM as the session user.
#
# phase  short  (the check) every surface opens and closes, parked windows are 1x1, take no pointer, keyboard or
#               focus, fullscreen and workspace switches, pacing per surface, Hyprland and Shell logs clean
#        full   the same with more cycles and three 60 s idle rounds (the numbers in rime-shell #53)
#        probe, input  diagnostics only (they always pass)
# A failed assertion fails the check (the driver exits 1). Not in hosted CI: nix/ci/checks-not-in-ci.txt says why.
#   nix build path:.#checks.x86_64-linux.shell-kept -L   (results: the VM's /tmp/kept-out in kept-out.tar)
{ pkgs, self, phase ? "short" }:

let
  user = "alice";
  shellDir = "/run/current-system/sw/share/rime-shell";
  py = pkgs.python3.withPackages (p: [ p.pillow ]);
  # The probe that runs inside the VM: cycles, parking geometry, pacing log parsing.
  driver = ./shell-kept-driver.py;
in
pkgs.testers.runNixOSTest {
  name = "rime-shell-kept";
  node.pkgsReadOnly = false;

  nodes.machine = { lib, config, ... }: {
    imports = [ self.nixosModules.default ];
    rime.desktop.enable = true;

    users.users.${user} = {
      isNormalUser = true;
      uid = 1000;
      initialPassword = "rime-test";
      extraGroups = [ "wheel" "video" "audio" "ydotool" ];
    };

    # Straight into the product session, no greeter: the greeter's sway needs pixman + Qt's software backend in
    # this VM (desktop.nix), and those would reach the shell too. The shell must render the way it does on a
    # laptop: Qt Quick on GL (Mesa llvmpipe here).
    services.greetd.settings.initial_session = {
      user = user;
      command = "${config.programs.hyprland.package}/bin/start-hyprland";
    };
    # firstrun's job on a real login: Rime's Hyprland config in the user's home, keyboard placeholders filled.
    systemd.services.rime-test-seed-hypr = {
      wantedBy = [ "greetd.service" ];
      before = [ "greetd.service" ];
      serviceConfig.Type = "oneshot";
      script = ''
        h=/home/${user}
        install -d -o ${user} -g users $h/.config $h/.config/hypr $h/.local $h/.local/share $h/.local/share/hyprland
        cp -r /run/current-system/sw/share/rime/hypr/. $h/.config/hypr/
        grep -rl '@KB_' $h/.config/hypr | xargs -r sed -i -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g'
        # Hyprland's file log stops at config load unless asked to keep it (debug.disable_logs defaults to true).
        echo 'hl.config({ debug = { disable_logs = false } })' >> $h/.config/hypr/hyprland.lua
        chown -R ${user}:users $h/.config $h/.local
      '';
    };

    environment.sessionVariables = {
      RIME_PACING_LOG = "1";
      RIME_RELEASE_JSON = "/nonexistent";
      RIME_RELEASE_OPENER = "/bin/false";
    };

    programs.ydotool.enable = true;
    environment.systemPackages = [ pkgs.wev pkgs.grim pkgs.jq py pkgs.foot ];

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
    };
    hardware.graphics.enable = true;
    systemd.services.rime-flathub-setup.enable = lib.mkForce false;
  };

  testScript = ''
    import shlex
    phase = "${phase}"

    def u(cmd):
        return "su - ${user} -c " + shlex.quote(
            "export XDG_RUNTIME_DIR=/run/user/1000; "
            "export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -n1); "
            "export WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1); " + cmd)

    def dump():
        print(machine.execute("ps -eo pid,ppid,etimes,args | grep -v ' \\[' | tail -n 80; "
                              "journalctl -b --no-pager -o short-monotonic | tail -n 150")[1])
        print(machine.execute(u("hyprctl configerrors; hyprctl layers; hyprctl clients; "
            "tail -n 300 /run/user/1000/hypr/*/hyprland.log; qs -c ${shellDir} log 2>&1 | tail -n 200"))[1])

    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
    try:
        machine.wait_until_succeeds(u("qs -c ${shellDir} ipc show | grep -q dashboard-home"), timeout=180)
        machine.sleep(8)
        machine.succeed("cp ${driver} /tmp/kept.py && chmod a+r /tmp/kept.py")
        rc, out = machine.execute(u("${py}/bin/python3 /tmp/kept.py " + phase + " ${shellDir} /tmp/kept-out 2>&1"), timeout=3600)
        print(out)
        assert rc == 0, f"shell-kept-driver.py {phase}: exit {rc} (the FAIL lines above)"
    except Exception:
        dump()
        raise
    finally:
        machine.execute("tar -C /tmp -cf /tmp/kept-out.tar kept-out 2>/dev/null")
        machine.copy_from_machine("/tmp/kept-out.tar")
    dump()
  '';
}
