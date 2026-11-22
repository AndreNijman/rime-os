#!/usr/bin/env bash
# test-eval-shard.sh: nix/ci/eval-each.sh's shards cover every flake output exactly once, and --merge refuses
# anything less, with a stub nix (no evaluation, seconds). Run by checks.ci (nix/tests/ci.nix); locally:
#
#   bash nix/ci/test-eval-shard.sh        (needs bash, coreutils, findutils, grep, python3 + pyyaml)
#
# Proves: for N = 1, 2, 3, 6, 8 the shards' merged output is byte-identical to one unsharded run; every item is
# assigned to exactly one shard; and --merge refuses a missing part, a duplicated part, a part from another
# revision, a failed item, a shard that swallowed its failure (exit 0, value missing), a partition that drops an
# item, and an "OK" with no value. Then nix/ci/check-eval-shards.py over nix.yml, and the workflow mutants it refuses.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
EACH="$here/eval-each.sh"
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
fails=0
ok() { echo "PASS: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
# expect RC DESCRIPTION -- CMD...: CMD must exit with RC (0, or "nonzero").
expect() {
    local want=$1 what=$2 rc; shift 3
    "$@" > "$t/last.out" 2>&1; rc=$?
    if { [ "$want" = 0 ] && [ "$rc" = 0 ]; } || { [ "$want" = nonzero ] && [ "$rc" != 0 ]; }; then ok "$what (rc $rc)"
    else bad "$what (rc $rc, wanted $want)"; while IFS= read -r l; do echo "    | $l"; done < "$t/last.out"; fi
}
has() { grep -F -- "$1" "$t/last.out" > /dev/null || { bad "output lacks: $1"; while IFS= read -r l; do echo "    | $l"; done < "$t/last.out"; }; }

# --- the stub nix: names from fixture files, a drvPath per attribute, a failure for any attribute in fail.txt
mkdir -p "$t/bin" "$t/fx"
# The bash on PATH, not /usr/bin/env (absent in the Nix build sandbox).
printf '#!%s\n' "$(command -v bash)" | tee "$t/bin/nix" > "$t/bin/nix-store"
cat >> "$t/bin/nix" << 'STUB'
if [ "${1:-}" = path-info ]; then
    # nix path-info --store file://CACHE OUT: in the results cache when the test marked it so (CACHE/<name>.ok)
    store='' last=''
    while [ $# -gt 0 ]; do case "$1" in --store) store=${2#file://}; shift ;; *) last=$1 ;; esac; shift; done
    [ -e "$store/${last##*/}.ok" ]; exit
fi
attr='' apply=0
for a in "$@"; do
    case "$a" in --apply) apply=1 ;; *'#'*) attr="${a#*#}" ;; esac
done
if [ "$apply" = 1 ]; then
    case "$attr" in
        checks.*) cat "$STUB_FX/checks.txt" ;;
        packages.*) cat "$STUB_FX/packages.txt" ;;
        nixosConfigurations) cat "$STUB_FX/configs.txt" ;;
        nixosModules | overlays) grep -qxF "$attr" "$STUB_FX/fail.txt" && { echo "error: $attr is broken" >&2; exit 1; }; echo true ;;
    esac
    exit 0
