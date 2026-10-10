#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-live-session.sh — the user-side half of a live update, against the
#  SHIPPED files/system/libexec/rime-live-session.
#
#  The helper replaces the running Rime Shell, which is also the lock screen.
#  What is asserted is mostly what it must NOT do:
#    • never stop the shell while the session is locked, while the shell says
#      it is locked, or when the lock state cannot be read — including a lock
#      that appears between the first check and the swap;
#    • always hold a sleep/idle/lid inhibitor across the swap;
#    • report failure (so the engine rolls back) when the new shell does not
#      answer with the expected revision.
#
#  qs, loginctl, hyprctl, systemd-inhibit and systemctl are stubs on PATH; the
#  "shell" is a sleep process. Nothing real is started, stopped or reloaded.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2

HELPER=../files/system/libexec/rime-live-session
[ -x "$HELPER" ] || { echo "cannot find $HELPER"; exit 2; }
HELPER="$(realpath "$HELPER")"

T=$(mktemp -d /tmp/rime-live-session-test.XXXXXX)
cleanup() {
    for f in "$T"/pid.*; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
    rm -rf "$T"
}
trap cleanup EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

mkdir -p "$T/bin"
# State the stubs read: $T/hint (LockedHint answers, one per call, last repeats),
# $T/state (what `qs ipc call shell state` prints), $T/rev (what the NEW shell
# says for `shell revision`), $T/pid.cur (the running shell).
cat > "$T/bin/loginctl" <<STUB
#!/bin/sh
f="$T/hint"; n=\$(cat "$T/hint.n" 2>/dev/null || echo 1)
line=\$(sed -n "\${n}p" "\$f"); [ -n "\$line" ] || line=\$(tail -n 1 "\$f")
echo \$((n+1)) > "$T/hint.n"
[ "\$line" = "ERR" ] && exit 1
printf '%s\n' "\$line"
STUB
cat > "$T/bin/qs" <<STUB
#!/bin/sh
case "\$1" in
  list)
    [ -f "$T/pid.cur" ] && kill -0 "\$(cat "$T/pid.cur")" 2>/dev/null \
      && printf '[{"config_path":"x/shell.qml","pid":%s}]\n' "\$(cat "$T/pid.cur")" || echo '[]'
    ;;
  ipc)
    shift 4   # ipc -p DIR call
    case "\$*" in
      "shell state")    cat "$T/state" ;;
      "shell revision") if [ -f "$T/pid.new" ]; then cat "$T/rev"; else cat "$T/oldrev"; fi ;;
      "caffeine state") echo false ;;
    esac
    ;;
esac
STUB
# The compositor's exec starts a new "shell".
cat > "$T/bin/hyprctl" <<STUB
#!/bin/sh
echo "\$*" >> "$T/hyprctl.log"
case "\$1" in
  dispatch) [ -f "$T/nospawn" ] || { sleep 300 >/dev/null 2>&1 </dev/null & echo \$! > "$T/pid.new"; cp "$T/pid.new" "$T/pid.cur"; }; echo ok ;;
  configerrors) cat "$T/configerrors" 2>/dev/null ;;
