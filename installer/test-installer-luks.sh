#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-luks.sh — the encryption decision, its refusals, and the
#  keyboard conversion that decides whether the owner can type their own
#  passphrase. Everything here runs WITHOUT touching a block device.
#
#  The companion suite test-installer-luks-live.sh does the other half: a real
#  `bootc install` onto a real LUKS2 volume on a loopback file. It needs root,
#  a 15 GB image and ~20 GB of disk, so it cannot run on a CI runner and is
#  listed in tests/suites-not-in-ci.txt. THIS file is the half CI runs, and it
#  is deliberately the half that guards the refusals — because every refusal
#  here is a case where the alternative is a disk the owner cannot open.
#
#  ── what is under test ──
#
#  1. `encrypt=` is MANDATORY. Not "defaults to no", not "defaults to yes".
#     Both silent defaults are wrong in opposite directions and the engine
#     refuses to pick one. Asserted in both arms: missing is refused, and a
#     bogus value is refused.
#  2. Every refusal fires BEFORE anything is written. Each case's expected text
#     contains "Nothing has been erased" or stops at the block-device check,
#     and none of them names a real device.
#  3. The passphrase rules, which exist because the unlock prompt is a kernel
#     console prompt: printable ASCII only (no accents, no input method, no
#     compose key at that prompt) and at least 8 characters.
#  4. A Rime OS image with no /usr/libexec/rime-luks-enroll cannot produce a
#     recovery key, so it may not produce an encrypted disk either. That
#     refusal is asserted, and so is its inverse — with a helper present the
#     same answers reach the dry-run stop.
#  5. XKB layout -> console keymap. `gb` is `uk`, `ch` is `sg`, `latam` is
#     `la-latin1`, `jp` is `jp106`; writing the XKB name into vconsole.conf
#     gives loadkeys a name it does not know, it fails, and the console stays
#     on `us` — which on an encrypted machine is the owner locked out of their
#     own disk. The conversion function is SOURCED OUT OF THE SHIPPED ENGINE,
#     not copied here, so this tests what installs.
#  6. MUTATION. Every engine-level assertion is re-run against a COPY of the
#     engine with the guard under test removed, and must then fail. A guard
#     that cannot be made to fail is not being tested. The original engine is
#     never modified: the mutant is a separate file.
#
#  PASS = every case behaves as named AND every mutant flips the result.
#  Run from the repo's installer/ directory. Needs passwordless root (sudo -n)
#  and podman, for the same reason test-installer.sh does: the engine refuses
#  to run unprivileged and checks for an image before parsing arguments.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 1

ENGINE=./rime-install
WORK=$(mktemp -d /tmp/rime-luks-suite.XXXXXX)
# The engine refuses to start without bootc (it installs with the live env's
# own), and the GitHub runner this suite runs on has none. Every engine case
# here is a dry run that stops before bootc would run, so a stub that is never
# executed is all preflight needs; if one ever IS executed, it fails loudly
# rather than pretending to have installed anything. RIME_BOOTC is the engine's
# seam for exactly this, and sudo's env_reset means it is passed per call.
BOOTC_STUB="$WORK/bootc-stub"
printf '#!/bin/sh\necho "bootc stub executed by a dry run: $*" >&2\nexit 99\n' > "$BOOTC_STUB"
chmod 755 "$BOOTC_STUB"
ANS="$WORK/answers"
SCRATCH_IMAGE="localhost/rime-luks-probe:test"
ENGINE_IMAGE=""
scratch_made=0
cleanup() {
    # The netinstall section attaches loop devices. They are released on the
    # happy path, but a run that dies in the middle would otherwise leave a
    # loop device holding a 32 GiB file open — and the next run would then pick
    # a DIFFERENT loop number and leak again. Belt and braces, and harmless
    # when the variables were never set.
    [ -n "${LOOP_OK:-}" ]    && sudo -n losetup -d "${LOOP_OK}"    >/dev/null 2>&1
    [ -n "${LOOP_SMALL:-}" ] && sudo -n losetup -d "${LOOP_SMALL}" >/dev/null 2>&1
    rm -f "${NETIMG_OK:-}" "${NETIMG_SMALL:-}" 2>/dev/null
    rm -rf "$WORK"
    [ "$scratch_made" = 1 ] && sudo -n podman rmi -f "$SCRATCH_IMAGE" >/dev/null 2>&1
    return 0
}
trap cleanup EXIT

