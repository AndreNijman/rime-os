#!/usr/bin/env bash
# run-checks.sh: what .github/workflows/nix.yml runs, as one script, so a local run is the same command.
#
#   nix/ci/run-checks.sh list                     every flake check, one name per line
#   nix/ci/run-checks.sh plan OUTDIR              which checks CI runs and which it skips, and why
#   nix/ci/run-checks.sh build CHECK OUTDIR       build one check, record pass/fail/timeout in OUTDIR/CHECK.result
#   nix/ci/run-checks.sh summary PLANDIR RESDIR   the RAN/SKIPPED table; exits 1 unless every planned check passed
#
# The plan is "every check, minus nix/ci/checks-not-in-ci.txt". A check added to the flake is therefore run by CI
# the day it lands, with no list to update; a check CI does not run has to be written down with its reason, and is
# printed as SKIPPED with that reason in every run, so a skip never reads as a pass.
#
# Environment:
#   FLAKE                   the flake reference (default path:. ; a git worktree whose .git points outside the
#                           tree needs path:, and CI uses the same reference so the commands are identical)
#   RIME_FLAKE_CHECKS_FILE  read the check names from this file instead of evaluating the flake (the tests use it)
#   RIME_CI_SKIP_FILE       the skip list (default nix/ci/checks-not-in-ci.txt)
#   RIME_CHECK_TIMEOUT      build: seconds before the build is stopped and recorded as `timeout` (default 0 = none).
#                           nix.yml sets it below the job's own timeout-minutes, so a check that runs too long is
#                           reported as TIMED OUT with its time instead of a job killed with no result at all.
set -euo pipefail

FLAKE=${FLAKE:-path:.}
SYSTEM=${SYSTEM:-x86_64-linux}
SKIP_FILE=${RIME_CI_SKIP_FILE:-nix/ci/checks-not-in-ci.txt}

die() { echo "run-checks: $*" >&2; exit 2; }

list_checks() {
    if [ -n "${RIME_FLAKE_CHECKS_FILE:-}" ]; then
        grep -vE '^\s*(#|$)' "$RIME_FLAKE_CHECKS_FILE" | sort -u
    else
        nix eval --json "$FLAKE#checks.$SYSTEM" --apply builtins.attrNames \
            | tr -d '[]"' | tr ',' '\n' | grep -vE '^\s*$' | sort -u
    fi
}

