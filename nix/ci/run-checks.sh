#!/usr/bin/env bash
# run-checks.sh: what .github/workflows/nix.yml runs, as one script, so a local run is the same command.
#
#   nix/ci/run-checks.sh list                     every flake check, one name per line
#   nix/ci/run-checks.sh plan OUTDIR              which checks CI runs and which it skips, and why
#   nix/ci/run-checks.sh build CHECK OUTDIR       build one check, record pass/fail/timeout in OUTDIR/CHECK.result
#   nix/ci/run-checks.sh classify LOG             is a failed build's log a download failure (exit 0) or not (1)
#   nix/ci/run-checks.sh summary PLANDIR RESDIR   the RAN/REUSED/SKIPPED table; exits 1 unless every planned check
#                                                 passed or was reused
#
# Build once, reuse everywhere (nix/ci/README.md "Dev cache and reuse"):
#   fresh                             "1 <why>" when this run must reuse nothing (nix/fresh-* branch, trailer
#                                     "Rime-CI: fresh", workflow_dispatch input), else "0"
#   prebuild-plan DIR                 evaluate nix/ci/prebuild.txt: DIR/items.tsv (name, drvPath, outPaths), DIR/key
#   prebuild DIR                      build the items, substituting what RIME_DEV_CACHE already holds; DIR/status.tsv
#                                     says BUILT (in this job) or REUSED (built and tested by an earlier run)
#   prebuild-report DIR               rimed's cargo test report, labelled RAN or REUSED (markdown)
#   dev-cache-export DIR CACHE        write the items' locally built runtime closures to the file:// cache CACHE
#   export-result CHECK RESDIR CACHE  keep a PASSED check's output in CACHE (refuses anything but a pass)
#   results-merge PLANDIR RESDIR INCOMING CACHE
#                                     merge this run's kept passes into CACHE (the rolling results cache), refusing
#                                     any check whose result is not pass and dropping a prior pass that failed now
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
#   RIME_DEV_CACHE          build/prebuild: a file:// binary cache directory (the packages job's dev cache) added as
#                           an extra substituter, trusted without a signature (it never leaves this workflow)
#   RIME_DEV_CACHE_STATE    build: how the leg got the dev cache (exact / fallback <key> / none), for the metrics
#   RIME_CHECK_DRVS         plan: "attribute<TAB>drvPath" lines (eval-each.sh's RIME_EVAL_OUT)
#   RIME_RESULTS            plan: the restored results cache; a check whose evaluated outPath it holds, with a
#                           manifest line, PASSED EARLIER on the same inputs and is not built again (REUSED)
#   RIME_CI_FRESH           plan: 1 = build every planned check anyway (prior passes are still recorded, so a fresh
#                           failure of a check that passed before says so)
#   RIME_RUN                export-result: the run that passed the check (nix.yml: the run number), default "local"
#
# build retries ONCE, and only when classify says the failure was a download (a substitute that could not be
# fetched: cache.nixos.org's HTTP 416 after a broken resume, curl's "Failed sending data to the peer"), never
# when any builder failed. The retry is said in the log and in the result (`pass N retried`), so it is visible.
set -euo pipefail

