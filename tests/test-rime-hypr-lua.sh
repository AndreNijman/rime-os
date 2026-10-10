#!/usr/bin/env bash
# The seeded Hyprland Lua tree, in a REAL running Hyprland.
#
# `Hyprland --verify-config` proves the config parses and every hl.* call is
# accepted. It cannot prove the three things that decide whether the settings
# pages work at all:
#
#   * `hyprctl configerrors` is clean on a live instance;
#   * `hyprctl reload` re-reads a generated module that was rewritten under it —
#     Lua caches modules in package.loaded, so a naive loader would serve what
#     was on disk at login and a saved display layout would never appear;
#   * how a live change is pushed at all. `hyprctl keyword` REFUSES against a
#     Lua config ("keyword can't work with non-legacy parsers. Use eval.") and
#     changes nothing, which is how Rime Shell re-tinted the window border and
#     how rime-display-apply applied a layout. Both use `hyprctl eval` now, and
#     this asserts both that eval works and that keyword still does not — so a
#     future Hyprland quietly restoring keyword is noticed here.
#
# ── How this avoids touching the developer's session ─────────────────────────
# labwc is started with the wlroots HEADLESS backend, which creates a Wayland
# socket with no visible output anywhere. Hyprland is then nested inside THAT,
# with its own XDG_RUNTIME_DIR, its own HOME and its own instance signature. So
# nothing is drawn on the machine running the test and no hyprctl in here can
# reach the real compositor: the signature it would need is not in this
# script's environment.
#
# The renderer is deliberately NOT pixman. A pixman host exports no
# linux-dmabuf, and Hyprland's Wayland backend dies with
# "CBackend::create() failed!" against one.
#
# Skips cleanly (status 0) when labwc or Hyprland is missing.
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
    printf '\nrime hypr-lua: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
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
if [ ! -f "$TMPL" ] || [ ! -d "$MODULES" ]; then
    bad "the seeded Hyprland tree is present"
    finish; exit $?
fi

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

# ── the fixture home ─────────────────────────────────────────────────────────
mkdir -p "$H/.config/labwc" "$H/.config/hypr/rime"
# An empty labwc autostart, so the host compositor does not launch this
# machine's real session inside the test.
: > "$H/.config/labwc/autostart"
printf '<?xml version="1.0"?>\n<labwc_config></labwc_config>\n' > "$H/.config/labwc/rc.xml"

render() { sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$1"; }
render "$TMPL" > "$H/.config/hypr/hyprland.lua"
for f in "$MODULES"/*.lua; do
    render "$f" > "$H/.config/hypr/rime/$(basename "$f")"
done

# ── the headless host ────────────────────────────────────────────────────────
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

# ── the nested Hyprland ──────────────────────────────────────────────────────
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

hc() {
    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl "$@" 2>&1
}

sec "the seeded config loads clean"

errs="$(hc configerrors | tr -d '[:space:]')"
if [ -z "$errs" ] || [ "$errs" = "noerrors" ]; then
    ok "hyprctl configerrors is empty"
else
    bad "hyprctl configerrors is empty — got: $(hc configerrors | head -5)"
fi

# The bind count is the sharp end of the migration: a keybinds module that
# loaded but bound nothing would still give a clean configerrors.
count="$(hc binds -j | python3 -c 'import json,sys
try: print(len(json.load(sys.stdin)))
except Exception: print(0)')"
if [ "${count:-0}" -ge 50 ]; then
    ok "the Rime keybinds are registered (${count} binds)"
else
    bad "the Rime keybinds are registered (got ${count}, expected 50+)"
fi

# Every bind the seeded module declares, actually present, addressed the way a
# user presses it. modmask 64 is SUPER, 65 is SUPER+SHIFT.
check_bind() {
    local desc="$1" modmask="$2" key="$3"
    if hc binds -j | python3 -c '
import json, sys
want_mod, want_key = int(sys.argv[1]), sys.argv[2]
binds = json.load(sys.stdin)
sys.exit(0 if any(b.get("modmask") == want_mod
                  and b.get("key", "").lower() == want_key.lower()
                  for b in binds) else 1)' "$modmask" "$key"; then
        ok "$desc"
    else
        bad "$desc"
    fi
}
check_bind "SUPER+T is bound (terminal)"          64 T
check_bind "SUPER+Q is bound (close window)"      64 Q
check_bind "SUPER+SHIFT+SPACE is bound (float)"   65 SPACE
check_bind "SUPER+1 is bound (workspace)"         64 1
check_bind "the mouse drag bind is registered"    64 "mouse:272"
check_bind "a bare XF86 media key is bound"        0 XF86AudioRaiseVolume

# bindm/bindel semantics survived the move to Lua options. `hyprctl binds -j`
# spells the repeat flag "repeat".
#
# The mouse drag binds must be PRESS binds: release:false. This used to assert
# the opposite, and so enforced the bug it should have caught. window.drag() and
# window.resize() start on button-down and end the move themselves; a bind with
# release:true (what `{ drag = true }` produces) fires only on button-up, and
# SUPER+drag moves nothing. Measured 2026-09-27 in a nested 0.56.2 with a
# virtual pointer holding SUPER: release:true moved no window, release:false
# floated it and followed the pointer. `mouse` reads false for both, so it
# tells them apart no better than the file does; `release` is the observable.
#
# Each key is checked on its own: exactly one SUPER (modmask 64) bind, on
# press, carrying its own action's description. A Lua bind's dispatcher reads
# `__lua` plus a registry number, so the description is the only field that
# says which action is behind it — two unrelated press binds on these keys, or
# the right ones under another modifier, would otherwise pass.
if hc binds -j | python3 -c '
import json, sys
binds = json.load(sys.stdin)
def one(key, desc):
    hits = [b for b in binds if b.get("key") == key and b.get("modmask") == 64]
    return (len(hits) == 1 and not hits[0].get("release")
            and hits[0].get("description") == desc)
vol = [b for b in binds if b.get("key") == "XF86AudioRaiseVolume"]
sys.exit(0 if one("mouse:272", "Move window") and one("mouse:273", "Resize window")
                and vol and vol[0].get("repeat") and vol[0].get("locked") else 1)'; then
    ok "the mouse drag binds fire on press, and locked/repeat reached the compositor"
else
    bad "the mouse drag binds fire on press, and locked/repeat reached the compositor"
fi

sec "the live-change path is hyprctl eval, because keyword is dead under Lua"

# THE finding of this migration, and the one that would have shipped silently.
# Rime Shell re-tints the active border from the wallpaper this way, and
# rime-display-apply applies a display layout live this way. `hyprctl keyword`
# refuses against a Lua config —
#
#     keyword can't work with non-legacy parsers. Use eval.
#
# — and changes nothing. Neither caller has a fallback and neither would have
# reported anything: Settings → Display would say the layout applied and no
# display would move.
#
# Asserted in BOTH directions. If a future Hyprland made keyword work again,
# the second half fails and someone re-reads this instead of finding out from a
# user whose gaps slider does nothing.
if hc keyword general:border_size 7 | pipe_has -i 'non-legacy\|use eval'; then
    ok "hyprctl keyword is refused under a Lua config (so nothing may rely on it)"
else
    bad "hyprctl keyword is refused under a Lua config — it answered: $(hc keyword general:border_size 7 | head -1)"
fi

if hc eval 'hl.config({ general = { border_size = 7 } })' | pipe_has -i '^ok'; then
    got="$(hc getoption general:border_size | sed -n 's/^int: //p' | tr -d '[:space:]')"
    if [ "$got" = "7" ]; then
        ok "hyprctl eval applies and reads back (border_size 2 -> 7)"
    else
        bad "hyprctl eval applies and reads back (got '${got}')"
    fi
else
    bad "hyprctl eval is accepted under a Lua config"
fi

# The gradient form Rime Shell pushes on every wallpaper change.
if hc eval 'hl.config({ general = { ["col.active_border"] = { colors = { "rgb(ff0000)" } } } })' \
     | pipe_has -i '^ok'; then
    hc getoption general:col.active_border | pipe_has -i 'ff0000' \
        && ok "a gradient border re-tint applies through eval" \
        || bad "a gradient border re-tint applies through eval"
else
    bad "a gradient border re-tint is accepted through eval"
fi

# And the monitor rule rime-display-apply now sends live.
hc eval 'hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1.0 })' \
    | pipe_has -i '^ok' \
    && ok "a live hl.monitor rule is accepted through eval" \
    || bad "a live hl.monitor rule is accepted through eval"

sec "hyprctl reload re-reads a rewritten generated module"

# The failure this catches is silent and total: Lua caches every module in
# package.loaded, so a loader that just called require() would serve whatever
# was on disk at login. A display layout saved from Settings would be written,
# reloaded, and never take effect — with no error anywhere.
cat > "$H/.config/hypr/rime/monitors.lua" <<'GENERATED'
hl.config({ general = { gaps_in = 11 } })
GENERATED
hc reload >/dev/null
sleep 1
# gaps_in is a CSS gap, so hyprctl prints "css gap data: 11 11 11 11" rather
# than "int: 11". Read the first number out of whatever shape it prints.
got="$(hc getoption general:gaps_in | sed -n '1s/[^0-9]*\([0-9]\+\).*/\1/p')"
if [ "$got" = "11" ]; then
    ok "a module written AFTER startup is picked up by reload"
else
    bad "a module written after startup is picked up by reload (gaps_in='${got}')"
fi

# ...and a second rewrite is picked up too, which is what proves the cache is
# cleared on every reload rather than the first one happening to be a cold read.
cat > "$H/.config/hypr/rime/monitors.lua" <<'GENERATED'
hl.config({ general = { gaps_in = 13 } })
GENERATED
hc reload >/dev/null
sleep 1
# gaps_in is a CSS gap, so hyprctl prints "css gap data: 11 11 11 11" rather
# than "int: 11". Read the first number out of whatever shape it prints.
got="$(hc getoption general:gaps_in | sed -n '1s/[^0-9]*\([0-9]\+\).*/\1/p')"
if [ "$got" = "13" ]; then
    ok "a SECOND rewrite is picked up too (package.loaded really is cleared)"
else
    bad "a second rewrite is picked up too (gaps_in='${got}')"
fi

sec "a broken generated module does not take the desktop with it"

# The whole reason hyprland.lua pcalls each module and raises at the END. A
# settings page writing one bad line must cost that page, not the session.
printf 'this is not lua(((\n' > "$H/.config/hypr/rime/monitors.lua"
hc reload >/dev/null
sleep 1
if hc configerrors | pipe_has 'monitors'; then
    ok "the broken module is named in configerrors"
else
    bad "the broken module is named in configerrors"
fi
if [ "$(hc binds -j | python3 -c 'import json,sys
try: print(len(json.load(sys.stdin)))
except Exception: print(0)')" -ge 50 ]; then
    ok "...and every keybind from the other modules still applied"
else
    bad "...and every keybind from the other modules still applied"
fi

sec "a claimed combo leaves exactly one bind"

# What replaces the hyprlang generator's unbind-then-rebind dance, and the
# failure it exists to prevent: Hyprland fires BOTH actions for a doubly-bound
# combo, which is how SUPER+Q once closed the window AND opened the launcher.
#
# rime/keybindings.lua binds SUPER+T. A generated module claims it, and the
# count for that combo must come back to ONE — the claimant's. Counted through
# `hyprctl binds` on purpose: that is the list Rime Shell reads to warn about
# conflicting shortcuts, so a default left listed-but-inert would make the
# warning lie. It is also why disable() removes rather than disabling — a
# disabled bind is still listed, with no field saying it does nothing.
rm -f "$H/.config/hypr/rime/monitors.lua"
cat > "$H/.config/hypr/rime/shell-keybinds.lua" <<'GENERATED'
local ok, defaults = pcall(require, "rime.keybindings")
local function claim(mods, key)
    if ok and defaults and defaults.disable then defaults.disable(mods, key) end
end
claim("SUPER", "T")
hl.bind("SUPER + T", hl.dsp.exec_cmd("kitty"))
GENERATED
hc reload >/dev/null
sleep 1
n="$(hc binds -j | python3 -c '
import json, sys
binds = json.load(sys.stdin)
print(sum(1 for b in binds if b.get("key") == "T" and b.get("modmask") == 64))')"
if [ "$n" = "1" ]; then
    ok "claiming SUPER+T leaves exactly one bind on it"
else
    bad "claiming SUPER+T leaves exactly one bind on it (found ${n})"
fi

# ...and it is the claimant's, not the default that was replaced.
if hc binds -j | python3 -c '
import json, sys
binds = [b for b in json.load(sys.stdin)
         if b.get("key") == "T" and b.get("modmask") == 64]
sys.exit(0 if binds and not binds[0].get("has_description") else 1)'; then
    ok "...and the surviving bind is the one that claimed it"
else
    bad "...and the surviving bind is the one that claimed it"
fi

rm -f "$H/.config/hypr/rime/shell-keybinds.lua"
hc reload >/dev/null
sleep 1

sec "an absent generated module is not an error"

rm -f "$H/.config/hypr/rime/monitors.lua"
hc reload >/dev/null
sleep 1
errs="$(hc configerrors | tr -d '[:space:]')"
if [ -z "$errs" ] || [ "$errs" = "noerrors" ]; then
    ok "removing a generated module leaves configerrors clean"
else
    bad "removing a generated module leaves configerrors clean — got: $(hc configerrors | head -3)"
fi

finish
