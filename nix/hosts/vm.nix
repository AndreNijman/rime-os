# rime-vm: a QEMU virtual machine for tests and local runs (nixos-rebuild build-vm style).
# The rime/rime account exists ONLY here, for VM tests. Never import this file on real hardware.
{ lib, modulesPath, ... }:

{
  imports = [
    (modulesPath + "/virtualisation/qemu-vm.nix")
    ./profile.nix
  ];

  nixpkgs.hostPlatform = "x86_64-linux";

  # No networking.hostName: the host layer names the machine from host.toml at activation ("rime" when
  # nothing names it), the same as on real hardware.

  # Every Rime area and the boot hook come from the shipped profile (./profile.nix). rime.kernel stays off: the VM
  # boots nixpkgs' cached kernel. qemu-vm boots the kernel directly, so the boot hook only runs if a test calls
  # switch-to-configuration boot; no test of rime-vm needs NixOS's own GRUB installer (it is off).

  virtualisation = {
    memorySize = 4096;
    cores = 4;
    diskSize = 16384;
  };

  users.users.rime = {
    isNormalUser = true;
    initialPassword = "rime";
    extraGroups = [ "wheel" "networkmanager" "video" "audio" ];
  };
}
