# Rime's desktop in a VM: boots to rime-greet (greetd, sway, VT 1), logs in through the greeter with the
# keyboard, and Hyprland starts as the user with Rime's own Lua config. Also checks the keyring PAM
# wiring, the session list and the portal/audio services the sessions rely on.
{ pkgs, self }:

let
  # A gnome-keyring binary keyring with no items whose password is "rime" (the test account's), in the
  # format rime-keyring-login reads (gkm-secret-binary.c: header, salt, iterated SHA-256 -> AES-128-CBC,
  # MD5 of the plaintext up front). Seeded as the user's DEFAULT keyring so the login has to move it.
  keyringFixture = pkgs.runCommand "rime-test-keyring" { nativeBuildInputs = [ pkgs.python3 pkgs.openssl ]; } ''
    python3 - "$out" <<'PY'
    import hashlib, struct, subprocess, sys
    password, salt, iterations = b"rime", bytes(range(1, 9)), 1953
    def string(b): return struct.pack(">I", len(b)) + b
    plain = bytearray(16)                      # MD5 placeholder, then the items' data (none)
    plain[:16] = hashlib.md5(bytes(plain[16:])).digest()
    digest = hashlib.sha256(password + salt).digest()
    for _ in range(iterations - 1):
        digest = hashlib.sha256(digest).digest()
    blob = subprocess.run(["openssl", "enc", "-aes-128-cbc", "-nopad", "-K", digest[:16].hex(),
                           "-iv", digest[16:32].hex()], input=bytes(plain), capture_output=True, check=True).stdout
    data = (b"GnomeKeyring\n\r\0\n" + b"\0\0\0\0" + string(b"Default keyring") + bytes(16)
            + struct.pack(">II", 0, 0) + struct.pack(">I", iterations) + salt + bytes(16)
            + struct.pack(">I", 0) + struct.pack(">I", len(blob)) + blob)
    open(sys.argv[1], "wb").write(data)
    PY
  '';
in
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

    with subtest("greeter state: the greeter owns its memory, root owns the published wallpapers"):
        machine.succeed("install -d -o root -g root /var/lib/rime-greet/wallpapers")
        machine.succeed("touch /var/lib/rime-greet/wallpapers/1000.jpg /var/lib/rime-greet/rime-test-state")
        machine.succeed("chown 4242:4242 /var/lib/rime-greet/rime-test-state")  # a bootc machine's greetd uid
        machine.succeed("systemd-tmpfiles --create --prefix=/var/lib/rime-greet")
        machine.succeed("test \"$(stat -c %U /var/lib/rime-greet/rime-test-state)\" = greetd")
        machine.succeed("test \"$(stat -c %U:%G /var/lib/rime-greet/wallpapers/1000.jpg)\" = root:root")
        machine.succeed("test \"$(stat -c %U:%G /var/lib/rime-greet/wallpapers)\" = root:root")
        machine.succeed("rm -f /var/lib/rime-greet/wallpapers/1000.jpg /var/lib/rime-greet/rime-test-state")

    with subtest("the user's default keyring opens with the login password (rime-keyring-login moves it)"):
        kr = "/home/rime/.local/share/keyrings"
        machine.succeed(f"install -d -m 700 -o rime -g users /home/rime/.local /home/rime/.local/share {kr}")
        machine.succeed(f"install -m 600 -o rime -g users ${keyringFixture} {kr}/Default_keyring.keyring")
        machine.succeed(f"printf Default_keyring > {kr}/default && chown rime:users {kr}/default")
        machine.fail(f"test -e {kr}/login.keyring")

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
        # 50-rime-keymap: the system keymap (services.xserver.xkb -> 00-keyboard.conf) reaches the
        # manager environment and from there the greeter's sway (NixOS default options, not sway's)
        machine.succeed("systemctl show-environment | grep -x XKB_DEFAULT_LAYOUT=us")
        machine.succeed("systemctl show-environment | grep -x XKB_DEFAULT_OPTIONS=terminate:ctrl_alt_bksp")
        print("greetd's own XKB environment:", machine.succeed(
            "tr '\\0' '\\n' < /proc/$(pgrep -u root -x greetd | head -n1)/environ | grep XKB_ || true"))
        machine.succeed(
            "tr '\\0' '\\n' < /proc/$(pgrep -u greetd -x sway | head -n1)/environ "
            "| grep -x XKB_DEFAULT_OPTIONS=terminate:ctrl_alt_bksp"
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
        # the seeded default keyring became the login keyring, so the login password opens it
        machine.succeed(f"test ! -e {kr}/Default_keyring.keyring")
        machine.succeed(f"test \"$(cat {kr}/default)\" = login")
        print(machine.succeed("journalctl -b -t rime-keyring-login --no-pager"))
        machine.succeed("journalctl -b -t rime-keyring-login --no-pager | grep \"'Default_keyring' keyring is now the login keyring\"")

    with subtest("rime-keyring-login moves nothing while the user's keyring daemon runs"):
        pid = machine.succeed("pgrep -u rime -f '/bin/gnome-keyring-daemon .*--login' | head -n1").strip()
        comm = machine.succeed(f"cat /proc/{pid}/comm").strip()
        print("keyring daemon comm:", comm, machine.succeed(f"stat -c %U /proc/{pid}").strip())
        # the comm the helper's guard is patched to know (nix/modules/desktop.nix keyringLogin)
        assert comm == ".gnome-keyring-", comm
        hook = machine.succeed("grep -o '/nix/store/[^ ]*/libexec/rime-keyring-login' /etc/pam.d/greetd").strip()
        machine.succeed(f"install -m 600 -o rime -g users ${keyringFixture} {kr}/Other.keyring")
        machine.succeed(f"printf Other > {kr}/default && chown rime:users {kr}/default")
        # what pam_exec does at the next login: the password on stdin, as root
        machine.succeed(f"printf 'rime\\0' | env PAM_TYPE=auth PAM_USER=rime {hook}")
        machine.succeed(f"test -e {kr}/Other.keyring")
        machine.succeed(f"test \"$(cat {kr}/default)\" = Other")
        machine.succeed("journalctl -b -t rime-keyring-login --no-pager | grep 'a keyring daemon is already running'")

    with subtest("the session's user services"):
        machine.wait_until_succeeds(as_rime("systemctl --user is-active pipewire.socket"))
        machine.wait_until_succeeds(as_rime("systemctl --user is-active wireplumber.service"))
        machine.succeed("test -e /etc/xdg/xdg-desktop-portal/hyprland-portals.conf")
        machine.succeed("grep -x 'org.freedesktop.impl.portal.Secret=gnome-keyring' /etc/xdg/xdg-desktop-portal/hyprland-portals.conf")
        machine.succeed("fc-list | grep 'JetBrainsMono Nerd Font'")
        # GTK reads settings.ini from $XDG_CONFIG_DIRS (its sysconfdir is a store path)
        machine.succeed(as_rime("test -n \"$XDG_CONFIG_DIRS\" && for d in $(echo $XDG_CONFIG_DIRS | tr : ' '); do "
                                "grep -x gtk-application-prefer-dark-theme=1 $d/gtk-3.0/settings.ini && exit 0; done; exit 1"))
  '';
}
