#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-greet-session-bus.sh — prove test-rime-greet-session-bus.sh can go red.
#
#  That suite used to assert ABSENCE — no session bus, no accessibility bus, on
#  either host — and these mutants were written to match: each one SUPPLIED the
#  missing thing and the suite had to notice it was fixed. The hole is closed
#  now, so the mutants are inverted with it. Each one either TAKES A PIECE AWAY
#  (the suite must notice the greeter is unreadable again) or breaks the probe
#  (the suite must notice it can no longer tell).
#
#  Two of them are not about accessibility at all. C6 stops the wrapper exec'ing
#  and C9 makes it die on a missing optional dependency: those are the mutants
#  for "this change cannot stop the login screen from starting", which is the
#  more expensive of the two things this wrapper could get wrong.
#
#  Restores are `git checkout --`, never `cp -p` and never `mv`: authoritative
#  about content, and a fresh mtime. The file set is compared against HEAD after
#  every mutate AND every restore.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

TOML="files/desktop/rime-greet/greetd-config.toml"
SWAYC="files/desktop/rime-greet/sway-greet.conf"
AUTO="files/desktop/rime-greet/labwc-greet/autostart"
LABWCRC="files/desktop/rime-greet/labwc-greet/rc.xml"
WRAP="files/system/libexec/rime-greet-session"
SUITE_F="tests/test-rime-greet-session-bus.sh"
FILES="$TOML $SWAYC $AUTO $LABWCRC $WRAP $SUITE_F"
SUITE="./tests/test-rime-greet-session-bus.sh"

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

run_suite() { "$SUITE" 2>&1; }

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
    if printf '%s' "$out" | pipe_has "^FAIL  .*$want"; then
        printf '%-5s CAUGHT    %s\n' "$id" "$want"
        caught=$((caught + 1))
    else
        printf '%-5s SURVIVED  %s\n' "$id" "$want"
        printf '      ── what the suite said instead ──\n'
        printf '%s\n' "$out" | grep -E '^(FAIL|SKIP|rime-greet-session-bus)' | sed 's/^/      /'
        survived=$((survived + 1))
    fi
    restore
}

echo "── baseline: green, or nothing below means anything ──"
base="$(run_suite)"
printf '%s\n' "$base" | grep -E '^rime-greet-session-bus'
if ! printf '%s' "$base" | pipe_has -E '^rime-greet-session-bus: [0-9]+ passed, 0 failed'; then
    echo "ABORT: the suite is not green to begin with" >&2
    printf '%s\n' "$base" | grep -E '^(FAIL|SKIP)' >&2
    exit 3
fi

echo
echo "── the mutants ──"

# C1 — the wrapper is taken out of greetd's command, which is exactly the state
#      the greeter shipped in until this round. Everything the login screen
#      needs for accessibility disappears at once.
mutate C1 "$TOML" \
    'command = "/usr/libexec/rime-greet-session sway --unsupported-gpu' \
    'command = "sway --unsupported-gpu' \
    "the greeter's client can reach a session bus"

# C2 — the live command becomes the abandoned cage host, which is one of the two
#      lines already sitting in this file as comments. A grep cannot tell a live
#      line from a commented one; tomllib can.
mutate C2 "$TOML" \
    'command = "/usr/libexec/rime-greet-session sway --unsupported-gpu -c /usr/share/rime-greet/sway-greet.conf"' \
    'command = "cage -ds -- qs -p /usr/share/rime-greet/shell.qml"' \
    "the live command is not one of the two hosts kept in comments"

# C3 — the host stops launching the greeter's client. Every assertion about what
#      that client can see is then vacuously true, so the floor assertion is the
#      only thing between this and a clean green run.
mutate C3 "$SWAYC" \
    'exec "qs -p /usr/share/rime-greet/shell.qml; swaymsg exit"' \
    'exec "swaymsg exit"' \
    "the shipped chain really reaches the greeter's own client"

# C4 — the sway host is fixed and the documented fallback is not. A machine that
#      took the fallback would have a login screen no reader can hear, and
#      nothing anywhere to say so.
mutate C4 "$TOML" \
    '#   command = "/usr/libexec/rime-greet-session labwc' \
    '#   command = "labwc' \
    "the documented labwc fallback command carries the wrapper too"