fi
grep -qxF "$attr" "$STUB_FX/fail.txt" && { echo "error: attribute '$attr' is broken (stub)" >&2; exit 1; }
h=$(printf '%s' "$attr" | sha256sum); h=${h:0:32}
printf '/nix/store/%s-%s.drv' "$h" "${attr%.drvPath}"
STUB
cat >> "$t/bin/nix-store" << 'STUB'
# STUB_NO_DRVS=1: a runner that never instantiated the .drv (the plan job's, since the shards evaluate elsewhere).
[ "${STUB_NO_DRVS:-0}" = 1 ] && { echo "error: path '${*: -1}' is not valid" >&2; exit 1; }
# nix-store -q --outputs DRV
d="${*: -1}"; d=${d#/nix/store/}
printf '/nix/store/%s-out\n' "${d%.drv}"
STUB
chmod +x "$t/bin/nix" "$t/bin/nix-store"
export PATH="$t/bin:$PATH" STUB_FX="$t/fx" RIME_EVAL_JOBS=3 RIME_EVAL_GATE_MIB=0
# Real heavy names (eval-heavy.txt) among them, so heavy weighting and the alone pass are exercised.
printf '%s\n' activation desktops-eval release-plan release-unit update-variants polish security boot-esp-prune \
    hostgates-net luks-boot ci ci-userns a-new-check-not-in-the-cost-table rimed rimed-session update-e2e > "$t/fx/checks.txt"
printf '%s\n' rimed rime-release-plan rime-shell > "$t/fx/packages.txt"
printf '%s\n' rime rime-vm > "$t/fx/configs.txt"
: > "$t/fx/fail.txt"

echo "== one unsharded run (the reference)"
expect 0 "unsharded eval-each" -- env RIME_EVAL_OUT="$t/ref.tsv" bash "$EACH" path:.
items=$(bash "$EACH" --list path:. | wc -l)
if [ "$(wc -l < "$t/ref.tsv")" = "$items" ]; then ok "reference has all $items items"; else bad "reference has $(wc -l < "$t/ref.tsv") of $items items"; fi
while IFS=$'\t' read -r a _ o; do
    case "$a" in *.drvPath) [ -n "$o" ] || bad "no output path column for $a" ;; esac
done < "$t/ref.tsv"

shards() { # N DIR [eval-each]: run every shard of N into DIR/<k>; return the number that failed
    local n=$1 dir=$2 each=${3:-$EACH} k f=0
    rm -rf "$dir"; mkdir -p "$dir"
    for ((k = 1; k <= n; k++)); do
        RIME_EVAL_PART="$dir/eval-$k" bash "$each" --shard "$k/$n" path:. > "$dir/log.$k" 2>&1 || f=$((f + 1))
    done
    return "$f"
}

for n in 1 2 3 6 8; do
    echo "== $n shards"
    expect 0 "every shard of $n evaluates" -- shards "$n" "$t/p$n"
    cat "$t/p$n"/eval-*/assigned | sort > "$t/assigned.$n"
    bash "$EACH" --list path:. | sort > "$t/all.sorted"
    if cmp -s "$t/assigned.$n" "$t/all.sorted"; then ok "$n shards: every item assigned exactly once"
    else bad "$n shards: the assignment is not a partition"; diff "$t/assigned.$n" "$t/all.sorted" | head; fi
    expect 0 "merge $n shards" -- env RIME_EVAL_OUT="$t/m$n.tsv" bash "$EACH" --merge "$t/p$n" path:.
    if cmp -s "$t/m$n.tsv" "$t/ref.tsv"; then ok "$n shards merged = the unsharded run, byte for byte"; else bad "$n shards merged differ from the unsharded run"; fi
done

echo "== the plan reuses passes by the merged output paths, on a runner that never instantiated a .drv"
# run-checks.sh plan, as the plan job runs it after --merge: the .drv files exist only on the shards' runners.
mkdir -p "$t/res/rime-results"; : > "$t/noskip.txt"
o=$(grep -F "checks.x86_64-linux.polish.drvPath" "$t/m6.tsv" | cut -f3)
printf 'polish\t%s\t4242\tsha\tdate\n' "$o" > "$t/res/rime-results/polish.tsv"; touch "$t/res/${o##*/}.ok"
plan_env=(env RIME_FLAKE_CHECKS_FILE="$t/fx/checks.txt" RIME_CI_SKIP_FILE="$t/noskip.txt" RIME_RESULTS="$t/res" STUB_NO_DRVS=1)
expect 0 "plan with the merged drvs.tsv" -- "${plan_env[@]}" RIME_CHECK_DRVS="$t/m6.tsv" bash "$here/run-checks.sh" plan "$t/plan"
has "REUSED polish: passed earlier with the same inputs (run 4242)"
if [ "$(wc -l < "$t/plan/outs.tsv")" = "$(wc -l < "$t/fx/checks.txt")" ]; then ok "every check has its output path in outs.tsv (what results-merge compares)"
else bad "outs.tsv has $(wc -l < "$t/plan/outs.tsv") of $(wc -l < "$t/fx/checks.txt") checks"; fi
echo "== mutant: the shards write only attribute + drvPath (no output path column)"
cut -f1,2 "$t/m6.tsv" > "$t/m6-two.tsv"
"${plan_env[@]}" RIME_CHECK_DRVS="$t/m6-two.tsv" bash "$here/run-checks.sh" plan "$t/plan2" > "$t/last.out" 2>&1
if grep -qF "REUSED polish" "$t/last.out"; then bad "mutant survived: reuse without output paths"
else ok "killed: without the output path column nothing is reused on the plan runner"; fi

