# Other desktops (docs/nixos/desktops.md §8.1).
#
#   desktops-eval   Evaluation only (no system is built): every release variant (nix/release/variants.nix, 16 with the
#                   three desktops) evaluates to a toplevel, with these facts read from its configuration:
#                     - no power-profiles-daemon and no system76-power: neither enabled nor in any package list
#                       (system.path, D-Bus, systemd), so rimed is the only owner of the PowerProfiles API (D6);
#                     - Rime's session-only environment (qt6ct, the Electron hint) is not in /etc/set-environment (D7);
#                       a variant with no desktop has no QT_QPA_PLATFORMTHEME there at all;
#                     - each enabled desktop's sessionIds are in the display manager's session set, and Rime's
#                       own sessions are still there;
#                     - no variant has two desktops, and each carries exactly the desktop its name says;
#                     - kernel, initrd and kernel parameters are plain's (a desktop never changes how the machine boots,
#                       so installing one never needs a restart for that reason, §3.5);
#                     - the blueman autostart override (NotShowIn GNOME/KDE/COSMIC) is in every system with
#                       blueman, the kwallet-pam override and kwalletrc (secrets API off) in every KDE system, and
#                       COSMIC's system76-scheduler is off.
{ pkgs, self }:
let
  lib = pkgs.lib;
  variants = import ../release/variants.nix { inherit self lib; };
  table = import ../desktops;
  ids = lib.attrNames table;
  forbidden = [ "power-profiles-daemon" "system76-power" ];
  nameOf = p: p.pname or (builtins.parseDrvName (p.name or "")).name;

  facts = name: v:
    let
      c = v.system.config;
      pkgsIn = l: lib.filter (n: lib.elem n forbidden) (map nameOf l);
      vars = c.environment.variables;
      setEnv = c.system.build.setEnvironment.text;
      d = n: builtins.unsafeDiscardStringContext n;
    in
    {
      toplevel = d c.system.build.toplevel.drvPath;
      desktops = lib.filter (id: c.rime.desktops.${id}.enable) ids;
      wanted = lib.filter (id: lib.elem id v.selected) ids;
      ppd = c.services.power-profiles-daemon.enable;
      system76Power = c.hardware.system76.power-daemon.enable;
      system76Scheduler = c.services.system76-scheduler.enable;
      forbiddenPackages = {
        systemPath = pkgsIn c.environment.systemPackages;
        dbus = pkgsIn c.services.dbus.packages;
        systemd = pkgsIn c.systemd.packages;
      };
      qtTheme = vars.QT_QPA_PLATFORMTHEME or null;
      setEnvHasQt6ct = lib.hasInfix "qt6ct" setEnv;
      setEnvHasQtTheme = lib.hasInfix "QT_QPA_PLATFORMTHEME" setEnv;
      setEnvHasElectronHint = lib.hasInfix "ELECTRON_OZONE_PLATFORM_HINT" setEnv;
      sessions = c.services.displayManager.sessionData.sessionNames;
      wantSessions = lib.concatMap (id: table.${id}.sessionIds) (lib.filter (id: c.rime.desktops.${id}.enable) ids);
      kernel = d c.system.build.kernel.drvPath;
      initrd = d c.system.build.initialRamdisk.drvPath;
      kernelParams = c.boot.kernelParams;
      blueman = c.services.blueman.enable;
      bluemanOverride = c.environment.etc ? "xdg/autostart/blueman.desktop";
      kwalletOverride = c.environment.etc ? "xdg/autostart/pam_kwallet_init.desktop";
      kwalletrc = c.environment.etc."xdg/kwalletrc".text or null;
      greetd = c.services.greetd.enable;
      otherDisplayManagers = lib.filter (x: x) [
        c.services.displayManager.gdm.enable
        c.services.displayManager.sddm.enable
        (c.services.displayManager.cosmic-greeter.enable or false)
      ];
    };

  input = pkgs.writeText "desktops-eval.json" (builtins.toJSON {
    inherit ids;
    variants = lib.mapAttrs facts variants;
  });
in
{
  desktops-eval = pkgs.runCommand "rime-desktops-eval" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 -I ${./desktops-eval-check.py} ${input} | tee $out
  '';
}
