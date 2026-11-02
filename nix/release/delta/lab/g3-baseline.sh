# (nix store delete --ignore-liveness: a chroot store sees the SHARED store's runtime roots, other agents' VMs, by path name)
#!/usr/bin/env bash
# G3 baseline (inside the container): the same machine move WITHOUT deltas, i.e. what `nix copy` from the release
# cache costs today. Removes the added paths apply-all imported from the machine store first (a scratch chroot store,
# never the shared store), then copies the new toplevel from the same server cache.
#   g3-baseline.sh PAIR OLD_STORE NEW_TOPLEVEL
set -euo pipefail
pair=$1 old_store=$2 new_top=$3
S=/work/scratch/g2
P=$S/pairs/$pair
NP=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
srv=$S/server-$pair
pk=$(cat $S/keys/TEST.pk)
# A referrer must go before its references: repeat until none of the added paths is valid.
for _ in 1 2 3 4 5 6 7 8; do
  left=()
  for p in $(cat "$P/added.txt"); do nix path-info --store "$old_store" "$p" >/dev/null 2>&1 && left+=("$p"); done
  [ ${#left[@]} -eq 0 ] && break
  nix store delete --ignore-liveness --store "$old_store" "${left[@]}" 2>&1 | tail -1 || true
done
[ ${#left[@]} -eq 0 ] || { echo "still valid: ${left[*]}"; exit 1; }
echo "machine store back on the old release: none of the $(wc -l < "$P/added.txt") added paths is valid"
nix shell $NP#time -c env time -v nix copy --from "file://$srv" --to "$old_store" \
  --option trusted-public-keys "$pk" "$new_top" 2> "$P/nixcopy-baseline.time"
grep -E "Elapsed|Maximum resident|Exit status|Percent of CPU" "$P/nixcopy-baseline.time"
nix path-info --store "$old_store" "$new_top" >/dev/null && echo "new toplevel valid after the plain nix copy"
