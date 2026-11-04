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
#    • an IPC client, in either argument order (`qs ipc -c <dir> call …` and
#      `qs -c <dir> ipc show`, which `rime shell ipc` and hypridle's lock_cmd
#      run), does not stop it either: the second form did, and lost the shell
#      in 1 boot of 6 of the shell-kept VM check (2026-11-04);
#    • a process that IS the shell (`quickshell -c <dir>`, with options) does.
#
#  And the start itself: quickshell gets -n (--no-duplicate); every decision
#  reaches the journal (systemd-cat, stubbed); a start whose quickshell exits
#  before the shell answers IPC is retried, up to 3 starts; when none answers,
#  or one runs and never answers, the failure is reported at priority err and
#  on screen (hyprctl notify, stubbed).
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
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
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

# quickshell, both halves:
#   `quickshell ipc -c <dir> show`  answers ("target probe") once $T/answer
#                                   exists, else fails like "No running instances";
#   `quickshell -c <dir> -n`        records its argv, then acts out $T/mode:
#       answer      the shell came up: answers, stays alive 3 s
#       exit-first  exits without answering on the first start, answers on a
#                   retried one (RIME_SHELL_START_ATTEMPT is set by the retry)
#       exit        exits without answering, every time
#       hang        stays alive 6 s and never answers
cat > "$T/bin/quickshell" <<STUB
#!/bin/sh
if [ "\$1" = ipc ]; then
    [ -e "$T/answer" ] || { echo "No running instances" >&2; exit 1; }
    echo "target probe"
    exit 0
fi
printf '%s\n' "\$*" >> "$T/started"
case "\$(cat "$T/mode" 2>/dev/null)" in
    exit) exit 0 ;;
    exit-first) [ -n "\${RIME_SHELL_START_ATTEMPT:-}" ] || exit 0
                : > "$T/answer"; sleep 3 ;;
    hang) sleep 6 ;;
    *) : > "$T/answer"; sleep 3 ;;
