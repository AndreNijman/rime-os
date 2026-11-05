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
# host.toml `[boot] microcode = "intel" | "amd" | "both" | "none"` picks the early microcode every entry loads.
#
# With a Rime-signed kernel the hook also puts the MOK certificate on the ESP (EFI/rime/rime-mok.der) and an "Enrol
# the Rime OS Secure Boot key" entry in custom.cfg, the same block bootc's rime-secureboot-key writes: the way back
# when Secure Boot refuses the kernel because the key was never enrolled (nix/pkgs/rime-boot/README.md).
{ config, lib, pkgs, options, ... }:

let
  cfg = config.rime.boot;
  # The certificate as DER, made at build time: MokManager's "Enroll key from disk" reads DER only. Input PEM or DER.
  mokDer =
    if cfg.mokCertificate == null then
      null
    else
      pkgs.runCommand "rime-mok.der" { nativeBuildInputs = [ pkgs.openssl ]; } ''
        set -euo pipefail
        if openssl x509 -in ${cfg.mokCertificate} -noout 2>/dev/null; then
          openssl x509 -in ${cfg.mokCertificate} -outform DER -out $out
        else
          openssl x509 -inform DER -in ${cfg.mokCertificate} -outform DER -out $out
        fi
        openssl x509 -inform DER -in $out -noout -subject
      '';
  # boot.nix is also used without nix/modules/kernel.nix (nix/probe/ovmf/boothook): then there is no signed kernel.
  kernelSigned =
    lib.hasAttrByPath [ "rime" "kernel" "signing" "imageSignature" ] options
    && config.rime.kernel.enable
    && config.rime.kernel.signing.imageSignature != null;
  hookConfig = pkgs.writeText "rime-boot.json" (builtins.toJSON {
    inherit (cfg) layout configurationLimit timeout reserveMiB grubPath;
    bootDir = cfg.mountPoint;
    hostToml = cfg.hostToml;
    mokCert = mokDer;
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

    mokCertificate = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = if kernelSigned then config.rime.kernel.signing.certificate else null;
      defaultText = lib.literalExpression ''
        config.rime.kernel.signing.certificate when rime.kernel is on and its image is signed, else null'';
      description = ''
        The certificate (PEM or DER) the kernel is signed with. Set: every hook run puts it on the ESP as
        EFI/rime/rime-mok.der and writes the "Enrol the Rime OS Secure Boot key" entry into custom.cfg (only on an
        ESP that holds Fedora's MokManager, EFI/fedora/mmx64.efi). null: nothing to enrol; the hook leaves any such
        entry already in custom.cfg as it is.
      '';
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

    # CPU microcode, early. One generic toplevel boots every machine (D9), so the toplevel names BOTH vendors'
    # early-update cpios (nixpkgs' own files, unchanged) in its bootspec, and the hook loads them as separate
    # `initrd` files before the generation's initrd. The kernel's early loader reads the uncompressed cpios at the
    # start of the concatenated image (nix/probe/ovmf/mok, boots U1/U2). host.toml [boot] microcode =
    # "intel" | "amd" | "both" | "none" picks per machine (the installer writes the CPU's vendor). Split rather than
    # prepended (boot.initrd.prepend, NixOS's default) so a microcode file is stored ONCE on the ESP for every
    # generation and a machine carries only its vendor's (15 MB for Intel, 0.3 MB for AMD, per generation before;
    # docs/nixos/boot-matrix.md "ESP budget"). checks.boot-microcode proves it on the shipped toplevel.
    hardware.cpu.intel.updateMicrocode = lib.mkDefault false;
    hardware.cpu.amd.updateMicrocode = lib.mkDefault false;
    boot.bootspec.extensions."org.rimeos.microcode.v1" = {
      intel = "${pkgs.microcode-intel}/intel-ucode.img";
      amd = "${pkgs.microcode-amd}/amd-ucode.img";
    };

    # A machine installed from Windows: the Rime installer app staged the live environment on this ESP and the
    # installer ran from it, so it could not delete it (rime-install remove_setup_entry). It goes at the first
    # boot, as on bootc (rime-install install_staged_cleanup_in); inert everywhere else.
    systemd.services.rime-staged-cleanup = {
      description = "Remove the installer Rime OS was set up from";
      unitConfig = {
        RequiresMountsFor = cfg.mountPoint;
        ConditionPathExists = "${cfg.mountPoint}/rimeinst/handoff.cfg";
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/rm -rf ${cfg.mountPoint}/rimeinst ${cfg.mountPoint}/EFI/rimeinst";
      };
      wantedBy = [ "multi-user.target" ];
    };

    # What the Secure Boot entry's own text tells a user to run once they have started Rime OS with Secure Boot off
    # and the key file was missing: the hook again, which puts the certificate and the entry in place.
    systemd.services.rime-secureboot-key = {
      description = "Put the Rime Secure Boot key and its boot menu entry in place";
      unitConfig.RequiresMountsFor = cfg.mountPoint;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${hook}";
      };
    };

    # With Secure Boot off the entry is hidden, so a key is enrolled later the usual way: `sudo mokutil --import
    # /var/lib/rime/secureboot/rime-mok.der` (the path the installer's notes give).
    environment.systemPackages = [ cli ] ++ lib.optional (cfg.mokCertificate != null) pkgs.mokutil;
    system.build.rimeBootHook = hook;
  };
}
