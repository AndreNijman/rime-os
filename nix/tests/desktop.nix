# Rime's desktop in a VM: boots to rime-greet (greetd, sway, VT 1), logs in through the greeter with the
# keyboard, and Hyprland starts as the user with Rime's own Lua config. Also checks the keyring PAM
# wiring, the session list and the portal/audio services the sessions rely on.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-desktop";

  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];

    rime.desktop.enable = true;

    users.users.rime = {
      isNormalUser = true;
      uid = 1000;
      initialPassword = "rime";
      extraGroups = [ "wheel" "video" "audio" "networkmanager" ];
    };

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      # virtio-gpu: a KMS device Mesa's software rasteriser can drive (GBM + EGL on llvmpipe).
      qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
    };

    # VM-only graphics knobs, through pam_env so they reach the greeter session and the user session.
    # sway's GLES2 renderer does not run on QEMU's GPU (nixpkgs nixos/tests/sway.nix), so the greeter
    # host renders with pixman, and Qt Quick then draws into shared memory instead of EGL.
    environment.sessionVariables = {
      WLR_RENDERER = "pixman";
      QT_QUICK_BACKEND = "software";
    };

    # No network in the test VM: the Flathub oneshot would only retry.
    systemd.services.rime-flathub-setup.enable = lib.mkForce false;
  };

  testScript = ''
    import shlex

    def as_rime(cmd):
        return "su - rime -c " + shlex.quote("export XDG_RUNTIME_DIR=/run/user/1000; " + cmd)

    machine.wait_for_unit("multi-user.target")

    with subtest("PAM: greetd's stack unlocks gnome-keyring and runs rime-keyring-login"):
        machine.succeed("grep -E '^auth .*pam_gnome_keyring.so' /etc/pam.d/login")
        machine.succeed("grep -E '^session .*pam_gnome_keyring.so.*auto_start' /etc/pam.d/login")
        machine.succeed("grep -E '^auth substack login' /etc/pam.d/greetd")
        machine.succeed("grep -E '^auth optional .*pam_exec.so quiet expose_authtok .*rime-keyring-login' /etc/pam.d/greetd")
        # the hook sits after the password check, never before it
        machine.succeed("awk '/^auth substack login/{s=NR} /rime-keyring-login/{h=NR} END{exit !(s && h > s)}' /etc/pam.d/greetd")

    with subtest("sessions: only Rime's, under their product names"):
        out = machine.succeed("grep -h '^Name=' /etc/rime/wayland-sessions/*.desktop | sort")
        print(out)
        names = sorted(l.strip() for l in out.strip().splitlines())
        assert names == ["Name=Rime Floating", "Name=Rime Safe Graphics", "Name=Rime Scrolling", "Name=Rime Tiling"], names
        machine.succeed("grep -qx 'Exec=.*/bin/start-hyprland' /etc/rime/wayland-sessions/hyprland.desktop")

    with subtest("Rime's Hyprland config is seeded for the user (rime-shell-firstrun's job on a real login)"):
        machine.succeed("install -d -o rime -g users /home/rime/.config /home/rime/.config/hypr")
        machine.succeed("cp -r /run/current-system/sw/share/rime/hypr/. /home/rime/.config/hypr/")
        # firstrun's render_hypr_tmpl: no system keymap set here, so its fallback (us, no variant)
        machine.succeed("grep -rl '@KB_' /home/rime/.config/hypr | xargs -r sed -i -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g'")
        machine.fail("grep -r '@KB_' /home/rime/.config/hypr")
        machine.succeed("chown -R rime:users /home/rime/.config/hypr")

    with subtest("boots to rime-greet on VT 1"):
        machine.wait_for_unit("greetd.service")
        machine.wait_until_succeeds("pgrep -u greetd -x sway")
        machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
        machine.succeed("test \"$(cat /sys/class/tty/tty0/active)\" = tty1")
        # the greeter owns tty1: its logind session sits on VT 1 with class greeter
        machine.wait_until_succeeds(
            "loginctl list-sessions --no-legend | awk '$3==\"greetd\"{print $1}' | head -n1 | "
            "xargs -r -I{} loginctl show-session {} -p VTNr -p Class | tr '\\n' ' ' | grep 'VTNr=1' | grep 'Class=greeter'"
        )
        machine.sleep(8)  # let the greeter paint before the screenshot
        machine.screenshot("greeter")
        print(machine.succeed("journalctl -b -u greetd --no-pager | tail -n 40"))

    with subtest("logs in through the greeter and Hyprland starts as the user"):
        machine.send_chars("rime\n")   # no last-user yet: focus starts on the username field
        machine.sleep(2)
        machine.send_chars("rime\n")   # password field
        # Hyprland's IPC socket is the proof it is up (the process name depends on how start-hyprland
        # execs it, so it is printed, not matched)
        machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
        print(machine.succeed("ps -u rime -o pid,comm,args --no-headers | cut -c1-200"))
        # the user's graphical session is greetd's: on VT 1, class user
        print(machine.succeed("loginctl list-sessions --no-legend"))
        machine.succeed(
            "for s in $(loginctl list-sessions --no-legend | awk '$3==\"rime\"{print $1}'); do "
            "loginctl show-session $s -p VTNr -p Class -p Service | tr '\\n' ' '; echo; done | "
            "grep 'VTNr=1' | grep 'Class=user' | grep 'Service=greetd'"
        )
        sig = machine.succeed("basename \"$(ls -d /run/user/1000/hypr/*/ | head -n1)\"").strip()
        out = machine.wait_until_succeeds(as_rime(f"HYPRLAND_INSTANCE_SIGNATURE={sig} hyprctl version"), timeout=60)
        print(out)
        assert "Hyprland 0.57" in out, out
        errs = machine.succeed(as_rime(f"HYPRLAND_INSTANCE_SIGNATURE={sig} hyprctl configerrors"))
        print("configerrors:", errs)
        machine.sleep(3)
        machine.screenshot("hyprland")

    with subtest("the login password opened the keyring"):
        # the daemon is a nix-wrapped binary (comm ".gnome-keyring-"), so match its path
        machine.wait_until_succeeds("pgrep -u rime -f '/bin/gnome-keyring-daemon .*--login'", timeout=60)
        machine.wait_until_succeeds("test -s /home/rime/.local/share/keyrings/login.keyring", timeout=60)

    with subtest("the session's user services"):
        machine.wait_until_succeeds(as_rime("systemctl --user is-active pipewire.socket"))
        machine.wait_until_succeeds(as_rime("systemctl --user is-active wireplumber.service"))
        machine.succeed("test -e /etc/xdg/xdg-desktop-portal/hyprland-portals.conf")
        machine.succeed("grep -x 'org.freedesktop.impl.portal.Secret=gnome-keyring' /etc/xdg/xdg-desktop-portal/hyprland-portals.conf")
        machine.succeed("fc-list | grep 'JetBrainsMono Nerd Font'")
  '';
}
