# Gaming Mode starts Steam and runs a game, offline, in a VM with a software GPU (docs/nixos/p2-gates.md P2).
#
# The session starts the way the greeter starts it: greetd's initial_session runs the offered entry's Exec as
# `sh -lc 'exec <Exec>'` (the form rime-session-select's one-shot switch.toml uses, checked in nix/tests/gaming.nix)
# for the user. The only change from a real machine is RIME_GAMESCOPE_ARGS="--backend headless ...": the VM has no
# GPU, so gamescope composites with Mesa's lavapipe (software Vulkan: the GPU stub) and presents nowhere, and the
# picture is taken with gamescope's own screenshot request (gamescopectl).
#
# Offline, Steam's client exits within a second of starting ("Steam needs to be online to update"), which ends the
# session. So the test points Steam's update host at a local port that accepts and never answers: the real client
# then stays up, waiting on its download, inside the real session.
#
# Proven: greetd -> rime-gaming-session -> gamescope -> rime-gamescope-steam-nocaps -> rime-gamescope-steam -> the
# real `steam` (nixpkgs' FHS wrapper) -> Valve's bootstrap -> Steam's own 32-bit client binary, running inside
# gamescope with the environment rime-gamescope-steam gives it. Then a stub game (vkcube, a Vulkan program) is started
# the way Steam starts a game in this session: inside Steam's FHS environment (steam-run), with the running Steam
# client's own environment (the game environment, gamescope's DISPLAY), named as Steam names a game to gamescope
# (STEAM_GAME on its window, GAMESCOPECTRL_BASELAYER_APPID on the root: the test sets both, as Steam would); gamescope
# focuses it (GAMESCOPE_FOCUSED_APP) and composites it: gamescope's own screenshot shows the CUBE, told apart from any
# other picture (Steam's UI, a black or grey frame) by its LunarG teal, at least 1% of the frame (the cube's two faces
# measure about 5%). Shots are taken until one shows it, for up to 90 s, and a failed screenshot request is retried
# while gamescope still runs; a second, different picture of the cube (it turned) is reported, not required.
#
# NOT proven (needs the network, a Steam account or real hardware): Steam logging in, updating, and launching an
# installed game itself (`steam -applaunch`), Proton, and anything on a real GPU (P3 on katana).
#
# Mutants (built by hand, each must FAIL):
#   checks.gaming-launch.extendNixOS with RIME_GAMESCOPE_STEAM=/nonexistent: a session that starts Steam without
#     rime-gamescope-steam fails the environment assertions.
#   checks.gaming-launch.mutants.game-unmapped: the game runs and is focused, then its window is unmapped, so
#     gamescope composites something else: fails at "gamescope never composited the game".
{ pkgs, self }:

let
  user = "rime";
  uid = 1000;
  appid = "480";

  startEntry = pkgs.writeShellScript "rime-start-gaming-entry" ''
    exec >>/tmp/rime-gaming-session.log 2>&1
    exec_line=$(${pkgs.gnused}/bin/sed -n 's/^Exec=//p' /etc/rime/wayland-sessions/rime-gaming.desktop)
    exec /bin/sh -lc "exec $exec_line"
  '';

  # Runs a command as the user with the running Steam client's exact environment (PID $1): the environment Steam
  # hands a game it starts (gamescope's DISPLAY, the Steam runtime, rime-gamescope-steam's variables).
  inSteamEnv = pkgs.writeShellScript "rime-in-steam-env" ''
    set -eu
    pid=$1; shift
    mapfile -d "" envs < /proc/"$pid"/environ
    cd /home/${user}
    exec ${pkgs.util-linux}/bin/runuser -u ${user} -- ${pkgs.coreutils}/bin/env -i "''${envs[@]}" "$@"
  '';

  # The stub game: vkcube (Vulkan) inside Steam's FHS runtime (steam-run), with Steam's app id variables.
  launchGame = pkgs.writeShellScript "rime-launch-stub-game" ''
    set -eu
    ${pkgs.util-linux}/bin/setsid ${inSteamEnv} "$1" ${pkgs.coreutils}/bin/env \
      SteamAppId=${appid} SteamGameId=${appid} \
      /run/current-system/sw/bin/steam-run ${pkgs.vulkan-tools}/bin/vkcube --wsi xcb \
      </dev/null >/tmp/game.log 2>&1 &
  '';

  # The share of a raw RGB frame (stdin) in the cube's LunarG teal: blue and green well above red, and close together
  # (Steam's own blue is far bluer than it is green).
  teal = pkgs.writeText "teal-share.py" ''
    import sys
    data = sys.stdin.buffer.read()
    r, g, b = data[0::3], data[1::3], data[2::3]
    n = sum(1 for x, y, z in zip(r, g, b) if z > x + 30 and y > x + 20 and abs(z - y) < 40)
    print(f"{n / max(len(r), 1):.4f}")
  '';

  # Accepts on 80 and 443 and never answers: Steam's update download waits instead of failing at once.
  stall = pkgs.writeText "steam-update-stall.py" ''
    import selectors, socket
    sel, held = selectors.DefaultSelector(), []
    for port in (80, 443):
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", port))
        s.listen(64)
        sel.register(s, selectors.EVENT_READ)
    while True:
        for key, _ in sel.select():
            conn, _ = key.fileobj.accept()
            held.append(conn)
  '';
  mk = mutant: pkgs.testers.runNixOSTest {
  name = "rime-gaming-launch" + pkgs.lib.optionalString (mutant != null) "-mutant-${mutant}";
  node.pkgsReadOnly = false;

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];

    rime.gaming.enable = true;
    # Mesa: llvmpipe + lavapipe, the GPU stub gamescope renders with.
    hardware.graphics.enable = true;

    users.users.${user} = {
      isNormalUser = true;
      inherit uid;
      extraGroups = [ "wheel" "video" "input" ];
    };

    services.greetd = {
      enable = true;
      settings = {
        default_session.command = "${pkgs.coreutils}/bin/sleep infinity";
        # Read from the entry at run time: rime.gaming.package itself depends on greetd's settings.
        initial_session = {
          inherit user;
          command = "${startEntry}";
        };
      };
    };

    environment.variables = {
      # The VM has no display device gamescope can drive; everything else in the session is as shipped.
      RIME_GAMESCOPE_ARGS = "--backend headless -W 1280 -H 800";
      # No desktop to go back to.
      RIME_GAMING_NO_RETURN = "1";
    };

    networking.hosts."127.0.0.1" = [ "client-update.steamstatic.com" ];
    systemd.services.steam-update-stall = {
      wantedBy = [ "multi-user.target" ];
      before = [ "greetd.service" ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${stall}";
    };

    environment.systemPackages = [ pkgs.vulkan-tools pkgs.procps pkgs.python3 pkgs.xprop pkgs.xwininfo pkgs.xdotool pkgs.file pkgs.imagemagick ];

    virtualisation = {
      memorySize = 6144;
      cores = 4;
      diskSize = 8192;
    };
  };

  testScript = ''
    def sh(cmd):
        rc, out = machine.execute(cmd)
        print(f"$ {cmd}\n[rc={rc}]\n{out}")
        return rc, out

    CLIENT = "pgrep -u ${user} -f '^/home/${user}/.local/share/Steam/ubuntu12_32/steam '"
    TREE = "ps -u ${user} -o pid,ppid,etimes,args --forest | cut -c1-240"

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("steam-update-stall.service")
    machine.wait_for_unit("greetd.service")

    with subtest("greetd starts Gaming Mode: gamescope runs for the user"):
        machine.wait_until_succeeds("pgrep -u ${user} -x gamescope-wl || pgrep -u ${user} -x gamescope", timeout=300)

    with subtest("Steam's own client binary starts inside the session"):
        machine.wait_until_succeeds(CLIENT, timeout=600)
        pid = machine.succeed(CLIENT).split()[0]
        machine.succeed(f"cp /proc/{pid}/environ /tmp/client.env")
        launched = machine.execute("${launchGame} " + pid)
        _, tree = sh(TREE)
        env = machine.succeed("tr '\\0' '\\n' < /tmp/client.env | sort")
        print(env)
        # Its ancestry is the session's: gamescope -> rime-gamescope-steam -> steam -> the client.
        for part in ("rime-gaming-session", "gamescope", "rime-gamescope-steam", "steam.sh", "ubuntu12_32/steam "):
            assert part in tree, f"{part!r} not in the process tree"
        assert "GAMESCOPE_WAYLAND_DISPLAY=gamescope-0" in env, "the client is not inside gamescope"
        # Set by rime-gamescope-steam and nowhere else.
        assert "STEAM_ENABLE_VOLUME_HANDLER=1" in env, "the client did not get rime-gamescope-steam's environment"
        assert "__GL_SHADER_DISK_CACHE_SIZE=12000000000" in env, "the client did not get rime-game-env's environment"
        assert "-gamepadui" in tree and "-steamos3" in tree, "Steam was not started with the session's arguments"

    with subtest("a stub game started the way Steam starts one is composited by gamescope"):
        assert launched[0] == 0, launched[1]
        machine.wait_until_succeeds("pgrep -u ${user} -x vkcube", timeout=60)
        X = "${inSteamEnv} " + pid + " "
        win = machine.wait_until_succeeds(X + "sh -c 'xwininfo -root -tree | grep -i vkcube'", timeout=60)
        print(win)
        wid = win.split()[0]
        # What Steam does for a game it starts: name the window and the base layer with the app id.
        machine.succeed(X + f"xprop -id {wid} -f STEAM_GAME 32c -set STEAM_GAME ${appid}")
        machine.succeed(X + "xprop -root -f GAMESCOPECTRL_BASELAYER_APPID 32c -set GAMESCOPECTRL_BASELAYER_APPID ${appid}")
        focus = machine.wait_until_succeeds(X + "sh -c 'xprop -root GAMESCOPE_FOCUSED_APP | grep -w ${appid}'", timeout=30)
        print(focus)
        sh(X + "xprop -root | grep -i -E '^GAMESCOPE' | cut -c1-200")
        # The mutant (mutants.game-unmapped) takes the game off screen here; gamescope composites whatever else there is.
        ${if mutant == "game-unmapped" then ''machine.succeed(X + f"xdotool windowunmap {wid}")'' else "pass"}
        # gamescope's own screenshot of what it composites, until one shows the cube. Some shots catch a black frame
        # (nothing committed yet), the cube can sit on one frame for seconds under load, and a screenshot request can
        # fail while gamescope is busy: retry for up to 90 s while gamescope runs. One picture of the cube is the
        # gate; a second, different one (the cube turned) is reported, not required.
        import time
        good, shots, deadline, n = {}, [], time.monotonic() + 90, 0
        while time.monotonic() < deadline and len(good) < 2:
            time.sleep(3)
            n += 1
            shot = f"/tmp/shot{n}.png"
            rc, out = machine.execute(X + f"gamescopectl screenshot {shot}")
            if rc != 0:
                if machine.execute("pgrep -u ${user} -x gamescope-wl || pgrep -u ${user} -x gamescope")[0] != 0:
                    print(f"gamescope has exited: {out}")
                    break
                print(f"screenshot request failed ({rc}), retrying: {out.strip()}")
                continue
            rc, _ = machine.execute(f"timeout 30 sh -c 'until identify {shot} >/dev/null 2>&1; do sleep 1; done'")
            if rc != 0:
                print(f"{shot} never became a readable picture, retrying")
                continue
            _, info = sh(f"identify -format '%m %w %h %#' {shot}")
            fmt, w, h, digest = info.split()
            assert fmt == "PNG" and (w, h) == ("1280", "800"), info
            share = float(machine.succeed(f"magick {shot} -depth 8 rgb:- | python3 ${teal}").strip())
            shots.append((shot, share))
            print(f"{shot}: cube teal {share:.2%}")
            if share >= 0.01:
                good.setdefault(digest, shot)
        sh(TREE)
        sh("tail -n 40 /tmp/game.log")
        print("shots (picture, cube teal share):", shots)
        assert good, "gamescope never composited the game (no screenshot shows the cube)"
        print(f"pictures of the game: {len(good)} distinct ({'the cube turned' if len(good) > 1 else 'one frame'})")
        for name, shot in zip(("game-a.png", "game-b.png"), good.values()):
            machine.succeed(f"cp {shot} /tmp/{name}")
            machine.copy_from_machine(f"/tmp/{name}", "")

    sh("cut -c1-300 /tmp/rime-gaming-session.log | tail -n 40")
  '';
};
in
mk null // { mutants.game-unmapped = mk "game-unmapped"; }
