#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Assertions for /usr/libexec/rime-input-apply — the compositor-neutral input
#  settings generator.
#
#  Three properties matter more than the individual mappings:
#
#    1. The DEFAULTS are a no-op against the configs the image already ships.
#       A settings page whose defaults silently change behaviour on first use is
#       worse than no settings page.
#    2. The in-place rc.xml edit is LOSSLESS outside <libinput>. labwc has no
#       include mechanism, so this is the only file it can be written to, and it
#       carries a forty-line header plus a comment on nearly every decision. An
#       earlier ElementTree round-trip deleted that header silently.
#    3. Every generated config is ACCEPTED by the compositor that consumes it.
#       Names are verified, not remembered: labwc ignores an unknown element in
#       silence, which is how `<tapToClick>` shipped doing nothing.
#
#  Skips cleanly where a compositor is not installed.
#      ./tests/test-rime-input.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# `set +e` is deliberate and load-bearing. This suite COUNTS failures rather
# than aborting on them, and several assertions run commands that exit non-zero
# on purpose — a refusal, a guard firing, a bad argument. GitHub Actions invokes
# a script as `bash -e {0}`, and under `-e` a `x="$(cmd)"` assignment whose
# command exits non-zero terminates the whole script. That is exactly what
# happened: the suite passed locally, and on CI it died part-way through with
# the remaining assertions reported as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Overridable so a change can be proven to FAIL against the generator it
# replaces: point RIME_INPUT_GEN at the previous revision and the assertions
# added with a fix go red. Defaults to the tree's own copy, so CI is unaffected.
GEN="${RIME_INPUT_GEN:-${ROOT}/files/system/libexec/rime-input-apply}"
TMPL="${ROOT}/files/desktop/labwc"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
# `producer | grep -q` under pipefail fails at random: grep -q exits at the
# first match, and a producer that still has output to write gets EPIPE
# (GitHub's runners ignore SIGPIPE, so sed reports "couldn't flush stdout"
# and exits 4) or dies of SIGPIPE (141). has reads its input to the end.
has() { grep "$@" >/dev/null; }
# Counted, and reported in the summary. An uncounted skip is how a suite ends up
# reporting a clean pass for assertions that never ran — which is the same
# failure mode as the bug this file's niri section exists to close.
skp() { printf 'SKIP  %s\n' "$1"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

[ -f "$GEN" ] || { printf 'missing %s\n' "$GEN" >&2; exit 1; }

# A throwaway HOME seeded with the SHIPPED configs, so every assertion runs
# against what users actually get.
mkhome() {
    local h="$1"
    mkdir -p "$h/.config/labwc" "$h/.config/rime-shell" "$h/.config/hypr"
    cp "${TMPL}/rc.xml" "${TMPL}/menu.xml" "$h/.config/labwc/"
    sed 's/@ACCENT@/#D9F99D/g' "${TMPL}/themerc-override" > "$h/.config/labwc/themerc-override"
}

# Every generator run in this suite gets a FIXTURED device list, because the
# per-device pass reads real hardware: without this the output depends on what
# is plugged into the machine running the suite, and — where a Hyprland is
# running — on what that compositor happens to call it. NODEV is the empty list,
# so the sections that are not about devices generate the same bytes on a laptop
# and on a CI runner. The per-device section overrides it with its own.
NODEV="${WORK}/no-devices.json"
echo '[]' > "$NODEV"
run_gen() { HOME="$1" RIME_INPUT_DEVICES="${RIME_INPUT_DEVICES:-$NODEV}" \
            python3 "$GEN" --no-reload "${@:2}"; }

section "the generator runs and self-tests"
python3 -c "import ast,sys; ast.parse(open('$GEN').read())" \
    && ok "rime-input-apply is valid Python" || bad "rime-input-apply is valid Python"

# --self-test generates from the defaults and runs each compositor's own
# validator. It reports SKIP for a compositor that is absent, so the only
# failure mode here is a mapping the compositor rejects.
st="$(python3 "$GEN" --self-test 2>&1)"
printf '%s\n' "$st" | sed 's/^/      /'
printf '%s\n' "$st" | has '^FAIL' \
    && bad "every installed compositor accepts the generated config" \
    || ok "every installed compositor accepts the generated config"

section "defaults are a no-op against the shipped config"
h="${WORK}/defaults"; mkhome "$h"
cp "$h/.config/labwc/rc.xml" "${WORK}/rc.orig"
echo '{}' > "$h/.config/rime-shell/input.json"
run_gen "$h" >/dev/null 2>&1

# The shipped rc.xml sets naturalScroll/tap/tapAndDrag/accelProfile on the
# touchpad. The defaults must reproduce all four, or a user opening Settings for
# the first time silently changes their own touchpad.
for want in '<naturalScroll>yes</naturalScroll>' '<tap>yes</tap>' \
            '<tapAndDrag>yes</tapAndDrag>' '<accelProfile>adaptive</accelProfile>'; do
    sed -n '/<device category="touchpad"/,/<\/device>/p' "$h/.config/labwc/rc.xml" \
        | has -F "$want" \
        && ok "default reproduces ${want}" || bad "default reproduces ${want}"
done

# The same property on Hyprland, which nothing checked. It matters more there:
# the seeded rime/input-defaults.lua set two touchpad options out of ten, so a
# user who moved one slider on the Input page also silently gained drag lock,
# disable-while-typing and clickfinger clicking — the page changing three things
# nobody asked it to.
SEED="${ROOT}/files/desktop/hypr/rime/input-defaults.lua"
if [ ! -f "$SEED" ]; then
    bad "the seeded Hyprland input defaults are where this suite looks"
else
    python3 - "$h/.config/hypr/rime/input.lua" "$SEED" <<'PY' \
        && ok "the seeded Hyprland defaults match what the defaults generate" \
        || bad "the seeded Hyprland defaults match what the defaults generate"
import re, sys
generated, seeded = (open(p, encoding="utf-8").read() for p in sys.argv[1:3])

def blocks(text):
    """The input block's own keys, and the touchpad sub-block's, kept apart.

    Both carry natural_scroll and scroll_factor with different meanings — one
    is the mouse's and one the touchpad's — so a flat key scan compares the
    touchpad's value against the mouse's and reports a difference that is not
    one.
    """
    pad = re.search(r"^\s+touchpad = \{$(.*?)^\s+\},$", text, re.M | re.S)
    body = pad.group(1) if pad else ""
    outer = text.replace(body, "") if pad else text
    pairs = lambda s: {k: v.strip() for k, v in re.findall(r"^\s+(\w+)\s+=\s+(.+),$", s, re.M)}
    return pairs(outer), pairs(body)

gen_outer, gen_pad = blocks(generated)
seed_outer, seed_pad = blocks(seeded)
wrong = []
for label, want, have in (("input", gen_outer, seed_outer), ("touchpad", gen_pad, seed_pad)):
    for key, value in want.items():
        if have.get(key) != value:
            wrong.append(f"{label}.{key}: generated {value}, seeded {have.get(key, 'nothing')}")
for w in wrong:
    print("      " + w)
sys.exit(1 if wrong else 0)
PY
fi

section "the rc.xml edit is lossless outside what it owns"
# rc.xml is the only file labwc reads, so the generator writes into a file full
# of the user's keybinds, theme and window rules. It owns exactly two spans:
# <libinput>, and the repeat rate/delay pair inside <keyboard>. Everything else
# has to come back byte-for-byte, which is what this strips down to.
strip_owned() {
    sed -e '/<libinput>/,/<\/libinput>/d' \
        -e '/<repeatRate>/d' -e '/<repeatDelay>/d' "$1"
}
diff <(strip_owned "${WORK}/rc.orig") <(strip_owned "$h/.config/labwc/rc.xml") >/dev/null \
    && ok "everything the generator does not own is byte-identical" \
    || bad "everything the generator does not own is byte-identical"

# And the converse: nothing else was added. Counting the added lines is what
# separates "wrote two elements" from "wrote two elements and reindented the
# file", which the diff above would forgive if the reindent were symmetric.
added="$(diff <(sed '/<libinput>/,/<\/libinput>/d' "${WORK}/rc.orig") \
              <(sed '/<libinput>/,/<\/libinput>/d' "$h/.config/labwc/rc.xml") \
         | grep -c '^>')"
[ "$added" = 2 ] \
    && ok "exactly two lines are added outside <libinput>" \
    || bad "exactly two lines are added outside <libinput> (got ${added})"

# The header comment sits OUTSIDE the root element, where an ElementTree
# round-trip cannot represent it. This is the assertion that caught that.
head -8 "$h/.config/labwc/rc.xml" | has 'Rime OS' \
    && ok "the file header comment survives" || bad "the file header comment survives"

before="$(grep -c '<!--' "${WORK}/rc.orig")"
after="$(grep -c '<!--' "$h/.config/labwc/rc.xml")"
[ "$before" = "$after" ] \
    && ok "all $before comments survive" || bad "all comments survive (${before} -> ${after})"

if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout "$h/.config/labwc/rc.xml" 2>/dev/null \
        && ok "the rewritten rc.xml is well-formed" || bad "the rewritten rc.xml is well-formed"
else
    skp "xmllint unavailable"
fi

section "settings reach every compositor"
h="${WORK}/applied"; mkhome "$h"
cat > "$h/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "tap": false, "natural_scroll": false, "click_method": "buttonAreas",
                "speed": 0.5, "three_finger_drag": true, "tap_button_map": "lmr" },
  "pointer":  { "left_handed": true, "speed": -0.2 },
  "keyboard": { "repeat_rate": 40, "repeat_delay": 300 } }
