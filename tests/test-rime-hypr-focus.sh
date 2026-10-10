#!/usr/bin/env bash
# What SUPER+arrow actually does, in a real Hyprland running the seeded config.
#
#     tests/test-rime-hypr-focus.sh
#
# ── The question ─────────────────────────────────────────────────────────────
#
# Andre: "make it so that super+arrow actually moves the mouse so that the
# window you move to actually stays in focus instead of instantly losing focus".
#
# That is a behavioural claim, and a suite asserting `cursor { no_warps = false }`
# appears in a Lua file would not test it. The seeded session focus-follows-mouse
# (`follow_mouse = 1`), so a keyboard focus change that leaves the pointer behind
# is undone by the next movement of the mouse: the window is focused only until
# somebody touches anything. Three things are asked of a live instance here:
#
#   1. directional focus moves focus to the window in that direction;
#   2. the POINTER ends up inside that window;
#   3. the window is still focused after a one-pixel pointer nudge — which is
#      the "instantly loses focus" the report was about.
#
# And the negative control, which is the part that makes the other three mean
# something: the same run with `no_warps = true` must FAIL (2) and (3). A gate
# that passes whatever the configuration says is the dominant defect family in
# this tree; this one is run both ways every time.
#
# ── Keys are not pressed here, and that is measured rather than assumed ──────
#
# Measured on this machine, 2026-09-20: wtype's virtual-keyboard-v1 device is
# accepted by Hyprland 0.56.2 — it appears in `hyprctl devices` as
# hl-virtual-keyboard-wtype — and Hyprland then reports `active keymap: error`
# for it, and no keybind fires from it at all. A wlroots compositor takes the
# same keymap and dispatches normally. Aquamarine's own headless backend
# core-dumps in CBackend::create() on a box with a GPU and no DRM master, so
# there is no Hyprland anywhere here that a synthetic keyboard can drive.
#
# So this suite splits the question in two and checks both halves:
#
#   * the BIND TABLE — `hyprctl binds` — must carry exactly one entry per arrow
#     combo, which is the half a synthetic keypress would have caught. The live
#     L16 has `movefocus l` bound TWICE for SUPER+Left (the hyprlang seed's
#     lowercase `left` plus the shell's generated uppercase `LEFT`, whose
#     `unbind` is case-sensitive and removed nothing), so one press moves focus
#     two windows. The Lua modules disable by handle through an uppercasing key
#     function, which is what makes that impossible here — and unasserted, it
#     would come back.
#
#   * the DISPATCHER — what the bind runs — must produce the behaviour above.
#
# ── Nothing is drawn on the machine running this ─────────────────────────────
#
# labwc on the wlroots headless backend, Hyprland nested inside it, both in a
# runtime directory and HOME this script created. Every hyprctl call passes the
# nested signature explicitly and the script refuses to run if that signature is
# the ambient session's.
#
# Skips cleanly (status 0) when labwc, Hyprland or quickshell is missing.
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }

finish() {
    printf '\nrime hypr-focus: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
    [ "$fail" -eq 0 ]
}

for tool in labwc Hyprland hyprctl quickshell python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is not installed; cannot run a nested compositor"
        finish; exit $?
    fi
done

TMPL="${root}/files/desktop/hypr/hyprland.lua"
MODULES="${root}/files/desktop/hypr/rime"
if [ ! -f "$TMPL" ] || [ ! -d "$MODULES" ]; then
    bad "the seeded Hyprland tree is present"
    finish; exit $?
fi

AMBIENT_SIG="${HYPRLAND_INSTANCE_SIGNATURE:-}"

RT=""; H=""; LABWC_PID=""; HYPR_PID=""; WIN_PIDS=""
LABWC_LOG=""; HYPR_LOG=""

