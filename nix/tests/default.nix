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
  size = import ./size.nix { inherit pkgs self; };
  fedora-parity = import ./fedora-parity.nix { inherit pkgs self; };
  hostlayer = import ./hostlayer.nix { inherit pkgs self; };
  hostlayer-katana = import ./hostlayer-katana.nix { inherit pkgs self; };
  activation = import ./activation.nix { inherit pkgs self; };
  polish = import ./polish.nix { inherit pkgs self; };
  release-unit = import ./release-unit.nix { inherit pkgs self; };
  release-standin = import ./release-standin.nix { inherit pkgs self; };
  release-github = import ./release-github.nix { inherit pkgs self; };
  update-client = import ./update-client.nix { inherit pkgs self; };
  update-e2e = import ./update-e2e.nix { inherit pkgs self; };
  branding = import ./branding.nix { inherit pkgs self; };
  greeting = import ./greeting.nix { inherit pkgs self; };
  greeting-vm = import ./greeting-vm.nix { inherit pkgs self; };
  terminal-migrate = import ./terminal-migrate.nix { inherit pkgs self; };
  terminal-look = import ./terminal-look.nix { inherit pkgs self; };
}
# kernel-config-fresh, kernel-gate, kernel-vm, kernel-boot-budget
// import ./kernel.nix { inherit pkgs self; }
# ci, ci-userns
// import ./ci.nix { inherit pkgs self; }
# kernel-sign, kernel-sign-workflow
// import ./kernel-sign.nix { inherit pkgs self; }
# boot-hook-unit, boot-hook-vm
// import ./boot.nix { inherit pkgs self; }
# catalogue, catalogue-data
// import ./catalogue.nix { inherit pkgs self; }
# branding-surfaces-static, branding-surfaces (the ridge on os-release, the splash and the greeter)
// import ./branding-surfaces.nix { inherit pkgs self; }
# installer-nixos, installer-target-eval (the NixOS install path of installer/rime-install)
// import ./installer.nix { inherit pkgs self; }
# boot-loader-scan, boot-microcode, luks-boot, kernel-vm-signed
// import ./bootmatrix.nix { inherit pkgs self; }
# keymap-luks
// import ./keymap.nix { inherit pkgs self; }
# hostgates-net, hostgates-disks, hostgates-wifi-eval
// import ./hostgates.nix { inherit pkgs self; }
