#!/usr/bin/env bash
# inside the container: the G3 runs after the measurements (quieter machine)
set -uo pipefail
S=/work/scratch/g2
cur=/nix/store/fjxcryan7pdbhj0gp4bfm9jj782xx5nl-nixos-system-rime-vm-26.11.20261030.1c89083
t3=$(cat $S/toplevel-3ec6363.txt)
clean() { # clean PAIR STORE: the machine store back on the old release
  for _ in 1 2 3 4; do
    l=()
    for p in $(cat $S/pairs/$1/added.txt); do nix path-info --store $2 $p >/dev/null 2>&1 && l+=($p); done
    [ ${#l[@]} -eq 0 ] && break
    nix store delete --ignore-liveness --store $2 "${l[@]}" 2>&1 | tail -1
  done
}
echo "===== g2-mass deltas $(date -Is) $(cat /proc/loadavg)"
bash $S/g3.sh g2-mass $S/store-55bfb4d $S/store-3ec6363 $t3
echo "===== g2-mass plain nix copy $(date -Is) $(cat /proc/loadavg)"
bash $S/g3-baseline.sh g2-mass $S/store-55bfb4d $t3
echo "===== g1-nixpkgs deltas, run 3 $(date -Is) $(cat /proc/loadavg)"
clean g1-nixpkgs $S/store-3ec6363
bash $S/g3.sh g1-nixpkgs $S/store-3ec6363 "" $cur
echo "===== g1-nixpkgs plain nix copy, run 2 $(date -Is) $(cat /proc/loadavg)"
bash $S/g3-baseline.sh g1-nixpkgs $S/store-3ec6363 $cur
echo "===== end $(date -Is) $(cat /proc/loadavg)"
