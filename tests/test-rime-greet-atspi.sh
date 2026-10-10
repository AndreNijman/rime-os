#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-greet-atspi.sh — the login screen, read back over AT-SPI, the way
#  a screen reader reads it (roadmap P2-003, "screen reader ... validated").
#
#  ── What this measures that nothing here measured before ────────────────────
#
#  tests/test-rime-greet-a11y.sh reads `Accessible.name` and `Accessible.role`
#  off live QQuickItems under qmltestrunner. That is a real measurement of the
#  QML, and it is not this criterion. A screen reader never sees a QQuickItem.
#  It connects to an accessibility bus and reads whatever Qt's AT-SPI bridge
#  decided to publish, and the bridge is free to -- and does -- publish
#  something different from what the QML declares:
#
#    * it returns an EMPTY name for any item with Accessible.passwordEdit set,
#      so the password field's carefully written label is not on the bus;
#    * it maps QML roles into AT-SPI's own vocabulary, which is neither the same
#      spelling nor the same numbering;
#    * it drops items it judges uninteresting, so a perfectly labelled control
#      can be absent from the tree entirely;
#    * "a reader can press this" is the org.a11y.atspi.Action interface, which
#      has no QML-side counterpart to read.
#
#  So this suite starts the shipped surface in a real window on a private
#  compositor, against a private accessibility bus, and asserts on the tree that
#  comes back over D-Bus.
#
#  ── The predecessor recorded this as impossible; it is not ──────────────────
#
#  state/agents/p2-b.md and roadmap.yaml both say the AT-SPI walk cannot be
#  performed here, because at-spi2-registryd fails to activate with "Permission
#  denied". That observation was real and the conclusion drawn from it was
#  wrong: the failure is D-Bus ACTIVATION following the SystemdService= line in
#  org.a11y.Bus.service out to the caller's real systemd manager, which
#  reasonably refuses to start a unit into a throwaway bus. Nothing is activated
#  here -- see tests/lib/atspi.sh, which execs the launcher and the registry
#  directly and is careful to prove the bus it ends up on is its own.
#
#  ── Headless, and checked rather than intended ──────────────────────────────
#
#  A private wlroots compositor on the headless backend, in a runtime directory
#  this suite created; a private session bus with an EMPTY service directory so
#  nothing can be activated onto it; a private a11y bus whose path is asserted
#  to be inside that directory before a single node is read. Nothing here can
#  reach the display or the accessibility bus of whoever is sitting at the
#  machine -- and if it somehow did, atspi_start aborts instead of continuing.
#
#  ── Real and fake ───────────────────────────────────────────────────────────
#
#  Real: files/desktop/rime-greet/GreetSurface.qml, staged byte for byte and
#  checked for it, and the whole Qt accessibility bridge.
#  Fake: the palette and the greetd auth backend, exactly as in the sibling
#  suite. No assertion reads a colour and nothing authenticates.
#
#  Run from anywhere: ./tests/test-rime-greet-atspi.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e

cd "$(dirname "$0")" || exit 2
TESTS="$(pwd)"
ROOT="$(cd .. && pwd)"

SURFACE="$ROOT/files/desktop/rime-greet/GreetSurface.qml"
FIXTURE="$TESTS/greet-atspi-app.qml"
WALK="$TESTS/atspi-walk.py"

