#!/usr/bin/env bash
# Stress one VM test: run its driver N times AT ONCE, each in its own TMPDIR, and report how each ended. Used to
# prove the virtiofsd fix (docs/nixos/p1-exit.md, "virtiofsd race"). Run inside the rime-nix container as root
# (the drivers need /dev/kvm), from the flake directory:
#
#   nix/tests/lib/stress.sh DRIVER N OUTDIR [LIMIT_S]
#
#   DRIVER   the test's driver, built ONCE: nix build path:.#checks.x86_64-linux.update-client.driver
#            (evaluating N renamed copies of a test instead costs N full evaluations: 14 GB and rising for 6
#            update-client copies, measured 2026-11-02, so this runs one derivation N times)
#   OUTDIR   gets run<i>.log and summary.txt: one line per run, ok / FAIL rc / HUNG
#   LIMIT_S  wall-clock limit per run (default 1800). A run still going at the limit is a HANG: it is stopped, and
#            any virtiofsd or QEMU it left behind is killed by PID (never by pattern).
set -uo pipefail

driver=$1 n=$2 out=$3 limit=${4:-1800}
mkdir -p "$out"
pids=()
for i in $(seq 1 "$n"); do
    d=$(mktemp -d "$out/run$i.XXXXXX")
    mkdir -p "$d/out"
    (
        start=$SECONDS
        TMPDIR=$d timeout --kill-after=30 "$limit" "$driver/bin/nixos-test-driver" -o "$d/out" > "$out/run$i.log" 2>&1
        rc=$?
        echo "rc=$rc after $((SECONDS - start)) s" >> "$out/run$i.log"
    ) &
    pids+=($!)
done
wait "${pids[@]}"

for i in $(seq 1 "$n"); do
    l=$out/run$i.log
    rc=$(tail -n 1 "$l" | awk -F'[= ]' '/^rc=/ { print $2 }')
    # VM starts where a socket was not listening yet when the run script exec'd QEMU: races QEMU would have lost
    waited=$(grep -ac 'rime-qemu: .* not listening yet' "$l")
    if [ "$rc" = 0 ]; then
        echo "run$i: ok ($(tail -n 1 "$l" | cut -d' ' -f2-), a socket was late in $waited of its VM starts)"
    elif [ "$rc" = 124 ] || [ "$rc" = 137 ]; then
        echo "run$i: HUNG (stopped at $limit s)"
    else
        echo "run$i: FAIL rc=$rc"
    fi
    grep -a 'Failed to connect\|is not listening after' "$l" | head -3
done | tee "$out/summary.txt"
# What a stopped run left behind (virtiofsd, and QEMU when the driver was killed), by PID: their command lines name
# a socket or disk under that run's TMPDIR.
for p in /proc/[0-9]*; do
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    case "$exe" in */virtiofsd | */qemu-system-x86_64 | */.qemu-system-x86_64-wrapped) ;; *) continue ;; esac
    if tr '\0' ' ' < "$p/cmdline" | grep -F -- "$out/run" > /dev/null; then
        echo "left behind: ${exe##*/} ${p#/proc/}, killed" | tee -a "$out/summary.txt"
        kill "${p#/proc/}"
    fi
done
grep -c '^run.*: ok' "$out/summary.txt" | awk -v n="$n" '{ print $1 " of " n " ok" }' | tee -a "$out/summary.txt"
