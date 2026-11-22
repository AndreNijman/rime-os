#!/usr/bin/env bash
# A Rime Shell restarted while the screen is locked gets its lock back on
# Hyprland, with Rime's own seeded config and the Hyprland installed here.
#
# Rime Shell draws the lock screen. A Shell that dies mid-lock (a crash, a kill,
# or a configuration reload, which on Quickshell 0.3.2 ends the process; see
# rime-shell src/services/system/LockKeeper.qml) leaves Hyprland holding an
# abandoned lock: the session stays locked, with nothing to type a password
# into. The Shell started again finds LockKeeper's record and asks for the lock
# at once. Hyprland grants that only with misc:allow_session_lock_restore
# (SessionLockManager.cpp onNewSessionLock, 0.56.2 and 0.57.0); without it the
# new lock is refused and the password prompt never comes back.
# files/desktop/hypr/rime/appearance.lua sets it, and this proves the option
# reaches a live Hyprland and does what it is there for:
#
#   option     `hyprctl getoption misc:allow_session_lock_restore` on the live
#              instance reads true and set (the shipped Hyprland accepted it);
#   lock       the real Shell (RIME_SHELL_DIR, default /usr/share/rime-shell),
#              locked over its IPC as hypridle and `loginctl lock-session` do,
#              is granted the lock (`secure` true, from its own log);
#   restart    killed with SIGKILL while locked and started again with the same
#              command line and environment, the new process logs LockKeeper's
#              "restarted while the session was locked" AND Hyprland grants its
#              lock: `secure` true in the NEW process's own log, and the record
#              names the new pid, engaged.
#
# Mutant (by hand): the same run from a copy of this tree whose appearance.lua
# lacks the option must FAIL at "option" and at "restart" (the restarted Shell
# logs the restart line but never `secure=true`).
#
# Nothing here types: no password, no PAM, no unlock. Input reaching the desktop
# is not measured here, because nested Hyprland does not deliver wtype's keys
# even to an unlocked window; the NixOS VM check (nix/tests/hypr-lua.nix) types
# real keyboard input instead.
#
# Isolation, as tests/test-rime-hypr-lua.sh: labwc on the wlroots HEADLESS
# backend, Hyprland nested inside it, and the Shell inside Hyprland, all under
# `env -i` with a private HOME and XDG_RUNTIME_DIR, so nothing reaches this
# machine's own session. No USER and stub loginctl/busctl, so the Shell's
# LockedHintService cannot tell logind anything.
#
# Skips cleanly (status 0) when labwc, Hyprland, hyprctl or quickshell is
# missing, or when the Shell predates LockKeeper.
set -uo pipefail
pipe_has() { grep "$@" >/dev/null; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }
finish() {
    printf '\nrime hypr-lock-restore: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
    [ "$fail" -eq 0 ]
}

for tool in labwc Hyprland hyprctl quickshell; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is not installed; cannot run the nested session"
        finish; exit $?
    fi
done

SHELL_DIR="${RIME_SHELL_DIR:-/usr/share/rime-shell}"
SHELL_DIR="${SHELL_DIR%/}"
if [ ! -f "$SHELL_DIR/shell.qml" ] || [ ! -f "$SHELL_DIR/src/services/system/LockKeeper.qml" ]; then
    skip "no Rime Shell with LockKeeper at $SHELL_DIR (set RIME_SHELL_DIR to a rime-shell tree from 3571ae3d or later)"
    finish; exit $?
fi

TMPL="${root}/files/desktop/hypr/hyprland.lua"
MODULES="${root}/files/desktop/hypr/rime"
if [ ! -f "$TMPL" ] || [ ! -d "$MODULES" ]; then
    bad "the seeded Hyprland tree is present"
    finish; exit $?
fi

W="$(mktemp -d)"
RT="$W/run"; H="$W/home"; B="$W/bin"
mkdir -p "$RT" "$H" "$B"; chmod 0700 "$RT"
LABWC_LOG="$W/labwc.log"; HYPR_LOG="$W/hypr.log"
LABWC_PID=""; HYPR_PID=""; SHELL_PIDS=()

cleanup() {
    for p in "${SHELL_PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done
    # A Shell the test did not start itself (a relaunch): only ever one whose
    # environment carries this run's private runtime dir.
    for e in /proc/[0-9]*/environ; do
        p="${e#/proc/}"; p="${p%/environ}"
        [ -O "$e" ] && tr '\0' '\n' 2>/dev/null < "$e" | pipe_has -xF "XDG_RUNTIME_DIR=$RT" && kill -9 "$p" 2>/dev/null
    done
    [ -n "$HYPR_PID" ]  && kill "$HYPR_PID"  2>/dev/null
    sleep 1
    [ -n "$LABWC_PID" ] && kill "$LABWC_PID" 2>/dev/null
    wait 2>/dev/null
    if [ -n "${KEEP_LOGS:-}" ]; then echo "logs kept in $W"; else rm -rf "$W"; fi
}
trap cleanup EXIT

