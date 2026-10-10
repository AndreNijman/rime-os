#!/usr/bin/env bash
# test-rime-update-notice.sh — the "new Rime update available" notice.
#
# Drives rime-update-check against a stub bootc and rime-update-notice against a
# stub notify-send. No root, registry or session bus.
#
# Failures it exists to catch: a notice for an update that is not there (or is
# already staged); a repeat notice for the same release; no notice for a newer
# one; a failed check wiping a good answer; the units drifting into something
# that polls, runs at normal priority, or runs the notice as root.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK="$REPO/files/system/libexec/rime-update-check"
NOTICE="$REPO/files/system/libexec/rime-update-notice"
U="$REPO/files/system/units"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }
for f in "$CHECK" "$NOTICE" "$U"/rime-update-{check.service,check.timer,notice.service,notice.path}; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
mkdir -p "$W/bin" "$W/home" "$W/run"
D1="sha256:$(printf 'a%.0s' {1..64})"; D2="sha256:$(printf 'b%.0s' {1..64})"

# Stub bootc: `status` prints $W/status, `upgrade --check` prints $W/check, exit $W/check.rc.
cat > "$W/bootc" <<STUB
#!/usr/bin/env bash
case "\$1" in
  status) cat "$W/status" ;;
  upgrade) cat "$W/check"; exit "\$(cat "$W/check.rc" 2>/dev/null || echo 0)" ;;
esac
STUB
chmod +x "$W/bootc"
cat > "$W/bin/notify-send" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$W/notified"
STUB
chmod +x "$W/bin/notify-send"

run_check()  { RIME_BOOTC_BIN="$W/bootc" RIME_UPDATE_DIR="$W/run" bash "$CHECK" >/dev/null 2>&1; }
run_notice() { HOME="$W/home" XDG_STATE_HOME="$W/home/state" PATH="$W/bin:$PATH" RIME_UPDATE_STATE="$W/run/state" bash "$NOTICE" >/dev/null 2>&1; }
count() { [[ -f "$W/notified" ]] && wc -l < "$W/notified" || echo 0; }
avail() { printf 'Update available for: docker://ghcr.io/x/rime-os:daily\n  Version: 1\n  Digest: %s\n' "$1" > "$W/check"; echo 0 > "$W/check.rc"; }

sec "check"
echo '{"status":{"staged":null}}' > "$W/status"
avail "$D1"; run_check
grep -q '^state=available$' "$W/run/state" && grep -q "^digest=$D1\$" "$W/run/state" \
    && ok "update available is recorded with its digest" || bad "available"
echo 'No changes in: docker://ghcr.io/x/rime-os:daily' > "$W/check"; run_check
grep -q '^state=none$' "$W/run/state" && ok "no changes -> none" || bad "none"
avail "$D1"; run_check
echo '{"status":{"staged":{"image":{}}}}' > "$W/status"; run_check
grep -q '^state=none$' "$W/run/state" && ok "an already-staged update is not announced" || bad "staged"
echo '{"status":{"staged":null}}' > "$W/status"; avail "$D1"; run_check
echo 'boom' > "$W/check"; echo 1 > "$W/check.rc"; run_check
grep -q '^state=available$' "$W/run/state" && ok "a failed check keeps the last answer" || bad "failed check wiped state"
echo 'weird output' > "$W/check"; echo 0 > "$W/check.rc"; run_check
grep -q '^state=available$' "$W/run/state" && ok "unrecognised output keeps the last answer" || bad "unrecognised"

sec "notice"
rm -f "$W/notified"
echo 'state=none' > "$W/run/state"; run_notice
[[ "$(count)" == 0 ]] && ok "nothing to say -> silent" || bad "silent"
printf 'state=available\ndigest=%s\n' "$D1" > "$W/run/state"; run_notice
[[ "$(count)" == 1 ]] && grep -q 'sudo rime update' "$W/notified" && ok "announces once and names the command" || bad "first notice"
run_notice; run_notice
[[ "$(count)" == 1 ]] && ok "same release is never repeated" || bad "repeat"
printf 'state=available\ndigest=%s\n' "$D2" > "$W/run/state"; run_notice
[[ "$(count)" == 2 ]] && ok "a newer release is announced again" || bad "newer"
printf 'state=available\n' > "$W/run/state"; rm -f "$W/home/state/rime/update-notice.seen"; run_notice
[[ "$(count)" == 2 ]] && ok "a partial write is ignored" || bad "partial"
rm -f "$W/run/state"; run_notice; [[ "$(count)" == 2 ]] && ok "missing state file is silent" || bad "missing"

sec "units"
grep -q '^OnBootSec=' "$U/rime-update-check.timer" && grep -q '^OnUnitActiveSec=' "$U/rime-update-check.timer" \
    && ok "check runs after boot and on an interval" || bad "timer"
grep -q '^CPUSchedulingPolicy=idle' "$U/rime-update-check.service" && grep -q '^IOSchedulingClass=idle' "$U/rime-update-check.service" \
    && grep -q '^Nice=19' "$U/rime-update-check.service" && ok "check runs at idle priority" || bad "priority"
grep -q '^PathChanged=/run/rime-update/state' "$U/rime-update-notice.path" && ok "notice is woken by inotify, not polling" || bad "path"
! grep -qE '^(User|OnCalendar|OnUnitActiveSec)=' "$U/rime-update-notice.service" "$U/rime-update-notice.path" \
    && ok "notice units poll nothing" || bad "notice polls"

sec "live update results"
rm -f "$W/run/state" "$W/notified"
run_live() { HOME="$W/home" XDG_STATE_HOME="$W/home/state" PATH="$W/bin:$PATH" RIME_UPDATE_STATE="$W/run/state" RIME_LIVE_STATUS="$W/run/live.json" bash "$NOTICE" >/dev/null 2>&1; }
live_doc() { printf '{"schema":1,"txn":"%s","state":"%s","summary":"%s","components":[{"component":"shell","state":"%s"}]}' "$1" "$2" "$3" "${4:-active}" > "$W/run/live.json"; }
sent_n() { [[ -f "$W/notified" ]] && wc -l < "$W/notified" || echo 0; }
live_doc t1 planned "plan only"; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "a plan is not announced" || bad "plan announced"
live_doc t1 deferred "everything waits for a restart"; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "a deferred update is not announced" || bad "deferred announced"
live_doc t1 active "Up to date." pending; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "active with nothing activated is not announced" || bad "empty active announced"
live_doc t2 active "Rime Shell is active now."; run_live
[[ "$(sent_n)" -eq 1 ]] && grep -q 'Rime update applied' "$W/notified" && grep -q 'Rime Shell is active now.' "$W/notified" \
    && ok "an applied update is announced in the engine's words" || bad "active" "$(cat "$W/notified" 2>/dev/null)"
run_live
[[ "$(sent_n)" -eq 1 ]] && ok "once per transaction" || bad "repeat"
live_doc t3 failed "Live activation failed; see rime live doctor." failed; run_live
[[ "$(sent_n)" -eq 2 ]] && grep -q 'rime live doctor' "$W/notified" && ok "a failure names rime live doctor" || bad "failed" "$(cat "$W/notified")"
printf '{"schema":2,"txn":"t4","state":"active","summary":"x"}' > "$W/run/live.json"; run_live
printf 'not json' > "$W/run/live.json"; run_live
[[ "$(sent_n)" -eq 2 ]] && ok "unknown schema or garbage is ignored" || bad "garbage"
grep -q '^PathChanged=/run/rime-live/status.json' "$U/rime-update-notice.path" && ok "notice is woken by a live result" || bad "live path"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
