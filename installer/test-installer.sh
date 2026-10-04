#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer.sh — test what actually ships: the engine's input guards,
#  and that every page of the GTK installer really draws.
#
#  (Renamed from test-engine-guards.sh when the GUI half was added. That file
#  in turn replaced test-interactive.sh, which drove the whiptail TUI with
#  canned answers — the TUI no longer exists: rime-install is engine-only now,
#  spoken to as `rime-install --headless ANSWERS` by the GTK installer, and
#  the text UI people kept getting stranded in has been deleted.)
#
#  ── Half 1: the engine refuses bad input BEFORE it wipes ────────────────────
#  Deleting the TUI silently deleted three guards that only lived inside it —
#  the username regex, the reserved-name check, and the hostname regex. Losing
#  them is not cosmetic. Nothing else rejects a bad username until `useradd`
#  runs, and `useradd` runs AFTER `bootc install --wipe` has already erased the
#  disk: the result is a fully installed system with no account on it, and the
#  user's previous OS gone. The original TUI validated early for exactly that
#  reason and said so in a comment. Two more guards (target == ESP, and target
#  not on the named disk) had no equivalent at all in headless mode. All five
#  are asserted below so a future refactor cannot quietly drop them again.
#
#  EVERY case here must fail BEFORE anything is written, so this half NEVER
#  touches a block device. The two partition-mode cases name real devices
#  (/dev/sda, /dev/sdb) because the guards need `-b` to succeed to be reached at
#  all — but they are rejected by the guard under test, several steps before any
#  mkfs, mount or bootc call. Nothing is opened for writing.
#
#  ── Half 2: every GUI page must draw ────────────────────────────────────────
#  The GUI is now the ONLY front end. If a page fails to render, or lays out so
#  its buttons land off-screen, the user is stranded with no fallback — and a
#  syntax-clean file proves nothing about either. So every page named in the
#  GUI's own registry is rendered headless (cage + wlroots-headless + grim in
#  the rime-guitest container) and the screenshot is measured, not just stat'd:
#  a produced PNG is NOT a pass — a blank or single-colour frame means the page
#  did not draw. Pages render at 1024x600 and 1366x768, the realistic
#  worst-case laptop panels; one page already clipped its action row at 720 px
#  (measured), which is exactly the failure class this half exists to catch.
#  Pixel checks alone are not enough, though: GTK prefers to SQUASH mid-page
#  widgets over pushing the action row off-screen (measured: at 1024x600 the
#  account page swallows the Computer-name field whole, buttons still visible),
#  so every page is also measured — GTK is asked for the page's minimum height
#  at each panel width, and it must fit. The exact pass criteria are documented
#  inline below. No disk, real or virtual, is enumerated (lsblk is stubbed
#  inside the container), let alone touched.
#
#  PASS = every engine case prints its expected RIME-INSTALL-FAILED reason and
#         never "Unexpected error on line" (that string means the ERR trap
#         fired, which is always a bug in the installer), and every GUI page
#         passes every render check at every size.
#
#  Run from the repo's installer/ directory. Needs passwordless root (sudo -n):
#  the engine refuses to run unprivileged, and the render container lives in
#  ROOT podman storage (built here on first run if missing).
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")"

ENGINE=./rime-install
ANS=$(mktemp /tmp/rime-test-answers.XXXXXX)

# ── Getting the engine as far as its own guards ──────────────────────────────
#
# Every case below feeds the engine an answers file and expects a named refusal.
# None of them could reach one. rime-install:353 refuses to continue unless the
# Rime OS image is present in ROOT podman storage, and that check runs BEFORE
# argument parsing — so on any machine that is not the ISO build box the engine
# died at preflight and every case in the three engine sections reported the
# same "image is not present" text instead of the guard under test.
#
# That was not a regression. `git log -S` puts the image check in dddabd6f
# (2026-07-23) and these cases in 33b744d5, five days later: they were written
# against an engine that already refused them, and only ever passed where root
# podman storage happened to hold localhost/rime-os:daily. pr-validation.yml
# runs this suite on a bare ubuntu-24.04 runner, so they were dead in CI too.
# The tell that needs no theory: the "no arguments" case asserts exit 2, and
# preflight's die() exits 1.
#
# rime-install:56 is IMAGE="${RIME_IMAGE:-localhost/rime-os:${EDITION}}", with
# the comment "override with RIME_IMAGE=... for testing". An empty tar imported
# by podman is a valid image with no layers — no network, no build, removed
# again on exit, so the suite does not depend on the ambient store either.
#
# sudo's env_reset strips RIME_* from the caller's environment, so this must be
# passed as `sudo -n RIME_IMAGE=...` on each invocation and cannot be exported.
SCRATCH_IMAGE="localhost/rime-engine-probe:test"
ENGINE_IMAGE=""
scratch_made=0
BUILD_CTX=""
LOOP_IMG=""
LOOP_DEV=""
STUB_DIR=$(mktemp -d /tmp/rime-bootc-stub.XXXXXX)
# The engine refuses to start without bootc (it installs with the live env's
# own), and the GitHub runner this suite runs on has none. Every engine case
# here is a dry run that stops before bootc would run, so a stub that is never
# executed is all preflight needs; if one ever IS executed, it fails loudly
# rather than pretending to have installed anything. RIME_BOOTC is the engine's
# seam for exactly this, and sudo's env_reset means it is passed per call.
BOOTC_STUB="$STUB_DIR/bootc-stub"
printf '#!/bin/sh\necho "bootc stub executed by a dry run: $*" >&2\nexit 99\n' > "$BOOTC_STUB"
chmod 755 "$BOOTC_STUB"
cleanup() {
    rm -f "$ANS"
    rm -rf "$STUB_DIR"
    # shellcheck disable=SC2033  # the real losetup; the stub further down is scoped to one case
    [ -n "$LOOP_DEV" ] && sudo -n losetup -d "$LOOP_DEV" 2>/dev/null || true
    [ -n "$LOOP_IMG" ] && rm -f "$LOOP_IMG"
    [ -n "$BUILD_CTX" ] && rm -rf "$BUILD_CTX"
    [ "$scratch_made" = 1 ] && sudo -n podman rmi -f "$SCRATCH_IMAGE" >/dev/null 2>&1
}
trap cleanup EXIT
chmod 600 "$ANS"

ensure_engine_image() {
    command -v podman >/dev/null 2>&1 || return 1
    sudo -n true 2>/dev/null || return 1
    if sudo -n podman image exists localhost/rime-os:daily 2>/dev/null; then
        ENGINE_IMAGE="localhost/rime-os:daily"; return 0
    fi
    local t; t=$(mktemp /tmp/rime-empty.XXXXXX.tar) || return 1
    tar -cf "$t" -T /dev/null 2>/dev/null \
        && sudo -n podman import -q "$t" "$SCRATCH_IMAGE" >/dev/null 2>&1
    local rc=$?
    rm -f "$t"
    [ "$rc" = 0 ] || return 1
    scratch_made=1
    ENGINE_IMAGE="$SCRATCH_IMAGE"
    return 0
}

pass=0; fail=0

# Skipping here is honest and failing is not: with no image the engine cannot be
# exercised at all, and a suite that reports 20 failures on a laptop teaches
# people to ignore it. But it must be LOUD, because a silent skip of the engine
# half is how this went unnoticed for six weeks.
ENGINE_RUNNABLE=1
if ! ensure_engine_image; then
    ENGINE_RUNNABLE=0
    echo "SKIP: the engine half cannot run here — preflight needs a Rime OS image in"
    echo "      ROOT podman storage and neither one nor passwordless podman is available."