JSON
run_gen "$h" >/dev/null 2>&1

tp() { sed -n '/<device category="touchpad"/,/<\/device>/p' "$h/.config/labwc/rc.xml"; }
[ "$(tp | sed -n 's|.*<tap>\(.*\)</tap>.*|\1|p')" = "no" ] \
    && ok "labwc: tap disabled" || bad "labwc: tap disabled"
[ "$(tp | sed -n 's|.*<clickMethod>\(.*\)</clickMethod>.*|\1|p')" = "buttonAreas" ] \
    && ok "labwc: clickMethod applied" || bad "labwc: clickMethod applied"
[ "$(tp | sed -n 's|.*<tapButtonMap>\(.*\)</tapButtonMap>.*|\1|p')" = "lmr" ] \
    && ok "labwc: tapButtonMap applied" || bad "labwc: tapButtonMap applied"
tp | has threeFingerDrag \
    && ok "labwc: threeFingerDrag emitted only when enabled" \
    || bad "labwc: threeFingerDrag emitted only when enabled"
grep -q '<leftHanded>yes</leftHanded>' "$h/.config/labwc/rc.xml" \
    && ok "labwc: leftHanded reaches the pointer device" || bad "labwc: leftHanded reaches the pointer device"

# The Hyprland half is a Lua module hyprland.lua requires, not a hyprlang
# fragment it sources. hyprlang's `tap-to-click` is spelled `tap_to_click` in
# Lua, and a wrong key is REJECTED rather than ignored — which is what makes
# the generator's own --self-test meaningful.
H="$h/.config/hypr/rime/input.lua"
[ -s "$H" ] && ok "hyprland: a Lua input module is written" \
            || bad "hyprland: a Lua input module is written"
grep -q 'tap_to_click            = false' "$H" && ok "hyprland: tap_to_click applied" || bad "hyprland: tap_to_click applied"
grep -q 'left_handed    = true'  "$H" && ok "hyprland: left_handed applied"   || bad "hyprland: left_handed applied"
grep -q 'repeat_rate    = 40'    "$H" && ok "hyprland: repeat_rate applied"   || bad "hyprland: repeat_rate applied"
grep -q 'tap_button_map          = "lmr"' "$H" && ok "hyprland: tap_button_map applied" || bad "hyprland: tap_button_map applied"
# The old hyprlang fragment must not come back beside it: 0.56.2 loads
# hyprland.lua and never mentions the .conf it ignored, so a regression here is
# a settings page that appears to work and changes nothing.
[ -e "$h/.config/hypr/rime-input.conf" ] \
    && bad "hyprland: no legacy rime-input.conf is written" \
    || ok "hyprland: no legacy rime-input.conf is written"
grep -q 'hl.config({' "$H" && ok "hyprland: the module is Lua, not hyprlang" \
                           || bad "hyprland: the module is Lua, not hyprlang"

N="$h/.config/rime-shell/RimeShellInput.kdl"
# niri expresses a false boolean by OMITTING the flag, so `tap` must be absent.
grep -qE '^\s+tap$' "$N" \
    && bad "niri: a disabled boolean is omitted, not written false" \
    || ok "niri: a disabled boolean is omitted, not written false"
grep -q 'click-method "button-areas"' "$N" && ok "niri: click-method translated" || bad "niri: click-method translated"
grep -q 'tap-button-map "left-middle-right"' "$N" && ok "niri: tap-button-map translated" || bad "niri: tap-button-map translated"
grep -q 'repeat-rate 40' "$N" && ok "niri: repeat-rate applied" || bad "niri: repeat-rate applied"

if command -v niri >/dev/null 2>&1; then
    niri validate --config "$N" >/dev/null 2>&1 \
        && ok "niri validates the generated file with non-default values" \
        || bad "niri validates the generated file with non-default values"
elif [ -n "${RIME_REQUIRE_NIRI:-}" ]; then
    # Same escalation as the reachability section at the end of this file, so
    # the flag means what its name says rather than covering only one of the
    # two places niri is needed.
    bad "niri is present (RIME_REQUIRE_NIRI is set)"
else
    skp "niri unavailable: the generated file is not validated with non-default values"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  UI-003: no control may write the model and change nothing.
#
#  Every assertion in this section covers a switch or slider that was on the
#  Input page, wrote input.json, ran the generator, reported success — and did
#  not reach the compositor, because the generator never emitted the option.
#  A user changed the setting and nothing happened, which is the complaint.
#
#  Each one names a real option in the shipped compositor, checked against that
#  compositor's own validator, not against its wiki.
# ─────────────────────────────────────────────────────────────────────────────
section "every control reaches the compositor that can do it"
h="${WORK}/noop"; mkhome "$h"
mkdir -p "$h/.config/niri"
cat > "$h/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "left_handed": true, "drag_lock": false, "tap_and_drag": false,
                "scroll_method": "edge", "three_finger_drag": true },
  "pointer":  { "left_handed": true, "middle_emulation": true },
  "keyboard": { "repeat_rate": 42, "repeat_delay": 275 } }
