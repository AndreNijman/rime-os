# Rime Shell in a real Hyprland session: a user is logged in on tty1 (getty autologin, test only), starts
# Hyprland, Hyprland runs the module's rime-shell-autostart, and the test checks that
#   - quickshell runs with the Rime config at the path contract's directory (nix/pkgs/rime-shell.nix),
#   - that instance answers IPC on exactly that path (hypridle's lock_cmd and every keybind depend on it),
#   - the Rime.I18n module loaded (it is built with the package's own Qt),
#   - the shell mapped its layer surfaces (the bar) on the output, and a screenshot is kept,
#   - the shell's log has no QML load errors.
# The VM has no 3D GPU: Hyprland renders with Mesa's software rasteriser on virtio-gpu.
{ pkgs, self }:

let
  user = "alice";
  shellDir = "/run/current-system/sw/share/rime-shell";

  # Hyprland 0.57 reads only Lua. The real session config (files/desktop/hypr, the desktop port) is not used:
  # this test is about the shell, so the session is the smallest one that starts it the way session.lua does.
  hyprConf = pkgs.writeText "hyprland.lua" ''
    hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1.0 })
    hl.on("hyprland.start", function()
        hl.exec_cmd("rime-shell-autostart > /tmp/rime-shell.log 2>&1")
    end)
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-shell";

  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];

    # Only the shell part of the desktop module: the session plumbing is the desktop port's and is tested there.
    rime.desktop.shell.enable = true;
    programs.hyprland.enable = true;

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      # A KMS device without virgl: Mesa falls back to kms_swrast/llvmpipe (as nixos/tests/sway.nix does).
      qemu.options = [ "-vga none -device virtio-gpu-pci" ];
    };
    hardware.graphics.enable = true;

    users.users.${user} = {
      isNormalUser = true;
      uid = 1000;
      password = "rime-test";
    };

    # Test only: the product logs in through greetd/rime-greet.
    services.getty.autologinUser = user;
    environment.loginShellInit = ''
      if [ "$(tty)" = /dev/tty1 ] && [ "$(id -un)" = ${user} ]; then
        mkdir -p ~/.config/hypr
        ln -sf ${hyprConf} ~/.config/hypr/hyprland.lua
        exec start-hyprland > /tmp/hyprland.log 2>&1
      fi
    '';
  };

  testScript = ''

    # nixpkgs wraps Hyprland and quickshell in binary wrappers: the process name is ".Hyprland-wrapp", while
    # the command line keeps the name it was started by. Match command lines, never process names.
    hyprland = "pgrep -u ${user} -f '(^|/)Hyprland( |$)'"

    def as_user(cmd):
        # As a client inside the session: the user's runtime directory and the compositor's Wayland socket.
        # qs ipc only lists instances on the caller's display, and the test shell's own DISPLAY is not it.
        return (
            "sudo -u ${user} env -u DISPLAY XDG_RUNTIME_DIR=/run/user/1000 "
            "WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1) " + cmd
        )

    def dump_logs():
        # On any failure: what the compositor and the launcher said, and what is running.
        print(machine.execute(
            "for f in /tmp/hyprland.log /tmp/rime-shell.log; do echo \"== $f\"; tail -n 200 $f; done; "
            "echo '== processes'; ps -u ${user} -o pid,args"
        )[1])
        print(machine.execute(as_user("qs -c ${shellDir} ipc show 2>&1; qs list --all 2>&1"))[1])

    machine.wait_for_unit("multi-user.target")

    with subtest("Hyprland starts on tty1 for the autologin user"):
        machine.wait_until_succeeds(hyprland, timeout=120)
        machine.wait_until_succeeds(as_user("hyprctl -i 0 monitors"), timeout=60)

    try:
        with subtest("quickshell runs with the Rime config at the contract path"):
            machine.wait_until_succeeds("grep -F 'starting quickshell -c ${shellDir}' /tmp/rime-shell.log", timeout=60)
            machine.wait_until_succeeds("pgrep -u ${user} -f '^[^ ]*quickshell -c ${shellDir}$'", timeout=60)
            pid = machine.succeed("pgrep -u ${user} -f '^[^ ]*quickshell -c ${shellDir}$' | head -n1").strip()
            machine.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ | grep -Fx 'RIME_SHELL_DIR=${shellDir}'")
            machine.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ | grep -E '^QML_IMPORT_PATH=/nix/store/[^:]*-rime-shell-[^:]*/lib/qt-6/qml'")
            # hypridle is the lock screen's timer and must run the shell's own config.
            machine.succeed("pgrep -u ${user} -f '^[^ ]*hypridle -c ${shellDir}/src/config/hypridle.conf$'")

        with subtest("the instance answers IPC on the same path the lock command uses"):
            machine.wait_until_succeeds(as_user("qs -c ${shellDir} ipc show"), timeout=90)
            targets = machine.succeed(as_user("qs -c ${shellDir} ipc show"))
            print(targets)
            assert "lockscreen" in targets, "no lockscreen IPC target"
            lock_cmd = machine.succeed("grep '^ *lock_cmd' ${shellDir}/src/config/hypridle.conf").strip()
            assert lock_cmd == "lock_cmd = qs -c ${shellDir} ipc call lockscreen lock", lock_cmd

        with subtest("Rime.I18n loaded"):
            machine.wait_until_succeeds("grep -F 'RIMEI18N: registerTypes uri=Rime.I18n' /tmp/rime-shell.log", timeout=60)

        with subtest("the bar is mapped as a layer surface"):
            machine.wait_until_succeeds(as_user("hyprctl -i 0 layers | grep -E 'namespace: .+'"), timeout=90)
            # Give the bar's open animation time to finish before the picture.
            machine.sleep(5)
            print(machine.succeed(as_user("hyprctl -i 0 layers")))
            machine.screenshot("rime-shell")

        with subtest("no QML load errors"):
            import re
            shell_log = machine.succeed("cat /tmp/rime-shell.log")
            print(shell_log)
            # QML errors only: a component or module that does not load. Run-time warnings from services this VM
            # does not have (NetworkManager, BlueZ, a battery) are printed above but are not load errors.
            qml_error = re.compile(
                r"Failed to load configuration|is not installed|is not a type|module \"[^\"]+\" plugin .* not found"
                r"|Type \S+ unavailable|Cannot assign to non-existent property|Expected token|Syntax ?Error"
            )
            bad = [line for line in shell_log.splitlines() if qml_error.search(line)]
            assert not bad, "QML errors in the shell log:\n" + "\n".join(bad)
            # quickshell keeps running after a load error (it waits for a reload), so it must still be the same process.
            machine.succeed(f"kill -0 {pid}")
    except Exception:
        dump_logs()
        raise
    # Kept in the test output either way: what the shell said while it ran.
    dump_logs()
  '';
}
