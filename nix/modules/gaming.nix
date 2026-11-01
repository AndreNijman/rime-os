# Gaming: Steam, Rime Gaming Mode (gamescope session), gamemode, NTSYNC, the game environment
# (PLAN.md 4.5, 4.8; decision A4).
#
# Ported from the bootc image (Containerfile.rime :100-262, Containerfile.core :1228-1262) as behaviour, not paths:
#   - the Gaming Mode scripts ship unchanged from files/**; the only edits are build-time path substitutions
#     (see rimeGaming below) and env defaults in their wrappers, which every script already honours;
#   - steam / gamescope / gamemode come from the upstream NixOS modules (programs.steam, programs.gamescope,
#     programs.gamemode). Rime's own session is used, NOT programs.steam.gamescopeSession: Rime's carries the
#     game-mode hold, GPU/output selection, HDR + composite guard, keyring, Discord and the desktop toggle, and the
#     upstream one would add a second "Steam" entry to the greeter.
#
# On bootc steam/gamescope/gamemode were on-demand (`rime install`). On NixOS they cannot be per-user (PLAN.md 4.5
# blockers), so they are system packages behind rime.gaming.enable; the default and its reasoning are on the option.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.gaming;

  root = ../..;

  # Sessions the display manager offers (services.displayManager.sessionPackages), at a stable path. rime-session-select
  # validates a session id against this directory, as it did against /usr/share/wayland-sessions on bootc. A stable
  # /etc path and not the store path: the session entry names rime-session-select, so a store path here is a cycle.
  sessionDir = "/etc/rime/wayland-sessions";

  # null = rime-gaming's own switcher.
  sessionSelect =
    if cfg.sessionSelect == null then "${switchTools}/libexec/rime-session-select" else cfg.sessionSelect;

  # gamescope as the session finds it. With capSysNice the only binary on PATH is the setcap wrapper.
  gamescopeBin =
    if config.programs.gamescope.capSysNice then
      "${config.security.wrapperDir}/gamescope"
    else
      lib.getExe' config.programs.gamescope.package "gamescope";

  # sudo for steamos-session-select. Steam on NixOS runs inside a bubblewrap FHS environment, and bubblewrap sets
  # no_new_privs, so the setuid sudo cannot raise privileges there (PLAN.md 4.8 blocker). Under no_new_privs this
  # hops out through the user's systemd manager (D-Bus works under no_new_privs; the transient unit does not carry
  # the flag) and runs the real sudo from there. Otherwise it is the real sudo.
  nnpSudo = pkgs.writeShellScriptBin "sudo" ''
    nnp=0
    while read -r key value _; do
      [ "$key" = "NoNewPrivs:" ] && nnp="$value"
    done < /proc/self/status
    if [ "$nnp" = 1 ]; then
      exec ${lib.getExe' pkgs.systemd "systemd-run"} --user --quiet --wait --pipe --collect \
        -- ${config.security.wrapperDir}/sudo "$@"
    fi
    exec ${config.security.wrapperDir}/sudo "$@"
  '';

  # The bootc scripts, unchanged in logic. Build-time substitutions only:
  #   rime-gamescope-steam  python3 + libX11 for the WM_STATE restore by store path (ctypes.util.find_library finds
  #                         nothing on NixOS), so neither leaks onto the PATH Steam inherits through `exec "$@"`.
  #   rime-session-select   nothing here: its --finish-switch re-exec is pointed at its WRAPPER in switchTools, because
  #                         that re-exec runs as a transient system unit (systemd-run), whose PATH has no coreutils.
  rimeGaming = pkgs.stdenvNoCC.mkDerivation {
    pname = "rime-gaming";
    version = "0.1.0";

    src = lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (root + "/files/system/libexec/rime-gaming-session")
        (root + "/files/system/libexec/rime-gamescope-steam")
        (root + "/files/system/libexec/rime-gaming-discord")
        (root + "/files/system/libexec/rime-game-env")
        (root + "/files/system/libexec/rime-session-select")
        (root + "/files/system/bin/steamos-session-select")
        (root + "/files/system/bin/steamos-update")
        (root + "/files/system/profile.d/rime-game-env.sh")
      ];
    };

    nativeBuildInputs = [ pkgs.makeWrapper ];
    # patchShebangs (fixupPhase) resolves `#!/usr/bin/env bash|python3` against these.
    buildInputs = [ pkgs.bash pkgs.python3 ];

    dontBuild = true;

    installPhase = ''
      runHook preInstall
      for f in rime-gaming-session rime-gamescope-steam rime-gaming-discord rime-game-env rime-session-select; do
        install -Dm755 files/system/libexec/$f $out/libexec/$f
      done
      install -Dm755 files/system/bin/steamos-session-select $out/bin/steamos-session-select
      install -Dm755 files/system/bin/steamos-update $out/bin/steamos-update
      install -Dm644 files/system/profile.d/rime-game-env.sh $out/share/rime/profile.d/rime-game-env.sh

      substituteInPlace $out/libexec/rime-gamescope-steam \
        --replace-fail 'python3 - "$1" "$STEAM_APPID"' '${lib.getExe pkgs.python3} - "$1" "$STEAM_APPID"' \
        --replace-fail 'ctypes.util.find_library("X11") or "libX11.so.6"' '"${lib.getLib pkgs.libx11}/lib/libX11.so.6"'
      runHook postInstall
    '';

    meta = {
      description = "Rime Gaming Mode session scripts";
      license = lib.licenses.mit;
      platforms = lib.platforms.linux;
    };
  };

  # Wrappers, made after fixup so patchShebangs sees the real scripts. Env values are --set-default: every one is a
  # knob the bootc scripts already read, and a value already in the environment still wins.
  #
  # Two derivations, because Steam's FHS environment carries steamos-session-select (programs.steam.extraPackages)
  # while the Gaming Mode session has Steam on its PATH: one derivation for both would be a dependency cycle.

  # The desktop <-> Gaming Mode switch: rime-session-select and Steam's steamos-* commands. Nothing here needs Steam.
  switchTools = pkgs.runCommand "rime-gaming-switch" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
    mkdir -p $out/libexec $out/bin
    # The --finish-switch re-exec (bootc: /usr/libexec/rime-session-select) runs through `systemd-run` as a transient
    # system unit, which inherits nothing from this process: it must re-exec the WRAPPER, or it dies on its first
    # `id` ("command not found") and the session is never ended. A private copy, so the substitution can name $out.
    install -Dm755 ${rimeGaming}/libexec/rime-session-select $out/libexec/.rime-session-select-unwrapped
    substituteInPlace $out/libexec/.rime-session-select-unwrapped \
      --replace-fail '/usr/libexec/rime-session-select --finish-switch' "$out/libexec/rime-session-select --finish-switch"
    # Runs as root under sudo (env_reset), so everything it needs is set here, not inherited.
    makeWrapper $out/libexec/.rime-session-select-unwrapped $out/libexec/rime-session-select \
      --set PATH ${lib.makeBinPath [ pkgs.coreutils pkgs.gawk pkgs.gnugrep pkgs.gnused pkgs.systemd pkgs.getent ]} \
      --set RIME_SESSION_DIR ${sessionDir} ${greetdConfigArgs}

    makeWrapper ${rimeGaming}/bin/steamos-session-select $out/bin/steamos-session-select \
      --prefix PATH : ${lib.makeBinPath [ nnpSudo ]} \
      --set-default RIME_SESSION_HELPER ${helperFor "$out"}
    ln -s ${rimeGaming}/bin/steamos-update $out/bin/steamos-update
  '';

  # rime-session-select copies greetd's config into the one-shot switch.toml (bootc: /etc/greetd/config.toml). NixOS
  # has no such file: greetd runs `--config <store toml>`, generated exactly as the greetd module generates it, so this
  # is the same store path. Without greetd the script's own default stays (and the copy is empty, as on bootc).
  greetdConfigArgs = lib.optionalString config.services.greetd.enable
    "--set RIME_GREETD_CONFIG ${(pkgs.formats.toml { }).generate "greetd.toml" config.services.greetd.settings}";

  helperFor = out: if cfg.sessionSelect == null then "${out}/libexec/rime-session-select" else cfg.sessionSelect;

  # The Gaming Mode session and what it runs inside gamescope.
  rimeGamingWrapped =
    let
      toolPath = lib.makeBinPath [
        pkgs.coreutils
        pkgs.gawk
        pkgs.gnugrep
        pkgs.gnused
        pkgs.libcap # getcap: the session's --rt probe
        pkgs.systemd # loginctl, systemctl, systemd-inhibit, systemd-run
        pkgs.util-linux
        pkgs.getent
        pkgs.xprop # rime-gamescope-steam's HDR / composite / WM_STATE guard
        pkgs.rime.rimed # `rime game start|stop`, `rime gaming --gamescope-device-args`
        config.programs.steam.package
        config.programs.gamescope.package
      ];
      # Setuid/setcap wrappers and the system profile first, so sudo is the setuid one, gamescope is the module's
      # (setcap wrapper when capSysNice), and only then Rime's fallbacks.
      sessionPath = "${config.security.wrapperDir}:/run/current-system/sw/bin:${toolPath}";
    in
    pkgs.runCommand "rime-gaming-wrapped" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
      mkdir -p $out/libexec $out/bin $out/share
      ln -s ${rimeGaming}/share/rime $out/share/rime
      for f in ${switchTools}/bin/* ${switchTools}/libexec/*; do
        ln -s "$f" "$out/''${f#${switchTools}/}"
      done

      # rime-game-env and rime-gaming-discord are not wrapped: the first is builtins only and runs in every login
      # shell; the second inherits the session's PATH (equibop, flatpak) on purpose.
      ln -s ${rimeGaming}/libexec/rime-game-env $out/libexec/rime-game-env
      ln -s ${rimeGaming}/libexec/rime-gaming-discord $out/libexec/rime-gaming-discord

      makeWrapper ${rimeGaming}/libexec/rime-gamescope-steam $out/libexec/rime-gamescope-steam \
        --prefix PATH : ${lib.makeBinPath [ pkgs.coreutils pkgs.xprop ]} \
        --set-default RIME_GAME_ENV_HELPER $out/libexec/rime-game-env \
        --set-default RIME_GAMING_DISCORD $out/libexec/rime-gaming-discord

      # What gamescope runs. programs.gamescope.capSysNice's wrapper raises CAP_SYS_NICE into the AMBIENT set
      # (nixos/modules/security/wrappers/wrapper.c make_caps_ambient), and ambient capabilities survive execve into
      # gamescope's children. Steam's bubblewrap refuses to start with anything in its permitted set ("Unexpected
      # capabilities but not setuid"; rime-gaming-session's capability block). Clearing the inheritable and ambient
      # sets before the exec leaves Steam with none, and costs nothing when there were none.
      makeWrapper ${lib.getExe' pkgs.util-linux "setpriv"} $out/libexec/rime-gamescope-steam-nocaps \
        --add-flags "--inh-caps=-all --ambient-caps=-all -- $out/libexec/rime-gamescope-steam"

      makeWrapper ${rimeGaming}/libexec/rime-gaming-session $out/libexec/rime-gaming-session \
        --prefix PATH : ${sessionPath} \
        --set-default RIME_GAMESCOPE_STEAM $out/libexec/rime-gamescope-steam-nocaps \
        --set-default RIME_GAMING_DISCORD $out/libexec/rime-gaming-discord \
        --set-default RIME_SECRETS_START ${cfg.secretsStart} \
        --set-default RIME_SESSION_HELPER ${sessionSelect}
    '';

  # The bootc session entry with Exec/TryExec pointed at store paths. On bootc TryExec hid the entry until
  # `rime install gamescope`; here gamescope is present whenever this session is, so TryExec names the binary the
  # session will run and the entry is offered exactly when it can start.
  gamingSession =
    (pkgs.runCommand "rime-gaming-session-entry" { } ''
      mkdir -p $out/share/wayland-sessions
      sed -e 's|^Exec=.*|Exec=${rimeGamingWrapped}/libexec/rime-gaming-session|' \
          -e 's|^TryExec=.*|TryExec=${gamescopeBin}|' \
          ${root + "/files/desktop/wayland-sessions/rime-gaming.desktop"} \
          > $out/share/wayland-sessions/rime-gaming.desktop
      grep -qx 'Exec=/nix/store/[A-Za-z0-9/._-]*' $out/share/wayland-sessions/rime-gaming.desktop
    '').overrideAttrs
      (_: {
        passthru.providedSessions = [ "rime-gaming" ];
      });

  gameEnvInit = ''
    RIME_GAME_ENV_HELPER=${rimeGaming}/libexec/rime-game-env
    . ${rimeGaming}/share/rime/profile.d/rime-game-env.sh
    unset RIME_GAME_ENV_HELPER
  '';
in
{
  options.rime.gaming = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Rime's gaming stack: Steam, Rime Gaming Mode (a gamescope session in the greeter), gamemode, NTSYNC and
        the game environment. Off by default (decision A4, measured 2026-11-01 with `nix path-info -S` on
        nix/tests/gaming.nix's `measure` toplevels, rime-vm config): it takes the closure from 2.87 GB NAR to
        5.82 GB NAR (+2.95 GB; 32-bit graphics, which programs.steam forces on, is 1.05 GB of that; about
        1.24 GB downloaded from cache.nixos.org plus 0.10 GB built locally). That reverses "install as small as
        possible" (2026-10-04), and bootc never shipped Steam or gamescope in the image either: they came from
        `rime install`. A machine that wants gaming sets this to true; `rime install steam` must do that.
      '';
    };

    gamescopeRealtime = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Give gamescope CAP_SYS_NICE (programs.gamescope.capSysNice), so Gaming Mode passes `--rt`. Off by default:
        bootc never had it, it is not measured on hardware, and the capability is cleared before Steam starts
        (rime-gamescope-steam-nocaps) only because the setcap wrapper makes it ambient.
      '';
    };

    sessionSelect = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        The privileged session switcher (bootc: /usr/libexec/rime-session-select) that Gaming Mode's exit and
        steamos-session-select run through `sudo -n`. null = rime-gaming's own wrapped copy, with a NOPASSWD sudo
        rule for wheel. A desktop module that ships its own copy (and its own sudo rule) sets this to it.
      '';
    };

    secretsStart = lib.mkOption {
      type = lib.types.str;
      default = "${pkgs.rime.rime-libexec}/libexec/rime-secrets-start";
      defaultText = lib.literalExpression ''"''${pkgs.rime.rime-libexec}/libexec/rime-secrets-start"'';
      description = ''
        rime-secrets-start, run by Gaming Mode at start so the login keyring PAM unlocked stays unlocked
        (bootc #104). The libexec package owns the script.
      '';
    };

    sessionPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = gamingSession;
      defaultText = lib.literalExpression "rime-gaming-session-entry";
      description = ''
        The Gaming Mode session entry (share/wayland-sessions/rime-gaming.desktop). It is added to
        services.displayManager.sessionPackages; a greeter that keeps its own list can take it from here.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = rimeGamingWrapped;
      defaultText = lib.literalExpression "rime-gaming-wrapped";
      description = "The wrapped Gaming Mode scripts (libexec/, bin/steamos-*).";
    };
  };

  config = lib.mkIf cfg.enable {
    programs.steam = {
      enable = true;
      # Steam's Gaming Mode Power menu (-steamos3) looks these up on its own PATH, inside the FHS environment.
      extraPackages = [ switchTools ];
    };

    programs.gamescope = {
      enable = true;
      capSysNice = cfg.gamescopeRealtime;
    };

    # bootc Containerfile.core :1241-1259. gamemoded's PATH is pkexec only (programs.gamemode), so `rime` is a
    # store path.
    programs.gamemode = {
      enable = true;
      settings = {
        general = {
          # rimed owns scaling_governor (M6 tier engine); matching gamemoded to rimed's game tier means neither
          # daemon flaps the governor against the other.
          desiredgov = "performance";
          defaultgov = "performance";
          igpu_desiredgov = "performance";
        };
        custom = {
          # `gamemoderun %command%` drives Rime game mode. A held Gaming mode refuses the stop (rimed hold.rs).
          start = "${lib.getExe' pkgs.rime.rimed "rime"} game start";
          end = "${lib.getExe' pkgs.rime.rimed "rime"} game stop";
        };
      };
    };

    # Steam forces this to true itself (programs.steam); stated here because the gaming stack is why it is on.
    hardware.graphics = {
      enable = true;
      enable32Bit = true;
    };

    # NTSYNC (bootc Containerfile.rime :243-262): Proton/Wine use /dev/ntsync when it exists. The driver registers a
    # dynamic misc minor, so nothing autoloads it; the device is 0666 from the driver. 51.4 -> 59.3 fps measured on
    # katana. The kernel must build CONFIG_NTSYNC (nixpkgs >= 6.14 does, as a module).
    boot.kernelModules = [ "ntsync" ];

    # bootc files/system/limits/30-rime-gaming-rtprio.conf, verbatim in effect (see that file for why 20 and wheel).
    security.pam.loginLimits = [
      { domain = "@wheel"; type = "-"; item = "rtprio"; value = "20"; }
      { domain = "@wheel"; type = "-"; item = "nice"; value = "-19"; }
    ];

    # bootc files/system/sudoers/rime-session-select: the switch into and out of Gaming Mode records the next
    # session as root. Only when this module's own switcher is in use.
    security.sudo.extraRules = lib.mkIf (cfg.sessionSelect == null) [
      {
        groups = [ "wheel" ];
        runAs = "root"; # bootc: %wheel ALL=(root) NOPASSWD
        commands = [
          {
            command = "${sessionSelect} *";
            options = [ "NOPASSWD" ];
          }
        ];
      }
    ];

    # sessionData (the session list a greeter reads) exists only with the shared display-manager integration on.
    # greetd turns it on too (mkDefault); on its own it only collects sessions and adds them to XDG_DATA_DIRS.
    services.displayManager.enable = lib.mkDefault true;
    services.displayManager.sessionPackages = [ gamingSession ];
    environment.etc."rime/wayland-sessions".source =
      lib.mkDefault "${config.services.displayManager.sessionData.desktops}/share/wayland-sessions";

    environment.systemPackages = [ rimeGamingWrapped pkgs.xprop ];

    # What the Shell reads to offer and run the desktop <-> Gaming Mode switch (PowerMenu.qml's gaming row probe,
    # PowerControl.sh gamingmode/desktopmode) and Settings > Gaming's Discord switch (GamingService.qml). Their
    # defaults are bootc's /usr/libexec and /usr/share/wayland-sessions, which do not exist here: without these the
    # Power menu has no Gaming Mode row and the Discord switch errors. mkDefault: a desktop module that owns the
    # switcher for every session (gaming off too) sets its own.
    environment.sessionVariables = {
      RIME_SESSION_HELPER = lib.mkDefault sessionSelect;
      RIME_SESSION_DIR = lib.mkDefault sessionDir;
      RIME_GAMING_DISCORD = lib.mkDefault "${rimeGamingWrapped}/libexec/rime-gaming-discord";
    };

    # bootc /etc/profile.d/rime-game-env.sh. NixOS sources no /etc/profile.d, so the same file is sourced from the
    # bash and zsh login init (greetd's `sh -c '. /etc/profile; …'` is bash on NixOS). Not environment.*ShellInit:
    # fish translates those with babelfish, and this file is POSIX sh. Fish shells inherit it from the session.
    programs.bash.loginShellInit = gameEnvInit;
    programs.zsh.loginShellInit = gameEnvInit;
  };
}
