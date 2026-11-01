# The host layer (PLAN.md D2): every fact that belongs to ONE machine stays out of the prebuilt, generic system
# closure and reaches the running system at boot and activation.
#
#   /var/lib/rime/host.toml   hostname, timezone, locale, console and X11 keyboard, extra mounts, the LUKS volume.
#                             Written by the bridge/installer (`rime-hostlayer carry`), read by:
#                               - the generator rime-hostlayer-mounts: one .mount unit in /run per [[mount]], its
#                                 options as written there (nofail kept: a missing disk never stops the boot);
#                               - activation (`rime-hostlayer apply`): /etc/hostname, /etc/localtime,
#                                 /etc/locale.conf, /etc/vconsole.conf, /etc/X11/xorg.conf.d/00-keyboard.conf.
#                             So the NixOS options behind those files are left imperative here (no hostName,
#                             time.timeZone null as base.nix has it, i18n.imperativeLocale, no static 00-keyboard).
#   /var/lib/rime/switch/     runtime switches. bootc enabled some units at run time (`systemctl enable`), which a
#                             NixOS /etc cannot take. Each such unit carries a ConditionPathExists on a file here:
#                             <unit>.on for one that is off unless switched on (a disposable guest's wipe unit),
#                             <unit>.off for one that is on unless switched off (rime-firewall). rime-user (root)
#                             and rimed's org.rimeos.Rimed1.Switch (polkit org.rimeos.rimed.manage-switches) flip them.
#   /var/lib/rime/{guest-accounts,firewall.d,sing-box/}
#                             Rime state that bootc kept under /etc; /etc keeps a link to each for readers outside
#                             this repository (Rime Shell's firewall view reads /etc/rime/firewall.d).
#   /etc/subuid, /etc/subgid  NixOS rewrites them from declared users only; a carried account's ranges (katana:
#                             andre 524288:65536, ghrunner 1000000:65536) are kept across every activation.
#
# The /etc carry itself (`rime-hostlayer carry`) runs from the bridge (D1) and the installer (D9) into the new root
# before its first activation; nix/tests/hostlayer.nix stands in for them with an activation hook.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.hostlayer;
  pkg = pkgs.rime.rime-hostlayer;
  hl = "${pkg}/bin/rime-hostlayer";
  switchDir = "/var/lib/rime/switch";

  # What `carry` checks the old accounts against: every account this system declares, by NAME (the attribute
  # name can differ), with its static id or null when NixOS allocates it.
  declared = {
    users = lib.listToAttrs (map (u: lib.nameValuePair u.name {
      inherit (u) uid group isSystemUser isNormalUser;
    }) (lib.filter (u: u.enable) (lib.attrValues config.users.users)));
    groups = lib.listToAttrs (map (g: lib.nameValuePair g.name { inherit (g) gid; })
      (lib.attrValues config.users.groups));
  };

  firewall = config.rime.security.enable;
in
{
  options.rime.hostlayer = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Rime's host layer: host.toml, runtime switches, the kept subordinate ids (PLAN.md D2).";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ pkg ];

    environment.etc."rime/hostlayer/declared-accounts.json".text = builtins.toJSON declared;
    # `<unit> <state when no marker says otherwise>`, read by rime-hostlayer and rimed.
    environment.etc."rime/hostlayer/switches".text = ''
      rime-guest-session@.service off
    '' + lib.optionalString firewall ''
      rime-firewall.service on
    '';

    # ── host.toml → the system ──────────────────────────────────────────────
    # The closure names no machine; `apply` writes /etc/hostname from host.toml ("rime" when nobody named it).
    # Above mkDefault, so a test node's name (the test framework's mkDefault) still wins.
    networking.hostName = lib.mkOverride 1100 "";
    i18n.imperativeLocale = true;
    # graphical-desktop.nix writes this file from services.xserver.xkb; here it is the machine's (host.toml), and
    # the configuration's text is only the default `apply` writes when host.toml names no X11 layout.
    environment.etc."X11/xorg.conf.d/00-keyboard.conf".enable = lib.mkForce false;
    environment.etc."rime/hostlayer/00-keyboard.conf" =
      lib.mkIf config.services.graphical-desktop.enable {
        inherit (config.environment.etc."X11/xorg.conf.d/00-keyboard.conf") source;
      };
    systemd.generators.rime-hostlayer-mounts = "${pkg}/lib/systemd/system-generators/rime-hostlayer-mounts";

    system.activationScripts.rimeHostlayerApply = lib.stringAfter [ "etc" "users" ] "${hl} apply";

    # ── subordinate ids ─────────────────────────────────────────────────────
    system.activationScripts.rimeHostlayerSubidSave = lib.stringAfter [ "specialfs" ] "${hl} subid save";
    system.activationScripts.users.deps = [ "rimeHostlayerSubidSave" ];
    system.activationScripts.rimeHostlayerSubidRestore = lib.stringAfter [ "users" ] "${hl} subid restore";

    # ── accounts carried from bootc ─────────────────────────────────────────
    # bootc's default interactive shell is zsh (Containerfile.core), so a carried person's login shell is
    # /run/current-system/sw/bin/zsh; it must exist and be in /etc/shells.
    programs.zsh.enable = lib.mkDefault true;
    # bootc's sshd served an ECDSA host key too; a client that knows only that one must still match.
    services.openssh.hostKeys = lib.mkDefault [
      { type = "rsa"; bits = 4096; path = "/etc/ssh/ssh_host_rsa_key"; }
      { type = "ed25519"; path = "/etc/ssh/ssh_host_ed25519_key"; }
      { type = "ecdsa"; path = "/etc/ssh/ssh_host_ecdsa_key"; }
    ];

    # ── runtime switches ────────────────────────────────────────────────────
    # A disposable guest's wipe unit (base.nix): pulled in by every login's user-runtime-dir@<uid> (the bootc
    # unit's WantedBy), run only for a uid whose switch is on. What `systemctl enable` did on bootc.
    systemd.services."rime-guest-session@" = lib.mkIf config.rime.base.enable {
      unitConfig.ConditionPathExists = "${switchDir}/%n.on";
    };
    systemd.services."user-runtime-dir@" = lib.mkIf config.rime.base.enable {
      wants = [ "rime-guest-session@%i.service" ];
    };
    systemd.services.rime-firewall = lib.mkIf firewall {
      unitConfig.ConditionPathExists = "!${switchDir}/%n.off";
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/rime 0755 root root -"
      "d ${switchDir} 0755 root root -"
      "d /var/lib/rime/sing-box 0755 root root -"
      "L /etc/sing-box - - - - /var/lib/rime/sing-box"
      "L /etc/rime/guest-accounts - - - - /var/lib/rime/guest-accounts"
    ] ++ lib.optionals firewall [
      "L /etc/rime/firewall.d - - - - /var/lib/rime/firewall.d"
    ];
  };
}
