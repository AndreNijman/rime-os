#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-store-compact.sh — the store compactor shrinks the store and
#  changes nothing else.
#
#  Runs files/system/libexec/rime-store-compact for real, as root, against a
#  throwaway btrfs on a loop device laid out like a Rime machine: an OS store
#  under sysroot/ostree/repo/objects with a deployment hardlinked into it, and a
#  Flatpak store. Objects are written the way libostree writes them, fallocated
#  first, which is exactly the case btrfs will not compress on its own.
#
#  What it holds:
#    - the objects come out compressed, byte-identical, still hardlinked, and
#      with no xattr added (an ostree bare repo checksums xattrs);
#    - an object that was already compressed is not rewritten (its physical
#      extent does not move), and neither is ostree metadata;
#    - a second run rewrites nothing; a new object is the only thing the next
#      run touches; a pruned object leaves the list;
#    - a batch that fails stays off the list and is retried;
#    - a store that is not on btrfs is left alone;
#    - /sysroot is made writable only inside the compactor's own namespace.
#
#  Needs root (sudo -n), btrfs-progs and filefrag. Without them it skips, and
#  RIME_COMPACT_REQUIRE=1 (set in CI) turns that skip into a failure.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 1
REPO=$(cd .. && pwd)
ENGINE="$REPO/files/system/libexec/rime-store-compact"

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip+1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }
is() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 — expected '$2', got '$3'"; fi; }
finish() { printf '\nstore-compact: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"; [ "$fail" -eq 0 ]; exit; }

section "structure"
if bash -n "$ENGINE"; then ok "the engine parses"; else bad "the engine parses"; fi
if [ -x "$ENGINE" ]; then ok "the engine is executable (it re-executes itself under unshare)"; else bad "the engine is executable"; fi
# What it may write to: the two object stores and its own state, nothing else.
targets=$(grep -v '^[[:space:]]*#' "$ENGINE" | grep -oE '^compact [a-z]+ "[^"]+"' | sort | tr '\n' ' ')
is "it compacts exactly the OS and Flatpak object stores" \
    'compact flatpak "$FLATPAK/repo/objects" compact os "$SYSROOT/ostree/repo/objects" ' "$targets"
# Image pruning acts on the RUNNING machine (ostree takes --sysroot /), so it
# must be gated on being pointed at the real /sysroot. Checked by reading, not
# by a mutant: a mutant run by this suite on a Rime machine would prune it.
if grep -v '^[[:space:]]*#' "$ENGINE" | grep -qF '[ "$SYSROOT" = /sysroot ] && [ -e /run/ostree-booted ] || return 0'; then
    ok "image pruning runs only against the machine's own /sysroot"
else
    bad "image pruning runs only against the machine's own /sysroot"
fi
if grep -v '^[[:space:]]*#' "$ENGINE" | grep -q -- "-name '\*\.file'"; then
    ok "only content objects (*.file) are rewritten, never ostree metadata"
else
    bad "only content objects (*.file) are rewritten, never ostree metadata"
fi

SUDO=""
[ "$(id -u)" = 0 ] || SUDO="sudo -n"
missing=""
$SUDO true 2>/dev/null || missing="root (sudo -n)"
for t in mkfs.btrfs btrfs filefrag losetup unshare getfattr; do
    command -v "$t" >/dev/null 2>&1 || PATH="$PATH:/usr/sbin:/sbin" command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
done
if [ -n "$missing" ]; then
    if [ "${RIME_COMPACT_REQUIRE:-0}" = 1 ]; then bad "the live half can run here (missing:$missing)"
    else skp "the live half needs:$missing"; fi
    finish
fi
export PATH="$PATH:/usr/sbin:/sbin"

T=$(mktemp -d /var/tmp/rime-compact-test.XXXXXX)
M="$T/mnt"; L=""
cleanup() {
    $SUDO umount -R "$M" 2>/dev/null
    [ -n "$L" ] && $SUDO losetup -d "$L" 2>/dev/null
    $SUDO rm -rf "$T"
}
trap cleanup EXIT
mkdir -p "$M"
truncate -s 1G "$T/img"
mkfs.btrfs -q "$T/img"
L=$($SUDO losetup -f --show "$T/img")
$SUDO mount "$L" "$M"

OS="$M/sysroot/ostree/repo/objects"; FP="$M/flatpak/repo/objects"; ST="$M/state"
hex() { printf '%s' "$1" | sha256sum | cut -c1-62; }
# object DIR NAME KIND: write one object the way libostree does (fallocate,
# then the data), KIND text (compressible), random, or tiny.
object() {
    local d f
    d="$1/$(hex "$2" | cut -c1-2)"
    f="$d/$(hex "$2").file"
    $SUDO mkdir -p "$d"
    case "$3" in
        text)   $SUDO fallocate -l 2M "$f"
                yes "rime store object $2" | head -c 2M | $SUDO dd of="$f" conv=notrunc status=none ;;
        random) $SUDO sh -c "head -c 1M /dev/urandom > '$f'" ;;
        mixed)  $SUDO sh -c "{ head -c 1M /dev/urandom; yes 'rime store object $2' | head -c 1M; } > '$f'" ;;
        tiny)   $SUDO sh -c "printf 'small object %s' '$2' > '$f'" ;;
    esac
    printf '%s' "$f"
}
encoded() {  # FILE: every extent compressed or inline
    $SUDO filefrag -v "$1" | awk '/^ *[0-9]+:/ { n++; if ($0 !~ /encoded|inline/) bad = 1 } END { exit (n == 0 || bad) }'
}
physical() { $SUDO filefrag -v "$1" | awk '/^ *0:/ { print $4; exit }'; }
digest() { $SUDO sha256sum "$1" | cut -d' ' -f1; }
run() { $SUDO env RIME_COMPACT_NAMESPACED=1 RIME_COMPACT_SYSROOT="$M/sysroot" \
            RIME_COMPACT_FLATPAK="$M/flatpak" RIME_COMPACT_STATE="$ST" "$ENGINE" 2>&1; }