cleanup() {
    local p
    for p in $WIN_PIDS; do kill "$p" 2>/dev/null || :; done
    [ -n "$HYPR_PID" ]  && kill "$HYPR_PID"  2>/dev/null || :
    sleep 1 || :
    [ -n "$LABWC_PID" ] && kill "$LABWC_PID" 2>/dev/null || :
    sleep 0.3 || :
    [ -n "$HYPR_PID" ]  && kill -9 "$HYPR_PID"  2>/dev/null || :
    [ -n "$LABWC_PID" ] && kill -9 "$LABWC_PID" 2>/dev/null || :
    [ -n "$RT" ] && rm -rf "$RT" || :
    [ -n "$H" ]  && rm -rf "$H"  || :
    rm -f "$LABWC_LOG" "$HYPR_LOG" 2>/dev/null || :
    return 0
}
trap cleanup EXIT INT TERM

# ─────────────────────────────────────────────────────────────────────────────
#  One run of the whole thing, at a given no_warps.
#
#  Everything is torn down and rebuilt between the two runs rather than
#  `hyprctl keyword`-ing the option: keyword REFUSES against a Lua config
#  ("keyword can't work with non-legacy parsers"), and a control that silently
#  changed nothing would make the negative run agree with the positive one and
#  look like proof.
# ─────────────────────────────────────────────────────────────────────────────
ACTIVE_AFTER=""; CURSOR_AFTER=""; TARGET_GEO=""; ACTIVE_NUDGED=""; BIND_DUPES=""
ACTIVE_CROSSED=""; OTHER_TITLE=""

run_session() {
    local no_warps="$1"
    ACTIVE_AFTER=""; CURSOR_AFTER=""; TARGET_GEO=""; ACTIVE_NUDGED=""; BIND_DUPES=""
    ACTIVE_CROSSED=""; OTHER_TITLE=""

    RT="$(mktemp -d)"; chmod 0700 "$RT"
    H="$(mktemp -d)"
    LABWC_LOG="$(mktemp)"; HYPR_LOG="$(mktemp)"

    mkdir -p "$H/.config/labwc" "$H/.config/hypr/rime"
    : > "$H/.config/labwc/autostart"
    printf '<?xml version="1.0"?>\n<labwc_config></labwc_config>\n' > "$H/.config/labwc/rc.xml"

    # The SHIPPED modules, with only the two seed placeholders resolved — the
    # same substitution rime-shell-firstrun makes.
    local f
    sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$TMPL" > "$H/.config/hypr/hyprland.lua"
    for f in "$MODULES"/*.lua; do
        sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$f" \
            > "$H/.config/hypr/rime/$(basename "$f")"
    done

    # The negative control. Appended AFTER the Rime modules so it wins, and
    # written as its own hl.config call so the shipped file is untouched.
    if [ "$no_warps" = "true" ]; then
        printf '\nhl.config({ cursor = { no_warps = true } })\n' \
            >> "$H/.config/hypr/hyprland.lua"
    fi
    # Gaps off, so two tiled windows are adjacent and a warp to the middle of
    # one is unambiguously inside it.
    printf '\nhl.config({ general = { gaps_in = 0, gaps_out = 0 } })\n' \
        >> "$H/.config/hypr/hyprland.lua"

    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        labwc > "$LABWC_LOG" 2>&1 &
    LABWC_PID=$!

    local host="" _i f2
    for _i in $(seq 1 40); do
        for f2 in "$RT"/wayland-*; do
            [ -S "$f2" ] || continue
            case "$(basename "$f2")" in wayland-[0-9]) : ;; *) continue ;; esac
            host="$(basename "$f2")"; break
        done
        [ -n "$host" ] && break
        sleep 0.3
    done
    [ -n "$host" ] || { skip "the headless host compositor did not come up"; tail -5 "$LABWC_LOG"; return 1; }

    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        WAYLAND_DISPLAY="$host" XDG_CURRENT_DESKTOP=Hyprland \
        Hyprland --i-am-really-stupid > "$HYPR_LOG" 2>&1 &
    HYPR_PID=$!

    local sock=""
    for _i in $(seq 1 60); do
        for f2 in "$RT"/hypr/*/.socket.sock; do
            [ -S "$f2" ] && { sock="$f2"; break; }
        done
        [ -n "$sock" ] && break
        sleep 0.5
    done
    [ -n "$sock" ] || { skip "the nested Hyprland did not come up"; tail -8 "$HYPR_LOG"; return 1; }
    SIG="$(basename "$(dirname "$sock")")"

    # A nested instance reporting the host's signature is not nested, and every
    # dispatch below would then be aimed at the desk.
    if [ -n "$AMBIENT_SIG" ] && [ "$SIG" = "$AMBIENT_SIG" ]; then
        bad "the nested signature is the ambient session's; refusing to dispatch"
        return 1
    fi

    # Nested Hyprland comes up with NO output — `hyprctl monitors` is `[]`, and
    # a compositor with no output has nowhere to put a window, so every
    # assertion below would be over an empty list. Hyprland can make itself one.
    hc output create headless >/dev/null 2>&1
    sleep 1
    local mons
    mons="$(hc monitors -j | python3 -c 'import json,sys
try: print(len(json.load(sys.stdin)))
except Exception: print(0)')"
    [ "${mons:-0}" -ge 1 ] || { skip "the nested Hyprland has no output to place windows on"; return 1; }

    # Two windows, tiled side by side by the default layout.
    # `wayland-<digits>` ONLY. The nested Hyprland's own children put other
    # sockets in the same directory — `wayland-1-awww-daemon.sock` among them —
    # and taking the last match handed quickshell a wallpaper daemon's socket
    # as its display. It then failed to create a wl_display and the suite
    # reported "two windows did not map", blaming the compositor.
    local nest="" w base
    for f2 in "$RT"/wayland-*; do
        [ -S "$f2" ] || continue
        base="$(basename "$f2")"
        case "${base#wayland-}" in '' | *[!0-9]*) continue ;; esac
        [ "$base" = "$host" ] && continue
        nest="$base"
    done
    [ -n "$nest" ] || { skip "the nested Hyprland published no wayland socket"; return 1; }

    for w in winLeft winRight; do
        cat > "$H/$w.qml" <<QML