# ── the fixture home ─────────────────────────────────────────────────────────
mkdir -p "$H/.config/labwc" "$H/.config/hypr/rime"
: > "$H/.config/labwc/autostart"
printf '<?xml version="1.0"?>\n<labwc_config></labwc_config>\n' > "$H/.config/labwc/rc.xml"
render() { sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$1"; }
render "$TMPL" > "$H/.config/hypr/hyprland.lua"
for f in "$MODULES"/*.lua; do
    render "$f" > "$H/.config/hypr/rime/$(basename "$f")"
done
# session.lua starts the session's own autostart (awww, the Shell, …): the
# Shell is started by hand below, so the nested session starts nothing.
: > "$H/.config/hypr/rime/session.lua"

# Stubs for what the Shell would otherwise reach outside the test: logind
# (LockedHintService), the rime CLI, notifications, idle. hyprctl is NOT
# stubbed: the Shell talks to the nested Hyprland through it.
cat > "$B/_stub" <<'STUB'
#!/usr/bin/env bash
case "$*" in *json*) echo "{}" ;; esac
exit 0
STUB
chmod +x "$B/_stub"
for n in loginctl busctl rime notify-send pkcheck hypridle matugen playerctl wpctl \
         brightnessctl xdg-open awww swww wlr-randr niri; do
    ln -s "$B/_stub" "$B/$n"
done
P="$B:/usr/bin:/bin"

# ── the headless host and the nested Hyprland ────────────────────────────────
env -i HOME="$H" PATH="$P" XDG_RUNTIME_DIR="$RT" \
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
    labwc > "$LABWC_LOG" 2>&1 &
LABWC_PID=$!
host=""
for _ in $(seq 1 40); do
    for f in "$RT"/wayland-*; do [ -S "$f" ] && { host="$(basename "$f")"; break; }; done
    [ -n "$host" ] && break
    sleep 0.3
done
if [ -z "$host" ]; then
    skip "the headless host compositor did not come up"; tail -5 "$LABWC_LOG"; finish; exit $?
fi

env -i HOME="$H" PATH="$P" XDG_RUNTIME_DIR="$RT" \
    WAYLAND_DISPLAY="$host" XDG_CURRENT_DESKTOP=Hyprland \
    Hyprland --i-am-really-stupid > "$HYPR_LOG" 2>&1 &
HYPR_PID=$!
sock=""
for _ in $(seq 1 60); do
    for f in "$RT"/hypr/*/.socket.sock; do [ -S "$f" ] && { sock="$f"; break; }; done
    [ -n "$sock" ] && break
    sleep 0.5
done
if [ -z "$sock" ]; then
    skip "the nested Hyprland did not come up"; tail -8 "$HYPR_LOG"; finish; exit $?
fi
SIG="$(basename "$(dirname "$sock")")"
hc() { env -i HOME="$H" PATH="$P" XDG_RUNTIME_DIR="$RT" HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl "$@" 2>&1; }
# Hyprland's own Wayland socket: the one that is not labwc's.
hwl=""
for _ in $(seq 1 20); do
    for f in "$RT"/wayland-*; do
        n="$(basename "$f")"
        [ -S "$f" ] && [ "$n" != "$host" ] && { hwl="$n"; break; }
    done
    [ -n "$hwl" ] && break
    sleep 0.3
done
if [ -z "$hwl" ]; then
    skip "the nested Hyprland published no Wayland socket"; finish; exit $?
fi
printf 'nested: %s inside %s (Hyprland socket %s); shell %s\n' \
    "$(hc version | head -1)" "$host" "$hwl" "$SHELL_DIR"

sec "option: the shipped config's misc:allow_session_lock_restore is live"
opt="$(hc getoption misc:allow_session_lock_restore)"
if printf '%s\n' "$opt" | pipe_has -xE '(bool|int): (true|1)' \
   && printf '%s\n' "$opt" | pipe_has -x 'set: true'; then
    ok "misc:allow_session_lock_restore is true and set by Rime's config"
else
    bad "misc:allow_session_lock_restore is true and set by Rime's config — got: $(printf '%s' "$opt" | tr '\n' ' ')"
fi
errs="$(hc configerrors | tr -d '[:space:]')"
if [ -z "$errs" ] || [ "$errs" = "noerrors" ]; then
    ok "hyprctl configerrors is empty"
else
    bad "hyprctl configerrors is empty — got: $(hc configerrors | head -3 | tr '\n' ' ')"
fi

# ── the Shell ────────────────────────────────────────────────────────────────
qml_env=()
[ -d /usr/lib64/rime-shell/qml ] && qml_env=(QML_IMPORT_PATH=/usr/lib64/rime-shell/qml QML2_IMPORT_PATH=/usr/lib64/rime-shell/qml)
shell_env=(env -i HOME="$H" PATH="$P" XDG_RUNTIME_DIR="$RT"
           WAYLAND_DISPLAY="$hwl" HYPRLAND_INSTANCE_SIGNATURE="$SIG"
           XDG_CURRENT_DESKTOP=Hyprland XDG_SESSION_TYPE=wayland QT_QPA_PLATFORM=wayland
           RIME_PACING_LOG=1 "${qml_env[@]}")
# As rime-shell-autostart starts it.
STARTED=""
start_shell() {
    "${shell_env[@]}" quickshell -c "$SHELL_DIR" -n > "$1" 2>&1 &
    STARTED=$!
    SHELL_PIDS+=("$STARTED")
}
answers() { "${shell_env[@]}" timeout 5 quickshell ipc -c "$SHELL_DIR" show 2>/dev/null | pipe_has '^target '; }
wait_log() { # <file> <fixed string> <seconds>
    for _ in $(seq 1 $(( $3 * 4 ))); do
        pipe_has -F "$2" "$1" && return 0
        sleep 0.25
    done
    return 1
}
alive() {
    local st
    st="$(sed -n 's/^.*) \(.\).*$/\1/p' "/proc/$1/stat" 2>/dev/null)"
    [ -n "$st" ] && [ "$st" != Z ] && [ "$st" != X ]
}

sec "lock: the Shell is granted the lock"
LOG1="$W/shell-1.log"
start_shell "$LOG1"; pid1=$STARTED
up=0
for _ in $(seq 1 120); do answers && { up=1; break; }; alive "$pid1" || break; sleep 0.5; done
if [ "$up" -ne 1 ]; then
    bad "the Shell started in the nested Hyprland and answers IPC"
    tail -20 "$LOG1"; finish; exit $?
fi
ok "the Shell started in the nested Hyprland and answers IPC (pid $pid1)"
# Let the lazy lock screen and the migration marker settle, as a login does.
sleep 3
"${shell_env[@]}" timeout 5 quickshell ipc -c "$SHELL_DIR" call lockscreen lock >/dev/null 2>&1
if wait_log "$LOG1" "lock secure=true" 30; then
    ok "the first Shell's lock is granted (secure=true)"
else
    bad "the first Shell's lock is granted (secure=true)"
    tail -20 "$LOG1"; finish; exit $?
fi

sec "restart: killed while locked, started again, the lock comes back"
kill -9 "$pid1"
for _ in $(seq 1 40); do alive "$pid1" || break; sleep 0.25; done
if alive "$pid1"; then bad "the first Shell ended on SIGKILL"; finish; exit $?; fi
ok "the first Shell ended on SIGKILL while the session was locked"
sleep 1

LOG2="$W/shell-2.log"
start_shell "$LOG2"; pid2=$STARTED
if wait_log "$LOG2" "restarted while the session was locked; locking again" 60; then
    ok "the restarted Shell found LockKeeper's record and asked for the lock at start"
else
    bad "the restarted Shell found LockKeeper's record and asked for the lock at start"
    tail -20 "$LOG2"
fi
if wait_log "$LOG2" "lock secure=true" 20; then
    ok "Hyprland granted the restarted Shell's lock (secure=true in the new process's log)"
else
    bad "Hyprland granted the restarted Shell's lock (secure=true in the new process's log)"
    pipe_has -F "lock secure" "$LOG2" && grep -F "lock secure" "$LOG2" | sed 's/^/      /'
fi
rec="$(cat "$RT/rime-shell/lock-held.json" 2>/dev/null)"
if printf '%s' "$rec" | python3 -c '
import json, sys
r = json.load(sys.stdin)
sys.exit(0 if r.get("held") is True and r.get("engaged") is True and r.get("pid") == int(sys.argv[1]) else 1)' "$pid2"; then
    ok "the lock record names the restarted Shell (pid $pid2), held and engaged"
else
    bad "the lock record names the restarted Shell (pid $pid2), held and engaged — got: $rec"
fi
if alive "$pid2"; then
    ok "the restarted Shell is still running"
else
    bad "the restarted Shell is still running"
fi

finish
