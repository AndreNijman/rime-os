#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-windows-installer.sh — the Windows binary builds, it RUNS, its rules
#  are checked, and on a machine with KVM it is pointed at a real Windows.
#
#  ═══ WHY THIS EXISTS ═══
#
#  `windows-installer/` is Windows-targeted code living in a repository that is
#  developed, tested and built entirely on Linux. Its first round shipped 655
#  lines of it and recorded honestly that "A Windows `.exe` has not been built
#  or tested". Code nobody compiles rots, and rots silently — the unit tests
#  all passed, because they were compiled for Linux.
#
#  ═══ FOUR STAGES, AND WHAT EACH ONE CAN AND CANNOT PROVE ═══
#
#   1. it cross-builds, and the artefact is a real PE32+ x86-64 binary.
#   2. the decision rules — which partition may be erased, and the words shown
#      before erasing it — pass their unit tests. These run on Linux on
#      purpose: they are the rules that must never be wrong, and a rule that
#      only executes on hardware nobody in this program owns is a rule nobody
#      has ever checked.
#   3. the binary EXECUTES under Windows semantics and prints its own words.
#      Wine is not a Windows machine and this does not pretend otherwise: raw
#      disk access, volume locking and UEFI variables do not work under it.
#      What it proves is that the binary is well-formed and reaches the user.
#   4. THE WINDOWS GUEST. `windows-installer/lab/` builds a real Windows
#      Server 2022 and points the installer at real GPT disks. This is the
#      only stage that can prove the Windows storage layer at all, and it
#      needs /dev/kvm and a golden image that is a build artefact rather than
#      a repository file.
#
#  ═══ THREE VERDICTS, NOT TWO ═══
#
#  A check that could not be PERFORMED is neither a pass nor a failure, and
#  saying so is the whole reason stage 4 does not quietly vanish on a machine
#  without KVM. `could-not-run` always names what was missing. The rule and the
#  vocabulary are `tests/vmlab/run-vmlab`'s.
#
#  Run from anywhere: ./tests/test-windows-installer.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

BUILDER=windows-installer/build-windows.sh
WINLAB=windows-installer/lab/winlab
[ -x "$BUILDER" ] || { echo "cannot find $BUILDER"; exit 2; }

if ! command -v podman >/dev/null 2>&1; then
    echo "SKIP  podman is absent; the Windows cross-build needs a container"
    exit 0
fi

IMAGE="${RIME_WIN_BUILD_IMAGE:-registry.fedoraproject.org/fedora:43}"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

pass=0; fail=0; cannot=0
ok()     { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()    { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }
nogo()   { printf 'COULD-NOT-RUN  %s\n' "$1"; cannot=$((cannot+1)); }

# ── 0. every write lives in ONE file, and only the writes it declares ───────
#  "What can this program change?" must be answerable by reading one file.
#  Part one: the Win32 write APIs may appear in src/winwrite.rs and nowhere
#  else. Part two exists because names are not enough: `DeviceIoControl`
#  takes an arbitrary u32, and IOCTL_DISK_SET_DRIVE_LAYOUT_EX is 0x0007C054,
#  a number no name-based grep will ever see. So every IOCTL/FSCTL constant
#  in the source must be on an ALLOWLIST (the five read-side codes, plus
#  IOCTL_DISK_UPDATE_PROPERTIES in winwrite.rs only), and none may be passed
#  as a bare literal. Part three does the same for firmware variables:
#  SetFirmwareEnvironmentVariable takes any name, so the names it is given
#  must be BootOrder, BootNext or a Boot#### made by option_name(), and
#  nothing else. Each part fails both ways: a new code, name or file, or one
#  that stops being declared.
W=windows-installer/src
writers=0
for api in GENERIC_WRITE GENERIC_ALL FILE_WRITE_DATA WriteFile \
           SetFirmwareEnvironmentVariable SetEndOfFile FlushFileBuffers \
           DeleteFile MoveFile SetFileAttributes ExitWindowsEx; do
    hits="$(grep -rln --include='*.rs' "\\b$api" "$W" | grep -v "^$W/winwrite.rs$" || true)"
    if [ -n "$hits" ]; then
        bad "$api appears outside src/winwrite.rs: $hits"
        writers=$((writers+1))
    fi
done
[ "$writers" = 0 ] && ok "every disk, firmware and restart write API is confined to src/winwrite.rs"
if grep -q 'SetFirmwareEnvironmentVariableExW' "$W/winwrite.rs" && grep -q 'fn permitted' "$W/winwrite.rs"; then
    ok "winwrite.rs still declares the firmware write and the disk writer's range check"
else
    bad "winwrite.rs lost the firmware write or the range check the gate reasons about"
fi

#  CTL_CODE(DeviceType, Function, Method, Access):
#    0x00070000  IOCTL_DISK_GET_DRIVE_GEOMETRY
#    0x00070050  IOCTL_DISK_GET_DRIVE_LAYOUT_EX
#    0x0007405c  IOCTL_DISK_GET_LENGTH_INFO
#    0x002d1400  IOCTL_STORAGE_QUERY_PROPERTY
#    0x00560000  IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS
#    0x00070140  IOCTL_DISK_UPDATE_PROPERTIES   (winwrite.rs only)
declared="$(grep -rhoE 'const (IOCTL|FSCTL)_[A-Z_0-9]+: u32 = 0x[0-9A-Fa-f_]+' "$W" |
            sed 's/.*= //' | tr -d '_' | tr '[:upper:]' '[:lower:]' | sort -u)"
