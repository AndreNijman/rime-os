# The shipped Rime profile: what every published toplevel turns on, whatever it runs on. rime-vm (nix/hosts/vm.nix)
# imports it today; D9's generic rime-plain and rime-luks toplevels build on it, and the OVMF boot matrix
# (nix/probe/ovmf/boothook/matrix.nix) boots it through Fedora's shim and GRUB. Hardware and VM facts stay in the
# host file, per-machine facts in host.toml (D2).
{ ... }:

{
  rime = {
    desktop.enable = true;
    services.enable = true;
    gaming.enable = true;
    security.enable = true;
    catalogue.enable = true;
    # D3 v1 chain: menu entries for Fedora's shim + GRUB, written by nix/pkgs/rime-boot. NixOS's own GRUB and
    # systemd-boot installers are off (they install loader binaries).
    boot.enable = true;
  };
}
