#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-greet-a11y.sh — the login screen, asked what a screen reader and a
#  keyboard-only user actually get (roadmap P2-003 "accessible login/lock/
#  recovery", and the greeter half of P2-004's "keyboard layout before
#  password").
#
#  ── Why a running surface and not a grep ────────────────────────────────────
#
#  Every other question about the greeter in this directory is answered by
#  extracting a script out of the QML and running it. Accessibility cannot be
#  answered that way: `Accessible.name` can be present and bound to an empty
#  string, present on a wrapper no reader reaches, or present on four controls
#  out of six, and a grep says "yes" to all three. Likewise a Tab chain —
#  `KeyNavigation.tab: passwordInput` reads like a complete chain in the source
#  and is in fact a two-element ring that cannot reach the session picker.
#
#  So this builds the SHIPPED files/desktop/rime-greet/GreetSurface.qml under
#  qmltestrunner, reads `Accessible.name` and `Accessible.role` off the live
#  objects — the same attached objects QAccessible publishes to AT-SPI — and
#  posts real Qt.Key_Tab events through the window's delivery agent.
#
#  ── Headless, and not negotiably ────────────────────────────────────────────
#
#  `-platform offscreen`. Nothing here opens a window, starts a compositor,
#  talks to greetd or authenticates anything. WAYLAND_DISPLAY is unset out of
#  the environment before the runner is invoked, so a stray Qt default cannot
#  put the login screen on the display of whoever is sitting at this machine.
#
#  ── What is real and what is a fake ─────────────────────────────────────────
#
#  Real: GreetSurface.qml, copied in byte for byte and checked for that.
#  Fake: the palette (`theme`) and the auth backend (`ctx`). No assertion here
#  reads a colour, and the fake ctx exists because the real GreetContext.qml
#  needs Quickshell.Services.Greetd, a greetd socket and a Process — none of
#  which a test may have. The fake is kept to exactly the members the surface
#  reads; a stub that grows spare members is one nobody can check.
#
#  Run from anywhere: ./tests/test-rime-greet-a11y.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# Deliberately +e, like every other suite in this directory: CI invokes a suite
# as `bash -e {0}`, and under -e an assignment from a failing command ends the
# run silently, mid-section.
set +e

cd "$(dirname "$0")" || exit 2
ROOT="$(cd .. && pwd)"

SURFACE="$ROOT/files/desktop/rime-greet/GreetSurface.qml"
FIXTURE="$ROOT/tests/greet-a11y-test.qml"
for f in "$SURFACE" "$FIXTURE"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done

pass=0; fail=0; skip=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
finish() {
    printf '\nrime-greet-a11y: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

# Fedora suffixes it, Arch does not and keeps it off PATH. Both spellings, or
# this skips on whichever distribution happens to be running it.
runner=""
for c in qmltestrunner-qt6 qmltestrunner \
         /usr/lib64/qt6/bin/qmltestrunner /usr/lib/qt6/bin/qmltestrunner; do
    if command -v "$c" >/dev/null 2>&1; then runner="$c"; break; fi
done
if [ -z "$runner" ]; then
    echo "SKIP: qmltestrunner not installed (qt6-qtdeclarative-devel)"
    exit 0
fi

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/rime-greet-a11y.XXXXXX")" || exit 2
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT INT TERM

cp "$SURFACE" "$STAGE/GreetSurface.qml"           || exit 2
cp "$FIXTURE" "$STAGE/tst_greet_a11y.qml"          || exit 2

section "the staged surface is the shipped one"
# A staging step that silently truncated, or copied an older file, would make
# every assertion below a statement about something this repository does not
# ship. Checked, never assumed.
# sha256sum rather than cmp: coreutils is everywhere, diffutils is not — a
# minimal Fedora container has no `cmp`, and this suite is run inside one by
# pr-validation.yml because the Ubuntu runner's Qt is too old for the greeter's
# QtQuick.Effects import. A missing tool must not read as a staging mismatch.
sum_of() { sha256sum < "$1" | cut -d' ' -f1; }
if [ "$(sum_of "$SURFACE")" = "$(sum_of "$STAGE/GreetSurface.qml")" ]; then
    ok "GreetSurface.qml staged byte for byte"
else
    bad "GreetSurface.qml staged byte for byte" "the staged copy differs from the shipped file"
    finish; exit 1
fi

section "the accessibility and keyboard assertions"

# The display is removed from the environment rather than merely ignored. Qt's
# offscreen platform does not need it, but a plugin that ever decides to probe
# for a compositor must not find the one somebody is working in.
out="$(env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE \
        QT_QPA_PLATFORM=offscreen \
        QT_LOGGING_RULES="qt.qml.binding.removal.info=false" \
        timeout 120 "$runner" -platform offscreen -input "$STAGE/tst_greet_a11y.qml" 2>&1)"
status=$?

printf '%s\n' "$out" | grep -E "^(PASS|FAIL!|SKIP|XFAIL|QWARN|Totals)" || true

# A load failure produces zero assertions and a zero exit from some Qt
# versions, which would read as a clean pass.
if printf '%s\n' "$out" | pipe_has -E "is not a type|module .* is not installed|Cannot assign|Required property"; then
    printf '%s\n' "$out" | tail -25
    bad "the staged tree loads" "GreetSurface.qml did not build under qmltestrunner"
    finish; exit 1
fi
ok "the staged tree loads"

totals="$(printf '%s\n' "$out" | grep -m1 '^Totals:')"
if [ -z "$totals" ]; then
    printf '%s\n' "$out" | tail -25
    bad "the fixture runs to completion" "no Totals line"
    finish; exit 1
fi
ok "the fixture runs to completion"

n_pass="$(printf '%s' "$totals" | sed -E 's/.*Totals: ([0-9]+) passed.*/\1/')"
n_fail="$(printf '%s' "$totals" | sed -E 's/.*, ([0-9]+) failed.*/\1/')"

# A suite that runs but asserts nothing is the failure this floor exists to
# catch — and it is not hypothetical here, because the fixture finds its
# controls by objectName and a fixture that found none would simply be quiet.
# The count is exact rather than a floor: the QtTest total is
# initTestCase + the assertions + cleanupTestCase, so a test function that is
# dropped changes it, and so does one that is added without this line moving.
EXPECT_TESTS=23
n_ran=$(( n_pass + n_fail ))
if [ "$n_ran" -eq "$EXPECT_TESTS" ]; then
    ok "all $EXPECT_TESTS fixture test functions ran"
else
    bad "all $EXPECT_TESTS fixture test functions ran" "only $n_ran ran"
fi

if [ "$n_fail" -eq 0 ]; then
    ok "every accessibility and keyboard assertion holds ($n_pass passed)"
else
    printf '%s\n' "$out" | grep -A3 '^FAIL!' | head -60
    bad "every accessibility and keyboard assertion holds" "$n_fail failed"
fi

if [ "$status" -ne 0 ] && [ "$n_fail" -eq 0 ]; then
    bad "the runner exited cleanly" "exit $status with no failing assertion"
fi

finish
