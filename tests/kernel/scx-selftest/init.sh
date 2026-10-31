#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
#  init.sh — PID 1 of the sched-ext selftest guest (rdinit=/init).
#
#  The guest is the SHIPPED kernel (vmlinuz out of the image under test) and an
#  initramfs holding nothing but the image's own scheduler binaries, their
#  shared libraries and enough of bash/coreutils/util-linux to drive them. See
#  run-scx-selftest.sh for why, and collect.sh for how the initramfs is built.
#
#  For every scheduler in /selftest/schedulers.txt this:
#    1. starts it and waits (bounded) for /sys/kernel/sched_ext/state=enabled
#       with root/ops naming that scheduler;
#    2. runs a CPU + fork load for LOAD seconds;
#    3. asserts it is still enabled, ops and enable_seq unchanged (no eject +
#       reload in between), the process alive, and no sched_ext "disabled" or
#       error line in /dev/kmsg during the window;
#    4. stops it with SIGINT and asserts state returns to disabled, with the
#       kernel naming "unregistered from user space" as the reason.
#  Then one scheduler is killed with SIGKILL, to prove the kernel itself hands
#  every task back to the fair class when the userspace half vanishes.
#
#  Output contract (parsed by run-scx-selftest.sh, one line each, on the
#  serial console):
#      SCX-SELFTEST sched=<name> result=PASS|FAIL [key=value ...]
#      SCX-SELFTEST kill9=<name> result=PASS|FAIL [key=value ...]
#      SCX-SELFTEST verdict=PASS|FAIL passed=<n> failed=<n> total=<n>
#  Values never contain spaces (reason= uses '_' for blanks).
#
#  Runs on the guest only. Nothing here touches the machine that built it.
# ─────────────────────────────────────────────────────────────────────────────
set -u
export PATH=/usr/bin:/usr/sbin:/bin:/sbin
export LC_ALL=C

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
# devtmpfs has no /dev/fd; bash's <(...) needs it.
ln -sfn /proc/self/fd /dev/fd
mkdir -p /sys/kernel/debug /sys/fs/cgroup /run /tmp /var
mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true
mount -t cgroup2 cgroup2 /sys/fs/cgroup 2>/dev/null || true
mount -t tmpfs tmpfs /run
mount -t tmpfs tmpfs /tmp
ln -sfn /run /var/run

say() { echo "SCX-SELFTEST $*"; }
# Kernel parameters: scx_selftest.load=<s> scx_selftest.settle=<s>
param() {
    local key="$1" def="$2" w
    for w in $(</proc/cmdline); do
        case "$w" in "scx_selftest.$key="*) echo "${w#*=}"; return ;; esac
    done
    echo "$def"
}
LOAD="$(param load 5)"
SETTLE="$(param settle 10)"
NCPU="$(grep -c '^processor' /proc/cpuinfo)"

poweroff_now() {
    sync 2>/dev/null
    # The tty drains asynchronously and the kernel's own "sysrq: Power Off"
    # line goes straight to the UART: without a pause it lands in the middle
    # of the last result line (seen on the first run of this harness).
    sleep 1
    echo o > /proc/sysrq-trigger 2>/dev/null
    # sysrq is asynchronous; if it is disabled, falling off the end of PID 1
    # panics, and panic=-1 + qemu -no-reboot ends the run either way.
    sleep 5
    exit 1
}

SX=/sys/kernel/sched_ext
if [ ! -d "$SX" ]; then
    say "verdict=FAIL passed=0 failed=0 total=0 reason=no_/sys/kernel/sched_ext_(CONFIG_SCHED_CLASS_EXT)"
    poweroff_now
fi

# Everything the kernel logs, from boot, so each window can be inspected by
# line number afterwards. /dev/kmsg is read from the beginning by a new reader.
cat /dev/kmsg > /tmp/kmsg.log 2>/dev/null &
KMSG_PID=$!
sleep 0.3

say "boot kernel=$(</proc/sys/kernel/osrelease) cpus=${NCPU} load_s=${LOAD} settle_s=${SETTLE}"

kmsg_lines() { local n=0; while IFS= read -r _; do n=$((n + 1)); done < /tmp/kmsg.log; echo "$n"; }
# kmsg_since <line-count> → the sched_ext lines logged after that point,
# message text only (the record header before ';' dropped).
kmsg_since() {
    local from="$1" n=0 line
    while IFS= read -r line; do
        n=$((n + 1))
        [ "$n" -le "$from" ] && continue
        case "$line" in *sched_ext*) echo "${line#*;}" ;; esac
    done < /tmp/kmsg.log
}
rd() { local v=""; [ -r "$1" ] && v="$(<"$1")"; echo "$v"; }
state() { rd "$SX/state"; }
ops() { rd "$SX/root/ops"; }
seq_() { rd "$SX/enable_seq"; }
# The kernel publishes the struct_ops name, which usually drops `scx_` (but
# scx_flow keeps it: scx_flow_3.1.2_...) and may append a `_<version>_<triple>`
# build tag (lavd_1.1.3_x86_64_unknown_linux_gnu). Same rule as rimed's
# scx_ops_matches: strip the prefix from both sides.
ops_matches() {
    local o="${1#scx_}" s="${2#scx_}"
    [ "$o" = "$s" ] && return 0
    case "$o" in "${s}_"[0-9]*) return 0 ;; esac
    return 1
}
clean() { local s="$*"; s="${s// /_}"; s="${s//=/:}"; echo "${s:0:300}"; }

