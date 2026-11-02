# Virtual machines (the catalogue's `virtualization` feature, D5): what `rime vm` and virt-manager need from the
# system, which bootc installed on demand (`rime install qemu-kvm libvirt edk2-ovmf swtpm virtiofsd`): libvirtd with
# QEMU/KVM, UEFI firmware (OVMF, Secure Boot capable), a software TPM and virtiofsd for shared folders.
#
# These need units, a daemon socket and setuid helpers, so they are a prebuilt system feature like gaming, not a
# class 2 app: `rime install qemu` selects it for the machine (the switch file the update client reads), and the
# release's system with `rime.virtualization.enable` carries it.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.virtualization;
in
{
  options.rime.virtualization.enable = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      QEMU/KVM, libvirt, OVMF, swtpm and virtiofsd. Off by default, like gaming (A4): it is several hundred MB of
      closure most machines never use, and `rime install qemu` (or libvirt, edk2-ovmf, swtpm, virtiofsd) selects it.
    '';
  };

  config = lib.mkIf cfg.enable {
    virtualisation.libvirtd = {
      enable = true;
      qemu = {
        package = pkgs.qemu_kvm;
        swtpm.enable = true;
        runAsRoot = false;
      };
    };
    environment.systemPackages = [ pkgs.virtiofsd pkgs.swtpm ];
    # libvirt finds virtiofsd for <filesystem type='mount'><driver type='virtiofs'/> through the qemu firmware/vhost
    # descriptors QEMU ships; the binary is also on PATH for rime vm's direct QEMU path.
  };
}
