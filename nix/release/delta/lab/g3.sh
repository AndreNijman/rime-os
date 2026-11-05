#!/usr/bin/env bash
# G3 (inside the rime-nix container): a machine on the old release (chroot store OLD_STORE) moves to the new release
# with deltas. Server = a file binary cache of the new release's whole closure (as the real cache holds it; a binary
# cache refuses a path whose references it lacks), written by `nix copy ?compression=zstd` exactly as the D6 pipeline
# writes it, signed with a throwaway TEST key.
#   g3.sh PAIR OLD_STORE NEW_STORE NEW_TOPLEVEL     (NEW_STORE "" = the default store)
set -euo pipefail
pair=$1 old_store=$2 new_store=$3 new_top=$4
S=/work/scratch/g2
P=$S/pairs/$pair
NP=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
srv=$S/server-$pair
pk=$(cat $S/keys/TEST.pk)
rm -rf "$S/g3-into-$pair"
cut -f1 "$P/pairs.tsv" | tail -n +2 > "$P/added.txt"
from=()
[ -n "$new_store" ] && from=(--from "$new_store")
if [ ! -e "$srv/nix-cache-info" ]; then
  start=$(date +%s)
  nix copy "${from[@]}" --to "file://$srv?compression=zstd&secret-key=$S/keys/TEST.sk" "$new_top" 2> "$P/server-copy.log"
  echo "server written in $(( $(date +%s) - start )) s"
fi
echo "server: $(ls "$srv" | grep -c narinfo) narinfos"
# What the pipeline ships for the ADDED paths (FileSize of their narinfos) = the download without deltas
s=0 n=0
for p in $(cat "$P/added.txt"); do
  h=${p#/nix/store/}
  while IFS=': ' read -r k v; do [ "$k" = FileSize ] && s=$((s + v)) && n=$((n + 1)); done < "$srv/${h:0:32}.narinfo"
done
echo "server FileSize total over $n added paths (nix copy ?compression=zstd): $s B"
if nix path-info --store "$old_store" "$new_top" >/dev/null 2>&1; then echo "new toplevel ALREADY valid in the machine store"; else echo "new toplevel absent from the machine store before"; fi
nix shell $NP#python3 $NP#zstd $NP#time -c env time -v \
  python3 -I /work/wt-g2/nix/release/delta/rime_delta.py apply-all "$P/results.tsv" --server "$srv" \
  --deltas "$P/deltas" --store "$old_store" --into "$S/g3-into-$pair" --trusted-key "$pk" --log "$P/apply-all.jsonl" \
  2> "$P/apply-all.time" | tee "$P/apply-all.json"
grep -E "Elapsed|Maximum resident|Exit status|Percent of CPU" "$P/apply-all.time"
nix path-info --store "$old_store" "$new_top" >/dev/null && echo "new toplevel valid in the machine store after"
set +e
nix store verify --store "$old_store" --trusted-public-keys "$pk" $(cat "$P/added.txt") 2>&1 | tail -3
echo "verify (contents + TEST signature) of the $(wc -l < "$P/added.txt") added paths: rc=${PIPESTATUS[0]}"
