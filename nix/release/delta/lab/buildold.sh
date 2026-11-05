#!/usr/bin/env bash
# usage: buildold.sh REV  -> realise the pair's old side in its own chroot store /work/scratch/g2/store-<rev7>
set -euo pipefail
r=$1; s=/work/scratch/g2/store-${r:0:7}
mkdir -p $s
cd /work/wt-g2
start=$(date +%s)
nix build --store "$s" --impure --max-jobs 4 --cores 6 --print-out-paths --no-link -L \
  --expr "import ./nix/release/delta/pair.nix { rimeFlake = \"path:/work/wt-g2\"; nixpkgsRev = \"$r\"; }" \
  > /work/scratch/g2/toplevel-${r:0:7}.txt
echo "built in $(( $(date +%s) - start )) s: $(cat /work/scratch/g2/toplevel-${r:0:7}.txt)"