pass=0; fail=0; skip=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
finish() {
    printf '\nrime-greet-atspi: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

for f in "$SURFACE" "$FIXTURE" "$WALK"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done

# ── the tools ───────────────────────────────────────────────────────────────
QMLRUN=""
for c in qml-qt6 qml /usr/lib64/qt6/bin/qml /usr/lib/qt6/bin/qml; do
    command -v "$c" >/dev/null 2>&1 && { QMLRUN="$c"; break; }
done
if [ -z "$QMLRUN" ]; then
    echo "SKIP: no qml runtime (qt6-qtdeclarative). Nothing was measured."
    exit 0
fi

COMP=""
for c in labwc sway; do command -v "$c" >/dev/null 2>&1 && { COMP="$c"; break; }; done
if [ -z "$COMP" ]; then
    echo "SKIP: no wlroots compositor (labwc/sway). Nothing was measured."
    exit 0
fi

# shellcheck source=tests/lib/atspi.sh
. "$TESTS/lib/atspi.sh"
atspi_require || exit 0

# ── the private world ───────────────────────────────────────────────────────
STAGE=""
COMP_PID=""
APP_PID=""
cleanup() {
    [ -n "$APP_PID"  ] && kill "$APP_PID"  2>/dev/null
    [ -n "$COMP_PID" ] && kill "$COMP_PID" 2>/dev/null
    [ -n "$STAGE"    ] && rm -rf "$STAGE"
    atspi_cleanup
}
trap cleanup EXIT INT TERM

atspi_start || { echo "SKIP: the private accessibility bus did not come up. This is a"; \
                 echo "      COULD-NOT-RUN: no assertion below was evaluated."; exit 0; }

section "the private bus is private"
case "$ATSPI_BUS" in
    *"unix:path=$ATSPI_RUNTIME/"*)
        ok "the accessibility bus is inside this suite's own runtime directory" ;;
    *)  bad "the accessibility bus is inside this suite's own runtime directory" "$ATSPI_BUS"
        finish; exit 1 ;;
esac

# The precondition a real screen reader establishes, asserted rather than
# assumed — and it is asserted because assuming it cost this unit a wrong
# finding. On a developer's machine org.a11y.Status is already true before the
# harness starts, which made it look as though Qt ignored the property; in a
# bare container both flags are false, Qt publishes NOTHING, and the whole suite
# reads as "the greeter has no accessibility tree". The flag is the switch Orca
# throws on connecting, so the harness throws it, and says so here.
case "$ATSPI_STATUS" in
    *"'ScreenReaderEnabled': <true>"*)
        ok "the accessibility status a screen reader sets is on ($ATSPI_STATUS)" ;;
    *)  bad "the accessibility status a screen reader sets is on" \
            "org.a11y.Status reads ${ATSPI_STATUS:-<no answer>} — Qt gates on this, so every assertion below would be about an empty tree"
        finish; exit 1 ;;
esac

STAGE="$ATSPI_W/stage"
mkdir -p "$STAGE" || exit 2
cp "$SURFACE" "$STAGE/GreetSurface.qml"  || exit 2
cp "$FIXTURE" "$STAGE/app.qml"           || exit 2

section "the staged surface is the shipped one"
sum_of() { sha256sum < "$1" | cut -d' ' -f1; }
if [ "$(sum_of "$SURFACE")" = "$(sum_of "$STAGE/GreetSurface.qml")" ]; then
    ok "GreetSurface.qml staged byte for byte"
else
    bad "GreetSurface.qml staged byte for byte" "the staged copy differs from the shipped file"
    finish; exit 1
fi

# ── the private compositor ──────────────────────────────────────────────────
# quickshell is a pure Wayland client and the greeter is a layer-shell surface,
# so the wayland QPA is the faithful one. The headless wlroots backend gives a
# compositor with no output and no input device, which is all that is needed to
# make Qt create a real window -- and the offscreen QPA will NOT do: it provides
# no platform accessibility, so the bridge publishes nothing and every assertion
# below would read an empty tree and have to be written to expect one.
export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
export XDG_SESSION_TYPE=wayland
unset DISPLAY
"$COMP" -s "sleep 600" >"$ATSPI_W/comp.log" 2>&1 &
COMP_PID=$!

WAYLAND_DISPLAY=""
for i in $(seq 1 60); do
    for c in "$ATSPI_RUNTIME"/wayland-*; do
        [ -S "$c" ] && { WAYLAND_DISPLAY="$(basename "$c")"; break 2; }
    done
    sleep 0.25
done
if [ -z "$WAYLAND_DISPLAY" ]; then
    echo "SKIP: $COMP did not create a socket in the private runtime dir."
    sed 's/^/      /' "$ATSPI_W/comp.log" 2>/dev/null | head -5
    echo "      This is a COULD-NOT-RUN, not a pass."
    exit 0
fi
export WAYLAND_DISPLAY

section "the compositor is this suite's own"
if [ -S "$ATSPI_RUNTIME/$WAYLAND_DISPLAY" ]; then
    ok "the wayland socket is inside this suite's own runtime directory"
else
    bad "the wayland socket is inside this suite's own runtime directory" "$WAYLAND_DISPLAY"
    finish; exit 1
