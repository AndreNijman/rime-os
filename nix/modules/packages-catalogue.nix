# Packages catalogue: what `rime install <name>` does on NixOS (PLAN.md 4.5, decision D5).
#
# There is no nixpkgs evaluation on the device (D5), so `rime install` resolves names against a catalogue that is
# built into the system: nix/catalogue/catalogue.nix, rendered here to /etc/rime/catalogue.json and read by
# `rime` (rimed/rime/src/nixos/pkg.rs). The data file explains the five kinds; this module adds what the device needs
# to carry them out:
#
#   - the catalogue itself, plus `in_system` per feature (whether THIS system was built with it);
#   - the runtime switches for class 1 features: rime-feature@<feature>.target starts only while its switch
#     /var/lib/rime/switch/rime-feature@<feature>.target.on exists (the D2 host-layer convention: <unit>.on for a
#     unit that is off unless switched on). `rime install steam` writes the switch; units of a feature can be
#     WantedBy its target;
#   - the rime-apps profile (/nix/var/nix/profiles/rime-apps) on every login's PATH and XDG_DATA_DIRS, which class 2
#     apps are installed into from the cache;
#   - nix-ld with a library set wide enough for the dynamic ELF programs people already have in ~/.local (claude-code,
#     uv, bun, ollama, Electron apps such as Equibop): PLAN 4.5 blocker "nix-ld is mandatory";
#   - appimage-run with binfmt, so an AppImage runs when it is executed;
#   - system.build.rimeCatalogue: every class 2 app built, for the release job to push to the cache and list in the
#     manifest's `catalogue[]` (docs/nixos/updates-and-release.md 3.1). The toplevel does NOT depend on it: the JSON
#     names no class 2 store path, so the apps are fetched only by the machines that install them.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.catalogue;

  data = import ../catalogue/catalogue.nix;

  entries = data.entries // cfg.extraEntries;

  # Paths shared with rimed/rime/src/nixos/pkg.rs through the JSON, so the two cannot disagree.
  switchDir = "/var/lib/rime/switch";
  appsProfile = "/nix/var/nix/profiles/rime-apps";
  # Written by the update client from the verified release manifest (D6 3.1 `catalogue[]`): app name -> store path.
  outsFile = "/var/lib/rime/nix/catalogue-outs.json";

  attrPackage = attr: lib.attrByPath (lib.splitString "." attr) (throw "rime.catalogue: no package ${attr}") pkgs;

  # One closure per class 2 entry: the package itself when it is one attribute, a buildEnv when it is several.
  appPackage = name: e:
    if builtins.length e.attrs == 1 then
      attrPackage (builtins.head e.attrs)
    else
      pkgs.buildEnv {
        name = "rime-app-${name}";
        paths = map attrPackage e.attrs;
      };

  buildable = e: e.kind == "nix" && (e.attrs or [ ]) != [ ];

  apps = lib.mapAttrs appPackage (lib.filterAttrs (_: buildable) entries);

  renderEntry = name: e:
    if e.kind == "nix" then
      builtins.removeAttrs e [ "attrs" ]
      // {
        buildable = buildable e;
      }
      // lib.optionalAttrs (buildable e) { version = lib.getVersion (attrPackage (builtins.head e.attrs)); }
    else if e.kind == "user-npm" then
      e // { npm = "${cfg.npm.package}/bin/npm"; }
    else
      e;

  renderFeature = name: f:
    builtins.removeAttrs f [ "option" ]
    // {
      in_system = lib.attrByPath f.option false config;
      unit = "rime-feature@${name}.target";
      switch = "${switchDir}/rime-feature@${name}.target.on";
    };

  catalogueJson = {
    inherit (data) schema aliases refuse;
    flathub_remote = "flathub";
    switch_dir = switchDir;
    apps_profile = appsProfile;
    outs_file = outsFile;
    features = lib.mapAttrs renderFeature data.features;
    entries = lib.mapAttrs renderEntry entries;
  };
