# Rime's NixOS VM tests, exposed as checks.x86_64-linux.<name>. One file per test.
{ pkgs, self }:

{
  vm-boot = import ./vm-boot.nix { inherit pkgs self; };
}
# kernel-config-fresh, kernel-gate, kernel-vm
// import ./kernel.nix { inherit pkgs self; }