fi

# ── the surface ─────────────────────────────────────────────────────────────
section "the greeter publishes an accessibility tree"

before="$(python3 "$WALK" --count 2>/dev/null || echo 0)"
if [ "$before" = "0" ]; then
    ok "nothing is registered with the registry before the greeter starts"
else
    bad "nothing is registered with the registry before the greeter starts" \
        "$before application(s) already present — the tree below is not only the greeter's"
fi

# atspi_run_app, not a bare exec: the surface must DISCOVER the accessibility
# bus through org.a11y.Bus on the session bus, exactly as a real application
# does, rather than being handed the address in AT_SPI_BUS_ADDRESS.
atspi_run_app "$QMLRUN" -platform wayland "$STAGE/app.qml" >"$ATSPI_W/app.out" 2>"$ATSPI_W/app.err" &
APP_PID=$!

registered=0
# shellcheck disable=SC2034  # a bounded wait; nothing reads the counter.
for i in $(seq 1 80); do
    n="$(python3 "$WALK" --count 2>/dev/null || echo 0)"
    [ "$n" != "0" ] && { registered=1; break; }
    kill -0 "$APP_PID" 2>/dev/null || break
    sleep 0.25
done

if [ "$registered" != "1" ]; then
    bad "the greeter registers with the accessibility registry" \
        "no application appeared on the bus"
    echo "      ── the surface's stderr ──"
    sed 's/^/      /' "$ATSPI_W/app.err" 2>/dev/null | head -20
    finish; exit 1
fi
ok "the greeter registers with the accessibility registry"

DUMP="$ATSPI_W/tree.txt"
python3 "$WALK" --dump >"$DUMP" 2>"$ATSPI_W/walk.err"
if [ ! -s "$DUMP" ]; then
    bad "the tree can be read back off the bus" "the walk produced nothing"
    sed 's/^/      /' "$ATSPI_W/walk.err" | head -10
    finish; exit 1
fi
ok "the tree can be read back off the bus"

# A tree with only the application node in it is the shape a broken bridge
# produces, and every "is this name present" assertion would fail loudly --
# but a suite that asserted only ABSENCE of bad things would pass on it. The
# node count is asserted as a floor before anything else is read.
nodes="$(wc -l <"$DUMP")"
if [ "$nodes" -ge 6 ]; then
    ok "the tree has real depth ($nodes nodes)"
else
    bad "the tree has real depth" "only $nodes node(s); the bridge published little more than the app itself"
    sed 's/^/      /' "$DUMP"
fi

# ── what a reader is told ───────────────────────────────────────────────────
section "what a screen reader is told about each control"

has_name() {  # has_name <printable> <exact name>
    if grep -qF "| name=$2 |" "$DUMP"; then ok "$1"; else
        bad "$1" "no node on the bus with name '$2'"; fi
}
role_of() { grep -F "| name=$1 |" "$DUMP" | head -1 | sed -E 's/.*\| role=([^|]*) \| name=.*/\1/' | sed 's/ *$//'; }
line_of() { grep -F "| name=$1 |" "$DUMP" | head -1; }

has_name "the username field reaches the bus with its label"   "Username"
has_name "the session picker's previous arrow reaches the bus"  "Previous session"
has_name "the session picker's next arrow reaches the bus"      "Next session"
has_name "the session name reaches the bus"                     "Session: Rime Desktop"
# The full name, including the hint the greeter appends. Spelled out rather
# than matched loosely: a prefix match would still pass if the hint that tells a
# non-sighted user HOW to change the layout were dropped.
LAYOUT_NAME="Keyboard layout: us (press Space to change)"
has_name "the keyboard layout indicator reaches the bus"        "$LAYOUT_NAME"

# The password field is the interesting one. Qt's bridge returns an EMPTY name
# for a passwordEdit item, so the greeter deliberately carries the label in the
# description instead. That is a claim about the BUS and it is asserted here
# rather than inferred from the Qt header the sibling suite quotes.
pw_line="$(grep -F 'desc=Password. Press Enter to log in' "$DUMP" | head -1)"
if [ -n "$pw_line" ]; then
    ok "the password field reaches the bus with its label in the description"