# wait_for <seconds> <predicate...> — poll every 100 ms.
wait_for() {
    local ticks=$(( $1 * 10 )); shift
    while [ "$ticks" -gt 0 ]; do
        "$@" && return 0
        sleep 0.1
        ticks=$((ticks - 1))
    done
    "$@"
}
# shellcheck disable=SC2329  # both are called through wait_for
is_enabled() { [ "$(state)" = enabled ]; }
# shellcheck disable=SC2329
is_disabled() { [ "$(state)" = disabled ]; }

# One CPU hog per CPU plus two fork loops, for $1 seconds. Shell only: the
# point is runnable tasks on every CPU and a steady stream of new tasks, which
# exercises init_task/enable/select_cpu/enqueue/dispatch/exit paths.
run_load() {
    local secs="$1" i pids=()
    for ((i = 0; i < NCPU; i++)); do
        ( end=$((SECONDS + secs)); while [ "$SECONDS" -lt "$end" ]; do :; done ) &
        pids+=($!)
    done
    for i in 1 2; do
        ( end=$((SECONDS + secs)); while [ "$SECONDS" -lt "$end" ]; do /usr/bin/true; done ) &
        pids+=($!)
    done
    wait "${pids[@]}"
}

# Per-scheduler arguments. Only schedulers that cannot start bare are listed.
args_for() {
    case "$1" in
        scx_layered) echo "--run-example" ;;   # needs a layer spec; this is upstream's built-in one
        scx_flow) echo "--no-webui" ;;         # do not open a listening socket
        *) echo "" ;;
    esac
}

PASSED=0
FAILED=0
TOTAL=0

test_one() {
    local name="$1" bin="/usr/bin/$1" args pid k0 seq0 st o fail="" t0 t1 lines
    TOTAL=$((TOTAL + 1))
    args="$(args_for "$name")"
    if [ ! -x "$bin" ]; then
        say "sched=${name} result=FAIL reason=binary_missing"; FAILED=$((FAILED + 1)); return
    fi
    if ! wait_for "$SETTLE" is_disabled; then
        say "sched=${name} result=FAIL reason=sched_ext_not_idle_before_start_($(state))"
        FAILED=$((FAILED + 1)); return
    fi
    k0="$(kmsg_lines)"
    t0="$(date +%s%N 2>/dev/null || echo 0)"
    # shellcheck disable=SC2086 # args is a word list on purpose
    "$bin" $args > "/tmp/${name}.out" 2>&1 &
    pid=$!
    if ! wait_for "$SETTLE" is_enabled; then
        st="$(state)"
        kill -INT "$pid" 2>/dev/null; sleep 1; kill -KILL "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
        lines="$(tail -n 3 "/tmp/${name}.out" 2>/dev/null | tr '\n' '|')"
        say "sched=${name} result=FAIL reason=not_enabled_within_${SETTLE}s state:${st} out:$(clean "$lines") kmsg:$(clean "$(kmsg_since "$k0" | tail -n 2 | tr '\n' '|')")"
        FAILED=$((FAILED + 1)); return
    fi
    t1="$(date +%s%N 2>/dev/null || echo 0)"
    o="$(ops)"
    seq0="$(seq_)"
    if ! ops_matches "$o" "$name"; then fail="ops_mismatch_(${o})"; fi

    if [ -z "$fail" ]; then
        run_load "$LOAD"
        st="$(state)"
        if [ "$st" != enabled ]; then fail="ejected_under_load_(state:${st})"
        elif [ "$(ops)" != "$o" ]; then fail="ops_changed_under_load_($(ops))"
        elif [ "$(seq_)" != "$seq0" ]; then fail="enable_seq_moved_${seq0}->$(seq_)_(ejected_and_reloaded)"
        elif ! kill -0 "$pid" 2>/dev/null; then fail="process_exited_under_load"
        fi
        if [ -z "$fail" ]; then
            local bad
            bad="$(kmsg_since "$k0" | grep -iE 'disabled|error|stall|watchdog' | grep -v ' enabled' | head -n 1)"
            [ -n "$bad" ] && fail="kmsg_during_load:$(clean "$bad")"
        fi
    fi

    # Stop it the way scx_loader does, and read back that the kernel let go.
    local k1; k1="$(kmsg_lines)"
    kill -INT "$pid" 2>/dev/null
    local ticks=$((SETTLE * 10))
    while kill -0 "$pid" 2>/dev/null && [ "$ticks" -gt 0 ]; do sleep 0.1; ticks=$((ticks - 1)); done
    if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null
        [ -z "$fail" ] && fail="did_not_exit_on_SIGINT_within_${SETTLE}s"
    fi
    wait "$pid" 2>/dev/null
    if ! wait_for "$SETTLE" is_disabled; then
        [ -z "$fail" ] && fail="state_not_disabled_after_stop_($(state))"
    elif [ -z "$fail" ]; then
        sleep 0.2
        local why
        why="$(kmsg_since "$k1" | grep -i 'disabled' | head -n 1)"
        case "$why" in
            *"unregistered from user space"*) : ;;
            "") fail="no_disable_record_in_kmsg_after_stop" ;;
            *) fail="unexpected_disable_reason:$(clean "$why")" ;;
        esac
    fi

    local ms=0
    [ "$t0" != 0 ] && [ "$t1" != 0 ] && ms=$(( (t1 - t0) / 1000000 ))
    if [ -z "$fail" ]; then
        say "sched=${name} result=PASS ops=${o} attach_ms=${ms} load_s=${LOAD}"
        PASSED=$((PASSED + 1))
    else
        say "sched=${name} result=FAIL reason=$(clean "$fail") ops=${o:-none}"
        FAILED=$((FAILED + 1))
        sed 's/^/  | /' "/tmp/${name}.out" 2>/dev/null | tail -n 15
    fi
}