pass=0; fail=0
ok()  { printf 'PASS  %-46s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-46s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Same trick test-installer.sh uses: the engine's preflight refuses to continue
# without a Rime OS image in ROOT podman storage, and that check runs before
# argument parsing. An empty tar imported by podman is a valid image with no
# layers — no network, no build, removed on exit.
ensure_engine_image() {
    command -v podman >/dev/null 2>&1 || return 1
    sudo -n true 2>/dev/null || return 1
    if sudo -n podman image exists localhost/rime-os:daily 2>/dev/null; then
        ENGINE_IMAGE="localhost/rime-os:daily"; return 0
    fi
    local t; t=$(mktemp "$WORK/empty.XXXXXX.tar") || return 1
    tar -cf "$t" -T /dev/null 2>/dev/null \
        && sudo -n podman import -q "$t" "$SCRATCH_IMAGE" >/dev/null 2>&1
    local rc=$?
    rm -f "$t"
    [ "$rc" = 0 ] || return 1
    scratch_made=1
    ENGINE_IMAGE="$SCRATCH_IMAGE"
    return 0
}

ENGINE_RUNNABLE=1
if ! ensure_engine_image; then
    ENGINE_RUNNABLE=0
    echo "SKIP: the engine half cannot run here — it needs a Rime OS image in ROOT"
    echo "      podman storage and passwordless sudo. The keymap half still runs."
fi

# A stand-in for /usr/libexec/rime-luks-enroll that the engine can find without
# an image carrying one. It is only ever reached by the dry-run cases, which
# stop before any enrolment happens; the live suite exercises a real one.
HELPER="$WORK/enroll-stub"
printf '#!/bin/sh\nexit 0\n' > "$HELPER"; chmod 755 "$HELPER"

# ── the keymap data the engine's conversion reads, AS A FIXTURE ─────────────
#
# WHY. The assertions further down run the SHIPPED ENGINE and read back the
# console keymap it resolved. Until now they read it out of whatever kbd
# package the machine running this suite happened to have — and a GitHub
# ubuntu-24.04 runner has no /usr/lib/kbd/keymaps and no
# /usr/share/systemd/kbd-model-map in the shape the engine expects. So every
# layout resolved to the `us` fallback, three assertions were red, and this
# suite had NEVER ONCE PASSED in CI since it landed on 2026-09-20:
#
#   FAIL XKB bg resolves to console keymap bg_bds-utf8 (end to end)  KEYMAP=us
#   FAIL vn + a passphrase containing '1'  out='typeable: yes console=us'
#
# That is the same hermeticity defect test-installer-locale.sh was fixed for
# (25/1 -> 26/0): a suite whose verdict is a property of the tester's machine
# rather than of the code under test.
#
# WHAT MOVES INTO THE FIXTURE AND WHAT DOES NOT. The claim "Fedora's kbd really
# does ship bg_bds-utf8, and `vn` really has no digit 1 anywhere in its table"
# is REAL DATA and stays where it belongs: installer/keymap-checks.sh asserts
# it over all 99 XKB layouts and all 562 keymaps, on a Fedora host or inside
# quay.io/fedora/fedora:43. What the fixture makes hermetic is the ENGINE
# WIRING — that an operator's `keymap=` reaches console_keymap_for(), that its
# answer reaches the install summary and the --check-passphrase verdict, and
# that an untypeable character is NAMED rather than merely counted. That wiring
# has to give the same answer on every machine, and now does.
#
# The engine reads the fixture through RIME_KBD_KEYMAPS / RIME_KBD_MODEL_MAP,
# the same shape of testing hook as the RIME_IMAGE the rest of this file
# already uses. Nothing the GUI, the answers file or the kernel command line
# can reach sets them.
#
# PROVED CONSULTED, NOT ASSUMED. Every fixture-backed assertion below is paired
# with a control that re-runs the identical call against an EMPTY tree and
# requires the `us` fallback back — which is the CI red reproduced on purpose.
# Without that pair a fixture that was silently ignored would look like a pass.
KBD_TREE="$WORK/kbdfix/keymaps"
KBD_MODELMAP="$WORK/kbdfix/kbd-model-map"
KBD_NONE="$WORK/kbdnone"
mkdir -p "$KBD_TREE/xkb" "$KBD_NONE"

# Shaped like the xkb-converted maps kbd ships: one `keycode N = ...` line per
# key, with keysym NAMES for the digits and punctuation. The names are not
# decoration — they are what makes the engine's own name table load-bearing,
# and a fixture written with bare characters would leave that table untested.
{
    printf 'keymaps 0-2,4-5,8,12\n'
    printf 'keycode %3d = %s %s\n' \
        16 q Q  17 w W  18 e E  19 r R  20 t T  21 y Y  22 u U  23 i I \
        24 o O  25 p P  30 a A  31 s S  32 d D  33 f F  34 g G  35 h H \
        36 j J  37 k K  38 l L  44 z Z  45 x X  46 c C  47 v V  48 b B \
        49 n N  50 m M
    printf 'keycode %3d = %s\n' \
        3 'two at'            4 'three numbersign'  5 'four dollar' \
        6 'five percent'      7 'six asciicircum'   8 'seven ampersand' \
        9 'eight asterisk'   10 'nine parenleft'   11 'zero parenright' \
        57 'space'
} > "$KBD_TREE/xkb/vn.map"
# The `1` key is the one thing this map does NOT have — the hole the real `vn`
# has, and the whole point of the vn case below.

# bg_bds-utf8 is the same map plus the digit 1, and it is GZIPPED: the engine's
# _keymap_cat() has a zcat branch for exactly the form Fedora ships, and a
# fixture of plain files would never enter it.
{ cat "$KBD_TREE/xkb/vn.map"; printf 'keycode %3d = one exclam\n' 2; } \
    > "$KBD_TREE/xkb/bg_bds-utf8.map"
gzip -n "$KBD_TREE/xkb/bg_bds-utf8.map" 2>/dev/null || true

# Both real `bg` rows, copied verbatim from /usr/share/systemd/kbd-model-map,
# in the real file's order. Two rows, not one, and that is deliberate:
#   * `bg,us` is a LIST, so it only matches through the engine's
#     `index($2, l ",")==1` clause — the clause whose absence is what sent bg
#     to `us` with bg_bds-utf8.map.gz sitting unused in the tree;
#   * bg_pho-utf8 comes FIRST and carries a variant, so an engine that took
#     the first row for the layout regardless of variant would answer
#     bg_pho-utf8 — for which this fixture deliberately ships NO keymap file,
#     so the engine's step-4 "does the table's answer actually exist" check
#     would then drop it to `us` and the assertion would go red.
printf '%s\n' \
    '# fixture — two rows copied from /usr/share/systemd/kbd-model-map' \
    'bg_pho-utf8\tbg,us\tpc105\t,phonetic\tterminate:ctrl_alt_bksp,grp:shifts_toggle' \
    'bg_bds-utf8\tbg,us\tpc105\t-\tterminate:ctrl_alt_bksp,grp:shifts_toggle' \
    | sed 's/\\t/\t/g' > "$KBD_MODELMAP"

for _f in "$KBD_TREE/xkb/vn.map" "$KBD_MODELMAP"; do
    [ -s "$_f" ] || { echo "FATAL: the keymap fixture was not built: $_f"; exit 1; }
done

# ── run one answers file against an engine (the real one, or a mutant) ───────
run_engine() {  # $1=engine path  $2=answers body  [$3..]=extra env assignments
    local eng="$1" body="$2"; shift 2
    printf '%s\n' "$body" > "$ANS"
    sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_DRY_RUN=1 RIME_IMAGE="$ENGINE_IMAGE" "$@" "$eng" --headless "$ANS" 2>&1 </dev/null
}

# $1 name, $2 expected substring, $3 answers body, $4.. extra env
check() {
    local name=$1 want=$2 body=$3; shift 3
    if [ "$ENGINE_RUNNABLE" != 1 ]; then printf 'SKIP  %-46s no engine image\n' "$name"; return; fi
    local out; out=$(run_engine "$ENGINE" "$body" "$@")
    # The ERR trap firing is always a bug in the installer, never a pass — the
    # same rule test-installer.sh states.
    if [[ "$out" == *"Unexpected error on line"* ]]; then
        bad "$name" "ERR TRAP FIRED"; return
    fi
    if [[ "$out" == *"$want"* ]]; then ok "$name"
    else bad "$name" "wanted '$want'; got: $(printf '%s' "$out" | grep -m1 'RIME-INSTALL-' || echo '<no sentinel>')"
    fi
}

# Mutation: copy the engine, delete the guard under test, and require the SAME
# answers to stop behaving that way. `cp` then `cmp` on the original afterwards,
# so a mutation can never leak into the shipped file.
mutate_check() {  # $1 name  $2 sed program  $3 no-longer-expected substring  $4 answers  $5.. env
    local name=$1 prog=$2 gone=$3 body=$4; shift 4
    if [ "$ENGINE_RUNNABLE" != 1 ]; then printf 'SKIP  %-46s no engine image\n' "mutant: $name"; return; fi
    local mut="$WORK/mutant-engine" out
    cp "$ENGINE" "$mut" || { bad "mutant: $name" "could not copy the engine"; return; }
    chmod 755 "$mut"
    sed -i "$prog" "$mut"
    if cmp -s "$ENGINE" "$mut"; then
        bad "mutant: $name" "the mutation changed nothing — the sed program matched no line"
        return
    fi
    out=$(run_engine "$mut" "$body" "$@")
    if [[ "$out" == *"$gone"* ]]; then
        bad "mutant: $name" "the guard still fired with its code removed — the case proves nothing"
    else
        ok "mutant: $name" "guard removed -> refusal gone"
    fi
    rm -f "$mut"
}

# A disk that cannot exist: the whole-disk cases that are SUPPOSED to get past
# the encryption guards then stop at the block-device check, so nothing real is
# ever named, let alone opened.
GHOST=/dev/zzz-does-not-exist
BASE=$'mode=disk\ndisk='"$GHOST"$'\nusername=bob\npassword=pw\nhostname=rime'

echo "── the encryption decision is explicit, or refused ────────────────────"
check "encrypt= missing is refused"        "encrypt missing"      "$BASE"
check "encrypt=maybe is refused"           "bad encrypt 'maybe'"  "$BASE"$'\nencrypt=maybe'
check "encrypt=no reaches the disk check"  "is not a block device" "$BASE"$'\nencrypt=no'
# The mutation is a ONE-LINE inversion, not a block delete: a `,+Nd` range over
# a multi-line die string eats the next statement too, the mutant then fails to
# parse, and a mutant that cannot run looks exactly like a mutation that worked.
mutate_check "encrypt= missing" \
    's/\[ -n "\${ENCRYPT:-}" \]/[ -z "${ENCRYPT:-}" ]/' "encrypt missing" "$BASE"

echo
echo "── encryption needs a whole disk, and says so before erasing ──────────"
if [ -b /dev/sda ] && [ -b /dev/sda2 ] && [ -b /dev/sda1 ]; then
    check "partition mode + encrypt=yes refused" "only available when Rime OS gets a whole disk" \
        $'mode=partition\ndisk=/dev/sda\ntarget=/dev/sda2\nesp=/dev/sda1\nusername=bob\npassword=pw\nhostname=rime\nencrypt=yes\nlukspass=correcthorse'
else
    echo "SKIP  partition+encrypt case (needs /dev/sda1 and /dev/sda2 present)"
fi

echo
echo "── the passphrase must be typeable at a console prompt ────────────────"
check "lukspass missing"      "lukspass missing"             "$BASE"$'\nencrypt=yes'
check "lukspass too short"    "too short"                    "$BASE"$'\nencrypt=yes\nlukspass=abc'
check "lukspass non-ASCII"    "cannot be typed at the boot prompt" "$BASE"$'\nencrypt=yes\nlukspass=pässwörd1'
check "lukspass emoji"        "cannot be typed at the boot prompt" "$BASE"$'\nencrypt=yes\nlukspass=abcdefgh\xf0\x9f\x94\x92'
check "good lukspass gets past the passphrase rules" "is not a block device" \
    "$BASE"$'\nencrypt=yes\nlukspass=correct horse 9'
mutate_check "non-ASCII passphrase" \
    's/if LC_ALL=C grep -qv/if false \&\& LC_ALL=C grep -qv/' \
    "cannot be typed at the boot prompt" "$BASE"$'\nencrypt=yes\nlukspass=pässwörd1'
mutate_check "short passphrase" \
    's/if \[ "\${#LUKSPASS}" -lt 8 \]; then/if false; then/' \
    "too short" "$BASE"$'\nencrypt=yes\nlukspass=abc'

echo
echo "── a non-btrfs root filesystem is not an exercised combination ────────"
check "encrypt=yes + ext4 refused" "only supported with the btrfs root filesystem" \
    "$BASE"$'\nencrypt=yes\nlukspass=correcthorse\nrootfs=ext4'

echo
echo "── the dry run, the keymap it resolved, and the helper check ──────────"
# RIME_DRY_RUN runs every guard against the REAL device named and stops
# immediately before the first destructive command. A loopback file is made
# here only so the device checks have something to look at; that nothing was
# written to it is itself asserted below.
LOOPIMG=""; LOOPDEV=""
if [ "$ENGINE_RUNNABLE" = 1 ] && command -v losetup >/dev/null 2>&1; then
    LOOPIMG="$WORK/dryrun.img"
    truncate -s 20G "$LOOPIMG" 2>/dev/null && LOOPDEV=$(sudo -n losetup -fP --show "$LOOPIMG" 2>/dev/null || true)
fi
if [ -n "$LOOPDEV" ]; then
    # A REAL regular file, never a process substitution: the engine checks
    # `[ -f "$ANS" ]`, /dev/fd/N is not a regular file, and the fd would not
    # survive sudo anyway — the case would "fail" for a reason that has nothing
    # to do with what it is testing.
    #
    # keymap=bg on purpose. `bg` is one of the 36 XKB layout names (of 99) that
    # is NOT a loadable console keymap, so it is a layout where the conversion
    # has to do real work: the answer must come back as bg_bds-utf8.
    LOOP_FP=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$LOOPDEV")
    printf '%s\n' "mode=disk" "disk=$LOOPDEV" "username=bob" "password=pw" \
        "hostname=rime" "encrypt=yes" "lukspass=correct horse 9" "keymap=bg" \
        "confirmed=ERASE" "confirm_target=$LOOPDEV" "confirm_disk_id=$LOOP_FP" > "$ANS"

    # dry_run <engine> <keymap-tree> <model-map> — the same run three ways.
    dry_run() {
        sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL="$HELPER" \
                RIME_KBD_KEYMAPS="$2" RIME_KBD_MODEL_MAP="$3" \
                "$1" --headless "$ANS" 2>&1 </dev/null
    }
    # The summary line's KEYMAP=, or the empty string. Never `grep -q` in a
    # pipeline here: pipefail turns a match into 141 via SIGPIPE on the writer.
    dry_keymap() { printf '%s' "$1" | grep -o 'KEYMAP=[a-z0-9_.-]*' | tail -1; }

    out=$(dry_run "$ENGINE" "$KBD_TREE" "$KBD_MODELMAP")
    if [[ "$out" == *"RIME-INSTALL-DRYRUN-OK"* ]]; then ok "encrypt=yes reaches the dry-run stop"
    else bad "encrypt=yes reaches the dry-run stop" "$(printf '%s' "$out" | tail -3 | tr '\n' ' ')"; fi
    if [[ "$out" == *"ENCRYPT=yes"* ]]; then ok "the dry run names the encryption decision"
    else bad "the dry run names the encryption decision" "no ENCRYPT= in the summary"; fi
    if [ "$(dry_keymap "$out")" = "KEYMAP=bg_bds-utf8" ]; then
        ok "XKB bg resolves to console keymap bg_bds-utf8 (end to end)"
    else
        bad "XKB bg resolves to console keymap bg_bds-utf8 (end to end)" \
            "$(dry_keymap "$out")"
    fi

    # CONTROL, and the CI red reproduced deliberately: point the engine at an
    # EMPTY tree and the same answers file must come back `us`. If this ever
    # passes AND the line above passes, the engine is reading keymap data this
    # suite did not put there and the assertion above is measuring the machine.
    out_none=$(dry_run "$ENGINE" "$KBD_NONE" "$KBD_NONE/absent-model-map")
    if [ "$(dry_keymap "$out_none")" = "KEYMAP=us" ]; then
        ok "…and an empty keymap tree falls back to us" "so the line above read the fixture, not this machine"
    else
        bad "…and an empty keymap tree falls back to us" \
            "got '$(dry_keymap "$out_none")' — the fixture is not what the engine consulted"
    fi

    # MUTATION on the ENGINE, not on the fixture. `bg` is written `bg,us` in
    # systemd's table; a plain `$2 == layout` test matches no row of that shape,
    # and that is precisely how bg used to fall through to `us` while
    # bg_bds-utf8.map.gz sat unused in the keymap tree. Delete the clause that
    # makes a list match and the identical run must report `us`.
    KMMUT="$WORK/rime-install.multilayout-mutant"
    sed 's/index($2, l ",")==1/0/g' "$ENGINE" > "$KMMUT"
    chmod 755 "$KMMUT"
    if cmp -s "$ENGINE" "$KMMUT"; then
        bad "mutant: the multi-layout table row" \
            "the sed program matched no line — the mutant is the engine unchanged"
    else
        mout=$(dry_run "$KMMUT" "$KBD_TREE" "$KBD_MODELMAP")
        if [ "$(dry_keymap "$mout")" = "KEYMAP=us" ]; then
            ok "mutant: the multi-layout table row" "clause removed -> bg falls back to us"
        else
            bad "mutant: the multi-layout table row" \
                "got '$(dry_keymap "$mout")' with the clause gone — the assertion above proves nothing"
        fi
    fi
    rm -f "$KMMUT"
    # The dry run stops before the first destructive command, and this is the
    # assertion that says so rather than trusting the name of the flag.
    if [ -z "$(sudo -n blkid -p "$LOOPDEV" 2>/dev/null || true)" ]; then
        ok "the dry run wrote nothing to the disk it validated"
    else
        bad "the dry run wrote nothing to the disk it validated" "blkid sees something on $LOOPDEV"
    fi

    # ── the helper check, both ways, on the same answers file ─────────────
    # The run above passed WITH a helper. These two differ from it only in the
    # helper, so that pass cannot be an accident of something else letting it
    # through.
    out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL=/nonexistent/x \
             "$ENGINE" --headless "$ANS" 2>&1 </dev/null)
    if [[ "$out" == *"is not executable"* ]]; then ok "an unusable helper stops the same run"
    else bad "an unusable helper stops the same run" "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; fi
    # With no override the engine looks for the live environment's own copy at
    # /usr/libexec/rime-luks-enroll. The refusal must name the path it looked
    # for, because "encryption failed" with no path is unactionable. A Rime
    # machine HAS that file — it is the installed image's — so the refusal can
    # only be observed where it is absent (a CI runner); here it is reported as
    # a skip with the reason, not passed.
    if [ -e /usr/libexec/rime-luks-enroll ]; then
        printf 'SKIP  %-46s this machine carries /usr/libexec/rime-luks-enroll\n' "a live env without the helper is refused"
    else
        out=$(sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_IMAGE="$ENGINE_IMAGE" RIME_DRY_RUN=1 \
                 "$ENGINE" --headless "$ANS" 2>&1 </dev/null)
        if [[ "$out" == *"/usr/libexec/rime-luks-enroll"* ]]; then
            ok "a live env without the helper is refused, by name"
        else
            bad "a live env without the helper is refused, by name" "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
        fi
    fi
    sudo -n losetup -d "$LOOPDEV" 2>/dev/null || true
