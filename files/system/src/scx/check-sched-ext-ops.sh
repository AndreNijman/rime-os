#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-sched-ext-ops.sh — is the scx release's BPF header at least as new as
#  the kernel it will run on?
#
#  usage: check-sched-ext-ops.sh BUNDLED_VMLINUX_H KERNEL_VMLINUX_H
#
#  scx compiles its BPF against the vmlinux.h it ships (rust/scx_utils/
#  vmlinux.tar.zst) and adapts to the RUNNING kernel at load time: CO-RE
#  relocations, plus compat checks that zero any sched_ext_ops field the kernel
#  lacks. That only works in one direction. A header NEWER than the kernel is
#  the supported case (scx 1.1.4's v7.3-rc5 header carries 8 sched_ext_ops
#  members the 7.2.9 kernel lacks, and its loader drops them). A kernel newer than the header is not: a member
#  the header has never heard of is a member no scheduler fills in, and nothing
#  at build time says so.
#
#  So, for `struct sched_ext_ops` (the interface every scheduler implements):
#    • every member of the KERNEL's struct must exist in the bundled header,
#      or this exits 1 naming them (a kernel bump that outran the scx pin);
#    • members only the header has are printed, one per line, prefixed
#      `header-only:`. Those are the ones the loader must drop at load time.
#
#  The kernel header is `bpftool btf dump file <vmlinux.btf> format c` of the
#  SHIPPED kernel. Exit 2 = a header has no sched_ext_ops at all.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

BUNDLED="${1:?usage: check-sched-ext-ops.sh BUNDLED_VMLINUX_H KERNEL_VMLINUX_H}"
KERNEL="${2:?usage: check-sched-ext-ops.sh BUNDLED_VMLINUX_H KERNEL_VMLINUX_H}"

# The member names of the top-level `struct sched_ext_ops { … };` in a
# bpftool-style header: one tab of indent per member, function pointers as
# `(*name)(…)`, arrays as `name[N]`, bitfields as `name: N`.
members() {
    awk '
        /^struct sched_ext_ops \{$/ { in_ops = 1; next }
        in_ops && /^\};/            { exit }
        in_ops && /^\t[^\t]/ {
            line = $0
            if (match(line, /\(\*[A-Za-z_][A-Za-z_0-9]*\)/)) {
                print substr(line, RSTART + 2, RLENGTH - 3); next
            }
            sub(/;.*$/, "", line)
            sub(/:[ 0-9]+$/, "", line)
            gsub(/\[[^]]*\]/, "", line)
            n = split(line, w, /[ \t*]+/)
            if (n > 0 && w[n] != "") print w[n]
        }
    ' "$1" | sort -u
}

b="$(members "$BUNDLED")"
k="$(members "$KERNEL")"
[ -n "$b" ] || { echo "FATAL: $BUNDLED has no struct sched_ext_ops"; exit 2; }
[ -n "$k" ] || { echo "FATAL: $KERNEL has no struct sched_ext_ops"; exit 2; }

missing="$(comm -13 <(printf '%s\n' "$b") <(printf '%s\n' "$k"))"
extra="$(comm -23 <(printf '%s\n' "$b") <(printf '%s\n' "$k"))"

echo "sched_ext_ops: kernel $(wc -l <<<"$k") members, bundled header $(wc -l <<<"$b")"
while IFS= read -r m; do [ -n "$m" ] && echo "header-only: $m"; done <<<"$extra"
if [ -n "$missing" ]; then
    echo "FATAL: the kernel's sched_ext_ops has members the scx header does not:"
    while IFS= read -r m; do echo "kernel-only: $m"; done <<<"$missing"
    echo "The kernel is newer than this scx release. Move the scx pin forward."
    exit 1
fi
echo "OK: every member of the kernel's sched_ext_ops is in the bundled header"
