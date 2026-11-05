#!/usr/bin/env bash
# G3 as its p2-gates row says it: a client VM runs `rime update` for the weekly pair (delta.md §3, g1-nixpkgs) with and
# without deltas, against the real release pipeline (nix/tests/update-delta.nix with `pair`). Inside the rime-nix
# container, from the worktree:
#   nix/release/delta/lab/g3-vm.sh LOG [OLD_TOPLEVEL NEW_TOPLEVEL]
# The VM disks live in the build directory, so it is put under /work/scratch (not the container's /tmp on /var).
set -euo pipefail
wt=$(pwd) log=$1
old=${2:-/nix/store/yygjs399k446xfa55yr0l9ycf81xkd4s-nixos-system-rime-vm-26.11.20261023.3ec6363}
new=${3:-/nix/store/fjxcryan7pdbhj0gp4bfm9jj782xx5nl-nixos-system-rime-vm-26.11.20261030.1c89083}
bd=/work/scratch/deltas/build
mkdir -p "$bd"
expr="let f = builtins.getFlake \"path:$wt\"; pkgs = import $wt/nix/tests/lib/test-pkgs.nix (import f.inputs.nixpkgs { system = \"x86_64-linux\"; overlays = [ f.overlays.default ]; }); in import $wt/nix/tests/update-delta.nix { inherit pkgs; self = f; pair = { old = builtins.storePath \"$old\"; new = builtins.storePath \"$new\"; }; }"
echo "host load before: $(cat /proc/loadavg)" | tee "$log"
start=$(date +%s)
set +e
nix build --impure --no-link -L --option cores 4 --option build-dir "$bd" --expr "$expr" >> "$log" 2>&1
rc=$?
set -e
echo "exit $rc after $(( $(date +%s) - start )) s; host load after: $(cat /proc/loadavg)" | tee -a "$log"
grep -o 'G3 [A-Za-z ]*:.*\|G3 RESULT.*' "$log" | sed 's/^.*\(G3 \)/\1/' | sort -u || true
exit $rc
