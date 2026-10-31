#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-scx-selftest.sh — the sched-ext QEMU selftest's VERDICT, without QEMU.
#
#  tests/kernel/scx-selftest/run-scx-selftest.sh boots the shipped kernel and
#  runs every shipped scheduler (build-image.yml runs it before a core image is
#  pushed). Whether that gate means anything comes down to parse_log: the part
#  that turns a serial console into PASS or FAIL. A parser that let a missing
#  scheduler, a FAIL line or a guest that never finished through would make the
#  gate pass on a broken kernel, so this feeds it the REAL serial log of a
#  passing run (the published 7.2.9 image, 16 schedulers + kill -9) and that
#  log broken one way at a time, and asserts each break fails.
#
#  It also pins the guest's ops_matches (init.sh) to rimed's scx_ops_matches
#  cases: the selftest proving "every scheduler attaches under the name rimed
#  expects" is only worth something if the two agree on what that name is.
#
#  Run from anywhere: ./tests/test-scx-selftest.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 2

RUN=kernel/scx-selftest/run-scx-selftest.sh
INIT=kernel/scx-selftest/init.sh
LOG=kernel/scx-selftest/fixtures/pass-7.2.9-serial.log
LIST=kernel/scx-selftest/fixtures/pass-7.2.9-schedulers.txt
for f in "$RUN" "$INIT" "$LOG" "$LIST"; do [ -f "$f" ] || { echo "cannot find $f"; exit 2; }; done

T=$(mktemp -d /tmp/rime-scx-selftest-test.XXXXXX)
trap 'rm -rf "$T"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

# parse LOG LIST → sets $out and $rc
parse() { out="$(bash "$RUN" --parse-only "$1" --expect "$2" 2>&1)"; rc=$?; }

expect_pass() {
    local name="$1" log="$2" list="${3:-$LIST}"
    parse "$log" "$list"
    if [ "$rc" = 0 ] && grep -F "VERDICT: PASS" <<<"$out" >/dev/null; then ok "$name"
    else bad "$name" "rc=$rc: $(tail -n 3 <<<"$out" | tr '\n' ' ')"; fi
}
# expect_fail NAME LOG WANT-TEXT [LIST]
expect_fail() {
    local name="$1" log="$2" want="$3" list="${4:-$LIST}"
    parse "$log" "$list"
    if [ "$rc" = 1 ] && grep -F "VERDICT: FAIL" <<<"$out" >/dev/null \
        && grep -F -- "$want" <<<"$out" >/dev/null; then ok "$name"
    else bad "$name" "rc=$rc, wanted '$want': $(tr '\n' ' ' <<<"$out" | cut -c1-300)"; fi
}
# mutate NAME SED-EXPR → $T/NAME.log
mutate() { sed -E "$2" "$LOG" > "$T/$1.log"; }

# ── 1. the real passing run passes, with every scheduler named ─────────────
expect_pass "the real 7.2.9 run passes" "$LOG"
n=$(grep -c 'PASS: sched=' <<<"$out")
if [ "$n" = 16 ]; then ok "all 16 schedulers are reported"; else bad "all 16 schedulers are reported" "got $n"; fi
# A serial console ends lines CRLF; the parser must not care.
sed 's/$/\r/' "$LOG" > "$T/crlf.log"
expect_pass "CRLF line endings still pass" "$T/crlf.log"

# ── 2. each way a run can go wrong fails ────────────────────────────────────
mutate one-fail 's/sched=scx_lavd result=PASS/sched=scx_lavd result=FAIL reason=kmsg_during_load:runnable_task_stall/'
expect_fail "a FAIL line fails the run" "$T/one-fail.log" "FAIL: sched=scx_lavd result=FAIL"

mutate missing '/sched=scx_cosmos /d'
expect_fail "a scheduler with no result line fails" "$T/missing.log" "scx_cosmos: no result line"

mutate twice '/sched=scx_flash /p'
expect_fail "a scheduler reported twice fails" "$T/twice.log" "scx_flash: more than one result line"

mutate no-verdict '/verdict=/d'
expect_fail "a guest that never printed a verdict fails" "$T/no-verdict.log" "no verdict line"

mutate verdict-fail 's/verdict=PASS passed=17 failed=0/verdict=FAIL passed=16 failed=1/'
expect_fail "a guest verdict of FAIL fails" "$T/verdict-fail.log" "verdict=FAIL"

mutate wrong-total 's/total=17/total=9/'
expect_fail "a guest that counted the wrong number of checks fails" "$T/wrong-total.log" "guest counted 9 checks, expected 17"

mutate kill9-fail 's/kill9=scx_lavd result=PASS/kill9=scx_lavd result=FAIL reason=still_on_ext_after_kill/'
expect_fail "a failed kill -9 fallback fails" "$T/kill9-fail.log" "kill9=scx_lavd result=FAIL"

mutate kill9-missing '/kill9=/d'
expect_fail "a missing kill -9 result fails" "$T/kill9-missing.log" "kill -9 fallback: no result line"

grep -v '^scx_tickless$' "$LIST" > "$T/short.txt"
expect_fail "a scheduler the image does not list fails" "$LOG" "guest tested scx_tickless, which is not in the expected list" "$T/short.txt"

{ cat "$LIST"; echo scx_newsched; } > "$T/long.txt"
expect_fail "a listed scheduler the guest never ran fails" "$LOG" "scx_newsched: no result line" "$T/long.txt"

: > "$T/empty.log"
expect_fail "an empty serial log fails" "$T/empty.log" "no verdict line"

# ── 3. the guest's ops check agrees with rimed's ────────────────────────────
# The same cases as rimed-core's scx_ops_matches tests
# (the_struct_ops_name_drops_the_scx_prefix…, the_build_tag_the_rust_schedulers…).
eval "$(sed -n '/^ops_matches()/,/^}/p' "$INIT")"
check_ops() {
    local ops="$1" sched="$2" want="$3" got=no
    ops_matches "$ops" "$sched" && got=yes
    if [ "$got" = "$want" ]; then ok "guest ops_matches '$ops' '$sched' = $want"
    else bad "guest ops_matches '$ops' '$sched'" "got $got, rimed says $want"; fi
}
check_ops lavd scx_lavd yes
check_ops scx_lavd scx_lavd yes
check_ops lavd_1.1.3_x86_64_unknown_linux_gnu scx_lavd yes
check_ops scx_flow_3.1.2_x86_64_unknown_linux_gnu scx_flow yes
check_ops rusty scx_lavd no
check_ops lavdx scx_lavd no
check_ops lavd_extra scx_lavd no

echo
echo "scx-selftest parser: $pass passed, $fail failed"
[ "$fail" = 0 ]
