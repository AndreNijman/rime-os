#!/usr/bin/env bash
# outs.sh — for every flake check of this checkout (RIME_TREE overrides): its outPath and whether it is valid in the
# store (BUILT / MISSING / EVALFAIL). Inside the rime-nix container:
#   sudo -n podman exec -w /work/<worktree> rime-nix nix/ci/integrate/outs.sh > outs.txt
set -uo pipefail
tree=${RIME_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
[ -f "$tree/flake.nix" ] || { echo "outs.sh: $tree is not a Rime checkout (no flake.nix)" >&2; exit 2; }
cd "$tree" || exit 2
names=$(nix eval --raw path:.#checks.x86_64-linux --apply 'a: builtins.concatStringsSep "\n" (builtins.attrNames a)') || exit 1
one() {
  o=$(nix eval --raw "path:.#checks.x86_64-linux.$1.outPath" 2>/dev/null) || { echo "EVALFAIL $1"; return; }
  if nix path-info "$o" >/dev/null 2>&1; then echo "BUILT $1 $o"; else echo "MISSING $1 $o"; fi
}
export -f one
printf '%s\n' "$names" | xargs -P 3 -I{} bash -c 'one {}'
