# rime-vm: a QEMU virtual machine for tests and local runs (nixos-rebuild build-vm style).
# The rime/rime account exists ONLY here, for VM tests. Never import this file on real hardware.
{ lib, modulesPath, ... }:

{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];

  nixpkgs.hostPlatform = "x86_64-linux";

  networking.hostName = "rime-vm";

  rime = {
    desktop.enable = true;
    services.enable = true;
    gaming.enable = true;
    security.enable = true;
    catalogue.enable = true;
    # rime.kernel stays off: the VM boots nixpkgs' cached kernel.
  };

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
