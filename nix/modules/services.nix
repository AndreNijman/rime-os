# Rime services: rimed and its helper daemons, libexec scripts, update/live engine, timers (PLAN.md 4.3, 4.4).
#
# This file's rimed part (PLAN.md 4.3): rimed, rime-secretd, rime-lid (system), rime-remoted, rime-agentd and
# rime-aid (user), their D-Bus policy, polkit actions and state.
#
# The unit files are the bootc ones, shipped by pkgs.rime.rimed under lib/systemd/{system,user} with only the program
# paths rewritten (nix/pkgs/rimed.nix), and handed to systemd through systemd.packages. So every hardening line the
# bootc image ships (ProtectSystem, CapabilityBoundingSet, MemoryDenyWriteExecute, RestrictAddressFamilies,
# ReadOnlyPaths, ...), every StateDirectory and the [Install] sections arrive unchanged: one source for both systems,
# nothing to drift. The [Install] sections matter: `rime agent enable` and `rime remote` run
# `systemctl --user enable --now rime-agentd|rime-remoted`, which a NixOS-generated unit (no [Install]) refuses.
#
# What this module adds on top is what bootc's Containerfile did by hand:
#   - `systemctl enable` / `systemctl --global enable`  -> wantedBy below (NixOS writes the .wants links);
#   - the programs each unit runs by name               -> `path` (on bootc: the default /usr PATH);
#   - D-Bus policy + activation, polkit actions          -> services.dbus.packages, systemPackages;
#   - /usr/lib/tmpfiles.d/rime-ai.conf                   -> systemd.tmpfiles.rules.
# Declaring a unit here writes an overrides.conf drop-in next to the shipped unit; it carries PATH and the wants,
# never a hardening change. rime-agentd and rime-aid are NOT declared: both are opt-in on bootc (no enable), and
# rime-agentd's own Environment=PATH (rewritten in the package for NixOS) must not be replaced by a NixOS one.
#
# SELinux is dropped (decision A2): the CIL modules and file contexts bootc installs for these daemons have no NixOS
# counterpart. Users: none of these run as a dedicated account (rimed, rime-secretd and rime-lid are root by design,
# the others are per-user), so there is no users.users entry to translate.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.services;
  rimed = cfg.rimed.package;
  # The system's own programs, as bootc's default PATH (/usr/sbin:/usr/bin) gave them: whatever this configuration
  # installs (nvidia-smi, nmcli, ...). A string, so it adds /run/current-system/sw/bin without adding a package.
  systemPath = "/run/current-system/sw";
in
{
  options.rime.services = {
    enable = lib.mkEnableOption "Rime's system services (rimed and helpers)";

    rimed.package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.rime.rimed;
      defaultText = lib.literalExpression "pkgs.rime.rimed";
      description = "The rimed workspace: rimed, rime, rime-secretd, rime-agentd, rime-remoted, rime-aid.";
    };
  };

  config = lib.mkIf cfg.enable {
    # ── binaries, D-Bus, polkit ──────────────────────────────────────────────
    environment.systemPackages = [ rimed ];
    # rimed's compiled-in paths name /run/current-system/sw/libexec/rime-* (rime-libexec's engines) and
    # /run/current-system/sw/share/rime-shell (Rime Shell). Link both into the system path; the packages that fill
    # them are added by their own modules.
    environment.pathsToLink = [ "/libexec" "/share/rime-shell" ];

    # org.rimeos.Rimed1: root owns the name, every local user may call it, polkit decides the rest (the bus policy as
    # bootc ships it); the activation file starts rimed.service.
    services.dbus.packages = [ rimed ];
    # org.rimeos.rimed.* and org.rimeos.agent.* actions, from $out/share/polkit-1/actions via systemPackages.
    security.polkit.enable = true;

    systemd.packages = [ rimed ];

    # ── system units (bootc: systemctl enable) ───────────────────────────────
    systemd.services.rimed = {
      wantedBy = [ "multi-user.target" ];
      path = [ systemPath ];
    };

    systemd.services.rime-secretd = {
      wantedBy = [ "multi-user.target" ];
      # The broker sets its children's PATH itself (rime_paths::SAFE_PATH); this is for the daemon's own lookups.
      path = [ systemPath ];
    };

    # Exits 0 on a machine without a lid; holds a handle-lid-switch inhibitor while live work runs (P1-063).
    systemd.services.rime-lid = {
      wantedBy = [ "multi-user.target" ];
      # What it runs by name: runuser and rfkill (util-linux), iw, busctl and systemd-inhibit (systemd, already on
      # every NixOS unit's PATH), sleep (coreutils, likewise), rime, and nmcli when the system has NetworkManager.
      path = [ pkgs.util-linux pkgs.iw rimed systemPath ];
    };

    # ── user units ───────────────────────────────────────────────────────────
    # rime-remoted: enabled for every account (bootc: systemctl --global enable). Its Wants= pulls in rime-agentd.
    # ConditionUser=!@system in the unit keeps it off system accounts (greetd's user manager).
    systemd.user.services.rime-remoted = {
      wantedBy = [ "default.target" ];
      # avahi-publish-service for LAN discovery, when the system has avahi.
      path = [ systemPath ];
    };

    # ── state ────────────────────────────────────────────────────────────────
    # The shared model store rime-aid reads (bootc: /usr/lib/tmpfiles.d/rime-ai.conf, line for line). Staging is
    # 0700: a partly written blob must not be readable before its digest is checked.
    systemd.tmpfiles.rules = [
      "d /var/lib/rime 0755 root root -"
      "d /var/lib/rime/ai 0755 root root -"
      "d /var/lib/rime/ai/models 0755 root root -"
      "d /var/lib/rime/ai/models/blobs 0755 root root -"
      "d /var/lib/rime/ai/models/manifests 0755 root root -"
      "d /var/lib/rime/ai/staging 0700 root root -"
    ];
  };
}
