# rime-libexec on a running system: every shipped script's interpreter and PATH resolve, the scripts with a
# --help / --self-test / --list run it without a missing command, the sudo rules match (through the
# /run/current-system/sw/libexec symlink as well as the store path), and the units the base module adds load.
{ pkgs, self }:

let
  # Runs inside the VM. For every rime-libexec script: the #! interpreter is executable, and every
  # directory in the PATH line the build injected exists. Prints one line per failure, exits 1 on any.
  resolveCheck = pkgs.writeShellScript "rime-libexec-resolve-check" ''
    set -u
    sw=/run/current-system/sw
    fail=0
    n=0
    for f in "$sw"/libexec/rime-* "$sw"/libexec/rime/tools/* "$sw"/bin/apex "$sw"/bin/steamos-session-select "$sw"/bin/steamos-update; do
      [ -f "$f" ] || continue
      n=$((n + 1))
      first=$(head -n1 "$f")
      case "$first" in
        '#!'*) ;;
        *) case "$f" in *.js) continue ;; esac; echo "FAIL $f: no #! line"; fail=1; continue ;;
      esac
      interp=''${first#\#!}
      interp=''${interp%% *}
      if [ ! -x "$interp" ]; then echo "FAIL $f: interpreter $interp is not executable"; fail=1; fi
      line=$(grep -m1 '# rime-libexec$' "$f" || true)
      case "$f" in *.js) continue ;; esac
      if [ -z "$line" ]; then echo "FAIL $f: no injected PATH line"; fail=1; continue; fi
      dirs=$(printf '%s\n' "$line" | grep -oE '/nix/store/[^:"]+/bin' || true)
      for d in $dirs; do
        [ -d "$d" ] || { echo "FAIL $f: PATH dir $d missing"; fail=1; }
      done
    done
    echo "checked $n scripts"
    exit $fail
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-libexec";

  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    # The desktop area is still a skeleton on this branch; turning it on adds the desktop-gated libexec units.
    rime.desktop.enable = true;
    # rime-switch-greeter@ needs greetd's binary; the stock agreety greeter is enough for the unit to load.
    services.greetd.enable = true;
    users.users.rime = { isNormalUser = true; extraGroups = [ "wheel" ]; };
    users.users.guest = { isNormalUser = true; };
    programs.zsh.enable = true; # so the zsh half of the profile snippets is exercised too
    # The session set is the desktop area's (/etc/rime/wayland-sessions = the display manager's sessions); the
    # skeleton desktop on this branch has none, so the test supplies Rime's own entries there.
    environment.etc."rime/wayland-sessions".source = "${pkgs.rime.rime-libexec}/share/wayland-sessions";
    virtualisation.memorySize = 2048;
  };

  testScript = ''
    import re

    sw = "/run/current-system/sw"
    machine.wait_for_unit("multi-user.target")

    with subtest("every script's interpreter and PATH resolve"):
        print(machine.succeed("${resolveCheck} 2>&1"))

    def run_as(user, cmd):
        # (status, stdout+stderr) of cmd run by user through a plain sh, not a login shell
        return machine.execute(f"su -s /bin/sh {user} -c '{cmd}' 2>&1")

    missing = re.compile(r"command not found|: not found|No such file or directory|cannot execute")

    # Only scripts whose own parser handles the flag (read off each script), so nothing here acts on the VM.
    probes = [
        "rime-browser --help", "rime-devices --help", "rime-disposable --help", "rime-env --help",
        "rime-firewall --help", "rime-plugin --help", "rime-safe-graphics --help",
        "rime-screen-reader --help", "rime-session-watchdog --help", "rime-switch-user --help",
        "rime-user --help", "rime-vm --help", "rime-luks-enroll --help", "rime-game-env", "rime-display-apply --self-test", "rime-hypr-migrate --self-test",
        "rime-input-apply --self-test",
    ]
    with subtest("scripts run their --help / --self-test without a missing command"):
        bad = []
        # (user, probe): rime-session-select refuses to run as anyone but root (it is a sudo target)
        # these exit non-zero by design when they print their usage
        usage_exits = {"rime-switch-user --help": 2, "rime-user --help": 2, "rime-luks-enroll --help": 1}
        outputs = {}
        for user, p in [("rime", p) for p in probes] + [("root", "rime-session-select --list")]:
            status, out = run_as(user, f"{sw}/libexec/{p}")
            outputs[p] = out
            print(f"--- {p}: exit {status}\n{out[-1500:]}")
            if status != usage_exits.get(p, 0):
                bad.append(f"{p}: exit {status}, expected {usage_exits.get(p, 0)}")
            for l in out.splitlines():
                if missing.search(l):
                    bad.append(f"{p}: {l}")
        assert not bad, "missing commands or wrong exits:\n" + "\n".join(bad)
        # the session list is read from /etc/rime/wayland-sessions (bootc: /usr/share/wayland-sessions)
        listed = outputs["rime-session-select --list"].split()
        for sid in ["niri", "rime-labwc", "rime-safe-graphics"]:
            assert sid in listed, f"rime-session-select --list does not name {sid}: {listed}"

    with subtest("sudo -l -U rime lists the libexec rules"):
        out = machine.succeed("sudo -l -U rime")
        print(out)
        for name in ["rime-boot-windows", "rime-greet-wallpaper", "rime-switch-greeter"]:
            assert re.search(rf"/nix/store/[^ ]+/libexec/{name} ", out), f"{name} (store path) not in sudo -l"
            assert f"{sw}/libexec/{name} " in out, f"{name} ({sw} path) not in sudo -l"
        out = machine.succeed("sudo -l -U guest")
        print(out)
        assert "rime-greet-wallpaper" in out and "rime-switch-greeter" in out
        assert "rime-boot-windows" not in out, "a non-wheel user got the Windows-boot rule"

    refused = re.compile(r"password is required|not allowed|not in the sudoers|may not run")

    with subtest("the rules match through the /run/current-system/sw symlink and the store path"):
        store = machine.succeed(f"readlink -f {sw}/libexec/rime-boot-windows").strip()
        assert store.startswith("/nix/store/"), store
        for path in [f"{sw}/libexec/rime-boot-windows", store]:
            status, out = run_as("rime", f"sudo -n {path} --check")
            print(f"--- sudo -n {path} --check: exit {status}\n{out}")
            assert not refused.search(out), f"sudo refused {path}"
        # ALL: a non-wheel account may publish its wallpaper and open a switch greeter
        for name in ["rime-greet-wallpaper", "rime-switch-greeter"]:
            for path in [f"{sw}/libexec/{name}", machine.succeed(f"readlink -f {sw}/libexec/{name}").strip()]:
                status, out = run_as("guest", f"sudo -n {path}")
                print(f"--- guest: sudo -n {path}: exit {status}\n{out}")
                assert not refused.search(out), f"sudo refused {path} for guest"

    with subtest("the rules are as narrow as on bootc"):
        # "" = no arguments: an argument is refused (the wheel user's default rule wants a password)
        status, out = run_as("rime", f"sudo -n {sw}/libexec/rime-greet-wallpaper /etc/shadow")
        print(out)
        assert status != 0 and refused.search(out), "rime-greet-wallpaper accepted an argument"
        status, out = run_as("rime", f"sudo -n {sw}/libexec/rime-boot-windows --other")
        assert status != 0 and refused.search(out), "rime-boot-windows accepted another argument"
        status, out = run_as("guest", f"sudo -n {sw}/libexec/rime-boot-windows --check")
        assert status != 0 and refused.search(out), "a non-wheel user ran rime-boot-windows"

    with subtest("the libexec units and settings are in place"):
        machine.succeed("test -f /etc/rime/guest-accounts")
        machine.succeed("test -d /var/lib/rime-greet/wallpapers")
        machine.succeed("systemctl cat rime-guest-wipe@guest.service | grep -F '/libexec/rime-guest-wipe %i'")
        machine.succeed("systemctl cat rime-guest-session@1001.service | grep -F 'ExecStop='")
        machine.succeed("systemctl cat rime-switch-greeter@7.service | grep -F '/bin/greetd --config'")
        machine.succeed("systemctl cat sing-box.service | grep -F 'sing-box run'")
        # never started at boot: Rime Shell's VPN toggle starts it
        machine.fail("systemctl is-enabled sing-box.service")
        # resumeCommands land in the ExecStop script of sleep-actions.service (run on wake)
        machine.succeed("grep -F 'rime-wifi-resume post' $(systemctl cat sleep-actions.service | sed -n 's/^ExecStop=//p')")
        machine.succeed("systemd-analyze cat-config systemd/coredump.conf | grep -Fx 'MaxUse=256M'")
        machine.succeed("test -f /etc/xdg/xdg-terminals.list")
        for u in ["rime-gpu-notice.timer", "rime-storage-notice.timer", "rime-shell-firstrun.service"]:
            machine.succeed(f"test -e /etc/systemd/user/*.wants/{u}")
        for unit in ["rime-guest-wipe@x.service", "rime-greet-publish.service", "rime-switch-greeter@7.service"]:
            out = machine.succeed(f"systemd-analyze verify {unit} 2>&1 || true")
            print(f"--- verify {unit}\n{out}")
            assert "is not executable" not in out and "No such file" not in out, out
        print(machine.succeed("systemctl status rime-greet-publish.service 2>&1 || true"))
        assert machine.succeed("systemctl show -P Result rime-greet-publish.service").strip() == "success"
        machine.succeed("journalctl -b -u rime-greet-publish.service | grep -F 'published for'")
        # nothing of rime-libexec is live through a directory NixOS links for every package
        machine.fail(f"test -e {sw}/share/polkit-1/rules.d/49-rime-singbox.rules")
        machine.fail(f"test -e {sw}/etc/xdg/mimeapps.list")

    with subtest("interactive bash and zsh start cleanly with the profile snippets"):
        for sh in ["bash", "zsh"]:
            status, out = run_as("rime", f"{sh} -ic true")
            print(f"--- {sh} -ic true: exit {status}\n{out}")
            assert status == 0 and not missing.search(out), out
            # an agent session (RIME_AGENT_SESSION) gets the tool broker first on PATH, login or interactive
            for flags in ["-lc", "-ic"]:
                out = machine.succeed(f"su -s /bin/sh rime -c 'RIME_AGENT_SESSION=1 {sh} {flags} \"echo \\$PATH\"' 2>/dev/null")
                print(f"--- agent {sh} {flags}: {out.strip()}")
                assert out.strip().split(":")[0] == f"{sw}/libexec/rime/tools", out
  '';
}
