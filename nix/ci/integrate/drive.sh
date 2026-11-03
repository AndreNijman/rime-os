#!/usr/bin/env bash
# drive.sh TAG P check... — build flake checks the way an integration does, inside the rime-nix container.
#   sudo -n podman exec -w /work/<worktree> rime-nix nix/ci/integrate/drive.sh F-A 2 desktops-install-kde polish
# Evaluates each check's drvPath in its own process (2 at a time: one evaluator never holds every VM test), then
# builds the .drv files P at a time (--max-jobs 6 each).
#   tree   the checkout this script is in (RIME_TREE overrides)
#   logs   $RIME_ART/<TAG>/<check>.log, summary <TAG>/summary.txt (PASS|FAIL <check> <outPath> <seconds>);
#          RIME_ART defaults to /work/artifacts/<the tree's directory name>
#   builds RIME_BUILD_DIR (default /work/scratch/<the tree's directory name>/builds, made root 0700). When /var/lab
#          is short of space, point it at the container's overlay (e.g. /root/builds).
# shellcheck disable=SC2174 # the parent is made first; -m 700 is meant for the build dir only
set -uo pipefail
[ $# -ge 3 ] || { echo "usage: $0 TAG P check..." >&2; exit 2; }
tag=$1 P=$2; shift 2
tree=${RIME_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
[ -f "$tree/flake.nix" ] || { echo "drive.sh: $tree is not a Rime checkout (no flake.nix)" >&2; exit 2; }
cd "$tree" || exit 2
name=$(basename "$tree")
d=${RIME_ART:-/work/artifacts/$name}/$tag; mkdir -p "$d"
bd=${RIME_BUILD_DIR:-/work/scratch/$name/builds}; mkdir -p "$(dirname "$bd")" && mkdir -p -m 700 "$bd"; export bd
echo "tree $tree at $(git -C "$tree" rev-parse --short HEAD 2>/dev/null || echo '?')" >> "$d/summary.txt"
# shellcheck disable=SC2016 # $drv expands in the child bash, not here
printf '%s\n' "$@" | xargs -P 2 -I{} bash -c 'drv=$(nix eval --raw "path:.#checks.x86_64-linux.{}.drvPath" 2>'"$d"'/{}.eval.err) && echo "{} $drv" > '"$d"'/{}.drv || echo "{} EVALFAIL" > '"$d"'/{}.drv'
echo "df before build: $(df -h "$bd" | tail -1)" >> "$d/summary.txt"
one() {
  c=$1 d=$2; read -r _ drv < "$d/$c.drv"
  if [ "$drv" = EVALFAIL ]; then echo "FAIL $c eval" >> "$d/summary.txt"; return; fi
  start=$(date +%s)
  nix build "$drv^*" -L --max-jobs 6 --option cores 3 --option build-dir "$bd" --no-link --print-out-paths > "$d/$c.log" 2>&1
  rc=$?; o=$(tail -2 "$d/$c.log" | grep '^/nix/store' | tail -1)
  if [ "$rc" = 0 ]; then echo "PASS $c $o $(( $(date +%s) - start ))s" >> "$d/summary.txt"; else echo "FAIL $c rc=$rc $(( $(date +%s) - start ))s" >> "$d/summary.txt"; fi
}
export -f one
printf '%s\n' "$@" | xargs -P "$P" -I{} bash -c "one {} $d"
echo "df after build: $(df -h "$bd" | tail -1)" >> "$d/summary.txt"
echo DONE >> "$d/summary.txt"