fi

# $1 = case name, $2 = expected substring in the failure reason, $3 = answers body
check() {
    local name=$1 want=$2 body=$3 out
    if [ "$ENGINE_RUNNABLE" != 1 ]; then
        printf 'SKIP  %-30s no engine image\n' "$name"; return
    fi
    printf '%s\n' "$body" > "$ANS"
    out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 "$ENGINE" --headless "$ANS" 2>&1 </dev/null)

    if grep -q 'Unexpected error on line' <<<"$out"; then
        printf 'FAIL  %-30s ERR TRAP FIRED\n' "$name"; fail=$((fail+1)); return
    fi
    if grep -qF "$want" <<<"$out"; then
        printf 'PASS  %-30s\n' "$name"; pass=$((pass+1))
    else
        printf 'FAIL  %-30s expected %q\n      got: %s\n' \
            "$name" "$want" "$(grep -m1 RIME-INSTALL-FAILED <<<"$out" || echo '<no sentinel>')"
        fail=$((fail+1))
    fi
}

# A disk that cannot exist, so the whole-disk cases stop at the block-device
# check instead of proceeding. The account guards run BEFORE that check — which
# is the ordering under test.
# `encrypt=no` is here because the engine now REFUSES an answers file that
# does not say, one way or the other, whether to encrypt the disk. It is not
# a default this suite is choosing: a missing key is its own refusal, and
# installer/test-installer-luks.sh is the suite that asserts that. Without
# it every case below would stop at the encryption question instead of the
# guard it is actually testing.
BASE=$'mode=disk\ndisk=/dev/zzz-does-not-exist\npassword=pw\nhostname=rime\nencrypt=no'

echo "── argument handling ──────────────────────────────────────────────────"
out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" "$ENGINE" </dev/null 2>&1); rc=$?
if [ "$ENGINE_RUNNABLE" != 1 ]; then
    printf 'SKIP  %-30s no engine image\n' "no arguments"
elif [ "$rc" = 2 ] && grep -q 'not a user interface' <<<"$out"; then
    printf 'PASS  %-30s (exit 2, starts nothing)\n' "no arguments"; pass=$((pass+1))
else
    printf 'FAIL  %-30s exit=%s\n' "no arguments" "$rc"; fail=$((fail+1))
fi

echo "── account validation (must run before the disk is touched) ───────────"
check "username: uppercase"   "Invalid username 'Bob'"        "$BASE"$'\nusername=Bob'
check "username: leading digit" "Invalid username '1bob'"     "$BASE"$'\nusername=1bob'
check "username: reserved"    "reserved system account"       "$BASE"$'\nusername=root'
check "hostname: underscore"  "Invalid hostname 'my_host'"    $'mode=disk\ndisk=/dev/zzz-does-not-exist\npassword=pw\nusername=bob\nhostname=my_host\nencrypt=no'

echo "── answers-file handling ──────────────────────────────────────────────"
check "unknown key"           "unknown key in answers file"   "$BASE"$'\nusername=bob\nbogus=1'
check "missing password"      "password missing"              $'mode=disk\ndisk=/dev/zzz-does-not-exist\nusername=bob\nhostname=rime'
check "bad mode value"        "bad mode"                      $'mode=wipeitall\ndisk=/dev/zzz-does-not-exist\nusername=bob\npassword=pw\nhostname=rime'
check "valid input reaches disk check" "is not a block device" "$BASE"$'\nusername=bob'

# The parser splits on '=' with IFS, so a password containing '=' is a real
# risk: everything after the first '=' must survive intact.
printf 'username=bob\npassword=a=b=c\nhostname=rime\n' > "$ANS"
got=$(while IFS='=' read -r k v || [ -n "$k" ]; do [ "$k" = password ] && printf '%s' "$v"; done < "$ANS")
if [ "$got" = 'a=b=c' ]; then
    printf 'PASS  %-30s\n' "password containing '='"; pass=$((pass+1))
else
    printf 'FAIL  %-30s got %q\n' "password containing '='" "$got"; fail=$((fail+1))
fi

# A file whose last line has no trailing newline used to lose that line
# entirely — measured. A dropped `mokpw` would skip Secure Boot enrolment
# without a word, so the parser reads the final unterminated line too.
printf 'username=bob\npassword=pw\nhostname=lastline' > "$ANS"
got=$(while IFS='=' read -r k v || [ -n "$k" ]; do [ "$k" = hostname ] && printf '%s' "$v"; done < "$ANS")
if [ "$got" = 'lastline' ]; then
    printf 'PASS  %-30s\n' "no trailing newline"; pass=$((pass+1))
else
    printf 'FAIL  %-30s last key lost\n' "no trailing newline"; fail=$((fail+1))
fi

echo "── partition mode: the two most destructive mistakes ──────────────────"
# These need devices that exist for the guard to be reached. Read-only: both
# cases are refused by the guard under test, long before any write.
if [ -b /dev/sda ] && [ -b /dev/sdb ] && [ -b /dev/sda2 ] && [ -b /dev/sdb1 ]; then
    check "target == ESP"     "same device"                   $'mode=partition\ndisk=/dev/sda\ntarget=/dev/sda2\nesp=/dev/sda2\nusername=bob\npassword=pw\nhostname=rime\nencrypt=no'
    check "target on another disk" "is not a partition of"    $'mode=partition\ndisk=/dev/sda\ntarget=/dev/sdb1\nesp=/dev/sda2\nusername=bob\npassword=pw\nhostname=rime\nencrypt=no'
else
    echo "SKIP  partition-mode cases (need /dev/sda2 and /dev/sdb1 present)"
fi