JSON
run_gen "$h" >/dev/null 2>&1
N="$h/.config/rime-shell/RimeShellInput.kdl"
H="$h/.config/hypr/rime/input.lua"
RC="$h/.config/labwc/rc.xml"

ntp() { sed -n '/^    touchpad {/,/^    }/p' "$N"; }
nms() { sed -n '/^    mouse {/,/^    }/p' "$N"; }

# niri. Six options niri 26.04 accepts and the generator never wrote.
ntp | has -E '^\s+left-handed$' \
    && ok "niri: touchpad left-handed is written" || bad "niri: touchpad left-handed is written"
ntp | has -E '^\s+drag false$' \
    && ok "niri: tap-and-drag off is written, not omitted" \
    || bad "niri: tap-and-drag off is written, not omitted"
ntp | has -E '^\s+scroll-method "edge"$' \
    && ok "niri: touchpad scroll-method is written" || bad "niri: touchpad scroll-method is written"
nms | has -E '^\s+left-handed$' \
    && ok "niri: mouse left-handed is written" || bad "niri: mouse left-handed is written"
nms | has -E '^\s+middle-emulation$' \
    && ok "niri: mouse middle-emulation is written" || bad "niri: mouse middle-emulation is written"
# drag-lock is a BARE flag in niri: `drag-lock false` is a parse error, so off
# has to be absence. Asserted both ways below.
ntp | has -E '^\s+drag-lock$' \
    && bad "niri: drag lock off is absent, not written false" \
    || ok "niri: drag lock off is absent, not written false"

# Hyprland. drag_3fg is an enum the touchpad block accepts and never got.
grep -qE 'drag_3fg *= *1,' "$H" \
    && ok "hyprland: three-finger drag reaches the touchpad block" \
    || bad "hyprland: three-finger drag reaches the touchpad block"

# labwc. Keyboard repeat is not a libinput setting there, so it went nowhere.
grep -qF '<repeatRate>42</repeatRate>' "$RC" \
    && ok "labwc: repeat rate reaches <keyboard>" || bad "labwc: repeat rate reaches <keyboard>"
grep -qF '<repeatDelay>275</repeatDelay>' "$RC" \
    && ok "labwc: repeat delay reaches <keyboard>" || bad "labwc: repeat delay reaches <keyboard>"
# Inside the block labwc reads, not merely somewhere in the file.
sed -n '/<keyboard[ >]/,/<\/keyboard>/p' "$RC" | has -F '<repeatRate>42</repeatRate>' \
    && ok "labwc: the repeat rate is inside <keyboard>" \
    || bad "labwc: the repeat rate is inside <keyboard>"
# The keybind markers another generator writes between must survive the edit.
grep -qF 'RIME-KEYBINDS-END' "$RC" \
    && ok "labwc: the keybind markers survive the keyboard edit" \
    || bad "labwc: the keybind markers survive the keyboard edit"

# Re-running must not stack a second pair of elements.
run_gen "$h" >/dev/null 2>&1
[ "$(grep -c '<repeatRate>' "$RC")" = 1 ] \
    && ok "labwc: a second run replaces the repeat rate rather than adding one" \
    || bad "labwc: a second run replaces the repeat rate rather than adding one ($(grep -c '<repeatRate>' "$RC"))"

if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout "$RC" 2>/dev/null \
        && ok "labwc: rc.xml is still well-formed after the keyboard edit" \
        || bad "labwc: rc.xml is still well-formed after the keyboard edit"
else
    skp "xmllint unavailable: the keyboard edit is not XML-checked"
fi

if command -v niri >/dev/null 2>&1; then
    niri validate --config "$N" >/dev/null 2>&1 \
        && ok "niri validates the file with every newly-written option" \
        || bad "niri validates the file with every newly-written option"
elif [ -n "${RIME_REQUIRE_NIRI:-}" ]; then
    bad "niri is present (RIME_REQUIRE_NIRI is set)"
else
    skp "niri unavailable: the newly-written options are not validated"
fi

# drag-lock ON, in its own run, because absence proves nothing on its own.
h="${WORK}/draglock"; mkhome "$h"
echo '{"touchpad":{"drag_lock":true,"tap_and_drag":true}}' > "$h/.config/rime-shell/input.json"
run_gen "$h" >/dev/null 2>&1
sed -n '/^    touchpad {/,/^    }/p' "$h/.config/rime-shell/RimeShellInput.kdl" \
    | has -E '^\s+drag-lock$' \
    && ok "niri: drag lock on is written as the bare flag" \
    || bad "niri: drag lock on is written as the bare flag"

# ─────────────────────────────────────────────────────────────────────────────
#  Per-device settings, and the four Hyprland can express no other way.
#
#  Hyprland's `input.touchpad` block has no sensitivity, accel_profile,
#  left_handed or scroll_method — --verify-config answers "unknown config key"
#  for each. They live on the parent `input` block, where they apply to the
#  mouse as well, so the Touchpad page's Pointer speed, Acceleration,
#  Left-handed and Scroll method can only mean "this touchpad" through
#  hl.device, by name. That is why the generator enumerates devices at all.
#
#  The enumeration is fixtured here. Asserting against whatever is plugged into
#  the machine running the suite is how a test comes to pass on a laptop and
#  fail on a build runner.
# ─────────────────────────────────────────────────────────────────────────────
section "input devices are classified, and a value of 0 is not a yes"
python3 - "$GEN" <<'PY' && ok "udev's ID_INPUT_POINTINGSTICK=0 is not read as a trackpoint" \
                        || bad "udev's ID_INPUT_POINTINGSTICK=0 is not read as a trackpoint"
import importlib.machinery, importlib.util, sys
spec = importlib.util.spec_from_loader("gen", importlib.machinery.SourceFileLoader("gen", sys.argv[1]))
gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)
# The real udev record of a touchpad's companion mouse node, verbatim.
flags = gen.parse_udev_flags("E:ID_INPUT=1\nE:ID_INPUT_POINTINGSTICK=0\nE:ID_INPUT_MOUSE=1\n", prefix="E:")
sys.exit(0 if flags == {"ID_INPUT", "ID_INPUT_MOUSE"} else 1)
PY

# Real hardware, read-only: the enumeration has to work for a user who is not in
# the `input` group, which is every Rime user. `libinput list-devices` cannot.
#
# Guarded on an actual event node rather than on the directory: a CI container
# has /sys/class/input and nothing in it, and "the directory exists" would turn
# that into a failure instead of the skip it is.
if compgen -G "/sys/class/input/event*" >/dev/null; then
    real="$(python3 "$GEN" --devices 2>/dev/null)"
    printf '%s' "$real" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if d["devices"] else 1)' \
        && ok "the enumeration finds devices without the input group" \
        || bad "the enumeration finds devices without the input group"
    printf '%s' "$real" | python3 -c 'import json,sys
d = json.load(sys.stdin)
bad = [x for x in d["devices"] if x["type"] not in
       ("touchpad","trackpoint","tablet","touchscreen","mouse","keyboard","other")]
