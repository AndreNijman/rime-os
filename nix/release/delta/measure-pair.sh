#!/usr/bin/env bash
# measure-pair.sh OUTDIR OLD_STORE OLD_TOPLEVEL NEW_STORE NEW_TOPLEVEL [JOBS]
#
# One release pair of docs/nixos/delta.md: both closures, the pairing, every delta (kept in OUTDIR/deltas for the
# client benchmark), the summary. A store is "" for the default store or a chroot store directory (`nix --store DIR`).
# Needs nix, python3 and zstd on PATH (the container: `nix shell <nixpkgs>#python3 <nixpkgs>#zstd -c ...`).
set -euo pipefail
out=$1 old_store=$2 old_top=$3 new_store=$4 new_top=$5 jobs=${6:-8}
tool=(python3 -I "$(dirname "$(readlink -f "$0")")/rime_delta.py")
mkdir -p "$out"
"${tool[@]}" closure "$old_store" "$old_top" "$out/closure-old.tsv"
"${tool[@]}" closure "$new_store" "$new_top" "$out/closure-new.tsv"
"${tool[@]}" pair "$out/closure-old.tsv" "$out/closure-new.tsv" "$out/pairs.tsv" | tee "$out/pair.txt"
start=$(date +%s)
"${tool[@]}" measure "$out/pairs.tsv" "$out/results.tsv" --old-store "$old_store" --new-store "$new_store" \
  -j "$jobs" --keep "$out/deltas" --tmp "$out"
echo "measure wall: $(( $(date +%s) - start )) s with $jobs jobs" | tee "$out/measure-time.txt"
"${tool[@]}" summary "$out/results.tsv" --top 25 | tee "$out/summary.txt"