echo "── final confirmation: binds the exact device before any write ───────"
if [ "$ENGINE_RUNNABLE" = 1 ] && command -v losetup >/dev/null \
   && LOOP_IMG=$(mktemp /var/tmp/rime-confirm-loop.XXXXXX); then
    truncate -s 32G "$LOOP_IMG"
    # shellcheck disable=SC2033  # the real losetup, deliberately (see cleanup)
    LOOP_DEV=$(sudo -n losetup --find --show "$LOOP_IMG" 2>/dev/null || true)
    if [ -n "$LOOP_DEV" ]; then
        fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$LOOP_DEV")
        base=$(printf 'mode=disk\ndisk=%s\nusername=bob\npassword=pw\nhostname=rime\nencrypt=no\n' "$LOOP_DEV")
        check "missing typed confirmation" "The final confirmation (typing ERASE) is missing" "$base"
        check "wrong confirmed target" "The confirmation was typed for" \
            "$base"$'\nconfirmed=ERASE\nconfirm_target=/dev/not-this-loop\n'"confirm_disk_id=$fp"
        check "changed disk identity" "is not the disk that was confirmed" \
            "$base"$'\nconfirmed=ERASE\n'"confirm_target=$LOOP_DEV"$'\nconfirm_disk_id=changed'
        printf '%s\nconfirmed=ERASE\nconfirm_target=%s\nconfirm_disk_id=%s\n' \
            "$base" "$LOOP_DEV" "$fp" > "$ANS"
        out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 \
              "$ENGINE" --headless "$ANS" 2>&1 </dev/null)
        if grep -q 'RIME-INSTALL-DRYRUN-OK' <<<"$out"; then
            printf 'PASS  %-30s\n' "exact device dry run"; pass=$((pass+1))
        else
            printf 'FAIL  %-30s %s\n' "exact device dry run" \
                "$(grep -m1 RIME-INSTALL-FAILED <<<"$out" || echo no-sentinel)"
            fail=$((fail+1))
        fi
        # The GUI can die mid-install and the engine must still finish. Same
        # dry run, with stdout and stderr on a pipe whose reader is already
        # gone. Before the relay, the bare `echo` of the final sentinel failed
        # there and the ERR trap recorded a finished install as a failure.
        _rc=$(python3 -c 'import os, subprocess, sys
r, w = os.pipe(); os.close(r)
print(subprocess.call(sys.argv[1:], stdin=subprocess.DEVNULL, stdout=w, stderr=w))' \
              sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 "$ENGINE" --headless "$ANS")
        # ...and the relay must really be in place, stderr included: a merge
        # condition that can never be true once passed this check unnoticed.
        if [ "$_rc" = 0 ] && sudo -n grep -q 'RIME-DRY-RUN: validation complete' /var/log/rime-install.log \
           && sudo -n grep -qE 'stdout relayed via pid [0-9]+; stderr merged: yes' /var/log/rime-install.log; then
            printf 'PASS  %-30s\n' "engine outlives a dead GUI"; pass=$((pass+1))
        else
            printf 'FAIL  %-30s rc=%s %s\n' "engine outlives a dead GUI" "$_rc" \
                "$(sudo -n tail -1 /var/log/rime-install.log 2>/dev/null)"; fail=$((fail+1))
        fi
        # Partition mode binds THREE identities — disk, root partition, ESP —
        # and each one on its own must be able to stop the install. The disk
        # below mimics a dual-boot layout (ESP, a partition for Rime, a
        # partition that must survive), all inside the sparse loop image
        # allocated above; the engine stays in dry-run mode throughout.
        if [[ "$LOOP_DEV" == /dev/loop* ]] && command -v sgdisk >/dev/null \
           && command -v mkfs.vfat >/dev/null; then
            sudo -n sgdisk --zap-all "$LOOP_DEV" >/dev/null 2>&1
            # p4 is under the engine's 10 GB partition floor; p2 and p3 are
            # over it, so every other case below is refused for the reason it
            # names and not for its size. Sparse, so the 32 GiB costs nothing.
            sudo -n sgdisk -n1:0:+300M -t1:ef00 -c1:"EFI system partition" \
                -n2:0:+14G -t2:8300 -c2:rime-root \
                -n4:0:+5G -t4:8300 -c4:too-small \
                -n3:0:0 -t3:0700 -c3:"Basic data partition" "$LOOP_DEV" >/dev/null 2>&1
            sudo -n partprobe "$LOOP_DEV" >/dev/null 2>&1
            sudo -n udevadm settle --timeout=10 >/dev/null 2>&1
            esp="${LOOP_DEV}p1"; target="${LOOP_DEV}p2"; kept="${LOOP_DEV}p3"; small="${LOOP_DEV}p4"
            if [ -b "$esp" ] && [ -b "$target" ] && [ -b "$kept" ]; then
                sudo -n mkfs.vfat -F32 -n SYSTEM "$esp" >/dev/null 2>&1
                disk_fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$LOOP_DEV")
                target_fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$target")
                esp_fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$esp")
                kept_fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$kept")
                pbase=$(printf 'mode=partition\ndisk=%s\ntarget=%s\nesp=%s\nusername=bob\npassword=pw\nhostname=rime\nencrypt=no\nconfirmed=ERASE\nconfirm_target=%s\nconfirm_disk_id=%s\nconfirm_target_id=%s\nconfirm_esp_id=%s\n' \
                    "$LOOP_DEV" "$target" "$esp" "$target" "$disk_fp" "$target_fp" "$esp_fp")
                check "changed root identity" "is not the partition that was confirmed" \
                    "${pbase/confirm_target_id=$target_fp/confirm_target_id=changed}"
                check "changed ESP identity" "is not the EFI System Partition that was confirmed" \
                    "${pbase/confirm_esp_id=$esp_fp/confirm_esp_id=changed}"
                # The confirmation named p2; an answers file that now says p3
                # (the partition that must survive) is the renamed-device case
                # in miniature, and must be refused on the name alone.
                check "target swapped after confirm" "The confirmation was typed for" \
                    "${pbase/target=$target/target=$kept}"
                # A partition too small for the install is refused while it is
                # still intact, whatever was confirmed.
                check "partition under the floor" "needs a partition of at least" \
                    "${pbase/target=$target/target=$small}"
                # ext4 has no transparent compression, so the same 14 GiB
                # partition that takes a btrfs install is too small for it.
                check "ext4 needs a bigger floor" "needs a partition of at least 16 GB" \
                    "${pbase}"$'\n'"rootfs=ext4"
                # …and a correct NAME carrying another partition's identity is
                # refused on the identity, which is the case a name check misses.
                check "identity of a kept partition" "is not the partition that was confirmed" \
                    "${pbase/confirm_target_id=$target_fp/confirm_target_id=$kept_fp}"
                printf '%s\n' "$pbase" > "$ANS"
                out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 \
                      "$ENGINE" --headless "$ANS" 2>&1 </dev/null)
                if grep -q 'RIME-INSTALL-DRYRUN-OK' <<<"$out"; then
                    printf 'PASS  %-30s\n' "partition dry run"; pass=$((pass+1))
                else
                    printf 'FAIL  %-30s %s\n' "partition dry run" \
                        "$(grep -m1 RIME-INSTALL-FAILED <<<"$out" || echo no-sentinel)"
                    fail=$((fail+1))
                fi
            else
                echo "SKIP  partition confirmation cases (loop partitions unavailable)"
            fi
        fi
    else
        echo "SKIP  confirmation loop tests (no free loop device)"
    fi
else
    echo "SKIP  confirmation loop tests (no engine or losetup)"
fi

echo
echo "── the install source, the one bootc call site, the compressed root ───"
# The engine used to stage a network install on disk — an OCI directory, then
# the whole image decompressed again into a containers-storage so there was a
# container to start bootc in — which is what made a single-disk install need
# ~53 GB. It now runs the live environment's own bootc against
# `--source-imgref registry:<digest>` and stages nothing (see "Where the OS
# comes from" in the engine). These hold that shape open.
#
# Everything below is sourced out of the shipped engine rather than copied, so
# this tests what installs, not a paraphrase of it.

# 1. The staging machinery is gone, not merely unused. Comment lines are
#    skipped: the engine explains what it replaced, and saying so is not code.
_staging=$(grep -nE 'pick_scratch|scratch_fs_ok|stage_setup|stage_budget_kb|netinstall_fetch_into|NEED_SCRATCH_GB|STAGE_RESERVE_GB|PODMAN_STORE|SKOPEO_TMP' "$ENGINE" \
           | grep -vE '^[0-9]+:[[:space:]]*#' || true)
if [ -n "$_staging" ]; then
    printf 'FAIL  %-30s %s\n' "no staging left" "$(head -1 <<<"$_staging")"; fail=$((fail+1))
else
    printf 'PASS  %-30s\n' "no staging left"; pass=$((pass+1))
fi
# …and nothing copies the image anywhere: no `skopeo copy`, no `podman pull`,
# no `podman run` of the image. The engine's only image consumer is bootc.
_copy=$(grep -nE '^\s*[^#]*\b(skopeo\s+(\S+\s+)*copy|podman\s+(\S+\s+)*(pull|run))\b' "$ENGINE" || true)
if [ -n "$_copy" ]; then
    printf 'FAIL  %-30s %s\n' "no image copy or container" "$(head -1 <<<"$_copy")"; fail=$((fail+1))