else
    echo "SKIP  dry-run cases (need losetup and an engine image)"
fi

echo
echo "── the ENCRYPTED NETWORK install ──────────────────────────────────────"
# `installer/rime-install` sets NETINSTALL=0 and only raises it when
# /usr/lib/rime-installer/netinstall exists, so on any developer machine or CI
# runner every assertion above this line runs the OFFLINE engine. This section
# forces the network path on, with the encrypted layout, because that is where
# the most is decided before the disk is touched: the network diagnosis, the
# registry check, the helper, and the size of the target.
#
# A network install stages nothing any more (bootc streams from the registry,
# see "Where the OS comes from" in the engine), so what a dry run can prove is
# that every refusal still happens with the disk intact, that the registry is
# asked exactly once and nothing is downloaded, and that the disk the dry run
# validated was not written.
#
# ═══ HOW THIS IS MADE HERMETIC, AND WHY THAT IS NOT A DODGE ═══
#
# A network install's first act is net_diagnose() — `ip route show default`
# then `getent hosts ghcr.io` — and its second is `skopeo inspect --raw
# docker://…`. Letting those reach the real world would make this suite's
# verdict a property of the runner's network and of whether a tag happens to
# be published.
#
# So three commands are SHIMMED on PATH, and each shim is the narrowest thing
# that will do: `ip` answers only `route show default` and execs the real
# binary for anything else, `getent` answers only `hosts ghcr.io` and execs the
# real binary for anything else, and `skopeo` answers `inspect` and REFUSES
# every other subcommand with exit 99. Every shim appends its argv to a log,
# and the log is ASSERTED — one inspect, nothing else — so "the shim was
# consulted" is measured rather than assumed.
#
# sudo has `Defaults secure_path` on this machine, so `sudo -n PATH=… engine`
# does not work: PATH is replaced. `sudo -n env PATH=… engine` does — env is
# found through secure_path and then sets PATH for the engine it execs.
NET_SHIM="$WORK/net-shim"
SHIMLOG="$WORK/net-shim.log"
mkdir -p "$NET_SHIM"
: > "$SHIMLOG"
REAL_IP=$(command -v ip 2>/dev/null || echo /usr/sbin/ip)
REAL_GETENT=$(command -v getent 2>/dev/null || echo /usr/bin/getent)
cat > "$NET_SHIM/ip" <<SHIM
#!/bin/sh
echo "ip \$*" >> "$SHIMLOG"
case "\$*" in
  "route show default") echo "default via 192.0.2.1 dev rime-test-shim proto static"; exit 0 ;;