a=$(object "$OS" a text); b=$(object "$OS" b text); c=$(object "$OS" c text)
r=$(object "$OS" r random); t=$(object "$OS" t tiny)
pre=$(object "$OS" pre text)
# Compressed by btrfs 128 KiB at a time: its random half stays plain beside the
# encoded text half, which is what a compressed binary looks like.
mix=$(object "$OS" mix mixed)
sync
$SUDO btrfs filesystem defragment -czstd "$pre" "$mix"
sync
f1=$(object "$FP" f1 text)
meta="$OS/$(hex meta | cut -c1-2)/$(hex meta).dirtree"
$SUDO mkdir -p "$(dirname "$meta")"
$SUDO sh -c "yes dirtree | head -c 64K > '$meta'"
$SUDO mkdir -p "$M/sysroot/ostree/deploy/default/usr/bin"
$SUDO ln "$a" "$M/sysroot/ostree/deploy/default/usr/bin/tool"
sync
declare -A sum
for f in "$a" "$b" "$c" "$r" "$t" "$pre" "$mix" "$f1" "$meta"; do sum[$f]=$(digest "$f"); done
pre_at=$(physical "$pre"); mix_at=$(physical "$mix"); meta_at=$(physical "$meta")
if $SUDO filefrag -v "$mix" | grep -q encoded && $SUDO filefrag -v "$mix" | grep -E '^ *[0-9]+:' | grep -qv encoded; then
    ok "the fixture has a mixed object: encoded extents beside plain ones"
else
    bad "the fixture has a mixed object: encoded extents beside plain ones"
fi
if ! encoded "$a"; then ok "the fixture is the real case: a fallocated object lands uncompressed"; else bad "the fixture is the real case"; fi