else
    bad "the password field reaches the bus with its label in the description" \
        "no node whose description carries the password prose"
fi

# ── A measured Qt limitation, pinned so it cannot change unnoticed ──────────
# AT-SPI has a role for this -- ROLE_PASSWORD_TEXT, "password text" -- and Qt
# 6.10.3 never publishes it. Measured four ways on this surface's own toolkit:
# an explicit Accessible.EditableText role plus passwordEdit (what the greeter
# does), passwordEdit with no explicit role, echoMode alone, and an explicit
# Accessible.PasswordText role ALL arrive on the bus as plain "text". There is
# no spelling of the QML that produces the AT-SPI password role, so this is a
# bridge limitation and not something the greeter can fix.
#
# It is asserted as an EQUALITY rather than left alone, so that a future Qt
# which starts publishing the right role makes this suite fail and say so --
# at which point the greeter should be checked and this assertion flipped. A
# test that merely tolerated "text" would stay silent through the fix.
pw_role="$(printf '%s' "$pw_line" | sed -E 's/.*\| role=([^|]*) \| name=.*/\1/' | sed 's/ *$//')"
if [ "$pw_role" = "text" ]; then
    ok "Qt publishes the password field as plain 'text' (no AT-SPI password role exists in this Qt)"
elif [ "$pw_role" = "password text" ]; then
    bad "Qt publishes the password field as plain 'text'" \
        "the bridge now publishes 'password text' — this is an IMPROVEMENT. Re-check the greeter and update this assertion."
else
    bad "Qt publishes the password field as plain 'text'" "unexpected role '$pw_role'"
fi

pw_name="$(printf '%s' "$pw_line" | sed -E 's/.*\| name=([^|]*) \| desc=.*/\1/' | sed 's/ *$//')"
if [ -z "$pw_name" ]; then
    ok "the bridge suppresses the password field's name, as Qt documents"
else
    bad "the bridge suppresses the password field's name, as Qt documents" \
        "the bus reports name='$pw_name' — the sibling suite's premise no longer holds"
fi

# ── The one that would matter most if it were false ─────────────────────────
# Everything connected to an accessibility bus can call
# org.a11y.atspi.Text.GetText on any node. If the bridge handed out the real
# characters of a password field, the login screen would be publishing the
# password to every process on that bus. Measured rather than assumed: the
# reply must be the MASK, and it must not be the text that was typed.
SECRET="correct-horse-battery-staple"

# First: the field really does contain the secret. Without this the masking
# assertion below is satisfied by an empty field, which is exactly the vacuous
# shape this unit has been caught by before (see N8 and K4 in state/agents/p2-b.md).
if grep -qF "name=probe-secretPlaced=1" "$DUMP"; then
    ok "the fixture really typed a secret into the password field"
else
    bad "the fixture really typed a secret into the password field" \
        "the masking assertion below would be vacuous; not trusting it"
fi

pw_text="$(python3 "$WALK" --get-text "Password. Press Enter to log in, Escape to clear the field." 2>/dev/null)"

# The one that would matter most. Checked FIRST and independently of the shape
# of the reply: whatever the bus returns, it must not be the password.
if printf '%s' "$pw_text" | pipe_has -F "$SECRET"; then
    bad "the password never crosses the accessibility bus" \
        "GetText returned the typed password itself"
else
    ok "the password never crosses the accessibility bus"
fi

if [ "$pw_text" = "NO-TEXT-INTERFACE" ]; then
    ok "the password field exposes no readable text on the bus at all"
elif printf '%s' "$pw_text" | pipe_has -E '^[●*•]+$'; then
    ok "the password field reads back MASKED on the bus, never its characters"
else
    bad "the password field reads back MASKED on the bus, never its characters" \
        "GetText returned: $pw_text"
fi
# And the same question asked of the field that is NOT a password, so the
# assertion above is shown capable of distinguishing the two rather than being
# true of everything.
u_text="$(python3 "$WALK" --get-text "Username" 2>/dev/null)"
if [ "$u_text" != "NO-TEXT-INTERFACE" ]; then
    ok "the username field, by contrast, does expose its text — the masking test can tell them apart"
else
    bad "the username field, by contrast, does expose its text" \
        "neither field exposes text, so the masking assertion above proves nothing"