esac
exec $REAL_IP "\$@"
SHIM
cat > "$NET_SHIM/getent" <<SHIM
#!/bin/sh
echo "getent \$*" >> "$SHIMLOG"
case "\$*" in
  "hosts ghcr.io") echo "192.0.2.10 ghcr.io"; exit 0 ;;
esac
exec $REAL_GETENT "\$@"
SHIM
cat > "$NET_SHIM/skopeo" <<SHIM
#!/bin/sh
echo "skopeo \$*" >> "$SHIMLOG"
case "\${1:-}" in
  inspect) echo '{}'; exit 0 ;;
esac
echo "SHIM-REFUSED \$*" >> "$SHIMLOG"
exit 99
SHIM
chmod 755 "$NET_SHIM/ip" "$NET_SHIM/getent" "$NET_SHIM/skopeo"

# sudo's secure_path, verbatim, so the engine still finds every real tool.
NET_PATH="$NET_SHIM:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
net_run() {   # $1 = engine, $2.. = env assignments. Reads $ANS.
    local eng="$1"; shift
    sudo -n env "PATH=$NET_PATH" RIME_BOOTC="$BOOTC_STUB" "$@" "$eng" --headless "$ANS" 2>&1 </dev/null
}
# `grep -c` PRINTS 0 and RETURNS 1 when nothing matches, so the obvious
# `grep -c … || echo 0` emits TWO lines and every `= 0` comparison against it
# is false. head -1 keeps grep's own count and ${n:-0} covers a missing file.
shim_count() {  # $1 = pattern
    local n
    n=$(grep -c -- "$1" "$SHIMLOG" 2>/dev/null | head -1)
    printf '%s' "${n:-0}"
}

# ── the loop-backed targets ─────────────────────────────────────────────────
# SPARSE FILES, and the dry run writes nothing to either — which is itself
# asserted with blkid below, after the runs. One under the engine's minimum,
# one comfortably over it.
NETLOOPDIR="${RIME_LOOP_DIR:-/var/lab-scratch}"
{ [ -d "$NETLOOPDIR" ] && [ -w "$NETLOOPDIR" ]; } || NETLOOPDIR="$WORK"
LOOP_OK=""; LOOP_SMALL=""; NETIMG_OK=""; NETIMG_SMALL=""
net_release() {
    [ -n "$LOOP_OK" ]    && sudo -n losetup -d "$LOOP_OK"    2>/dev/null
    [ -n "$LOOP_SMALL" ] && sudo -n losetup -d "$LOOP_SMALL" 2>/dev/null
    rm -f "$NETIMG_OK" "$NETIMG_SMALL" 2>/dev/null
    return 0
}
if [ "$ENGINE_RUNNABLE" = 1 ] && command -v losetup >/dev/null 2>&1; then
    NETIMG_OK=$(mktemp "$NETLOOPDIR/rime-luks-net-ok.XXXXXX.img")
    NETIMG_SMALL=$(mktemp "$NETLOOPDIR/rime-luks-net-small.XXXXXX.img")
    truncate -s 32G "$NETIMG_OK"   2>/dev/null && LOOP_OK=$(sudo -n losetup -fP --show "$NETIMG_OK" 2>/dev/null || true)
    truncate -s 6G  "$NETIMG_SMALL" 2>/dev/null && LOOP_SMALL=$(sudo -n losetup -fP --show "$NETIMG_SMALL" 2>/dev/null || true)
fi

net_answers() {  # $1 = disk
    local disk_fp
    disk_fp=$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$1")
    printf '%s\n' "mode=disk" "disk=$1" "username=bob" "password=pw" \
        "hostname=rime" "encrypt=yes" "lukspass=correct horse 9" "keymap=us" \
        "confirmed=ERASE" "confirm_target=$1" "confirm_disk_id=$disk_fp" > "$ANS"
}