sys.exit(1 if bad else 0)' \
        && ok "every enumerated device carries a known kind" \
        || bad "every enumerated device carries a known kind"
else
    skp "no input event nodes: the enumeration is not exercised against real devices"
fi

section "the four settings Hyprland can only express per device"
DEVFIX="${WORK}/devices.json"
cat > "$DEVFIX" <<'JSON'
[ {"name":"Fixture Touchpad","type":"touchpad","hypr_name":"fixture-touchpad","hypr_name_source":"derived"},
  {"name":"Fixture TrackPoint","type":"trackpoint","hypr_name":"fixture-trackpoint","hypr_name_source":"derived"},
  {"name":"Fixture Mouse","type":"mouse","hypr_name":"fixture-mouse","hypr_name_source":"derived"},
  {"name":"Fixture Tablet","type":"tablet","hypr_name":"fixture-tablet","hypr_name_source":"derived"},
  {"name":"Fixture Touchscreen","type":"touchscreen","hypr_name":"fixture-touchscreen","hypr_name_source":"derived"} ]
JSON
run_dev() { HOME="$1" RIME_INPUT_DEVICES="$DEVFIX" python3 "$GEN" --no-reload "${@:2}"; }

h="${WORK}/perdev"; mkhome "$h"
cat > "$h/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "speed": 0.4, "accel_profile": "flat", "left_handed": true,
                "scroll_method": "edge" },
  "pointer":  { "speed": -0.3, "middle_emulation": true },
  "devices":  { "Fixture Mouse": { "type": "mouse", "speed": 0.9, "natural_scroll": true },
                "Fixture TrackPoint": { "type": "trackpoint", "tap": true } } }
JSON
notes="$(run_dev "$h" 2>&1)"
H="$h/.config/hypr/rime/input.lua"
RC="$h/.config/labwc/rc.xml"

tpdev() { sed -n '/name = "fixture-touchpad"/,/^})/p' "$H"; }
tpdev | has 'sensitivity = 0.4' \
    && ok "hyprland: touchpad pointer speed reaches the touchpad, by name" \
    || bad "hyprland: touchpad pointer speed reaches the touchpad, by name"
tpdev | has 'accel_profile = "flat"' \
    && ok "hyprland: touchpad acceleration reaches the touchpad, by name" \
    || bad "hyprland: touchpad acceleration reaches the touchpad, by name"
tpdev | has 'left_handed = true' \
    && ok "hyprland: touchpad left-handed reaches the touchpad, by name" \
    || bad "hyprland: touchpad left-handed reaches the touchpad, by name"
# The one spelling nothing else catches: --verify-config accepts any string for
# scroll_method and the compositor then matches none of them, so the model's
# own word would parse clean and do nothing.
tpdev | has 'scroll_method = "2fg"' \
    && bad "hyprland: the touchpad scroll method is Hyprland's spelling" \
    || ok "hyprland: the touchpad scroll method is Hyprland's spelling"
tpdev | has 'scroll_method = "edge"' \
    && ok "hyprland: the touchpad scroll method reaches the touchpad, by name" \
    || bad "hyprland: the touchpad scroll method reaches the touchpad, by name"
grep -q 'scroll_method *= *"twofinger"' "$H" \
    && bad "hyprland: the model's own scroll-method word never reaches the config" \
    || ok "hyprland: the model's own scroll-method word never reaches the config"

# The mouse must NOT pick up the touchpad's pointer speed. That is exactly what
# writing these four globally would do, and why they are routed per device.
sed -n '/^    input = {/,/^        touchpad = {/p' "$H" | has 'sensitivity    = -0.3' \
    && ok "hyprland: the global input block still carries the mouse's speed" \
    || bad "hyprland: the global input block still carries the mouse's speed"

# Middle-click emulation has no global Hyprland option at all.
sed -n '/name = "fixture-mouse"/,/^})/p' "$H" | has 'middle_button_emulation = true' \
    && ok "hyprland: mouse middle-click emulation reaches the mouse, by name" \
    || bad "hyprland: mouse middle-click emulation reaches the mouse, by name"
sed -n '/name = "fixture-trackpoint"/,/^})/p' "$H" | has 'middle_button_emulation = true' \
    && ok "hyprland: a trackpoint counts as a pointer for middle-click emulation" \
    || bad "hyprland: a trackpoint counts as a pointer for middle-click emulation"

# A per-device override beats the section-wide value routed to the same device.
sed -n '/name = "fixture-mouse"/,/^})/p' "$H" | has 'sensitivity = 0.9' \
    && ok "hyprland: a per-device speed beats the section it belongs to" \
    || bad "hyprland: a per-device speed beats the section it belongs to"
sed -n '/name = "fixture-mouse"/,/^})/p' "$H" | has 'natural_scroll = true' \
    && ok "hyprland: a per-device override is written by name" \
    || bad "hyprland: a per-device override is written by name"

# labwc matches a device by name too — labwc-config(5): a category that is not
# one of its keywords "will be used to match the device name directly".
grep -q '<device category="Fixture Mouse">' "$RC" \
    && ok "labwc: a per-device override becomes a named device profile" \
    || bad "labwc: a per-device override becomes a named device profile"
sed -n '/<device category="Fixture Mouse">/,/<\/device>/p' "$RC" | has '<pointerSpeed>0.9</pointerSpeed>' \
    && ok "labwc: the named profile carries the value in labwc's own words" \
    || bad "labwc: the named profile carries the value in labwc's own words"
# Order is the argument: a name profile has to come after the category ones.
[ "$(grep -n '<device category=' "$RC" | tail -1 | grep -c 'Fixture')" = 1 ] \
    && ok "labwc: named profiles are written after the category profiles" \
    || bad "labwc: named profiles are written after the category profiles"

# A tap setting on a trackpoint is a category error, not a typo.
printf '%s\n' "$notes" | has 'a trackpoint has no such setting' \
    && ok "a touchpad-only setting on a trackpoint is refused and named" \
    || bad "a touchpad-only setting on a trackpoint is refused and named"

# And the whole thing still has to be a config the compositors accept.
st="$(RIME_INPUT_DEVICES="$DEVFIX" python3 "$GEN" --self-test 2>&1)"
printf '%s\n' "$st" | has '^FAIL' \
    && bad "the per-device output is accepted by every installed compositor" \
    || ok "the per-device output is accepted by every installed compositor"

section "a control no compositor can honour is disabled with a reason"
caps() { RIME_INPUT_DEVICES="$1" python3 "$GEN" --capabilities; }
NOPAD="${WORK}/nopad.json"
echo '[{"name":"Fixture Mouse","type":"mouse","hypr_name":"fixture-mouse","hypr_name_source":"derived"}]' > "$NOPAD"

caps "$DEVFIX" | python3 -c '
import json, sys
c = json.load(sys.stdin)["controls"]
sys.exit(0 if c["touchpad.speed"]["hyprland"]["supported"] else 1)' \
    && ok "with a touchpad present, Hyprland can do touchpad speed" \
    || bad "with a touchpad present, Hyprland can do touchpad speed"

