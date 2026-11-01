#!/usr/bin/env bash
# build-nixos.sh: build the probe's NixOS kernel, initrd, toplevel and root
# filesystem at the plan's nixpkgs pin (A1), inside the rime-nix container, and
# copy them out of the store to $ART/in for the firmware lab.
#   ART   artifacts dir on the host (default /var/lab/rime-nix/artifacts/ovmf)
#
# KERNEL=rime (kernsign): Rime's own kernel through the rime-os flake this rig sits
# in (system.nix rimeFlake), in two passes around the detached signing:
#   build-nixos.sh image    the unsigned image (rimeKernelImage) + its hash -> $ART/in
#   (lab.sh keys detach     sign-detached.sh with the throwaway key -> $ART/sig/<hash>.sig)
#   build-nixos.sh system   the system whose rime.kernel.signing reads $ART/sig and
#                           the throwaway certificate: kernel = the Nix-signed image
set -euo pipefail
PIN=${PIN:-1c89083e451e891152365b799595cf7790a43325}
ART=${ART:-/var/lab/rime-nix/artifacts/ovmf}
HERE=$(cd "$(dirname "$0")" && pwd)
FLAKE=$(cd "$HERE/../../.." && pwd)
# /var/lab/rime-nix is mounted at /work in the rime-nix container.
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
CART=$(to_work "$ART"); CHERE=$(to_work "$HERE"); CFLAKE=$(to_work "$FLAKE")
mkdir -p "$ART/nix" "$ART/in"

if [[ ${KERNEL:-nixpkgs} != rime ]]; then
  sudo -n podman exec -w "$CHERE" rime-nix nix-build "$CHERE/system.nix" \
    --arg nixpkgs "builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/$PIN.tar.gz\"" \
    -A kernel -A initrd -A toplevel -A rootfs -A entry \
    -o "$CART/nix/result" --option max-jobs auto --option cores 0 | tee "$ART/nix/outputs.txt"
  sudo -n podman exec rime-nix sh -euc "
    R=$CART/nix; I=$CART/in
    rm -f \$I/*
    cp -L \$R/result/bzImage \$I/bzImage
    cp -L \$R/result-2/initrd \$I/initrd
    cp -L --sparse=always \$R/result-4 \$I/rootfs.img
    cp -L \$R/result-5 \$I/entry.json
    cp -L \$R/result-3/boot.json \$I/bootspec.json
    chmod u+w \$I/*; chown $(id -u):$(id -g) \$I/*"
  ls -la "$ART/in"
  exit 0
fi

# KERNEL=rime. The flake is the checkout this script is in (path:, so uncommitted
# changes count, as with every other build on this branch).
flake_arg="builtins.getFlake \"path:$CFLAKE\""
# nix build -o X a b c links X, X-1, X-2 (nix-build above: X, X-2, X-3).
nb() {  # nb OUTLINK ATTR... [extra nix args]
  local out=$1; shift
  sudo -n podman exec -w "$CHERE" rime-nix nix build --impure -L -f "$CHERE/system.nix" \
    --arg rimeFlake "$flake_arg" "$@" \
    -o "$CART/nix/$out" --print-out-paths --option max-jobs auto --option cores 0
}
case ${1:-} in
  image)
    start=$SECONDS
    nb image unsignedImage signing | tee "$ART/nix/image-outputs.txt"
    sudo -n podman exec rime-nix sh -euc "
      R=$CART/nix; I=$CART/in
      rm -f \$I/*
      cp -L \$R/image/bzImage \$I/bzImage.unsigned
      cp -L \$R/image-1 \$I/signing-unsigned.json
      chmod u+w \$I/*; chown $(id -u):$(id -g) \$I/*"
    jq . "$ART/in/signing-unsigned.json"
    [[ $(jq -r .signed "$ART/in/signing-unsigned.json") == false ]] || { echo "first pass must be unsigned" >&2; exit 1; }
    echo "build-nixos.sh image: $(( SECONDS - start )) s"
    ;;
  system)
    start=$SECONDS
    hash=$(jq -r .imageHash "$ART/in/signing-unsigned.json")
    [[ -s $ART/sig/$hash.sig ]] || { echo "no $ART/sig/$hash.sig: run lab.sh detach first" >&2; exit 1; }
    nb result kernel initrd toplevel rootfs entry signing unsignedImage \
      --arg signatureDir "\"$CART/sig\"" --arg certificate "\"$CART/keys/test-mok.crt\"" \
      | tee "$ART/nix/outputs.txt"
    sudo -n podman exec rime-nix sh -euc "
      R=$CART/nix; I=$CART/in
      cp -L \$R/result/bzImage \$I/bzImage.signed
      cp -L \$R/result-1/initrd \$I/initrd
      cp -L --sparse=always \$R/result-3 \$I/rootfs.img
      cp -L \$R/result-4 \$I/entry.json
      cp -L \$R/result-2/boot.json \$I/bootspec.json
      cp -L \$R/result-5 \$I/signing.json
      chmod u+w \$I/*; chown $(id -u):$(id -g) \$I/*"
    # The second pass signs the SAME image the first pass handed to the signer.
    [[ $(jq -r .imageHash "$ART/in/signing.json") == "$hash" ]] || { echo "image hash moved between passes" >&2; exit 1; }
    [[ $(jq -r .signed "$ART/in/signing.json") == true ]] || { echo "rime.kernel.signing did not pick up the signature" >&2; exit 1; }
    sudo -n podman exec rime-nix sh -euc "
      a=\$(sha256sum < $CART/in/bzImage.unsigned); b=\$(sha256sum < $CART/nix/result-6/bzImage)
      [ \"\$a\" = \"\$b\" ] || { echo 'the unsigned image changed between passes' >&2; exit 1; }"
    jq . "$ART/in/signing.json" "$ART/in/entry.json"
    echo "build-nixos.sh system: $(( SECONDS - start )) s"
    ls -la "$ART/in"
    ;;
  *) echo "usage: KERNEL=rime build-nixos.sh image|system" >&2; exit 2;;
esac