if [ -n "$LOOP_OK" ] && [ -n "$LOOP_SMALL" ]; then
    # ── 1. a disk under the minimum is refused, intact, before the network ──
    : > "$SHIMLOG"
    net_answers "$LOOP_SMALL"
    out=$(net_run "$ENGINE" RIME_NETINSTALL=1 RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL="$HELPER")
    if [[ "$out" == *"Rime OS needs at least"* ]] && [[ "$out" == *"Nothing has been erased"* ]]; then
        ok "netinstall+encrypt: a disk under the minimum is refused"
    else
        bad "netinstall+encrypt: a disk under the minimum is refused" \
            "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
    fi
    if [ "$(shim_count '^skopeo')" = 0 ]; then
        ok "…before the registry is asked anything" "no skopeo call was made"
    else
        bad "…before the registry is asked anything" "skopeo calls: $(shim_count '^skopeo')"
    fi

    # ── 2. a disk over it reaches the dry-run stop, having downloaded nothing
    : > "$SHIMLOG"
    net_answers "$LOOP_OK"
    out=$(net_run "$ENGINE" RIME_NETINSTALL=1 RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL="$HELPER")
    if [[ "$out" == *"RIME-INSTALL-DRYRUN-OK"* ]]; then
        ok "netinstall+encrypt: a 32 GiB disk reaches the dry-run stop"
    else
        bad "netinstall+encrypt: a 32 GiB disk reaches the dry-run stop" \
            "$(printf '%s' "$out" | tail -3 | tr '\n' ' ')"
    fi
    # Printed only on a network install, because there the download and the
    # destruction are the same step and the owner has to be told before it.
    if [[ "$out" == *"downloads onto it as it goes"* ]]; then
        ok "…and warns that the download and the wipe are one step"
    else
        bad "…and warns that the download and the wipe are one step" "no such note in the output"
    fi
    if [ "$(shim_count '^skopeo inspect')" = 1 ] && [ "$(shim_count '^skopeo')" = 1 ]; then
        ok "…having asked the registry once and downloaded nothing" "1 inspect, nothing else"
    else
        bad "…having asked the registry once and downloaded nothing" \
            "inspect=$(shim_count '^skopeo inspect') all=$(shim_count '^skopeo')"
    fi
    if [ "$(shim_count '^ip route show default')" != 0 ] && [ "$(shim_count '^getent hosts ghcr.io')" != 0 ]; then
        ok "…and the network diagnosis ran through the shim" "so this case is hermetic"
    else
        bad "…and the network diagnosis ran through the shim" \
            "ip=$(shim_count '^ip route show default') getent=$(shim_count '^getent hosts ghcr.io')"
    fi
    if [ -z "$(sudo -n blkid -p "$LOOP_OK" 2>/dev/null || true)" ]; then
        ok "…and the dry run wrote nothing to the disk it validated"
    else
        bad "…and the dry run wrote nothing to the disk it validated" "blkid sees something on $LOOP_OK"
    fi

    # ── 3. the helper is asked before the network, on this path too ────────
    # It is the live environment's own copy now, so there is no image to wait
    # for and no reason to defer the question past anything.
    : > "$SHIMLOG"
    out=$(net_run "$ENGINE" RIME_NETINSTALL=1 RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL=/nonexistent/rime-luks-enroll)
    if [[ "$out" == *"is not executable"* ]] && [ "$(shim_count '^skopeo')" = 0 ]; then
        ok "netinstall+encrypt: no helper is refused before the registry is asked"
    else
        bad "netinstall+encrypt: no helper is refused before the registry is asked" \
            "skopeo=$(shim_count '^skopeo') $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
    fi

    # MUTATION. Take the minimum-size refusal away and the 6 GiB disk must stop
    # being refused: that is what makes case 1 a test of the check and not of
    # something else that happens to say "needs at least".
    NETMUT="$WORK/mutant-netsize"
    cp "$ENGINE" "$NETMUT"; chmod 755 "$NETMUT"
    sed -i 's|^  \*) \[ "\$_sz" -ge "\$_min_bytes" \] \\$|  *) true \\|' "$NETMUT"
    if cmp -s "$ENGINE" "$NETMUT"; then
        bad "mutant: the minimum-size check" "the sed program matched no line"
    elif ! bash -n "$NETMUT" 2>/dev/null; then
        bad "mutant: the minimum-size check" "the mutant does not parse"
    else
        net_answers "$LOOP_SMALL"
        mout=$(net_run "$NETMUT" RIME_NETINSTALL=1 RIME_DRY_RUN=1 RIME_LUKS_ENROLL_LOCAL="$HELPER")
        if [[ "$mout" == *"Rime OS needs at least"* ]]; then
            bad "mutant: the minimum-size check" "the refusal survived its own deletion"
        else
            ok "mutant: the minimum-size check" "removed -> a 6 GiB disk would be erased"
        fi
    fi
    rm -f "$NETMUT"

    # CONTROL for the "wrote nothing" assertion: it is a measurement only if
    # blkid would have SAID something had there been something to say.
    sudo -n mkfs.ext4 -q -F "$LOOP_SMALL" >/dev/null 2>&1
    if [ -n "$(sudo -n blkid -p "$LOOP_SMALL" 2>/dev/null || true)" ]; then
        ok "…and blkid would have seen a write if there had been one" "control: mkfs is reported"
    else
        bad "…and blkid would have seen a write if there had been one" \
            "blkid reported nothing even after mkfs — the 'wrote nothing' case proves nothing"
    fi
elif [ "$ENGINE_RUNNABLE" != 1 ]; then
    printf 'SKIP  %-46s no engine image\n' "netinstall+encrypt cases"
else
    bad "netinstall+encrypt cases could not run" \
        "engine=$ENGINE_RUNNABLE loops='$LOOP_OK' '$LOOP_SMALL'"
fi
net_release

echo
echo "── the order the encrypted path does things in ────────────────────────"
# RIME_DRY_RUN stops the engine before the first destructive command, which is
# above everything the encrypted branch does, so no dry run can reach the
# order of its steps; a real encrypted install (test-installer-luks-live.sh,
# not in CI) and a reading of the source can. This reads the source and asks
# the question the branch exists to answer: is the recovery key created, and
# proven to open the volume, BEFORE bootc downloads or writes anything?
enc_scan() {   # $1 = engine file
    awk '
      /cryptsetup luksFormat "\$\{LUKS_FMT\[@\]\}"/ && !luksfmt { luksfmt = NR }
      luksfmt && /"\$LUKS_ENROLL_PATH" \\$/ && !enrol { enrol = NR }
      enrol && /recovery key does not open the encrypted volume/ && !proven { proven = NR }
      proven && /run_bootc_install "\$DISK" "\$TROOT" "\$\{LUKS_KARGS\[@\]\}"/ && !bootc { bootc = NR }
      END { printf "luksfmt=%d enrol=%d proven=%d bootc=%d\n", luksfmt, enrol, proven, bootc }
    ' "$1"
}
encline=$(enc_scan "$ENGINE")
e_bootc=$(printf '%s\n' "$encline" | tr ' ' '\n' | sed -n 's/^bootc=//p')
if [ "${e_bootc:-0}" != 0 ]; then
    ok "the recovery key is made and proven before bootc runs" "$encline"
else
    bad "the recovery key is made and proven before bootc runs" "$encline"
fi
# THE SCAN IS NOT A TAUTOLOGY: move the bootc call above the enrolment in a
# copy, and the same scan must stop finding the order.
ORDPROBE="$WORK/engine-reordered"
cp "$ENGINE" "$ORDPROBE"
if python3 - "$ORDPROBE" <<'PYMUT' 2>/dev/null
import sys
p = sys.argv[1]
lines = open(p, encoding="utf-8").read().split("\n")
call = next(i for i, l in enumerate(lines)
            if l.strip() == 'run_bootc_install "$DISK" "$TROOT" "${LUKS_KARGS[@]}" 2>&1 | tee -a "$LOG"')