else
    printf 'PASS  %-30s\n' "no image copy or container"; pass=$((pass+1))
fi

# 2. Exactly ONE executable line starts a bootc install, it is to-filesystem,
#    and it is inside run_bootc_install. A second call site is a second place
#    the NVRAM decision can be forgotten, which is how 2026-09-20 happened.
_calls=$(grep -nE '\binstall (to-filesystem|to-disk|to-existing-root)\b' "$ENGINE" | grep -vE '^[0-9]+:[[:space:]]*#' || true)
_ncalls=$(grep -c . <<<"$_calls" || true)
_fnstart=$(grep -n '^run_bootc_install() {' "$ENGINE" | cut -d: -f1)
_fnend=$(awk -v s="${_fnstart:-0}" 'NR>s && /^}/ {print NR; exit}' "$ENGINE")
_callln=$(cut -d: -f1 <<<"$_calls" | head -1)
if [ "$_ncalls" = 1 ] && [ -n "$_fnstart" ] && [ "$_callln" -gt "$_fnstart" ] && [ "$_callln" -lt "${_fnend:-0}" ] \
   && grep -q 'install to-filesystem' <<<"$_calls"; then
    printf 'PASS  %-30s\n' "one bootc call site"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "one bootc call site" "found $_ncalls: $(tr '\n' ' ' <<<"$_calls")"; fail=$((fail+1))
fi

_fns=$(mktemp /tmp/rime-source-fns.XXXXXX)
sed -n '/^choose_source_imgref()/,/^}/p;/^disk_is_loopback()/,/^}/p;/^set_nvram_args_for()/,/^}/p;/^run_bootc_install()/,/^}/p;/^mount_new_root()/,/^}/p' "$ENGINE" > "$_fns"
if [ "$(grep -c '^[a-z_]*() {' "$_fns")" != 5 ]; then
    printf 'FAIL  %-30s could not extract the functions from %s\n' "source functions" "$ENGINE"
    fail=$((fail+1))
else
(
    set +u
    # shellcheck disable=SC1090
    . "$_fns"
    log() { :; }; note() { :; }
    _p=0; _f=0
    _ck() {  # name, got, want
        if [ "$2" = "$3" ]; then printf 'PASS  %-30s\n' "$1"; _p=$((_p+1))
        else printf 'FAIL  %-30s want %s got %s\n' "$1" "$3" "$2"; _f=$((_f+1)); fi
    }
    _has() {  # name, haystack, needle
        if grep -qF -- "$3" <<<"$2"; then printf 'PASS  %-30s\n' "$1"; _p=$((_p+1))
        else printf 'FAIL  %-30s %s not in: %s\n' "$1" "$3" "$(tr '\n' ' ' <<<"$2")"; _f=$((_f+1)); fi
    }
    _lacks() {
        if grep -qF -- "$3" <<<"$2"; then printf 'FAIL  %-30s %s present in: %s\n' "$1" "$3" "$(tr '\n' ' ' <<<"$2")"; _f=$((_f+1))
        else printf 'PASS  %-30s\n' "$1"; _p=$((_p+1)); fi
    }
    _t=$(mktemp -d /tmp/rime-source-test.XXXXXX)

    # ── the source ───────────────────────────────────────────────────────────
    # shellcheck disable=SC2034  # all read by choose_source_imgref, sourced above
    { NET_SOURCE_IMAGE=ghcr.io/x/rime-os@sha256:abc; IMAGE=localhost/rime-os:rime; }
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    NETINSTALL=1 OCI_DIR="$_t/nope"; choose_source_imgref
    _ck "netinstall streams the pin" "$SOURCE_IMGREF" "registry:ghcr.io/x/rime-os@sha256:abc"
    mkdir -p "$_t/oci"
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    NETINSTALL=0 OCI_DIR="$_t/oci"; choose_source_imgref
    _ck "offline reads the ISO's OCI" "$SOURCE_IMGREF" "oci:$_t/oci"
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    NETINSTALL=0 OCI_DIR="$_t/nope"; choose_source_imgref
    _ck "else containers-storage"     "$SOURCE_IMGREF" "containers-storage:localhost/rime-os:rime"

    # ── what bootc is handed, read off a recording stub ──────────────────────
    cat > "$_t/bootc" <<STUB
#!/bin/sh
: > "$_t/argv"; for a in "\$@"; do printf '%s\n' "\$a" >> "$_t/argv"; done
STUB
    chmod +x "$_t/bootc"
    # unshare is stubbed too: the mask needs root and a real efivarfs, and what
    # is asserted is that the loop case goes through it at all. It records its
    # own argv and then runs the command it was given, minus the mount script.
    # Its argv is: --mount --propagation private -- sh -c SCRIPT NAME CMD...
    unshare() { printf '%s\n' "$@" > "$_t/unshare"; shift 8; "$@"; }
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    BOOTC="$_t/bootc"; TARGET_IMAGE=ghcr.io/x/rime-os:rime
    SOURCE_IMGREF="registry:ghcr.io/x/rime-os@sha256:abc"
    # A real disk: a name with no loop backing anywhere.
    mkdir -p "$_t/sys"
    rm -f "$_t/unshare"
    run_bootc_install /dev/nvme9n9 /run/rime-target --karg rd.luks.uuid=U >/dev/null 2>&1
    a=$(cat "$_t/argv" 2>/dev/null)
    _ck "argv: install to-filesystem"  "$(sed -n 1,2p <<<"$a" | tr '\n' ' ')" "install to-filesystem "
    _has "argv: the source"            "$a" "registry:ghcr.io/x/rime-os@sha256:abc"
    _has "argv: the origin is the tag" "$a" "ghcr.io/x/rime-os:rime"
    _has "argv: extra kargs pass"      "$a" "rd.luks.uuid=U"
    # grub, named: left to choose, bootc picks systemd-boot in a live env with
    # no bootupd and then refuses with the OS already deployed.
    _ck "argv: --bootloader grub"      "$(grep -A1 -x -- '--bootloader' <<<"$a" | tail -1)" grub
    # The last compression pass writes after bootc returns; finalize would have
    # remounted the target read-only first.
    _has "argv: --skip-finalize"       "$a" "--skip-finalize"
    _ck "argv: the root is last"       "$(tail -1 <<<"$a")" "/run/rime-target"
    _lacks "real disk: no --generic-image" "$a" "--generic-image"
    _ck "real disk: no mask namespace" "$([ -e "$_t/unshare" ] && echo yes || echo no)" no
    # A loop device, as the kernel would report it (the seam only ADDS loop-ness).
    mkdir -p "$_t/sys/loop9/loop"; echo /var/x.img > "$_t/sys/loop9/loop/backing_file"
    RIME_SYSFS_BLOCK="$_t/sys" run_bootc_install /dev/loop9 /run/rime-target >/dev/null 2>&1
    a=$(cat "$_t/argv" 2>/dev/null)
    _has "loop: bootc gets --generic-image" "$a" "--generic-image"
    u=$(cat "$_t/unshare" 2>/dev/null)
    _ck "loop: runs in a mount namespace" "$(sed -n 1p <<<"$u")" "--mount"
    _has "loop: efivars masked there"  "$u" "/sys/firmware/efi/efivars"

    # ── the compressed root ──────────────────────────────────────────────────
    mount() { printf 'mount %s\n' "$*" >> "$_t/calls"; }
    btrfs() {
        printf 'btrfs %s\n' "$*" >> "$_t/calls"
        [ "$1 $2" = "property get" ] && echo "compression=${_prop_answer:-zstd}"
        return 0
    }
    : > "$_t/calls"
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    ROOTFS_TYPE=btrfs ROOT_ZSTD_LEVEL=3 LOG=/dev/null
    mount_new_root /dev/x /run/rime-target && r=ok || r=refused
    c=$(cat "$_t/calls")
    _ck "btrfs root mounted"           "$r" ok
    _has "btrfs: forced zstd at install" "$c" "mount -o compress-force=zstd:3 /dev/x /run/rime-target"
    _has "btrfs: compression property"  "$c" "btrfs property set /run/rime-target compression zstd"
    # A property that did not stick must be a refusal, not a silent 2x install.
    : > "$_t/calls"; _prop_answer=lzo
    mount_new_root /dev/x /run/rime-target && r=ok || r=refused
    _ck "btrfs: property re-read"      "$r" refused
    _prop_answer=zstd
    : > "$_t/calls"
    # shellcheck disable=SC2034  # read by the engine functions sourced above
    ROOTFS_TYPE=ext4
    mount_new_root /dev/x /run/rime-target && r=ok || r=refused
    c=$(cat "$_t/calls")
    _ck "ext4 root mounted"            "$r" ok
    _lacks "ext4: no compression"      "$c" "compress"
    rm -rf "$_t"
    echo "$_p $_f" > /tmp/rime-source-counts
)
read -r _sp _sf < /tmp/rime-source-counts 2>/dev/null || { _sp=0; _sf=1; }
pass=$((pass + _sp)); fail=$((fail + _sf))
rm -f /tmp/rime-source-counts
fi
rm -f "$_fns"