FLAKE=${FLAKE:-path:.}
SYSTEM=${SYSTEM:-x86_64-linux}
SKIP_FILE=${RIME_CI_SKIP_FILE:-nix/ci/checks-not-in-ci.txt}
CI_DIR=${RIME_CI_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}
PREBUILD_FILE=${RIME_PREBUILD_FILE:-$CI_DIR/prebuild.txt}

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

    : > "$out/run.txt"; : > "$out/reused.tsv"; : > "$out/prior.tsv"; : > "$out/outs.tsv"
    rm -rf "$out/reused"; mkdir -p "$out/reused"
    local prior prun
    while IFS= read -r c; do
        reason=$(awk -F'\t' -v n="$c" '$1 == n { print $2 }' "$out/skipped.tsv")
        if [ -n "$reason" ]; then
            echo "SKIPPED $c: $reason"
            continue
        fi
        prior=$(prior_pass "$c" "$out")
        if [ -n "$prior" ]; then
            prun=${prior#*$'\t'}
            printf '%s\t%s\n' "$c" "$prior" >> "$out/prior.tsv"
            if [ "${RIME_CI_FRESH:-0}" != 1 ]; then
                echo "REUSED $c: passed earlier with the same inputs (run $prun); not built again"
                printf '%s\t%s\n' "$c" "$prior" >> "$out/reused.tsv"
                printf 'reused\t0\t%s\n' "$prun" > "$out/reused/$c.result"
                printf '{"check": "%s", "status": "reused", "seconds": 0, "reused": true, "passed_in_run": "%s"}\n' \
                    "$c" "$prun" > "$out/reused/$c.metrics.json"
                continue
            fi
            echo "WILL RUN $c (fresh run: it passed earlier with the same inputs, run $prun)"
        else
            echo "WILL RUN $c"
        fi
        echo "$c" >> "$out/run.txt"
    done < "$out/checks.txt"
    json_array < "$out/run.txt" > "$out/run.json"
    json_array < "$out/checks.txt" > "$out/checks.json"
    printf 'plan: %s checks, %s run by CI, %s reused (passed earlier, same inputs), %s skipped%s\n' \
        "$(wc -l < "$out/checks.txt" | tr -d ' ')" "$(wc -l < "$out/run.txt" | tr -d ' ')" \
        "$(wc -l < "$out/reused.tsv" | tr -d ' ')" "$(wc -l < "$out/skipped.tsv" | tr -d ' ')" \
        "$( [ "${RIME_CI_FRESH:-0}" = 1 ] && echo '; FRESH run: nothing reused')"
    return "$rc"
}

# prior_pass CHECK PLANDIR: "outPath<TAB>run" when the results cache (RIME_RESULTS) holds this check's output as
# evaluated now, with its manifest line; nothing otherwise. Identity is the output path, which Nix derives from the
# derivation and so from every input: same path = same inputs. Proof is `nix path-info` against the restored
# cache, never a log or a time. Also records CHECK, drvPath, outPath in PLANDIR/outs.tsv.
prior_pass() {
    local c=$1 out=$2 line drv outp m mc mout mrun
    [ -n "${RIME_CHECK_DRVS:-}" ] && [ -f "$RIME_CHECK_DRVS" ] || return 0
    line=$(awk -F'\t' -v a="checks.$SYSTEM.$c.drvPath" '$1 == a' "$RIME_CHECK_DRVS")
    drv=$(printf '%s' "$line" | cut -f2)
    outp=$(printf '%s' "$line" | cut -f3)
    [ -n "$drv" ] || return 0
    [ -n "$outp" ] || outp=$(nix-store -q --outputs "$drv" 2> /dev/null | head -1)
    [ -n "$outp" ] || return 0
    printf '%s\t%s\t%s\n' "$c" "$drv" "$outp" >> "$out/outs.tsv"
    [ -n "${RIME_RESULTS:-}" ] || return 0
    m=$RIME_RESULTS/rime-results/$c.tsv
    [ -f "$m" ] || return 0
    IFS=$'\t' read -r mc mout mrun _ < "$m"
    [ "$mc" = "$c" ] && [ "$mout" = "$outp" ] || return 0
    nix path-info --store "file://$RIME_RESULTS" "$outp" > /dev/null 2>&1 || return 0
    printf '%s\t%s\n' "$outp" "${mrun:-?}"
}

