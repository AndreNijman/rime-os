# The old `apex` unit names, as systemd aliases of the Rime units (PLAN.md D1).
#
# bootc: files/scripts/install-rename-compat links /usr/lib/systemd/{system,user}/apex-X -> rime-X. What depends on
# it is a migrated HOME: a user who enabled the agent runtime or the remote daemon before the rename still has
# ~/.config/systemd/user/default.target.wants/apex-agentd.service (and apex-remoted), pointing at a unit file that no
# longer exists. systemd reads a .wants/ entry by its NAME, so the link starts whatever `apex-agentd.service` loads
# as; with no unit of that name it is silently skipped and the runtime never starts. An alias gives the name a unit.
#
# Only the unit names are ported. The libexec and /usr/share aliases have no counterpart on NixOS (no /usr), and a
# NixOS machine is a fresh install, so no old /etc enablement link survives; the system aliases are kept for the
# `systemctl status apexd`-style muscle memory the bootc transition promised.
#
# An alias is only made for a unit this configuration actually has: aliasing an absent unit would make
# systemd.units create an EMPTY unit file of the Rime name. Package units (rimed's) exist whenever rime.services
# does; module units are checked by name.
{ config, lib, ... }:

let
  cfg = config.rime.compat.apexNames;
  old = "apex"; # rime-rename: keep (the old name, spelled once)

  # [ Rime unit name, present? ] — the suffix after the name is the same on both sides, as install-rename-compat
  # writes it ("d.service": apexd.service -> rimed.service).
  sys = config.systemd.services;
  usr = config.systemd.user.services;
  pkgUnits = config.rime.services.enable;

  systemUnits = {
    "d.service" = pkgUnits;
    "-lid.service" = pkgUnits;
    "-secretd.service" = pkgUnits;
    "-firewall.service" = sys ? "rime-firewall";
    "-flathub-setup.service" = sys ? "rime-flathub-setup";
    "-greet-publish.service" = sys ? "rime-greet-publish";
    "-guest-session@.service" = sys ? "rime-guest-session@";
    "-guest-wipe@.service" = sys ? "rime-guest-wipe@";
    "-switch-greeter@.service" = sys ? "rime-switch-greeter@";
  };
  userUnits = {
    "-agentd.service" = pkgUnits;
    "-aid.service" = pkgUnits;
    "-remoted.service" = pkgUnits || usr ? "rime-remoted";
    "-shell-firstrun.service" = usr ? "rime-shell-firstrun";
    "-storage-notice.service" = usr ? "rime-storage-notice";
    "-storage-notice.timer" = config.systemd.user.timers ? "rime-storage-notice";
  };

  aliasesFor =
    units:
    lib.mapAttrs' (suffix: _: lib.nameValuePair "rime${suffix}" { aliases = [ "${old}${suffix}" ]; }) (
      lib.filterAttrs (_: present: present) units
    );
in
{
  options.rime.compat.apexNames.enable = lib.mkOption {
    type = lib.types.bool;
    default = config.rime.services.enable;
    defaultText = lib.literalExpression "config.rime.services.enable";
    description = "Alias the pre-rename `apex` unit names (apexd, apex-agentd, ...) to the Rime units, so enablement links in a migrated home keep working.";
  };

  config = lib.mkIf cfg.enable {
    systemd.units = aliasesFor systemUnits;
    systemd.user.units = aliasesFor userUnits;
  };
}
