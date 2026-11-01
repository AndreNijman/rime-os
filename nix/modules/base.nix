# Base system: Nix settings, networking, locale and user defaults every Rime machine has.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.base;

  # libexec: Rime's helper scripts and system files (nix/pkgs/rime-libexec.nix). Callers use the stable
  # /run/current-system/sw/libexec/<name> path; sudo rules name the store path (sudo matches both, see
  # nix/tests/libexec.nix).
  libexec = pkgs.rime.rime-libexec;
  helper = name: "${libexec}/libexec/${name}";
  # A sudo rule for one helper with exact arguments, under both names the helper has: the store path, and
  # the /run/current-system/sw/libexec path every caller uses. sudo does NOT match one through the other
  # (it compares the path it was given; measured in nix/tests/libexec.nix), so a store-path-only rule
  # would refuse rime-shell. The /run/current-system path is a root-owned link, so naming it grants nothing more.
  sudoCommands = name: args:
    lib.concatMap (path: map (a: { command = "${path} ${a}"; options = [ "NOPASSWD" ]; }) args) [
      (helper name)
      "/run/current-system/sw/libexec/${name}"
    ];
  desktop = config.rime.desktop.enable;
  greetd = config.services.greetd.enable;
  # The guest units' sandbox (files/system/units/rime-guest-{wipe,session}@.service, kept as they were).
  guestHardening = {
    NoNewPrivileges = true;
    ProtectSystem = "strict";
    # "-": a path that does not exist on this machine is skipped instead of failing the unit. /var/home is
    # bootc's home; NixOS homes are under /home.
    ReadWritePaths = [ "-/var/lib/rime-secretd" "-/var/lib/rime-greet" "-/home" ];
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectControlGroups = true;
    RestrictRealtime = true;
    RestrictSUIDSGID = true;
    LockPersonality = true;
    PrivateNetwork = true;
  };
