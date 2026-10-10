#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-greet-atspi.sh — prove every assertion in test-rime-greet-atspi.sh is
#  able to fail.
#
#  A suite that has never been seen to go red is a suite nobody has checked. The
#  ledger in ROADMAP/state/agents/p2-b.md records three separate occasions in
#  this unit where an assertion passed because it could not fail -- a disabled
#  switch whose counter had never been shown to move (N8), a state file that was
#  never consulted (K4), a grep that matched the guard it was meant to detect
#  the loss of (M7). None was visible without a mutant.
#
#  Each mutant changes ONE arm and a NAMED assertion must go red.
#
#  Restores are `git checkout --`, never `cp -p` and never `mv`: it is
#  authoritative about content and gives a fresh mtime. The whole file set is
#  compared against HEAD after every mutate AND every restore -- a previous
#  round of this unit produced a page of verdicts against a working tree that
#  had been silently corrupted, and nothing noticed because the harness had no
#  integrity check.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

SURFACE="files/desktop/rime-greet/GreetSurface.qml"
FIXTURE="tests/greet-atspi-app.qml"
LIB="tests/lib/atspi.sh"
FILES="$SURFACE $FIXTURE $LIB"
SUITE="./tests/test-rime-greet-atspi.sh"

applied=0; noapply=0; caught=0; survived=0

tree_clean() {
    local d
    d="$(git diff --name-only -- $FILES 2>/dev/null)"
    [ -z "$d" ]
}

restore() {
    git checkout -- $FILES 2>/dev/null
    if ! tree_clean; then
        echo "ABORT: the tree is still dirty after restore; verdicts would be meaningless" >&2
        git diff --stat -- $FILES >&2
        exit 3
    fi
}

run_suite() { env -i HOME="$HOME" PATH="$PATH" USER="${USER:-$(id -un)}" \
                  TMPDIR="${TMPDIR:-/tmp}" "$SUITE" 2>&1; }

# mutate <id> <file> <from> <to> <assertion substring that must go red>
mutate() {
    local id="$1" file="$2" from="$3" to="$4" want="$5"

    if ! tree_clean; then
        echo "ABORT: tree dirty BEFORE $id" >&2; exit 3
    fi
    if ! grep -qF -- "$from" "$file"; then
        printf '%-5s NO-APPLY  the anchor is not in %s — this mutant proves nothing\n' "$id" "$file"
        noapply=$((noapply + 1)); return
    fi
    python3 - "$file" "$from" "$to" <<'PY'
import sys
p, a, b = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
assert s.count(a) >= 1
open(p, 'w').write(s.replace(a, b, 1))
PY
    if tree_clean; then
        printf '%-5s NO-APPLY  the edit did not change the file\n' "$id"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))

    local out; out="$(run_suite)"
    if printf '%s' "$out" | pipe_has "^FAIL  .*$want"; then
        printf '%-5s CAUGHT    %s\n' "$id" "$want"
        caught=$((caught + 1))
    else
        printf '%-5s SURVIVED  %s\n' "$id" "$want"
        printf '      ── what the suite said instead ──\n'
        printf '%s\n' "$out" | grep -E '^(FAIL|SKIP|rime-greet-atspi)' | sed 's/^/      /'
        survived=$((survived + 1))
    fi
    restore
}

echo "── baseline: the suite must be green before any mutant means anything ──"
base="$(run_suite)"
printf '%s\n' "$base" | grep -E '^rime-greet-atspi'
if ! printf '%s' "$base" | pipe_has -E '^rime-greet-atspi: [0-9]+ passed, 0 failed'; then
    echo "ABORT: the suite is not green to begin with" >&2
    printf '%s\n' "$base" | grep -E '^(FAIL|SKIP)' >&2
    exit 3
fi

echo
echo "── the mutants ──"

# A1 — the plainest one: a label that goes missing.
mutate A1 "$SURFACE" \
    'Accessible.name:        "Username"' \
    'Accessible.name:        ""' \
    "the username field reaches the bus with its label"

