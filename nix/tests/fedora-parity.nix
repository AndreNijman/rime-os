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

    with subtest("record: I/O scheduler and sysctls Fedora set (printed, compared in docs/nixos/fedora-defaults.md)"):
        print(machine.succeed("for d in /sys/block/*/queue/scheduler; do echo \"$d: $(cat $d)\"; done"))
        print(machine.succeed("sysctl vm.max_map_count kernel.sysrq kernel.yama.ptrace_scope kernel.kptr_restrict fs.inotify.max_user_watches kernel.core_pattern vm.swappiness"))
        print(machine.succeed("systemctl is-enabled fstrim.timer systemd-oomd.service systemd-pstore.service logrotate.timer || true"))
  '';
}