allowed="$(printf '0x00070000\n0x00070050\n0x0007405c\n0x002d1400\n0x00560000\n0x00070140\n' | sort)"
unexpected="$(comm -23 <(printf '%s\n' "$declared") <(printf '%s\n' "$allowed"))"
missing="$(comm -13 <(printf '%s\n' "$declared") <(printf '%s\n' "$allowed"))"
if [ -n "$unexpected" ]; then
    bad "an IOCTL/FSCTL code outside the allowlist is declared: $(echo $unexpected)"
elif [ -n "$missing" ]; then
    bad "an allowlisted IOCTL is no longer declared ($(echo $missing)); update the allowlist deliberately"
else
    ok "every declared IOCTL code ($(printf '%s\n' "$declared" | wc -l)) is on the allowlist"
fi
if grep -rlE 'IOCTL_DISK_UPDATE_PROPERTIES' "$W" | pipe_has -v "^$W/winwrite.rs$"; then
    bad "IOCTL_DISK_UPDATE_PROPERTIES appears outside winwrite.rs"
else
    ok "the one write-side IOCTL is confined to winwrite.rs"
fi
if grep -rqnE '\.ioctl\(\s*0x|DeviceIoControl\([^,]+,\s*0x' "$W"; then
    bad "an IOCTL code is passed as a bare numeric literal, bypassing the allowlist"
else
    ok "no IOCTL code is passed as a bare numeric literal"
fi
vars="$(grep -oE 'pub const VAR_[A-Z_]+: &str = "[A-Za-z]+"' "$W/winwrite.rs" | sed 's/.*= "//; s/"//' | LC_ALL=C sort | tr '\n' ' ')"
if [ "$vars" = "BootCurrent BootNext BootOrder " ]; then
    ok "the firmware variable names winwrite.rs declares are exactly BootOrder, BootNext, BootCurrent"
else
    bad "firmware variable names changed: '$vars'"
fi
calls="$(grep -E 'set_var\(' "$W/winwrite.rs" | grep -v 'fn set_var' | grep -oE 'set_var\([^,]+' | sed 's/set_var(//' | LC_ALL=C sort -u | tr '\n' ' ')"
if [ "$calls" = "&crate::bootentry::option_name(n) VAR_BOOT_NEXT VAR_BOOT_ORDER " ]; then
    ok "firmware writes go only to BootOrder, BootNext and Boot#### from option_name()"
else
    bad "a firmware write names something else: '$calls'"
fi
if grep -rqnE '"(PK|KEK|db|dbx|SetupMode|SecureBoot|OsIndications|MokList[A-Za-z]*|Driver[0-9A-F]{4}|SysPrep[0-9A-F]{4})"' "$W"; then
    bad "the source names a firmware variable outside the boot-entry allowlist"
else
    ok "no Secure Boot, OS-indication or vendor variable is named anywhere"
fi

if [ "${RIME_WIN_GATE_ONLY:-}" = 1 ]; then
    printf '\nrime-windows-installer gate: %d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ]; exit
fi

# ── 1. it cross-builds ───────────────────────────────────────────────────────
build_log="$OUT/build.log"
if "$BUILDER" "$OUT" >"$build_log" 2>&1; then
    ok "the Windows cross-build completed"