# classify LOG: exit 0 when the log shows a substitute that could not be downloaded AND no builder that failed;
# exit 1 otherwise (a real failure, an evaluation error, or nothing recognisable: never retried).
# Only nix's own lines count (column 0, or its indented "Reason:"); a build's -L output is prefixed "name> ".
classify() {
    local log=$1
    [ -f "$log" ] || die "classify: no log at $log"
    if grep -E "^error: builder for '|^[[:space:]]+Reason: builder failed|^error: .* failed with exit code" "$log" > /dev/null; then
        echo "run-checks: a builder failed: not a download failure"
        return 1
    fi
    if grep -E "^error: unable to download '[^']*': HTTP error|^error: some substitutes for the outputs of derivation '[^']*' failed|^error: path '[^']*' is required, but there is no substituter that can build it" "$log" > /dev/null; then
        echo "run-checks: a substitute could not be downloaded: $(grep -m1 -E '^error: unable to download|^error: some substitutes' "$log" | cut -c1-200)"
        return 0
    fi
    echo "run-checks: no download failure in the log"
    return 1
}

# The dev cache as an extra substituter. `trusted=1`: its paths carry no signature (they were built by this
# workflow's packages job and never leave it); Nix still checks every NAR against its narinfo's hash. Passing a
# substituter needs a trusted user (install-nix-action makes the runner one). Never used by a release workflow
# (nix/ci/check-cache-isolation.py).
dev_cache_args() {
    local d=${RIME_DEV_CACHE:-}
    if [ -n "$d" ] && [ -f "$d/nix-cache-info" ]; then
        printf '%s\n' --option extra-substituters "file://$d?trusted=1"
    fi
}

# stdin -> stdout, LOG (as is: classify reads nix's lines at column 0) and TLOG ("<epoch seconds> <line>").
stamp_tee() {
    local log=$1 tlog=$2 line lf tf
    exec {lf}> "$log" {tf}> "$tlog"
    while IFS= read -r line || [ -n "$line" ]; do
        printf '%s\n' "$line"
        printf '%s\n' "$line" >&"$lf"
        printf '%s %s\n' "${EPOCHREALTIME:-$(date +%s)}" "$line" >&"$tf"
    done
    exec {lf}>&- {tf}>&-
}

build_once() { # CHECK LIMIT LOG: one nix build, its output to the terminal and LOG; returns nix's status
    local check=$1 limit=$2 log=$3
    local -a extra
    mapfile -t extra < <(dev_cache_args)
    set +e
    if [ "$limit" -gt 0 ]; then
        timeout --kill-after=60 "$limit" nix build "$FLAKE#checks.$SYSTEM.$check" -L --print-out-paths --no-link "${extra[@]}" 2>&1 | stamp_tee "$log" "${log%.log}.tlog"
    else
        nix build "$FLAKE#checks.$SYSTEM.$check" -L --print-out-paths --no-link "${extra[@]}" 2>&1 | stamp_tee "$log" "${log%.log}.tlog"
    fi
    local rc=${PIPESTATUS[0]}
    set -e
    return "$rc"
}

# The leg's metrics (nix/ci/cihelp.py metrics): eval, substitution, local builds, the VM test. Never fails a build.
write_metrics() { # CHECK OUTDIR
    local check=$1 out=$2 status secs _
    IFS=$'\t' read -r status secs _ < "$out/$check.result"
    if command -v python3 > /dev/null; then
        python3 "$CI_DIR/cihelp.py" metrics "$check" "$out/$check.tlog" "$status" "$secs" "${RIME_DEV_CACHE_STATE:-none}" \
            > "$out/$check.metrics.json" 2> "$out/$check.metrics.err" || echo "run-checks: no metrics for $check (see $check.metrics.err)" >&2
    else
        printf '{"check": "%s", "status": "%s", "seconds": %s, "dev_cache": "%s", "reused": false}\n' \
            "$check" "$status" "$secs" "${RIME_DEV_CACHE_STATE:-none}" > "$out/$check.metrics.json"
    fi
}

