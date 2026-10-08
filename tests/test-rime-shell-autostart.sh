#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-shell-autostart.sh — the launcher's "is the shell already running?"
#  check, against the SHIPPED files/system/libexec/rime-shell-autostart.
#
#  The launcher refuses to start a second shell, and that refusal is the one
#  place it can leave a desktop with no shell at all. Its check used to be an
#  unanchored `pgrep -f "quickshell -c <dir>"`, which matched ANY process whose
#  arguments contained that text. On katana (2026-10-07) a restart was run from
#  an ssh `zsh -c '…'` that mentioned the shell's path: the launcher saw that
#  zsh, said "already running", and the desktop had no bar until the shell was
#  started again by hand.
#
#  Both directions are asserted, so the fix cannot be "never skip":
#    • a process that merely MENTIONS the shell does not stop the start;
#    • a process that IS the shell (`quickshell -c <dir>`) does.
#
#  quickshell is stubbed (it records its argv and exits), as are sudo and
#  hypridle; HOME is a throwaway. The real shell is never started or touched.
#  The stand-in "running shell" is a perl process that rewrites its own
#  command line, because pgrep -f reads /proc/<pid>/cmdline and a stub script
#  would show up as "/bin/sh …/quickshell -c …", which a real shell never does.
#
#  Run from anywhere: ./tests/test-rime-shell-autostart.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2

LAUNCHER=../files/system/libexec/rime-shell-autostart
[ -f "$LAUNCHER" ] || { echo "cannot find $LAUNCHER"; exit 2; }

T=$(mktemp -d /tmp/rime-autostart-test.XXXXXX)
pids=()
cleanup() { [ "${#pids[@]}" -gt 0 ] && kill "${pids[@]}" 2>/dev/null; rm -rf "$T"; }
trap cleanup EXIT

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %s  %s\n' "$1" "$2"; skip=$((skip+1)); }

# A shell tree the launcher accepts: it waits for shell.qml and nothing else.
DIR="$T/rime-shell-under-test"
mkdir -p "$DIR" "$T/bin" "$T/home"
: > "$DIR/shell.qml"

cat > "$T/bin/quickshell" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$T/started"
exit 0
STUB
# hypridle is started in the background when its config exists; there is none
# under this DIR or HOME, so this stub should never run. If it does, it exits.
printf '#!/bin/sh\nexit 0\n' > "$T/bin/hypridle"
printf '#!/bin/sh\nexit 0\n' > "$T/bin/sudo"
chmod +x "$T/bin/quickshell" "$T/bin/hypridle" "$T/bin/sudo"

launch() {
    : > "$T/started"
    HOME="$T/home" PATH="$T/bin:$PATH" RIME_SHELL_DIR="$DIR" RIME_SHELL_WAIT_TIMEOUT=2 \
        bash "$LAUNCHER" >"$T/out" 2>&1 </dev/null
}

# Wait until pgrep can see a process's command line as $2 (perl rewrites it
# after it starts).
wait_cmdline() {
    local pid=$1 want=$2 _
    for _ in $(seq 1 50); do
        tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF -- "$want" && return 0
        sleep 0.1
    done
    return 1
}

# 1. Nothing running: the shell starts, with exactly the expected argv.
launch
if [ "$(cat "$T/started")" = "-c $DIR" ]; then ok "starts the shell when none is running"
else bad "starts the shell when none is running" "started: $(cat "$T/started")"; fi

# 2. A process that only MENTIONS the shell's path in its arguments — the ssh
#    `zsh -c '…'` from katana — must not count as a running shell.
bash -c "sleep 30; : quickshell -c $DIR" &
decoy=$!
pids+=("$decoy")
wait_cmdline "$decoy" "quickshell -c $DIR"
launch
if [ "$(cat "$T/started")" = "-c $DIR" ]; then ok "a command that mentions the shell does not stop the start"
else bad "a command that mentions the shell does not stop the start" "$(tail -1 "$T/out")"; fi
kill "$decoy" 2>/dev/null; wait "$decoy" 2>/dev/null

# 3. The IPC client the keybinds run (`qs ipc -c <dir> call …`) is not the shell.
if command -v perl >/dev/null; then
    perl -e '$0 = shift; sleep 30' "qs ipc -c $DIR call launcher toggle" &
    pids+=($!)
    wait_cmdline "$!" "qs ipc -c $DIR"
    launch
    if [ "$(cat "$T/started")" = "-c $DIR" ]; then ok "an IPC call is not a running shell"
    else bad "an IPC call is not a running shell" "$(tail -1 "$T/out")"; fi

    # 4. A process that IS the shell stops the start, under either name.
    for argv in "quickshell -c $DIR" "/usr/bin/quickshell -c $DIR" "qs -p $DIR/"; do
        perl -e '$0 = shift; sleep 30' "$argv" &
        p=$!
        pids+=("$p")
        wait_cmdline "$p" "$argv"
        launch
        if [ ! -s "$T/started" ] && grep -qF "already running" "$T/out"; then
            ok "a running '$argv' is not started twice"
        else
            bad "a running '$argv' is not started twice" "started: $(cat "$T/started")"
        fi
        kill "$p" 2>/dev/null; wait "$p" 2>/dev/null
    done
else
    skipped "an IPC call is not a running shell" "(no perl to set a command line)"
    skipped "a running shell is not started twice" "(no perl to set a command line)"
fi

echo
echo "rime-shell-autostart: $pass passed, $fail failed, $skip skipped"
[ "$fail" = 0 ]