# A2 — the security mutant. echoMode is what masks the field; without it the
#      bridge hands the typed password to everything on the accessibility bus.
mutate A2 "$SURFACE" \
    'echoMode:            TextInput.Password' \
    'echoMode:            TextInput.Normal' \
    "the password never crosses the accessibility bus"

# A3 — passwordEdit is what suppresses the name. Dropping it is invisible to any
#      check that does not ask the bus.
mutate A3 "$SURFACE" \
    'Accessible.passwordEdit: true' \
    'Accessible.passwordEdit: false' \
    "the bridge suppresses the password field's name"

# A4 — a control a reader can see but not press. The Press action disappears
#      while every name assertion stays green.
mutate A4 "$SURFACE" \
    'Accessible.onPressAction: root.ctx.cycleSession(1)' \
    'Accessible.onPressAction: { }' \
    "pressing 'Next session' over D-Bus actually runs the greeter's handler"

# A5 — P2-004's half: the layout pill stops responding to a reader.
mutate A5 "$SURFACE" \
    'Accessible.onPressAction: layoutPill.activate()' \
    'Accessible.onPressAction: { }' \
    "a reader can change the keyboard layout from the login screen"

# A6 — a button that announces itself as static text. A blind user is told there
#      is nothing to press.
mutate A6 "$SURFACE" \
    'Accessible.role: Accessible.Button
            Accessible.name: "Next session"' \
    'Accessible.role: Accessible.StaticText
            Accessible.name: "Next session"' \
    "'Next session' is published as a button"

# A7 — the HARNESS mutant, and the most important one here: an EMPTY tree must
#      not read as a clean run. A suite whose assertions are all of the form
#      "this name is present" fails loudly on an empty tree, but one that had
#      drifted towards "no bad names found" would pass on it, and that is the
#      purest false green available in an accessibility audit.
#
#      The lever is reachability, not the a11y flag. The first version of this
#      mutant flipped org.a11y.Status.IsEnabled to false and SURVIVED, and the
#      reason recorded here was that the property reads back true however it is
#      set and that Qt does not consult it. BOTH HALVES OF THAT WERE WRONG, and
#      wrong in a way only a second machine could show: on a developer box with
#      a live desktop session the property is ALREADY true before the harness
#      starts, so it was never pushed from false, and Qt's non-consultation was
#      never tested against a flag that was off. In a bare fedora:43 container
#      both flags start false, Qt publishes nothing at all, and setting them is
#      what makes the tree appear. See the corrected note in tests/lib/atspi.sh.
#
#      That correction is also why the harness sets those flags BEFORE it
#      exports AT_SPI_BUS_ADDRESS, which is where this mutant splices in: with
#      the order the other way round, A7 breaks the HARNESS's own status read
#      and the suite goes red one assertion earlier, on a line that is about the
#      test rather than about the greeter. The mutant is unchanged; the code it
#      mutates was reordered so it still means what it says.
#
#      Taking the session bus away from the application is the real gate, and it
#      is the exact condition the shipped greeter runs in.
mutate A7 "$LIB" \
    '    export AT_SPI_BUS_ADDRESS="$ATSPI_BUS"' \
    '    export AT_SPI_BUS_ADDRESS="$ATSPI_BUS"
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$ATSPI_W/no-such-bus"' \
    "the greeter registers with the accessibility registry"

# A8 — the fixture stops typing the secret, so the masking assertion would be
#      satisfied by an empty field. The vacuity guard must catch it.
mutate A8 "$FIXTURE" \
    'f.text = win.secret' \
    'f.text = ""' \
    "the fixture really typed a secret into the password field"

echo
printf 'mutants applied=%d, failed-to-apply=%d, caught=%d, SURVIVED=%d\n' \
    "$applied" "$noapply" "$caught" "$survived"
if ! tree_clean; then
    echo "ABORT: the tree is dirty at the end of the run" >&2; exit 3
fi
echo "the tree matches HEAD"
[ "$survived" -eq 0 ] && [ "$noapply" -eq 0 ]
