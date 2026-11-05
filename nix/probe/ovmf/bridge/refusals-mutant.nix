# The mutant for bridge-refusals' "check writes nothing" assertion (nix/tests/bridge.nix): the same VM test with a
# rime-migrate whose btrfs mounts are read-write. This build MUST FAIL (a disk's sha256 changes); if it passes, the
# sha256 comparison proves nothing. Not a flake check, because its expected result is a failed build:
#   nix build --impure -L --expr '(import ./nix/probe/ovmf/bridge/refusals-mutant.nix { flake = builtins.getFlake "path:/work/wt-bridge"; })'
{ flake }:

let
  pkgs = import flake.inputs.nixpkgs {
    system = "x86_64-linux";
    overlays = [ flake.overlays.default ];
    config.allowUnfree = true;
  };
  mutant = pkgs.rime.rime-bridge.overrideAttrs (old: {
    pname = "rime-bridge-mutant-rw";
    doInstallCheck = false;
    postPatch = (old.postPatch or "") + ''
      substituteInPlace rime-bridge.py --replace-fail '"btrfs": "ro,rescue=nologreplay"' '"btrfs": "rw"'
    '';
  });
  pkgs' = pkgs // { rime = pkgs.rime // { rime-bridge = mutant; }; };
in
(import ../../../tests/bridge.nix { pkgs = pkgs'; self = flake; }).bridge-refusals
