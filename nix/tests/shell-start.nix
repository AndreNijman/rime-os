# shell-start: the Rime Shell starts at login, every time, and a start that fails is retried and then reported.
# The product session (greetd initial_session -> start-hyprland, Rime's Lua config seeded as firstrun does, the Shell
# started by session.lua through rime-shell-autostart), then:
#
#   boot     two IPC clients that name the Shell's config FIRST run from before greetd starts the session, so they are
#            alive when the launcher decides whether a Shell is already running: a held `qs -c <shell> ipc show`
#            command line (what `rime shell ipc`, hypridle's lock_cmd and the VM checks' own polls look like to
#            pgrep, kept alive so the race always happens) and a loop of real `qs -c <shell> ipc show` calls. The
#            Shell must answer IPC, and the journal (tag rime-shell-autostart) must hold the start and its
#            confirmation and no "already running". The launcher used to take these clients for the Shell and exit 0
#            with no bar and no log line: 1 boot in 6 of checks.shell-kept (docs/nixos/open-items.md).
#   retry    a config that dies (SIGKILL) on its first start and answers on a retried one (the launcher sets
#            RIME_SHELL_START_ATTEMPT): two starts, "starting it again (start 2 of 3)", then "Rime Shell is up".
#   report   a config that dies on every start: three starts, an err line, and Hyprland's own notification on screen
#            (report-dead.png). Hyprland draws it, so it needs no Shell.
#
# Mutants (nix build path:.#checks.x86_64-linux.shell-start.mutants.<name>; each must FAIL):
#   clients-count-as-shell  the launcher's pre-fix pgrep pattern (`<dir>( |$)`): fails at boot
#   no-watcher              the launcher execs quickshell with no watcher: fails at retry
#   nix build path:.#checks.x86_64-linux.shell-start -L
{ pkgs, self }:

