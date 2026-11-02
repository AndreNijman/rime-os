# TPM2 unlock of LUKS volumes, and the TPM-only re-enrolment (PLAN.md D3 "TPM", S3; p2-gates T1-T3).
#
# Boot: the initrd carries the TPM2 drivers and systemd's libcryptsetup-token-systemd-tpm2 plugin (nixpkgs
# boot.initrd.systemd.tpm2 + luksroot), so a volume named on the kernel command line (rd.luks.name=, from host.toml
# [boot] kernel_params) unseals its systemd-tpm2 token by itself and asks for the passphrase or the recovery key only
# when the TPM refuses (PCR 7 changed, TPM cleared, no TPM). Which volumes exist is nix/modules/luks.nix's
# (rime.luks.*); this module defines none.
#
# Re-enrolment: rime-luks-tpm-reenrol@<luks-uuid>.service runs `rime-luks-enroll --tpm-only`. It asks ONCE for the
# passphrase or the recovery key (systemd-ask-password: `systemctl start` answers it on the terminal it runs in),
# wipes the TPM slot in its own systemd-cryptenroll call, enrols a new one bound to PCR 7 (Secure Boot enforcing
# only), and fails if any keyslot or token that is not the TPM's changed: the recovery key the owner filed at install
# keeps working. It also fails (exit 3) when it ends without a TPM slot (Secure Boot off, no TPM, a refused
# enrolment), so the unit never reports success for a volume that will ask at every boot. bootc's rime-luks-enroll wiped it and printed a new one (S3); --tpm-only never does. The unit is not
# started by anything: the OVMF lab (nix/probe/ovmf/tpm, README.md: boots P3 and N1) measured that PCR 7 is the
# same on NixOS as on bootc Rime when shim, GRUB and the MOK stay the same (PCR 7 records the key that signed the
# kernel, not the kernel), so the move itself needs no re-enrolment. It is for the cases that DO
# change PCR 7 (a new Secure Boot key, a dbx update, a slot sealed to another chain), run by the owner:
#
#   sudo systemctl start rime-luks-tpm-reenrol@$(cryptsetup luksUUID /dev/<part>).service
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.luksTpm;
  enroll = "${pkgs.rime.rime-libexec}/libexec/rime-luks-enroll";
  reenrol = pkgs.writeShellScript "rime-luks-tpm-reenrol" ''
    set -euo pipefail
    uuid=$1
    dev=/dev/disk/by-uuid/$uuid
    [ -b "$dev" ] || { echo "rime-luks-tpm-reenrol: no LUKS volume with UUID $uuid" >&2; exit 1; }
    # One prompt for every systemd-cryptenroll call below (the wipe and the enrolment are separate calls).
    PASSWORD=$(${pkgs.systemd}/bin/systemd-ask-password --timeout=0 --id="rime-luks-tpm:$uuid" \
      "Passphrase or recovery key for $uuid (re-binding its TPM slot):")
    export PASSWORD
    exec ${enroll} --tpm-only --device "$dev"
  '';
in
{
  options.rime.luksTpm.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = ''
      TPM2 unlock of LUKS volumes in the initrd, and rime-luks-tpm-reenrol@<luks-uuid>.service: re-bind a volume's
      TPM slot to PCR 7 without touching its recovery key or passphrase slots.
    '';
  };

  config = lib.mkIf cfg.enable {
    # nixpkgs' default (the systemd package has the TPM2 units), stated: the token plugin and tpm_crb/tpm_tis in the
    # initrd depend on it.
    boot.initrd.systemd.tpm2.enable = true;
    systemd.tpm2.enable = true;

    systemd.services."rime-luks-tpm-reenrol@" = {
      description = "Re-bind the TPM2 slot of LUKS volume %i to PCR 7 (keeps its recovery key)";
      documentation = [ "file:${enroll}" ];
      after = [ "tpm2.target" ];
      wants = [ "tpm2.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${reenrol} %i";
        PrivateNetwork = true;
      };
    };
  };
}