esac
STUB
cat > "$T/bin/systemd-inhibit" <<STUB
#!/bin/sh
echo "\$*" >> "$T/inhibit.log"
while [ "\${1#--}" != "\$1" ]; do shift; done
exec "\$@"
STUB
cat > "$T/bin/systemctl" <<STUB
#!/bin/sh
echo "\$*" >> "$T/systemctl.log"
[ -f "$T/firstrun.fail" ] && exit 1
exit 0
STUB
chmod +x "$T"/bin/*

reset() {
    for f in "$T"/pid.*; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
    rm -f "$T"/pid.* "$T"/*.log "$T/hint.n" "$T/nospawn" "$T/firstrun.fail" "$T/configerrors"
    printf 'no\n' > "$T/hint"
    printf '{"revision":"old","locked":false,"lockSecure":false,"pid":1}' > "$T/state"
    printf 'new123' > "$T/rev"; printf 'old' > "$T/oldrev"
    sleep 300 & echo $! > "$T/pid.cur"; cp "$T/pid.cur" "$T/pid.old"
}
run() {
    PATH="$T/bin:$PATH" HYPRLAND_INSTANCE_SIGNATURE=test RIME_LIVE_SHELL_WAIT=2 \
        RIME_LIVE_SHELL_DIR=/nonexistent RIME_LIVE_AUTOSTART=/bin/true \
        "$HELPER" "$@" > "$T/out" 2>&1
    echo $?
}
old_alive() { kill -0 "$(cat "$T/pid.old")" 2>/dev/null; }

# 1. Unlocked: the swap happens, under the inhibitor, and is verified.
reset
rc=$(run shell new123 4)
if [ "$rc" = 0 ] && ! old_alive && grep -q 'revision new123' "$T/out"; then ok "unlocked: shell replaced and verified"; else bad "unlocked swap" "rc=$rc $(cat "$T/out")"; fi
grep -q 'sleep:idle:handle-lid-switch' "$T/inhibit.log" 2>/dev/null && ok "swap holds a sleep/idle/lid inhibitor" || bad "inhibitor" "$(cat "$T/inhibit.log" 2>/dev/null)"

# 2-4. Locked, shell says locked, unreadable hint: nothing is touched.
for case in hint state err; do
    reset
    case $case in
        hint)  printf 'yes\n' > "$T/hint" ;;
        state) printf '{"locked":true}' > "$T/state" ;;
        err)   printf 'ERR\n' > "$T/hint" ;;
    esac
    rc=$(run shell new123 4)
    if [ "$rc" = 3 ] && old_alive && [ ! -f "$T/hyprctl.log" ]; then ok "locked ($case): exit 3, shell untouched"; else bad "locked ($case)" "rc=$rc $(cat "$T/out")"; fi
done

# 5. Locked between the first check and the swap.
reset
printf 'no\nyes\n' > "$T/hint"
rc=$(run shell new123 4)
if [ "$rc" = 3 ] && old_alive && [ ! -f "$T/hyprctl.log" ]; then ok "lock during the inhibitor handoff: exit 3, shell untouched"; else bad "late lock" "rc=$rc $(cat "$T/out")"; fi

# 6. Unknown shell state text is not "unlocked".
reset
printf 'garbage' > "$T/state"
rc=$(run shell new123 4)
[ "$rc" = 3 ] && old_alive && ok "unreadable shell state: exit 3" || bad "garbage state" "rc=$rc"

# 7. A shell from before the IPC target: LockedHint decides.
reset
printf 'Target not found.' > "$T/state"
rc=$(run shell new123 4)
[ "$rc" = 0 ] && ok "old shell without the target: LockedHint=no permits" || bad "old shell" "rc=$rc $(cat "$T/out")"

# 8. Wrong revision from the new shell: failure, so the engine rolls back.
reset
printf 'something-else' > "$T/rev"
rc=$(run shell new123 4)
[ "$rc" = 1 ] && grep -q 'did not answer' "$T/out" && ok "wrong revision: exit 1" || bad "wrong revision" "rc=$rc $(cat "$T/out")"

# 9. New shell never comes up: failure.
reset; : > "$T/nospawn"
rc=$(run shell new123 4)
[ "$rc" = 1 ] && ok "no new shell: exit 1" || bad "no new shell" "rc=$rc"

# 10. Rollback mode ("-") accepts an old shell that cannot report a revision.
reset
printf 'Target not found.' > "$T/rev"
rc=$(run shell - 4)
[ "$rc" = 0 ] && ok "rollback mode accepts any answering shell" || bad "rollback mode" "rc=$rc $(cat "$T/out")"

# 11. No shell running: nothing to do.
reset; kill "$(cat "$T/pid.cur")"; sleep 0.1
rc=$(run shell new123 4)
[ "$rc" = 4 ] && ok "no running shell: exit 4" || bad "no shell" "rc=$rc"

# 12. Session ids are validated.
reset
rc=$(run shell new123 '4;rm')
[ "$rc" = 1 ] && old_alive && ok "bad session id refused" || bad "session id" "rc=$rc"

# 13-15. Hyprland modules.
reset
rc=$(run hypr - 4)
if [ "$rc" = 0 ] && grep -q 'start rime-shell-firstrun.service' "$T/systemctl.log" && grep -q '^reload' "$T/hyprctl.log"; then ok "hypr: modules refreshed and reloaded"; else bad "hypr" "rc=$rc $(cat "$T/out")"; fi
reset; printf 'config error at rime/keybindings.lua:3\n' > "$T/configerrors"
rc=$(run hypr - 4)
[ "$rc" = 1 ] && ok "hypr: config errors fail the activation" || bad "hypr errors" "rc=$rc"
reset
rc=$(env -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u LABWC_PID PATH="$T/bin:$PATH" "$HELPER" hypr - 4 >/dev/null 2>&1; echo $?)
[ "$rc" = 4 ] && ok "hypr: not a Hyprland session: exit 4" || bad "hypr non-hyprland" "rc=$rc"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