esac
exit 0
STUB
# hypridle is started in the background when its config exists; there is none
# under this DIR or HOME, so this stub should never run. If it does, it exits.
printf '#!/bin/sh\nexit 0\n' > "$T/bin/hypridle"
printf '#!/bin/sh\nexit 0\n' > "$T/bin/sudo"
# The journal: "<priority>|<line>" per line written through systemd-cat.
cat > "$T/bin/systemd-cat" <<STUB
#!/bin/sh
prio=info
while [ \$# -gt 0 ]; do case "\$1" in -p) prio=\$2; shift 2 ;; *) shift ;; esac; done
while IFS= read -r l; do printf '%s|%s\n' "\$prio" "\$l" >> "$T/journal"; done
STUB
# The on-screen report: Hyprland's notification, recorded.
cat > "$T/bin/hyprctl" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$T/notified"
echo ok
STUB
chmod +x "$T/bin/quickshell" "$T/bin/hypridle" "$T/bin/sudo" "$T/bin/systemd-cat" "$T/bin/hyprctl"

# One launcher run. The watcher it leaves behind is waited for: launch returns
# once the journal holds a final word (up, already running, or a failure).
launch() {
    : > "$T/started"; : > "$T/journal"; : > "$T/notified"; rm -f "$T/answer"
    HOME="$T/home" PATH="$T/bin:$PATH" RIME_SHELL_DIR="$DIR" RIME_SHELL_WAIT_TIMEOUT=2 \
        RIME_SECRETS_START=/nonexistent HYPRLAND_INSTANCE_SIGNATURE=test-instance \
        RIME_SHELL_CONFIRM_SECS="${CONFIRM:-10}" \
        bash "$LAUNCHER" >"$T/out" 2>&1 </dev/null
    local _
    for _ in $(seq 1 200); do
        grep -qE '^info\|(Rime Shell is up|Rime Shell already running)|^err\|' "$T/journal" && break
        sleep 0.1
    done
    # anything the watcher still writes after its final word lands now
    sleep 0.3
}
set_mode() { printf '%s\n' "$1" > "$T/mode"; }
set_mode answer

# Wait until pgrep can see a process's command line as $2 (perl rewrites it
# after it starts).
wait_cmdline() {
    local pid=$1 want=$2 _
    for _ in $(seq 1 50); do
        tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | pipe_has -F -- "$want" && return 0
        sleep 0.1
    done
    return 1
}

# 1. Nothing running: the shell starts, with exactly the expected argv.
launch
if [ "$(cat "$T/started")" = "-c $DIR -n" ]; then ok "starts the shell when none is running, with -n"
else bad "starts the shell when none is running, with -n" "started: $(cat "$T/started")"; fi
if grep -qF "info|starting quickshell -c $DIR -n (start 1 of 3)" "$T/journal" \
    && grep -qE '^info\|Rime Shell is up \(pid [0-9]+, start 1 of 3' "$T/journal"; then
    ok "the start and the confirmed shell are in the journal"
else bad "the start and the confirmed shell are in the journal" "$(cat "$T/journal")"; fi

# 2. A process that only MENTIONS the shell's path in its arguments — the ssh
#    `zsh -c '…'` from katana — must not count as a running shell.
bash -c "sleep 30; : quickshell -c $DIR" &
decoy=$!
pids+=("$decoy")
wait_cmdline "$decoy" "quickshell -c $DIR"
launch
if [ "$(cat "$T/started")" = "-c $DIR -n" ]; then ok "a command that mentions the shell does not stop the start"
else bad "a command that mentions the shell does not stop the start" "$(tail -1 "$T/out")"; fi
kill "$decoy" 2>/dev/null; wait "$decoy" 2>/dev/null

# 3. The IPC client the keybinds run (`qs ipc -c <dir> call …`) is not the shell.
if command -v perl >/dev/null; then
    perl -e '$0 = shift; sleep 30' "qs ipc -c $DIR call launcher toggle" &
    pids+=($!)
    wait_cmdline "$!" "qs ipc -c $DIR"
    launch
    if [ "$(cat "$T/started")" = "-c $DIR -n" ]; then ok "an IPC call is not a running shell"
    else bad "an IPC call is not a running shell" "$(tail -1 "$T/out")"; fi
    kill "$!" 2>/dev/null

    # 3b. The same clients with the config named FIRST (`rime shell ipc` runs
    #     `qs -p <dir> ipc call …`, hypridle's lock_cmd and the VM checks
    #     `qs -c <dir> ipc …`). The old check took these for the shell.
    for argv in "qs -c $DIR ipc show" "qs -p $DIR ipc call shell state" \
                "/run/current-system/sw/bin/qs -c $DIR/ ipc call lockscreen lock" "quickshell -c $DIR log"; do
        perl -e '$0 = shift; sleep 30' "$argv" &
        p=$!
        pids+=("$p")
        wait_cmdline "$p" "$argv"
        launch
        if [ "$(cat "$T/started")" = "-c $DIR -n" ]; then ok "'$argv' is a client, not a running shell"
        else bad "'$argv' is a client, not a running shell" "started: $(cat "$T/started"); $(tail -1 "$T/out")"; fi
        kill "$p" 2>/dev/null; wait "$p" 2>/dev/null
    done

    # 4. A process that IS the shell stops the start, under either name.
    for argv in "quickshell -c $DIR" "/usr/bin/quickshell -c $DIR" "qs -p $DIR/" "quickshell -c $DIR -n"; do
        perl -e '$0 = shift; sleep 30' "$argv" &
        p=$!
        pids+=("$p")
        wait_cmdline "$p" "$argv"
        launch
        if [ ! -s "$T/started" ] && grep -qF "already running" "$T/out" \
            && grep -qF "info|Rime Shell already running" "$T/journal"; then
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

# 5. A start whose quickshell exits before the shell answers is retried, and
#    the retry that answers ends it: two starts, both in the journal.
set_mode exit-first
launch
if [ "$(grep -c . "$T/started")" = 2 ] \
    && grep -qF "warning|quickshell (pid" "$T/journal" \
    && grep -qF "starting it again (start 2 of 3)" "$T/journal" \
    && grep -qE '^info\|Rime Shell is up \(pid [0-9]+, start 2 of 3' "$T/journal" \
    && [ ! -s "$T/notified" ]; then
    ok "a start that dies before answering is retried, and the retry is confirmed"
else bad "a start that dies before answering is retried, and the retry is confirmed" \
    "starts=$(grep -c . "$T/started") journal=$(tr '\n' ';' < "$T/journal") notified=$(cat "$T/notified")"; fi

# 6. Every start dies: three starts, then an err line and a notification.
set_mode exit
launch
if [ "$(grep -c . "$T/started")" = 3 ] \
    && grep -qF "err|quickshell exited before the shell answered, 3 starts in a row" "$T/journal" \
    && grep -qE '^notify 3 [0-9]+ 0 Rime Shell did not start: ' "$T/notified"; then
    ok "three dead starts end in an err line and an on-screen notice"
else bad "three dead starts end in an err line and an on-screen notice" \
    "starts=$(grep -c . "$T/started") journal=$(tr '\n' ';' < "$T/journal") notified=$(cat "$T/notified")"; fi

# 7. A shell that runs and never answers: reported once the confirm time is up,
#    and not started again (it is running; a second start would be refused).
set_mode hang
CONFIRM=2 launch
if [ "$(grep -c . "$T/started")" = 1 ] \
    && grep -qE '^err\|quickshell \(pid [0-9]+\) is running but the shell has not answered in 2 s' "$T/journal" \
    && grep -qF "Rime Shell did not start" "$T/notified"; then
    ok "a shell that never answers is reported, not restarted"
else bad "a shell that never answers is reported, not restarted" \
    "starts=$(grep -c . "$T/started") journal=$(tr '\n' ';' < "$T/journal") notified=$(cat "$T/notified")"; fi
sleep 5  # the hanging stub's own sleep, so cleanup removes no live files
set_mode answer

echo
echo "rime-shell-autostart: $pass passed, $fail failed, $skip skipped"
[ "$fail" = 0 ]