else
    rc=$?
    if [ "$rc" = 77 ]; then
        echo "SKIP  $(tail -1 "$build_log")"
        exit 0
    fi
    bad "the Windows cross-build failed (rc $rc)"
    tail -15 "$build_log" | sed 's/^/        /'
    printf '\nrime-windows-installer: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

exe="$OUT/rime-windows-installer.exe"
if [ -s "$exe" ]; then
    ok "a binary was produced"
else
    bad "no binary at $exe"
fi

# ── 2. it is the right KIND of binary ────────────────────────────────────────
# An ELF here would mean the host target was used and every Windows-only path
# in the source was skipped by cfg() — the build would still be green.
kind="$(file -b "$exe" 2>/dev/null)"
case "$kind" in
    PE32+*x86-64*) ok "it is a PE32+ x86-64 Windows binary" ;;
    *)             bad "not a 64-bit Windows PE: $kind" ;;
esac

# ── 3. the decision rules and the image laboratory ───────────────────────────
#  Both in one container, because the image lab drives the COMPILED binary
#  against GPT fixtures and so needs the same toolchain. Nothing in CI ran the
#  image lab before this; it was a file you had to know to type.
unit_log="$OUT/unit.log"
if podman run --rm -v "$PWD/windows-installer":/src:ro,z "$IMAGE" bash -euo pipefail -c '
        # e2fsprogs is not incidental: one image-lab case makes a real
        # ext4 filesystem in a scratch partition image and requires the
        # validator to refuse it. Without mkfs.ext4 that case SKIPS, and a
        # skipped case is the one that would have caught a signature-based
        # shortcut creeping into the content scan.
        dnf install -y -q --setopt=install_weak_deps=False rust cargo python3 e2fsprogs \
            util-linux gdisk dosfstools 7zip >/dev/null
        cp -r /src /build && cd /build
        cargo test --offline --locked 2>&1
        cargo build --offline --locked 2>&1
        echo "RIME_IMAGE_LAB_BEGIN"
        python3 tests/image_lab.py 2>&1
    ' >"$unit_log" 2>&1; then
    n="$(grep -c '^test .* ok$' "$unit_log")"
    ok "the partition-eligibility and confirmation rules pass ($n unit tests)"
    lab_n="$(sed -n '/RIME_IMAGE_LAB_BEGIN/,$p' "$unit_log" | grep -oE 'Ran [0-9]+ tests' | grep -oE '[0-9]+')"
    lab_tail="$(sed -n '/RIME_IMAGE_LAB_BEGIN/,$p' "$unit_log" | grep -E '^OK( |$)|^FAILED')"
    if [ "$lab_tail" = OK ]; then
        ok "the GPT image laboratory passes (${lab_n:-?} cases, against the compiled binary)"
    elif [ -n "$lab_tail" ] && [ "${lab_tail#OK}" != "$lab_tail" ]; then
        # unittest writes "OK (skipped=1)". A skipped case is not a pass, and
        # the one that skips is the one that builds a real ext4 filesystem --
        # exactly the case that would catch a signature-based shortcut in the
        # content scan. Say so rather than counting it green.
        bad "the GPT image laboratory did not run every case: $lab_tail"
    else
        bad "the GPT image laboratory failed"
        sed -n '/RIME_IMAGE_LAB_BEGIN/,$p' "$unit_log" | tail -20 | sed 's/^/        /'
    fi
else
    bad "the rule unit tests or the image laboratory failed"
    tail -20 "$unit_log" | sed 's/^/        /'
fi
# The negative that matters most. The confirmation screen must never identify a
# target by device index, and the test that enforces it must actually exist.
if grep -q 'the_confirmation_names_the_disk_by_serial_and_never_by_index' "$unit_log"; then
    ok "the 'never identify a disk by index' rule is among the tests that ran"
else
    bad "the index-identification test did not run; the rule is unguarded"
fi

# ── 4. it RUNS, and the words are its own ────────────────────────────────────
# Wine's own failures (missing loader, bad image) also exit non-zero, so the
# exit code is not the assertion — the program's message is.
run_out="$OUT/run.txt"
podman run --rm -v "$OUT":/exe:ro,z "$IMAGE" bash -c '
    dnf install -y -q --setopt=install_weak_deps=False wine >/dev/null 2>&1 \
        || { echo "RIME_WINE_UNAVAILABLE"; exit 0; }
    export WINEDEBUG=-all WINEPREFIX=/tmp/wineprefix
    # With no arguments the program opens its window, which a headless wine
    # cannot show; an unknown command is the scriptable way to reach the
    # same entry point and its words.
    wine /exe/rime-windows-installer.exe no-such-command >/tmp/out 2>/tmp/err
    echo "RIME_RC=$?"
    echo "RIME_STDOUT_BEGIN"; cat /tmp/out
    echo "RIME_STDERR_BEGIN"; cat /tmp/err
    echo "RIME_SURVEY_BEGIN"
    wine /exe/rime-windows-installer.exe survey 2>&1
    echo "RIME_SURVEY_RC=$?"
