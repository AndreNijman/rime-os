#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-scx-sched-ext-ops-check.sh — the build-time check that the scx
#  release's BPF header is at least as new as the kernel Rime ships
#  (files/system/src/scx/check-sched-ext-ops.sh, run by Containerfile.core's
#  scx-builder stage against the kernel tier's own BTF).
#
#  The fixtures are cut down from the real headers, in bpftool's exact layout:
#  the 7.2.9 kernel's sched_ext_ops ends with cpu_acquire/cpu_release, and scx
#  1.1.3's bundled header adds the v7.3 rescue_* fields. Asserted:
#    • header newer than the kernel passes and names the header-only members;
#    • a kernel member the header lacks fails and names it (the case the check
#      exists for: a kernel bump that outran the scx pin);
#    • function pointers, arrays and plain members are all read as members;
#    • a header with no sched_ext_ops at all is an error, not a pass.
#
#  Run from anywhere: ./tests/test-scx-sched-ext-ops-check.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 2

CHECK=../files/system/src/scx/check-sched-ext-ops.sh
[ -f "$CHECK" ] || { echo "cannot find $CHECK"; exit 2; }

T=$(mktemp -d /tmp/rime-scx-ops-check.XXXXXX)
trap 'rm -rf "$T"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

# hdr FILE EXTRA-MEMBER-LINES… → a bpftool-shaped header
hdr() {
    local f="$1"; shift
    {
        printf 'struct scx_exit_info {\n\tint kind;\n};\n\n'
        printf 'struct sched_ext_ops {\n'
        printf '\ts32 (*select_cpu)(struct task_struct *, s32, u64);\n'
        printf '\tvoid (*enqueue)(struct task_struct *, u64);\n'
        printf '\tvoid (*exit)(struct scx_exit_info *);\n'
        printf '\tu32 dispatch_max_batch;\n'
        printf '\tu64 flags;\n'
        printf '\tchar name[128];\n'
        printf '\tvoid *priv;\n'
        local l; for l in "$@"; do printf '\t%s\n' "$l"; done
        printf '};\n\n'
        # A later struct whose members must not be read as sched_ext_ops's.
        printf 'struct sched_ext_entity {\n\tu32 weight;\n};\n'
    } > "$f"
}

hdr "$T/kernel.h" 'void (*cpu_acquire)(s32, struct scx_cpu_acquire_args *);'
hdr "$T/bundled.h" 'void (*cpu_acquire)(s32, struct scx_cpu_acquire_args *);' \
    'u32 rescue_bandwidth_ppt;' 'u32 rescue_quantum_us;'

# 1. the shipped shape: header newer than the kernel
out="$(bash "$CHECK" "$T/bundled.h" "$T/kernel.h" 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a header newer than the kernel passes"; else bad "a header newer than the kernel passes" "rc=$rc: $out"; fi
if grep -x 'header-only: rescue_bandwidth_ppt' <<<"$out" >/dev/null \
    && grep -x 'header-only: rescue_quantum_us' <<<"$out" >/dev/null; then
    ok "header-only members are named"
else bad "header-only members are named" "$out"; fi
if grep -F 'kernel 8 members, bundled header 10' <<<"$out" >/dev/null; then
    ok "function pointers, arrays and plain members all count (8 and 10)"
else bad "function pointers, arrays and plain members all count" "$out"; fi
if grep -E 'header-only: (weight|kind)$' <<<"$out" >/dev/null; then
    bad "other structs are not read as sched_ext_ops" "$out"
else ok "other structs are not read as sched_ext_ops"; fi

# 2. a kernel that outran the scx pin
hdr "$T/newer-kernel.h" 'void (*cpu_acquire)(s32, struct scx_cpu_acquire_args *);' \
    's32 (*sub_new_op)(struct task_struct *);' 'u64 new_knob;'
out="$(bash "$CHECK" "$T/bundled.h" "$T/newer-kernel.h" 2>&1)"; rc=$?
if [ "$rc" = 1 ]; then ok "a kernel member the header lacks fails"; else bad "a kernel member the header lacks fails" "rc=$rc: $out"; fi
if grep -x 'kernel-only: sub_new_op' <<<"$out" >/dev/null && grep -x 'kernel-only: new_knob' <<<"$out" >/dev/null; then
    ok "the missing members are named"
else bad "the missing members are named" "$out"; fi

# 3. a header with no sched_ext_ops is an error
printf 'struct task_struct {\n\tint pid;\n};\n' > "$T/none.h"
out="$(bash "$CHECK" "$T/bundled.h" "$T/none.h" 2>&1)"; rc=$?
if [ "$rc" = 2 ]; then ok "a kernel header without sched_ext_ops is an error"; else bad "a kernel header without sched_ext_ops is an error" "rc=$rc: $out"; fi
out="$(bash "$CHECK" "$T/none.h" "$T/kernel.h" 2>&1)"; rc=$?
if [ "$rc" = 2 ]; then ok "a bundled header without sched_ext_ops is an error"; else bad "a bundled header without sched_ext_ops is an error" "rc=$rc: $out"; fi

echo
echo "check-sched-ext-ops: $pass passed, $fail failed"
[ "$fail" = 0 ]