note = next(i for i, l in enumerate(lines) if l.strip() == 'note "Creating the recovery key …"')
moved = lines.pop(call)
lines.insert(note, moved)
open(p, "w", encoding="utf-8").write("\n".join(lines))
PYMUT
then
    if cmp -s "$ENGINE" "$ORDPROBE"; then
        bad "the order scan can also say no" "the mutation changed nothing"
    else
        probeline=$(enc_scan "$ORDPROBE")
        case "$probeline" in
            *"bootc=0"*) ok "the order scan can also say no" "bootc moved above the enrolment -> $probeline" ;;
            *) bad "the order scan can also say no" "$probeline — the scan may be measuring nothing" ;;
        esac
    fi
else
    bad "the order scan can also say no" "could not build the reordered mutant"
fi
rm -f "$ORDPROBE"

echo
echo "── a loopback target must not be able to reach this machine's NVRAM ───"
# WHY THIS IS IN THE ENCRYPTION SUITE. The live half of this suite points a
# real `bootc install to-filesystem` at a LOOPBACK FILE on a developer's own
# machine. On 2026-09-20 exactly that shape of run — a loopback install with
# the host's efivarfs visible — deleted a laptop's real `Rime OS` boot entry
# and recreated it against the loop device's ESP. The laptop would not boot.
# BOOT-BREAKAGE-2026-09-20.md.
#
# These assertions run the REAL FUNCTIONS OUT OF THE SHIPPED ENGINE against a
# fabricated sysfs tree, so they measure behaviour and not the presence of a
# string in a file.
NVFNS="$WORK/nvram-fns.sh"
FAKESYS="$WORK/sysblock"
mkdir -p "$FAKESYS/loop9/loop"
printf '/var/lab-scratch/pretend.img\n' > "$FAKESYS/loop9/loop/backing_file"

# nvram_probe ENGINE DEVICE [RIME_SYSFS_BLOCK] — prints what the named engine
# would hand bootc for that device, and whether it would mask efivars:
# "<NVRAM_BOOTC_ARGS> mask=<0|1>", or `EXTRACT-FAILED`.
nvram_probe() {
    local eng="$1" dev="$2" seam="${3:-}"
    sed -n '/^disk_is_loopback()/,/^}/p;/^set_nvram_args_for()/,/^}/p' "$eng" > "$NVFNS"
    grep -q 'set_nvram_args_for' "$NVFNS" || { echo "EXTRACT-FAILED"; return; }
    RIME_SYSFS_BLOCK="$seam" bash -c '
        log()  { :; }
        note() { :; }
        . "$1"
        set_nvram_args_for "$2"
        printf "%s mask=%s\n" "${NVRAM_BOOTC_ARGS[*]-}" "${NVRAM_MASK:-unset}"
    ' _ "$NVFNS" "$dev" 2>/dev/null
}

got=$(nvram_probe "$ENGINE" /dev/loop9 "$FAKESYS")
# THE PREVENTION, and it is the bootc flag and not the mount: bootc's own help
# for --generic-image is "Changes to the system firmware will be skipped."
case "$got" in
    *"--generic-image"*)
        ok "a loop-backed target skips the firmware step" "$got" ;;
    EXTRACT-FAILED*)
        bad "a loop-backed target skips the firmware step" "could not extract the functions from the engine" ;;
    *)  bad "a loop-backed target skips the firmware step" "got '${got:-<empty>}'" ;;
esac
# Defence in depth, kept and asserted, but never mistaken for the guard.
case "$got" in
    *"mask=1"*) ok "and still masks efivars around bootc" "second layer, not the first" ;;
    *)          bad "and still masks efivars around bootc" "got '${got:-<empty>}'" ;;
esac

# The inverse, and it is the one that must not regress: a REAL disk still gets
# the firmware, because a real install has to create a boot entry or the
# machine it just installed will not start.
got=$(nvram_probe "$ENGINE" /dev/nvme0n1 "$FAKESYS")
if [ "$(printf '%s' "$got" | tr -d '[:space:]')" = "mask=0" ]; then
    ok "a real block device still reaches the firmware" "no extra arguments, no mask"
else
    bad "a real block device still reaches the firmware" "engine would have used '$got'"
fi

# The seam only ever ADDS loop-ness: with the override unset, the fabricated
# tree is invisible and /dev/loop9 (which does not exist here) is not loop.
got=$(nvram_probe "$ENGINE" /dev/loop9 "")
if [ "$(printf '%s' "$got" | tr -d '[:space:]')" = "mask=0" ]; then
    ok "the test seam cannot hide a real loop device" "unset override -> real sysfs only"
else
    bad "the test seam cannot hide a real loop device" "got '$got' with the override unset"
fi

# STRUCTURAL: an install call site added later without the guard would pass
# every behavioural assertion above and still brick a laptop. So the engine is
# read as a whole: every executable line that starts a bootc install is found,
# and each must be inside run_bootc_install, which must call set_nvram_args_for
# on its disk before it and hand NVRAM_BOOTC_ARGS to bootc.
nv_scan() {  # $1 = engine file
    awk '
      /^run_bootc_install\(\) \{/ { infn = 1; fnstart = NR }
      infn && /^\}/ { infn = 0 }
      infn && /set_nvram_args_for "\$disk"/ && !setl { setl = NR }
      infn && /NVRAM_BOOTC_ARGS\[@\]/ && !argl { argl = NR }
      /^[[:space:]]*#/ { next }
      /install (to-filesystem|to-disk|to-existing-root)/ {
        sites++
        if (infn) inside++; else printf "OUTSIDE:%d ", NR
      }
      END { printf "sites=%d inside=%d set=%d args=%d\n", sites, inside, setl, argl }
    ' "$1"
}
nvscan=$(nv_scan "$ENGINE")
case "$nvscan" in
    *OUTSIDE*) bad "the one bootc call site is guarded" "$nvscan" ;;
    *"sites=1 inside=1 set=0 "*|*" args=0"*) bad "the one bootc call site is guarded" "$nvscan" ;;
    *"sites=1 inside=1 "*) ok "the one bootc call site is guarded" "$nvscan" ;;
    *) bad "the one bootc call site is guarded" "$nvscan" ;;
esac

# MUTATION for the scan itself: take the guard out of run_bootc_install and
# the scan must say so.
MUT_SITE="$WORK/mutant-site"
cp "$ENGINE" "$MUT_SITE"
sed -i '/^run_bootc_install() {/,/^}/{/^  set_nvram_args_for "\$disk"$/d}' "$MUT_SITE"
if cmp -s "$ENGINE" "$MUT_SITE"; then
    bad "mutant: the call site loses its guard" "the mutation changed nothing"
elif ! bash -n "$MUT_SITE" 2>/dev/null; then
    bad "mutant: the call site loses its guard" "the mutant does not parse"
else
    case "$(nv_scan "$MUT_SITE")" in
        *"set=0 "*) ok "mutant: the call site loses its guard" "the scan named it" ;;
        *) bad "mutant: the call site loses its guard" "the scan saw nothing wrong: $(nv_scan "$MUT_SITE")" ;;
    esac
fi
# …and a second call site outside it must be named too.
MUT_SITE2="$WORK/mutant-site2"
cp "$ENGINE" "$MUT_SITE2"
printf '\nbootc install to-filesystem /tmp/nowhere\n' >> "$MUT_SITE2"
case "$(nv_scan "$MUT_SITE2")" in
    *OUTSIDE*) ok "mutant: a second call site is named" ;;
    *) bad "mutant: a second call site is named" "$(nv_scan "$MUT_SITE2")" ;;
esac
rm -f "$MUT_SITE" "$MUT_SITE2"

