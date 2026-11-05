#!/usr/bin/env bash
# payload.sh TOPLEVEL BRIDGE SECRET: the payload disk the lab VMs get as their third virtio disk (README.md here).
# Runs INSIDE the rime-nix container (run.sh calls it under `nix shell` with the tools).
#
# The payload/ files are the product's payload-build.sh (share/rime-bridge of BRIDGE), the release pipeline's builder:
#   payload/store.tar.zst      every store path of TOPLEVEL's closure (closureInfo), as nix/store/... entries
#   payload/registration       the closure's nix-store --load-db input (narHash, narSize, references)
#   payload/preview.tar        what `check --explain --payload` runs BEFORE start: the payload's own rime-hostlayer,
#                              the system's etc/ (contents, dereferenced) and sw/bin names, greetd's shipped config
#   payload/manifest.json      toplevel, the closure's own nix, and the sha256 of the three files above
#   payload/manifest.json.sig  signed by SECRET (the lab's TEST key) with the product's payload-sign.sh
#   rime-migrate               the bridge as it runs on bootc (share/rime-bridge/rime-migrate of BRIDGE, the lab build
#                              that trusts the TEST key: lab-bridge.nix)
#   guest-prep.sh, verb-probe.sh   the lab's guest helpers (README.md here)
#   finish-custom.cfg   what phase finish adds to custom.cfg before start: the rime-secureboot-key block and a
#                       Windows-like entry, both of which `rime migrate finish` must keep
#
# The bridge checks the signature, the three sha256s and, after --load-db, every path's narHash
# (nix-store --verify --check-contents).
set -euo pipefail
TOP=$1 BRIDGE=$2 SECRET=$3
LAB=${LAB:-/work/scratch/bridge2}
RIG=$(cd "$(dirname "$0")" && pwd)
if [[ ${REFRESH_ONLY:-} == 1 ]]; then   # swap in a rebuilt bridge + guest-prep.sh; the payload part is kept
  for f in rime-migrate guest-prep.sh verb-probe.sh; do debugfs -w -R "rm $f" "$LAB/payload.img" >/dev/null 2>&1 || true; done
  install -m755 "$BRIDGE/share/rime-bridge/rime-migrate" "$LAB/.rime-migrate"
  debugfs -w -R "write $LAB/.rime-migrate rime-migrate" "$LAB/payload.img"
  debugfs -w -R "write $RIG/guest-prep.sh guest-prep.sh" "$LAB/payload.img"
  debugfs -w -R "write $RIG/verb-probe.sh verb-probe.sh" "$LAB/payload.img"
  for f in rime-migrate guest-prep.sh verb-probe.sh; do debugfs -w -R "sif $f mode 0100755" "$LAB/payload.img"; done
  rm -f "$LAB/.rime-migrate"; debugfs -R "ls -l /" "$LAB/payload.img"; exit 0
fi
P=$LAB/payload-tree
CI=$(nix build --no-link --print-out-paths --impure --expr \
  "(builtins.getFlake \"github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325\").legacyPackages.x86_64-linux.closureInfo { rootPaths = [ (builtins.storePath \"$TOP\") ]; }")
rm -rf "$P"; mkdir -p "$P"
bash "$BRIDGE/share/rime-bridge/payload-build.sh" "$TOP" "$CI" "$P/payload"
bash "$BRIDGE/share/rime-bridge/payload-sign.sh" "$SECRET" "$P/payload"
cat "$P/payload/manifest.json.sig"
install -m755 "$BRIDGE/share/rime-bridge/rime-migrate" "$P/rime-migrate"
cp "$RIG/guest-prep.sh" "$RIG/verb-probe.sh" "$RIG/finish-custom.cfg" "$P/"
SZ=$(( $(du -sm "$P" | cut -f1) + 512 ))
rm -f "$LAB/payload.img"; truncate -s "${SZ}M" "$LAB/payload.img"
mkfs.ext4 -q -L rime-payload -d "$P" "$LAB/payload.img"
rm -rf "$P"
ls -la "$LAB/payload.img"
