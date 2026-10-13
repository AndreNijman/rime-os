#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-installer-a11y.sh — prove test-installer-a11y.sh can go red.
#
#  The suite it checks is an accessibility audit, and an audit that cannot fail
#  is worse than none: it produces a green line somebody will quote. This unit
#  has already been caught three times by assertions structurally incapable of
#  failing (N8, K4, M7 in ROADMAP/state/agents/p2-b.md), and the audit's central
#  claim -- "every focusable control announces itself" -- is precisely the shape
#  that passes over an empty tree.
#
#  Each mutant changes ONE arm and a NAMED assertion must go red.
#
#  Restores are `git checkout --`, never `cp -p` and never `mv`: authoritative
#  about content, and a fresh mtime. The file set is compared against HEAD after
#  every mutate AND every restore, because a previous round of this unit spent a
#  whole run producing verdicts against a tree that had been silently corrupted.
#
#  SLOW: every mutant starts five GUI processes on a private Xvfb. Budget a few
#  minutes each.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

GUI="installer/apex-installer-gui"
LIB="tests/lib/atspi.sh"
SUITE_F="installer/test-installer-a11y.sh"
FILES="$GUI $LIB $SUITE_F"
SUITE="./installer/test-installer-a11y.sh"

applied=0; noapply=0; caught=0; survived=0

tree_clean() { [ -z "$(git diff --name-only -- $FILES 2>/dev/null)" ]; }

restore() {
    git checkout -- $FILES 2>/dev/null
    if ! tree_clean; then
        echo "ABORT: tree still dirty after restore; verdicts would be meaningless" >&2
        git diff --stat -- $FILES >&2
        exit 3
    fi
}

run_suite() {
    env -i HOME="$HOME" PATH="$PATH" USER="${USER:-$(id -un)}" \
        TMPDIR="${TMPDIR:-/tmp}" "$SUITE" 2>&1
}

# mutate <id> <file> <from> <to> <assertion substring that must go red>
mutate() {
    local id="$1" file="$2" from="$3" to="$4" want="$5"

    tree_clean || { echo "ABORT: tree dirty BEFORE $id" >&2; exit 3; }
    if ! grep -qF -- "$from" "$file"; then
        printf '%-5s NO-APPLY  anchor absent in %s — this mutant proves nothing\n' "$id" "$file"
        noapply=$((noapply + 1)); return
    fi
    python3 - "$file" "$from" "$to" <<'EDIT'
import sys
p, a, b = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
assert s.count(a) >= 1
open(p, 'w').write(s.replace(a, b, 1))
EDIT
    if tree_clean; then
        printf '%-5s NO-APPLY  the edit changed nothing\n' "$id"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))

    local out; out="$(run_suite)"
    if printf '%s' "$out" | grep -q "^FAIL  .*$want"; then
        printf '%-5s CAUGHT    %s\n' "$id" "$want"
        caught=$((caught + 1))
    else
        printf '%-5s SURVIVED  %s\n' "$id" "$want"
        printf '      ── what the suite said instead ──\n'
        printf '%s\n' "$out" | grep -E '^(FAIL|SKIP|installer-a11y)' | sed 's/^/      /'
        survived=$((survived + 1))
    fi
    restore
}

echo "── baseline: green, or nothing below means anything ──"
base="$(run_suite)"
printf '%s\n' "$base" | grep -E '^installer-a11y'
if ! printf '%s' "$base" | grep -qE '^installer-a11y: [0-9]+ passed, 0 failed'; then
    echo "ABORT: the suite is not green to begin with" >&2
    printf '%s\n' "$base" | grep -E '^(FAIL|SKIP)' >&2
    exit 3
fi

echo
echo "── the mutants ──"

# B1 — the exact regression this work exists to prevent: an account password
#      field goes back to having nothing but a placeholder.
mutate B1 "$GUI" \
    '        a11y(widget, caption)' \
    '        pass  # a11y(widget, caption)' \
    "page 'account': every focusable control it builds announces itself"

# B2 — the Wi-Fi password, on a different page, so the audit is shown to be
#      per-page rather than passing on one page's strength.
mutate B2 "$GUI" \
    'a11y(self.wifi_pw, "Network password")' \
    'pass  # a11y(self.wifi_pw, "Network password")' \
    "page 'wifi': every focusable control it builds announces itself"

# B3 — a Secure Boot enrolment password. These two have no visible caption at
#      all, so the accessible name is their ONLY label.
mutate B3 "$GUI" \
    'a11y(self.e_mok1, "One-time enrolment password",' \
    'a11y(self.e_mok1, "",' \
    "page 'secureboot'"

# B4 — a name that is PRESENT but wrong. Every "is it named" check stays green;
#      only the by-name assertions can see this one.
mutate B4 "$GUI" \
    'a11y(self.kb_test, "Keyboard test",' \
    'a11y(self.kb_test, "Field",' \
    "the keyboard test field announces itself"

# B5 — the audit's own vacuity floor, and the most important mutant here. Empty
#      the set of roles the audit considers interactive and it audits NOTHING:
#      "every focusable control announces itself" is then trivially true. The
#      floor assertion is the only thing standing between that and a green run.
mutate B5 "$SUITE_F" \
    '    "text box", "entry", "password text", "push button", "button",' \
    '    "NOTHING-MATCHES-THIS",' \
    "the audit found controls to audit"

# B6 — the harness. With no session bus the GUI can resolve no accessibility bus
#      and publishes nothing; an empty tree must read as a failure, never as a
#      clean audit.
mutate B6 "$LIB" \
    '    export AT_SPI_BUS_ADDRESS="$ATSPI_BUS"' \
    '    export AT_SPI_BUS_ADDRESS="$ATSPI_BUS"
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$ATSPI_W/no-such-bus"' \
    "builds and reaches the accessibility bus"

echo
printf 'mutants applied=%d, failed-to-apply=%d, caught=%d, SURVIVED=%d\n' \
    "$applied" "$noapply" "$caught" "$survived"
tree_clean || { echo "ABORT: tree dirty at end of run" >&2; exit 3; }
echo "the tree matches HEAD"
[ "$survived" -eq 0 ] && [ "$noapply" -eq 0 ]
