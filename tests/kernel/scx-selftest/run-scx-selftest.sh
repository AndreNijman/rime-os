#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  run-scx-selftest.sh — boot the SHIPPED kernel in QEMU and load every SHIPPED
#  sched-ext scheduler on it, under load, before an image may publish.
#
#  ═══ WHY ═══
#
#  sched_ext's BPF-facing API is not stable across kernel versions; upstream
#  says so. Rime builds its own kernel (Containerfile.kernel) and its own
#  scheduler set (Containerfile.core's scx-builder stage), and the two only
#  meet on a user's machine — where a scheduler that cannot attach, or that the
#  kernel's watchdog ejects, is a Gaming Mode that silently is not one. Every
#  kernel bump changes core's pinned kernel digest, which rebuilds core, which
#  runs this, so the pair is proven together before either reaches a user.
#
#  A compile is not this proof. The BPF in each scheduler is compiled against
#  the shipped kernel's own BTF (scx-builder does that), but the kfunc
#  prototypes are deliberately not taken from it (BPF_NO_KFUNC_PROTOTYPES in
#  scx's common.bpf.h) and the verifier only runs at load. Only loading it on
#  the kernel finds out.
#
#  ═══ HOW ═══
#
#  1. collect.sh runs INSIDE the image under test and packs its vmlinuz, every
#     /usr/bin/scx_* scheduler and their libraries into a small initramfs.
#  2. QEMU boots that kernel with rdinit=/init (init.sh): no disk, no systemd,
#     no scx_loader — each scheduler is started, loaded, exercised and stopped
#     directly, and the guest prints one line per scheduler and a verdict.
#  3. This script parses the serial log and exits non-zero on any failure,
#     a missing scheduler line, a missing verdict, or a timeout.
#
#  NEVER run a sched-ext scheduler on the machine running this. It is the
#  guest's kernel that is tested, and only the guest's.
#
#  usage:
#    run-scx-selftest.sh --image REF [--podman CMD] [--kernel-rpm-dir DIR]
#                        [--workdir DIR] [--load S] [--kill9 NAME]
#    run-scx-selftest.sh --kernel VMLINUZ --initrd INITRD --expect LIST
#    run-scx-selftest.sh --parse-only SERIAL_LOG --expect LIST
#
#  --podman      the podman to run the image with (default: podman; CI passes
#                "sudo podman" because its images live in root storage).
#  --kernel-rpm-dir  take vmlinuz from the kernel-cachyos-core RPM in DIR
#                instead of the image's /usr/lib/modules (for an image that has
#                schedulers but no installed kernel).
#  QEMU          env: the qemu binary (default qemu-system-x86_64).
#  SCX_SELFTEST_ACCEL  env: kvm | tcg (default: kvm when /dev/kvm is usable).
#
#  Exit: 0 every scheduler passed; 1 a failure; 2 usage; 3 could not run.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

IMAGE="" PODMAN="podman" KRPMDIR="" WORK="" LOAD=5 KILL9=""
KERNEL="" INITRD="" EXPECT="" PARSE_ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --image) IMAGE="$2"; shift 2 ;;
        --podman) PODMAN="$2"; shift 2 ;;
        --kernel-rpm-dir) KRPMDIR="$2"; shift 2 ;;
        --workdir) WORK="$2"; shift 2 ;;
        --load) LOAD="$2"; shift 2 ;;
        --kill9) KILL9="$2"; shift 2 ;;
        --kernel) KERNEL="$2"; shift 2 ;;
        --initrd) INITRD="$2"; shift 2 ;;
        --expect) EXPECT="$2"; shift 2 ;;
        --parse-only) PARSE_ONLY="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^# ──*$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "run-scx-selftest: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
case "$LOAD" in ''|*[!0-9]*) echo "run-scx-selftest: --load must be whole seconds" >&2; exit 2 ;; esac

summary() { [ -n "${GITHUB_STEP_SUMMARY:-}" ] && echo "$*" >> "$GITHUB_STEP_SUMMARY"; return 0; }

