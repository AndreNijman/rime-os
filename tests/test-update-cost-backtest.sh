#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-update-cost-backtest.sh — the numbers in docs/update-cost.md come from
#  tools/update-cost-backtest.py and the data committed beside it.
#
#  1. The script's own fixture: delta() counts only layers the old image does
#     not hold, unpublished (branch) images never enter the sequence, and the
#     switch to the rechunked layout is reported apart.
#  2. The committed ledger reproduces the figures the document quotes, offline.
#     A change to the script or the data that moves one of them fails here,
#     so the document cannot drift from what the script prints.
#
#  Needs python3 only: `report` reads the committed TSVs and no network.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

PASS=0 FAIL=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }
# Reads its whole input: grep -q under pipefail can fail the producer.
has() { grep -F -- "$1" >/dev/null; }

TOOL=tools/update-cost-backtest.py
DATA=docs/update-cost

command -v python3 >/dev/null || { echo "FATAL: python3 not found"; exit 2; }
[ -f "$TOOL" ] || { echo "FATAL: $TOOL missing"; exit 2; }

if out=$(python3 -I "$TOOL" --self-test 2>&1); then
	pass "self-test ($(printf '%s\n' "$out" | grep -c '^PASS') fixture checks)"
else
	printf '%s\n' "$out"
	fail "self-test"
fi

report=$(python3 -I "$TOOL" report --data "$DATA" 2>&1)
rc=$?
if [ "$rc" = 0 ]; then pass "report exits 0 on the committed data"; else fail "report exited $rc"; fi

expect() { # description, literal text the report must contain
	if printf '%s\n' "$report" | has "$2"; then pass "$1"; else fail "$1 (missing: $2)"; fi
}

# The figures docs/update-cost.md quotes. Update both together.
expect "43 published transitions, 10 with a core rebuild, in 33 days" \
	"| 2026-09-29 → 2026-11-01 | 33.0 | 43 | 10 | 39.0 | 9.1 |"
expect "tiered core rebuild median 4.779 GB over 9" \
	"| core rebuild | 9 | 4.779 GB | 4.821 GB | 4.044 GB | 5.990 GB |"
expect "tiered non-core release median 0.161 GB over 33" \
	"| no core rebuild | 33 | 0.161 GB | 0.161 GB | 0.151 GB | 0.162 GB |"
expect "the rechunk switch is excluded and named" \
	"2026.10.19 → 2026.11.01: 128 of 129 layers, **4.219 GB**"
expect "rechunked non-core pair" \
	"| base + image, same core | 5 of 129 | **0.255 GB** |"
expect "rechunked core pair" \
	"| core + base + image | 50 of 129 | **1.270 GB** |"
expect "tiered history, every release, as downloaded and replayed rechunked" \
	"| every release (27.4 days) | 42 | 48.69 GB | **53.26 GB** | 21.70 GB |"
expect "tiered history, a weekly updater" \
	"| once every 7 days (27.4 days) | 4 | 19.82 GB | **21.68 GB** | 5.56 GB |"
expect "30 days at today's cadence, rechunked" \
	"| today, last 30 days | 36.0 | 9.0 | 47.37 GB | **18.31 GB** | 22.54 GB |"
expect "30 days at a monthly core cadence, rechunked" \
	"| monthly core, releases as last 30 days | 36.0 | 1.0 | 10.42 GB | **10.19 GB** | 10.66 GB |"
expect "the weekly cron's reissue is counted as a release" \
	"| 2026.10.19 (reissue) | y | core | 84 of 125 | 4.825 GB |"

# Branch builds are in the ledger (the rechunked model needs them) but must
# never be counted as something a machine downloaded.
if printf '%s\n' "$report" | grep -E '^\| [0-9]+ \|' | has "| dev |"; then
	fail "a branch build (release id dev) appears as a published transition"
else
	pass "no branch build appears as a published transition"
fi

echo
echo "update-cost-backtest: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
