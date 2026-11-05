#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-suites-run-in-ci.sh — a suite nobody runs is not a gate.
#
#  CI invokes suites BY NAME. Writing tests/test-foo.sh, landing it, and never
#  adding a line to a workflow leaves a file that looks like coverage, passes
#  review, and gates nothing. Measured 2026-09-12: 70 test-*.sh suites existed
#  and 13 were named in no workflow — among them tests/test-rime-lid.sh, sixty
#  assertions for a feature the owner had asked for by name, which no CI run
#  had ever executed.
#
#  Same species as a path selector that matches nothing and a lint list that
#  omits new files: the step runs, reports success, and inspects nothing. This
#  repository's own workflow comments record five instances.
#
#  A suite must therefore be EITHER named in .github/workflows/, OR listed in
#  tests/suites-not-in-ci.txt with the reason on the same line. Both arms fail:
#
#    a suite in neither place            -> fail, named
#    a listed suite now run by CI        -> fail, "delete the line"
#
#  The second arm is what stops the exemption list becoming permanent.
#
#  On a branch with a Nix flake (feat/nixos), a FLAKE CHECK is a CI home too:
#  .github/workflows/nix.yml builds every check except the ones listed, with a
#  reason, in nix/ci/checks-not-in-ci.txt (nix/ci/run-checks.sh plan). So:
#
#    a suite named in no workflow but run by a nix/tests/*.nix file -> run by
#      CI, unless that file defines a check CI skips (counted as unrun then)
#    a skip-list entry with no reason, or naming no check  -> fail
#    nix.yml no longer running the plan                   -> every check unrun
#
#  The check names come from RIME_FLAKE_CHECKS_FILE (one per line; nix.yml's
#  plan job writes it) or, failing that, from `nix eval` when nix is on PATH.
#  With neither, the flake half is NOT verified and the script says so on a
#  warning line rather than passing it silently. Without a flake.nix (main,
#  the bootc product) the script behaves exactly as it did before.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 1

EXEMPT=tests/suites-not-in-ci.txt
WORKFLOWS=.github/workflows

[ -d "$WORKFLOWS" ] || { echo "FATAL: no $WORKFLOWS directory"; exit 1; }

# A filename is never split mid-token by a line continuation -- a backslash
# wraps BETWEEN arguments -- so the raw files are searched directly. An earlier
# draft joined continuations with sed first; the escaping inside its own
# heredoc differed from what was tested at the prompt, and it silently matched
# nothing, reporting suites as unrun that CI runs twice. Simpler is checkable.
exempt=()
[ -f "$EXEMPT" ] && mapfile -t exempt < <(grep -vE '^\s*(#|$)' "$EXEMPT" | awk '{print $1}')
is_exempt() { local n; for n in ${exempt[@]+"${exempt[@]}"}; do [ "$n" = "$1" ] && return 0; done; return 1; }

