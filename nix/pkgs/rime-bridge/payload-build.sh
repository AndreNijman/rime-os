#!/usr/bin/env bash
# payload-build.sh TOPLEVEL CLOSURE_INFO OUTDIR: the bridge payload of one NixOS system (docs/nixos/bridge.md
# "Where a payload comes from"), unsigned. The release pipeline runs it on the release's plain toplevel; the OVMF lab
# (nix/probe/ovmf/bridge/payload.sh) on its own.
#
#   TOPLEVEL      the system (a /nix/store path), realised
#   CLOSURE_INFO  nixpkgs' closureInfo { rootPaths = [ TOPLEVEL ]; } (store-paths, registration)
#   OUTDIR        empty or absent; gets:
#     store.tar.zst   every store path of the closure, as nix/store/... entries (zstd -3)
#     registration    the closure's nix-store --load-db input (narHash, narSize, references)
#     preview.tar     what `check --explain --payload` runs BEFORE start: the payload's own rime-hostlayer, the
#                     system's etc/ (contents, dereferenced) and sw/bin names, greetd's shipped config
#     manifest.json   toplevel, the closure's own nix, and the sha256 of the three files above
# payload-sign.sh (or payload-release.sh) then writes manifest.json.sig.
# Needs: tar (GNU), zstd, python3, coreutils, sed, grep.
set -euo pipefail
die() { printf 'payload-build: %s\n' "$*" >&2; exit 1; }
[[ $# -eq 3 ]] || die "usage: payload-build.sh TOPLEVEL CLOSURE_INFO OUTDIR"
TOP=$1 CI=$2 OUT=$3
umask 022
[[ $TOP =~ ^/nix/store/[0-9a-z]{32}-[^/]+$ && -d $TOP ]] || die "$TOP is not a realised store path"
[[ -f $CI/store-paths && -f $CI/registration ]] || die "$CI is not a closureInfo (store-paths, registration)"
grep -qxF -- "$TOP" "$CI/store-paths" || die "$CI is not the closure of $TOP"
NIX=$(dirname "$(dirname "$(readlink -f "$TOP/sw/bin/nix-store")")")
HL=$(dirname "$(dirname "$(readlink -f "$TOP/sw/bin/rime-hostlayer")")")
for p in "$NIX" "$HL"; do
  grep -qxF -- "$p" "$CI/store-paths" || die "$p (from $TOP/sw/bin) is not in the closure"
done
mkdir -p "$OUT"
[[ -z $(ls -A "$OUT") ]] || die "$OUT is not empty"

echo ">> closure: $(wc -l < "$CI/store-paths") paths, nix $NIX, hostlayer $HL" >&2
sed 's|^/||' "$CI/store-paths" | tar -C / --owner=0 --group=0 --numeric-owner -cf - -T - | zstd -T0 -3 -q -o "$OUT/store.tar.zst"
cp "$CI/registration" "$OUT/registration"

V=$(mktemp -d)
# etc/ comes out of the store read-only (dr-xr-xr-x): only root could remove it as it is.
trap 'chmod -R u+w -- "$V"; rm -rf -- "$V"' EXIT
mkdir -p "$V/system/sw/bin"
cp "$HL/bin/rime-hostlayer" "$V/rime-hostlayer"
cp "$HL/share/rime-hostlayer/greetd-config.toml" "$V/greetd-config.toml"
# /etc as the new system will have it (every file's contents: carry compares settings), dereferenced; a link to a file
# only the running system makes (etc/cups, etc/fonts/2.11/fonts.conf) is skipped: tar says so and exits 1, not 2.
# sw/bin: the command names.
# A unit MASK is a link to /dev/null: dereferenced, it is the /dev/null character device, which an unprivileged tar
# cannot make (Cannot mknod, exit 2). Masks stay out of the copy and are made again as links to /dev/null, the form
# the bridge reads back (rime-bridge.py: preview extraction). The walk follows links as tar -h does, skips a dangling
# one (tar leaves it out, exit 1) and a directory that loops back into its own ancestry; names are NUL-separated.
python3 - "$TOP" > "$V/masks" <<'PY'
import os, stat, sys
top, out = os.fsencode(sys.argv[1]), sys.stdout.buffer
def walk(rel, ancestry):
    for name in sorted(os.listdir(os.path.join(top, rel))):
        r = rel + b"/" + name
        p = os.path.join(top, r)
        try:
            st = os.stat(p)
        except OSError:
            continue
        if stat.S_ISDIR(st.st_mode):
            real = os.path.realpath(p)
            if real not in ancestry:
                walk(r, ancestry | {real})
        elif stat.S_ISCHR(st.st_mode):
            if os.path.realpath(p) != b"/dev/null":
                sys.exit(f"payload-build: {os.fsdecode(r)}: a device in etc/ that is not a unit mask")
            out.write(r + b"\0")
walk(b"etc", {os.path.realpath(os.path.join(top, b"etc"))})
PY
# One exact --exclude per mask: GNU tar 1.35 honours one entry of a --null -X list (measured: 1 of 16 excluded).
mapfile -d '' -t masks < "$V/masks"
ex=()
for m in "${masks[@]}"; do ex+=("--exclude=$m"); done
{ tar -C "$TOP" -h --owner=0 --group=0 --numeric-owner --anchored --no-wildcards "${ex[@]}" -cf - etc || [[ $? -eq 1 ]]; } | tar -C "$V/system" -xf -
while IFS= read -r -d '' m; do
  d="$V/system/$(dirname -- "$m")"
  mkdir -p -- "$d"
  mode=$(stat -c %a -- "$d")   # the copy keeps the store's read-only directory modes
  chmod u+w -- "$d"
  ln -s /dev/null "$V/system/$m"
  chmod "$mode" -- "$d"
done < "$V/masks"
for b in "$TOP"/sw/bin/*; do : > "$V/system/sw/bin/$(basename "$b")"; done
# --mtime: the copies and the sw/bin names are made now; with the store's own time, two builds of one system match.
tar -C "$V" --owner=0 --group=0 --numeric-owner --sort=name --mtime=@1 -cf "$OUT/preview.tar" rime-hostlayer greetd-config.toml system

python3 - "$OUT" "$TOP" "$NIX" <<'PY'
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
