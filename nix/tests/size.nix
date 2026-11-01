# checks.x86_64-linux.size: the closure guard for the P1 size cuts (docs/nixos/closure.md).
#
# Reads the rime-vm toplevel's closure (closureInfo) and fails when a cut comes back:
#   - mbrola-voices (645 MiB, speech-dispatcher's espeak-ng; bootc ships espeak-ng without voices);
#   - a second JetBrainsMono Nerd Font (nixpkgs' 3.5.0 next to Rime's 3.5.1 pin);
#   - webkitgtk 4.1 (NetworkManager-openconnect's GNOME auth dialog; bootc has no -gnome subpackage);
#   - the nixpkgs source tree (/etc/nix/registry.json pointing at a store path instead of a locked GitHub rev);
#   - the big -doc outputs and the NixOS manual (documentation.doc / documentation.nixos);
# and when the closure's NAR total passes the budget below. The toplevel is the same one `nix build
# path:.#nixosConfigurations.rime-vm.config.system.build.toplevel` builds, so the check costs one closureInfo.
#
# Extra attributes, not built by `nix flake check`:
#   measure.<variant>   the toplevels docs/nixos/closure.md measures (nix/tests/size-measure.sh builds them)
#   mutants.<name>      the guard run on a toplevel with one cut undone; each must FAIL
{ pkgs, self }:

let
  lib = pkgs.lib;

  vm = self.nixosConfigurations.rime-vm;

  variant = modules: (vm.extendModules { inherit modules; }).config.system.build.toplevel;

  # A laptop-shaped install (no qemu-vm.nix: its mkVMOverride sets videoDrivers = [ "modesetting" ], which drops the
  # NVIDIA userspace from any rime-vm variant). Every area on as rime-vm has them, Rime's kernel with NVIDIA 615,
  # no test account. Stands in for D9's generic rime-plain toplevel until that exists; never deployed.
  laptop =
    gaming: extra:
    (self.inputs.nixpkgs.lib.nixosSystem {
      modules = [
        self.nixosModules.default
        {
          nixpkgs.hostPlatform = "x86_64-linux";
          networking.hostName = "rime";
          rime = {
            desktop.enable = true;
            services.enable = true;
            security.enable = true;
            catalogue.enable = true;
            kernel.enable = true;
            gaming.enable = gaming;
          };
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/disk/by-label/rime";
            fsType = "btrfs";
          };
        }
      ]
      ++ extra;
    }).config.system.build.toplevel;

  # rime-vm with nothing changed, as `nixosConfigurations.rime-vm` builds it (every area on, gaming included).
  rimeVm = vm.config.system.build.toplevel;

  # The NAR total the guard allows, in bytes. Measured after the cuts: 9,530,593,800 bytes = 8.88 GiB (closure.md
  # table 1). The budget leaves ~640 MiB for the P2 ports still landing; raising it is a deliberate edit with a
  # closure.md entry saying what grew and why.
  budget = 10200547328; # 9.5 GiB

  # The nixpkgs source this flake evaluates. In the closure only through a path-type registry entry.
  nixpkgsSource = builtins.unsafeDiscardStringContext (toString pkgs.path);

  guard =
    name: toplevel:
    let
      info = pkgs.closureInfo { rootPaths = [ toplevel ]; };
    in
    pkgs.runCommand "rime-size-${name}" { } ''
      set -f # the doc patterns below are regexes, not globs
      paths=${info}/store-paths
      total=$(cat ${info}/total-nar-size)
      fail=0
      no() { echo "FAIL: $*"; fail=1; }
      count() { { grep -E -c -- "$1" "$paths" || true; } | tr -d ' '; }

      echo "closure: $(wc -l < "$paths") paths, $total bytes NAR (budget ${toString budget})"

      [ "$(count '^/nix/store/[a-z0-9]{32}-mbrola-voices-')" = 0 ] ||
        no "mbrola-voices is in the closure (speechd's espeak-ng must be built with mbrolaSupport = false)"
      fonts=$(count '^/nix/store/[a-z0-9]{32}-nerd-fonts-jetbrains-mono-')
      [ "$fonts" = 1 ] || no "$fonts JetBrainsMono Nerd Font paths in the closure, expected exactly 1 (the 3.5.1 pin)"
      [ "$(count '^/nix/store/[a-z0-9]{32}-webkitgtk-[^/]*abi=4\.1$')" = 0 ] ||
        no "webkitgtk 4.1 is in the closure (networkmanager-openconnect must be built withGnome = false)"
      # The source itself, or a module's re-import of it (a `pkgs.path` copy is named <hash>-<its basename>).
      if grep -E -e '^${nixpkgsSource}$' -e '-${baseNameOf nixpkgsSource}$' "$paths" >/dev/null; then
        no "the nixpkgs source (${baseNameOf nixpkgsSource}) is in the closure"
      fi
      if grep -F '/nix/store/' ${toplevel}/etc/nix/registry.json >/dev/null; then
        no "/etc/nix/registry.json names a store path: $(cat ${toplevel}/etc/nix/registry.json)"
      fi
      for d in python3[.0-9]*-[.0-9]*-doc pipewire-[.0-9]*-doc wireplumber-[.0-9]*-doc nixos-manual-html; do
        [ "$(count "^/nix/store/[a-z0-9]{32}-$d\$")" = 0 ] || no "$d is in the closure (documentation.doc/nixos)"
      done
      [ "$total" -le ${toString budget} ] ||
        no "closure NAR size $total > budget ${toString budget} (docs/nixos/closure.md says how to raise it)"

      [ "$fail" = 0 ] || exit 1
      echo "size guard: all cuts hold"
      cp "$paths" $out
    '';
