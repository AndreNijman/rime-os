#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-fstrim.sh — the weekly TRIM wrapper (files/system/libexec/rime-fstrim)
#  and the fstrim.service drop-in that runs it.
#
#  Stock fstrim.service stayed FAILED for a week whenever system sleep cut into
#  the trim: btrfs returns -ERESTARTSYS once the freezer runs, and with no signal
#  yet pending the value leaks out as "FITRIM ioctl failed: Unknown error 512"
#  (fstrim exit 32). And with an /etc/fstab present it trimmed only what fstab
#  lists, which on a bootc machine never includes the root filesystem.
#
#  Asserted, both directions, against the SHIPPED wrapper with fstrim stubbed
#  (the stub plays back a script of answers and records every call):
#    • every mounted filesystem is listed (mountinfo), with or without fstab;
#      an fstab X-fstrim.notrim line keeps the stock fstab-first order;
#    • a filesystem interrupted with 512 or EINTR is trimmed again, alone, and
#      the run succeeds; one that stays interrupted fails the run;
#    • any other error is NOT retried and keeps fstrim's status, even next to
#      an interrupted filesystem that a retry fixed;
#    • the drop-in really replaces ExecStart, and the image installs both files.
#
#  Nothing is trimmed: no real fstrim, no block device, no root.
#  Run from anywhere: ./tests/test-rime-fstrim.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2

WRAP=../files/system/libexec/rime-fstrim
DROPIN=../files/system/units/10-rime-fstrim.conf
[ -f "$WRAP" ] || { echo "cannot find $WRAP"; exit 2; }
[ -f "$DROPIN" ] || { echo "cannot find $DROPIN"; exit 2; }

T=$(mktemp -d /tmp/rime-fstrim-test.XXXXXX)
trap 'rm -rf "$T"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

# The stub: each call appends "LC_ALL=<v> <argv>" to calls and plays the next
# answer from plan ("<rc>|<output, \n-separated>"); past the end it succeeds.
cat > "$T/fstrim" <<'STUB'
#!/bin/bash
d="$(dirname "$0")"
printf 'LC_ALL=%s %s\n' "${LC_ALL:-}" "$*" >> "$d/calls"
n=$(( $(wc -l < "$d/calls") ))
line="$(sed -n "${n}p" "$d/plan" 2>/dev/null)"
[ -n "$line" ] || exit 0
rc="${line%%|*}"; out="${line#*|}"
[ -n "$out" ] && printf '%b\n' "$out" >&2
exit "$rc"
STUB
chmod +x "$T/fstrim"

# run CASE FSTAB-CONTENT|- PLAN-LINES...: run the wrapper, leave rc/out/calls behind
run_case() {
    local name="$1" fstab="$2"; shift 2
    rm -f "$T/calls" "$T/plan" "$T/fstab"; : > "$T/calls"
    [ "$fstab" = "-" ] || printf '%b' "$fstab" > "$T/fstab"
    for l in "$@"; do printf '%s\n' "$l" >> "$T/plan"; done
    out="$(RIME_FSTRIM_BIN="$T/fstrim" RIME_FSTRIM_FSTAB="$T/fstab" RIME_FSTRIM_DELAY=0 \
           RIME_FSTRIM_TRIES=3 LC_ALL=de_DE.UTF-8 bash "$WRAP" 2>&1)"
    rc=$?
    calls="$(cat "$T/calls")"
    ncalls=$(wc -l < "$T/calls")
    CASE="$name"
}
expect_rc()    { [ "$rc" -eq "$1" ] && ok "$CASE: exit $1" || bad "$CASE: exit $1" "got $rc; out: $out"; }
expect_calls() { [ "$ncalls" -eq "$1" ] && ok "$CASE: $1 fstrim call(s)" || bad "$CASE: $1 fstrim call(s)" "got $ncalls: $calls"; }
expect_call()  { # N exact-argv
    local got; got="$(printf '%s\n' "$calls" | sed -n "${1}p")"
    [ "$got" = "LC_ALL=C $2" ] && ok "$CASE: call $1 is '$2'" || bad "$CASE: call $1 is '$2'" "got '$got'"
}

ALL='--listed-in /proc/self/mountinfo --verbose --quiet-unsupported'

# ── what gets listed ─────────────────────────────────────────────────────────
run_case "no fstab (plain install)" - "0|/etc: 50.3 GiB (54015447040 bytes) trimmed on /dev/nvme0n1p3"
expect_rc 0; expect_calls 1; expect_call 1 "$ALL"
case "$out" in *"50.3 GiB"*) ok "$CASE: fstrim's report is passed through" ;; *) bad "$CASE: report passed through" "$out" ;; esac

