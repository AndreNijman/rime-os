# Rime's NixOS VM tests, exposed as checks.x86_64-linux.<name>. One file per test.
{ pkgs, self }:

{
  vm-boot = import ./vm-boot.nix { inherit pkgs self; };
  rimed = import ./rimed.nix { inherit pkgs self; };
  libexec = import ./libexec.nix { inherit pkgs self; };
  desktop = import ./desktop.nix { inherit pkgs self; };
  shell = import ./shell.nix { inherit pkgs self; };
}
