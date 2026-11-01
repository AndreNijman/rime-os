# Branding: the layered ridge on every OS surface Rime owns outside the Shell and the terminal greeting
# (docs/branding.md). Imported by desktop.nix; on by default wherever the module set is.
#
#   os-release      NAME/PRETTY_NAME "Rime OS", ID=rime with ID_LIKE=nixos, LOGO=rime-os, Rime's URLs, a Rime Blue
#                   ANSI_COLOR (bootc: Containerfile.core's os-release sed, which keeps ID=fedora for dnf copr; on NixOS
#                   the one ID consumer, switch-to-configuration, compares against this same distroId).
#   icons           pkgs.rime.rime-branding: hicolor apps/rime-os (+ distributor-logo, start-here, rime-os-logo),
#                   share/rime/branding, and share/pixmaps/rime-os*.png.
#   boot splash     pkgs.rime.rime-plymouth: rime-os (plane 3 in Rime Blue) is the theme; rime-os-accent-00..23 recolour
#                   only plane 3 and follow the owner's accent, at shutdown/reboot (rime-plymouth-accent rewrites
#                   /etc/plymouth/plymouthd.conf) and at boot (the initrd unit below reads the ESP credential
#                   loader/credentials/rime.accent.cred, as bootc's dracut module rime-plymouth-accent does).
#   plymouth logo   /etc/plymouth/logo.png (the bgrt/spinner fallback, also in the initrd): the ridge, not the snowflake.
#
# Splash enablement (boot.plymouth.enable, kernel args) stays in desktop.nix; this module only picks the artwork.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.branding;
  # One file, not ${../../files}: the picker (and so the initrd) changes only when it does.
  picker = ../../files/dracut/rime-plymouth-accent/rime-plymouth-theme;
  themes = pkgs.rime.rime-plymouth;
  branding = pkgs.rime.rime-branding;
  themeDir = "${themes}/share/plymouth/themes";

  # bootc's initrd half, unchanged (files/dracut/rime-plymouth-accent/rime-plymouth-theme): read the accent bucket
  # from the ESP credential and point the initrd's plymouthd.conf at rime-os-accent-NN before plymouthd starts. The
  # NixOS systemd initrd has coreutils, udevadm and mount on /bin; the script also needs awk, sed and grep.
  initrdThemePicker = pkgs.runCommand "rime-plymouth-theme-initrd" { } ''
    {
      echo '#!${pkgs.runtimeShell}'
      echo 'export PATH=${lib.makeBinPath [ pkgs.gawk pkgs.gnused pkgs.gnugrep ]}:/bin:/sbin'
      echo 'export RIME_PLYMOUTH_THEMES=/etc/plymouth/themes RIME_PLYMOUTH_CONF=/etc/plymouth/plymouthd.conf'
      tail -n +2 ${picker}
    } > $out
    chmod +x $out
    grep -q 'theme="rime-os-accent-$nn"' $out
  '';

  # Some programs still look for a distributor logo under share/pixmaps (Fedora's fedora-logo*.png).
  pixmaps = pkgs.runCommand "rime-pixmaps" { } ''
    p=$out/share/pixmaps
    mkdir -p $p
    cp ${branding}/share/rime/branding/icons/app/rime-app-icon-dark-256.png $p/rime-os.png
    cp ${branding}/share/rime/branding/png/symbol/rime-symbol-dark-256.png $p/rime-os-logo.png
    cp ${branding}/share/rime/branding/png/symbol/rime-symbol-light-256.png $p/rime-os-logo-light.png
    ln -s rime-os-logo.png $p/distributor-logo.png
  '';
in
{
  options.rime.branding = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Rime's identity (the layered ridge) on os-release, icons, the boot splash and the Plymouth logo.";
    };
    plymouthTheme = lib.mkOption {
      type = lib.types.str;
      default = themes.defaultTheme;
      readOnly = true;
      description = "The boot splash theme Rime selects when Plymouth is on (rime-os; the accents are rime-os-accent-NN).";
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      system.nixos = {
        distroName = lib.mkDefault "Rime OS";
        distroId = lib.mkDefault "rime";
        vendorName = lib.mkDefault "Rime OS";
        variantName = lib.mkDefault "Rime";
        extraOSReleaseArgs = lib.mapAttrs (_: lib.mkDefault) {
          PRETTY_NAME = "Rime OS";
          LOGO = "rime-os";
          ANSI_COLOR = "0;38;2;145;194;220"; # Rime Blue #91C2DC
          HOME_URL = "https://rimeos.com/";
          VENDOR_URL = "https://rimeos.com/";
          DOCUMENTATION_URL = "https://github.com/AndreNijman/rime-os#readme";
          SUPPORT_URL = "https://github.com/AndreNijman/rime-os/discussions";
          BUG_REPORT_URL = "https://github.com/AndreNijman/rime-os/issues";
        };
      };

      environment.systemPackages = [ branding pixmaps ];
    }

    (lib.mkIf config.boot.plymouth.enable {
      boot.plymouth = {
        themePackages = [ themes ];
        theme = cfg.plymouthTheme;
        logo = "${branding}/share/rime/branding/png/symbol/rime-symbol-dark-128.png";
      };

      # Shutdown and reboot: rime-plymouth-accent (run by rime-greet-publish-all at boot and rime-greet-wallpaper on a
      # change) looks for the accent themes under the profile's share/plymouth/themes and rewrites this file, so it is a
      # real file (activation puts rime-os back; the next publish puts the accent back).
      environment.systemPackages = [ themes ];
      environment.pathsToLink = [ "/share/plymouth/themes" ];
      environment.etc."plymouth/plymouthd.conf".mode = "0644";

      # Boot: nixpkgs copies only boot.plymouth.theme into the initrd; the accent needs all 25 there.
      boot.initrd.systemd.contents."/etc/plymouth/themes".source = lib.mkForce themeDir;
      boot.initrd.systemd.storePaths = [ initrdThemePicker ];
      boot.initrd.availableKernelModules = [ "vfat" "nls_cp437" "nls_iso8859-1" ];
      boot.initrd.systemd.services.rime-plymouth-theme = {
        description = "Rime OS: start the boot splash in the owner's accent";
        # Before plymouthd starts, so its first frame is the chosen theme; after udev has seen the disks, so an ESP
        # can be found by its partition type (files/dracut/rime-plymouth-accent/rime-plymouth-theme.service).
        unitConfig.DefaultDependencies = false;
        after = [ "systemd-udev-trigger.service" "systemd-udevd.service" ];
        before = [ "plymouth-start.service" "shutdown.target" ];
        conflicts = [ "shutdown.target" ];
        wantedBy = [ "sysinit.target" ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${initrdThemePicker}";
          # Never hold the boot back for a colour.
          TimeoutStartSec = 5;
          StandardOutput = "kmsg";
          StandardError = "kmsg";
        };
      };
    })
  ]);
}
