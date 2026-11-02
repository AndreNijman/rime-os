#!/usr/bin/env bash
# payload.sh TOPLEVEL BRIDGE: the payload disk the lab VMs get as their second virtio disk (README.md here).
# Runs INSIDE the rime-nix container (run.sh calls it under `nix shell` with the tools).
#
#   payload/store.tar.zst    every store path of TOPLEVEL's closure (closureInfo), as nix/store/... entries
#   payload/registration     the closure's nix-store --load-db input (narHash, narSize, references)
#   payload/manifest.json    toplevel, the closure's own nix, and the sha256 of both files above
#   rime-migrate             the bridge as it runs on bootc (share/rime-bridge/rime-migrate of BRIDGE)
#
# The bridge checks the two sha256s and, after --load-db, every path's narHash (nix-store --verify --check-contents).
# TODO(handover): production signs the manifest (D6); this lab payload is unsigned.
set -euo pipefail
TOP=$1 BRIDGE=$2
LAB=${LAB:-/work/scratch/bridge}
if [[ ${REFRESH_ONLY:-} == 1 ]]; then   # swap in a rebuilt bridge + guest-prep.sh; the closure part is kept
  for f in rime-migrate guest-prep.sh; do debugfs -w -R "rm $f" "$LAB/payload.img" >/dev/null 2>&1 || true; done
  install -m755 "$BRIDGE/share/rime-bridge/rime-migrate" "$LAB/.rime-migrate"
  debugfs -w -R "write $LAB/.rime-migrate rime-migrate" "$LAB/payload.img"
  debugfs -w -R "write /work/wt-bridge/nix/probe/ovmf/bridge/guest-prep.sh guest-prep.sh" "$LAB/payload.img"
  for f in rime-migrate guest-prep.sh; do debugfs -w -R "sif $f mode 0100755" "$LAB/payload.img"; done
  rm -f "$LAB/.rime-migrate"; debugfs -R "ls -l /" "$LAB/payload.img"; exit 0
fi
P=$LAB/payload-tree
CI=$(nix build --no-link --print-out-paths --impure --expr \
  "(builtins.getFlake \"github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325\").legacyPackages.x86_64-linux.closureInfo { rootPaths = [ (builtins.storePath \"$TOP\") ]; }")
NIX=$(dirname "$(dirname "$(readlink -f "$TOP/sw/bin/nix-store")")")
rm -rf "$P"; mkdir -p "$P/payload"
echo ">> closure: $(wc -l < "$CI/store-paths") paths, nix $NIX"
sed 's|^/||' "$CI/store-paths" | tar -C / --owner=0 --group=0 --numeric-owner -cf - -T - | zstd -T0 -3 -q -o "$P/payload/store.tar.zst"
cp "$CI/registration" "$P/payload/registration"
python3 - "$P/payload" "$TOP" "$NIX" <<'PY'
import hashlib, json, os, sys
d, top, nix = sys.argv[1:]
def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()
m = {"toplevel": top, "nix": nix,
     "store": {"file": "store.tar.zst", "sha256": sha(os.path.join(d, "store.tar.zst"))},
     "registration": {"file": "registration", "sha256": sha(os.path.join(d, "registration"))}}
json.dump(m, open(os.path.join(d, "manifest.json"), "w"), indent=1)
print(json.dumps(m, indent=1))
PY
install -m755 "$BRIDGE/share/rime-bridge/rime-migrate" "$P/rime-migrate"
cp /work/wt-bridge/nix/probe/ovmf/bridge/guest-prep.sh "$P/"
SZ=$(( $(du -sm "$P" | cut -f1) + 512 ))
rm -f "$LAB/payload.img"; truncate -s "${SZ}M" "$LAB/payload.img"
mkfs.ext4 -q -L rime-payload -d "$P" "$LAB/payload.img"
rm -rf "$P"
ls -la "$LAB/payload.img"