run_case "fstab with only /boot ro (LUKS install)" 'UUID=abc /boot auto ro 0 0\n' "0|"
expect_rc 0; expect_call 1 "$ALL"

run_case "fstab with /boot/efi (beside Windows)" \
    'UUID=abc /boot auto ro 0 0\nPARTUUID=d /boot/efi vfat umask=0077,nofail 0 2\n' "0|"
expect_call 1 "$ALL"

run_case "fstab with X-fstrim.notrim keeps the stock order" \
    'UUID=abc /data btrfs defaults,X-fstrim.notrim 0 0\n' "0|"
expect_call 1 "--listed-in $T/fstab:/proc/self/mountinfo --verbose --quiet-unsupported"

run_case "a commented-out X-fstrim.notrim does not count" \
    '# UUID=abc /data btrfs defaults,X-fstrim.notrim 0 0\n' "0|"
expect_call 1 "$ALL"

# ── interrupted by system sleep ──────────────────────────────────────────────
run_case "512 once, then the retry trims" - \
    "32|fstrim: /etc: FITRIM ioctl failed: Unknown error 512" \
    "0|/etc: 1 GiB (1073741824 bytes) trimmed on /dev/nvme0n1p3"
expect_rc 0; expect_calls 2; expect_call 2 "--verbose --quiet-unsupported /etc"

run_case "EINTR once, then the retry trims" - \
    "32|fstrim: /etc: FITRIM ioctl failed: Interrupted system call" "0|"
expect_rc 0; expect_calls 2

run_case "a mount point with spaces is retried whole" - \
    "64|/etc: 1 GiB trimmed\nfstrim: /run/media/a b: FITRIM ioctl failed: Unknown error 512" "0|"
expect_rc 0; expect_call 2 "--verbose --quiet-unsupported /run/media/a b"

run_case "512 every time fails the run after 3 tries" - \
    "32|fstrim: /etc: FITRIM ioctl failed: Unknown error 512" \
    "32|fstrim: /etc: FITRIM ioctl failed: Unknown error 512" \
    "32|fstrim: /etc: FITRIM ioctl failed: Unknown error 512" "0|"
expect_rc 32; expect_calls 3
case "$out" in *"not trimmed: /etc"*) ok "$CASE: says what was not trimmed" ;; *) bad "$CASE: says what was not trimmed" "$out" ;; esac

run_case "a retry that fails differently is not retried again" - \
    "32|fstrim: /etc: FITRIM ioctl failed: Unknown error 512" \
    "32|fstrim: /etc: FITRIM ioctl failed: Input/output error" "0|"
expect_rc 32; expect_calls 2

# ── real errors stay errors ──────────────────────────────────────────────────
run_case "an I/O error is not retried" - "32|fstrim: /etc: FITRIM ioctl failed: Input/output error"
expect_rc 32; expect_calls 1

run_case "I/O error next to a fixed interruption keeps 64" - \
    "64|fstrim: /etc: FITRIM ioctl failed: Unknown error 512\nfstrim: /data: FITRIM ioctl failed: Input/output error" "0|"
expect_rc 64; expect_calls 2; expect_call 2 "--verbose --quiet-unsupported /etc"

run_case "a non-trim failure passes through" - "1|fstrim: failed to parse /proc/self/mountinfo"
expect_rc 1; expect_calls 1

# ── the unit and the image ───────────────────────────────────────────────────
mapfile -t execs < <(grep -E '^ExecStart=' "$DROPIN")
if [ "${#execs[@]}" -eq 2 ] && [ "${execs[0]}" = "ExecStart=" ] && [ "${execs[1]}" = "ExecStart=/usr/libexec/rime-fstrim" ]; then
    ok "drop-in resets ExecStart and runs the wrapper"
else
    bad "drop-in resets ExecStart and runs the wrapper" "${execs[*]}"
fi
grep -E '^\[Service\]$' "$DROPIN" >/dev/null && ok "drop-in is a [Service] section" || bad "drop-in is a [Service] section" ""
CF=../Containerfile.base
if grep -A1 -F 'files/system/libexec/rime-fstrim' "$CF" | grep -F '/usr/libexec/rime-fstrim' >/dev/null \
   && grep -A1 -F 'files/system/units/10-rime-fstrim.conf' "$CF" \
      | grep -F '/usr/lib/systemd/system/fstrim.service.d/10-rime-fstrim.conf' >/dev/null \
   && grep -B1 -F 'files/system/libexec/rime-fstrim' "$CF" | grep -F -- '--chmod=0755' >/dev/null; then
    ok "Containerfile.base installs the wrapper (0755) and the drop-in"
else
    bad "Containerfile.base installs the wrapper (0755) and the drop-in" ""
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
