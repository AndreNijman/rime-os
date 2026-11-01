# The full system test: the rime-vm host (every Rime area on) plus Rime's own kernel, met the way a user meets it.
# Boot -> rime-greet on tty1 -> a keyboard login through greetd -> the Rime Tiling (Hyprland) session ->
# Rime Shell (quickshell) running the shipped config, with rimed on the system bus, NetworkManager up, sshd
# refusing passwords and rime-firewall's table loaded. Screenshots: greeter.png, desktop.png in $out.
#
# Only the test account's CORRECT password is ever typed (Rime's rule: never test a wrong PAM password).
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-vm-boot";

  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.machine =
    { lib, pkgs, ... }:
    {
      imports = [
        self.nixosModules.default
        ../hosts/vm.nix
      ];

      # rime-vm leaves the kernel off so local runs reuse nixpkgs' cached kernel; the system test boots Rime's.
      # QEMU has no NVIDIA GPU, so the NVIDIA stack stays off here (nix/tests/kernel.nix carries it).
      rime.kernel = {
        enable = true;
        nvidia.enable = false;
      };

      users.users.rime.uid = 1000;

      virtualisation = {
        # virtio-gpu: a KMS device Mesa's software rasteriser can drive (GBM + EGL on llvmpipe).
        qemu.options = [
          "-vga none"
          "-device virtio-gpu-pci,xres=1280,yres=800"
        ];
      };

      # VM-only graphics knobs (as nix/tests/desktop.nix): sway's GLES2 renderer does not run on QEMU's GPU,
      # so the greeter host renders with pixman, and Qt Quick draws into shared memory instead of EGL.
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };

      # No network in the test VM: the Flathub oneshot would only retry.
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;

      # The password-login probe against sshd.
      environment.systemPackages = [ pkgs.sshpass ];
    };

  testScript = ''
    import shlex

    sw = "/run/current-system/sw"
    shell_dir = sw + "/share/rime-shell"
    quickshell = "pgrep -u rime -f '^[^ ]*quickshell -c " + shell_dir + "$'"

    def as_rime(cmd):
        return "su - rime -c " + shlex.quote("export XDG_RUNTIME_DIR=/run/user/1000; " + cmd)

    def in_session(cmd):
        # A client inside the graphical session: its runtime dir and the compositor's Wayland socket.
        return (
            "sudo -u rime env -u DISPLAY XDG_RUNTIME_DIR=/run/user/1000 "
            "WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1) " + cmd
        )

    def dump_logs():
        print(machine.execute(
            "echo '== greetd'; journalctl -b -u greetd --no-pager | tail -n 60; "
            "echo '== user journal'; journalctl -b _UID=1000 --no-pager | tail -n 200; "
            "echo '== processes'; ps -u rime -o pid,args --no-headers | cut -c1-220"
        )[1])

    machine.wait_for_unit("multi-user.target")

    with subtest("every area is on and owns its files"):
        rel = machine.succeed("uname -r").strip()
        print("kernel:", rel)
        assert "cachyos" in rel, f"booted {rel}, not Rime's kernel"
        machine.succeed("test -d /sys/kernel/sched_ext")
        # One owner per path on the profile (system-path's buildEnv drops collisions silently).
        owners = {
            "libexec/rime-session-select": "rime-gaming-switch",
            "libexec/rime-firewall": "rime-firewall",
            "share/rime/hypr/rime/session.lua": "rime-desktop-data",
            "libexec/rime-shell-autostart": "rime-libexec",
            "libexec/rime-greetd": "rime-libexec",
        }
        for rel_path, owner in owners.items():
            real = machine.succeed(f"readlink -f {sw}/{rel_path}").strip()
            print(rel_path, "->", real)
            assert owner in real, f"{rel_path} resolves to {real}, expected {owner}"
        machine.succeed("test -e /etc/rime/wayland-sessions/hyprland.desktop")
        machine.succeed("ls /etc/rime/wayland-sessions/ | grep -i gaming")

    with subtest("greetd: the one-shot session switch is wired"):
        unit = machine.succeed("systemctl cat greetd.service")
        assert "EnvironmentFile=-/run/rime-greet/greetd.env" in unit, unit
        assert "libexec/rime-greetd" in unit, unit
        assert "$RIME_GREETD_ARGS" in unit, unit
        machine.succeed("test -s /etc/greetd/config.toml")

    with subtest("boots to rime-greet on tty1"):
        machine.wait_for_unit("greetd.service")
        machine.wait_until_succeeds("pgrep -u greetd -x sway", timeout=120)
        machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=120)
        machine.succeed("test \"$(cat /sys/class/tty/tty0/active)\" = tty1")
        machine.wait_until_succeeds(
            "loginctl list-sessions --no-legend | awk '$3==\"greetd\"{print $1}' | head -n1 | "
            "xargs -r -I{} loginctl show-session {} -p VTNr -p Class | tr '\\n' ' ' | grep 'VTNr=1' | grep 'Class=greeter'"
        )
        machine.sleep(8)  # let the greeter paint
        machine.screenshot("greeter")

    try:
        with subtest("the test user logs in through greetd into Rime Tiling (Hyprland)"):
            machine.send_chars("rime\n")   # no last user yet: focus starts on the username field
            machine.sleep(2)
            machine.send_chars("rime\n")   # password field (the account's real password)
            machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
            print(machine.succeed("loginctl list-sessions --no-legend"))
            machine.succeed(
                "for s in $(loginctl list-sessions --no-legend | awk '$3==\"rime\"{print $1}'); do "
                "loginctl show-session $s -p VTNr -p Class -p Service | tr '\\n' ' '; echo; done | "
                "grep 'VTNr=1' | grep 'Class=user' | grep 'Service=greetd'"
            )
            out = machine.wait_until_succeeds(in_session("hyprctl -i 0 version"), timeout=60)
            assert "Hyprland" in out, out

        with subtest("rime-shell-firstrun seeded the user's Hyprland config"):
            machine.wait_until_succeeds(
                as_rime("systemctl --user show rime-shell-firstrun.service -p Result -p ActiveState | tr '\\n' ' ' "
                        "| grep 'Result=success' | grep 'ActiveState=inactive'"), timeout=90)
            machine.succeed("test -s /home/rime/.config/hypr/hyprland.lua")
            print(machine.succeed(as_rime("journalctl --user -b -u rime-shell-firstrun --no-pager | tail -n 30")))
            rc, left = machine.execute("grep -rn '@KB_' /home/rime/.config/hypr")
            print("unrendered @KB_ placeholders in ~/.config/hypr:", "none" if rc else left)
            assert rc != 0, "firstrun left @KB_ placeholders:\n" + left

        with subtest("Rime Shell runs the shipped config"):
            machine.wait_until_succeeds(quickshell, timeout=120)
            pid = machine.succeed(quickshell + " | head -n1").strip()
            env = machine.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ")
            for line in env.splitlines():
                if line.split("=", 1)[0] in ("QT_PLUGIN_PATH", "RIME_SHELL_DIR", "RIME_QML_DIR", "QML_IMPORT_PATH"):
                    print("quickshell env:", line)
            assert any(l.startswith("QT_PLUGIN_PATH=") and "qt-6/plugins" in l for l in env.splitlines()), \
                "quickshell has no QT_PLUGIN_PATH with a Qt 6 plugin dir"
            # qtimageformats (webp/tiff wallpapers) is reachable through the shell's QT_PLUGIN_PATH.
            qpp = [l for l in env.splitlines() if l.startswith("QT_PLUGIN_PATH=")][0].split("=", 1)[1]
            machine.succeed(
                "for d in " + " ".join(shlex.quote(d) for d in qpp.split(":") if d) + "; do "
                "test -e \"$d/imageformats/libqwebp.so\" && echo \"$d\" && exit 0; done; exit 1")
            # hypridle is the lock screen's timer. rime-shell-autostart prefers ~/.config/hypr/hypridle.conf, which
            # firstrun installs from the image; either way its lock command must reach this shell's IPC.
            idle = machine.succeed("pgrep -u rime -a -x hypridle").strip()
            print("hypridle:", idle)
            idle_conf = idle.split(" -c ", 1)[1].strip()
            lock_cmd = machine.succeed(f"grep '^ *lock_cmd' {idle_conf}").strip()
            assert lock_cmd == "lock_cmd = qs -c " + shell_dir + " ipc call lockscreen lock", lock_cmd
            machine.wait_until_succeeds(in_session("qs -c " + shell_dir + " ipc show | grep lockscreen"), timeout=120)
            machine.wait_until_succeeds(in_session("hyprctl -i 0 layers | grep -E 'namespace: .+'"), timeout=90)
            print(machine.succeed(in_session("hyprctl -i 0 layers")))
            machine.sleep(6)  # the bar's open animation
            machine.screenshot("desktop")
            machine.succeed(f"kill -0 {pid}")
    except Exception:
        dump_logs()
        raise

    with subtest("rimed answers on the system bus and the CLI is on PATH"):
        out = machine.wait_until_succeeds(
            as_rime("busctl --system introspect org.rimeos.Rimed1 /org/rimeos/Rimed1"), timeout=60)
        assert "org.rimeos.Rimed1" in out, out
        machine.succeed("systemctl is-active rimed.service")
        print(machine.succeed(as_rime("command -v rime")))

    with subtest("NetworkManager is up"):
        machine.wait_for_unit("NetworkManager.service")
        print(machine.succeed("nmcli -t -f RUNNING,STATE general status"))

    with subtest("sshd refuses password authentication"):
        machine.wait_for_unit("sshd.service")
        cfg = machine.succeed("sshd -T | grep -Ei '^(passwordauthentication|kbdinteractiveauthentication) '")
        print(cfg)
        cfg = cfg.lower()  # this sshd -T prints the keywords in mixed case
        assert "passwordauthentication no" in cfg and "kbdinteractiveauthentication no" in cfg, cfg
        machine.succeed("ssh-keyscan -T 10 127.0.0.1 | grep ssh-ed25519")  # sshd is listening
        rc, out = machine.execute(
            "sshpass -p rime ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "
            "-o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive "
            "-o ConnectTimeout=10 rime@127.0.0.1 true 2>&1")
        print(f"password login with the correct password: rc={rc} {out}")
        assert rc != 0 and "Permission denied" in out, "sshd accepted a password"

    with subtest("rime-firewall is the firewall, and its table is loaded"):
        machine.wait_for_unit("rime-firewall.service")
        print(machine.succeed("nft list table inet rime | head -n 40"))
        machine.fail("systemctl is-active firewall.service")
        machine.fail("systemctl is-active nftables.service")

    dump_logs()
  '';
}