in
{
  options.rime.base = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Rime's base system settings.";
    };
  };

  config = lib.mkIf cfg.enable {
    # First release Rime installs on. Never bump it for an existing machine.
    # Below mkDefault so the NixOS test framework and hosts can set their own without a conflict.
    system.stateVersion = lib.mkOverride 1200 "26.11";

    networking.hostName = lib.mkOverride 1200 "rime";
    networking.networkmanager.enable = true;

    # null = the user sets it at runtime (timedatectl / Settings), as on bootc.
    time.timeZone = lib.mkDefault null;
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    # Accounts are created and changed at runtime (installer, Settings, `rime user`), not declared.
    users.mutableUsers = true;

    # Rime ships NVIDIA, Steam and other unfree packages (same setting as the flake's own pkgs).
    nixpkgs.config.allowUnfree = true;

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      auto-optimise-store = true;
    };

    # ── libexec ──────────────────────────────────────────────────────────────────────────────────────────
    # bootc /usr/libexec/rime-*, /usr/share/rime, /usr/bin/{apex,steamos-*}. Linked into the system profile
    # so every Fedora path the scripts and rime-shell name has one NixOS twin under /run/current-system/sw.
    environment.systemPackages = [ libexec ]; # libexec
    environment.pathsToLink = [ "/libexec" "/share/rime" ]; # libexec

    # bootc files/system/sudoers/* (each file's header says why the grant is safe), with the same arguments.
    # `""` = no arguments at all. rime-session-select's rule is the gaming area's (gaming.nix).
    security.sudo.extraRules = [
      # libexec: files/system/sudoers/rime-boot-windows
      {
        groups = [ "wheel" ];
        runAs = "root";
        commands = sudoCommands "rime-boot-windows" [ "\"\"" "--check" ];
      }
      # libexec: files/system/sudoers/rime-greet-wallpaper and rime-switch-greeter (every local user)
      {
        users = [ "ALL" ];
        runAs = "root";
        commands = sudoCommands "rime-greet-wallpaper" [ "\"\"" ] ++ sudoCommands "rime-switch-greeter" [ "\"\"" ];
      }
    ];

    # libexec: /etc/profile.d/rime-{tool-broker,starship,zsh-highlight}.sh. NixOS reads no /etc/profile.d.
    # Each file checks its own shell and session, so the same lines go to bash and zsh. rime-game-env.sh is
    # the gaming area's (gaming.nix).
    # The broker line also goes to login shells (greetd's `sh -c '. /etc/profile; …'`, agent sessions started as
    # login shells): bootc reached both through /etc/profile.d. It is idempotent (it checks PATH first).
    programs.bash.loginShellInit = ". ${libexec}/etc/profile.d/rime-tool-broker.sh";
    programs.zsh.loginShellInit = ". ${libexec}/etc/profile.d/rime-tool-broker.sh";
    programs.bash.interactiveShellInit = ''
      . ${libexec}/etc/profile.d/rime-tool-broker.sh
      . ${libexec}/etc/profile.d/rime-starship.sh
    '';
    programs.zsh.interactiveShellInit = ''
      . ${libexec}/etc/profile.d/rime-tool-broker.sh
      . ${libexec}/etc/profile.d/rime-zsh-highlight.sh
    '';

    # libexec: files/system/coredump/50-rime-coredump-limits.conf
    systemd.coredump.settings.Coredump = { MaxUse = "256M"; KeepFree = "2G"; };

    # libexec: files/system/system-sleep/rime-wifi-resume (bootc ran it as a system-sleep hook with "post").
    powerManagement.resumeCommands = "${helper "rime-wifi-resume"} post";

    # libexec: files/system/shared-machine/guest-accounts. Ships empty: the guest engine refuses every
    # account it does not name. C = copied once, then the machine's own file.
    systemd.tmpfiles.rules = [
      "d /etc/rime 0755 root root -"
      "C /etc/rime/guest-accounts 0644 root root - ${libexec}/share/rime/shared-machine/guest-accounts"
    ] ++ lib.optionals desktop [
      # rime-greet-wallpaper writes here as root; the greeter only reads (bootc /usr/lib/tmpfiles.d/rime-greet.conf).
      "d /var/lib/rime-greet/wallpapers 0755 root root -"
    ];

    # libexec: files/system/units/rime-guest-{wipe,session}@.service
    systemd.services."rime-guest-wipe@" = {
      description = "Clear the disposable guest account %i";
      documentation = [ "man:rime(1)" ];
      after = [ "user-%i.slice" ];
      serviceConfig = guestHardening // {
        Type = "oneshot";
        ExecStart = "${helper "rime-guest-wipe"} %i";
        SuccessExitStatus = "0 2";
      };
    };
    systemd.services."rime-guest-session@" = {
      description = "Wipe the disposable guest uid %i when its session ends";
      documentation = [ "man:rime(1)" ];
      bindsTo = [ "user-runtime-dir@%i.service" ];
      before = [ "user-runtime-dir@%i.service" ];
      # Installed, NOT enabled (as on bootc): only a configured guest's uid gets it. TODO(handover): rime-user
      # enables it with `systemctl enable rime-guest-session@<uid>`, which NixOS refuses (read-only
      # /etc/systemd/system, no [Install] section). Needs a boot-time `systemctl add-wants --runtime` from
      # /etc/rime/guest-accounts, or a declarative guest list.
      serviceConfig = guestHardening // {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/true";
        RemainAfterExit = true;
        ExecStop = "${helper "rime-guest-wipe"} %i";
      };
    };

    # libexec: files/system/units/rime-switch-greeter@.service (fast user switching; needs greetd).
    systemd.services."rime-switch-greeter@" = lib.mkIf greetd {
      description = "Rime fast-user-switch login screen on VT %i";
      after = [ "systemd-user-sessions.service" "getty@tty%i.service" "kmsconvt@tty%i.service" "autovt@tty%i.service" ];
      conflicts = [ "getty@tty%i.service" "kmsconvt@tty%i.service" "autovt@tty%i.service" ];
      unitConfig.ConditionPathExists = "/run/rime-switch-greeter/%i.toml";
      serviceConfig = {
        Type = "simple";
        ExecStart = "${config.services.greetd.package}/bin/greetd --config /run/rime-switch-greeter/%i.toml --socket-path /run/rime-switch-greeter/%i.sock";
        ExecStopPost = "-${pkgs.coreutils}/bin/rm -f /run/rime-switch-greeter/%i.toml /run/rime-switch-greeter/%i.run /run/rime-switch-greeter/%i.sock";
        IgnoreSIGPIPE = false;
        SendSIGHUP = true;
        TimeoutStopSec = "30s";
        KeyringMode = "shared";
        Restart = "no";
      };
    };

    # libexec: files/system/units/rime-greet-publish.service (wallpapers + accents for the login screen).
    systemd.services.rime-greet-publish = lib.mkIf desktop {
      description = "Rime: publish wallpapers and accents for the login screen";
      after = [ "local-fs.target" "systemd-tmpfiles-setup.service" ];
      before = [ "greetd.service" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = helper "rime-greet-publish-all";
        TimeoutStartSec = 20;
      };
    };

    # libexec: files/system/units/sing-box.service. Never started at boot: Rime Shell's VPN toggle starts and
    # stops it (the polkit rule is the security area's). Not when the upstream services.sing-box owns the unit.
    systemd.services.sing-box = lib.mkIf (desktop && !config.services.sing-box.enable) {
      description = "sing-box VPN (Rime OS; toggled by Rime Shell)";
      documentation = [ "https://sing-box.sagernet.org" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      unitConfig.ConditionPathExists = "/etc/sing-box/config.json";
      serviceConfig = {
        Type = "simple";
        ExecStart = "${pkgs.sing-box}/bin/sing-box run -c /etc/sing-box/config.json";
        Restart = "on-failure";
        RestartSec = 2;
        CapabilityBoundingSet = [ "CAP_NET_ADMIN" "CAP_NET_RAW" "CAP_NET_BIND_SERVICE" ];
        AmbientCapabilities = [ "CAP_NET_ADMIN" "CAP_NET_RAW" "CAP_NET_BIND_SERVICE" ];
      };
    };

    # libexec: files/system/units/rime-{gpu,storage}-notice.{service,timer} (user units, enabled for everyone).
    systemd.user.services.rime-gpu-notice = {
      description = "Rime OS NVIDIA GPU support notice";
      documentation = [ "man:rime(1)" ];
      unitConfig.ConditionPathExists = "/sys/bus/pci/devices";
      serviceConfig = { Type = "oneshot"; ExecStart = helper "rime-gpu-notice"; NoNewPrivileges = true; };
    };
    systemd.user.timers.rime-gpu-notice = {
      description = "Check once per session whether the NVIDIA GPU has a driver";
      wantedBy = [ "timers.target" ];
      timerConfig = { OnStartupSec = "2min"; Unit = "rime-gpu-notice.service"; };
    };
    systemd.user.services.rime-storage-notice = {
      description = "Rime OS disk-health warning";
      documentation = [ "man:rime(1)" ];
      unitConfig.ConditionPathExists = "/sys/block";
      serviceConfig = { Type = "oneshot"; ExecStart = helper "rime-storage-notice"; NoNewPrivileges = true; };
    };
    systemd.user.timers.rime-storage-notice = {
      description = "Check disk health daily";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnStartupSec = "10min";
        OnUnitActiveSec = "1d";
        Persistent = true;
        Unit = "rime-storage-notice.service";
      };
    };

    # libexec: files/system/units/rime-shell-firstrun.service (seeds Rime Shell's user config at every login).
    systemd.user.services.rime-shell-firstrun = lib.mkIf desktop {
      description = "Rime OS per-login user provisioner (seed Rime Shell user config)";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = helper "rime-shell-firstrun";
        TimeoutStartSec = 60;
        Restart = "no";
      };
    };

    # files/system/xdg/xdg-terminals.list is the desktop area's: xdg.terminal-exec (desktop.nix) writes the same
    # /etc/xdg/xdg-terminals.list (Alacritty.desktop).
  };
}
