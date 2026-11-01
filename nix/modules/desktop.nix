# Desktop: greetd + rime-greet, the Hyprland/niri/labwc sessions, portals, audio, fonts, input methods,
# keyring, Plymouth, Flatpak and the desktop services (PLAN.md 4.8, out/understand-desktop.md).
#
# Ported from the bootc image: Containerfile.core (desktop stack, fonts, IME, Nerd Font, printing,
# bluetooth, power), Containerfile.base (greeter, PAM, portals, sessions, Flathub) and Containerfile.rime
# (Plymouth). Upstream NixOS modules are used wherever one exists; Rime's own files are copied from
# files/** unchanged and only their FHS paths are rewritten at build time.
#
# ── Where the bootc paths go (the rule other workers should follow) ─────────────────────────────────
# Files a user COPIES into their home (the Hyprland/labwc configs rime-shell-firstrun seeds) must not
# name a /nix/store path: the copy outlives the generation and a garbage collection would break it. So
# FHS paths are rewritten to the stable profile of the running system:
#
#   /usr/libexec/<x>               -> /run/current-system/sw/libexec/<x>   (environment.pathsToLink "/libexec")
#   /usr/share/{rime,rime-shell,rime-greet,backgrounds}/...
#                                  -> /run/current-system/sw/share/...     (pathsToLink below)
#   /usr/bin/<x>                   -> /run/current-system/sw/bin/<x>
#   /usr/share/wayland-sessions    -> /etc/rime/wayland-sessions           (= the display manager's session set)
#   PATH=...:/usr/local/bin:/usr/bin:/usr/local/sbin
#                                  -> the NixOS PATH (wrappers, per-user profile, system profile)
#
# Files read in place by a running system service (the greetd config, PAM) use store paths.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.desktop;
  files = ../../files;
  sw = "/run/current-system/sw";
  sessionsDir = "/etc/rime/wayland-sessions";
  hyprland = config.programs.hyprland.package;

  # The NixOS PATH, written so it can replace bootc's "/usr/local/bin:/usr/bin:/usr/local/sbin".
  nixosPathFor = home: user:
    "/run/wrappers/bin:${home}/.nix-profile/bin:/etc/profiles/per-user/${user}/bin"
    + ":/nix/var/nix/profiles/default/bin:${sw}/bin";

  # sed program applied to every text file Rime ships for the desktop: the generic FHS -> NixOS rule.
  fhsToNixos = pkgs.writeText "rime-fhs-to-nixos.sed" ''
    s|/usr/share/wayland-sessions|${sessionsDir}|g
    s|/usr/libexec/|${sw}/libexec/|g
    s|/usr/share/rime-shell|${sw}/share/rime-shell|g
    s|/usr/share/rime-greet|${sw}/share/rime-greet|g
    s|/usr/share/rime/|${sw}/share/rime/|g
    s|/usr/share/backgrounds/|${sw}/share/backgrounds/|g
    s|/usr/bin/|${sw}/bin/|g
  '';

  # ── Desktop data: the compositor configs Rime seeds or reads, the wallpaper, the provider file ──────
  # bootc: /usr/share/rime/{hypr,labwc,safe-graphics}, /usr/share/backgrounds/rime/default.jpg,
  # /usr/share/rime/secrets-provider. Reached at ${sw}/share/... (stable, see the header).
  rimeDesktopData = pkgs.runCommand "rime-desktop-data" { } ''
    mkdir -p $out/share/rime $out/share/backgrounds/rime
    cp -r ${files}/desktop/hypr $out/share/rime/hypr
    cp -r ${files}/desktop/labwc $out/share/rime/labwc
    cp -r ${files}/desktop/safe-graphics $out/share/rime/safe-graphics
    cp ${files}/system/rime-secrets-provider $out/share/rime/secrets-provider
    cp ${files}/branding/wallpapers/rime-wallpaper-default.jpg $out/share/backgrounds/rime/default.jpg
    chmod -R u+w $out

    # The session PATH (session.lua:16, labwc autostart:14) names only FHS bin dirs, which on NixOS
    # would leave a session with no programs at all. Fails the build if the line ever changes.
    substituteInPlace $out/share/rime/hypr/rime/session.lua --replace-fail \
      'home .. "/.local/bin:/usr/local/bin:/usr/bin:/usr/local/sbin"' \
      'home .. "/.local/bin:${nixosPathFor ''" .. home .. "'' ''" .. (os.getenv("USER") or "") .. "''}"'
    substituteInPlace $out/share/rime/labwc/autostart --replace-fail \
      'PATH="@HOME@/.local/bin:/usr/local/bin:/usr/bin:/usr/local/sbin"' \
      'PATH="@HOME@/.local/bin:${nixosPathFor "@HOME@" "\${USER}"}"'

    find $out/share/rime -type f \( -name '*.lua' -o -name '*.xml' -o -name autostart \
      -o -name environment -o -name themerc-override \) -exec sed -i -f ${fhsToNixos} {} +

    # The two lines every session depends on, checked in their final form.
    grep -q 'hl.exec_cmd("${sw}/libexec/rime-shell-autostart")' $out/share/rime/hypr/rime/session.lua
    grep -q '^${sw}/libexec/rime-shell-autostart &$' $out/share/rime/labwc/autostart
    # Whatever /usr path is left is named in the build log (comments, /usr/share/lua, ...).
    grep -rn '/usr/' $out/share/rime || true
  '';

  # ── The session list ─────────────────────────────────────────────────────────────────────────────
  # bootc keeps only Rime's own entries (Containerfile.base:3147-3237): Hyprland's renamed "Rime Tiling",
  # niri "Rime Scrolling", labwc "Rime Floating", "Rime Safe Graphics". sway/labwc/hyprland-uwsm stock
  # entries are not offered. Gaming Mode comes from the gaming area through rime.desktop.extraSessions.
  rimeSessions = pkgs.runCommand "rime-sessions"
    {
      passthru.providedSessions = [ "hyprland" "niri" "rime-labwc" "rime-safe-graphics" ];
    }
    ''
      d=$out/share/wayland-sessions
      mkdir -p $d
      cat > $d/hyprland.desktop <<EOF
      [Desktop Entry]
      Name=Rime Tiling
      Comment=Windows tile themselves and reflow as you open and close them (Hyprland)
      Exec=${hyprland}/bin/start-hyprland
      Type=Application
      DesktopNames=Hyprland
      Keywords=tiling;wayland;compositor;
      EOF
      for f in niri rime-labwc rime-safe-graphics; do
        cp ${files}/desktop/wayland-sessions/$f.desktop $d/$f.desktop
      done
      chmod u+w $d/*
      sed -i 's|^Exec=niri |Exec=${pkgs.niri}/bin/niri |' $d/niri.desktop
      sed -i 's|^Exec=labwc$|Exec=${pkgs.labwc}/bin/labwc|' $d/rime-labwc.desktop
      sed -i -f ${fhsToNixos} $d/rime-safe-graphics.desktop
      grep -qx 'Exec=${pkgs.niri}/bin/niri --session' $d/niri.desktop
      grep -qx 'Exec=${pkgs.labwc}/bin/labwc' $d/rime-labwc.desktop
      grep -qx 'Exec=${sw}/libexec/rime-safe-graphics' $d/rime-safe-graphics.desktop
      # Session names are product names (tests/test-rime-greet-sessions.sh rule).
      if grep -iE '^Name=.*(hyprland|labwc|niri|sway)' $d/*.desktop; then exit 1; fi
    '';

  # ── rime-greet: Rime's Quickshell greeter (files/desktop/rime-greet) ────────────────────────────
  # Read in place by the greeter, so its own files are store paths; helpers it calls use the stable
  # profile like everything else.
  rimeGreet = pkgs.runCommand "rime-greet" { } ''
    mkdir -p $out/share
    cp -r ${files}/desktop/rime-greet $out/share/rime-greet
    chmod -R u+w $out
    g=$out/share/rime-greet
    substituteInPlace $g/sway-greet.conf \
      --replace-fail 'exec "qs -p /usr/share/rime-greet/shell.qml; swaymsg exit"' \
                     'exec "${lib.getExe' pkgs.quickshell "qs"} -p '"$g"'/shell.qml; ${lib.getExe' pkgs.sway "swaymsg"} exit"'
    sed -i "s|/usr/share/rime-greet|$g|g" $g/*.qml $g/sway-greet.conf $g/labwc-greet/*
    sed -i -f ${fhsToNixos} $g/*.qml $g/sway-greet.conf $g/labwc-greet/*
    grep -q '${sessionsDir}/\*.desktop' $g/GreetContext.qml
    grep -q 'bindsym --to-code Mod4+Mod1+s exec ${sw}/libexec/rime-screen-reader toggle' $g/sway-greet.conf
  '';

  # ── Plymouth: one splash, chartreuse (Containerfile.rime:265-312) ────────────────────────────────
  # The 24 per-user accent themes need the boot chain to hand the accent to the initrd (ESP credential):
  # TODO(handover): boot subsystem decides; until then every machine shows chartreuse.
  plymouthTheme = pkgs.runCommand "rime-plymouth-theme" { } ''
    t=$out/share/plymouth/themes/rime-os-chartreuse
    mkdir -p $(dirname $t)
    cp -r ${files}/branding/plymouth/rime-os-chartreuse $t
    chmod -R u+w $t
    substituteInPlace $t/rime-os-chartreuse.plymouth \
      --replace-fail /usr/share/plymouth/themes/rime-os-chartreuse $t
  '';

  # ── rime-keyring-login: the login password becomes the keyring's (Containerfile.base:3864-3905) ─
  # Run by pam_exec in greetd's auth stack. Python, with four FHS paths made explicit.
  keyringLogin = pkgs.runCommand "rime-keyring-login" { } ''
    mkdir -p $out/libexec
    cp ${files}/system/libexec/rime-keyring-login $out/libexec/rime-keyring-login
    chmod u+w,+x $out/libexec/rime-keyring-login
    substituteInPlace $out/libexec/rime-keyring-login \
      --replace-fail '#!/usr/bin/python3' '#!${pkgs.python3}/bin/python3' \
      --replace-fail 'CHKPWD = "/usr/sbin/unix_chkpwd"' 'CHKPWD = "/run/wrappers/bin/unix_chkpwd"' \
      --replace-fail 'PROVIDER_FILE = "/usr/share/rime/secrets-provider"' \
                     'PROVIDER_FILE = "${rimeDesktopData}/share/rime/secrets-provider"' \
      --replace-fail 'GNOME_KEYRING_DAEMON = "/usr/bin/gnome-keyring-daemon"' \
                     'GNOME_KEYRING_DAEMON = "${pkgs.gnome-keyring}/bin/gnome-keyring-daemon"' \
      --replace-fail 'ctypes.CDLL("libcrypto.so.3")' 'ctypes.CDLL("${lib.getLib pkgs.openssl}/lib/libcrypto.so.3")'
    ${pkgs.python3}/bin/python3 -c 'import ast, sys; ast.parse(open(sys.argv[1]).read())' \
      $out/libexec/rime-keyring-login
  '';

  # ── 50-rime-keymap: the system keymap reaches greetd's greeter host (files/system/systemd/rime-keymap-env)
  # NixOS writes /etc/X11/xorg.conf.d/00-keyboard.conf from services.xserver.xkb (graphical-desktop.nix),
  # the same file localectl writes on Fedora, so the generator runs unchanged; only its PATH is given.
  keymapGenerator = pkgs.writeShellScript "50-rime-keymap" ''
    export PATH=${lib.makeBinPath [ pkgs.coreutils pkgs.gnused ]}
    exec ${pkgs.bash}/bin/bash ${files}/system/systemd/rime-keymap-env
  '';

  # JetBrainsMono Nerd Font at the version the bootc image pins (Containerfile.core:1490, v3.5.1);
  # nixpkgs carries 3.5.0. rime-greet draws its text and its glyphs with this family.
  nerdFontJetBrainsMono = pkgs.nerd-fonts.jetbrains-mono.overrideAttrs (old: {
    version = "3.5.1+2.304";
    src = pkgs.fetchurl {
      url = "https://github.com/ryanoasis/nerd-fonts/releases/download/v3.5.1/JetBrainsMono.tar.xz";
      hash = "sha256-BNXo+QNpP53RPhb4Z+mUg05oHrPHLA0zencNzaCQEM8=";
    };
  });

  # Firefox defaults: files/system/firefox/policies.json (dark UI, about:home) + rime-default-prefs.js.
  firefoxPolicies = (lib.importJSON (files + "/system/firefox/policies.json")).policies;
in
{
  options.rime.desktop = {
    enable = lib.mkEnableOption "Rime's desktop (sessions, Shell, portals)";

    extraSessions = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      example = lib.literalExpression "[ pkgs.rime.rime-gaming-session ]";
      description = ''
        More login sessions for the greeter, besides Rime's own (Tiling, Scrolling, Floating, Safe
        Graphics). Each package carries share/wayland-sessions/<id>.desktop and passthru.providedSessions,
        like services.displayManager.sessionPackages. Gaming Mode is added here by the gaming area.
      '';
    };

    greeterUser = lib.mkOption {
      type = lib.types.str;
      default = "greetd";
      description = ''
        Account the greeter runs as. bootc's is `greetd` (Fedora's sysusers), and /var/lib/rime-greet on
        an existing machine belongs to it, so Rime keeps the name rather than NixOS's `greeter`.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          # Containerfile.base:3803-3862: exactly one Secret Service, the one the login unlocks.
          assertion = !(config.services.oo7.enable or false);
          message = "rime.desktop: oo7 must stay off; Rime's login unlocks gnome-keyring (files/system/rime-secrets-provider).";
        }
        {
          # Containerfile.core:1392: the Accessible.* fix (quickshell 916a0dd) is in 0.3.2 and later.
          assertion = lib.versionAtLeast pkgs.quickshell.version "0.3.2";
          message = "rime.desktop: quickshell ${pkgs.quickshell.version} predates the accessibility fix Rime needs (>= 0.3.2).";
        }
        {
          # Containerfile.base:3014-3093: Rime's Hyprland config is Lua, validated against 0.57.
          assertion = lib.versionAtLeast hyprland.version "0.57";
          message = "rime.desktop: Hyprland ${hyprland.version} cannot read Rime's Lua config (needs >= 0.57).";
        }
        {
          # The keyring-login hook needs a password in the stack. fprintd adds pam_fprintd to every
          # service by default and would let auth succeed without one (Containerfile.base:3880).
          assertion = !config.services.fprintd.enable || !config.security.pam.services.login.fprintAuth;
          message = "rime.desktop: fprintd may not authenticate the login stack (rime-keyring-login needs a typed password).";
        }
      ];

      # ── Login: greetd running rime-greet in sway on VT 1 (files/desktop/rime-greet/greetd-config.toml) ─
      services.greetd = {
        enable = true;
        settings.default_session = {
          user = cfg.greeterUser;
          # rime-greet-session gives the greeter a session bus + accessibility bus, then execs sway.
          # --unsupported-gpu: sway exits on any machine where nvidia-drm binds without it (rime-logs 46).
          command = "${sw}/libexec/rime-greet-session ${lib.getExe pkgs.sway} --unsupported-gpu"
            + " -c ${rimeGreet}/share/rime-greet/sway-greet.conf";
        };
      };
      users.users.${cfg.greeterUser} = {
        isSystemUser = true;
        group = cfg.greeterUser;
        description = "Rime login screen";
        home = "/var/lib/rime-greet";
      };
      users.groups.${cfg.greeterUser} = { };
      systemd.tmpfiles.rules = [
        # The greeter's memory (last user/session, published wallpapers and accents). Z: a machine that
        # came from bootc carries it owned by Fedora's greetd uid, which NixOS numbers differently.
        "d /var/lib/rime-greet 0755 ${cfg.greeterUser} ${cfg.greeterUser} -"
        "Z /var/lib/rime-greet - ${cfg.greeterUser} ${cfg.greeterUser} -"
      ];
      # VARIANT_ID picks the greeter's edition accent (GreetContext.qml:313-320).
      system.nixos.variant_id = lib.mkDefault "rime";

      environment.etc."systemd/system-environment-generators/50-rime-keymap".source = keymapGenerator;

      # ── Sessions ────────────────────────────────────────────────────────────────────────────────────
      services.displayManager.sessionPackages = lib.mkForce ([ rimeSessions ] ++ cfg.extraSessions);
      services.displayManager.defaultSession = "hyprland"; # niri.nix sets "niri" at mkDefault
      environment.etc."rime/wayland-sessions".source =
        "${config.services.displayManager.sessionData.desktops}/share/wayland-sessions";

      programs.hyprland.enable = true; # 0.57 + xdg-desktop-portal-hyprland
      # Rime Tiling execs start-hyprland directly, as bootc does, and never goes through UWSM. The option
      # stays at the package's own default (true) only because false is a different derivation: Hyprland
      # would be rebuilt from source instead of coming from the binary cache.
      programs.hyprland.withUWSM = true;
      programs.niri.enable = true;
      programs.labwc.enable = true;

      environment.pathsToLink = [
        "/libexec"
        "/share/rime"
        "/share/rime-greet"
        "/share/rime-shell"
        "/share/backgrounds"
      ];

      # ── Keyring: the password typed at the login screen opens it (Containerfile.base:3767-3905) ───
      # services.gnome.gnome-keyring enables pam_gnome_keyring in `login`; greetd's stack is
      # `auth substack login` / `session include login`, so that is the stack greetd uses.
      # (security.pam.services.greetd.enableGnomeKeyring would do nothing: greetd has useDefaultRules = false.)
      services.gnome.gnome-keyring.enable = true;
      # rime-keyring-login runs right after the password was checked (inside the login substack) and
      # renames an existing default keyring to `login` when that password opens it. The daemon starts
      # only in the session phase (auto_start), so the rename always happens before it reads the files.
      security.pam.services.greetd.rules.auth.rime-keyring-login = {
        order = config.security.pam.services.greetd.rules.auth.login.order + 50;
        control = "optional";
        modulePath = "${pkgs.pam}/lib/security/pam_exec.so";
        # Never `stdout`: pam_exec would hand it to the greeter as a message.
        args = [ "quiet" "expose_authtok" "${keyringLogin}/libexec/rime-keyring-login" ];
      };
      # The session itself runs `gnome-keyring-daemon --start --components=secrets` (#103: without it
      # the PAM-started daemon exits ~2 min after login). That is rime-secrets-start, which
      # rime-shell-autostart runs from every Rime session; both are rime-libexec scripts.

      # ── Portals (files/system/xdg-desktop-portal/*-portals.conf) ──────────────────────────────────
      xdg.portal = {
        enable = true;
        extraPortals = [
          pkgs.xdg-desktop-portal-gtk
          pkgs.xdg-desktop-portal-gnome
          pkgs.xdg-desktop-portal-wlr
        ];
        config = {
          hyprland = {
            default = [ "hyprland" "gtk" ];
            "org.freedesktop.impl.portal.ScreenCast" = [ "hyprland" ];
            "org.freedesktop.impl.portal.Screenshot" = [ "hyprland" ];
            "org.freedesktop.impl.portal.Secret" = [ "gnome-keyring" ];
          };
          labwc = {
            default = [ "gtk" ];
            "org.freedesktop.impl.portal.ScreenCast" = [ "wlr" ];
            "org.freedesktop.impl.portal.Screenshot" = [ "wlr" ];
            "org.freedesktop.impl.portal.Secret" = [ "gnome-keyring" ];
            "org.freedesktop.impl.portal.Inhibit" = [ "none" ];
          };
          niri = {
            default = lib.mkForce [ "gtk" "gnome" ]; # niri.nix says gnome first; Rime prefers gtk
            "org.freedesktop.impl.portal.ScreenCast" = [ "gnome" ];
            "org.freedesktop.impl.portal.Screenshot" = [ "gnome" ];
            "org.freedesktop.impl.portal.Secret" = [ "gnome-keyring" ];
          };
        };
      };

      # ── Audio: PipeWire + WirePlumber + pulse (Containerfile.core:1366) ───────────────────────────
      security.rtkit.enable = true;
      services.pipewire = {
        enable = true;
        alsa.enable = true;
        alsa.support32Bit = true;
        pulse.enable = true;
        wireplumber.enable = true;
      };

      # ── Input methods: fcitx5 (Containerfile.core:1786-1814) ──────────────────────────────────────
      # Wayland frontend: only XMODIFIERS is exported (GTK/QT_IM_MODULE on Wayland break native input),
      # which is exactly the bootc choice. The Rime sessions start fcitx5 themselves.
      i18n.inputMethod = {
        enable = true;
        type = "fcitx5";
        fcitx5 = {
          waylandFrontend = true;
          addons = with pkgs; [
            fcitx5-gtk
            qt6Packages.fcitx5-chinese-addons
            fcitx5-hangul
            fcitx5-anthy
            fcitx5-m17n
            qt6Packages.fcitx5-configtool
          ];
        };
      };

      # ── Fonts (Containerfile.core:1490-1501, 2011-2031) ───────────────────────────────────────────
      fonts = {
        enableDefaultPackages = true;
        packages = with pkgs; [
          nerdFontJetBrainsMono
          jetbrains-mono
          noto-fonts
          noto-fonts-cjk-sans
          noto-fonts-cjk-serif
          noto-fonts-color-emoji
          liberation_ttf
          dejavu_fonts
        ];
      };

      # ── Network, Bluetooth, printing, scanning (Containerfile.core:1607-1780, 1940-1990) ──────────
      networking.networkmanager = {
        enable = true;
        # vpnc: removed from nixpkgs as insecure (on bootc F45 obsoleted it too, Containerfile.core:1979).
        plugins = with pkgs; [ networkmanager-openvpn networkmanager-openconnect ];
      };
      hardware.bluetooth.enable = true;
      services.blueman.enable = true;
      services.printing = {
        enable = true; # socket-activated by default on NixOS (cups.socket), as on bootc
        drivers = with pkgs; [ gutenprint hplip ];
      };
      services.avahi = {
        enable = true;
        nssmdns4 = true; # .local printer discovery (authselect with-mdns4 on bootc)
        # rime-remoted publishes _rime-remote._tcp with avahi-publish-service (user services)
        settings.publish = {
          disable-publishing = false;
          disable-user-service-publishing = false;
          publish-addresses = true;
        };
      };
      hardware.sane = {
        enable = true;
        extraBackends = [ pkgs.sane-airscan ];
      };

      # ── Power, sensors, devices ─────────────────────────────────────────────────────────────────────
      # power-profiles-daemon, not tlp (Containerfile.core:1940): rimed layers its own policy on PPD.
      services.power-profiles-daemon.enable = true;
      services.thermald.enable = true;
      # thermald is Intel-only and restarts on failure: skip it cleanly elsewhere (10-rime-intel-only.conf).
      systemd.services.thermald.serviceConfig.ExecCondition =
        "${pkgs.bash}/bin/sh -c '${pkgs.gnugrep}/bin/grep -q GenuineIntel /proc/cpuinfo'";
      services.upower.enable = true;
      services.udisks2.enable = true;
      services.gvfs.enable = true;
      services.fwupd.enable = true;
      services.geoclue2.enable = true;
      services.switcherooControl.enable = true;
      services.hardware.bolt.enable = true;
      hardware.sensor.iio.enable = true;
      programs.dconf.enable = true;

      # ── Accessibility: Orca + the AT-SPI bus the greeter publishes on (Containerfile.core:1815) ──
      services.gnome.at-spi2-core.enable = true;
      services.orca.enable = true;
      services.speechd.enable = true;

      # ── Boot splash (Containerfile.rime:265-312; bootc kargs.d `quiet splash`) ────────────────────
      boot.plymouth = {
        enable = true;
        themePackages = [ plymouthTheme ];
        theme = "rime-os-chartreuse";
      };
      boot.kernelParams = [ "quiet" "splash" ];
      boot.consoleLogLevel = lib.mkDefault 3;

      # ── Flatpak + Flathub (files/system/units/rime-flathub-setup.service) ─────────────────────────
      services.flatpak.enable = true;
      environment.etc."flatpak/flathub.flatpakrepo".source = files + "/system/flatpak/flathub.flatpakrepo";
      systemd.services.rime-flathub-setup = {
        description = "Rime OS: register the Flathub Flatpak remote (first boot)";
        documentation = [ "https://flathub.org" ];
        wantedBy = [ "multi-user.target" ];
        wants = [ "network-online.target" ];
        after = [ "network-online.target" ];
        unitConfig = {
          ConditionPathExists = [ "!/var/lib/flatpak/.rime-flathub-added" "/etc/flatpak/flathub.flatpakrepo" ];
          StartLimitIntervalSec = "20min";
          StartLimitBurst = 10;
        };
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${lib.getExe pkgs.flatpak} remote-add --if-not-exists --system flathub /etc/flatpak/flathub.flatpakrepo";
          ExecStartPost = "${pkgs.coreutils}/bin/touch /var/lib/flatpak/.rime-flathub-added";
          Restart = "on-failure";
          RestartSec = "60s";
          TimeoutStartSec = "2min";
        };
      };

      # ── Browser, terminal, default apps, theme (Containerfile.base:566-631, 2566) ─────────────────
      programs.firefox = {
        enable = true;
        policies = firefoxPolicies;
        # the bootc prefs file sets these with pref() (user may change them), so not locked; the
        # homepage and pinned tiles already come from policies.json with Status "default".
        preferencesStatus = "default";
        preferences."startup.homepage_override_url" = "";
      };
      xdg.mime = {
        enable = true;
        defaultApplications = {
          "x-scheme-handler/http" = "firefox.desktop";
          "x-scheme-handler/https" = "firefox.desktop";
          "x-scheme-handler/about" = "firefox.desktop";
          "x-scheme-handler/unknown" = "firefox.desktop";
          "text/html" = "firefox.desktop";
          "application/xhtml+xml" = "firefox.desktop";
        };
      };
      xdg.terminal-exec = {
        enable = true;
        settings.default = [ "Alacritty.desktop" ];
      };
      environment.sessionVariables = {
        TERMINAL = "alacritty";
        QT_QPA_PLATFORMTHEME = "qt6ct";
        ELECTRON_OZONE_PLATFORM_HINT = "auto";
      };
      environment.etc = {
        "gtk-3.0/settings.ini".source = files + "/system/gtk/gtk-3.0-settings.ini";
        "gtk-4.0/settings.ini".source = files + "/system/gtk/gtk-4.0-settings.ini";
        "xdg/qt6ct/qt6ct.conf".source = files + "/system/qt6ct/qt6ct.conf";
        "mpv/mpv.conf".source = files + "/desktop/mpv/mpv.conf";
      };
      programs.dconf.profiles.user.databases = [
        { settings."org/gnome/desktop/interface" = { color-scheme = "prefer-dark"; gtk-theme = "Adwaita"; }; }
      ];

      # ── What the sessions call (Containerfile.core:1366-1383, 1649-1662) ──────────────────────────
      environment.systemPackages = with pkgs; [
        rimeDesktopData
        rimeGreet
        pkgs.rime.rime-libexec # the session helpers, at ${sw}/libexec (see the header)
        mate-polkit # polkit agent: ${sw}/libexec/polkit-mate-authentication-agent-1 (session.lua:43)
        at-spi2-core # at-spi-bus-launcher / at-spi2-registryd for rime-greet-session

        quickshell
        sway # the greeter host only; not offered as a session
        xwayland-satellite
        foot
        alacritty
        hyprlock
        hypridle
        hyprsunset
        hyprshot
        hyprland-guiutils # bootc builds hyprland-qtutils for this: Hyprland's dialogs + ANR manager
        grimblast
        awww
        matugen
        playerctl
        brightnessctl
        wl-clipboard
        slurp
        wtype
        cliphist
        libnotify
        lm_sensors
        cava
        wf-recorder
        imagemagick
        qt6Packages.qt6ct
        mpv
        mpvScripts.mpris
        xdg-user-dirs
        xdg-utils
        gammastep
        jq
        kanshi
        wlr-randr
        wlopm
        bazaar
        thunar
        thunar-volman
        thunar-archive-plugin
        evince
        qimgv
        _7zz
        unzip
        unar
        xarchiver
        btop
        qalculate-gtk
        fastfetch
        pavucontrol
        gparted
        networkmanagerapplet # nm-connection-editor
        bluez-tools
        simple-scan
        libsecret
        adwaita-icon-theme
      ];
    }

    # shell ── Rime Shell (the shell area owns this section): its package, QML import path, autostart.
    # The configs above already call ${sw}/libexec/rime-shell-autostart and read ${sw}/share/rime-shell
    # ("/share/rime-shell" is linked above); the Shell package itself is added here by the shell area.
    { }
  ]);
}
