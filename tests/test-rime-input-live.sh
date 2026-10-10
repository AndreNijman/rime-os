#!/usr/bin/env bash
# The input settings pipeline, in a REAL running Hyprland.
#
# tests/test-rime-input.sh proves the generator writes what each compositor's
# validator accepts. It cannot prove the two things a user actually cares about:
#
#   * that changing a setting REACHES the running compositor. The supported
#     runtime path under a Lua config is `hyprctl reload`, and Lua caches
#     modules, so "the file was written" and "the compositor has the value" are
#     different claims;
#   * that the option names the page reads back with still exist. This is the
#     sharpest assertion in the file: `hyprctl getoption` answers "no such
#     option" for a name it does not know, and a read-back that treats that as
#     "unset" would report a working control as untouched forever. Hyprland
#     spells two of them with hyphens — tap-to-click and tap-and-drag — where
#     the Lua config keys are underscored, so this is not hypothetical.
#
# ── How this avoids touching the developer's session ─────────────────────────
# Same shape as tests/test-rime-hypr-lua.sh: labwc on the wlroots HEADLESS
# backend creates a Wayland socket with no visible output anywhere, Hyprland is
# nested inside it with its own XDG_RUNTIME_DIR, HOME and instance signature.
# Nothing is drawn, and no hyprctl in here can reach the real compositor —
# the signature it would need is not in this script's environment.
#
# WHAT THIS CANNOT PROVE. A headless nested Hyprland has no input devices, so
# nothing here observes a real touchpad changing behaviour, and the hl.device
# blocks are accepted rather than matched. Device enumeration and classification
# are tested against real hardware in tests/test-rime-input.sh; what happens
# when a finger touches the pad is not tested by anything and cannot be.
#
# Skips cleanly (status 0) when labwc or Hyprland is missing.
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
GEN="${RIME_INPUT_GEN:-${root}/files/system/libexec/rime-input-apply}"

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }

finish() {
    printf '\nrime input-live: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
    [ "$fail" -eq 0 ]
}

for tool in labwc Hyprland hyprctl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is not installed; cannot run a nested compositor"
        finish; exit $?
    fi
done

TMPL="${root}/files/desktop/hypr/hyprland.lua"
MODULES="${root}/files/desktop/hypr/rime"
[ -f "$GEN" ] || { printf 'missing %s\n' "$GEN" >&2; exit 1; }

RT="$(mktemp -d)"; chmod 0700 "$RT"
H="$(mktemp -d)"
LABWC_LOG="$(mktemp)"; HYPR_LOG="$(mktemp)"
LABWC_PID=""; HYPR_PID=""

cleanup() {
    [ -n "$HYPR_PID" ]  && kill "$HYPR_PID"  2>/dev/null
    sleep 1
    [ -n "$LABWC_PID" ] && kill "$LABWC_PID" 2>/dev/null
    wait 2>/dev/null
    rm -rf "$RT" "$H" "$LABWC_LOG" "$HYPR_LOG"
}
trap cleanup EXIT

mkdir -p "$H/.config/labwc" "$H/.config/hypr/rime" "$H/.config/rime-shell"
: > "$H/.config/labwc/autostart"
printf '<?xml version="1.0"?>\n<labwc_config></labwc_config>\n' > "$H/.config/labwc/rc.xml"

render() { sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$1"; }
render "$TMPL" > "$H/.config/hypr/hyprland.lua"
for f in "$MODULES"/*.lua; do
    render "$f" > "$H/.config/hypr/rime/$(basename "$f")"
done

# No devices, so what is asserted below is the global block and nothing that
# depends on whatever is plugged into the machine running this.
DEVFIX="${H}/devices.json"; echo '[]' > "$DEVFIX"

env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
    labwc > "$LABWC_LOG" 2>&1 &
LABWC_PID=$!

host=""
for _ in $(seq 1 40); do
    for f in "$RT"/wayland-*; do
        [ -S "$f" ] && { host="$(basename "$f")"; break; }
    done
    [ -n "$host" ] && break
    sleep 0.3
done
if [ -z "$host" ]; then
    skip "the headless host compositor did not come up"
    tail -5 "$LABWC_LOG"
    finish; exit $?
fi
ok "a headless host compositor is running (nothing is drawn on this machine)"

env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
    WAYLAND_DISPLAY="$host" XDG_CURRENT_DESKTOP=Hyprland \
    Hyprland --i-am-really-stupid > "$HYPR_LOG" 2>&1 &
HYPR_PID=$!

sock=""
for _ in $(seq 1 60); do
    for f in "$RT"/hypr/*/.socket.sock; do
        [ -S "$f" ] && { sock="$f"; break; }
    done
    [ -n "$sock" ] && break
    sleep 0.5
done
if [ -z "$sock" ]; then
    skip "the nested Hyprland did not come up"
    tail -8 "$HYPR_LOG"
    finish; exit $?
fi
SIG="$(basename "$(dirname "$sock")")"
ok "Hyprland is running nested, with its own instance signature"

# Every call in here carries the NESTED signature and the fixture HOME, so
# nothing can reach the developer's compositor or read their settings.
hc() {
    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl "$@" 2>&1
}
# shellcheck disable=SC2120  # `apply` mirrors `hc` above it and forwards "$@"
# to the generator. No case needs a generator flag today, which is the true half
# of the warning; the forward is what lets one be added without editing the
# helper, and dropping it would make the two helpers differ for no reason.
apply() {
    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        HYPRLAND_INSTANCE_SIGNATURE="$SIG" XDG_CURRENT_DESKTOP=Hyprland \
        RIME_INPUT_DEVICES="$DEVFIX" python3 "$GEN" "$@" 2>&1
}

sec "every option the page reads back with still exists"

# The assertion the read-back's honesty rests on. `hyprctl getoption` answers
# "no such option" for a name it does not know, and the read-back would report
# that as "the compositor was never told" — a control that works, shown as
# untouched, forever. Two of these are spelled with hyphens where the Lua
# config keys are underscored, which is exactly how such a mistake is made.
PATHS="$(python3 - "$GEN" <<'PY'
import importlib.machinery, importlib.util, sys
spec = importlib.util.spec_from_loader(
    "gen", importlib.machinery.SourceFileLoader("gen", sys.argv[1]))
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)
print("\n".join(gen.HYPR_GETOPTION.values()))
PY
)"
# An empty list is a failure, not a clean sweep. Without this, a generator with
# no read-back table at all sails through the loop below with nothing missing
# and the section reports PASS for zero assertions.
count="$(printf '%s\n' "$PATHS" | grep -c .)"
if [ "${count:-0}" -lt 15 ]; then
    bad "the read-back's option table is readable (found ${count:-0} paths, expected 15+)"
else
    ok "the read-back's option table lists ${count} options to check"
    missing=""
    while read -r path; do
        [ -z "$path" ] && continue
        if hc getoption "$path" | pipe_has -i 'no such option'; then
            missing="${missing} ${path}"
        fi
    done <<EOF
${PATHS}
EOF
    if [ -z "$missing" ]; then
        ok "Hyprland knows every option name the read-back asks for"
    else
        bad "Hyprland does not know:${missing}"
    fi
fi

sec "a setting written by the page reaches the running compositor"

cat > "$H/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "tap": false, "natural_scroll": false, "tap_and_drag": false,
                "drag_lock": false, "three_finger_drag": true,
                "tap_button_map": "lmr", "scroll_factor": 2.5 },
  "pointer":  { "speed": 0.6, "accel_profile": "flat", "left_handed": true },
  "keyboard": { "repeat_rate": 42, "repeat_delay": 275 } }
JSON

# WITH the reload, because that is the supported runtime path under Lua and the
# thing being tested. It reaches only the nested instance.
out="$(apply)"
printf '%s\n' "$out" | sed 's/^/      /'
[ -s "$H/.config/hypr/rime/input.lua" ] \
    && ok "the generator wrote the Lua module" || bad "the generator wrote the Lua module"

errs="$(hc configerrors | tr -d '[:space:]')"
if [ -z "$errs" ] || [ "$errs" = "noerrors" ]; then
    ok "hyprctl configerrors is clean after the apply"
else
    bad "hyprctl configerrors is clean after the apply — got: $(hc configerrors | head -5)"
fi

want() { # path expected description
    local raw got
    raw="$(hc getoption "$1")"
    # Hyprland prints the type it holds the option as — int, float, str or bool
    # — and which one it picks is not obvious from the config key: drag_3fg is
    # an int and tap-to-click is a bool. Accept all four and normalise, rather
    # than encode a guess per option.
    got="$(printf '%s\n' "$raw" | sed -n 's/^\(int\|float\|str\|bool\): //p' | head -1 | tr -d '[:space:]')"
    case "$got" in true) got=1 ;; false) got=0 ;; esac
    case "$1" in
        *sensitivity|*scroll_factor) got="$(printf '%.2f' "${got:-0}")" ;;
    esac
    if [ "$got" = "$2" ]; then
        ok "$3"
    else
        bad "$3 (got '${got}', wanted '$2'; hyprctl said: $(printf '%s' "$raw" | head -1))"
    fi
}