in
{
  options.rime.catalogue = {
    enable = lib.mkEnableOption "Rime's package catalogue (`rime install` on NixOS), nix-ld and appimage-run";

    extraEntries = lib.mkOption {
      type = lib.types.attrsOf (lib.types.attrsOf lib.types.anything);
      default = { };
      description = "Entries added to (or replacing) nix/catalogue/catalogue.nix. Same shape as its `entries`.";
    };

    npm.package = lib.mkPackageOption pkgs "nodejs" {
      extraDescription = "npm for the per-user `user-npm` route (claude-code). Its closure is in the system.";
    };

    nixLd.extraLibraries = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "Libraries added to Rime's nix-ld set.";
    };

    appimage.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        appimage-run with binfmt: an AppImage runs when it is executed. This reverses bootc's "unpack, never
        execute" policy (docs/packages.md), and puts appimage-run's FHS environment in the system closure.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."rime/catalogue.json".text = builtins.toJSON catalogueJson;

    # Class 2 apps: the profile `rime install` writes, on PATH and XDG_DATA_DIRS of every login.
    environment.profiles = lib.mkAfter [ appsProfile ];

    # claude-code's npm route puts it in ~/.local/bin (rime-pkg user_tool_run; bootc: Fedora's profile had it).
    environment.localBinInPath = true;

    # Class 1 runtime switches.
    systemd.targets."rime-feature@" = {
      description = "Rime system feature %i";
      unitConfig.ConditionPathExists = "${switchDir}/%n.on";
    };
    systemd.services.rime-features = {
      description = "Start the Rime system features this machine has switched on";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        for f in ${switchDir}/rime-feature@*.target.on; do
          [ -f "$f" ] || continue
          f=''${f##*/}
          ${config.systemd.package}/bin/systemctl start --no-block "''${f%.on}"
        done
      '';
    };
    systemd.tmpfiles.rules = [
      "d ${switchDir} 0755 root root -"
      "d /var/lib/rime/pkg 0755 root root -"
      "d /var/lib/rime/nix 0755 root root -"
    ];

    # Dynamic ELF programs from outside Nix: claude-code's native binary, ~/.local/bin tools, Electron apps, unpacked
    # vendor tarballs. nix-ld's own defaults (zlib, zstd, libstdc++, curl, openssl, ...) plus the GUI/Electron set.
    programs.nix-ld = {
      enable = true;
      libraries =
        (with pkgs; [
          alsa-lib
          at-spi2-atk
          at-spi2-core
          cairo
          cups
          dbus
          expat
          fontconfig
          freetype
          fuse
          gdk-pixbuf
          glib
          gtk3
          icu
          libGL
          libappindicator-gtk3
          libcap
          libdrm
          libgbm
          libnotify
          libpulseaudio
          libsecret
          libusb1
          libuuid
          libva
          libxkbcommon
          mesa
          nspr
          nss
          pango
          pipewire
          sqlite
          systemdLibs
          vulkan-loader
          wayland
          libX11
          libXcomposite
          libXcursor
          libXdamage
          libXext
          libXfixes
          libXi
          libXrandr
          libXrender
          libXScrnSaver
          libXtst
          libxcb
          libxkbfile
          libxshmfence
        ])
        ++ cfg.nixLd.extraLibraries;
    };

    programs.appimage = lib.mkIf cfg.appimage.enable {
      enable = true;
      binfmt = true;
    };

    # For the release job (D6): every class 2 app, and the name -> store path table the update client writes to
    # ${outsFile}. Building this builds every app; the toplevel never references it.
    system.build.rimeCatalogue = pkgs.writeTextFile {
      name = "rime-catalogue-outs.json";
      text = builtins.toJSON {
        schema = 1;
        apps = lib.mapAttrs (_: p: "${p}") apps;
      };
      passthru = { inherit apps; };
    };
  };
}