fi

u_role="$(role_of "Username")"
if [ "$u_role" = "text" ] || [ "$u_role" = "entry" ]; then
    ok "the username field is published as an editable text role ($u_role)"
else
    bad "the username field is published as an editable text role" "got '$u_role'"
fi

for n in "Previous session" "Next session"; do
    r="$(role_of "$n")"
    if [ "$r" = "push button" ]; then
        ok "'$n' is published as a button"
    else
        bad "'$n' is published as a button" "got '$r'"
    fi
done

# Focusable is what decides whether a reader can put the caret in a field at
# all. A named node that is not focusable is a label, not a control.
# shellcheck disable=SC2043  # one field today. The greeter grows a password
# field and a session picker, and both belong in this list when they do.
for n in "Username"; do
    if printf '%s' "$(line_of "$n")" | pipe_has 'focusable'; then
        ok "'$n' is focusable, so a reader can reach it"
    else
        bad "'$n' is focusable, so a reader can reach it" "$(line_of "$n")"
    fi
done

# ── what a reader can DO ────────────────────────────────────────────────────
section "what a screen reader can operate"

for n in "Previous session" "Next session" "$LAYOUT_NAME"; do
    if printf '%s' "$(line_of "$n")" | pipe_has -i 'actions=.*Press'; then
        ok "'$n' offers a Press action on the bus"
    else
        bad "'$n' offers a Press action on the bus" "$(line_of "$n")"
    fi
done

probe() {  # probe <probe name prefix> -> current value
    grep -oE "name=probe-$1=[0-9]+" "$DUMP" | head -1 | sed "s/name=probe-$1=//"
}
reread() { python3 "$WALK" --dump >"$DUMP" 2>/dev/null; }

# The load-bearing one. DoAction returns true on this surface for a control
# whose handler does nothing, so the reply is not evidence. The effect is read
# back OFF THE BUS: press "Next session", then re-walk and require both the
# counter probe and the session name to have moved.
cyc_before="$(probe cycleCount)"
python3 "$WALK" --do-action "Next session" >"$ATSPI_W/do1.out" 2>&1
sleep 0.5
reread
cyc_after="$(probe cycleCount)"

if [ -n "$cyc_before" ] && [ -n "$cyc_after" ] && [ "$cyc_after" -gt "$cyc_before" ]; then
    ok "pressing 'Next session' over D-Bus actually runs the greeter's handler ($cyc_before → $cyc_after)"
else
    bad "pressing 'Next session' over D-Bus actually runs the greeter's handler" \
        "cycleCount went $cyc_before → $cyc_after; DoAction said: $(cat "$ATSPI_W/do1.out")"
fi

if grep -qF "| name=Session: Rime Gaming |" "$DUMP"; then
    ok "the session the greeter would launch changed, and the new name is on the bus"
else
    bad "the session the greeter would launch changed, and the new name is on the bus" \
        "still: $(grep -oE 'name=Session: [^|]*' "$DUMP" | head -1)"
fi

# P2-004's half of this screen: a reader must be able to change the keyboard
# layout BEFORE typing the password, which means reaching and pressing the
# layout control without a mouse and without sight.
lay_before="$(probe layoutCycles)"
python3 "$WALK" --do-action "$LAYOUT_NAME" >"$ATSPI_W/do2.out" 2>&1
sleep 0.5
reread
lay_after="$(probe layoutCycles)"
if [ -n "$lay_before" ] && [ -n "$lay_after" ] && [ "$lay_after" -gt "$lay_before" ]; then
    ok "a reader can change the keyboard layout from the login screen ($lay_before → $lay_after)"
else
    bad "a reader can change the keyboard layout from the login screen" \
        "layoutCycles went $lay_before → $lay_after; DoAction said: $(cat "$ATSPI_W/do2.out")"
fi

if grep -qF "| name=Keyboard layout: de (press Space to change) |" "$DUMP"; then
    ok "the layout indicator announces the NEW layout after the press"
else
    bad "the layout indicator announces the NEW layout after the press" \
        "still: $(grep -oE 'name=Keyboard layout: [^|]*' "$DUMP" | head -1)"
fi

printf '\n── the tree a screen reader receives ──\n'
sed 's/^/    /' "$DUMP"

finish
