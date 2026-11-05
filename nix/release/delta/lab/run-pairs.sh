#!/usr/bin/env bash
# Runs inside the rime-nix container: the three delta pairs of docs/nixos/delta.md, one after the other.
set -euo pipefail
S=/work/scratch/g2
T=/work/wt-g2/nix/release/delta/measure-pair.sh
NP=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
cur=/nix/store/fjxcryan7pdbhj0gp4bfm9jj782xx5nl-nixos-system-rime-vm-26.11.20261030.1c89083
old_rime=$(cat $S/toplevel-rimeold.txt)
t3=$(cat $S/toplevel-3ec6363.txt)
t5=$(cat $S/toplevel-55bfb4d.txt)
jobs=${JOBS:-6}

run() { # run NAME OLD_STORE OLD_TOP NEW_STORE NEW_TOP
  local name=$1 out=$S/pairs/$1
  mkdir -p "$out"
  echo "##### $name $(date -Is)"
  # what changed, by package (the old side may be in a chroot store: copy nothing, read both via diff-closures)
  if [ -z "$2" ] && [ -z "$4" ]; then
    nix store diff-closures "$3" "$5" > "$out/diff-closures.txt" 2>&1 || true
  fi
  nix shell $NP#python3 $NP#zstd -c bash $T "$out" "$2" "$3" "$4" "$5" "$jobs"
  echo "##### $name done $(date -Is)"
}

for p in ${PAIRS:-g1-rime g1-nixpkgs g2-mass}; do
  case $p in
    g1-rime) run g1-rime "" "$old_rime" "" "$cur" ;;
    g1-nixpkgs) run g1-nixpkgs "$S/store-3ec6363" "$t3" "" "$cur" ;;
    g2-mass) run g2-mass "$S/store-55bfb4d" "$t5" "$S/store-3ec6363" "$t3" ;;
  esac
done
