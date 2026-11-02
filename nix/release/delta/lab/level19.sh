#!/usr/bin/env bash
# delta.md §8 "zstd -19 without --long": what `nix copy ?compression=zstd&compression-level=19` would ship for the
# weekly pair's added paths (data/g1-nixpkgs), next to level 3 (shipped) and -19 --long=27 (results.tsv).
# Inside the rime-nix container:  level19.sh OUT.tsv [JOBS]
# One path is also written by `nix copy` itself at level 19, to show the CLI number is what Nix writes.
set -euo pipefail
out=$1 jobs=${2:-6}
here=$(cd "$(dirname "$0")/.." && pwd)
tail -n +2 "$here/data/g1-nixpkgs/results.tsv" | cut -f1,3,8,14 > "$out.in"
one() {
  IFS=$'\t' read -r p nar l27 l3 <<< "$1"
  s19=$(nix-store --dump "$p" | zstd -q -19 -T1 -c | wc -c)
  printf '%s\t%s\t%s\t%s\t%s\n' "$p" "$nar" "$l3" "$s19" "$l27"
}
export -f one
start=$(date +%s)
printf 'path\tnar_size\tzstd3_shipped\tzstd19\tzstd19_long27\n' > "$out"
tr '\n' '\0' < "$out.in" | xargs -0 -P "$jobs" -I{} bash -c 'one "$@"' _ {} >> "$out"
echo "measured $(( $(wc -l < "$out") - 1 )) paths in $(( $(date +%s) - start )) s"
awk -F'\t' 'NR>1 { n+=$2; a+=$3; b+=$4; c+=$5 } END {
  printf "NAR %.1f MB | zstd -3 (shipped) %.1f MB | zstd -19 %.1f MB (%.1f%%) | zstd -19 --long=27 %.1f MB (%.1f%%)\n",
    n/1e6, a/1e6, b/1e6, 100*(b-a)/a, c/1e6, 100*(c-a)/a }' "$out"
# Nix's own writer at level 19, for one leaf path with no references (a binary cache needs references present).
leaf=$(awk -F'\t' 'NR>1 { print $1 }' "$out" | while read -r p; do
  [ -z "$(nix-store -q --references "$p" | grep -v "^$p\$" || true)" ] && [ "$(nix path-info -S "$p" | cut -f2)" -gt 1000000 ] && { echo "$p"; break; }; done || true)
if [ -n "$leaf" ]; then
  d=$(mktemp -d /work/scratch/deltas/l19.XXXXXX)
  nix copy --to "file://$d?compression=zstd&compression-level=19" "$leaf"
  fs=$(grep '^FileSize:' "$d/$(basename "$leaf" | cut -c1-32).narinfo" | cut -d' ' -f2)
  cli=$(awk -F'\t' -v p="$leaf" '$1 == p { print $4 }' "$out")
  echo "nix copy level 19 of $leaf: FileSize $fs B; zstd -19 -T1: $cli B"
  rm -rf "$d"
fi
rm -f "$out.in"