' >"$run_out" 2>&1

if grep -q RIME_WINE_UNAVAILABLE "$run_out"; then
    nogo "the binary was not executed: wine is unavailable in $IMAGE"
else
    rc_line="$(grep -m1 '^RIME_RC=' "$run_out" | cut -d= -f2)"
    if [ "$rc_line" = 1 ]; then
        ok "an unknown command exits 1"
    else
        bad "expected exit 1 for an unknown command, got '${rc_line:-none}'"
    fi

    # The discriminator: our text, not wine's.
    if grep -q 'rime-windows-installer lab FILE.img' "$run_out"; then
        ok "the usage line the program itself prints reached the user"
    else
        bad "the program's own usage text never appeared — it may not have run at all"
        sed -n '1,12p' "$run_out" | sed 's/^/        /'
    fi

    # It must keep saying which commands change anything.
    if grep -q 'Nothing is written to a disk or a firmware variable except by install' "$run_out"; then
        ok "it states which commands write, and that install waits for confirmation"
    else
        bad "the statement of what writes is gone from the binary's output"
    fi

    # The Windows-only code path is now compiled in, so `survey` reaches real
    # Win32 calls. Under wine there are no physical drives; what matters is
    # that it says so rather than crashing, because that is the same code path
    # a user with an unreadable disk takes.
    if grep -qE 'survey-complete|REFUSED' "$run_out"; then
        ok "the Windows survey path runs to a stated conclusion under wine"
    else
        bad "survey produced neither a conclusion nor a refusal"
        sed -n '/RIME_SURVEY_BEGIN/,$p' "$run_out" | head -12 | sed 's/^/        /'
    fi
fi

# ── 5. the Windows guest ─────────────────────────────────────────────────────
#  The only stage that can prove the storage layer. It needs KVM and a golden
#  image, and neither is a repository file: `windows-installer/lab/winlab
#  golden` builds one in about three minutes from Microsoft's evaluation media.
#  CI runners have no KVM, so this reports could-not-run there — never a pass.
WINLAB_DIR="${RIME_WINLAB_DIR:-/var/lab-scratch/winlab}"
if [ ! -x "$WINLAB" ]; then
    nogo "the Windows guest was not exercised: $WINLAB is missing"