# C5 — the vacuity floor for the session-bus half. Break the probe so it reports
#      a bus whatever it is given, and every positive assertion in the suite
#      still passes. Only the control — the same chain with the wrapper removed
#      — can see it.
mutate C5 "$SUITE_F" \
    "        printf 'session_bus=no\\n'" \
    "        printf 'session_bus=yes\\n'" \
    "with the wrapper gone, the client can reach no session bus at all"

# C6 — THE LOGIN-PATH MUTANT. The wrapper stops exec'ing and forks instead, so
#      greetd ends up holding a shell rather than the compositor: on a
#      successful login it signals the shell, the compositor survives holding
#      the VT and the DRM master, and the machine cannot be logged into. Every
#      accessibility assertion in the suite stays green while that is true.
mutate C6 "$WRAP" \
    'exec "$@"' \
    '"$@"' \
    "the pid greetd would hold is the compositor's own"

# C7 — the wrapper gives the greeter a session bus and stops starting the
#      accessibility bus on it. This is the `dbus-run-session` one-liner that
#      looks like the fix and is not: the bridge has a bus to sit on and still
#      nothing to publish to.
mutate C7 "$WRAP" \
    '        "$_launcher" >/dev/null 2>&1 &' \
    '        : "$_launcher" >/dev/null 2>&1 &' \
    "org.a11y.Bus resolves from inside the greeter chain"

# C8 — the accessibility bus comes up and the REGISTRY does not. The most
#      deceptive failure available here: org.a11y.Bus.GetAddress answers
#      perfectly, everything looks configured, and a screen reader enumerates
#      nothing at all. The original "at-spi cannot work here" report in this
#      repository was this shape.
mutate C8 "$WRAP" \
    '                exec "$_registryd" >/dev/null 2>&1' \
    '                exec true "$_registryd" >/dev/null 2>&1' \
    "a screen reader has a registry to enumerate the tree with"

# C9 — THE OTHER LOGIN-PATH MUTANT. The wrapper stops surviving a missing
#      optional dependency and dies instead. On a machine without dbus-daemon
#      that is a greeter that never starts — over accessibility plumbing, which
#      is never worth a login screen.
mutate C9 "$WRAP" \
    'if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && command -v dbus-daemon >/dev/null 2>&1; then' \
    'if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    command -v dbus-daemon >/dev/null 2>&1 || exit 1' \
    "with no dbus-daemon on the machine, the greeter still starts"

# C10 — the one key a user who cannot see the screen can press is deleted from
#       the sway host. The buses are all still there and there is no way to ask
#       for the reader that would use them.
mutate C10 "$SWAYC" \
    'bindsym --to-code Mod4+Mod1+s exec /usr/libexec/rime-screen-reader toggle' \
    '# (binding removed)' \
    "the sway host binds a key that starts the screen reader"

# C11 — the same key deleted from the fallback host only.
mutate C11 "$LABWCRC" \
    '      <action name="Execute" command="/usr/libexec/rime-screen-reader toggle"/>' \
    '      <action name="Execute" command="true"/>' \
    "the labwc fallback host binds the same key"


# C12 — the wrapper stops cleaning up after the compositor. It EXECS, so it is
#       not there to do it; without the watcher every login and every greeter
#       restart leaves a session bus, an accessibility bus, a launcher and a
#       registry behind as orphans owned by the greetd user. Nothing about
#       accessibility goes wrong, which is exactly why it needs its own mutant.
mutate C12 "$WRAP" \
    '        while kill -0 $$ 2>/dev/null; do sleep 2; done' \
    '        return 0
        while kill -0 $$ 2>/dev/null; do sleep 2; done' \
    "the wrapper takes its buses away when the compositor goes"

# C13 — the fallback host's rc.xml stops being well-formed. labwc has no
#       validate verb, and a malformed config means it comes up with its
#       BUILT-IN keybindings instead of this file's — a greeter a stray shortcut
#       can escape from.
mutate C13 "$LABWCRC" \
    '    </keybind>
  </keyboard>' \
    '    </keybind>
  </keyboardTYPO>' \
    "the fallback host's rc.xml is well-formed XML"

echo
printf 'mutants applied=%d, failed-to-apply=%d, caught=%d, SURVIVED=%d\n' \
    "$applied" "$noapply" "$caught" "$survived"
tree_clean || { echo "ABORT: tree dirty at end of run" >&2; exit 3; }
echo "the tree matches HEAD"
[ "$survived" -eq 0 ] && [ "$noapply" -eq 0 ]