build() {
    local check=$1 out=$2 limit=${RIME_CHECK_TIMEOUT:-0} start rc secs left note=
    mkdir -p "$out"
    start=$(date +%s)
    export RIME_BUILD_STARTED=${EPOCHREALTIME:-$start}
    rc=0; build_once "$check" "$limit" "$out/$check.log" || rc=$?
    secs=$(( $(date +%s) - start ))
    if [ "$rc" != 0 ] && ! { [ "$limit" -gt 0 ] && [ "$secs" -ge "$limit" ]; } && classify "$out/$check.log" >&2; then
        left=0
        [ "$limit" -gt 0 ] && left=$(( limit - secs ))
        if [ "$limit" = 0 ] || [ "$left" -ge 60 ]; then
            echo "run-checks: $check failed on a download; retrying ONCE" >&2
            mv "$out/$check.log" "$out/$check.first.log"
            mv "$out/$check.tlog" "$out/$check.first.tlog"
            rc=0; build_once "$check" "$left" "$out/$check.log" || rc=$?
            secs=$(( $(date +%s) - start ))
            note=retried
        fi
    fi
    if [ "$rc" = 0 ]; then
        printf 'pass\t%s\t%s\n' "$secs" "$note" > "$out/$check.result"
        # The output nix printed (--print-out-paths, last store path), for export-result.
        grep -E '^/nix/store/[a-z0-9]{32}-' "$out/$check.log" | tail -1 > "$out/$check.out" || true
    elif [ "$limit" -gt 0 ] && { [ "$rc" = 124 ] || [ "$rc" = 137 ]; } && [ "$secs" -ge "$limit" ]; then
        echo "run-checks: $check did not finish in $limit s; stopped" >&2
        printf 'timeout\t%s\t%s\n' "$secs" "$note" > "$out/$check.result"
    else
        printf 'fail\t%s\t%s\n' "$secs" "$note" > "$out/$check.result"
    fi
    write_metrics "$check" "$out"
    return "$rc"
}

summary() {
    local plan=$1 res=$2 rc=0 c status secs reason note prior flake n_ran=0 n_reused=0 n_skipped=0 n_bad=0
    [ -s "$plan/checks.txt" ] || die "no plan in $plan"
    echo "| flake check | CI | result | why |"
    echo "|---|---|---|---|"
    while IFS= read -r c; do
        reason=$(awk -F'\t' -v n="$c" '$1 == n { print $2 }' "$plan/skipped.tsv" 2>/dev/null || true)
        if [ -n "$reason" ]; then
            echo "| \`$c\` | SKIPPED | not built | $reason |"
            n_skipped=$((n_skipped + 1))
            continue
        fi
        note=
        if [ -f "$res/$c.result" ]; then
            IFS=$'\t' read -r status secs note < "$res/$c.result"
        elif [ -f "$plan/reused/$c.result" ]; then
            IFS=$'\t' read -r status secs note < "$plan/reused/$c.result"
        else
            status=missing secs=
        fi
        # A check that fails now on inputs that passed before is a flake (or a runner problem), and says so.
        prior=$(awk -F'\t' -v n="$c" '$1 == n { print $3 }' "$plan/prior.tsv" 2>/dev/null || true)
        flake=
        [ -n "$prior" ] && flake="**flake signal**: the same inputs (same output path) PASSED in run $prior"
        case "$status" in
            pass) n_ran=$((n_ran + 1))
                  if [ "$note" = retried ]; then echo "| \`$c\` | RAN | pass (${secs} s) | after ONE retry: the first attempt failed on a download |"
                  else echo "| \`$c\` | RAN | pass (${secs} s) | |"; fi ;;
            # Not a pass of this run: nothing was built. Its own outcome, never counted or printed as pass.
            reused) n_reused=$((n_reused + 1))
                  echo "| \`$c\` | REUSED | PASSED EARLIER, same inputs, run ${note:-?} | not built in this run: its output path is in the results cache (nix/ci/README.md) |" ;;
            fail) n_ran=$((n_ran + 1)); n_bad=$((n_bad + 1)); echo "| \`$c\` | RAN | **FAIL** (${secs} s) | $flake |"; rc=1 ;;
            timeout) n_ran=$((n_ran + 1)); n_bad=$((n_bad + 1)); echo "| \`$c\` | RAN | **TIMED OUT** (stopped after ${secs} s) | RIME_CHECK_TIMEOUT in nix.yml${flake:+; $flake} |"; rc=1 ;;
            setup) n_bad=$((n_bad + 1)); echo "| \`$c\` | NOT RUN | **FAIL** before the build | the job's setup failed (disk, Nix or KVM) |"; rc=1 ;;
            *)    n_bad=$((n_bad + 1)); echo "| \`$c\` | RAN | **NO RESULT** | the job did not report (cancelled, timed out or never started) |"; rc=1 ;;
        esac
    done < "$plan/checks.txt"
    echo
    echo "$n_ran built in this run, $n_reused REUSED (passed earlier on the same inputs), $n_skipped skipped, $n_bad not passing."
    return "$rc"
}

