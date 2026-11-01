# Rime's NixOS VM tests, exposed as checks.x86_64-linux.<name>. One file per test.
{ pkgs, self }:

{
  vm-boot = import ./vm-boot.nix { inherit pkgs self; };
  rimed = import ./rimed.nix { inherit pkgs self; };
  libexec = import ./libexec.nix { inherit pkgs self; };
  desktop = import ./desktop.nix { inherit pkgs self; };
  shell = import ./shell.nix { inherit pkgs self; };
  gaming = import ./gaming.nix { inherit pkgs self; };
  security = import ./security.nix { inherit pkgs self; };
  release-unit = import ./release-unit.nix { inherit pkgs self; };
  release-standin = import ./release-standin.nix { inherit pkgs self; };
  release-github = import ./release-github.nix { inherit pkgs self; };
  update-client = import ./update-client.nix { inherit pkgs self; };
}
# kernel-config-fresh, kernel-gate, kernel-vm, kernel-boot-budget
// import ./kernel.nix { inherit pkgs self; }