section "first run"
out=$(run); rc=$?
all_out="$out"
is "it exits 0" 0 "$rc"
printf '%s\n' "$out" | sed 's/^/      /'
for f in "$a" "$b" "$c" "$f1"; do
    if encoded "$f"; then ok "compressed: ${f#"$M"/}"; else bad "compressed: ${f#"$M"/}"; fi
done
same=1; for f in "${!sum[@]}"; do [ "$(digest "$f")" = "${sum[$f]}" ] || same=0; done
is "every object is byte-identical" 1 "$same"
is "the deployment still hardlinks the object" \
    "$($SUDO stat -c %i "$a")" "$($SUDO stat -c %i "$M/sysroot/ostree/deploy/default/usr/bin/tool")"
is "an object that was already compressed was not rewritten" "$pre_at" "$(physical "$pre")"
is "  ...nor one compressed with chunks that would not shrink" "$mix_at" "$(physical "$mix")"
is "ostree metadata was not touched" "$meta_at" "$(physical "$meta")"
is "the OS list holds every content object over 2 KiB, incompressible ones included" \
    "$(printf '%s\n' "$a" "$b" "$c" "$r" "$pre" "$mix" | LC_ALL=C sort)" "$($SUDO cat "$ST/os.done")"
is "the Flatpak list holds its object" "$f1" "$($SUDO cat "$ST/flatpak.done")"
# The property would land on new objects as a btrfs.compression xattr, which
# an ostree bare repo checksums; fsck then calls the object corrupt.
is "no compression property is set on the store" \
    "" "$($SUDO btrfs property get "$OS" compression; $SUDO btrfs property get "$(dirname "$a")" compression)"

if grep -q 'os: first run, classifying 6 objects' <<<"$out" && grep -q 'os: 2 already compressed, 4 to do' <<<"$out"; then
    ok "the first run classified before rewriting (2 already compressed, 4 to do)"
else
    bad "the first run classified before rewriting"
fi

section "second run"
declare -A at
for f in "$a" "$b" "$c" "$r" "$f1"; do at[$f]=$(physical "$f"); done
out=$(run); rc=$?
is "it exits 0" 0 "$rc"
if grep -q 'os: compressed 0 new objects' <<<"$out" && grep -q 'flatpak: compressed 0 new objects' <<<"$out"; then
    ok "nothing is new, so nothing is compressed"
else
    bad "nothing is new, so nothing is compressed"; printf '%s\n' "$out" | sed 's/^/      /'
fi
moved=0; for f in "${!at[@]}"; do [ "$(physical "$f")" = "${at[$f]}" ] || moved=1; done
is "  ...and no object was rewritten, the incompressible one included" 0 "$moved"

section "an update lands, an object is pruned"
# Written and NOT synced: still delalloc in its preallocated extent, as an
# object is right after `rime update` pulls with fsync off.
n=$(object "$OS" new text)
$SUDO rm -f "$b"
out=$(run)
if grep -q 'os: compressed 1 new objects' <<<"$out"; then ok "only the new object is compressed"; else bad "only the new object is compressed"; printf '%s\n' "$out" | sed 's/^/      /'; fi
if encoded "$n"; then ok "  ...and it is, though it was still in delalloc when the run began"; else bad "  ...and it is, though it was still in delalloc when the run began"; fi
if $SUDO grep -qxF "$b" "$ST/os.done"; then bad "the pruned object leaves the list"; else ok "the pruned object leaves the list"; fi

section "a failed batch is retried"
x=$(object "$OS" x text)
SHIM="$T/shim"; mkdir -p "$SHIM"
real_btrfs=$(command -v btrfs)
cat > "$SHIM/btrfs" <<EOF
#!/bin/sh
[ "\$1 \$2" = "filesystem defragment" ] && exit 1
exec "$real_btrfs" "\$@"
EOF
chmod +x "$SHIM/btrfs"
out=$($SUDO env PATH="$SHIM:$PATH" RIME_COMPACT_NAMESPACED=1 RIME_COMPACT_SYSROOT="$M/sysroot" \
        RIME_COMPACT_FLATPAK="$M/flatpak" RIME_COMPACT_STATE="$ST" "$ENGINE" 2>&1); rc=$?