# What the engine must and must not say.
#
# The refusal these used to guard — "There is nowhere to put the download",
# with "the full offline ISO" named as the way out — was a dead end: that ISO
# has never been published. Its absence is still asserted, because
# reintroducing it would put the dead end back.
for _gone in "There is nowhere to put the download" \
             "Use the full offline ISO. It carries the OS and needs no staging at all."; do
    if grep -qF "$_gone" "$ENGINE"; then
        printf 'FAIL  %-30s the dead-end refusal is back in the engine\n' "no dead end: ${_gone:0:18}"; fail=$((fail+1))
    else
        printf 'PASS  %-30s\n' "no dead end: ${_gone:0:18}"; pass=$((pass+1))
    fi
done
# And what must be there instead:
#   * the reassurance every pre-erase refusal gives;
#   * the warning a network install must give, because there the download and
#     the destruction are the same step;
#   * the reachability probe that is the last free check before the wipe.
for _want in "Nothing has been erased" \
             "downloads onto it as it goes" \
             "skopeo inspect --raw"; do
    if grep -qF -- "$_want" "$ENGINE"; then
        printf 'PASS  %-30s\n' "engine says: ${_want:0:22}"; pass=$((pass+1))
    else
        printf 'FAIL  %-30s missing from the engine\n' "engine says: ${_want:0:22}"; fail=$((fail+1))
    fi
done

