#!/usr/bin/env bash
# Mutants of rimed's delta client (rimed/rime/src/nixos/delta.rs): each must make the update-delta VM check FAIL.
# Inside the rime-nix container, from the worktree:  nix/release/delta/lab/update-delta-mutants.sh LOGDIR [NAME...]
# The check itself must pass first: nix build path:.#checks.x86_64-linux.update-delta
set -uo pipefail
wt=$(pwd) logs=$1; shift
mkdir -p "$logs"
declare -A M=(
  # a refused delta stops the update instead of fetching that path whole
  [refusal-aborts]='s/out.refused.push((nodes\[i\].path.clone(), why));/return Err(why);/'
  # the decoder window cap is never applied
  [window-cap-ignored]='s/if w > s.window_log_max {/if false \&\& w > s.window_log_max {/'
  # the downloaded delta file is not checked against the manifest (a tampered delta reaches the decoder)
  [delta-hash-unchecked]='s/if size != e.file_size || !hash::nar_hash_bytes(\&e.file_hash).is_some_and(|h| h == got) {/if false \&\& size != e.file_size {/'
  # the local base is not checked against the manifest's base (a changed base reaches the decoder)
  [base-unchecked]='s/if over || n != size || !hash::nar_hash_bytes(nar_hash).is_some_and(|x| x == h.finalize().to_vec()) {/if over {/'
  # --no-deltas is ignored
  [no-deltas-ignored]='s/let settings = if no_deltas { None } else { Settings::from_config(cfg) };/let settings = Settings::from_config(cfg);/'
)
names=("$@"); [ ${#names[@]} -eq 0 ] && names=("${!M[@]}")
expr="{ mutant }: let f = builtins.getFlake \"path:$wt\"; pkgs = import $wt/nix/tests/lib/test-pkgs.nix (import f.inputs.nixpkgs { system = \"x86_64-linux\"; overlays = [ f.overlays.default ]; }); in import $wt/nix/tests/update-delta.nix { inherit pkgs; self = f; inherit mutant; }"
rc=0
for n in "${names[@]}"; do
  start=$(date +%s)
  if nix build --impure --no-link -L --option cores 4 --argstr mutant "${M[$n]}" --expr "$expr" > "$logs/mutant-$n.log" 2>&1; then
    echo "mutant $n SURVIVED ($(( $(date +%s) - start )) s): the check passed with it"; rc=1
  elif grep -E "mutant: sed changed nothing|(builder for|Cannot build) .*rimed-mutant" "$logs/mutant-$n.log" >/dev/null; then
    echo "mutant $n did not build (see $logs/mutant-$n.log)"; rc=1
  elif ! grep -E '!!! Test ".*" failed' "$logs/mutant-$n.log" >/dev/null; then
    # A timeout, ENOSPC or a VM that never booted also fails the build: that is not the check catching the mutant.
    echo "mutant $n FAILED WITHOUT A SUBTEST FAILING (see $logs/mutant-$n.log): not counted as caught"; rc=1
  else
    echo "mutant $n caught ($(( $(date +%s) - start )) s): $(grep -E "AssertionError|Test .* failed|assert " "$logs/mutant-$n.log" | head -2 | tr '\n' ' ')"
  fi
done
exit $rc