import Quickshell
import QtQuick
ShellRoot { FloatingWindow { title: "$w"; visible: true; implicitWidth: 400; implicitHeight: 300
    Rectangle { anchors.fill: parent; color: "#1b1b1b" } } }
QML
        # NOT `env -i` for these two. The compositors are started with a bare
        # environment on purpose, but quickshell needs the Qt platform plugin
        # path, XDG_DATA_DIRS and the rest of an ordinary session to start at
        # all — with `env -i` it exits before mapping anything and the suite
        # reports "two windows did not map", which is true and blames the
        # compositor. The session-identifying variables are overridden
        # explicitly instead, so nothing here can reach the ambient desk.
        env -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u WLR_BACKENDS \
            HOME="$H" XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$nest" \
            XDG_CURRENT_DESKTOP=Hyprland QT_QPA_PLATFORM=wayland \
            quickshell -p "$H/$w.qml" > "$H/$w.log" 2>&1 &
        WIN_PIDS="$WIN_PIDS $!"
        sleep 2
    done

    local clients
    clients="$(hc clients -j | python3 -c 'import json,sys
try: print(len(json.load(sys.stdin)))
except Exception: print(0)')"
    if [ "${clients:-0}" -lt 2 ]; then
        skip "two windows did not map in the nested Hyprland (got ${clients:-0})"
        for w in winLeft winRight; do
            printf '      %s: ' "$w"
            tail -8 "$H/$w.log" 2>/dev/null | tr '\n' ' '
            printf '\n'
        done
        return 1
    fi

    # The bind table, once. Counted per (modmask, key) so a duplicate is a
    # number rather than an eyeball.
    BIND_DUPES="$(hc binds -j | python3 -c '