# ── the verdict, from a serial log ──────────────────────────────────────────
# Kept separate so it can be tested without a guest (tests/test-scx-selftest.sh
# feeds it canned logs). A PASS needs ALL of: a verdict line saying PASS, one
# PASS line per expected scheduler, a PASS kill -9 line, and totals that agree.
# Anything else, including silence, is a failure.
parse_log() {
    local log="$1" expect="$2" rc=0 n want got verdict total
    [ -r "$log" ] || { echo "FAIL: no serial log at $log"; return 1; }
    [ -r "$expect" ] || { echo "FAIL: no scheduler list at $expect"; return 1; }
    mapfile -t want < <(grep -vE '^\s*(#|$)' "$expect")
    if [ "${#want[@]}" -eq 0 ]; then echo "FAIL: the expected scheduler list is empty"; return 1; fi
    # \r: a serial console line ends CRLF.
    local lines; lines="$(tr -d '\r' < "$log" | grep -oE 'SCX-SELFTEST .*' || true)"
    for n in "${want[@]}"; do
        got="$(grep -E "^SCX-SELFTEST sched=${n} result=" <<<"$lines" || true)"
        if [ -z "$got" ]; then
            echo "FAIL: ${n}: no result line (the guest never reached it, or crashed)"; rc=1
        elif [ "$(grep -c '' <<<"$got")" -ne 1 ]; then
            echo "FAIL: ${n}: more than one result line"; rc=1
        elif grep -E ' result=PASS( |$)' <<<"$got" >/dev/null; then
            echo "PASS: ${got#SCX-SELFTEST }"
        else
            echo "FAIL: ${got#SCX-SELFTEST }"; rc=1
        fi
    done
    # A scheduler the guest tested that the list did not name means the two
    # disagree about what shipped; that is a fault in this harness, not a pass.
    while read -r got; do
        [ -n "$got" ] || continue
        n="${got#SCX-SELFTEST sched=}"; n="${n%% *}"
        local known=0 w
        for w in "${want[@]}"; do [ "$w" = "$n" ] && known=1; done
        [ "$known" = 1 ] || { echo "FAIL: guest tested ${n}, which is not in the expected list"; rc=1; }
    done < <(grep -E '^SCX-SELFTEST sched=' <<<"$lines" || true)
    got="$(grep -E '^SCX-SELFTEST kill9=' <<<"$lines" || true)"
    if [ -z "$got" ]; then
        echo "FAIL: kill -9 fallback: no result line"; rc=1
    elif grep -E ' result=PASS( |$)' <<<"$got" >/dev/null; then
        echo "PASS: ${got#SCX-SELFTEST }"
    else
        echo "FAIL: ${got#SCX-SELFTEST }"; rc=1
    fi
    verdict="$(grep -E '^SCX-SELFTEST verdict=' <<<"$lines" | tail -n 1 || true)"
    if [ -z "$verdict" ]; then
        echo "FAIL: no verdict line (guest timed out, panicked or never booted)"; rc=1
    else
        echo "guest: ${verdict#SCX-SELFTEST }"
        grep -E ' verdict=PASS ' <<<" ${verdict#SCX-SELFTEST}" >/dev/null || rc=1
        total="$(sed -nE 's/.* total=([0-9]+).*/\1/p' <<<"$verdict")"
        if [ "${total:-x}" != "$(( ${#want[@]} + 1 ))" ]; then
            echo "FAIL: guest counted ${total:-?} checks, expected $(( ${#want[@]} + 1 )) (${#want[@]} schedulers + kill -9)"
            rc=1
        fi
    fi
    if [ "$rc" = 0 ]; then echo "VERDICT: PASS — ${#want[@]} schedulers + kill -9 fallback"; else echo "VERDICT: FAIL"; fi
    return "$rc"
}

if [ -n "$PARSE_ONLY" ]; then
    [ -n "$EXPECT" ] || { echo "run-scx-selftest: --parse-only needs --expect LIST" >&2; exit 2; }
    parse_log "$PARSE_ONLY" "$EXPECT"
    exit $?
fi