# SIGKILL the userspace half: the kernel must disable the BPF scheduler by
# itself and every task must be back on the fair class.
test_kill9() {
    local name="$1" bin="/usr/bin/$1" args pid fail="" o
    args="$(args_for "$name")"
    wait_for "$SETTLE" is_disabled || true
    # shellcheck disable=SC2086
    "$bin" $args > "/tmp/${name}.kill9.out" 2>&1 &
    pid=$!
    if ! wait_for "$SETTLE" is_enabled; then
        kill -KILL "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
        say "kill9=${name} result=FAIL reason=not_enabled_within_${SETTLE}s"
        return 1
    fi
    o="$(ops)"
    # A task started while the BPF scheduler is attached runs on it (switch_all
    # is the default for every shipped scheduler); /proc/<pid>/sched says so.
    local on_ext=""
    on_ext="$(grep -E '^ext\.enabled' /proc/self/sched 2>/dev/null | tr -d ' ' | cut -d: -f2)"
    kill -KILL "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    if ! wait_for "$SETTLE" is_disabled; then
        fail="state_not_disabled_after_SIGKILL_($(state))"
    else
        local after=""
        after="$(grep -E '^ext\.enabled' /proc/self/sched 2>/dev/null | tr -d ' ' | cut -d: -f2)"
        if [ -n "$after" ] && [ "$after" != 0 ]; then
            fail="task_still_on_ext_class_after_disable"
        fi
        # And the machine still schedules: a one-second load on the fair class
        # must finish (a wedged runqueue would hang here until the outer timeout).
        run_load 1
        [ -z "$on_ext" ] && on_ext="unpublished"
        [ -z "$after" ] && after="unpublished"
        if [ -z "$fail" ]; then
            say "kill9=${name} result=PASS ops=${o} ext_enabled_before=${on_ext} ext_enabled_after=${after} state=$(state)"
            return 0
        fi
    fi
    say "kill9=${name} result=FAIL reason=$(clean "$fail") ops=${o}"
    return 1
}

SCHEDS=()
while IFS= read -r s; do
    case "$s" in ''|'#'*) continue ;; esac
    SCHEDS+=("$s")
done < /selftest/schedulers.txt
say "schedulers=${#SCHEDS[@]} list=$(IFS=,; echo "${SCHEDS[*]}")"
for s in "${SCHEDS[@]}"; do
    test_one "$s"
done

K9="$(rd /selftest/kill9.txt)"
[ -z "$K9" ] && K9="${SCHEDS[0]:-}"
if [ -n "$K9" ]; then
    if test_kill9 "$K9"; then PASSED=$((PASSED + 1)); else FAILED=$((FAILED + 1)); fi
    TOTAL=$((TOTAL + 1))
fi

kill "$KMSG_PID" 2>/dev/null
if [ "$FAILED" -eq 0 ] && [ "$TOTAL" -gt 0 ]; then V=PASS; else V=FAIL; fi
say "verdict=${V} passed=${PASSED} failed=${FAILED} total=${TOTAL}"
poweroff_now