# The skip list as "name<TAB>reason" lines. Refuses an entry with no reason and a name listed twice.
read_skips() {
    [ -f "$SKIP_FILE" ] || { echo "run-checks: no skip list at $SKIP_FILE" >&2; return 1; }
    awk '
        /^[[:space:]]*(#|$)/ { next }
        {
            name = $1; reason = $0
            sub(/^[[:space:]]*[^[:space:]]+[[:space:]]*/, "", reason)
            if (reason == "") { printf "run-checks: %s is skipped with no reason\n", name > "/dev/stderr"; bad = 1 }
            if (seen[name]++) { printf "run-checks: %s is listed twice\n", name > "/dev/stderr"; bad = 1 }
            printf "%s\t%s\n", name, reason
        }
        END { exit bad }
    ' "$SKIP_FILE"
}

json_array() { # stdin: one string per line -> a compact JSON array
    local first=1
    printf '['
    while IFS= read -r x; do
        [ -n "$x" ] || continue
        [ "$first" = 1 ] || printf ','
        first=0
        printf '"%s"' "$(printf '%s' "$x" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    done
    printf ']\n'
}

plan() {
    local out=$1 rc=0
    mkdir -p "$out"
    list_checks > "$out/checks.txt"
    [ -s "$out/checks.txt" ] || die "the flake has no checks (or they could not be listed)"
    read_skips > "$out/skipped.tsv" || rc=1

    # A skip entry naming a check that does not exist is a list that has stopped saying what is true.
    while IFS=$'\t' read -r name _; do
        if ! grep -qxF "$name" "$out/checks.txt"; then
            echo "run-checks: $SKIP_FILE skips '$name', which is not a flake check (renamed or removed?)" >&2
            rc=1
        fi
    done < "$out/skipped.tsv"

    : > "$out/run.txt"
    while IFS= read -r c; do
        reason=$(awk -F'\t' -v n="$c" '$1 == n { print $2 }' "$out/skipped.tsv")
        if [ -n "$reason" ]; then
            echo "SKIPPED $c: $reason"
        else
            echo "WILL RUN $c"
            echo "$c" >> "$out/run.txt"
        fi
    done < "$out/checks.txt"
    json_array < "$out/run.txt" > "$out/run.json"
    json_array < "$out/checks.txt" > "$out/checks.json"
    printf 'plan: %s checks, %s run by CI, %s skipped\n' \
        "$(wc -l < "$out/checks.txt" | tr -d ' ')" "$(wc -l < "$out/run.txt" | tr -d ' ')" \
        "$(wc -l < "$out/skipped.tsv" | tr -d ' ')"
    return "$rc"
}

build() {
    local check=$1 out=$2 limit=${RIME_CHECK_TIMEOUT:-0} start rc secs
    mkdir -p "$out"
    start=$(date +%s)
    set +e
    if [ "$limit" -gt 0 ]; then
        timeout --kill-after=60 "$limit" nix build "$FLAKE#checks.$SYSTEM.$check" -L --print-out-paths --no-link
    else
        nix build "$FLAKE#checks.$SYSTEM.$check" -L --print-out-paths --no-link
    fi
    rc=$?
    set -e
    secs=$(( $(date +%s) - start ))
    if [ "$rc" = 0 ]; then
        printf 'pass\t%s\n' "$secs" > "$out/$check.result"
    elif [ "$limit" -gt 0 ] && { [ "$rc" = 124 ] || [ "$rc" = 137 ]; } && [ "$secs" -ge "$limit" ]; then
        echo "run-checks: $check did not finish in $limit s; stopped" >&2
        printf 'timeout\t%s\n' "$secs" > "$out/$check.result"
    else
        printf 'fail\t%s\n' "$secs" > "$out/$check.result"
    fi
    return "$rc"
}

summary() {
    local plan=$1 res=$2 rc=0 c status secs reason
    [ -s "$plan/checks.txt" ] || die "no plan in $plan"
    echo "| flake check | CI | result | why |"
    echo "|---|---|---|---|"
    while IFS= read -r c; do
        reason=$(awk -F'\t' -v n="$c" '$1 == n { print $2 }' "$plan/skipped.tsv" 2>/dev/null || true)
        if [ -n "$reason" ]; then
            echo "| \`$c\` | SKIPPED | not built | $reason |"
            continue
        fi
        if [ -f "$res/$c.result" ]; then
            IFS=$'\t' read -r status secs < "$res/$c.result"
        else
            status=missing secs=
        fi
        case "$status" in
            pass) echo "| \`$c\` | RAN | pass (${secs} s) | |" ;;
            fail) echo "| \`$c\` | RAN | **FAIL** (${secs} s) | |"; rc=1 ;;
            timeout) echo "| \`$c\` | RAN | **TIMED OUT** (stopped after ${secs} s) | RIME_CHECK_TIMEOUT in nix.yml |"; rc=1 ;;
            setup) echo "| \`$c\` | NOT RUN | **FAIL** before the build | the job's setup failed (disk, Nix or KVM) |"; rc=1 ;;
            *)    echo "| \`$c\` | RAN | **NO RESULT** | the job did not report (cancelled, timed out or never started) |"; rc=1 ;;
        esac
    done < "$plan/checks.txt"
    return "$rc"
}

cmd=${1:-}
shift || true
case "$cmd" in
    list) list_checks ;;
    plan) [ $# = 1 ] || die "usage: plan OUTDIR"; plan "$1" ;;
    build) [ $# = 2 ] || die "usage: build CHECK OUTDIR"; build "$1" "$2" ;;
    summary) [ $# = 2 ] || die "usage: summary PLANDIR RESULTDIR"; summary "$1" "$2" ;;
    *) die "usage: $0 list | plan OUTDIR | build CHECK OUTDIR | summary PLANDIR RESULTDIR" ;;
esac
