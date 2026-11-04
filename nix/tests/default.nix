# Rime's NixOS VM tests, exposed as checks.x86_64-linux.<name>. One file per test.
{ pkgs, self }:

{
  vm-boot = import ./vm-boot.nix { inherit pkgs self; };
  rimed = import ./rimed.nix { inherit pkgs self; };
  libexec = import ./libexec.nix { inherit pkgs self; };
  desktop = import ./desktop.nix { inherit pkgs self; };
  desktop-plymouth-stuck = import ./desktop-plymouth-stuck.nix { inherit pkgs self; };
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
  release-plan = import ./release-plan.nix { inherit pkgs self; };
  release-licences = import ./release-licences.nix { inherit pkgs self; };
  update-delta = import ./update-delta.nix { inherit pkgs self; };
  update-floors = import ./update-floors.nix { inherit pkgs self; };
  update-variants = import ./update-variants.nix { inherit pkgs self; };
  branding = import ./branding.nix { inherit pkgs self; };
  greeting = import ./greeting.nix { inherit pkgs self; };
  greeting-vm = import ./greeting-vm.nix { inherit pkgs self; };
  terminal-migrate = import ./terminal-migrate.nix { inherit pkgs self; };
  terminal-look = import ./terminal-look.nix { inherit pkgs self; };
  delta = import ./delta.nix { inherit pkgs self; };
  unit-docs = import ./unit-docs.nix { inherit pkgs self; };
  rimed-session = import ./rimed-session.nix { inherit pkgs self; };
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
# boot-loader-scan, boot-microcode, boot-esp-budget, luks-boot, kernel-vm-signed
// import ./bootmatrix.nix { inherit pkgs self; }
# keymap-luks
// import ./keymap.nix { inherit pkgs self; }
# hostgates-net, hostgates-disks, hostgates-wifi-eval
// import ./hostgates.nix { inherit pkgs self; }
# niri-default, libexec-pycache, hostname-default
// import ./hygiene.nix { inherit pkgs self; }
# bridge-unit, bridge-refusals, rootfs-unit
// import ./bridge.nix { inherit pkgs self; }
# pkg-translate (+ .katana)
// import ./pkg-translate.nix { inherit pkgs self; }
# equibop
// { equibop = import ./equibop.nix { inherit pkgs self; }; }
# vendor-apps (+ .mutants: inrelease-tampered, rpm-tampered)
// { vendor-apps = import ./vendor-apps.nix { inherit pkgs self; }; }
# claude-code-npm (+ .mutants.bad-integrity)
// { claude-code-npm = import ./claude-code-npm.nix { inherit pkgs self; }; }
# virtualization
// { virtualization = import ./virtualization.nix { inherit pkgs self; }; }
# vendor-verify, vendor-fetch-refuses
// import ./vendor-verify.nix { inherit pkgs self; }
# gaming-launch (P2)
// { gaming-launch = import ./gaming-launch.nix { inherit pkgs self; }; }
# vm-harness, vm-harness-vm
// import ./vm-harness.nix { inherit pkgs self; }
# luks-tpm-rebind (rime-luks-tpm-status, rime luks, rime doctor)
// import ./luks-tpm.nix { inherit pkgs self; }
# desktops-eval (docs/nixos/desktops.md §8.1)
// import ./desktops.nix { inherit pkgs self; }
# desktops-install-{gnome,kde,cosmic} (+ .mutants; docs/nixos/desktops.md §8.1)
// import ./desktops-install.nix { inherit pkgs self; }
# settings-quicksettings-{gnome,kde,cosmic} (docs/nixos/desktops.md §8.2; + .mutants)
// import ./settings-quicksettings.nix { inherit pkgs self; }
// import ./settings-integrations-eval.nix { inherit pkgs self; }
# settings-app-{gnome,kde,cosmic} (VM; docs/nixos/desktops.md §8.2; + .mutants)
// import ./settings-app.nix { inherit pkgs self; }
# gnome-unlock (+ .mutants): rime-gnome-unlock's own tests in the sandbox (docs/nixos/desktops.md §12.4)
// import ./gnome-unlock.nix { inherit pkgs self; }
# desktop-lock-{gnome,kde,cosmic} (VM; docs/nixos/desktops.md §12.4, §12.5; + gnome .mutants)
// import ./desktop-lock.nix { inherit pkgs self; }
# avahi-resolvconf (+ .mutants: no-ready-gate, unprivileged-cleanup; nix/modules/avahi-resolvconf.nix)
// { avahi-resolvconf = import ./avahi-resolvconf.nix { inherit pkgs self; }; }
# gnome-keyring-client-race (+ .mutants.unpatched; nix/modules/gnome-keyring-client-race.nix)
// { gnome-keyring-client-race = import ./gnome-keyring-client-race.nix { inherit pkgs self; }; }
# shell-kept (the Shell's kept panel windows on Hyprland; phase "short"; nix/tests/shell-kept.nix)
// { shell-kept = import ./shell-kept.nix { inherit pkgs self; }; }
