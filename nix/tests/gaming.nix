# Gaming Mode on NixOS (nix/modules/gaming.nix): the session entry the greeter offers, gamescope/Steam and Rime's
# wrappers resolve, rime-game-env's output, gamemoded, NTSYNC, the rtprio limits, the CAP_SYS_NICE trap and its
# fix, and steamos-session-select under no_new_privs. No GPU: nothing here starts gamescope or Steam's client.
#
# Also carries the A4 closure measurement as extra attributes (not built by `nix flake check`):
#   nix build path:.#checks.x86_64-linux.gaming.measure.{gaming-off-32bit-off,gaming-off-32bit-on,gaming-on}
# then `nix path-info -S` / `-rS` on each out-link.
{ pkgs, self }:

let
  lib = pkgs.lib;

  # A desktop-like baseline on top of rime-vm (whose area modules are still stubs on this branch), so the gaming
  # delta is measured against a system that already has 64-bit graphics, PipeWire and fonts.
  measureBase = { lib, ... }: {
    hardware.graphics.enable = true;
    services.pipewire = {
      enable = true;
      alsa.enable = true;
      pulse.enable = true;
    };
    fonts.enableDefaultPackages = true;
  };

  variant = gaming: graphics32: (self.nixosConfigurations.rime-vm.extendModules {
    modules = [
      measureBase
      {
        rime.gaming.enable = lib.mkForce gaming;
        hardware.graphics.enable32Bit = lib.mkForce graphics32;
      }
    ];
  }).config.system.build.toplevel;

  test = pkgs.testers.runNixOSTest {
    name = "rime-gaming";

    # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
    node.pkgsReadOnly = false;

    nodes.machine = { pkgs, ... }: {
      imports = [ self.nixosModules.default ];

      rime.gaming.enable = true;

      users.users.rime = {
        isNormalUser = true;
        uid = 1000;
        extraGroups = [ "wheel" ];
        # A user manager without a graphical login: gamemoded is a user service, and the steamos-session-select
        # hop runs through `systemd-run --user`.
        linger = true;
      };

      # A setcap wrapper made exactly the way programs.gamescope.capSysNice makes gamescope's, around `env`, so the
      # test can show what gamescope's children would inherit without a GPU to run gamescope on.
      security.wrappers.rime-captest = {
        owner = "root";
        group = "root";
        source = "${pkgs.coreutils}/bin/env";
        capabilities = "cap_sys_nice+pie";
      };

      virtualisation = {
        memorySize = 3072;
        cores = 2;
      };
    };

    testScript = { nodes, ... }:
      let
        m = nodes.machine;
        g = m.rime.gaming.package;
        desktops = "${m.services.displayManager.sessionData.desktops}/share/wayland-sessions";
        # A Python string literal (JSON is one), so the shell quoting escapeShellArg makes survives Python.
        asRime = cmd: builtins.toJSON "su - rime -c ${lib.escapeShellArg cmd}";
        userEnv = "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus";
      in
      ''
        import re

        def field(pattern: str, text: str) -> str:
            hit = re.search(pattern, text, re.M)
            assert hit is not None, f"{pattern!r} not found in:\n{text}"
            return hit.group(1)

        machine.wait_for_unit("multi-user.target")

        with subtest("the Gaming Mode entry is in the display manager's sessions and runs store paths"):
            machine.succeed("test -f ${desktops}/rime-gaming.desktop")
            machine.succeed("test -f /etc/rime/wayland-sessions/rime-gaming.desktop")
            entry = machine.succeed("cat /etc/rime/wayland-sessions/rime-gaming.desktop")
            print(entry)
            exec_line = field(r"^Exec=(.*)$", entry)
            tryexec = field(r"^TryExec=(.*)$", entry)
            # rime-session-select only autologins into an Exec made of these characters.
            assert re.fullmatch(r"[A-Za-z0-9/._ -]+", exec_line), exec_line
            machine.succeed(f"test -x {exec_line}")
            machine.succeed(f"test -x {tryexec}")
            machine.succeed(f"bash -n {exec_line}")
            # The greeter's own enumeration (Containerfile.rime's ENUM) offers it, and its Name names no compositor.
            offered = machine.succeed(
                "for f in /etc/rime/wayland-sessions/*.desktop; do id=$(basename \"$f\" .desktop); "
                "t=$(sed -n 's/^TryExec=//p' \"$f\" | head -n1); "
                "if [ -n \"$t\" ] && ! command -v \"$t\" >/dev/null 2>&1; then continue; fi; "
                "printf '%s\\n' \"$id\"; done"
            )
            assert "rime-gaming" in offered.split(), offered
            name = field(r"^Name=(.*)$", entry).lower()
            assert not re.search(r"gamescope|hyprland|niri|labwc|sway", name), name

        with subtest("gamescope, steam and Rime's wrappers resolve for the user"):
            gs = machine.succeed(${asRime "command -v gamescope"}).strip()
            st = machine.succeed(${asRime "command -v steam"}).strip()
            print(f"gamescope={gs} steam={st}")
            machine.succeed(f"test -x {gs}")
            print(machine.execute(${asRime "gamescope --help 2>&1 | head -n 3"})[1])
            machine.succeed("test -x ${g}/libexec/rime-gamescope-steam")
            machine.succeed("test -x ${g}/libexec/rime-gamescope-steam-nocaps")
            machine.succeed(${asRime "command -v steamos-session-select && command -v steamos-update"})
            rc = machine.execute(${asRime "steamos-update check"})[0]
            assert rc == 7, f"steamos-update answered {rc}, expected SteamOS's 'no update' (7)"
            # Steam's Gaming Mode Power menu runs these from inside Steam's FHS environment.
            inside = machine.succeed(${asRime "steam-run sh -c 'command -v steamos-session-select'"}).strip()
            print(f"inside steam-run: {inside}")
            # The wrapper hands Steam the game environment and the volume handler, then execs the command.
            out = machine.succeed(${asRime "${g}/libexec/rime-gamescope-steam env 2>/dev/null"})
            assert "STEAM_ENABLE_VOLUME_HANDLER=1" in out, out
            assert "__GL_SHADER_DISK_CACHE_SIZE=12000000000" in out, out
            # The WM_STATE restore's python finds libX11 by store path.
            real = machine.succeed("sed -n 's|.*\\(/nix/store/[a-z0-9]*-rime-gaming-[^/]*/libexec/rime-gamescope-steam\\).*|\\1|p' ${g}/libexec/rime-gamescope-steam | head -n1").strip()
            x11 = machine.succeed(f"grep -o '/nix/store/[^\"]*libX11.so.6' {real}").strip()
            pybin = machine.succeed(f"grep -o '/nix/store/[^ ]*/bin/python3[^ ]*' {real} | head -n1").strip()
            machine.succeed(f"{pybin} -c 'import ctypes; ctypes.CDLL(\"{x11}\")'")

        with subtest("rime-game-env prints what Steam and games should get"):
            env = "${g}/libexec/rime-game-env"
            out = machine.succeed(f"env -u __GL_SHADER_DISK_CACHE_SIZE {env}")
            assert out == "__GL_SHADER_DISK_CACHE_SIZE=12000000000\n", repr(out)
            # Idempotent: an environment it already produced prints nothing.
            out = machine.succeed(f"__GL_SHADER_DISK_CACHE_SIZE=12000000000 {env}")
            assert out == "", repr(out)
            # An NVIDIA GPU older than Ada (RTX 3070 Laptop, 10de:249d) turns opacity micromaps off for Proton.
            machine.succeed(
                "d=/tmp/fakesys/bus/pci/devices/0000:01:00.0; mkdir -p $d; "
                "echo 0x10de > $d/vendor; echo 0x030000 > $d/class; echo 0x249d > $d/device"
            )
            out = machine.succeed(f"RIME_SYSFS=/tmp/fakesys env -u __GL_SHADER_DISK_CACHE_SIZE {env}")
            assert out == "__GL_SHADER_DISK_CACHE_SIZE=12000000000\nVKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap\n", repr(out)
            out = machine.succeed(f"RIME_SYSFS=/tmp/fakesys VKD3D_DISABLE_EXTENSIONS=VK_EXT_x env -u __GL_SHADER_DISK_CACHE_SIZE {env}")
            assert "VKD3D_DISABLE_EXTENSIONS=VK_EXT_x,VK_KHR_opacity_micromap\n" in out, repr(out)
            # Ada (0x2684) is left alone.
            machine.succeed("echo 0x2684 > /tmp/fakesys/bus/pci/devices/0000:01:00.0/device")
            out = machine.succeed(f"RIME_SYSFS=/tmp/fakesys env -u __GL_SHADER_DISK_CACHE_SIZE {env}")
            assert "VKD3D" not in out, repr(out)
            # Login shells get it (bootc: /etc/profile.d), including greetd's `sh -c '. /etc/profile; ...'`.
            out = machine.succeed(${asRime "echo $__GL_SHADER_DISK_CACHE_SIZE"}).strip()
            assert out == "12000000000", repr(out)
            out = machine.succeed("su rime -c 'env -i HOME=/home/rime /bin/sh -c \". /etc/profile; echo \\$__GL_SHADER_DISK_CACHE_SIZE\"'").strip()
            assert out == "12000000000", repr(out)
            out = machine.succeed("su rime -s /run/current-system/sw/bin/zsh -l -c 'echo $__GL_SHADER_DISK_CACHE_SIZE' 2>&1 || true").strip()
            print(f"zsh login: {out!r}")

        with subtest("gamemoded starts and drives Rime game mode"):
            ini = machine.succeed("cat /etc/gamemode.ini")
            print(ini)
            rime_cli = field(r"^start=(\S+) game start$", ini)
            assert re.search(r"^end=" + re.escape(rime_cli) + r" game stop$", ini, re.M), ini
            assert re.search(r"^desiredgov=performance$", ini, re.M), ini
            machine.succeed(f"test -x {rime_cli}")
            machine.wait_for_unit("user@1000.service")
            machine.succeed(${asRime "${userEnv} systemctl --user start gamemoded.service"})
            machine.wait_until_succeeds(${asRime "${userEnv} systemctl --user is-active gamemoded.service"})
            status = machine.succeed(${asRime "${userEnv} gamemoded -s"})
            print(status)
            assert "gamemode is inactive" in status, status
            # The renice capability comes from the setcap wrapper, not from the user's session.
            machine.succeed("${pkgs.libcap}/bin/getcap /run/wrappers/bin/gamemoded | grep cap_sys_nice")

        with subtest("NTSYNC is loaded at boot and open to every user"):
            machine.succeed("lsmod | grep -w ntsync")
            machine.succeed("test -c /dev/ntsync")
            mode = machine.succeed("stat -c %a /dev/ntsync").strip()
            assert mode == "666", mode

        with subtest("wheel gets the realtime and nice limits (bootc 30-rime-gaming-rtprio.conf)"):
            assert machine.succeed(${asRime "ulimit -Hr"}).strip() == "20"
            # RLIMIT_NICE = 20 - (-19)
            assert machine.succeed(${asRime "ulimit -He"}).strip() == "39"

        with subtest("CAP_SYS_NICE from a setcap wrapper reaches the child, and the Steam hand-off clears it"):
            amb = machine.succeed(${asRime "/run/wrappers/bin/rime-captest grep ^CapAmb: /proc/self/status"})
            print(amb)
            assert int(amb.split()[1], 16) & (1 << 23), "the setcap wrapper did not make cap_sys_nice ambient"
            rc, out = machine.execute(${asRime "/run/wrappers/bin/rime-captest steam-run true 2>&1"})
            print(f"steam-run with an ambient capability: rc={rc}\n{out}")
            cleared = machine.succeed(
                ${asRime "/run/wrappers/bin/rime-captest ${g}/libexec/rime-gamescope-steam-nocaps grep -E '^Cap(Prm|Eff|Inh|Amb):' /proc/self/status 2>/dev/null"}
            )
            print(cleared)
            for line in cleared.splitlines():
                assert int(line.split()[1], 16) == 0, line
            machine.succeed(${asRime "/run/wrappers/bin/rime-captest ${g}/libexec/rime-gamescope-steam-nocaps steam-run true 2>/dev/null"})

        with subtest("steamos-session-select switches to Gaming Mode from inside no_new_privs"):
            # The trap: under no_new_privs (Steam's bubblewrap) the setuid sudo cannot work.
            rc, out = machine.execute(${asRime "setpriv --no-new-privs /run/wrappers/bin/sudo -n true 2>&1"})
            print(f"sudo under no_new_privs: rc={rc} {out}")
            assert rc != 0
            machine.succeed("rm -f /var/lib/rime-greet/last-session")
            machine.succeed(${asRime "${userEnv} setpriv --no-new-privs steamos-session-select gaming"})
            machine.wait_until_succeeds("test \"$(cat /var/lib/rime-greet/last-session)\" = rime-gaming")
            # rime-session-select refuses a session that is not installed.
            rc, out = machine.execute(${asRime "sudo -n $(readlink -f ${g}/libexec/rime-session-select) no-such-session 2>&1"})
            assert rc != 0 and "not an installed session" in out, out
      '';
  };
in
test // {
  measure = {
    gaming-off-32bit-off = variant false false;
    gaming-off-32bit-on = variant false true;
    gaming-on = variant true true;
  };
}