echo "== --merge refuses what is not a whole, successful evaluation"
cp -r "$t/p6" "$t/x"; rm -rf "$t/x/eval-4"
expect nonzero "a shard that left no part" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "shard 4 of 6 left no part"
rm -rf "$t/x"; cp -r "$t/p6" "$t/x"; cp -r "$t/x/eval-2" "$t/x/eval-2-again"
expect nonzero "the same shard twice" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "has two parts"
rm -rf "$t/x"; cp -r "$t/p6" "$t/x"; echo checks.x86_64-linux.gone.drvPath >> "$t/x/eval-3/all"
expect nonzero "a part that enumerated other items" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "enumerated other items"
rm -rf "$t/x"; cp -r "$t/p6" "$t/x"
k=$(grep -l . "$t/x"/eval-*/drvs.tsv | head -1); k=${k%/drvs.tsv}
a=$(head -1 "$k/drvs.tsv" | cut -f1); grep -vF "$a" "$k/drvs.tsv" > "$k/d"; mv "$k/d" "$k/drvs.tsv"
expect nonzero "an item marked OK with no value" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "has no drvPath"
rm -rf "$t/x"; cp -r "$t/p6" "$t/x"; echo "7 6" > "$t/x/eval-5/shard"
expect nonzero "a part numbered past N" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "shard 5 of 6 left no part"
rm -rf "$t/x"; cp -r "$t/p6" "$t/x"; echo "x 6" > "$t/x/eval-5/shard"
expect nonzero "a part with no shard number" -- env RIME_EVAL_OUT="$t/x.tsv" bash "$EACH" --merge "$t/x" path:.
has "has no shard number"
expect nonzero "--shard 7/6 does not exist" -- bash "$EACH" --shard 7/6 path:.
expect nonzero "--shard 0/6 does not exist" -- bash "$EACH" --shard 0/6 path:.
expect nonzero "--shard x/6 is not a number" -- bash "$EACH" --shard x/6 path:.

echo "== a failed evaluation in one shard"
echo checks.x86_64-linux.polish.drvPath > "$t/fx/fail.txt"
expect nonzero "the shard holding the broken item fails" -- shards 6 "$t/pf"
expect nonzero "and the merge refuses it" -- env RIME_EVAL_OUT="$t/pf.tsv" bash "$EACH" --merge "$t/pf" path:.
has "'checks.x86_64-linux.polish.drvPath' did not evaluate in shard"
echo nixosModules > "$t/fx/fail.txt"
expect nonzero "a broken nixosModules fails its shard" -- shards 6 "$t/pm"
expect nonzero "and the merge" -- env RIME_EVAL_OUT="$t/pm.tsv" bash "$EACH" --merge "$t/pm" path:.

echo "== mutant: a shard that swallows its failure (exits 0)"
# shellcheck disable=SC2016 # the literal line of eval-each.sh
grep -vxF '[ ! -e "$work/failed" ]' "$EACH" > "$t/each-swallow.sh"
cmp -s "$EACH" "$t/each-swallow.sh" && bad "mutant: pattern not found"
echo checks.x86_64-linux.polish.drvPath > "$t/fx/fail.txt"
expect 0 "the mutant's shards all exit 0" -- shards 6 "$t/ps" "$t/each-swallow.sh"
expect nonzero "killed: the merge refuses the missing value" -- env RIME_EVAL_OUT="$t/ps.tsv" bash "$EACH" --merge "$t/ps" path:.
has "did not evaluate in shard"
: > "$t/fx/fail.txt"