# MUTATION. Remove the one line that adds --generic-image and the loop case
# must stop skipping the firmware. One line, deleted by an exact match, so the
# mutant still parses.
MUT_NV="$WORK/mutant-nvram"
cp "$ENGINE" "$MUT_NV"
sed -i '/^    NVRAM_BOOTC_ARGS=(--generic-image)$/d' "$MUT_NV"
if cmp -s "$ENGINE" "$MUT_NV"; then
    bad "mutant: the firmware-skip flag" "the mutation changed nothing — the sed program matched no line"
elif ! bash -n "$MUT_NV" 2>/dev/null; then
    bad "mutant: the firmware-skip flag" "the mutant does not parse; the mutation ate more than its line"
else
    got=$(nvram_probe "$MUT_NV" /dev/loop9 "$FAKESYS")
    case "$got" in
        *"--generic-image"*) bad "mutant: the firmware-skip flag" "it survived its own deletion — the case proves nothing" ;;
        *)                   ok "mutant: the firmware-skip flag" "removed -> a loopback target would write NVRAM again" ;;
    esac
fi
rm -f "$MUT_NV"

echo
echo "── the recovery key on a machine not booted from a Rime USB stick ─────"
# A DVD, Ventoy, a VM's ISO: no RIMEEFI partition to save the key to. Under
# `set -u` a bare `local dst` in surface_recovery_key was unset on that path and
# killed the engine right after the key was created — no RIME-INSTALL-FAILED, an
# encrypted empty disk. Measured in a VM booting the ISO as a CD, 2026-10-04.
# The shipped function runs here, with blkid stubbed to find nothing.
RK="$WORK/recovery-fn.sh"
sed -n '/^surface_recovery_key()/,/^}/p' "$ENGINE" > "$RK"
rk_probe() {  # $1 = file holding the function
    bash -c '
        set -uo pipefail
        LOG=/dev/null
        log() { :; }; note() { printf "%s\n" "$*"; }; secret_line() { printf "%s\n" "$*"; }
        blkid() { return 2; }
        unset RIME_RECOVERY_DIR
        . "$1"
        surface_recovery_key test-key-0000 && echo "RETURNED-0"
    ' _ "$1" 2>&1
}
if grep -q '^surface_recovery_key()' "$RK"; then
    out=$(rk_probe "$RK")
    if [[ "$out" == *"RIME-INSTALL-RECOVERY-UNSAVED: this installer is not running from a Rime USB stick"* ]] \
       && [[ "$out" == *"RETURNED-0"* ]]; then
        ok "no Rime stick: the key is shown, reported unsaved, and the install goes on"
    else
        bad "no Rime stick: the key is shown, reported unsaved, and the install goes on" \
            "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
    fi
    # MUTATION: the declaration as it was.
    sed 's/^  local key="\$1" esp="" dst="" path="" saved=0 why=""$/  local key="$1" esp dst path saved=0 why=""/' "$RK" > "$RK.mut"
    if cmp -s "$RK" "$RK.mut"; then
        bad "mutant: the bare local dst" "the sed program matched no line"
    else
        out=$(rk_probe "$RK.mut")
        if [[ "$out" == *"unbound variable"* ]] && [[ "$out" != *"RETURNED-0"* ]]; then
            ok "mutant: the bare local dst" "dies on 'dst: unbound variable', as the shipped engine did"
        else
            bad "mutant: the bare local dst" "the old declaration did not fail here: $(printf '%s' "$out" | tail -1)"
        fi
    fi
else
    bad "the recovery-key function could be read out of the engine" "no surface_recovery_key()"
fi

# ── the keymap half, run where the data it reads actually exists ───────────
# These assertions need Fedora's keymap tree (/usr/lib/kbd/keymaps), systemd's
# kbd-model-map and xkeyboard-config's rules/base.lst. A GitHub ubuntu-24.04
# runner has none of them in that shape — Debian puts keymaps elsewhere and
# names them .kmap.gz — and while this block lived inline it SKIPped there. A
# skip is not a weaker pass: it meant the required deliverable of this round
# was measured on one developer's laptop and nowhere else, while the CI step
# still reported success. That is the gate-that-inspects-nothing shape this
# repository keeps finding.
#
# So: run installer/keymap-checks.sh here when this machine can answer it,
# otherwise run the identical script inside a Fedora container, and FAIL if
# neither route is available. It never skips.
KM_OUT="$WORK/keymap-out.txt"

# km_runtime — sets KM_RT to a container runtime that actually STARTS here.
#
# ROOT podman first, and not as a formality. On a GitHub runner there is no
# systemd user session, so rootless podman tries to create its run directory
# under /run/user/$UID and dies:
#
#   cannot open run directory '/run/user/1001/crun': Permission denied
#   Error: OCI permission denied
#
# The container never started, keymap-checks.sh never printed its result line,
# and the caller's "did they report anything" guard fired as
# `FAIL the keymap checks reported a result` — the third of this suite's three
# permanent CI reds. Root podman uses /run/podman and needs no such directory,
# and this suite has ALREADY proved `sudo -n podman` works in this process: it
# is how the engine image above was made.
#
# Each candidate is PROBED with `info`, not merely found on PATH. `command -v`
# answering is what made the old chooser pick a podman that could not run a
# container, and a runtime that cannot start is indistinguishable from one that
# is absent as far as this measurement is concerned.
KM_RT=()
km_runtime() {
    if command -v podman >/dev/null 2>&1 && sudo -n podman info >/dev/null 2>&1; then
        KM_RT=(sudo -n podman); return 0
    fi
    if command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
        KM_RT=(podman); return 0
    fi
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        KM_RT=(docker); return 0
    fi
    return 1
}

if [ -z "${RIME_KEYMAP_FORCE_CONTAINER:-}" ] \
   && [ -f /usr/share/systemd/kbd-model-map ] && [ -d /usr/lib/kbd/keymaps ] \
   && [ -r /usr/share/X11/xkb/rules/base.lst ]; then
    bash ./keymap-checks.sh "$ENGINE" "$WORK" 2>&1 | tee "$KM_OUT"
elif km_runtime; then
    echo "note: no Fedora keymap data on this machine — measuring inside a container (${KM_RT[*]})"
    # Fully qualified on purpose: a bare `fedora:43` makes podman ask which
    # registry it meant and makes the step fail for a reason that has nothing
    # to do with keymaps.
    "${KM_RT[@]}" run --rm -v "$PWD":/w:ro,z -w /w "${RIME_KEYMAP_IMAGE:-quay.io/fedora/fedora:43}" bash -c '
        dnf -y install --setopt=install_weak_deps=False kbd systemd xkeyboard-config >/dev/null 2>&1 || {
            echo "FAIL  the container could not install kbd, systemd and xkeyboard-config"; exit 1; }
        bash ./keymap-checks.sh ./rime-install /tmp/kmwork' 2>&1 | tee "$KM_OUT"
else
    echo "FAIL  the keymap conversion could not be measured here: no Fedora keymap data and no container runtime"
    printf 'KEYMAP-CHECKS: 0 1\n' > "$KM_OUT"
fi
# Fold the child's counts in. A MISSING result line is a failure, not a zero:
# a script that died halfway through must not read as "nothing to report".
kmline=$(grep -m1 '^KEYMAP-CHECKS: ' "$KM_OUT" 2>/dev/null || true)
if [ -z "$kmline" ]; then
    bad "the keymap checks reported a result" "no KEYMAP-CHECKS line — they did not finish"
else
    read -r _kmtag kp kf <<<"$kmline"
    : "$_kmtag"
    pass=$((pass + kp)); fail=$((fail + kf))
    # And a floor, so a future edit that quietly guts keymap-checks.sh cannot
    # turn 18 assertions into 0 and still report a clean run.
    if [ "$kp" -lt 15 ]; then
        bad "the keymap checks asserted their full set" "only $kp assertions ran"
    fi