status_of() { # RESDIR CHECK -> the first field of its result file, or nothing
    local f="$1/$2.result" st _
    [ -f "$f" ] || return 0
    IFS=$'\t' read -r st _ < "$f"
    printf '%s' "$st"
}

new_cache() { # DIR: an empty file:// binary cache
    mkdir -p "$1/nar" "$1/rime-results"
    [ -f "$1/nix-cache-info" ] || printf 'StoreDir: /nix/store\nWantMassQuery: 0\nPriority: 30\n' > "$1/nix-cache-info"
}

# export-result CHECK RESDIR CACHE: keep a PASSED check's output (that one path, not its closure: path-info only
# needs its narinfo) and the manifest line saying which run passed it. A check that did not pass is never kept.
export_result() {
    local c=$1 res=$2 cache=$3 st outp
    st=$(status_of "$res" "$c")
    if [ "$st" != pass ]; then
        echo "run-checks: export-result: $c did not pass (result: ${st:-none}); not kept" >&2
        return 1
    fi
    outp=$(cat "$res/$c.out" 2> /dev/null || true)
    case "$outp" in /nix/store/*) ;; *) echo "run-checks: export-result: no output path for $c" >&2; return 1 ;; esac
    new_cache "$cache"
    python3 "$CI_DIR/cihelp.py" export "$cache" "$outp"
    printf '%s\t%s\t%s\t%s\t%s\n' "$c" "$outp" "${RIME_RUN:-local}" "${GITHUB_SHA:-?}" "$(date -u +%FT%TZ)" \
        > "$cache/rime-results/$c.tsv"
    echo "run-checks: kept $c ($outp) as passed in run ${RIME_RUN:-local}"
}

# results-merge PLANDIR RESDIR INCOMING CACHE: CACHE starts as the restored results cache. A prior pass whose check
# failed or timed out in this run is dropped (its inputs are no longer trusted to pass). Each kept pass in INCOMING
# is added only if RESDIR says pass for it. Prints what it saved, refused and dropped.
results_merge() {
    local plan=$1 res=$2 in=$3 cache=$4 saved=0 refused=0 dropped=0 c outp prun st m mc mout ni url h want
    new_cache "$cache"
    if [ -f "$plan/prior.tsv" ]; then
        while IFS=$'\t' read -r c outp prun; do
            st=$(status_of "$res" "$c")
            case "$st" in
                fail|timeout)
                    if [ -f "$cache/rime-results/$c.tsv" ] && [ "$(cut -f2 "$cache/rime-results/$c.tsv")" = "$outp" ]; then
                        rm -f "$cache/rime-results/$c.tsv"
                        echo "DROPPED $c: passed in run $prun, $st now on the same inputs; it will be built again"
                        dropped=$((dropped + 1))
                    fi ;;
            esac
        done < "$plan/prior.tsv"
    fi
    for m in "$in"/rime-results/*.tsv; do
        [ -f "$m" ] || continue
        c=$(basename "$m" .tsv)
        IFS=$'\t' read -r mc mout _ < "$m"
        st=$(status_of "$res" "$c")
        if [ "$mc" != "$c" ] || [ "$st" != pass ]; then
            echo "REFUSED $c: its result in this run is '${st:-none}', not pass; not saved"
            refused=$((refused + 1))
            continue
        fi
        # The kept output must be the one the plan evaluated for this check (when the plan knows it).
        want=$(awk -F'\t' -v n="$c" '$1 == n { print $3 }' "$plan/outs.tsv" 2> /dev/null || true)
        if [ -n "$want" ] && [ "$want" != "$mout" ]; then
            echo "REFUSED $c: kept $mout, but the plan evaluated $want; not saved"
            refused=$((refused + 1))
            continue
        fi
        h=${mout#/nix/store/}; h=${h:0:32}
        ni=$in/$h.narinfo
        url=$(sed -n 's/^URL: //p' "$ni" 2> /dev/null)
        if [ -z "$url" ] || [ ! -f "$in/$url" ]; then
            echo "REFUSED $c: no narinfo/NAR for $mout"
            refused=$((refused + 1))
            continue
        fi
        mkdir -p "$cache/$(dirname "$url")"
        cp "$in/$url" "$cache/$url"
        cp "$ni" "$cache/$h.narinfo"
        cp "$m" "$cache/rime-results/$c.tsv"
        saved=$((saved + 1))
    done
    echo "results-merge: $saved saved, $refused refused, $dropped dropped; $(find "$cache/rime-results" -name '*.tsv' | wc -l | tr -d ' ') passes in the cache"
}

# fresh: is this a FRESH run (nothing reused: every planned check built, the dev cache not restored)? Prints
# "1 <why>" or "0". Asked for, never guessed: a branch named nix/fresh-*, the trailer "Rime-CI: fresh" in the head
# commit's message, or workflow_dispatch's `fresh` input. Env: RIME_REF (branch), RIME_COMMIT_MSG, RIME_FRESH_INPUT.
fresh_mode() {
    case "${RIME_REF:-}" in
        nix/fresh-*) echo "1 branch ${RIME_REF}"; return ;;
    esac
    if [ "${RIME_FRESH_INPUT:-false}" = true ]; then echo "1 workflow_dispatch input fresh"; return; fi
    if printf '%s\n' "${RIME_COMMIT_MSG:-}" | git interpret-trailers --parse 2> /dev/null | grep -ixE 'Rime-CI: *fresh' > /dev/null; then
        echo "1 commit trailer Rime-CI: fresh"; return
    fi
    echo 0
}

# ---- build once, reuse everywhere: the packages job --------------------------------------------------------------

prebuild_plan() {
    local out=$1 rc=0 name attr pick rest apply drv outs
    mkdir -p "$out"
    : > "$out/items.tsv"
    [ -f "$PREBUILD_FILE" ] || die "no prebuild list at $PREBUILD_FILE"
    while read -r name attr pick rest; do
        case "$name" in '' | '#'*) continue ;; esac
        if [ -z "$attr" ] || [ -n "$rest" ]; then echo "run-checks: $PREBUILD_FILE: bad line for '$name'" >&2; rc=1; continue; fi
        if cut -f1 "$out/items.tsv" | grep -qxF "$name"; then echo "run-checks: $PREBUILD_FILE: '$name' listed twice" >&2; rc=1; continue; fi
        if [ -n "$pick" ]; then
            apply="ps: let m = builtins.filter (p: (p.pname or (builtins.parseDrvName p.name).name) == \"$pick\") ps; in
                   if builtins.length m == 1 then (builtins.head m).drvPath
                   else throw \"prebuild: \${toString (builtins.length m)} packages named $pick in $attr\""
        else
            apply="x: x.drvPath"
        fi
        if ! drv=$(nix eval --raw "$FLAKE#$attr" --apply "$apply"); then
            echo "run-checks: prebuild item $name ($attr) does not evaluate" >&2; rc=1; continue
        fi
        # Not a -debug output: no check's closure holds one (cosmic-initial-setup's is 133 MB of the 337 MB).
        outs=$(nix-store -q --outputs "$drv" | grep -v -- '-debug$' | paste -sd, -)
        printf '%s\t%s\t%s\n' "$name" "$drv" "$outs" >> "$out/items.tsv"
        echo "ITEM $name $drv"
    done < "$PREBUILD_FILE"
    [ -s "$out/items.tsv" ] || die "no prebuild items"
    # The cache key names every item's derivation: any input of any item changes it.
    echo "rime-dev-pkgs-v1-$(cut -f1,2 "$out/items.tsv" | sort | sha256sum | cut -c1-32)" > "$out/key"
    echo "prebuild-plan: $(wc -l < "$out/items.tsv" | tr -d ' ') items, key $(cat "$out/key")"
    return "$rc"
}

in_dev_cache() { # OUTS (comma-separated): every output's narinfo is in RIME_DEV_CACHE
    local d=${RIME_DEV_CACHE:-} o
    [ -n "$d" ] && [ -f "$d/nix-cache-info" ] || return 1
    for o in ${1//,/ }; do nix path-info --store "file://$d" "$o" > /dev/null 2>&1 || return 1; done
}

meta_run() { # NAME DRV: the run that built NAME's DRV, from the dev cache's meta
    local f=${RIME_DEV_CACHE:-/nonexistent}/rime-meta/$1.tsv d r
    [ -f "$f" ] || { echo "an earlier run"; return; }
    IFS=$'\t' read -r _ d r < "$f"
    if [ "$d" = "$2" ] && [ -n "$r" ]; then echo "run $r"; else echo "an earlier run"; fi
}

# prebuild DIR: realise the items. An item whose outputs the dev cache already holds is REUSED (substituted, never
# compiled or tested again: an earlier run on identical inputs did both); the others are BUILT here. With
# RIME_PREBUILD_ALL=0 (an exact cache hit: nothing to export) only rimed is realised, for its test report.
prebuild() {
    local dir=$1 all=${RIME_PREBUILD_ALL:-1} name drv outs state rc=0 o
    local -a todo=() extra
    mapfile -t extra < <(dev_cache_args)
    : > "$dir/status.tsv"
    while IFS=$'\t' read -r name drv outs; do
        state=BUILT
        in_dev_cache "$outs" && state=REUSED
        printf '%s\t%s\t%s\t%s\n' "$name" "$state" "$drv" "$outs" >> "$dir/status.tsv"
        if [ "$state" = BUILT ] || [ "$all" = 1 ] || [ "$name" = rimed ]; then todo+=("$drv^*"); fi
    done < "$dir/items.tsv"
    echo "prebuild: realising ${#todo[@]} of $(wc -l < "$dir/items.tsv" | tr -d ' ') items (dev cache: ${RIME_DEV_CACHE:-none})"
    if [ "${#todo[@]}" -gt 0 ]; then
        nix build -L --no-link --print-out-paths "${extra[@]}" "${todo[@]}" || rc=$?
    fi
    : > "$dir/status.new"
    while IFS=$'\t' read -r name state drv outs; do
        if [ "$state" = REUSED ]; then
            # Substituted, not built. If the dev cache was not trusted, Nix would have compiled it instead: say so.
            for o in ${outs//,/ }; do
                if nix path-info --json-format 1 --json "$o" 2> /dev/null | grep -F '"ultimate":true' > /dev/null; then
                    echo "::warning::$name was in the dev cache but was BUILT here (was the dev cache trusted?)"
                    state=BUILT
                fi
            done
        fi
        printf '%s\t%s\t%s\t%s\n' "$name" "$state" "$drv" "$outs" >> "$dir/status.new"
        if [ "$state" = REUSED ]; then echo "REUSED $name: built and tested by $(meta_run "$name" "$drv") on identical inputs ($drv)"
        elif [ "$rc" = 0 ]; then echo "BUILT  $name: in this job ($drv)"
        else echo "FAILED $name? (the build failed; see above) ($drv)"; fi
    done < "$dir/status.tsv"
    mv "$dir/status.new" "$dir/status.tsv"
    return "$rc"
}

# prebuild-report DIR: rimed's cargo test report as markdown, saying whether it ran in this job or is REUSED.
prebuild_report() {
    local dir=$1 name state drv outs o report
    IFS=$'\t' read -r name state drv outs < <(awk -F'\t' '$1 == "rimed"' "$dir/status.tsv")
    o=${outs%%,*}
    report=$o/share/doc/rimed/cargo-tests.txt
    if [ -z "$o" ] || [ ! -f "$report" ]; then
        echo "::error::no cargo test report: rimed did not build"
        return 1
    fi
    if [ "$state" = REUSED ]; then
        echo "### rimed: cargo test --workspace --no-fail-fast: REUSED, not run in this job"
        echo
        echo "rimed ($drv) was built and its cargo test passed in $(meta_run rimed "$drv") on identical inputs; this job took"
        echo "that output from the dev cache. The report below is that run's."
    else
        echo "### rimed: cargo test --workspace --no-fail-fast (ran in this job)"
    fi
    echo
    echo '```'
    cat "$report"
    echo '```'
}

# dev-cache-export DIR CACHE: every item's runtime closure, only the paths no upstream key signed (built here, or
# taken from the previous dev cache), as a fresh file:// cache; plus which run built each item.
dev_cache_export() {
    local dir=$1 cache=$2 name state drv outs o run
    local -a all=()
    while IFS=$'\t' read -r name state drv outs; do
        for o in ${outs//,/ }; do
            nix path-info "$o" > /dev/null 2>&1 || die "dev-cache-export: $name's $o is not in the store (run prebuild with RIME_PREBUILD_ALL=1)"
            all+=("$o")
        done
    done < "$dir/status.tsv"
    rm -rf "$cache"
    new_cache "$cache"
    python3 "$CI_DIR/cihelp.py" export "$cache" --closure --unsigned-only "${all[@]}"
    mkdir -p "$cache/rime-meta"
    while IFS=$'\t' read -r name state drv outs; do
        if [ "$state" = BUILT ]; then run=${RIME_RUN:-local}
        else run=$(meta_run "$name" "$drv"); case "$run" in "run "*) run=${run#run } ;; *) run= ;; esac; fi
        printf '%s\t%s\t%s\n' "$name" "$drv" "$run" > "$cache/rime-meta/$name.tsv"
    done < "$dir/status.tsv"
    echo "dev-cache-export: $(find "$cache" -name '*.narinfo' | wc -l | tr -d ' ') paths, $(du -sh "$cache" | cut -f1) in $cache"
}

cmd=${1:-}
shift || true
case "$cmd" in
    list) list_checks ;;
    classify) [ $# = 1 ] || die "usage: classify LOG"; classify "$1" ;;
    plan) [ $# = 1 ] || die "usage: plan OUTDIR"; plan "$1" ;;
    build) [ $# = 2 ] || die "usage: build CHECK OUTDIR"; build "$1" "$2" ;;
    summary) [ $# = 2 ] || die "usage: summary PLANDIR RESULTDIR"; summary "$1" "$2" ;;
    export-result) [ $# = 3 ] || die "usage: export-result CHECK RESULTDIR CACHE"; export_result "$1" "$2" "$3" ;;
    results-merge) [ $# = 4 ] || die "usage: results-merge PLANDIR RESULTDIR INCOMING CACHE"; results_merge "$1" "$2" "$3" "$4" ;;
    fresh) fresh_mode ;;
    prebuild-plan) [ $# = 1 ] || die "usage: prebuild-plan DIR"; prebuild_plan "$1" ;;
    prebuild) [ $# = 1 ] || die "usage: prebuild DIR"; prebuild "$1" ;;
    prebuild-report) [ $# = 1 ] || die "usage: prebuild-report DIR"; prebuild_report "$1" ;;
    dev-cache-export) [ $# = 2 ] || die "usage: dev-cache-export DIR CACHE"; dev_cache_export "$1" "$2" ;;
    *) die "usage: $0 list | plan OUTDIR | build CHECK OUTDIR | classify LOG | summary PLANDIR RESULTDIR | export-result CHECK RESULTDIR CACHE | results-merge PLANDIR RESULTDIR INCOMING CACHE | fresh | prebuild-plan DIR | prebuild DIR | prebuild-report DIR | dev-cache-export DIR CACHE" ;;
esac