caps "$NOPAD" | python3 -c '
import json, sys
e = json.load(sys.stdin)["controls"]["touchpad.speed"]["hyprland"]
sys.exit(0 if not e["supported"] and "no touchpad" in e["reason"] else 1)' \
    && ok "with no touchpad, it is disabled and the reason says why" \
    || bad "with no touchpad, it is disabled and the reason says why"

caps "$DEVFIX" | python3 -c '
import json, sys
c = json.load(sys.stdin)["controls"]
e = c["touchpad.three_finger_drag"]["niri"]
v = c["touchpad.click_method"]["niri"]
sys.exit(0 if (not e["supported"] and e["reason"]
               and "none" in v.get("unsupported_values", {})) else 1)' \
    && ok "niri declares its three-finger-drag and click-method gaps" \
    || bad "niri declares its three-finger-drag and click-method gaps"

caps "$DEVFIX" | python3 -c '
import json, sys
p = json.load(sys.stdin)["per_device"]
sys.exit(0 if (p["hyprland"]["supported"] and p["labwc"]["supported"]
               and not p["niri"]["supported"] and p["niri"]["reason"]) else 1)' \
    && ok "per-device settings are declared unsupported on niri, with a reason" \
    || bad "per-device settings are declared unsupported on niri, with a reason"

# Every reason a user can be shown must be a sentence, not an option name.
caps "$DEVFIX" | python3 -c '
import json, sys
d = json.load(sys.stdin)
reasons = [e["reason"] for c in d["controls"].values() for e in c.values()
           if not e["supported"]]
reasons += [v["reason"] for v in d["per_device"].values() if not v["supported"]]
reasons += [r for c in d["controls"].values() for e in c.values()
            for r in e.get("unsupported_values", {}).values()]
bad = [r for r in reasons if len(r) < 20 or "_" in r or r.endswith(".")]
sys.exit(1 if bad else 0)' \
    && ok "every disabled reason reads as an explanation, not an option name" \
    || bad "every disabled reason reads as an explanation, not an option name"

section "the page can read back what is actually in effect"
# Divergence is the property worth having: a control that writes and never reads
# cannot tell a working setting from one whose option was renamed upstream.
h="${WORK}/readback"; mkhome "$h"
mkdir -p "$h/.config/niri"
echo '{"touchpad":{"tap":false,"natural_scroll":false,"speed":0.25}}' \
    > "$h/.config/rime-shell/input.json"
run_dev "$h" >/dev/null 2>&1
rb="$(HOME="$h" RIME_INPUT_DEVICES="$DEVFIX" XDG_CURRENT_DESKTOP=niri \
      python3 "$GEN" --read-back --model "$h/.config/rime-shell/input.json" 2>/dev/null)"
printf '%s' "$rb" | python3 -c '
import json, sys
d = json.load(sys.stdin)
v = d["values"]
ok = (d["compositor"] == "niri"
      and v["touchpad.tap"]["value"] is False
      and v["touchpad.natural_scroll"]["value"] is False
      and abs(v["touchpad.speed"]["value"] - 0.25) < 1e-6
      and not d["diverged"])
sys.exit(0 if ok else 1)' \
    && ok "niri: the generated file reads back as the values that were written" \
    || bad "niri: the generated file reads back as the values that were written"

# Every niri value must say it came from a file, because niri has no input query
# and claiming a device read would be a lie the user cannot check.
printf '%s' "$rb" | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if all(v["source"] == "file" for v in d["values"].values())
         and any("no input query" in n for n in d["notes"]) else 1)' \
    && ok "niri: the read-back says it came from the file, not the compositor" \
    || bad "niri: the read-back says it came from the file, not the compositor"

# Now break the file behind the page's back. A read-back that still agrees with
# the model is not reading anything.
sed -i 's/^        tap$//' "$h/.config/rime-shell/RimeShellInput.kdl"
sed -i 's/^        drag true$/        drag false/' "$h/.config/rime-shell/RimeShellInput.kdl"
HOME="$h" RIME_INPUT_DEVICES="$DEVFIX" XDG_CURRENT_DESKTOP=niri \
    python3 "$GEN" --read-back --model "$h/.config/rime-shell/input.json" 2>/dev/null \
    | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if any(x["control"] == "touchpad.tap_and_drag" for x in d["diverged"]) else 1)' \
    && ok "a value changed behind the page is reported as diverged" \
    || bad "a value changed behind the page is reported as diverged"

section "bad input is corrected, not obeyed"
h="${WORK}/bad"; mkhome "$h"
cat > "$h/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "speed": 99, "accel_profile": "turbo", "click_method": "wishful",
                "tap_button_map": "xyz", "scroll_factor": 0 },
  "keyboard": { "repeat_rate": 9999 },
  "nonsense": { "whatever": 1 } }
JSON
notes="$(run_gen "$h" 2>&1)"
printf '%s\n' "$notes" | has 'must be between -1.0 and 1.0' \
    && ok "an out-of-range speed is clamped and reported" || bad "an out-of-range speed is clamped and reported"
printf '%s\n' "$notes" | has "accel_profile" \
    && ok "an unknown accel profile is corrected and reported" || bad "an unknown accel profile is corrected and reported"
grep -q '<accelProfile>adaptive</accelProfile>' "$h/.config/labwc/rc.xml" \
    && ok "the corrected value is what gets written" || bad "the corrected value is what gets written"
if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout "$h/.config/labwc/rc.xml" 2>/dev/null \
        && ok "nonsense input still yields valid XML" || bad "nonsense input still yields valid XML"
fi

section "one compositor's broken validator is not a veto over the other two"
# A machine with a half-installed niri could not change a touchpad setting in a
# Hyprland session: the generator refused to write ANYTHING when either
# validator said no, the page reported success, and the reason went to a stderr
# line about a compositor the user does not run.
h="${WORK}/veto"; mkhome "$h"
mkdir -p "$h/.config/hypr" "${WORK}/vetobin"
printf '#!/bin/sh\nexit 127\n' > "${WORK}/vetobin/niri"
chmod 0755 "${WORK}/vetobin/niri"
echo '{"touchpad":{"tap":false}}' > "$h/.config/rime-shell/input.json"
out="$(HOME="$h" RIME_INPUT_DEVICES="$NODEV" PATH="${WORK}/vetobin:$PATH" \
       python3 "$GEN" --no-reload 2>&1)"
printf '%s\n' "$out" | has 'niri config was rejected' \
    && ok "it says the niri half was refused" || bad "it says the niri half was refused"
grep -q '<tap>no</tap>' "$h/.config/labwc/rc.xml" \
    && ok "labwc is still written when niri refuses" \
    || bad "labwc is still written when niri refuses"
grep -q 'tap_to_click            = false' "$h/.config/hypr/rime/input.lua" \
    && ok "Hyprland is still written when niri refuses" \
    || bad "Hyprland is still written when niri refuses"
# And the file the refusing compositor reads is untouched rather than replaced
# with something it just rejected.
[ -e "$h/.config/rime-shell/RimeShellInput.kdl" ] \
    && bad "the refused niri config is not written" \
    || ok "the refused niri config is not written"