import json, sys, collections
try: binds = json.load(sys.stdin)
except Exception: binds = []
seen = collections.Counter()
for b in binds:
    seen[(b.get("modmask"), str(b.get("key", "")).upper())] += 1
dupes = [f"{m}+{k} x{n}" for (m, k), n in sorted(seen.items(), key=str) if n > 1]
print(",".join(dupes))')"

    # ── the measurement ──────────────────────────────────────────────────────
    #
    # `hyprctl dispatch movefocus l` is a SYNTAX ERROR under a Lua config: it is
    # wrapped as `return hl.dispatch(movefocus l)` and Hyprland answers
    # "')' expected near 'l'". It fails loudly, which is the only reason this
    # was found rather than silently measuring a focus change that never
    # happened. The Lua form is the dispatcher object the config itself binds.
    #
    # Focus is moved twice — right, then left — so the run does not depend on
    # which window the compositor happened to focus when the second one mapped.
    hc dispatch 'hl.dsp.focus({ direction = "right" })' >/dev/null 2>&1
    sleep 0.5

    # Put the pointer where a real one would be: inside the window that
    # currently has focus. Without this the run starts with the cursor at the
    # screen's centre, which on two tiled windows is the two-pixel border
    # between them — so the control run's "nudge" moves the pointer from one
    # gap pixel to the next, focus has nowhere to go, and the control reports
    # that nothing was stolen. Which is true, and proves nothing.
    local here_geo
    here_geo="$(hc activewindow -j | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin); x, y = d.get("at", [0, 0]); w, h = d.get("size", [0, 0])
    print(x + w // 2, y + h // 2)
except Exception: print(0, 0)')"
    # shellcheck disable=SC2086  # three fields, split on purpose
    set -- $here_geo
    hc dispatch "hl.dsp.cursor.move({ x = $1, y = $2 })" >/dev/null 2>&1
    sleep 0.4

    hc dispatch 'hl.dsp.focus({ direction = "left" })' >/dev/null 2>&1
    sleep 0.8

    ACTIVE_AFTER="$(hc activewindow -j | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("title") or "<none>")
except Exception: print("<none>")')"
    TARGET_GEO="$(hc activewindow -j | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin); print(*d.get("at", [0, 0]), *d.get("size", [0, 0]))
except Exception: print("0 0 0 0")')"
    CURSOR_AFTER="$(hc cursorpos | tr -d ' ')"

    # The nudge. One pixel, which is the smallest thing a hand resting on a
    # mouse produces, and the event follow_mouse acts on.
    local cx cy
    cx="${CURSOR_AFTER%%,*}"; cy="${CURSOR_AFTER##*,}"
    hc dispatch "hl.dsp.cursor.move({ x = $((cx + 1)), y = $cy })" >/dev/null 2>&1
    sleep 0.8
    ACTIVE_NUDGED="$(hc activewindow -j | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("title") or "<none>")
except Exception: print("<none>")')"

    # ── is follow_mouse actually live in this session? ───────────────────────
    #
    # Every assertion above rests on it. If the seeded config somehow stopped
    # focusing under the pointer, "the window is still focused after a nudge"
    # would pass for the wrong reason and would keep passing forever. So the
    # pointer is put in the middle of the OTHER window and focus must follow it
    # there — which is the same mechanism that takes focus away from a keyboard
    # focus change whose pointer was left behind.
    local other
    other="$(hc clients -j | python3 -c '
import json, sys
active = sys.argv[1]
try: clients = json.load(sys.stdin)
except Exception: clients = []
for c in clients:
    if (c.get("title") or "") != active:
        x, y = c.get("at", [0, 0]); w, h = c.get("size", [0, 0])
        print("%d %d %s" % (x + w // 2, y + h // 2, c.get("title") or "?"))
        break' "$ACTIVE_NUDGED")"
    if [ -n "$other" ]; then
        # shellcheck disable=SC2086  # three fields, split on purpose
        set -- $other
        OTHER_TITLE="$3"
        hc dispatch "hl.dsp.cursor.move({ x = $1, y = $2 })" >/dev/null 2>&1
        sleep 0.8
        ACTIVE_CROSSED="$(hc activewindow -j | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("title") or "<none>")
except Exception: print("<none>")')"
    fi
    return 0
}

hc() {
    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl "$@" 2>&1
}

cursor_inside() {   # cursor_inside "x,y" "ax ay w h"
    python3 - "$1" "$2" <<'PY'
import sys
try:
    cx, cy = (int(v) for v in sys.argv[1].split(","))
    ax, ay, w, h = (int(v) for v in sys.argv[2].split())
except Exception:
    sys.exit(2)
sys.exit(0 if (ax <= cx <= ax + w and ay <= cy <= ay + h) else 1)
PY
}

# ─────────────────────────────────────────────────────────────────────────────
sec "the seeded config, as shipped (cursor:no_warps = false)"

if ! run_session false; then
    cleanup; finish; exit $?
fi

warp_active="$ACTIVE_AFTER"; warp_cursor="$CURSOR_AFTER"
warp_geo="$TARGET_GEO";      warp_nudged="$ACTIVE_NUDGED"; warp_dupes="$BIND_DUPES"
warp_crossed="$ACTIVE_CROSSED"; warp_other="$OTHER_TITLE"

if [ "$warp_active" != "<none>" ]; then
    ok "directional focus moved focus to a window ($warp_active)"
else
    bad "directional focus moved focus to a window (nothing is focused)"
fi

if cursor_inside "$warp_cursor" "$warp_geo"; then
    ok "the pointer ended up INSIDE the focused window (cursor $warp_cursor, window $warp_geo)"
else
    bad "the pointer ended up inside the focused window (cursor $warp_cursor, window $warp_geo)"
fi

if [ "$warp_nudged" = "$warp_active" ] && [ "$warp_active" != "<none>" ]; then
    ok "the window is STILL focused after a one-pixel pointer nudge ($warp_nudged)"
else
    bad "the window is still focused after a one-pixel pointer nudge ($warp_active -> $warp_nudged)"
fi

# The premise. Without this, "still focused after a nudge" could be passing
# because nothing in this session focuses under the pointer at all, and the warp
# would be doing no work.
if [ -n "$warp_other" ] && [ "$warp_crossed" = "$warp_other" ]; then
    ok "focus DOES follow the pointer here, so the warp is load-bearing (pointer into $warp_other -> focused $warp_crossed)"
elif [ -z "$warp_other" ]; then
    skip "there was no second window to move the pointer into; the nudge assertion proves less"
else
    bad "focus follows the pointer here (pointer into $warp_other -> focused $warp_crossed) — if it does not, the assertions above are vacuous"
fi

# The bind table. This is the half a synthetic keypress would have covered.
if [ -z "$warp_dupes" ]; then
    ok "no combo is bound twice (one press cannot move focus two windows)"
else
    bad "no combo is bound twice — duplicates: $warp_dupes"
fi

for combo in "64 LEFT" "64 RIGHT" "64 UP" "64 DOWN"; do
    # shellcheck disable=SC2086  # two fields, split on purpose
    set -- $combo
    if hc binds -j | python3 -c '
import json, sys
want_mod, want_key = int(sys.argv[1]), sys.argv[2]
try: binds = json.load(sys.stdin)
except Exception: binds = []
n = sum(1 for b in binds
        if b.get("modmask") == want_mod
        and str(b.get("key", "")).upper() == want_key)
sys.exit(0 if n == 1 else 1)' "$1" "$2"; then
        ok "SUPER+$2 is bound exactly once"
    else
        bad "SUPER+$2 is bound exactly once"
    fi
done

# The switcher's own bindings, including the one thing no headless run can
# press: that the ALT release binding is REGISTERED as a release.
sec "the ALT+Tab switcher's bindings"

switcher_binds="$(hc binds -j | python3 -c '
import json, sys
try: binds = json.load(sys.stdin)
except Exception: binds = []
for b in binds:
    d = str(b.get("description", ""))
    if "switcher" in d.lower():
        print("%s|%s|%s|%s|%s" % (b.get("modmask"), b.get("key"), b.get("release"),
                                  b.get("non_consuming"), d))')"
echo "$switcher_binds" | sed 's/^/    /'

want_one() {   # want_one DESC MODMASK KEY RELEASE NON_CONSUMING
    if printf '%s\n' "$switcher_binds" \
        | pipe_has -iE "^$2\|$3\|$4\|$5\|"; then
        ok "$1"
    else
        bad "$1"
    fi
}
# modmask 8 is ALT, 9 is ALT+SHIFT.
#
# The `non_consuming` column is the one worth explaining. ALT+Return and
# ALT+Escape are pressed with the switcher CLOSED almost every time — Thunar
# opens Properties on ALT+Return — so a consuming bind would take them away
# from every application on the machine to serve a switcher that is not open.
# A consumed ALT RELEASE is worse: that is how an application ends up believing
# Alt is still held.
#
# It is asserted rather than trusted because the wrong spelling is SILENT.
# `nonConsuming`, `consume = false` and `ignore_mods` are all accepted by
# hl.bind without an error and all leave `non_consuming: false`; so does
# `transparent = true`, which means a different thing entirely (this bind
# cannot be shadowed by another bind).
want_one "ALT+Tab opens the switcher, and consumes Tab"        8 Tab    false false
want_one "ALT+SHIFT+Tab steps backwards"                       9 Tab    false false
want_one "ALT+Escape cancels, without eating ALT+Escape"       8 Escape false true
want_one "ALT+Return commits, without eating ALT+Return"       8 Return false true
want_one "releasing Alt_L commits (release, non-consuming)"    8 Alt_L  true  true
want_one "releasing Alt_R commits (release, non-consuming)"    8 Alt_R  true  true
# After ALT+SHIFT+Tab the fingers do not leave both modifiers at the same
# instant, so the ALT release usually arrives with SHIFT still down — modmask 9,
# which the binds above do not match.
want_one "releasing Alt_L with SHIFT still down also commits"  9 Alt_L  true  true
want_one "releasing Alt_R with SHIFT still down also commits"  9 Alt_R  true  true

cleanup

# ─────────────────────────────────────────────────────────────────────────────
sec "the negative control (cursor:no_warps = true) — these MUST fail"

# Without this, every assertion above would pass on a session that does not
# warp at all, and the suite would be measuring nothing.
if ! run_session true; then
    skip "the negative control could not be run; the assertions above prove less"
    finish; exit $?
fi

if cursor_inside "$CURSOR_AFTER" "$TARGET_GEO"; then
    bad "no_warps = true still put the pointer inside the window — the warp assertion above tests nothing (cursor $CURSOR_AFTER, window $TARGET_GEO)"
else
    ok "no_warps = true leaves the pointer behind (cursor $CURSOR_AFTER, window $TARGET_GEO) — so the assertion above has teeth"
fi

if [ "$ACTIVE_NUDGED" != "$ACTIVE_AFTER" ]; then
    ok "and the nudge then takes focus away ($ACTIVE_AFTER -> $ACTIVE_NUDGED) — exactly the reported defect"
else
    # Not a failure of the shipped configuration, but it does mean the nudge
    # assertion above is weaker than it looks, and saying so is the point.
    skip "the nudge did not steal focus even with no_warps = true; the nudge assertion above proves less than it claims ($ACTIVE_AFTER)"
fi

cleanup
finish