echo "== mutant: a partition that drops an item (assigned to no shard)"
# The greedy loop skips the lightest item, so no shard owns it. First layer: each shard's own check refuses.
python3 - "$EACH" "$t/each-drop.sh" "$t/each-drop-nocheck.sh" << 'PY'
import sys
s = open(sys.argv[1]).read()
old = '    done < "$work/weighted"\n'
assert s.count(old) == 1, "mutant: pattern not found"
s = s.replace(old, '    done < <(head -n -1 "$work/weighted")\n')
open(sys.argv[2], "w").write(s)
# Second layer: the same mutant without the shard's self-check, so every shard exits 0; --merge must still refuse.
chk = '        echo "eval-each: \'$item\' was assigned to no shard (partition bug)" >&2; exit 2\n'
assert s.count(chk) == 1, "mutant: self-check not found"
open(sys.argv[3], "w").write(s.replace(chk, '        :\n'))
PY
expect nonzero "killed: every shard refuses the partition" -- shards 6 "$t/pd1" "$t/each-drop.sh"
if grep -l "was assigned to no shard (partition bug)" "$t/pd1"/log.* > /dev/null; then ok "and says why"; else bad "no 'assigned to no shard' line in the shard logs"; fi
expect 0 "without its self-check the mutant's shards all exit 0" -- shards 6 "$t/pd" "$t/each-drop-nocheck.sh"
expect nonzero "killed: the merge refuses the unassigned item" -- env RIME_EVAL_OUT="$t/pd.tsv" bash "$EACH" --merge "$t/pd" path:.
has "was assigned to no shard: nothing evaluated it"

echo "== mutant: an item assigned to two shards"
python3 - "$EACH" "$t/each-dup.sh" << 'PY'
import sys
s = open(sys.argv[1]).read()
old = 'while IFS=$\'\\t\' read -r o item; do [ "$o" = "$SHARD" ] && echo "$item" >> "$work/mine"; done < "$work/assign"'
assert s.count(old) == 1, "mutant: pattern not found"
s = s.replace(old, old + '\n[ "$SHARD" = 2 ] && head -1 "$work/attrs" >> "$work/mine"')
open(sys.argv[2], "w").write(s)
PY
shards 6 "$t/pdup" "$t/each-dup.sh"
expect nonzero "killed: the merge refuses the double assignment" -- env RIME_EVAL_OUT="$t/pdup.tsv" bash "$EACH" --merge "$t/pdup" path:.
has "was assigned to 2 shards"

echo "== nix.yml: the shard job graph (nix/ci/check-eval-shards.py)"
WF="$repo/.github/workflows/nix.yml"
expect 0 "nix.yml's eval shards gate the plan and the summary" -- python3 "$here/check-eval-shards.py" "$WF"
mutant() { # DESCRIPTION PYTHON-EXPR-ON-s: apply to nix.yml, the guard must refuse it
    python3 - "$WF" "$t/wf.yml" "$2" << 'PY'
import sys
s = open(sys.argv[1]).read()
new = eval(sys.argv[3], {"s": s})
assert new != s, "mutant: pattern not found"
open(sys.argv[2], "w").write(new)
PY
    expect nonzero "killed: $1" -- python3 "$here/check-eval-shards.py" "$t/wf.yml"
}
mutant "the eval shards may fail without failing (continue-on-error)" \
    "s.replace('    name: Evaluate shard', '    continue-on-error: true\n    name: Evaluate shard', 1)"
mutant "the summary does not look at the eval shards' result" \
    "s.replace('[ \"\$EVAL_RESULT\" = success ]', '[ \"\$EVAL_RESULT\" = success ] || true', 1)"
mutant "the summary does not need the eval shards" \
    "s.replace('needs: [eval, plan, packages, check]', 'needs: [plan, packages, check]', 1)"
mutant "the plan does not wait for the eval shards" \
    "s.replace('    needs: [eval]\n', '', 1)"
mutant "a shard told a fixed N instead of the matrix's own size" \
    "s.replace('/\${{ strategy.job-total }}', '/6', 1)"
mutant "the plan runs even when a shard failed" \
    "s.replace('    needs: [eval]\n', '    needs: [eval]\n    if: always()\n', 1)"
mutant "the plan does not merge the shards" \
    "s.replace('nix/ci/eval-each.sh --merge', 'true nix/ci/eval-each.sh --merge', 1)"

echo
if [ "$fails" = 0 ]; then echo "test-eval-shard: all passed"; else echo "test-eval-shard: $fails failed"; fi
[ "$fails" = 0 ]
