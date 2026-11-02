# Rime's boot chain on NixOS (PLAN.md D3 v1, D9, decision A8): Fedora's Microsoft-signed shim -> Fedora-signed GRUB
# (bootupd's static config, installed by the installer or left by bootc) -> the MOK-signed kernel and NixOS initrd.
#
# NixOS's own GRUB and systemd-boot installers are not used (both install loader binaries; PLAN 4.1). Instead
# boot.loader.external runs nix/pkgs/rime-boot on every `switch-to-configuration boot|switch` and on
# `nixos-install`: it writes BLS entries (fresh installs, the ESP is /boot) or a Rime-owned file sourced from GRUB's
# custom.cfg (migrated bootc machines), copies kernels and initrds, and prunes old generations.
#
# The layout is a per-machine fact (D2 host layer): /var/lib/rime/host.toml `[boot] layout = "bls" | "custom-cfg"`
# wins over rime.boot.layout, and "auto" detects ostree's /boot/loader symlink. Per-machine kernel arguments (LUKS
# rd.luks.uuid=, rd.vconsole.keymap=, wave 2) come from host.toml `[boot] kernel_params = [ ... ]`.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.boot;
  hookConfig = pkgs.writeText "rime-boot.json" (builtins.toJSON {
    inherit (cfg) layout configurationLimit timeout reserveMiB grubPath;
    bootDir = cfg.mountPoint;
    hostToml = cfg.hostToml;
  });
  # The hook NixOS calls with the toplevel as $1, and the same program with the same configuration on PATH for
  # `rime-boot check` (D6 8.7) and for rime gc / rime rollback to re-run it.
  hook = pkgs.writeShellScript "rime-boot-install-hook" ''
    exec ${lib.getExe pkgs.rime.rime-boot} --config ${hookConfig} install "$@"
  '';
  cli = pkgs.writeShellScriptBin "rime-boot" ''
    exec ${lib.getExe pkgs.rime.rime-boot} --config ${hookConfig} "$@"
  '';
in
{
  options.rime.boot = {
    enable = lib.mkEnableOption ''
      Rime's boot hook (boot.loader.external): GRUB menu entries for Fedora's shim + GRUB, the D3 v1 chain.
      Hosts that boot from a disk turn it on; VM tests that boot the kernel directly leave it off'';

    layout = lib.mkOption {
      type = lib.types.enum [ "auto" "bls" "custom-cfg" ];
      default = "auto";
      description = ''
        bls: one BLS entry per generation in <mountPoint>/loader/entries (fresh installs; Fedora GRUB's blscfg
        reads them). custom-cfg: every entry in <mountPoint>/grub2/rime-nixos.cfg, sourced from custom.cfg
        (migrated bootc machines; ostree's /boot/loader is never touched). auto: ostree's loader present ->
        custom-cfg, else bls. /var/lib/rime/host.toml [boot] layout overrides this at run time.
      '';
    };

    mountPoint = lib.mkOption {
      type = lib.types.str;
      default = "/boot";
      description = "Where the filesystem GRUB reads the menu from is mounted (the ESP, or bootc's /boot).";
    };

    configurationLimit = lib.mkOption {
      type = lib.types.ints.positive;
      default = 10;
      description = ''
        Newest generations listed in the menu. The profile's current generation, the booted one and every
        pinned one (/nix/var/nix/gcroots/rime/pin-<N>) are always listed as well.
      '';
    };

    timeout = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 5;
      description = ''
        Seconds GRUB shows the menu (timeout_style=menu). v1 recovery from a bad generation is manual, through
        this menu (D3, R22), so it is always shown; bootupd's static config would show it for 1 s. 5 s: long
        enough to read the list and press an arrow key (any key stops the countdown), short enough not to slow
        every boot (docs/nixos/boot-matrix.md, "Menu").
      '';
    };

    reserveMiB = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 32;
      description = ''
        Floor of free space on the boot filesystem. The hook refuses, writing nothing, when copying the new
        kernels and initrds would leave less than this.
      '';
    };

    grubPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        The path GRUB sees for mountPoint on its filesystem ("" for an ESP mounted at /boot, "/boot" for a
        directory at the top of bootc's btrfs). null: derived at run time from /proc/self/mountinfo.
      '';
    };

    hostToml = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/rime/host.toml";
      description = "The host layer file (PLAN.md D2) whose [boot] table may set layout and kernel_params.";
    };
  };

  config = lib.mkIf cfg.enable {
    boot.loader.external = {
      enable = true;
      installHook = hook;
    };
    # boot.loader.external already defaults these to false; forced here so nothing turns a loader installer back on.
    boot.loader.grub.enable = lib.mkForce false;
    boot.loader.systemd-boot.enable = lib.mkForce false;

    # CPU microcode, early (wave-1 handover): one generic toplevel boots every machine (D9), so both vendors'
    # update cpios are prepended to the initrd (boot.initrd.prepend, first in the file); the kernel's early loader
    # takes the one matching the CPU. The hook copies <toplevel>/initrd whole, so the entry's `initrd` line names
    # a file that starts with them (checks.boot-microcode). A host that knows its vendor may turn the other off.
    hardware.cpu.intel.updateMicrocode = lib.mkDefault true;
    hardware.cpu.amd.updateMicrocode = lib.mkDefault true;

    environment.systemPackages = [ cli ];
    system.build.rimeBootHook = hook;
  };
}
