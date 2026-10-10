#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-ci-jobs-aggregated.sh — a job nobody aggregates cannot block a merge.
#
#  `result` is the ONLY required status on a pull request: every other job
#  reports its own tick, and nothing reads those ticks. `result` reads them,
#  through `needs` — and a job left out of `needs` is a job whose failure is
#  invisible to the merge button. It still goes red in the checks list, where
#  it looks exactly like the ones that count.
#
#  That was a theoretical hole while there were five jobs. On 2026-09-22 the
#  three slowest jobs were split into fourteen shards to cut CI wall-clock from
#  14 minutes to about 4, and fourteen is a number people get wrong. Adding a
#  shard and forgetting this one line would hand back a green PR over a red
#  suite, which is strictly worse than the slow CI the split was for.
#
#  Same species as tests/check-suites-run-in-ci.sh one level up: that one says
#  a suite nobody runs is not a gate, this one says a job nobody aggregates is
#  not a gate either. Both arms fail:
#
#    a job absent from result.needs   -> fail, named
#    a needs entry naming no job      -> fail, named  (a typo'd shard name is
#                                        silently dropped by Actions, so the
#                                        shard is unaggregated AND unnamed)
#
#  Deliberately text rather than a YAML library, for the reason
#  tests/check-ci-selector-parity.sh gives: the runner image is not contracted
#  to carry PyYAML, and a gate that cannot run is worth nothing.
#
#  Usage: tests/check-ci-jobs-aggregated.sh [path/to/pr-validation.yml]
#  The argument exists so the same commit can be run against an older copy —
#  `git show 'HEAD~1:.github/workflows/pr-validation.yml'` — to show it red.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 1

WORKFLOW="${1:-.github/workflows/pr-validation.yml}"

# A missing prerequisite is a FAILURE here, never a skip.
[ -f "$WORKFLOW" ] || { echo "FATAL: no workflow file at $WORKFLOW"; exit 1; }

# The aggregator itself, and the one job whose result it cannot require.
AGGREGATOR=result

# ── every job id ─────────────────────────────────────────────────────────────
# Job ids are the only two-space-indented bare keys under `jobs:`, and `jobs:`
# is the last top-level key in this file. Anchored on both, so a restructured
# file fails loudly here rather than quietly reading nothing.
jobs_line=$(grep -n '^jobs:[[:space:]]*$' "$WORKFLOW" | cut -d: -f1)
[ -n "$jobs_line" ] || { echo "FATAL: no top-level 'jobs:' key in $WORKFLOW"; exit 1; }

mapfile -t ids < <(
    tail -n +"$jobs_line" "$WORKFLOW" \
    | sed -n 's/^  \([a-z][a-z0-9_-]*\):[[:space:]]*$/\1/p'
)
[ "${#ids[@]}" -ge 2 ] || { echo "FATAL: found ${#ids[@]} job ids; the anchors have moved"; exit 1; }

printf '%s\n' "${ids[@]}" | pipe_has -x "$AGGREGATOR" \
    || { echo "FATAL: no '$AGGREGATOR' job in $WORKFLOW"; exit 1; }

# ── what the aggregator requires ─────────────────────────────────────────────
# Both YAML spellings, because either is legal and a gate that reads only one
# is the same defect as a selector that matches one path: it reports success
# and inspects nothing.
#
#   needs: [a, b, c]
#   needs:
#     - a
#     - b
agg_line=$(grep -n "^  ${AGGREGATOR}:[[:space:]]*$" "$WORKFLOW" | cut -d: -f1)
block=$(tail -n +"$agg_line" "$WORKFLOW" | sed -n '2,/^  [a-z][a-z0-9_-]*:[[:space:]]*$/p')

flow=$(printf '%s\n' "$block" | sed -n 's/^    needs:[[:space:]]*\[\(.*\)\][[:space:]]*$/\1/p' | head -1)
if [ -n "$flow" ]; then
    mapfile -t needs < <(printf '%s\n' "$flow" | tr ',' '\n' | tr -d " '\"" | grep -v '^$')
else
    mapfile -t needs < <(
        printf '%s\n' "$block" \
        | sed -n '/^    needs:[[:space:]]*$/,/^    [a-z]/p' \
        | sed -n "s/^      -[[:space:]]*\([a-z][a-z0-9_-]*\)[[:space:]]*$/\1/p"
    )
fi
[ "${#needs[@]}" -ge 1 ] || { echo "FATAL: could not read ${AGGREGATOR}.needs"; exit 1; }

# ── both arms ────────────────────────────────────────────────────────────────
unaggregated=(); phantom=()
for j in "${ids[@]}"; do
    [ "$j" = "$AGGREGATOR" ] && continue
    printf '%s\n' "${needs[@]}" | pipe_has -x "$j" || unaggregated+=("$j")
done
for n in "${needs[@]}"; do
    printf '%s\n' "${ids[@]}" | pipe_has -x "$n" || phantom+=("$n")
done

printf '\nCI aggregation: %d jobs, %d required by %s\n' \
    "$(( ${#ids[@]} - 1 ))" "${#needs[@]}" "$AGGREGATOR"

rc=0
if [ "${#unaggregated[@]}" -gt 0 ]; then
    echo "  FAIL — these jobs are in no '${AGGREGATOR}.needs', so a failure in them blocks nothing:"
    printf '    %s\n' "${unaggregated[@]}"
    rc=1
fi
if [ "${#phantom[@]}" -gt 0 ]; then
    echo "  FAIL — '${AGGREGATOR}.needs' names jobs that do not exist (a typo is dropped silently):"
    printf '    %s\n' "${phantom[@]}"
    rc=1
fi
[ "$rc" -eq 0 ] && echo "  every job is required by $AGGREGATOR, and every requirement is a real job"
exit "$rc"
