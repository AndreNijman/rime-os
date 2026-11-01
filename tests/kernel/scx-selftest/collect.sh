#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
#  collect.sh — build the sched-ext selftest's kernel + initramfs FROM INSIDE
#  the image under test.
#
#  Run inside the image (run-scx-selftest.sh does this with `podman run`), so
#  every file comes from the image itself: its vmlinuz, its scheduler binaries
#  and the exact shared libraries its own `ldd` resolves them to. Nothing from
#  the machine running the test can leak into what is tested.
#
#  usage: collect.sh OUTDIR [--kernel-rpm RPM] [--kill9 NAME]
#
#  Writes OUTDIR/vmlinuz, OUTDIR/initramfs.cpio.gz and OUTDIR/schedulers.txt.
#
#  The scheduler set is DISCOVERED, not listed: every /usr/bin/scx_* except
#  scx_loader (the D-Bus loader, not a scheduler). A scheduler added to the
#  image is therefore tested without anyone remembering to add it here, and one
#  removed stops being expected.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

OUT="${1:?usage: collect.sh OUTDIR [--kernel-rpm RPM] [--kill9 NAME]}"
shift
KRPM=""
KILL9=""
while [ $# -gt 0 ]; do
    case "$1" in
        --kernel-rpm) KRPM="$2"; shift 2 ;;
        --kill9) KILL9="$2"; shift 2 ;;
        *) echo "collect.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$OUT"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# ── the kernel ───────────────────────────────────────────────────────────────
if [ -n "$KRPM" ]; then
    x="$(mktemp -d)"
    # The kernel RPM puts it under /lib/modules (Fedora's /lib is /usr/lib,
    # so the installed path is /usr/lib/modules); accept either spelling.
    (cd "$x" && rpm2cpio "$KRPM" | cpio -idm --quiet './lib/modules/*/vmlinuz' './usr/lib/modules/*/vmlinuz')
    mapfile -t vz < <(find "$x" -path '*/lib/modules/*/vmlinuz' -type f)
else
    mapfile -t vz < <(find /usr/lib/modules -mindepth 2 -maxdepth 2 -name vmlinuz)
fi
if [ "${#vz[@]}" -ne 1 ]; then
    echo "FATAL: expected exactly one vmlinuz, found ${#vz[@]}: ${vz[*]:-none}" >&2
    exit 1
fi
cp "${vz[0]}" "$OUT/vmlinuz"
echo "kernel: ${vz[0]}"

# ── the schedulers ───────────────────────────────────────────────────────────
: > "$OUT/schedulers.txt"
for b in /usr/bin/scx_*; do
    [ -x "$b" ] || continue
    n="$(basename "$b")"
    [ "$n" = scx_loader ] && continue
    echo "$n" >> "$OUT/schedulers.txt"
done
N="$(wc -l < "$OUT/schedulers.txt")"
[ "$N" -gt 0 ] || { echo "FATAL: no /usr/bin/scx_* scheduler in this image" >&2; exit 1; }
echo "schedulers ($N): $(tr '\n' ' ' < "$OUT/schedulers.txt")"
if [ -z "$KILL9" ]; then
    # Gaming Mode's scheduler when present: it is the one users load.
    if grep -qx scx_lavd "$OUT/schedulers.txt"; then KILL9=scx_lavd; else KILL9="$(head -n 1 "$OUT/schedulers.txt")"; fi
fi

# ── the userland that drives them ───────────────────────────────────────────
mkdir -p "$ROOT"/usr/bin "$ROOT"/usr/lib64 "$ROOT"/proc "$ROOT"/sys "$ROOT"/dev \
         "$ROOT"/run "$ROOT"/tmp "$ROOT"/var "$ROOT"/selftest "$ROOT"/etc
ln -s usr/bin "$ROOT/bin"
ln -s usr/bin "$ROOT/sbin"
ln -s usr/lib64 "$ROOT/lib64"
ln -s lib64 "$ROOT/usr/lib"

TOOLS=(bash mount mkdir ln cat sleep grep date tail head tr cut sed sync true)
bins=()
for t in "${TOOLS[@]}"; do
    p="$(type -P "$t")" || { echo "FATAL: $t is not in this image" >&2; exit 1; }
    bins+=("$p")
done
while read -r n; do bins+=("/usr/bin/$n"); done < "$OUT/schedulers.txt"

for b in "${bins[@]}"; do
    install -m755 "$(readlink -f "$b")" "$ROOT/usr/bin/$(basename "$b")"
done
# Every library, as the image's own loader resolves it. `not found` is fatal:
# a scheduler that cannot link would fail in the guest for a reason that has
# nothing to do with the kernel, and be misread as an API break.
libs="$(mktemp)"
for b in "${bins[@]}"; do
    out="$(ldd "$b" 2>&1)" || true
    if grep 'not found' <<<"$out" >/dev/null; then
        echo "FATAL: $b has unresolved libraries:" >&2; echo "$out" >&2; exit 1
    fi
    awk '/=>/ {print $3} /^\s*\/.*ld-linux/ {print $1}' <<<"$out" >> "$libs"
done
sort -u "$libs" | while read -r l; do
    [ -n "$l" ] && [ -e "$l" ] || continue
    cp -L "$l" "$ROOT/usr/lib64/$(basename "$l")"
done
rm -f "$libs"

install -m755 "$HERE/init.sh" "$ROOT/init"
cp "$OUT/schedulers.txt" "$ROOT/selftest/schedulers.txt"
echo "$KILL9" > "$ROOT/selftest/kill9.txt"

(cd "$ROOT" && find . -print0 | cpio --null -o -H newc --quiet) | gzip -1 > "$OUT/initramfs.cpio.gz"
echo "initramfs: $(du -h "$OUT/initramfs.cpio.gz" | cut -f1), kill -9 target: $KILL9"