let
  user = "alice";
  shellDir = "/run/current-system/sw/share/rime-shell";
  libexec = "/run/current-system/sw/libexec";
  launcher = "files/system/libexec/rime-shell-autostart";

  mkTest = name: overlay: pkgs.testers.runNixOSTest {
    inherit name;
    node.pkgsReadOnly = false;

    nodes.machine = { lib, config, ... }: {
      imports = [ self.nixosModules.default ];
      nixpkgs.overlays = lib.optional (overlay != null) overlay;
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
          install -d -o ${user} -g users $h/.config $h/.config/hypr $h/.local $h/.local/share $h/.local/share/hyprland
          cp -r /run/current-system/sw/share/rime/hypr/. $h/.config/hypr/
          grep -rl '@KB_' $h/.config/hypr | xargs -r sed -i -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g'
          chown -R ${user}:users $h/.config $h/.local
          chmod -R u+w $h/.config/hypr   # cp -r keeps the store's read-only modes; firstrun renders into this tree
        '';
      };

      # The IPC clients of the boot subtest, as the session user, up before greetd starts the session.
      systemd.services.rime-test-ipc-clients = {
        wantedBy = [ "greetd.service" ];
        before = [ "greetd.service" ];
        serviceConfig = { User = user; Type = "simple"; };
        path = [ pkgs.coreutils ];
        script = ''
          # held: the command line pgrep sees for `qs -c <shell> ipc show`, alive through the launcher's check
          # (perl rewrites its own command line; coreutils' sleep is a multi-call binary and dies under exec -a)
          ${pkgs.perl}/bin/perl -e '$0 = shift; sleep 900' "qs -c ${shellDir} ipc show" &
          # real: the loop a test (or a keybind held down) runs; qs finds no runtime dir until login, which is fine
          export XDG_RUNTIME_DIR=/run/user/1000
          while :; do /run/current-system/sw/bin/qs -c ${shellDir} ipc show >/dev/null 2>&1 || true; sleep 0.2; done
        '';
      };

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

      def u(cmd):
          return "su - ${user} -c " + shlex.quote(
              "export XDG_RUNTIME_DIR=/run/user/1000; "
              "export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -n1); "
              "export WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1); " + cmd)

      # The launcher's journal lines after the first `mark` of them (a mark is a line count: --since has only
      # second resolution, and the boot's lines can share the second a subtest starts in).
      def jl(mark=0):
          out = machine.succeed("journalctl -b -t rime-shell-autostart -o cat --no-pager")
          return "\n".join(out.splitlines()[mark:])

      def mark():
          return len(jl().splitlines())

      def wait_jl(m, pattern, timeout):
          machine.wait_until_succeeds(
              f"journalctl -b -t rime-shell-autostart -o cat --no-pager | tail -n +{m + 1} | grep -q {shlex.quote(pattern)}",
              timeout=timeout)

      def dump():
          print(machine.execute("ps -eo pid,ppid,stat,etimes,args | grep -v ' \\[' | tail -n 80; "
                                "journalctl -b --no-pager -o short-monotonic | tail -n 150")[1])
          print(machine.execute(u("qs -c ${shellDir} log 2>&1 | tail -n 100"))[1])

      machine.wait_for_unit("multi-user.target")
      machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
      try:
          with subtest("boot: the Shell starts while IPC clients naming its config run"):
              # control: the held client was there for the launcher to see
              machine.succeed("systemctl is-active rime-test-ipc-clients.service")
              machine.succeed("pgrep -u ${user} -f '^qs -c ${shellDir} ipc show' >/dev/null")
              machine.wait_until_succeeds(u("qs -c ${shellDir} ipc show | grep -q dashboard-home"), timeout=180)
              wait_jl(0, "^Rime Shell is up (pid [0-9]*, start 1 of 3", 60)
              out = jl()
              print(out)
              assert "starting quickshell -c ${shellDir} -n (start 1 of 3)" in out, "no start line in the journal"
              assert "already running" not in out, "the launcher took an IPC client for the Shell"
              machine.succeed("systemctl stop rime-test-ipc-clients.service")
              machine.screenshot("boot")

          machine.copy_from_host("${./shell-start/flaky/shell.qml}", "/tmp/qs-flaky/shell.qml")
          machine.copy_from_host("${./shell-start/dead/shell.qml}", "/tmp/qs-dead/shell.qml")
          machine.succeed("chmod -R a+rX /tmp/qs-flaky /tmp/qs-dead")

          with subtest("retry: a start that dies before answering is started again, and the retry is confirmed"):
              m = mark()
              machine.succeed(u("RIME_SHELL_DIR=/tmp/qs-flaky setsid -f ${libexec}/rime-shell-autostart </dev/null >/dev/null 2>&1"))
              wait_jl(m, "^Rime Shell is up", 90)
              out = jl(m)
              print(out)
              assert "starting quickshell -c /tmp/qs-flaky -n (start 1 of 3)" in out, "no first start"
              assert "starting it again (start 2 of 3)" in out, "the dead first start was not retried"
              assert "starting quickshell -c /tmp/qs-flaky -n (start 2 of 3)" in out, "no second start"
              assert "start 2 of 3, answered after" in out, "the retried start was not the confirmed one"
              assert "pong" in machine.succeed(u("quickshell ipc -c /tmp/qs-flaky call probe ping 2>/dev/null")), "the retried shell does not answer"

          with subtest("report: a shell that dies on every start is reported in the journal and on screen"):
              m = mark()
              machine.succeed(u("RIME_SHELL_DIR=/tmp/qs-dead setsid -f ${libexec}/rime-shell-autostart </dev/null >/dev/null 2>&1"))
              wait_jl(m, "3 starts in a row", 120)
              out = jl(m)
              machine.succeed("journalctl -b -t rime-shell-autostart -p err -o cat --no-pager | grep -q '3 starts in a row'")
              print(out)
              assert out.count("starting quickshell -c /tmp/qs-dead -n") == 3, "not three starts"
              assert "hyprctl notify failed" not in out, "the on-screen notice failed"
              machine.sleep(1)
              machine.screenshot("report-dead")
      except Exception:
          dump()
          raise
    '';
  };

  # A mutant: the launcher as packaged, with one substitution (each must make the check FAIL).
  mutant = name: from: to: mkTest "rime-shell-start-mutant-${name}" (_final: prev: {
    rime = prev.rime // {
      rime-libexec = prev.rime.rime-libexec.overrideAttrs (o: {
        postInstall = (o.postInstall or "") + ''
          substituteInPlace $out/libexec/rime-shell-autostart --replace-fail ${pkgs.lib.escapeShellArg from} ${pkgs.lib.escapeShellArg to}
        '';
      });
    };
  });
in
mkTest "rime-shell-start" null // {
  mutants = {
    clients-count-as-shell = mutant "clients-count-as-shell"
      ''SHELL_RE="^([^ ]*/)?(quickshell|qs)( -[^ ]+)* -(c|p) ''${SHELL_DIR}/?( -[^ ]+)*$"''
      ''SHELL_RE="^([^ ]*/)?(quickshell|qs) -(c|p) ''${SHELL_DIR}/?( |$)"'';
    no-watcher = mutant "no-watcher" ''( watch_start "$$" </dev/null & )'' ":";
  };
}