# ── build the guest out of the image ────────────────────────────────────────
if [ -z "$WORK" ]; then WORK="$(mktemp -d "${TMPDIR:-/var/tmp}/scx-selftest.XXXXXX")"; fi
mkdir -p "$WORK"
if [ -n "$IMAGE" ]; then
    vols=(-v "$HERE:/scx-selftest:ro" -v "$WORK:/out")
    cargs=()
    if [ -n "$KRPMDIR" ]; then
        mapfile -t k < <(find "$KRPMDIR" -maxdepth 1 -name 'kernel-cachyos-core-*.rpm')
        [ "${#k[@]}" -eq 1 ] || { echo "FATAL: expected one kernel-cachyos-core RPM in $KRPMDIR, found ${#k[@]}"; exit 3; }
        vols+=(-v "$KRPMDIR:/krpms:ro")
        cargs+=(--kernel-rpm "/krpms/$(basename "${k[0]}")")
    fi
    [ -n "$KILL9" ] && cargs+=(--kill9 "$KILL9")
    # label=disable: bind mounts from a SELinux host without relabelling the
    # source tree. --net=none: collecting needs nothing from the network.
    # shellcheck disable=SC2086 # PODMAN may be "sudo podman"
    $PODMAN run --rm --net=none --security-opt label=disable --entrypoint /bin/bash \
        "${vols[@]}" "$IMAGE" /scx-selftest/collect.sh /out "${cargs[@]}" \
        || { echo "FATAL: could not build the selftest guest from $IMAGE"; exit 3; }
    # Root podman leaves root-owned files; make them ours to read and delete.
    case "$PODMAN" in sudo*) sudo chown -R "$(id -u):$(id -g)" "$WORK" ;; esac
    KERNEL="$WORK/vmlinuz"; INITRD="$WORK/initramfs.cpio.gz"; EXPECT="$WORK/schedulers.txt"
fi
[ -n "$KERNEL" ] && [ -n "$INITRD" ] && [ -n "$EXPECT" ] \
    || { echo "run-scx-selftest: need --image, or --kernel + --initrd + --expect" >&2; exit 2; }

# ── boot it ─────────────────────────────────────────────────────────────────
QEMU="${QEMU:-qemu-system-x86_64}"
command -v "$QEMU" >/dev/null || { echo "COULD-NOT-RUN: $QEMU is not installed"; exit 3; }
ACCEL="${SCX_SELFTEST_ACCEL:-}"
if [ -z "$ACCEL" ]; then
    if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
NSCHED="$(grep -cvE '^\s*(#|$)' "$EXPECT")"
if [ "$ACCEL" = kvm ]; then
    accel=(-accel kvm -cpu host); SETTLE=10
else
    # TCG is 10-50x slower; the waits scale, the assertions do not.
    accel=(-accel tcg -cpu max); SETTLE=60
fi
# Boot, then per scheduler: start (<= SETTLE) + load + stop (<= 2*SETTLE), and
# the kill -9 case. Generous: the timeout is the failure mode for a hang, not a
# performance target.
TIMEOUT=$(( 120 + (NSCHED + 1) * (3 * SETTLE + LOAD + 10) ))
SMP="${SCX_SELFTEST_SMP:-8}"
LOG="$WORK/serial.log"
echo "scx-selftest: ${NSCHED} schedulers, accel=${ACCEL}, smp=${SMP}, load=${LOAD}s, timeout=${TIMEOUT}s"
echo "scx-selftest: kernel $(file -b "$KERNEL" 2>/dev/null | sed -n 's/.*version \([^ ]*\).*/\1/p')"
summary "### sched-ext selftest (${ACCEL}, ${NSCHED} schedulers)"
start="$(date +%s)"
set +e
timeout --foreground -k 10 "$TIMEOUT" "$QEMU" \
    -machine q35 "${accel[@]}" -smp "$SMP" -m 2048 \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append "console=ttyS0 rdinit=/init panic=-1 loglevel=3 scx_selftest.load=${LOAD} scx_selftest.settle=${SETTLE}" \
    -display none -monitor none -serial "file:$LOG" -no-reboot -nodefaults
qrc=$?
set -e
echo "scx-selftest: qemu exited ${qrc} after $(( $(date +%s) - start ))s"
[ "$qrc" = 124 ] && echo "FAIL: the guest did not finish within ${TIMEOUT}s"
echo "──── guest serial (selftest lines) ────"
tr -d '\r' < "$LOG" | grep -E 'SCX-SELFTEST|  \| |Kernel panic|BUG:|sched_ext' || true
echo "───────────────────────────────────────"
set +e
out="$(parse_log "$LOG" "$EXPECT")"
rc=$?
set -e
echo "$out"
summary '```'
summary "$out"
summary '```'
[ "$qrc" = 124 ] && rc=1
exit "$rc"