fi

echo
echo "── --check-passphrase: the encrypt-page check, run standalone ─────────"
# The GUI calls this from the encrypt page's Continue handler, BEFORE the
# confirm step, so a Croatian owner whose passphrase has an `x` in it finds
# out while they can still pick a different one — not on the PROGRESS page,
# after the point of no return. It needs no image, no answers file, no disk:
# it is dispatched in the engine's own argument case, before any of that is
# read. `sudo -n true` is asserted directly rather than trusted, since this
# section has no ENGINE_RUNNABLE guard to fall back on.
if sudo -n true 2>/dev/null && [ -x "$ENGINE" ]; then
    # cp_check <engine> <passphrase> <layout> <keymap-tree> — one verdict line.
    # The fixture tree is passed on every call for the reason given where it is
    # built: without it these six assertions read the tester's own kbd package
    # and were red on every CI runner this suite has ever run on.
    cp_check() {
        printf '%s' "$2" | sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_KBD_KEYMAPS="$4" RIME_KBD_MODEL_MAP="$KBD_MODELMAP" \
            "$1" --check-passphrase "$3" "" 2>&1
    }

    out=$(cp_check "$ENGINE" "rimebootproof1" us "$KBD_TREE"); rc=$?
    if [ "$out" = "typeable: yes console=us" ] && [ "$rc" = 0 ]; then
        ok "us + an all-ASCII passphrase: typeable: yes"
    else bad "us + an all-ASCII passphrase: typeable: yes" "rc=$rc out='$out'"; fi

    # THE CASE THIS MODE EXISTS FOR. A keymap with no digit `1` anywhere in its
    # table must come back `no`, naming the character — not a count, not a
    # shrug. `vn` is that keymap on a real Fedora tree, which
    # installer/keymap-checks.sh asserts against the real file; the fixture
    # here reproduces the SHAPE so that the engine's wiring — dispatch ->
    # console_keymap_for -> keymap_can_type -> this exact output format — is
    # measured identically on a machine with no Fedora kbd at all.
    out=$(cp_check "$ENGINE" "rime1zed" vn "$KBD_TREE"); rc=$?
    if [ "$out" = "typeable: no console=vn chars=1" ] && [ "$rc" = 0 ]; then
        ok "vn + a passphrase containing '1': typeable: no, names the character"
    else bad "vn + a passphrase containing '1': typeable: no, names the character" "rc=$rc out='$out'"; fi

    # CONTROL, and the CI red reproduced: with an EMPTY tree the layout cannot
    # resolve, the engine falls back to `us`, and the verdict is the useless
    # `typeable: yes console=us` this suite used to report as a pass. If this
    # and the case above are ever both green, the fixture is being ignored.
    out=$(cp_check "$ENGINE" "rime1zed" vn "$KBD_NONE"); rc=$?
    if [ "$out" = "typeable: yes console=us" ] && [ "$rc" = 0 ]; then
        ok "…and with an empty keymap tree it falls back to us" "so the case above read the fixture"
    else bad "…and with an empty keymap tree it falls back to us" "rc=$rc out='$out'"; fi

    # THE COUNTER-HALF. A verdict of `no` proves nothing if the check says `no`
    # to everything. bg reaches its keymap only THROUGH the conversion table's
    # `bg,us` row, so this one assertion covers both halves at once: the
    # multi-layout lookup, and a passphrase the resulting keymap can type.
    out=$(cp_check "$ENGINE" "correct horse 9" bg "$KBD_TREE"); rc=$?
    if [ "$out" = "typeable: yes console=bg_bds-utf8" ] && [ "$rc" = 0 ]; then
        ok "bg + a passphrase it can type: typeable: yes, on the converted name"
    else bad "bg + a passphrase it can type: typeable: yes, on the converted name" "rc=$rc out='$out'"; fi

    # MUTATION: make keymap_can_type() unable to ever report a missing
    # character. The vn case above must then go green-for-the-wrong-reason,
    # which is what this arm refuses to let happen silently.
    KCMUT="$WORK/rime-install.can-type-mutant"
    sed 's/\[ -n "$missing" \]/[ -z "$missing" ]/' "$ENGINE" > "$KCMUT"
    chmod 755 "$KCMUT"
    if cmp -s "$ENGINE" "$KCMUT"; then
        bad "mutant: keymap_can_type can no longer say no" \
            "the sed program matched no line — the mutant is the engine unchanged"
    else
        mout=$(cp_check "$KCMUT" "rime1zed" vn "$KBD_TREE")
        if [ "$mout" = "typeable: no console=vn chars=1" ]; then
            bad "mutant: keymap_can_type can no longer say no" \
                "it still reported the missing character with the branch removed"
        else
            ok "mutant: keymap_can_type can no longer say no" "verdict removed -> '$mout'"
        fi
    fi
    rm -f "$KCMUT"

    # No layout at all must not crash the GUI's call — it is asked before the
    # user has necessarily reached the keyboard page in every flow.
    out=$(printf '%s' "hello" | sudo -n "$ENGINE" --check-passphrase "" "" 2>&1); rc=$?
    if [ "$out" = "typeable: unknown reason=no-layout" ] && [ "$rc" = 0 ]; then
        ok "no layout: typeable: unknown, not a crash"
    else bad "no layout: typeable: unknown, not a crash" "rc=$rc out='$out'"; fi

    # THE PROPERTY THAT MATTERS MOST: this is advice for a form field, never a
    # gate. A verdict of "no" must still exit 0 — a future edit that turns this
    # into `exit 1` on "no" would make the GUI's subprocess call read a
    # typeable-but-inconvenient passphrase as "the engine crashed" and block an
    # install this same passphrase would succeed at today.
    printf '%s' "rime1zed" \
        | sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_KBD_KEYMAPS="$KBD_TREE" RIME_KBD_MODEL_MAP="$KBD_MODELMAP" \
            "$ENGINE" --check-passphrase vn "" >/dev/null 2>&1
    _cprc=$?
    if [ "$_cprc" = 0 ]; then ok "a 'no' verdict still exits 0 — advisory, never a gate"
    else bad "a 'no' verdict still exits 0 — advisory, never a gate" "exit $_cprc"; fi

    # MUTATION: remove the case arm entirely and confirm the same call falls
    # through to the engine's ordinary refusal instead of silently doing
    # nothing — a copy of the engine, the original is never touched.
    CPMUT="$WORK/rime-install.check-passphrase-mutant"
    sed '/^  --check-passphrase)$/,/^    ;;$/d' "$ENGINE" > "$CPMUT" 2>/dev/null
    chmod 755 "$CPMUT" 2>/dev/null
    # A substring match on --check-passphrase would also match the header
    # comment and the die() message naming it — both mention the flag by name
    # and neither is what this mutation removes. The CASE LABEL is the thing
    # under test.
    if grep -q -- '^  --check-passphrase)$' "$CPMUT"; then
        bad "mutant: the case arm removed" "the sed program matched no line — the mutant is identical to the engine"
    else
        mutout=$(printf '%s' "rimebootproof1" | sudo -n "$CPMUT" --check-passphrase us "" 2>&1)
        if [[ "$mutout" == *"unknown argument"* ]]; then
            ok "mutant: the case arm removed" "falls through to the ordinary refusal, as it must"
        else
            bad "mutant: the case arm removed" "got '$mutout' — something still answers --check-passphrase with the arm gone"
        fi
    fi
else
    echo "SKIP  --check-passphrase cases (need passwordless sudo and the engine present)"
fi

echo
echo "──────────────────────────────────────────────────────────────────────"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
[ "$pass" -gt 0 ] || { echo "FATAL: nothing was asserted"; exit 1; }
exit 0