elif [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    nogo "the Windows guest was not exercised: /dev/kvm is not usable here"
elif [ ! -s "$WINLAB_DIR/golden.raw" ]; then
    nogo "the Windows guest was not exercised: no golden image at $WINLAB_DIR/golden.raw (build one with '$WINLAB golden')"
elif [ "${RIME_WINLAB_GUEST:-}" != 1 ]; then
    nogo "the Windows guest was not exercised: it takes several minutes, so set RIME_WINLAB_GUEST=1 to ask for it"
else
    # Both enumeration orders. The second is not a repetition: --swap moves
    # fixture A to a different AHCI port, so Windows gives it a different disk
    # number, and the claim under test is that nothing the tool prints moves
    # with it.
    guest_log="$OUT/guest.log"
    swap_log="$OUT/guest-swap.log"
    if "$WINLAB" run windows-installer/lab/jobs/survey >"$guest_log" 2>&1; then
        ok "the Windows guest ran the survey job"
    else
        bad "the Windows guest run failed"
        tail -20 "$guest_log" | sed 's/^/        /'
    fi
    if "$WINLAB" run windows-installer/lab/jobs/survey --swap >"$swap_log" 2>&1; then
        ok "the Windows guest ran it again with the disks on swapped ports"
    else
        bad "the swapped-order guest run failed"
        tail -20 "$swap_log" | sed 's/^/        /'
    fi

    assert_guest() {   # $1 description   $2 grep -E pattern
        if grep -qE "$2" "$guest_log"; then ok "guest: $1"; else
            bad "guest: $1 (no line matching $2)"; fi
    }
    assert_guest "the survey reached its own conclusion" 'survey-complete'
    assert_guest "the on-disk GPT and Windows' table agreed on every disk" \
                 'partition table AGREE'
    if grep -q 'DISAGREE' "$guest_log"; then
        bad "guest: at least one disk's two partition-table readings disagreed"
    else
        ok "guest: no disk had disagreeing partition-table readings"
    fi
    # The ESP and C: are refused as IN USE; the Microsoft reserved partition is
    # the one that reaches the protected-type rule, because Windows builds no
    # volume over it. Asserting the right reason against the right partition
    # matters: a description that names the wrong one passes by accident and
    # then documents something false.
    assert_guest "the Microsoft reserved partition was refused as a protected type" \
                 'REFUSED \(protected partition type\): this is a Microsoft reserved'
    assert_guest "the running Windows system partition was refused, and C: named" \
                 'REFUSED \(in use by Windows\).*mounted at C:'
    assert_guest "the NTFS fixture partition was refused, and its letter named" \
                 'REFUSED \(in use by Windows\).*NTFS, label "WINDATA"'
    # The zeroed basic-data fixture is refused for a STRONGER reason than its
    # type: Windows gives a RAW basic-data partition a drive letter, so it is
    # already in use by the time the tool looks. That is the design's own claim
    # about basic-data partitions, demonstrated rather than argued. The type
    # rule itself is covered by the unit test
    # every_windows_owned_type_is_refused_even_when_empty_and_large.
    assert_guest "an all-zero basic-data partition was refused, Windows having lettered it" \
                 'REFUSED \(in use by Windows\).*unrecognised filesystem'
    assert_guest "an eligible partition was read to the last byte and found zero" \
                 'ALL-ZERO CONTENT'
    assert_guest "the exclusivity check was re-asked immediately before reading" \
                 'exclusivity no volume object covers this partition, re-checked'
    assert_guest "the confirmation named the disk by its serial number" 'FIXA00000001'
    assert_guest "the firmware variables were unchanged by the run" \
                 'IDENTICAL'

    # ── the enumeration-order claim, made properly ───────────────────────────
    WINLAB_DIR="${RIME_WINLAB_DIR:-/var/lab-scratch/winlab}"
    a="$WINLAB_DIR/guest-normal.txt"
    b="$WINLAB_DIR/guest-swapped.txt"
    if [ -s "$a" ] && [ -s "$b" ]; then
        # Windows' own disk number for the fixture must actually have moved,
        # or the comparison below is vacuous.
        na="$(grep -E '^ *[0-9]+ +RIME-FIXTURE-A' "$a" | awk '{print $1}')"
        nb="$(grep -E '^ *[0-9]+ +RIME-FIXTURE-A' "$b" | awk '{print $1}')"
        if [ -n "$na" ] && [ -n "$nb" ] && [ "$na" != "$nb" ]; then
            ok "guest: Windows numbered RIME-FIXTURE-A as disk $na and then as disk $nb"
        else
            bad "guest: the swapped run did not change Windows' disk number (${na:-?} vs ${nb:-?}); the comparison below would prove nothing"
        fi
        # And the words the user commits against must be byte-identical.
        sed -n '/ERASE AND INSTALL/,/Disk numbers change/p' "$a" | tr -d '\r' >"$OUT/conf-a.txt"
        sed -n '/ERASE AND INSTALL/,/Disk numbers change/p' "$b" | tr -d '\r' >"$OUT/conf-b.txt"
        if [ -s "$OUT/conf-a.txt" ] && diff -q "$OUT/conf-a.txt" "$OUT/conf-b.txt" >/dev/null; then
            ok "guest: the confirmation text is identical across both enumeration orders"
        else
            bad "guest: the confirmation text changed when the disk number changed"
            diff -u "$OUT/conf-a.txt" "$OUT/conf-b.txt" | head -20 | sed 's/^/        /'
        fi
    else
        bad "guest: one of the two run transcripts is missing ($a, $b)"
    fi

    if grep -q 'ERASE AND INSTALL' "$guest_log" &&
       sed -n '/ERASE AND INSTALL/,/Disk numbers change/p' "$guest_log" |
           pipe_has 'PhysicalDrive'; then
        bad "guest: the confirmation text named a device index"
    else
        ok "guest: the confirmation text contains no device index"
    fi
fi

echo
printf 'rime-windows-installer: %d passed, %d failed, %d could-not-run\n' \
    "$pass" "$fail" "$cannot"
[ "$fail" -eq 0 ]
