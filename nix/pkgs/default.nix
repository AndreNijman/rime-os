# Rime's own packages. Reached as pkgs.rime.<name> through the flake's overlays.default.
# One file per package; a worker owns its file and never this set (add a line here only for a NEW package).
{ callPackage, pkgs, rime-shell-src }:

{
  rimed = callPackage ./rimed.nix { };
  rime-shell = callPackage ./rime-shell.nix { src = rime-shell-src; };
  rime-libexec = callPackage ./rime-libexec.nix { };
  # The layered-ridge identity: masters, exports, hicolor icons (files/branding/ridge).
  rime-branding = callPackage ./rime-branding.nix { };
  # The boot splash built from it: rime-os + 24 accent themes (files/branding/plymouth/ridge).
  rime-plymouth = callPackage ./rime-plymouth.nix { inherit (pkgs.rime) rime-branding; };
  # The terminal greeting's helpers: image protocol detection and the wallpaper-recoloured logo.
  rime-greeting = callPackage ./rime-greeting.nix { rime-branding = pkgs.rime.rime-branding; };
  # Ghostty's Rime theme and the one-time Alacritty -> Ghostty carry-over (files/desktop/ghostty).
  rime-terminal = callPackage ./rime-terminal.nix { inherit rime-shell-src; };
  # D5: rime-pkg's vendor-key check (ChatGPT, Claude Desktop) as a verifier and a verified-fetch builder.
  rime-vendor = callPackage ./rime-vendor.nix { };
  # D5 catalogue apps Rime builds itself: Equibop with nixpkgs' Equicord (no run-time download), and the vendor apps
  # (ChatGPT, Claude Desktop) from their vendor-key-checked, pinned downloads.
  rime-equibop = callPackage ./rime-equibop.nix { };
  rime-vendor-sources = callPackage ./rime-vendor-apps/sources.nix { inherit (pkgs.rime) rime-vendor; };
  rime-chatgpt = callPackage ./rime-vendor-apps/chatgpt.nix { sources = pkgs.rime.rime-vendor-sources; };
  rime-claude-desktop = callPackage ./rime-vendor-apps/claude-desktop.nix { sources = pkgs.rime.rime-vendor-sources; };
  # Not callPackage: that would wrap the kernel's own .override, which NixOS's boot.kernelPackages calls
  # (linuxPackagesFor -> kernel.override { features, randstructSeed, ... }).
  rime-kernel = import ./kernel { inherit pkgs; };
  # The config the kernel is built from is the committed nix/pkgs/kernel/config; this regenerates it.
  rime-kernel-config = callPackage ./kernel/config.nix { };
  # Rime's shipped sched-ext schedulers (bootc: Containerfile.core scx-builder).
  rime-scx = callPackage ./kernel/scx.nix { };
  rime-boot = callPackage ./rime-boot { };
  rime-hostlayer = callPackage ./rime-hostlayer { };
  rime-kbd-xkb = callPackage ./rime-kbd-xkb.nix { };
  rime-bridge = callPackage ./rime-bridge { };
  # The release pipeline's payload tools (build, sign, check) with their tools on PATH.
  rime-bridge-payload = callPackage ./rime-bridge/payload.nix { inherit (pkgs.rime) rime-bridge; };
  # GNOME Shell's screen lock under rime-greet: the org.gnome.DisplayManager password check (docs/nixos/desktops.md §12.4).
  rime-gnome-unlock = callPackage ./rime-gnome-unlock { };
  # Rime in each desktop's own quick settings (docs/nixos/desktops.md §6): nix/integrations.
  rime-gnome-extension = callPackage ../integrations/gnome { };
  rime-plasma-applet = callPackage ../integrations/kde { };
  rime-cosmic-applet = callPackage ../integrations/cosmic { };
}
