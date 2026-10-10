#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-gpu-notice.sh — the once-per-state notice for an NVIDIA GPU the
#  shipped driver cannot drive (Maxwell, Pascal, Volta and older).
#
#  Drives files/system/libexec/rime-gpu-notice against a stub `rime` (its
#  doctor output is whatever this script writes) and a stub notify-send that
#  records its arguments. No toolchain, no session bus, no real GPU.
#
#  The failures it exists to catch:
#    1. a notice at EVERY login for a fact that never changes (muted, then
#       useless);
#    2. a notice on a machine with nothing to say (no NVIDIA, or a supported
#       one), or when doctor itself failed;
#    3. a notice that never comes back after the state changed;
#    4. the unit drifting into a system unit, where notify-send has no bus.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NOTICE="$REPO/files/system/libexec/rime-gpu-notice"
SVC="$REPO/files/system/units/rime-gpu-notice.service"
TIMER="$REPO/files/system/units/rime-gpu-notice.timer"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }

for f in "$NOTICE" "$SVC" "$TIMER"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/home" "$WORK/state"

# The stub doctor prints $WORK/doctor.out and exits with $WORK/doctor.rc.
cat > "$WORK/rime" <<EOF
#!/usr/bin/env bash
[[ "\${1:-}" == doctor ]] || exit 64
cat "$WORK/doctor.out"
exit "\$(cat "$WORK/doctor.rc")"
EOF
chmod +x "$WORK/rime"
cat > "$WORK/bin/notify-send" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$WORK/notified"
EOF
chmod +x "$WORK/bin/notify-send"

PASCAL='[WARN] NVIDIA GPU 10de:1b80 at 0000:01:00.0 is a Maxwell, Pascal or Volta GPU. No driver is loaded for it.'
OTHER='[WARN] NVIDIA GPU 10de:1c8d at 0000:01:00.0 is a Maxwell, Pascal or Volta GPU. It goes unused.'
BASE=$'[PASS] rimed running (owns org.rimeos.Rimed1)\n[WARN] s2idle is the active suspend mode'

doctor() { printf '%s\n' "$1" > "$WORK/doctor.out"; printf '%s\n' "${2:-0}" > "$WORK/doctor.rc"; }
run() {
    HOME="$WORK/home" XDG_STATE_HOME="$WORK/state" RIME_BIN="$WORK/rime" \
        PATH="$WORK/bin:$PATH" "$NOTICE" >/dev/null 2>&1
}
count() { if [[ -f "$WORK/notified" ]]; then wc -l < "$WORK/notified"; else echo 0; fi; }
expect_count() {
    local want=$1 name=$2 got
    got=$(count)
    if [[ "$got" -eq "$want" ]]; then ok "$name"; else bad "$name" "notified $got times, wanted $want"; fi
}

sec "a machine with nothing to say"
doctor "$BASE"
run; expect_count 0 "no NVIDIA line: no notification"
doctor "$BASE"$'\n''[PASS] NVIDIA GPU supported by the driver in this image (1 found)'
run; expect_count 0 "a supported NVIDIA GPU: no notification"
doctor "$PASCAL" 1
run; expect_count 0 "doctor failing: no notification, even with the line in its output"

sec "an unsupported GPU is announced once"
doctor "$BASE"$'\n'"$PASCAL"
run; expect_count 1 "first run notifies"
if grep -qF '10de:1b80' "$WORK/notified" && ! grep -qF '[WARN]' "$WORK/notified"; then
    ok "the body is doctor's sentence, without the [WARN] tag"
else
    bad "the body is doctor's sentence, without the [WARN] tag" "$(cat "$WORK/notified")"
fi
run; run; expect_count 1 "the same state at later logins: silent"

sec "a change is announced again"
doctor "$BASE"$'\n'"$OTHER"
run; expect_count 2 "a different GPU state notifies"
doctor "$BASE"
run; expect_count 2 "the line going away is silent"
if [[ ! -e "$WORK/state/rime/gpu-notice.seen" ]]; then
    ok "…and forgets what was said"
else
    bad "…and forgets what was said" "seen file still present"
fi
doctor "$BASE"$'\n'"$OTHER"
run; expect_count 3 "so the same state coming back is announced again"

sec "the unit is a user unit with a session to talk to"
if grep -q '^ExecStart=/usr/libexec/rime-gpu-notice$' "$SVC"; then ok "service runs the script"; else bad "service runs the script"; fi
if grep -q '^NoNewPrivileges=yes$' "$SVC"; then ok "…without privilege"; else bad "…without privilege"; fi
if grep -q '^OnStartupSec=' "$TIMER" && ! grep -q '^OnUnitActiveSec=' "$TIMER"; then
    ok "timer fires once per session, no repeat"
else
    bad "timer fires once per session, no repeat"
fi
if grep -q 'rime-gpu-notice.service.*/usr/lib/systemd/user/' "$REPO/Containerfile.base" \
   || grep -A1 'files/system/units/rime-gpu-notice.service' "$REPO/Containerfile.base" | pipe_has '/usr/lib/systemd/user/'; then
    ok "Containerfile.base installs it as a USER unit"
else
    bad "Containerfile.base installs it as a USER unit"
fi
if grep -q 'systemctl --global enable rime-gpu-notice.timer' "$REPO/Containerfile.base"; then
    ok "…and enables the timer for every account"
else
    bad "…and enables the timer for every account"
fi

printf '\nrime-gpu-notice: %d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
