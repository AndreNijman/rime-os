# The OVMF lab's rime-migrate: the shipped program with ONE line changed, so it trusts the lab's TEST payload key
# instead of the bootc image's /usr/share/rime-os/trust/nixos-bridge-payload.pub (which the published image used by
# the lab does not carry yet). Everything else, the signature check included, is the product's code.
#   nix build --impure --expr '(import ./nix/probe/ovmf/bridge/lab-bridge.nix {
#     flake = builtins.getFlake "path:/work/wt-bridge2"; publicKey = "rime-bridge-lab-TEST-1:<base64>"; })'
{ flake, publicKey }:

let
  pkgs = import flake.inputs.nixpkgs {
    system = "x86_64-linux";
    overlays = [ flake.overlays.default ];
    config.allowUnfree = true;
  };
  key = builtins.replaceStrings [ "\n" ] [ "" ] publicKey;
in
assert builtins.match "rime-bridge-lab-TEST-[0-9]+:[A-Za-z0-9+/]{43}=" key != null;
pkgs.rime.rime-bridge.overrideAttrs (old: {
  pname = "rime-bridge-lab-TEST";
  # The product's suite asserts TRUSTED_PAYLOAD_KEYS is None; this build is the lab's on purpose.
  doInstallCheck = false;
  postPatch = (old.postPatch or "") + ''
    substituteInPlace rime-bridge.py --replace-fail 'TRUSTED_PAYLOAD_KEYS = None' 'TRUSTED_PAYLOAD_KEYS = ("${key}",)'
  '';
})