is "a failed batch does not fail the run" 0 "$rc"
if grep -q 'os: 1 batch(es) failed' <<<"$out"; then ok "  ...it says so"; else bad "  ...it says so"; fi
if $SUDO grep -qxF "$x" "$ST/os.done"; then bad "  ...and the object stays off the list"; else ok "  ...and the object stays off the list"; fi
out=$(run)
if grep -q 'os: compressed 1 new objects' <<<"$out" && encoded "$x"; then ok "the next run compresses it"; else bad "the next run compresses it"; fi

section "a store that is not on btrfs"
TF=$(mktemp -d /tmp/rime-compact-tmpfs.XXXXXX)
mkdir -p "$TF/repo/objects"
out=$($SUDO env RIME_COMPACT_NAMESPACED=1 RIME_COMPACT_SYSROOT="$M/sysroot" RIME_COMPACT_FLATPAK="$TF" \
        RIME_COMPACT_STATE="$ST" "$ENGINE" 2>&1); rc=$?
rm -rf "$TF"
is "it exits 0" 0 "$rc"
if grep -qE 'flatpak: .* is on (tmpfs|[a-z0-9]+), which does not compress; nothing to do' <<<"$out"; then ok "  ...and leaves it alone, saying why"; else bad "  ...and leaves it alone, saying why"; fi

section "the read-only /sysroot, as on a booted machine"
y=$(object "$OS" y text)
$SUDO mount --bind "$M/sysroot" "$M/sysroot"
$SUDO mount -o remount,bind,ro "$M/sysroot"
if $SUDO touch "$M/sysroot/probe" 2>/dev/null; then bad "the fixture's /sysroot is read-only"; else ok "the fixture's /sysroot is read-only"; fi
out=$($SUDO env RIME_COMPACT_SYSROOT="$M/sysroot" RIME_COMPACT_FLATPAK="$M/flatpak" \
        RIME_COMPACT_STATE="$ST" "$ENGINE" 2>&1); rc=$?
is "it exits 0 (through unshare, its own namespace)" 0 "$rc"
if encoded "$y"; then ok "  ...and compressed the new object under the read-only mount"; else bad "  ...and compressed the new object under the read-only mount"; printf '%s\n' "$out" | sed 's/^/      /'; fi
case ",$(findmnt -no OPTIONS --mountpoint "$M/sysroot")," in
    *,ro,*) ok "  ...while /sysroot stayed read-only for everyone else" ;;
    *)      bad "  ...while /sysroot stayed read-only for everyone else" ;;
esac
$SUDO umount "$M/sysroot"

section "after every run above"
# Objects were created after runs here (the update, the failed batch, the
# read-only mount), which is when an inherited property would have reached
# them as a btrfs.compression xattr.
is "no object carries a btrfs.compression xattr" "0" \
    "$($SUDO find "$OS" "$FP" -type f -exec getfattr --absolute-names -n btrfs.compression {} + 2>/dev/null | grep -c 'btrfs.compression=')"

section "never the running machine's own store"
# This suite points the engine at a loop device, and on a Rime machine the
# engine would otherwise prune THAT machine's images: ostree takes --sysroot /.
if grep -q 'Removed images\|pruning unused images' <<<"$all_out"; then
    bad "no run above pruned images (the engine was pointed away from /sysroot)"
else
    ok "no run above pruned images (the engine was pointed away from /sysroot)"
fi

section "privilege"
if [ "$(id -u)" != 0 ]; then
    out=$(RIME_COMPACT_NAMESPACED=1 "$ENGINE" 2>&1); rc=$?
    is "it refuses to run unprivileged" 1 "$rc"
else
    skp "privilege check (the suite itself runs as root)"
fi

finish