# A published netinstall ISO downloads a pinned DIGEST. Once :rime moves on,
# that digest is untagged, and deleting untagged package versions would break
# every ISO in the wild at its first pull. Two halves: no workflow may delete
# package versions, and every release pins its digest with a durable
# netinstall-<release> tag through pin-netinstall-image.yml (write-once, and
# byte-for-byte: --preserve-digests).
_wf=../.github/workflows
# Comment lines are skipped: explaining why deletion is dangerous is not deletion.
_del=$(grep -nE 'delete-package-versions|/packages/container/[^[:space:]]*/versions/|(-X|--method)[[:space:]]+DELETE[^#]*packages' "$_wf"/*.yml 2>/dev/null \
       | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' || true)
if [ -n "$_del" ]; then
    printf 'FAIL  %-30s %s\n' "no GHCR version deletion" "$(head -1 <<<"$_del")"; fail=$((fail+1))
else
    printf 'PASS  %-30s\n' "no GHCR version deletion"; pass=$((pass+1))
fi
# Secure Boot enrolment (5b) runs after the installed system is unmounted, so
# everything it needs from the deployment must be read before that unmount.
# It used to read the key and the kernel's signing state from the empty mount
# point: every install then skipped the enrolment the user asked for, and a
# machine with Secure Boot on refused the kernel it had just been given.
_sbread_ln=$(grep -n '^SB_KSTATE=\$(cat "\$deploy/' "$ENGINE" | head -1 | cut -d: -f1)
_5b_ln=$(grep -n '^# ── 5b\. Secure Boot' "$ENGINE" | head -1 | cut -d: -f1)
_6_ln=$(grep -n '^# ── 6\. done' "$ENGINE" | head -1 | cut -d: -f1)
_umnt_ln=""
if [ -n "$_5b_ln" ]; then
    _umnt_ln=$(head -n "$_5b_ln" "$ENGINE" | grep -n '^sync || true; umount "\$MNT"' | tail -1 | cut -d: -f1)
fi
_5b_deploy=""
if [ -n "$_5b_ln" ] && [ -n "$_6_ln" ]; then
    _5b_deploy=$(sed -n "${_5b_ln},${_6_ln}p" "$ENGINE" | grep -v '^[[:space:]]*#' | grep -n '\$deploy' || true)
fi
if [ -n "$_sbread_ln" ] && [ -n "$_umnt_ln" ] && [ "$_sbread_ln" -lt "$_umnt_ln" ] && [ -z "$_5b_deploy" ]; then
    printf 'PASS  %-30s\n' "Secure Boot reads before umount"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "Secure Boot reads before umount" \
        "reads at ${_sbread_ln:-none}, unmount at ${_umnt_ln:-?}, \$deploy inside 5b: ${_5b_deploy:-none}"; fail=$((fail+1))
fi
echo "── one engine at a time, and a front end that died can reattach ───────"
# A VT switch can take cage (and so the GUI) down mid-install while the engine
# keeps writing. The fresh front end must never start a second engine, and a
# second engine must refuse before it touches the first one's log or mounts.
_lock_ln=$(grep -n 'flock -n 9' "$ENGINE" | head -1 | cut -d: -f1)
_trunc_ln=$(grep -n '^: > "\$LOG"' "$ENGINE" | head -1 | cut -d: -f1)
_um_ln=$(grep -n '^unmount_target$' "$ENGINE" | head -1 | cut -d: -f1)
if [ -n "$_lock_ln" ] && [ -n "$_trunc_ln" ] && [ -n "$_um_ln" ] \
   && [ "$_lock_ln" -lt "$_trunc_ln" ] && [ "$_lock_ln" -lt "$_um_ln" ]; then
    printf 'PASS  %-30s\n' "lock before log and mounts"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "lock before log and mounts" "flock at ${_lock_ln:-?}, log truncation at ${_trunc_ln:-?}, unmount_target at ${_um_ln:-?}"; fail=$((fail+1))
fi
if [ "$ENGINE_RUNNABLE" = 1 ] && command -v flock >/dev/null; then
    _L=/run/rime-install-test.$$.lock
    sudo -n sh -c 'printf "log of the install that is running\n" > /var/log/rime-install.log'
    sudo -n timeout 20 flock "$_L" sleep 20 & _holder=$!
    sleep 1
    printf 'mode=disk\ndisk=/dev/null\nusername=bob\npassword=pw\nhostname=rime\nencrypt=no\n' > "$ANS"
    out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_INSTALL_LOCK="$_L" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 "$ENGINE" --headless "$ANS" 2>&1 </dev/null); _rc=$?
    kill "$_holder" 2>/dev/null; wait "$_holder" 2>/dev/null
    if [ "$_rc" = 1 ] && grep -q 'already running' <<<"$out" \
       && sudo -n grep -q 'log of the install that is running' /var/log/rime-install.log; then
        printf 'PASS  %-30s\n' "second engine refuses cleanly"; pass=$((pass+1))
    else
        printf 'FAIL  %-30s rc=%s %s\n' "second engine refuses cleanly" "$_rc" "$(tail -1 <<<"$out")"; fail=$((fail+1))
    fi
    sudo -n rm -f "$_L"
else
    echo "SKIP  second engine refuses cleanly (no engine or flock)"
fi
# The result record a reattaching front end reads: root-only, and it carries
# the recovery key (the only on-machine copy once the front end is gone).
_rf=$(mktemp -d /var/tmp/rime-result-test.XXXXXX)
(
    set +u
    eval "$(sed -n '/^write_result() {/,/^}/p' "$ENGINE")"
    RESULT_ON=1; RESULT_FILE="$_rf/sub/result"
    INSTALL_MODE=disk; DISK=/dev/vda; TARGET=/dev/vda; USERNAME=bob; HOSTNAME=rime
    RESULT_RECOVERY_KEY=abcd-efgh; RESULT_RECOVERY_SAVED=rime-recovery-key-rime.txt; RESULT_RECOVERY_UNSAVED=
    log() { :; }
    write_result ok ""
    # A die() after success must not turn a finished install into a failure.
    write_result failed "Unexpected error on line 1"
    stat -c %a "$RESULT_FILE"; cat "$RESULT_FILE"
) > "$_rf/out" 2>&1
if grep -qx 600 "$_rf/out" && grep -qx 'status=ok' "$_rf/out" && grep -qx 'recovery_key=abcd-efgh' "$_rf/out" \
   && grep -qx 'username=bob' "$_rf/out"; then
    printf 'PASS  %-30s\n' "engine records its result"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "engine records its result" "$(tr '\n' ' ' < "$_rf/out")"; fail=$((fail+1))
fi
# ...and the GUI reads it back into the state its done page draws from.
printf 'status=failed\nmessage=disk went away\nmode=disk\ndisk=/dev/vda\ntarget=\nusername=bob\nhostname=rime\nrecovery_key=abcd\nrecovery_saved=\nrecovery_unsaved=x\n' > "$_rf/result"
if RIME_RESULT_FILE="$_rf/result" python3 -c "
import os, re, subprocess
src = open('rime-installer-gui').read()
g = {'os': os, 're': re, 'subprocess': subprocess}
exec(compile(src[src.index('ENGINE = '):src.index('def netinstall')].replace('ENGINE = ', 'ENGINE_ = ', 1), 'gui', 'exec'), g)
r = g['read_result']()
assert r and r['status'] == 'failed' and r['message'] == 'disk went away' and r['recovery_key'] == 'abcd', r
" 2>"$_rf/pyerr"; then
    printf 'PASS  %-30s\n' "GUI reads the result back"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "GUI reads the result back" "$(tail -1 "$_rf/pyerr")"; fail=$((fail+1))
fi
# A withdrawn pinned image is not a network failure, and must not say it is.
(
    set +u
    eval "$(sed -n '/^pinned_image_gone() {/,/^}/p' "$ENGINE")"
    LOG="$_rf/log"; printf 'reading manifest sha256:00 in ghcr.io/andrenijman/rime-os: manifest unknown\n' > "$LOG"
    NET_SOURCE_IMAGE=ghcr.io/andrenijman/rime-os@sha256:00; TARGET_IMAGE=ghcr.io/andrenijman/rime-os:rime
    pinned_image_gone && echo GONE-PINNED
    NET_SOURCE_IMAGE=$TARGET_IMAGE
    pinned_image_gone || echo UNPINNED-NOT-GONE
    printf 'dial tcp: lookup ghcr.io: no such host\n' > "$LOG"; NET_SOURCE_IMAGE=ghcr.io/andrenijman/rime-os@sha256:00
    pinned_image_gone || echo OFFLINE-NOT-GONE
) > "$_rf/gone" 2>&1
if [ "$(tr '\n' ' ' < "$_rf/gone")" = "GONE-PINNED UNPINNED-NOT-GONE OFFLINE-NOT-GONE " ]; then
    printf 'PASS  %-30s\n' "withdrawn image told apart"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "withdrawn image told apart" "$(tr '\n' ' ' < "$_rf/gone")"; fail=$((fail+1))
fi
# A relaunched session must land on tty1. seatd binds it to whichever VT is in
# front, and after a crash that is the VT the user switched to: in a VM the
# relaunched GUI came up on tty2 while tty1 showed boot messages. So the
# launcher brings tty1 forward before every start, and a chvt that never
# returns must not keep cage from starting.
mkdir -p "$_rf/bin"
printf '#!/bin/sh\necho "chvt $*" >> "%s/order"\n' "$_rf" > "$_rf/bin/chvt"
printf '#!/bin/sh\necho gui >> "%s/order"\n' "$_rf" > "$_rf/bin/gui"
chmod +x "$_rf/bin/chvt" "$_rf/bin/gui"
_launch_fns="$(sed -n '/^front_tty1() {/,/^}/p; /^start_gui() {/,/^}/p' rime-installer-launch)"
( set +u; PATH="$_rf/bin:$PATH"; LOG="$_rf/launch.log"; log() { :; }; GUI_CMD=("$_rf/bin/gui")
  eval "$_launch_fns"; start_gui 1 ) >/dev/null 2>&1
_order="$(tr '\n' ' ' < "$_rf/order" 2>/dev/null)"
printf '#!/bin/sh\nexec sleep 30\n' > "$_rf/bin/chvt"; : > "$_rf/order"
_t0=$SECONDS
( set +u; PATH="$_rf/bin:$PATH"; LOG="$_rf/launch.log"; log() { :; }; GUI_CMD=("$_rf/bin/gui")
  eval "$_launch_fns"; start_gui 2 ) >/dev/null 2>&1
_hung=$((SECONDS - _t0))
if [ "$_order" = "chvt 1 gui " ] && grep -qx gui "$_rf/order" && [ "$_hung" -le 10 ]; then
    printf 'PASS  %-30s\n' "relaunch lands on tty1"; pass=$((pass+1))
else
    printf 'FAIL  %-30s order=[%s] hung-chvt start took %ss\n' "relaunch lands on tty1" "$_order" "$_hung"; fail=$((fail+1))
fi
rm -rf "$_rf"

_pin="$_wf/pin-netinstall-image.yml"
if [ -f "$_pin" ] && grep -q -- '--preserve-digests' "$_pin" \
   && grep -q 'netinstall-\$RELEASE' "$_pin" && grep -q 'write-once' "$_pin" \
   && grep -q "grep -q 'manifest unknown'" "$_pin" \
   && grep -q 'pin-netinstall-image.yml' ./build-live-iso.sh \
   && ! grep -q 'release=vX.Y.Z' ./build-live-iso.sh; then
    printf 'PASS  %-30s\n' "netinstall digest gets pinned"; pass=$((pass+1))
else
    printf 'FAIL  %-30s %s\n' "netinstall digest gets pinned" \
        "pin-netinstall-image.yml missing or not write-once/--preserve-digests, or build-live-iso.sh no longer says to run it"
    fail=$((fail+1))
fi

echo "── GUI: every page must draw — it is the only front end there is ──────"

GUI=./rime-installer-gui
GUITEST=localhost/rime-guitest:latest       # gtk4/libadwaita/cage/grim/python3-cairo
RANDR=localhost/rime-guitest-randr:latest   # + wlr-randr, to drive the output geometry
SIZES="1024x600 1366x768"

# The page list comes from the GUI's own registry (the add_named loop in
# startup()), never from a list here that would rot the first time a page is
# added — the secureboot page appeared exactly that way.
PAGES=$(sed -n '/for name, build in (/,/):/p' "$GUI" \
        | grep -oE '"[a-z]+"' | tr -d '"' | awk '!seen[$0]++' | xargs)

gui_skip=0
case " $PAGES " in
    *" welcome "*) [ "$(wc -w <<<"$PAGES")" -ge 6 ] || gui_skip=1 ;;
    *) gui_skip=1 ;;
esac
if [ "$gui_skip" = 1 ]; then
    printf 'FAIL  %-30s could not read the page registry from %s (got: "%s")\n' \
        "gui: page registry" "$GUI" "$PAGES"
    fail=$((fail+1))
fi

# The render images live in ROOT podman storage; build them here if absent so
# the test is runnable on a fresh machine. wlroots' headless output is
# hard-wired to 1280x720 — the only supported way to get another geometry is
# the wlr-output-management protocol, which cage speaks and wlr-randr drives.
# Hence the one-package derived image.
# An EMPTY build context, made here rather than named. This used to be
# /var/empty, which exists on Fedora and does NOT exist on a stock
# ubuntu-24.04 GitHub runner — so `podman build` failed instantly with a
# missing-context error, and because the build's output went to /dev/null the
# suite reported only "could not build", with the actual reason discarded.
#
# That went unnoticed because this job is gated on installer changes and the
# roadmap branches had not touched installer/ until now. It is a pre-existing
# defect surfaced by this branch, not one it introduced.
BUILD_CTX=$(mktemp -d /tmp/rime-guitest-ctx.XXXXXX)
build_img() {   # build_img <tag> <containerfile-on-stdin>
    local tag=$1 err
    if err=$(sudo -n podman build -t "$tag" -f - "$BUILD_CTX" 2>&1 >/dev/null); then
        return 0
    fi
    # The reason, not just the verdict. A build that fails for no stated cause
    # is the shape of bug this whole file exists to prevent.
    printf 'FAIL  %-30s could not build %s\n' "gui: render image" "$tag"
    printf '      podman said: %s\n' "$(printf '%s' "$err" | tail -3 | tr '\n' ' ')"
    fail=$((fail+1)); gui_skip=1
    return 1
}

if [ "$gui_skip" = 0 ] && ! sudo -n podman image exists "$GUITEST" 2>/dev/null; then
    echo "      ($GUITEST missing — building it, first run only)"
    printf 'FROM registry.fedoraproject.org/fedora:43\nRUN dnf install -y cage gtk4 libadwaita python3-gobject gobject-introspection python3-cairo cairo-gobject mesa-dri-drivers seatd grim && dnf clean all\n' \
        | build_img "$GUITEST"
fi
if [ "$gui_skip" = 0 ] && ! sudo -n podman image exists "$RANDR" 2>/dev/null; then
    printf 'FROM %s\nRUN dnf install -y wlr-randr && dnf clean all\n' "$GUITEST" \
        | build_img "$RANDR"
fi

if [ "$gui_skip" = 0 ]; then
    WORK=$(mktemp -d /tmp/rime-gui-render.XXXXXX)
    mkdir -p "$WORK/gui" "$WORK/stub"
    # SELinux denies the container read access to $HOME even :ro, so the GUI is
    # copied beside the output dir and the whole thing is mounted :Z. The copy
    # is made fresh every run — it IS the file under test, just relabelled.
    cp "$GUI" "$WORK/gui/rime-installer-gui"

    # Stub lsblk (PATH-first inside the container): the container has no disks,
    # which would render only the empty-state pages. This presents a realistic
    # dual-boot table — ESP + Windows + Linux + a crypto_LUKS partition — so
    # disk/mode/part/confirm draw their full lists, including the blocked
    # container-member row and confirm's ERASED/KEPT/SHARED verdicts. Nothing
    # real is enumerated, let alone touched.
    cat > "$WORK/stub/lsblk" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  *NAME,MOUNTPOINT*) exit 0 ;;   # live-media scan: nothing here is live media
  *NAME,SIZE,TYPE,MODEL,TRAN,RM,SERIAL*)
    echo 'NAME="vda" SIZE="512G" TYPE="disk" MODEL="Rime Test SSD" TRAN="nvme" RM="0" SERIAL="APXTEST01"'; exit 0 ;;
  *NAME,TYPE,SIZE,FSTYPE,LABEL,PARTTYPE*)
    printf '%s\n' \
      'vda1 part 512M vfat ESP c12a7328-f81f-11d2-ba4b-00a0c93ec93b' \
      'vda2 part 220G ntfs Windows ebd0a0a2-b9e5-4433-87c0-68b6b72699c7' \
      'vda3 part 240G btrfs Linux 0fc63daf-8483-4772-8e79-3d69d8477de4' \
      'vda4 part 50G crypto_LUKS vault 0fc63daf-8483-4772-8e79-3d69d8477de4'; exit 0 ;;
  *TYPE,SIZE,FSTYPE,LABEL*)
    printf '%s\n' 'part 512M vfat ESP' 'part 220G ntfs Windows' \
                  'part 240G btrfs Linux' 'part 50G crypto_LUKS vault'; exit 0 ;;
esac
exit 0
STUB

    # Inert engine stand-in for the run page. Without it the GUI's spawn of
    # /usr/bin/rime-install fails instantly and the page bounces to "done"
    # before grim fires — the screenshot would show the wrong page. It emits
    # the two status lines the page displays, then idles. Touches nothing.
    cat > "$WORK/stub/rime-install" <<'STUB'
#!/usr/bin/env bash
echo "Installing Rime OS to /dev/vda3 (partition of /dev/vda) … (full log: /var/log/rime-install.log)"
echo "Do not power off — /dev/vda3 is being erased and rewritten from here on."
sleep 300
STUB
    # Exec bits matter: a non-executable stub is silently SKIPPED by the PATH
    # search and the REAL lsblk answers instead — measured: the disk page came
    # back listing this machine's actual drives through the container's /sys.
    chmod +x "$WORK/stub/"*

    # Measures every screenshot. One line per PNG:
    #   METRIC <file> <w> <h> <bytes> <ncolours> <bottom_clean> <actionpx> <right_clean>
    #
    # Criteria (thresholds applied by the shell below):
    #  ncolours ≥ 32 and ≥ 10000 bytes — "the page drew". A rendered page has
    #    HUNDREDS of distinct colours from font antialiasing alone (welcome
    #    measures ~700, 44 KB); a frame where GTK died is the compositor's
    #    solid fill: 1 colour, a few KB of PNG. The thresholds sit far from
    #    both, so neither theme tweaks nor compression changes can flip them.
    #  bottom_clean — frame() gives every page a 32 px background-only margin
    #    BELOW the action row. Any non-background pixel in the bottom 12 rows
    #    means the column overflowed the window and was clipped — the buttons
    #    are (at least partly) off-screen. The GUI is the only front end, so an
    #    unreachable Continue is a stranded user; this is that detector.
    #  actionpx ≥ 150 — non-background pixels in rows [h-90, h-12). A visible
    #    action row (min-height-44 buttons sitting directly above the margin)
    #    paints thousands there. This closes the one hole in bottom_clean: an
    #    overflow that happens to cut inside the background gap just ABOVE the
    #    buttons leaves the bottom strip clean while the buttons are still
    #    off-screen. The run page has no buttons by design (an install must not
    #    be abortable mid-write); its "Do not power off" caption occupies the
    #    same band, so the check holds there too.
    #  right_clean — the same idea sideways: a three-button action row that
    #    does not fit paints the last 8 columns; catches horizontal clipping.
    cat > "$WORK/analyze.py" <<'PY'
import cairo, os
OUT = "/out"
for f in sorted(os.listdir(OUT)):
    if not f.endswith(".png"):
        continue
    p = os.path.join(OUT, f)
    s = cairo.ImageSurface.create_from_png(p)
    w, h, stride = s.get_width(), s.get_height(), s.get_stride()
    ints = memoryview(bytes(s.get_data())).cast("I")  # one uint32 per pixel
    spx = stride // 4
    bg = ints[0]  # (0,0) sits inside the page's top margin: always background
    colours = set()
    for y in range(h):
        colours.update(ints[y*spx : y*spx + w])
    bottom_clean = int(all(v == bg for y in range(h-12, h)
                           for v in ints[y*spx : y*spx + w]))
    right_clean = int(all(ints[y*spx + x] == bg
                          for y in range(h) for x in range(w-8, w)))
    actionpx = sum(1 for y in range(max(0, h-90), h-12)
                   for v in ints[y*spx : y*spx + w] if v != bg)
    print("METRIC", f, w, h, os.path.getsize(p), len(colours),
          bottom_clean, actionpx, right_clean)
PY

    # The pixel checks cannot see a widget squashed in the MIDDLE of a page:
    # when a page is taller than the panel, GTK shrinks body children below
    # their minimum instead of pushing the action row off — measured at
    # 1024x600, where the account page kept its buttons but swallowed the
    # Computer-name entry whole. So ask GTK itself: import the real GUI (its
    # __main__ guard makes that safe), build every page from the builders
    # registry, and print each page's MINIMUM height at each panel width. A
    # page whose minimum exceeds the panel height cannot be laid out without
    # squashing or clipping something — that is the assertion.
    cat > "$WORK/measure.py" <<'PY'
import importlib.util, os
from importlib.machinery import SourceFileLoader
import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Gtk

# SourceFileLoader explicitly: the GUI has no .py extension, so
# spec_from_file_location alone cannot infer a loader for it.
loader = SourceFileLoader("rimegui", "/out/gui/rime-installer-gui")
spec = importlib.util.spec_from_loader("rimegui", loader)
mod = importlib.util.module_from_spec(spec)
loader.exec_module(mod)

widths = [int(x) for x in os.environ["MEASURE_WIDTHS"].split()]
app = mod.Installer()

def measure(_app):
    # Runs after the GUI's own activate handler, so builders exist and the
    # RIME_GUI_* state has been seeded exactly as in a jump-to-page render.
    for name, build in app.builders.items():
        page = build()
        for w in widths:
            print("MEASURE", name, w, page.measure(Gtk.Orientation.VERTICAL, w)[0],
                  flush=True)
    app.quit()

app.connect("activate", measure)
app.run(None)
PY

    # Runs INSIDE the container: for each geometry × page, start cage on a
    # headless output, let the first client resize it with wlr-randr, exec the
    # real GUI jumped to the page via RIME_GUI_PAGE (its documented test
    # affordance), screenshot with grim, tear down. Measure everything at the
    # end in one pass.
    cat > "$WORK/inner.sh" <<'INNER'
#!/usr/bin/env bash
set -u
sizes=$1; pages=$2
export XDG_RUNTIME_DIR=/run/user/0
mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
export GSK_RENDERER=cairo GDK_BACKEND=wayland LIBGL_ALWAYS_SOFTWARE=1
export PATH=/out/stub:$PATH
install -m 0755 /out/stub/rime-install /usr/bin/rime-install
# Jump-to-page state: a partition-mode install of /dev/vda3, so confirm shows
# a per-partition verdict list and done shows the partition-mode success text.
export RIME_GUI_MODE=partition RIME_GUI_DISK=/dev/vda \
       RIME_GUI_TARGET=/dev/vda3 RIME_GUI_ESP=/dev/vda1 RIME_GUI_OK=1
for size in $sizes; do
  for p in $pages; do
    rm -f "$XDG_RUNTIME_DIR"/wayland*   # fresh socket → grim finds wayland-0
    RIME_GUI_PAGE=$p timeout 30 cage -- bash -c \
      "wlr-randr --output HEADLESS-1 --custom-mode $size >/dev/null 2>&1; sleep 1; exec python3 /out/gui/rime-installer-gui" \
      2>/dev/null &
    cpid=$!
    sleep 6                             # measured: first frame lands well within this
    grim "/out/$size-$p.png" 2>/dev/null || echo "RENDER-FAIL $size-$p"
    kill "$cpid" 2>/dev/null; wait "$cpid" 2>/dev/null
  done
done
# Layout audit (see measure.py): one more cage session, no screenshot — the
# client measures every page at every panel width and prints MEASURE lines.
rm -f "$XDG_RUNTIME_DIR"/wayland*
MEASURE_WIDTHS="$(for s in $sizes; do printf '%s ' "${s%x*}"; done)" \
  RIME_GUI_PAGE=confirm timeout 60 cage -- python3 /out/measure.py 2>/dev/null
exec python3 /out/analyze.py
INNER

    sudo -n podman run --rm --network=none -v "$WORK":/out:Z "$RANDR" \
        bash /out/inner.sh "$SIZES" "$PAGES" >"$WORK/render.log" 2>&1 || true

    for size in $SIZES; do
        for p in $PAGES; do
            name="gui: $p @ $size"
            line=$(grep -m1 "^METRIC $size-$p\.png " "$WORK/render.log" || true)
            if [ -z "$line" ]; then
                printf 'FAIL  %-30s no screenshot produced (see %s/render.log)\n' \
                    "$name" "$WORK"
                fail=$((fail+1)); continue
            fi
            read -r _ _ w h bytes ncolours bclean apx rclean <<<"$line"
            why=""
            [ "${w}x${h}" = "$size" ] \
                || why="rendered ${w}x${h}, wanted $size (mode-set failed)"
            if [ "$ncolours" -lt 32 ] || [ "$bytes" -lt 10000 ]; then
                why="${why:+$why; }blank frame ($ncolours colours, $bytes bytes) — page did not draw"
            fi
            [ "$bclean" = 1 ] \
                || why="${why:+$why; }content clipped at the BOTTOM edge — action row off-screen"
            [ "$apx" -ge 150 ] \
                || why="${why:+$why; }action-row band empty — buttons not visible"
            [ "$rclean" = 1 ] \
                || why="${why:+$why; }content clipped at the RIGHT edge"
            W=${size%x*}; H=${size#*x}
            minh=$(grep -m1 "^MEASURE $p $W " "$WORK/render.log" | awk '{print $4}')
            if [ -z "$minh" ]; then
                why="${why:+$why; }page was never measured (measure.py died — see render.log)"
            elif [ "$minh" -gt "$H" ]; then
                why="${why:+$why; }needs ${minh}px height at ${W}px wide — a $size panel squashes or hides part of it"
            fi
            if [ -z "$why" ]; then
                printf 'PASS  %-30s\n' "$name"; pass=$((pass+1))
            else
                printf 'FAIL  %-30s %s\n' "$name" "$why"; fail=$((fail+1))
            fi
        done
    done
    echo "      (screenshots kept in $WORK for eyeballing)"
fi

echo
echo "──────────────────────────────────────────────────────────────────────"
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
