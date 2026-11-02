#!/usr/bin/env bash
# run-pinned.sh DRIVER LOG CPUS HOGS: run a built NixOS test driver with QEMU (and the driver) pinned to CPUS (a
# taskset list, e.g. 4,5), while HOGS busy loops pinned to the same CPUs compete with it. Prints rc=N.
#
# What a VM check meets on the 4-vCPU hosted runner, on a fast machine: run it before trusting a VM test that waits
# for the guest (docs/nixos/ci.md, "keymap-luks under load"). It builds nothing. DRIVER is the check's `driver`
# attribute (nix build path:.#checks.x86_64-linux.<check>.driver), run as bin/nixos-test-driver in a fresh
# directory under RIME_PINNED_WORK (default $TMPDIR or /tmp), deleted afterwards. Run it where Nix and /dev/kvm are
# (here: inside the rime-nix container). taskset comes from PATH, or from the pinned nixpkgs util-linux.
set -u
[ $# = 4 ] || { echo "usage: $0 DRIVER LOG CPUS HOGS" >&2; exit 2; }
if ! command -v taskset > /dev/null; then
    exec nix shell github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#util-linux --command bash "$0" "$@"
fi
drv=$1 log=$2 cpus=$3 hogs=$4
[ -x "$drv/bin/nixos-test-driver" ] || { echo "$drv/bin/nixos-test-driver: not a built test driver" >&2; exit 2; }
work=$(mktemp -d "${RIME_PINNED_WORK:-${TMPDIR:-/tmp}}/rime-pinned.XXXXXX")
pids=()
for _ in $(seq "$hogs"); do
    taskset -c "$cpus" bash -c 'while :; do :; done' &
    pids+=($!)
done
echo "hogs: ${pids[*]} on CPUs $cpus; load before: $(cut -d' ' -f1-3 /proc/loadavg)" > "$log"
start=$(date +%s)
( cd "$work" && TMPDIR=$work taskset -c "$cpus" timeout 3000 "$drv/bin/nixos-test-driver" ) >> "$log" 2>&1
rc=$?
echo "driver rc=$rc after $(( $(date +%s) - start )) s; load after: $(cut -d' ' -f1-3 /proc/loadavg)" >> "$log"
kill "${pids[@]}" 2>/dev/null
wait "${pids[@]}" 2>/dev/null
rm -rf "$work"
echo "rc=$rc"
