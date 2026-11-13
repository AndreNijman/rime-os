# The Fedora defaults Rime relied on (nix/modules/fedora-parity.nix, docs/nixos/fedora-defaults.md), checked on a
# running desktop machine. Every subtest reads the behaviour, not the option: the swap device the kernel has, the
# config NetworkManager actually loaded, the font fontconfig actually picks, the shell useradd actually gives.
#
# Mutant: the same test with `rime.fedoraParity.enable = false` on the node fails at its first subtest (firmware).
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-fedora-parity";

  node.pkgsReadOnly = false;

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];

    rime.desktop.enable = true;
    # Passwordless sudo is the security area's setting; every shipped system has it on (nix/hosts/profile.nix).
    rime.security.enable = true;

    # An administrator (wheel, as a moved Fedora admin is) and an account that is not.
    users.users.admin = { isNormalUser = true; extraGroups = [ "wheel" ]; };
    users.users.plain = { isNormalUser = true; };

    # Declared in the configuration: keeps the shell NixOS gives it (bash). Only NEW accounts get zsh.
    users.users.declared = {
      isNormalUser = true;
      initialPassword = "declared";
    };

    virtualisation.memorySize = 2048;
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")

    with subtest("firmware is installed (linux-firmware + wireless-regdb)"):
        fw = machine.succeed("readlink -f /run/current-system/firmware").strip()  # already .../lib/firmware
        machine.succeed(f"ls {fw}/ | grep -E '^iwlwifi-' >/dev/null")
        machine.succeed(f"ls {fw}/mediatek/mt7925/ | grep -E '^WIFI_RAM_CODE_MT7925' >/dev/null")
        machine.succeed(f"test -e {fw}/regulatory.db.zst || test -e {fw}/regulatory.db")

    with subtest("zram swap: min(ram, 8G), zstd, active"):
        machine.wait_for_unit("systemd-zram-setup@zram0.service")
        print(machine.succeed("swapon --show; zramctl"))
        machine.succeed("swapon --show=NAME --noheadings | grep -x /dev/zram0")
        machine.succeed("zramctl --output ALGORITHM --noheadings /dev/zram0 | grep -w zstd")
        ram = int(machine.succeed("awk '/MemTotal/ {print $2 * 1024}' /proc/meminfo").strip())
        disk = int(machine.succeed("zramctl --bytes --output DISKSIZE --noheadings /dev/zram0").strip())
        assert abs(disk - min(ram, 8 * 1024**3)) < 64 * 1024**2, f"zram {disk} != min(ram {ram}, 8G)"

    with subtest("/tmp is a tmpfs; nix builds go to /var/tmp"):
        machine.succeed("findmnt -no FSTYPE /tmp | grep -x tmpfs")
        machine.succeed("systemctl show nix-daemon.service -p Environment | grep -w TMPDIR=/var/tmp")

    with subtest("NetworkManager loaded Rime's and Fedora's settings"):
        machine.wait_for_unit("NetworkManager.service")
        cfg = machine.succeed("NetworkManager --print-config")
        print(cfg)
        for want in ["wifi.powersave=2", "uri=http://fedoraproject.org/static/hotspot.txt", "response=OK",
                     "wifi.cloned-mac-address=stable-ssid", "ipv4.clat=1"]:
            assert want in cfg.replace(" ", ""), f"NetworkManager --print-config lacks {want}"
        machine.succeed("busctl get-property org.freedesktop.NetworkManager /org/freedesktop/NetworkManager org.freedesktop.NetworkManager ConnectivityCheckEnabled | grep -x 'b true'")

    with subtest("new accounts get zsh; declared ones keep theirs"):
        machine.succeed("grep -x /run/current-system/sw/bin/zsh /etc/shells")
        machine.succeed("test -s /etc/zshrc")
        machine.succeed("useradd -m newcomer")
        shell = machine.succeed("getent passwd newcomer | cut -d: -f7").strip()
        assert shell == "/run/current-system/sw/bin/zsh", shell
        machine.succeed("su - newcomer -c 'echo $ZSH_VERSION' | grep .")
        declared = machine.succeed("getent passwd declared | cut -d: -f7").strip()
        assert declared.endswith("/bash"), declared

    with subtest("umask 0022 for the login screen and user managers, home 0700 (Fedora: 0022, HOME_MODE 0700)"):
        # Neither stack runs pam_umask, so a session inherits the service manager's UMask.
        machine.succeed("systemctl show greetd.service -p UMask | grep -x UMask=0022")
        machine.succeed("systemctl show user@1000.service -p UMask | grep -x UMask=0022")
        assert machine.succeed("stat -c %a /home/newcomer").strip() == "700"

    with subtest("fontconfig picks Noto for the generic families"):
        for fam, want in [("sans-serif", "Noto Sans"), ("serif", "Noto Serif"),
                          ("monospace", "Noto Sans Mono"), ("emoji", "Noto Color Emoji")]:
            got = machine.succeed(f"fc-match -f '%{{family[0]}}' {fam}").strip()
            assert got == want, f"fc-match {fam} = {got!r}, want {want!r}"

    with subtest("udev rules Fedora's packages shipped"):
        for rule in ["30-discrete-gpu.rules", "80-libinput-device-groups.rules",
                     "90-libinput-fuzz-override.rules", "71-ipp-usb.rules"]:
            machine.succeed(f"ls /etc/udev/rules.d/ | grep -x {rule}")
        # NixOS's usbmuxd module writes its own Apple rule (99-local.rules) instead of the package's 39-usbmuxd.rules,
        # and runs the daemon from boot rather than on plug-in.
        machine.succeed("grep -F 'ATTR{idVendor}==\"05ac\"' /etc/udev/rules.d/99-local.rules")
        machine.succeed("systemctl cat usbmuxd.service >/dev/null")

    with subtest("hyprlock has a PAM service, not 'other'"):
        machine.succeed("grep pam_unix /etc/pam.d/hyprlock")

    with subtest("mpris-proxy starts in every user session"):
        machine.succeed("test -L /etc/systemd/user/default.target.wants/mpris-proxy.service")

    with subtest("a home written on bootc finds its programs: /usr/bin, /usr/sbin and /bin"):
        # bootc paths a moved home names literally: user units (ExecStart=/usr/bin/sing-box), ssh helpers
        # (socat), #!/bin/bash scripts (a Claude Code status line).
        for path in ["/usr/bin/sing-box", "/usr/bin/socat", "/usr/sbin/socat", "/bin/bash", "/usr/bin/flatpak"]:
            target = machine.succeed(f"readlink {path}").strip()
            assert target == "/run/current-system/sw/bin/" + path.rsplit("/", 1)[1], f"{path} -> {target}"
        machine.succeed("/usr/bin/sing-box version | grep -i sing-box")
        machine.succeed("/usr/bin/socat -V | grep -i socat")
        machine.succeed("printf '#!/bin/bash\\necho \"$BASH_VERSION\"\\n' > /tmp/s.sh && chmod +x /tmp/s.sh")
        machine.succeed("su - admin -c /tmp/s.sh | grep .")
        # A unit naming the bootc path runs (systemd execs it with no PATH lookup).
        machine.succeed("systemd-run --wait --collect -p Type=oneshot /usr/bin/socat -V")
        # NixOS's own entries are never replaced.
        assert machine.succeed("readlink /usr/bin/env").startswith("/nix/store/")
        assert machine.succeed("readlink /bin/sh").startswith("/nix/store/")
        # At the next activation: a link whose program the system no longer has goes; what an admin put there stays.
        machine.succeed("ln -s /run/current-system/sw/bin/rime-no-such-program /usr/bin/rime-no-such-program")
        machine.succeed("ln -s /opt/elsewhere /usr/bin/admin-link; printf '#!/bin/sh\\n' > /usr/bin/admin-file")
        machine.succeed("/run/current-system/activate")
        machine.fail("test -L /usr/bin/rime-no-such-program")
        machine.succeed("test -L /usr/bin/admin-link && test -f /usr/bin/admin-file")
        machine.succeed("test -L /usr/bin/sing-box")
        # setuid programs reach their wrapper (sw/bin's sudo has no setuid bit), also a link an older system made.
        machine.succeed("ln -sfn /run/current-system/sw/bin/sudo /usr/bin/sudo")
        machine.succeed("/run/current-system/activate")
        for prog in ["sudo", "su", "passwd", "mount"]:
            target = machine.succeed(f"readlink /usr/bin/{prog}").strip()
            assert target == f"/run/wrappers/bin/{prog}", f"/usr/bin/{prog} -> {target}"
        machine.succeed("su - admin -c '/usr/bin/sudo -n true'")

    with subtest("the shell and developer tools bootc Rime ships are on every machine"):
        for prog in ["starship", "zoxide", "fzf", "eza", "bat", "rg", "fd", "tmux", "fish", "nu", "zellij",
                     "gh", "node", "npm", "gcc", "ld", "make", "sing-box", "nvim", "nano", "socat", "lsof",
                     "file", "lspci", "lsusb", "smartctl", "xxd"]:
            machine.succeed(f"su - admin -c 'command -v {prog}'")
        # The plugins, where the seeded zshrc looks on NixOS and where a moved (bootc) zshrc looks.
        for plugin in ["zsh-autosuggestions/zsh-autosuggestions.zsh",
                       "zsh-syntax-highlighting/zsh-syntax-highlighting.zsh"]:
            machine.succeed(f"test -r /run/current-system/sw/share/{plugin}")
            machine.succeed(f"test -r /usr/share/{plugin}")

    with subtest("ssh from another terminal: its terminfo is here"):
        for term in ["xterm-ghostty", "xterm-kitty", "alacritty", "foot"]:
            machine.succeed(f"su - admin -c 'infocmp {term} >/dev/null'")

    with subtest("every login has the machine's locale, and English locales bootc shipped exist"):
        # imperativeLocale takes LANG out of /etc/pam/environment; PAM now reads /etc/locale.conf (hostlayer.nix).
        # Without it `btop` refused to start: "No UTF-8 locale detected!" (the L16, 2026-11-13).
        assert machine.succeed("su - admin -c 'printf %s \"$LANG\"'").strip() == "en_US.UTF-8"
        # a carried or chosen en_AU (glibc-langpack-en on bootc) is a real locale here, and reaches the next login
        machine.succeed("localectl set-locale LANG=en_AU.UTF-8")
        assert machine.succeed("su - admin -c 'printf %s \"$LANG\"'").strip() == "en_AU.UTF-8"
        out = machine.succeed("su - admin -c 'locale 2>&1; date +%x -d 2026-11-13'")
        assert "Cannot set" not in out and "LC_CTYPE=\"en_AU.UTF-8\"" in out and "13/11/26" in out, out
        machine.succeed("localectl set-locale LANG=en_US.UTF-8")
        # zsh's `cd` is zoxide again with the seeded zshrc (zoxide was missing on NixOS until 2026-11-13)
        machine.succeed("su - admin -c 'cp /run/current-system/sw/share/rime/zsh/zshrc ~/.zshrc'")
        out = machine.succeed("su - admin -c \"zsh -ic 'whence -w cd __zoxide_z'\" 2>&1")
        assert "cd: function" in out and "__zoxide_z: function" in out, out

    with subtest("administrators use sudo without a password; nobody else gets sudo"):
        machine.succeed("su - admin -c 'sudo -n true'")
        machine.succeed("su - admin -c 'sudo -n id -u' | grep -x 0")
        rc, out = machine.execute("su - plain -c 'sudo -n true' 2>&1")
        assert rc != 0, f"a non-wheel account ran sudo: {out}"
        # no_new_privs (every agent session) still stops sudo.
        rc, out = machine.execute("su - admin -c 'setpriv --no-new-privs sudo -n true' 2>&1")
        assert rc != 0, f"sudo worked under no_new_privs: {out}"

    with subtest("record: I/O scheduler and sysctls Fedora set (printed, compared in docs/nixos/fedora-defaults.md)"):
        print(machine.succeed("for d in /sys/block/*/queue/scheduler; do echo \"$d: $(cat $d)\"; done"))
        print(machine.succeed("sysctl vm.max_map_count kernel.sysrq kernel.yama.ptrace_scope kernel.kptr_restrict fs.inotify.max_user_watches kernel.core_pattern vm.swappiness"))
        print(machine.succeed("systemctl is-enabled fstrim.timer systemd-oomd.service systemd-pstore.service logrotate.timer || true"))
  '';
}