section "an unparseable rc.xml is never overwritten"
h="${WORK}/broken"; mkhome "$h"
printf '<?xml version="1.0"?>\n<labwc_config><core>\n' > "$h/.config/labwc/rc.xml"
cp "$h/.config/labwc/rc.xml" "${WORK}/broken.orig"
echo '{}' > "$h/.config/rime-shell/input.json"
run_gen "$h" 2>&1 | has 'refusing to touch it' \
    && ok "it refuses and says so" || bad "it refuses and says so"
cmp -s "$h/.config/labwc/rc.xml" "${WORK}/broken.orig" \
    && ok "the broken file is left exactly as it was" || bad "the broken file is left exactly as it was"

section "idempotence"
h="${WORK}/idem"; mkhome "$h"
echo '{"touchpad":{"tap":false}}' > "$h/.config/rime-shell/input.json"
run_gen "$h" >/dev/null 2>&1
cp "$h/.config/labwc/rc.xml" "${WORK}/idem.1"
run_gen "$h" >/dev/null 2>&1
cmp -s "$h/.config/labwc/rc.xml" "${WORK}/idem.1" \
    && ok "re-running changes nothing" || bad "re-running changes nothing"

# ─────────────────────────────────────────────────────────────────────────────
#  niri: the generated file has to be REACHABLE, not merely written.
#
#  Every assertion above this line proves rime-input-apply produced a file niri
#  would accept. None of them proved niri ever READS it — and it did not. The
#  generator emitted a comment telling the user to hand-add the `include` line
#  themselves, while Hyprland got a real `source =` in the shipped template. So
#  a niri user changed a touchpad setting, the Settings page reported success,
#  the .kdl was written, and nothing happened. Same for keybinds.
#
#  niri DOES have an include mechanism (top-level `include`, since 25.11; the
#  image ships 26.04), so nothing had to be invented — the line just was not
#  being written. rime-shell-firstrun writes it now, and this section drives
#  that block from the shipped provisioner rather than restating it.
#
#  The proof that matters is the LAST one: break the included file and niri must
#  reject the config that includes it. A config that still validates is a config
#  that never read it.
# ─────────────────────────────────────────────────────────────────────────────
FIRSTRUN="${ROOT}/files/system/libexec/rime-shell-firstrun"

section "niri: the include is wired by the provisioner, not by the user"

# Extracted by its own comment marker, so a renamed or deleted block fails here
# instead of silently skipping. `^    fi$` is the block's own closing fi — its
# inner ones are indented deeper.
INC_BLOCK="${WORK}/niri-include-block.sh"
sed -n '/^    # ── 6a\. the generated configs must actually be INCLUDED/,/^    fi$/p' \
    "$FIRSTRUN" > "$INC_BLOCK"
PRE_BLOCK="${WORK}/niri-precreate-block.sh"
sed -n '/^\[ -f "${CFG_DIR}\/RimeShellInput.kdl" \]/p' "$FIRSTRUN" > "$PRE_BLOCK"

# ── the one thing the block needs from OUTSIDE itself ────────────────────────
# The block calls niri through ${NIRI_BIN}. That resolution used to live inside
# block 6a, on purpose — 2605db27's message says "defined inside 6a so the block
# stays self-contained for the suite that extracts and runs it". 37497975 then
# added block 6a-pre (disable niri's stock waybar spawn) above it and moved the
# resolution up to share it, which is correct for the provisioner and silently
# took the line out of the sed range above. The extracted block then ran under
# `set -u` with NIRI_BIN unbound, died on its first validate, and SEVEN
# assertions below went red with no named cause.
#
# So the harness takes the SHIPPED line rather than restating the resolution —
# a change to how niri is resolved is still felt here — and the two assertions
# under it pin the invariant that actually matters at runtime: the line exists
# exactly once, and it is assigned before the first use.
NIRI_BIN_LINE="${WORK}/niri-bin-line.sh"
sed -n '/^ *NIRI_BIN="\$(command -v niri/p' "$FIRSTRUN" > "$NIRI_BIN_LINE"
nbl="$(grep -c . "$NIRI_BIN_LINE")"
[ "$nbl" = 1 ] \
    && ok "the provisioner resolves NIRI_BIN exactly once" \
    || bad "the provisioner resolves NIRI_BIN exactly once (found ${nbl})"
nbl_ln="$(grep -n '^ *NIRI_BIN="\$(command -v niri' "$FIRSTRUN" | cut -d: -f1 | head -1)"
use_ln="$(grep -n '"\${NIRI_BIN}" validate --config' "$FIRSTRUN" | cut -d: -f1 | head -1)"
if [ -n "$nbl_ln" ] && [ -n "$use_ln" ] && [ "$nbl_ln" -lt "$use_ln" ]; then
    ok "NIRI_BIN is resolved before its first use (${nbl_ln} < ${use_ln})"
else
    bad "NIRI_BIN is resolved before its first use (${nbl_ln:-?} < ${use_ln:-?})"
fi

if [ ! -s "$INC_BLOCK" ]; then
    bad "the provisioner's niri include block is where this suite drives it"
else
    ok "the provisioner's niri include block is where this suite drives it"
    bash -n "$INC_BLOCK" \
        && ok "the extracted block is self-contained bash" \
        || bad "the extracted block is self-contained bash"

    # Step 6's own guard is `command -v niri || [ -x /usr/bin/niri ]`, so it
    # admits a niri that exists but is not on PATH — which is ordinary for a
    # per-user systemd unit. A bare `niri` there exits 127, `! niri validate`
    # reads 127 as "invalid", and the block takes its refusal branch and never
    # writes the includes: silently, on every login, forever. Every validate
    # call must therefore go through the resolved binary, and this counts them
    # rather than trusting one to have been noticed in review.
    nb="$(grep -c '"${NIRI_BIN}" validate --config' "$INC_BLOCK")"
    va="$(grep -c 'validate --config' "$INC_BLOCK")"
    if [ "$nb" = "$va" ] && [ "$nb" -ge 2 ]; then
        ok "all ${nb} validate calls resolve the binary instead of assuming PATH"
    else
        bad "all validate calls resolve the binary instead of assuming PATH (${nb}/${va})"
    fi
fi

[ -s "$PRE_BLOCK" ] \
    && ok "RimeShellInput.kdl has a pre-create in the provisioner" \
    || bad "RimeShellInput.kdl has a pre-create in the provisioner"

# Order is the whole safety argument, not a detail. The append refuses to write
# an include whose target is missing, and the pre-create is what guarantees it
# is not — so a pre-create that ran AFTER the append would leave the include
# permanently unwritten, and moving the append earlier would be the booby trap
# this change exists to remove. Nothing between them exits early.
pre_ln="$(grep -n '^\[ -f "${CFG_DIR}/RimeShellInput.kdl" \]' "$FIRSTRUN" | cut -d: -f1)"
inc_ln="$(grep -n '# ── 6a\. the generated configs must actually be INCLUDED' "$FIRSTRUN" | cut -d: -f1)"
if [ -n "$pre_ln" ] && [ -n "$inc_ln" ] && [ "$pre_ln" -lt "$inc_ln" ]; then
    ok "the pre-create runs before the include is appended (${pre_ln} < ${inc_ln})"
else
    bad "the pre-create runs before the include is appended (${pre_ln:-?} < ${inc_ln:-?})"
fi
[ -z "$(awk -v a="${pre_ln:-0}" -v b="${inc_ln:-0}" \
        'NR>a && NR<b && /^[[:space:]]*exit[[:space:]]/' "$FIRSTRUN")" ] \
    && ok "nothing between them exits the provisioner early" \
    || bad "nothing between them exits the provisioner early"

# ── path agreement, derived from source on both sides ────────────────────────
# Hardcoding the two paths here would pass forever after a rename on either
# side. The Hyprland equivalent of this check in test-rime-firstrun.sh carries
# the note that forgetting it "has already happened once per file added".
inc_paths="$(sed -n 's|^include "\${CFG_DIR}/\(.*\)"$|\1|p' "$INC_BLOCK")"
[ "$(printf '%s\n' "$inc_paths" | grep -c .)" = 2 ] \
    && ok "the block includes exactly two generated files" \
    || bad "the block includes exactly two generated files (got: ${inc_paths})"

# rime-input-apply's own NIRI_OUT, read out of the shipped script.
gen_out="$(python3 - "$GEN" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
m = re.search(r'^NIRI_OUT = os\.path\.join\(HOME, "(.*)"\)$', src, re.M)
print(m.group(1) if m else "")
PY
)"
[ -n "$gen_out" ] \
    && ok "rime-input-apply declares NIRI_OUT" || bad "rime-input-apply declares NIRI_OUT"