# Each of these was false or unset a moment ago. That the compositor now reports
# the model's value is the whole of "changing Input configuration has an effect".
want input:touchpad:tap-to-click 0    "tap-to-click reached the compositor"
want input:touchpad:tap-and-drag 0    "tap-and-drag reached the compositor"
want input:touchpad:natural_scroll 0  "natural scrolling reached the compositor"
want input:touchpad:drag_3fg 1        "three-finger drag reached the compositor"
want input:touchpad:tap_button_map lmr "the tap button map reached the compositor"
want input:touchpad:scroll_factor 2.50 "the touchpad scroll factor reached the compositor"
want input:sensitivity 0.60           "pointer speed reached the compositor"
want input:accel_profile flat         "the acceleration profile reached the compositor"
want input:left_handed 1              "left-handed reached the compositor"
want input:repeat_rate 42             "the key repeat rate reached the compositor"
want input:repeat_delay 275           "the key repeat delay reached the compositor"

sec "a second change is picked up too"

# Lua caches modules in package.loaded. A loader that only require()d would
# serve what was on disk at login, so the FIRST apply could pass by accident on
# a cold read and every later one silently do nothing.
cat > "$H/.config/rime-shell/input.json" <<'JSON'
{ "touchpad": { "tap": true, "scroll_factor": 0.5 }, "keyboard": { "repeat_rate": 77 } }
JSON
apply >/dev/null 2>&1
want input:touchpad:tap-to-click 1     "a second apply is picked up (tap back on)"
want input:touchpad:scroll_factor 0.50 "a second apply is picked up (scroll factor)"
want input:repeat_rate 77              "a second apply is picked up (repeat rate)"

sec "the page reads back what the compositor reports, not what it asked for"

rb="$(env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
      HYPRLAND_INSTANCE_SIGNATURE="$SIG" XDG_CURRENT_DESKTOP=Hyprland \
      RIME_INPUT_DEVICES="$DEVFIX" \
      python3 "$GEN" --read-back --model "$H/.config/rime-shell/input.json" 2>/dev/null)"
printf '%s' "$rb" > "${H}/readback.json"
why="$(python3 - "${H}/readback.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
v = d["values"]
wrong = []
if d["compositor"] != "hyprland":
    wrong.append("compositor is " + str(d["compositor"]))
if v["touchpad.tap"]["source"] != "compositor":
    wrong.append("tap did not come from the compositor")
if v["touchpad.tap"]["value"] is not True:
    wrong.append("tap reads " + repr(v["touchpad.tap"]["value"]))
if abs(v["touchpad.scroll_factor"]["value"] - 0.5) > 1e-4:
    wrong.append("scroll factor reads " + repr(v["touchpad.scroll_factor"]["value"]))
if v["keyboard.repeat_rate"]["value"] != 77:
    wrong.append("repeat rate reads " + repr(v["keyboard.repeat_rate"]["value"]))
if d["diverged"]:
    wrong.append("diverged: " + ", ".join(x["control"] for x in d["diverged"]))
print("; ".join(wrong))
PY
)"
if [ -z "$why" ]; then
    ok "the read-back agrees with the compositor and reports no divergence"
else
    bad "the read-back agrees with the compositor and reports no divergence — ${why}"
fi

# And it must NOT agree when the compositor disagrees. Push a value past the
# generator, straight into the running instance, and the read-back has to
# notice — otherwise it is echoing the model back at itself.
hc eval 'hl.config({ input = { repeat_rate = 11 } })' >/dev/null
env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
    HYPRLAND_INSTANCE_SIGNATURE="$SIG" XDG_CURRENT_DESKTOP=Hyprland \
    RIME_INPUT_DEVICES="$DEVFIX" \
    python3 "$GEN" --read-back --model "$H/.config/rime-shell/input.json" 2>/dev/null \
  | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if any(x["control"] == "keyboard.repeat_rate" and x["effective"] == 11
                  for x in d["diverged"]) else 1)' \
    && ok "a value changed under the page is reported as diverged" \
    || bad "a value changed under the page is reported as diverged"

finish
