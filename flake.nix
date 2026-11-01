{
  description = "Rime OS on NixOS: packages, NixOS modules, hosts and VM tests";

  inputs = {
    # Pinned nixos-unstable (decision A1). Moving the pin is a deliberate PR, never `nix flake update` by habit.
    nixpkgs.url = "github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325";

    # rime-shell is source only; its own flake is retired (PLAN.md D8). The release job bumps this input (A15).
    rime-shell = {
      url = "github:AndreNijman/rime-shell/feat/nixos";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rime-shell }:
    let
      system = "x86_64-linux";
      lib = nixpkgs.lib;

      pkgs = import nixpkgs {
        inherit system;
        overlays = [ self.overlays.default ];
        # Same as nix/modules/base.nix: Rime ships NVIDIA, Steam and other unfree packages.
        config.allowUnfree = true;
      };
    in
    {
      # Every Rime package lives under pkgs.rime.* (nix/pkgs/default.nix). Modules use pkgs.rime.<name>, so
      # nothing but this file needs to know about flake inputs.
      overlays.default = final: _prev: {
        rime = final.callPackage ./nix/pkgs { rime-shell-src = rime-shell; };
      };

      packages.${system} = {
        inherit (pkgs.rime) rimed rime-shell rime-libexec rime-kernel;
      };

      nixosModules = {
        default = import ./nix/modules { overlay = self.overlays.default; };
        base = ./nix/modules/base.nix;
        desktop = ./nix/modules/desktop.nix;
        services = ./nix/modules/services.nix;
        kernel = ./nix/modules/kernel.nix;
        gaming = ./nix/modules/gaming.nix;
        security = ./nix/modules/security.nix;
        packages-catalogue = ./nix/modules/packages-catalogue.nix;
      };

      nixosConfigurations.rime-vm = lib.nixosSystem {
        modules = [
          self.nixosModules.default
          ./nix/hosts/vm.nix
        ];
      };

      checks.${system} =
        import ./nix/tests { inherit pkgs self; }
        // {
          # Cheap: proves the rime-vm system evaluates without building or downloading it.
          eval = pkgs.runCommand "rime-vm-eval" { } ''
            echo ${builtins.unsafeDiscardStringContext self.nixosConfigurations.rime-vm.config.system.build.toplevel.drvPath} > $out
          '';
        };
    };
}