# ── the discovery root was itself a hand-written list ───────────────────────
# This file was written to replace a hand-written list of suites, and it kept
# one: `tests/`. The installer's suites live in `installer/` and are named one
# by one in pr-validation.yml's `installer` job, so another one could be landed,
# look like coverage, and be run by nobody — the exact defect this file exists
# to prevent, one directory over. Discovery has to cover every place a suite
# lives, or it is a list again.
#
# It kept a second hand-written list for two weeks after that was written: the
# NAME. Discovery matched `test-*.sh` only, so every `check-*.sh` in this very
# directory — this file included — was invisible to it. Measured 2026-09-21:
# 8 check-*.sh existed and `tests/check-no-conflict-markers.sh` was named in no
# workflow, having been landed after a merge replay shipped `<<<<<<<` markers
# into `src/state/IpcManager.qml` on the rime-shell tip. That is a QML parse
# error, so IpcManager failed to load, so every singleton importing `src/`
# failed with it and the shell did not start at all. The gate written to stop
# that recurring was itself never run. A gate this file cannot see is exactly
# the defect this file exists to name, so the pattern is now `{test,check}-*`.
# ── flake checks: which ones CI runs, and which nix/tests files those are ────
FLAKE_SKIP=nix/ci/checks-not-in-ci.txt
NIX_WORKFLOW="$WORKFLOWS/nix.yml"
flake_rc=0; flake_note=""; flake_checks=(); flake_skipped=(); skipped_files=()
nix_runs_plan=0
if [ -f flake.nix ]; then
    # The plan is what turns "a flake check" into "a CI run". A workflow that
    # mentions the script only in a comment runs nothing.
    if [ -f "$NIX_WORKFLOW" ] && grep -hF 'nix/ci/run-checks.sh plan' "$NIX_WORKFLOW" | pipe_has -vE '^[[:space:]]*#'; then
        nix_runs_plan=1
    fi
    checks_file=""
    if [ -n "${RIME_FLAKE_CHECKS_FILE:-}" ]; then
        checks_file=$RIME_FLAKE_CHECKS_FILE
    elif command -v nix >/dev/null 2>&1; then
        checks_file=$(mktemp)
        trap 'rm -f "$checks_file"' EXIT
        FLAKE=${FLAKE:-path:.} nix/ci/run-checks.sh list > "$checks_file" || { echo "FATAL: cannot list the flake checks"; exit 1; }
    fi
    if [ -z "$checks_file" ]; then
        flake_note="NOT VERIFIED (no nix on PATH and RIME_FLAKE_CHECKS_FILE unset)"
        echo "::warning::flake checks not verified here: no nix on PATH and RIME_FLAKE_CHECKS_FILE unset"
    else
        mapfile -t flake_checks < <(grep -vE '^\s*(#|$)' "$checks_file")
        if [ -f "$FLAKE_SKIP" ]; then
            while read -r name reason; do
                case "$name" in ''|'#'*) continue ;; esac
                if [ -z "$reason" ]; then
                    echo "  FAIL — $FLAKE_SKIP skips $name with no reason"; flake_rc=1
                fi
                found=0
                for c in ${flake_checks[@]+"${flake_checks[@]}"}; do [ "$c" = "$name" ] && found=1; done
                if [ "$found" = 0 ]; then
                    echo "  FAIL — $FLAKE_SKIP skips $name, which is not a flake check: delete the line"; flake_rc=1
                fi
                flake_skipped+=("$name")
                # The file that defines the skipped check: `name = import ./file.nix` in
                # nix/tests/default.nix, else a top-level `name =` in a nix/tests file.
                f=$(sed -nE "s|^[[:space:]]*\"?$name\"?[[:space:]]*=[[:space:]]*import[[:space:]]+\./([A-Za-z0-9_./-]+\.nix).*|nix/tests/\1|p" nix/tests/default.nix 2>/dev/null | head -1)
                [ -n "$f" ] || f=$(grep -lE "^[[:space:]]*\"?$name\"?[[:space:]]*=" nix/tests/*.nix 2>/dev/null | head -1)
                [ -n "$f" ] && skipped_files+=("$f")
            done < "$FLAKE_SKIP"
        fi
        if [ "$nix_runs_plan" = 0 ] && [ "${#flake_checks[@]}" -gt "${#flake_skipped[@]}" ]; then
            echo "  FAIL — $NIX_WORKFLOW does not run nix/ci/run-checks.sh plan, so no flake check runs in CI"
            flake_rc=1
        fi
        flake_note="$(( ${#flake_checks[@]} - ${#flake_skipped[@]} )) run by $NIX_WORKFLOW, ${#flake_skipped[@]} skipped with a reason"
    fi
fi
is_skipped_file() { local f; for f in ${skipped_files[@]+"${skipped_files[@]}"}; do [ "$f" = "$1" ] && return 0; done; return 1; }

# A suite run by a flake check that CI runs: named (not in a comment) in a nix/tests file, none of whose checks CI
# skips. Conservative on purpose: a file defining one skipped check counts as skipped as a whole.
run_by_flake_check() {
    local base=$1 f
    [ "$nix_runs_plan" = 1 ] && [ "${#flake_checks[@]}" -gt 0 ] || return 1
    for f in nix/tests/*.nix; do
        [ -f "$f" ] || continue
        is_skipped_file "$f" && continue
        if grep -hF "$base" "$f" | pipe_has -vE '^[[:space:]]*#'; then
            return 0
        fi
    done
    return 1
}

missing=(); resurrected=(); run=0; run_flake=0
for s in tests/test-*.sh tests/check-*.sh installer/test-*.sh installer/check-*.sh; do
    [ -f "$s" ] || continue
    base="${s##*/}"
    # A COMMENT naming a suite is not an invocation of it. This file's own
    # step in pr-validation.yml describes the defect by naming
    # tests/test-rime-lid.sh, and a bare `grep -F` read that prose as proof the
    # suite runs — the same shape as the forbid-check this repository once
    # shipped that matched the comment explaining what it forbade. Lines whose
    # first non-blank character is `#` are dropped, which covers both YAML
    # comments and shell comments inside a `run:` block.
    if grep -rhF "$base" "$WORKFLOWS"/ | pipe_has -vE '^[[:space:]]*#'; then
        run=$((run + 1))
        is_exempt "$s" && resurrected+=("$s")
    elif run_by_flake_check "$base"; then
        run=$((run + 1)); run_flake=$((run_flake + 1))
        is_exempt "$s" && resurrected+=("$s")
    else
        is_exempt "$s" || missing+=("$s")
    fi
done

total=$(ls tests/test-*.sh tests/check-*.sh installer/test-*.sh installer/check-*.sh 2>/dev/null | wc -l | tr -d ' ')
printf '\nsuite coverage: %s suites, %d run by CI, %d exempt, %d unrun and undeclared\n' \
    "$total" "$run" "${#exempt[@]}" "${#missing[@]}"
if [ -f flake.nix ]; then
    printf 'flake checks: %s; %d suite(s) run through a flake check\n' "$flake_note" "$run_flake"
fi

rc=$flake_rc
if [ "${#missing[@]}" -gt 0 ]; then
    echo "  FAIL — these suites are named in no workflow and are not in $EXEMPT:"
    printf '    %s\n' "${missing[@]}"
    echo "  Add the suite to a workflow, or record it in $EXEMPT with the reason."
    rc=1
fi
if [ "${#resurrected[@]}" -gt 0 ]; then
    echo "  FAIL — these are in $EXEMPT but CI now runs them. Delete their lines:"
    printf '    %s\n' "${resurrected[@]}"
    rc=1
fi
if [ "$rc" -eq 0 ]; then
    if [ -f flake.nix ]; then
        echo "  every suite (and flake check) is either run by CI or a recorded, reasoned exception"
    else
        echo "  every suite is either run by CI or a recorded, reasoned exception"
    fi
fi
exit "$rc"