printf '%s\n' "$inc_paths" | has -xF "${gen_out#.config/rime-shell/}" \
    && ok "the include path is exactly the file rime-input-apply writes" \
    || bad "the include path is exactly the file rime-input-apply writes (${gen_out})"

# The keybind half is generated by Rime Shell, so the agreement is with the
# shell tree when one is available. Same lookup order as test-rime-firstrun.sh.
SHELL_TREE=""
for cand in "${ROOT}/../rime-shell" /usr/share/rime-shell; do
    [ -f "${cand}/src/services/config_tab/KeybindService.qml" ] && { SHELL_TREE="$cand"; break; }
done
if [ -z "$SHELL_TREE" ]; then
    skp "no rime-shell tree: cannot cross-check the keybind include path"
else
    kdl_name="$(sed -n 's|.*_kdlPath: *_configDir + "/\([^"]*\)".*|\1|p' \
        "${SHELL_TREE}/src/services/config_tab/KeybindService.qml" | head -1)"
    if [ -z "$kdl_name" ]; then
        bad "KeybindService declares _kdlPath"
    else
        printf '%s\n' "$inc_paths" | has -xF "$kdl_name" \
            && ok "the include path is exactly the file the shell writes (${kdl_name})" \
            || bad "the include path is exactly the file the shell writes (${kdl_name})"
    fi
fi

# ── driving the real block ───────────────────────────────────────────────────
# `niri validate` is a decision point inside the block (refuse a config that was
# already broken; restore the backup if the includes break one that was not), so
# the append logic is exercised against a STUB that answers on demand. That is
# the seam, not a re-implementation: the stub decides nothing about KDL, it only
# returns the verdict the case under test needs. Real niri runs further down.
STUBBIN="${WORK}/stubbin"
mkdir -p "$STUBBIN"
cat > "${STUBBIN}/niri" <<'STUB'
#!/usr/bin/env bash
exit "${STUB_NIRI_EXIT:-0}"
STUB
chmod 0755 "${STUBBIN}/niri"

# Refuse to run the stubbed cases if the stub is not what resolves — otherwise
# a machine with real niri would quietly test something else.
if [ "$(PATH="${STUBBIN}" command -v niri)" != "${STUBBIN}/niri" ]; then
    bad "the niri stub is what resolves for the stubbed cases"
else
    ok "the niri stub is what resolves for the stubbed cases"
fi

# A throwaway HOME with a seeded niri config and both include targets present,
# exactly as the provisioner leaves them by the time block 6a runs.
mkniri() {
    local h="$1"
    mkdir -p "$h/.config/niri" "$h/.config/rime-shell"
    if [ -f /usr/share/doc/niri/default-config.kdl ]; then
        cp /usr/share/doc/niri/default-config.kdl "$h/.config/niri/config.kdl"
    else
        printf 'input {\n    touchpad {\n        tap\n    }\n}\n' > "$h/.config/niri/config.kdl"
    fi
    printf '\n// Rime Shell autostarts (seeded by rime-shell-firstrun)\nspawn-at-startup "/usr/libexec/rime-shell-autostart"\n' \
        >> "$h/.config/niri/config.kdl"
    : > "$h/.config/rime-shell/RimeShellInput.kdl"
    : > "$h/.config/rime-shell/RimeShellKeybinds.kdl"
}

# The block reads HOME, CFG_DIR, NIRI_CONF, log() and NIRI_BIN. The harness
# supplies the first four and nothing else; NIRI_BIN comes from the shipped
# resolution line extracted above, sourced first, so the binary is still
# resolved by the provisioner's own code and nothing here can drift from it.
run_inc() {
    local h="$1" p="${2:-$PATH}"
    HOME="$h" CFG_DIR="$h/.config/rime-shell" NIRI_CONF="$h/.config/niri/config.kdl" \
    PATH="$p" bash -c 'set -euo pipefail; log() { printf "%s\n" "$*"; }
                       source "$1"; source "$2"' \
        -- "$NIRI_BIN_LINE" "$INC_BLOCK" 2>&1
}

section "niri: the include line is appended, and nothing else changes"
h="${WORK}/niri-ok"; mkniri "$h"
NC="$h/.config/niri/config.kdl"
before_lines="$(wc -l < "$NC")"
before_sum="$(sha256sum < "$NC" | cut -d' ' -f1)"
before_bytes="$(wc -c < "$NC")"
out="$(run_inc "$h" "${STUBBIN}:${PATH}")"
printf '%s\n' "$out" | sed 's/^/      /'

# FIRST, because everything under it is downstream of it. The block is sourced
# under `set -u`, so anything the provisioner assigns OUTSIDE the extracted
# range kills it before the append and turns one defect into seven red lines
# that each look like a different bug. This names it in one.
printf '%s\n' "$out" | has 'unbound variable' \
    && bad "the block ran with everything it needs (nothing it uses is assigned outside the extracted range)" \
    || ok "the block ran with everything it needs (nothing it uses is assigned outside the extracted range)"

grep -qF "include \"$h/.config/rime-shell/RimeShellInput.kdl\"" "$NC" \
    && ok "the generated input config is included" || bad "the generated input config is included"
grep -qF "include \"$h/.config/rime-shell/RimeShellKeybinds.kdl\"" "$NC" \
    && ok "the generated keybind config is included" || bad "the generated keybind config is included"

# Grepping for what was added cannot detect what was deleted, so the whole
# former content must still be there byte-for-byte, at the front.
[ "$(head -c "$before_bytes" "$NC" | sha256sum | cut -d' ' -f1)" = "$before_sum" ] \
    && ok "every byte the user already had is untouched" \
    || bad "every byte the user already had is untouched"
[ "$(wc -l < "$NC")" -eq "$((before_lines + 6))" ] \
    && ok "exactly six lines were added" \
    || bad "exactly six lines were added ($before_lines -> $(wc -l < "$NC"))"
grep -qF 'spawn-at-startup "/usr/libexec/rime-shell-autostart"' "$NC" \
    && ok "the autostart landmark survives" || bad "the autostart landmark survives"
[ -f "${NC}.pre-include.bak" ] \
    && ok "a backup was taken before the edit" || bad "a backup was taken before the edit"

run_inc "$h" "${STUBBIN}:${PATH}" >/dev/null 2>&1
[ "$(grep -c '^include ' "$NC")" = 2 ] \
    && ok "re-running adds nothing (its own marker, not the autostart block's)" \
    || bad "re-running adds nothing (its own marker, not the autostart block's)"

section "niri: an include is never written for a file that is not there"
# A missing include target is a HARD parse error in niri and takes the whole
# config with it, so this refusal is the difference between a partial feature
# and a session with no keybinds at all.
h="${WORK}/niri-missing"; mkniri "$h"
rm -f "$h/.config/rime-shell/RimeShellInput.kdl"
NC="$h/.config/niri/config.kdl"
sum="$(sha256sum < "$NC" | cut -d' ' -f1)"
out="$(run_inc "$h" "${STUBBIN}:${PATH}")"
printf '%s\n' "$out" | has 'include targets are missing' \
    && ok "it refuses and says which way it refused" || bad "it refuses and says which way it refused"
[ "$(sha256sum < "$NC" | cut -d' ' -f1)" = "$sum" ] \
    && ok "the config is left exactly as it was" || bad "the config is left exactly as it was"

section "niri: a config that was already broken is not touched"
h="${WORK}/niri-broken"; mkniri "$h"
NC="$h/.config/niri/config.kdl"
sum="$(sha256sum < "$NC" | cut -d' ' -f1)"
out="$(STUB_NIRI_EXIT=1 run_inc "$h" "${STUBBIN}:${PATH}")"
printf '%s\n' "$out" | has 'refusing to touch it' \
    && ok "it refuses and says so" || bad "it refuses and says so"
[ "$(sha256sum < "$NC" | cut -d' ' -f1)" = "$sum" ] \
    && ok "a pre-broken config is left exactly as it was" \
    || bad "a pre-broken config is left exactly as it was"

section "niri: if the includes break the config, the backup goes back"
# Deterministic only with a stub: valid on the first call, rejected on the
# second. Real niri will never produce this, which is exactly why the restore
# path would otherwise never be executed by anything.
h="${WORK}/niri-restore"; mkniri "$h"
NC="$h/.config/niri/config.kdl"
sum="$(sha256sum < "$NC" | cut -d' ' -f1)"
cat > "${STUBBIN}/niri" <<'STUB'
#!/usr/bin/env bash
c="${STUB_NIRI_COUNT:-/dev/null}"
n=0; [ -f "$c" ] && n="$(cat "$c")"
n=$((n + 1)); [ "$c" = /dev/null ] || printf '%s' "$n" > "$c"
[ "$n" -ge 2 ] && exit 1
exit 0
STUB
chmod 0755 "${STUBBIN}/niri"
out="$(STUB_NIRI_COUNT="${WORK}/niri-count" run_inc "$h" "${STUBBIN}:${PATH}")"
printf '%s\n' "$out" | has 'restored the backup' \
    && ok "it restores and says so" || bad "it restores and says so"
[ "$(sha256sum < "$NC" | cut -d' ' -f1)" = "$sum" ] \
    && ok "the restored config is byte-identical to the original" \
    || bad "the restored config is byte-identical to the original"

section "niri: the compositor really reads what the generator wrote"
# The only assertion that can tell a live include from a decorative one. It
# needs the real binary; there is none on the CI runner, so this skips there and
# runs on any Rime machine and in the image build (Containerfile.base).
if ! command -v niri >/dev/null 2>&1; then
    # A legitimate skip, of the same kind test-secret-broker.sh keeps for
    # bubblewrap: without the real binary this property cannot be tested at
    # all, and reporting "failed" would misdescribe what was checked. niri is
    # not installable on the ubuntu-24.04 runner these suites run on.
    #
    # But a skip that nothing can escalate is how a green tick comes to sit
    # over an assertion that never ran, so RIME_REQUIRE_NIRI refuses it. Set it
    # anywhere niri IS expected — a Rime machine, the image build — and a lost
    # niri becomes a failure instead of three silent skips.
    #
    # What still holds without it: Containerfile.base runs
    # `rime-input-apply --self-test`, which since this change validates the
    # generated file THROUGH an `include` against the image's own niri, and the
    # base guard next to it proves that niri has includes and that a missing
    # target is fatal. What is NOT covered anywhere else is the part below —
    # the provisioner appending to a real config.kdl, and a broken include
    # being rejected.
    if [ -n "${RIME_REQUIRE_NIRI:-}" ]; then
        bad "niri is present (RIME_REQUIRE_NIRI is set)"
        printf '      niri is missing, so whether config.kdl actually READS the\n'
        printf '      generated file cannot be tested. Refusing to skip.\n'
        printf '\nrime-input: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
        exit 1
    fi
    skp "niri unavailable: the include-reachability proof did not run"
    skp "niri unavailable: a broken include is not proven to be rejected"
    skp "niri unavailable: the generated input values are not proven reachable"
else
    h="${WORK}/niri-live"; mkniri "$h"
    NC="$h/.config/niri/config.kdl"
    run_inc "$h" >/dev/null 2>&1
    niri validate --config "$NC" >/dev/null 2>&1 \
        && ok "niri accepts config.kdl with both includes" \
        || bad "niri accepts config.kdl with both includes"

    # THE assertion. If niri still validates a config whose included file is
    # garbage, the include is not being read and every check above is theatre.
    printf 'this-is-not-a-niri-node\n' > "$h/.config/rime-shell/RimeShellInput.kdl"
    niri validate --config "$NC" >/dev/null 2>&1 \
        && bad "a broken include is rejected — proving config.kdl really reads it" \
        || ok "a broken include is rejected — proving config.kdl really reads it"

    # And the real generator's output, reached the same way: settings written by
    # rime-input-apply arrive through the include the provisioner added.
    mkdir -p "$h/.config/labwc" "$h/.config/hypr"
    cp "${TMPL}/rc.xml" "${TMPL}/menu.xml" "$h/.config/labwc/"
    sed 's/@ACCENT@/#D9F99D/g' "${TMPL}/themerc-override" > "$h/.config/labwc/themerc-override"
    printf '{"keyboard":{"repeat_rate":42}}\n' > "$h/.config/rime-shell/input.json"
    run_gen "$h" >/dev/null 2>&1
    # The include line is part of the condition on purpose: `niri validate` +
    # a grep of the GENERATED file both pass with no include at all, so without
    # it this assertion would go green while the setting reached nothing.
    if niri validate --config "$NC" >/dev/null 2>&1 \
       && grep -qF "include \"$h/.config/rime-shell/RimeShellInput.kdl\"" "$NC" \
       && grep -q 'repeat-rate 42' "$h/.config/rime-shell/RimeShellInput.kdl"; then
        ok "a setting changed in Settings reaches niri through the include"
    else
        bad "a setting changed in Settings reaches niri through the include"
    fi
fi

printf '\nrime-input: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
