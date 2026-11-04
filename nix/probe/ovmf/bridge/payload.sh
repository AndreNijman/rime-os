#!/usr/bin/env bash
# payload.sh TOPLEVEL BRIDGE SECRET: the payload disk the lab VMs get as their third virtio disk (README.md here).
# Runs INSIDE the rime-nix container (run.sh calls it under `nix shell` with the tools).
#
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
NIX=$(dirname "$(dirname "$(readlink -f "$TOP/sw/bin/nix-store")")")
HL=$(dirname "$(dirname "$(readlink -f "$TOP/sw/bin/rime-hostlayer")")")
rm -rf "$P"; mkdir -p "$P/payload"
echo ">> closure: $(wc -l < "$CI/store-paths") paths, nix $NIX, hostlayer $HL"
sed 's|^/||' "$CI/store-paths" | tar -C / --owner=0 --group=0 --numeric-owner -cf - -T - | zstd -T0 -3 -q -o "$P/payload/store.tar.zst"
cp "$CI/registration" "$P/payload/registration"

V=$P/preview
mkdir -p "$V/system/sw/bin"
cp "$HL/bin/rime-hostlayer" "$V/rime-hostlayer"
cp "$HL/share/rime-hostlayer/greetd-config.toml" "$V/greetd-config.toml"
# /etc as the new system will have it (every file's contents: carry compares settings), dereferenced; a link to a file
# only the running system makes (etc/cups, etc/fonts/2.11/fonts.conf) is skipped: tar says so and exits 1, not 2.
# sw/bin: the command names.
{ tar -C "$TOP" -h --owner=0 --group=0 --numeric-owner -cf - etc || [[ $? -eq 1 ]]; } | tar -C "$V/system" -xf -
for b in "$TOP"/sw/bin/*; do : > "$V/system/sw/bin/$(basename "$b")"; done
tar -C "$V" --owner=0 --group=0 --numeric-owner --sort=name -cf "$P/payload/preview.tar" rime-hostlayer greetd-config.toml system
rm -rf "$V"

python3 - "$P/payload" "$TOP" "$NIX" <<'PY'
import hashlib, json, os, sys
d, top, nix = sys.argv[1:]
def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()
m = {"toplevel": top, "nix": nix}
for k, f in (("store", "store.tar.zst"), ("registration", "registration"), ("preview", "preview.tar")):
    m[k] = {"file": f, "sha256": sha(os.path.join(d, f))}
json.dump(m, open(os.path.join(d, "manifest.json"), "w"), indent=1)
print(json.dumps(m, indent=1))
PY
bash "$BRIDGE/share/rime-bridge/payload-sign.sh" "$SECRET" "$P/payload"
cat "$P/payload/manifest.json.sig"
install -m755 "$BRIDGE/share/rime-bridge/rime-migrate" "$P/rime-migrate"
cp "$RIG/guest-prep.sh" "$RIG/verb-probe.sh" "$RIG/finish-custom.cfg" "$P/"
SZ=$(( $(du -sm "$P" | cut -f1) + 512 ))
rm -f "$LAB/payload.img"; truncate -s "${SZ}M" "$LAB/payload.img"
mkfs.ext4 -q -L rime-payload -d "$P" "$LAB/payload.img"
rm -rf "$P"
ls -la "$LAB/payload.img"