in
guard "rime-vm" rimeVm
// {
  measure = {
    # 1. rime-vm as nixosConfigurations.rime-vm has it: gaming on, nixpkgs' kernel, no NVIDIA.
    rime-vm = rimeVm;
    # 2. The same without the gaming stack (rime.gaming.enable's default).
    rime-vm-no-gaming = variant [ { rime.gaming.enable = lib.mkForce false; } ];
    # 3. Gaming on with 32-bit graphics forced off. Not a shipped configuration (programs.steam turns it on): it measures
    #    what hardware.graphics.enable32Bit alone costs. Steam's FHS rootfs keeps its own i686 glibc and libraries.
    rime-vm-gaming-no32 = variant [ { hardware.graphics.enable32Bit = lib.mkForce false; } ];
    # 4. The shape a fresh install ships (D9 rime-plain): the laptop host, Rime's kernel with NVIDIA 615, msi-ec,
    #    controllers, scx; no gaming. This is the P5 fresh-install figure and the A11 floor's input.
    plain = laptop false [ ];
    # 5. plain + `rime install steam` (rime.gaming.enable): the largest system a supported install switches to.
    plain-gaming = laptop true [ ];
    # 6. plain-gaming with 32-bit graphics forced off (not shipped): enable32Bit's own cost on the NVIDIA host.
    plain-gaming-no32 = laptop true [ { hardware.graphics.enable32Bit = lib.mkForce false; } ];
  };

  # Each must fail the guard (run: nix build path:.#checks.x86_64-linux.size.mutants.<name>).
  mutants = {
    # nixpkgs' 3.5.0 Nerd Font back in fonts.packages, as desktop.nix had it before the cut.
    second-nerd-font = guard "mutant-second-nerd-font" (variant [
      ({ pkgs, ... }: { fonts.packages = [ pkgs.nerd-fonts.jetbrains-mono ]; })
    ]);
    # The registry back on the nixpkgs store path, as nixpkgs.flake.setFlakeRegistry writes it.
    registry-store-path = guard "mutant-registry-store-path" (variant [
      ({ lib, pkgs, ... }: {
        nix.registry.nixpkgs.to = lib.mkForce {
          type = "path";
          path = pkgs.path;
        };
      })
    ]);
  };
}
