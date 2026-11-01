#!/usr/bin/env bash
# build-nixos.sh: build the probe's NixOS kernel, initrd, toplevel and root
# filesystem at the plan's nixpkgs pin (A1), inside the rime-nix container, and
# copy them out of the store to $ART/in for the firmware lab.
#   ART   artifacts dir on the host (default /var/lab/rime-nix/artifacts/ovmf)
set -euo pipefail
PIN=${PIN:-1c89083e451e891152365b799595cf7790a43325}
ART=${ART:-/var/lab/rime-nix/artifacts/ovmf}
HERE=$(cd "$(dirname "$0")" && pwd)
# /var/lab/rime-nix is mounted at /work in the rime-nix container.
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
CART=$(to_work "$ART"); CHERE=$(to_work "$HERE")
mkdir -p "$ART/nix" "$ART/in"
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
