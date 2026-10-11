#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-gamescope-steam.sh — the Gaming Mode guard that keeps HDR off and
#  gamescope compositing, run against a fake `xprop`.
#
#  The fake's `-spy` follows an events file the way the real one follows the X
#  server: every line appended is a property change, and every `-set` the
#  guard makes is appended too (and logged), exactly as gamescope's X server
#  reports a write back. So "writes only what differs" and "does not loop on
#  its own writes" are measured, not assumed. No X server, no gamescope.
#
#  Run from anywhere:  ./tests/test-rime-gamescope-steam.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAP="${ROOT}/files/system/libexec/rime-gamescope-steam"

pass=0
fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); [ -n "${2:-}" ] && printf '      %s\n' "$2"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-gamescope-steam-XXXXXX")"
cleanup() {
    [ -f "${WORK}/spypid" ] && kill "$(cat "${WORK}/spypid")" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

BIN="${WORK}/bin"
mkdir -p "$BIN"
EV="${WORK}/events"
SETS="${WORK}/sets"
cat > "${BIN}/xprop" <<FAKE
#!/usr/bin/env bash
case " \$* " in
    *" -spy "*) echo \$\$ > "${WORK}/spypid"; exec tail -n +1 -s 0.1 -f "${EV}" ;;
    *" -set "*)
        # xprop -root -f NAME 32c -set NAME VALUE
        name="\$6" value="\$7"
        printf '%s %s\n' "\$name" "\$value" >> "${SETS}"
        printf '%s(CARDINAL) = %s\n' "\$name" "\$value" >> "${EV}" ;;
    *" -root GAMESCOPE_FOCUSED_APP "*)
        grep '^GAMESCOPE_FOCUSED_APP(' "${EV}" | tail -n 1 ;;
    *" -root GAMESCOPE_FOCUSED_WINDOW "*)
        printf 'GAMESCOPE_FOCUSED_WINDOW(CARDINAL) = %s\n' "\$(cat "${WORK}/focwin" 2>/dev/null)" ;;
    *" -id "*" WM_STATE "*)
        printf 'WM_STATE(WM_STATE):\n\t\twindow state: %s\n\t\ticon window: 0x0\n' "\$(cat "${WORK}/wmstate" 2>/dev/null)" ;;
esac
exit 0
FAKE
chmod +x "${BIN}/xprop"
# What the wrapper uses to set WM_STATE Normal: recorded, and the window then
# reads Normal, as Wine's restore makes it.
NORMALS="${WORK}/normals"
cat > "${BIN}/wm-normal" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "${NORMALS}"
echo Normal > "${WORK}/wmstate"
FAKE
chmod +x "${BIN}/wm-normal"

HDR=GAMESCOPE_DISPLAY_HDR_ENABLED
COMP=GAMESCOPE_COMPOSITE_FORCE
prop() { printf '%s(CARDINAL) = %s\n' "$1" "$2" >> "$EV"; }

# run_case SECONDS [ENV…] — start the wrapper around `sleep`, with the events
# already in $EV, and wait for it.
run_case() {
    local secs="$1"; shift
    env "$@" PATH="${BIN}:${PATH}" DISPLAY=:99 bash "$WRAP" sleep "$secs" 2>"${WORK}/log" &
    WPID=$!
}
# A guard notices its owner is gone within one 2 s read timeout; wait for
# the last case's watch to end so it cannot answer this case's events.
fresh() {
    local old
    old="$(cat "${WORK}/spypid" 2>/dev/null)"
    for _ in $(seq 1 40); do
        { [ -z "$old" ] || ! kill -0 "$old" 2>/dev/null; } && break
        sleep 0.1
    done
    : > "$EV"; : > "$SETS"; : > "$NORMALS"; rm -f "${WORK}/spypid"
    echo 100663297 > "${WORK}/focwin"; echo Normal > "${WORK}/wmstate"
}
sets() { tr '\n' ';' < "$SETS"; }

# ── Steam's own start-up state on an HDR display: HDR on, composition off ───
fresh; prop $HDR 1; prop $COMP 0
run_case 2; sleep 1
[ "$(sets)" = "$HDR 0;$COMP 1;" ] \
    && ok "HDR on + composition off: HDR is turned off, then composition forced, once each" \
    || bad "HDR on + composition off" "sets: $(sets)"

# Steam writes its developer setting back to 0 later: the guard answers again.
prop $COMP 0; sleep 0.6
[ "$(sets)" = "$HDR 0;$COMP 1;$COMP 1;" ] \
    && ok "Steam resetting composition later is undone" \
    || bad "Steam resetting composition later" "sets: $(sets)"
prop $HDR 1; sleep 0.6
[ "$(sets)" = "$HDR 0;$COMP 1;$COMP 1;$HDR 0;" ] \
    && ok "Steam turning HDR on later is undone (composition already forced)" \
    || bad "Steam turning HDR on later" "sets: $(sets)"
wait "$WPID"

# The guard must not outlive the process it was started for.
sleep 2.5
spy="$(cat "${WORK}/spypid" 2>/dev/null)"
if [ -n "$spy" ] && ! kill -0 "$spy" 2>/dev/null; then
    ok "the property watch ends when Steam does"
else
    bad "the property watch ends when Steam does" "xprop -spy ${spy:-?} still running"
fi

# ── already right: nothing is written ───────────────────────────────────────
fresh; prop $HDR 0; prop $COMP 1
run_case 1; wait "$WPID"
[ ! -s "$SETS" ] && ok "HDR off + composition forced already: nothing written" \
    || bad "already right" "sets: $(sets)"

# ── properties gamescope has not created yet ────────────────────────────────
fresh
printf '%s:  not found.\n' $HDR $COMP >> "$EV"
run_case 1; wait "$WPID"
[ "$(sets)" = "$COMP 1;" ] && ok "no HDR property (= off): composition forced" \
    || bad "absent properties" "sets: $(sets)"

# ── RIME_GAMING_HDR=steam: Steam's HDR stands; composition follows it ───────
fresh; prop $HDR 1; prop $COMP 0
run_case 2 RIME_GAMING_HDR=steam; sleep 1
[ ! -s "$SETS" ] && ok "RIME_GAMING_HDR=steam: HDR on is left alone, no forcing while it is on" \
    || bad "RIME_GAMING_HDR=steam, HDR on" "sets: $(sets)"
prop $HDR 0; sleep 0.6
[ "$(sets)" = "$COMP 1;" ] && ok "RIME_GAMING_HDR=steam: HDR turned off in Steam forces composition" \
    || bad "RIME_GAMING_HDR=steam, HDR off" "sets: $(sets)"
wait "$WPID"

# ── RIME_GAMING_FORCE_COMPOSITE=0: HDR still kept off, composition untouched
fresh; prop $HDR 1; prop $COMP 0
run_case 1 RIME_GAMING_FORCE_COMPOSITE=0; wait "$WPID"
[ "$(sets)" = "$HDR 0;" ] && ok "RIME_GAMING_FORCE_COMPOSITE=0: HDR off, composition left to gamescope" \
    || bad "RIME_GAMING_FORCE_COMPOSITE=0" "sets: $(sets)"

# ── it is Steam: exec, not a child ──────────────────────────────────────────
fresh; prop $HDR 0; prop $COMP 1
out="$(PATH="${BIN}:${PATH}" DISPLAY=:99 bash "$WRAP" bash -c 'echo $PPID $$' 2>/dev/null & echo "W $!"; wait)"
wpid="$(printf '%s\n' "$out" | awk '$1=="W"{print $2}')"
cpid="$(printf '%s\n' "$out" | awk '$1!="W"{print $2}')"
[ -n "$wpid" ] && [ "$wpid" = "$cpid" ] && ok "the command replaces the wrapper (same PID: gamescope's child is Steam)" \
    || bad "exec" "wrapper $wpid, command $cpid"

# ── no xprop, no DISPLAY, no command ────────────────────────────────────────
NOX="${WORK}/nox"; mkdir -p "$NOX"
for t in bash env sleep; do ln -s "$(command -v $t)" "$NOX/$t"; done
if PATH="$NOX" DISPLAY=:99 "$NOX/bash" "$WRAP" sleep 0 2>"${WORK}/log"; then
    grep -q "no xprop" "${WORK}/log" && ok "no xprop: Steam still starts, and the log says why nothing is guarded" \
        || bad "no xprop message" "$(cat "${WORK}/log")"
else
    bad "no xprop: the command must still run"
fi
if env -u DISPLAY PATH="${BIN}:${PATH}" bash "$WRAP" true 2>"${WORK}/log" && grep -q "no DISPLAY" "${WORK}/log"; then
    ok "no DISPLAY: the command runs unguarded, and says so"
else
    bad "no DISPLAY" "$(cat "${WORK}/log")"
fi
bash "$WRAP" 2>/dev/null; rc=$?
[ "$rc" = 2 ] && ok "no command: usage error (2)" || bad "no command" "exit $rc"

# ── NVIDIA GPUs older than Ada: VK_KHR_opacity_micromap off ────────────────
# Fixture PCI trees. katana: Intel iGPU + RTX 3070 Laptop (GA104, 0x249d).
pci() {  # root slot vendor device class
    mkdir -p "$1/bus/pci/devices/$2"
    printf '%s\n' "$3" > "$1/bus/pci/devices/$2/vendor"
    printf '%s\n' "$4" > "$1/bus/pci/devices/$2/device"
    printf '%s\n' "$5" > "$1/bus/pci/devices/$2/class"
}
KAT="${WORK}/sys-katana"; pci "$KAT" 0000:00:02.0 0x8086 0x46a6 0x030000; pci "$KAT" 0000:01:00.0 0x10de 0x249d 0x030200
ADA="${WORK}/sys-ada";    pci "$ADA" 0000:01:00.0 0x10de 0x2820 0x030000; pci "$ADA" 0000:01:00.1 0x10de 0x22bd 0x040300
AMD="${WORK}/sys-amd";    pci "$AMD" 0000:03:00.0 0x1002 0x73bf 0x030000
# what the command (Steam) sees in VKD3D_DISABLE_EXTENSIONS; no DISPLAY, so the guard stays out of it
seen() { env -u DISPLAY "$@" bash "$WRAP" bash -c 'printf %s "${VKD3D_DISABLE_EXTENSIONS-UNSET}"' 2>"${WORK}/log"; }

[ "$(seen RIME_SYSFS="$KAT")" = VK_KHR_opacity_micromap ] && grep -q "older than Ada" "${WORK}/log" \
    && ok "katana (GA104): opacity micromaps are disabled for Proton, and the log says why" \
    || bad "katana (GA104)" "seen: $(seen RIME_SYSFS="$KAT"); log: $(cat "${WORK}/log")"
[ "$(seen RIME_SYSFS="$ADA")" = UNSET ] \
    && ok "Ada (and its audio function, an older device id): nothing is disabled" \
    || bad "Ada" "seen: $(seen RIME_SYSFS="$ADA")"
[ "$(seen RIME_SYSFS="$AMD")" = UNSET ] && ok "no NVIDIA GPU: nothing is disabled" \
    || bad "AMD" "seen: $(seen RIME_SYSFS="$AMD")"
[ "$(seen RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_EXT_mesh_shader)" = VK_EXT_mesh_shader,VK_KHR_opacity_micromap ] \
    && ok "a list the user already set is kept and appended to" \
    || bad "append" "seen: $(seen RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_EXT_mesh_shader)"
[ "$(seen RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap)" = VK_KHR_opacity_micromap ] \
    && ok "already in the list: not added twice" \
    || bad "no duplicate" "seen: $(seen RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap)"
[ "$(seen RIME_SYSFS="$KAT" RIME_GAMING_OMM=on)" = UNSET ] && ok "RIME_GAMING_OMM=on keeps micromaps on" \
    || bad "RIME_GAMING_OMM=on" "seen: $(seen RIME_SYSFS="$KAT" RIME_GAMING_OMM=on)"
# ── NVIDIA shader cache size ────────────────────────────────────────────────
cache_seen() { env -u DISPLAY "$@" bash "$WRAP" bash -c 'printf %s "${__GL_SHADER_DISK_CACHE_SIZE-UNSET}"' 2>/dev/null; }
[ "$(cache_seen -u __GL_SHADER_DISK_CACHE_SIZE)" = 12000000000 ] \
    && ok "Steam gets a 12 GB NVIDIA shader cache size" \
    || bad "NVIDIA shader cache size" "seen: $(cache_seen -u __GL_SHADER_DISK_CACHE_SIZE)"
[ "$(cache_seen __GL_SHADER_DISK_CACHE_SIZE=3000000000)" = 3000000000 ] \
    && ok "a size the user set is kept" \
    || bad "user cache size" "seen: $(cache_seen __GL_SHADER_DISK_CACHE_SIZE=3000000000)"

# ── a game the overlay minimised is restored when focus comes back ─────────
# katana, Cyberpunk 2077 (2026-10-11): Shift+Tab moves GAMESCOPE_FOCUSED_APP to
# Steam (769) and the game marks its window Iconic; back on the game, nothing
# took it out of Iconic and it stayed a 160x31 picture in the top-left corner.
FA=GAMESCOPE_FOCUSED_APP
normals() { tr '\n' ';' < "$NORMALS"; }
fresh; prop $HDR 0; prop $COMP 1; prop $FA 1091500
run_case 4 RIME_GAMING_WM_NORMAL="${BIN}/wm-normal"; sleep 0.6
[ ! -s "$NORMALS" ] && ok "a game in focus and not minimised: nothing is touched" \
    || bad "game focused, Normal" "normals: $(normals)"
echo Iconic > "${WORK}/wmstate"; prop $FA 769; sleep 0.6
[ ! -s "$NORMALS" ] && ok "Steam's overlay in focus, the game minimised: left alone (it would only minimise again)" \
    || bad "overlay focused" "normals: $(normals)"
prop $FA 1091500; sleep 0.6
[ "$(normals)" = "100663297;" ] && ok "focus back on the game, its window still minimised: set Normal again, once" \
    || bad "focus back, Iconic" "normals: $(normals)"
sleep 1.2
[ "$(normals)" = "100663297;" ] && ok "the second look a moment later finds it Normal and writes nothing" \
    || bad "second look" "normals: $(normals)"
# a quick Shift+Tab: the game minimises only after focus is already back
echo Normal > "${WORK}/wmstate"; prop $FA 769; prop $FA 1091500; sleep 0.3
echo Iconic > "${WORK}/wmstate"; sleep 1.5
[ "$(normals)" = "100663297;100663297;" ] && ok "a game that minimises just after focus is back is restored by the second look" \
    || bad "late minimise" "normals: $(normals)"
wait "$WPID"
[ ! -s "$SETS" ] && ok "watching focus writes none of gamescope's root properties" \
    || bad "focus watch wrote root properties" "sets: $(sets)"

fresh; prop $HDR 0; prop $COMP 1; echo Iconic > "${WORK}/wmstate"; prop $FA 1091500
run_case 2 RIME_GAMING_WM_NORMAL="${BIN}/wm-normal" RIME_GAMING_RESTORE_MINIMIZED=0; wait "$WPID"
[ ! -s "$NORMALS" ] && ok "RIME_GAMING_RESTORE_MINIMIZED=0: a minimised game is left as it is" \
    || bad "RIME_GAMING_RESTORE_MINIMIZED=0" "normals: $(normals)"

fresh; echo Iconic > "${WORK}/wmstate"; prop $FA 1091500
run_case 2 RIME_GAMING_WM_NORMAL="${BIN}/wm-normal" RIME_GAMING_HDR=steam RIME_GAMING_FORCE_COMPOSITE=0; wait "$WPID"
[ "$(normals)" = "100663297;" ] && ok "HDR and composition both left to Steam: the focus watch still runs" \
    || bad "watch with no HDR/composite guard" "normals: $(normals)"

# ── the real setter, on a real X server (Xvfb) ─────────────────────────────
# Everything above uses a stub setter; this runs the shipped one: the typed
# WM_STATE write, and its refusal when focus moved before it could write.
# CI installs Xvfb and x11-utils and sets RIME_TEST_REQUIRE_X=1, so a missing
# server is a failure there rather than a quiet skip.
REAL_XPROP="$(PATH="${PATH//${BIN}:/}" command -v xprop || true)"
if command -v Xvfb >/dev/null && [ -n "$REAL_XPROP" ]; then
    XDISP=$((90 + RANDOM % 400))
    Xvfb ":${XDISP}" -nolisten tcp -noreset >/dev/null 2>&1 &
    XVFB=$!
    for _ in $(seq 50); do [ -e "/tmp/.X11-unix/X${XDISP}" ] && break; sleep 0.1; done
    cat > "${WORK}/xwin.py" <<'PY'
import ctypes, ctypes.util, sys
x = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
vp, ul = ctypes.c_void_p, ctypes.c_ulong
x.XOpenDisplay.restype = vp
x.XDefaultRootWindow.restype = ul; x.XDefaultRootWindow.argtypes = [vp]
x.XCreateSimpleWindow.restype = ul
x.XCreateSimpleWindow.argtypes = [vp, ul, ctypes.c_int, ctypes.c_int, ctypes.c_uint, ctypes.c_uint,
                                  ctypes.c_uint, ul, ul]
x.XInternAtom.restype = ul; x.XInternAtom.argtypes = [vp, ctypes.c_char_p, ctypes.c_int]
x.XChangeProperty.argtypes = [vp, ul, ul, ul, ctypes.c_int, ctypes.c_int, vp, ctypes.c_int]
x.XSetCloseDownMode.argtypes = [vp, ctypes.c_int]
x.XCloseDisplay.argtypes = [vp]
d = x.XOpenDisplay(None)
w = x.XCreateSimpleWindow(d, x.XDefaultRootWindow(d), 0, 0, 160, 31, 0, 0, 0)
a = x.XInternAtom(d, b"WM_STATE", 0)
x.XChangeProperty(d, w, a, a, 32, 0, (ctypes.c_long * 2)(3, 0), 2)   # IconicState
x.XSetCloseDownMode(d, 1)   # RetainPermanent: the window outlives this client
x.XCloseDisplay(d)
print(w)
PY
    xwin()  { DISPLAY=":${XDISP}" python3 "${WORK}/xwin.py"; }
    xroot() { DISPLAY=":${XDISP}" "$REAL_XPROP" -root -f "$1" 32c -set "$1" "$2"; }
    xstate() { DISPLAY=":${XDISP}" "$REAL_XPROP" -id "$1" WM_STATE; }
    # the shipped wm_normal, as the wrapper defines it
    setter() {
        ( eval "$(sed -n '/^STEAM_APPID=/p; /^wm_normal() {/,/^}/p' "$WRAP")"
          DISPLAY=":${XDISP}" wm_normal "$1" )
    }

    W="$(xwin)"
    xroot GAMESCOPE_FOCUSED_WINDOW "$W"; xroot GAMESCOPE_FOCUSED_APP 769
    setter "$W"; rc=$?
    [ "$rc" = 2 ] && xstate "$W" | grep -q "window state: Iconic" \
        && ok "real X: the setter writes nothing while the overlay (app 769) has focus" \
        || bad "real X refusal under the overlay" "rc=$rc; $(xstate "$W" | tr '\n' ' ')"
    xroot GAMESCOPE_FOCUSED_APP 1091500; xroot GAMESCOPE_FOCUSED_WINDOW 4242
    setter "$W"; rc=$?
    [ "$rc" = 2 ] && xstate "$W" | grep -q "window state: Iconic" \
        && ok "real X: the setter writes nothing once another window has focus" \
        || bad "real X refusal for another window" "rc=$rc; $(xstate "$W" | tr '\n' ' ')"
    xroot GAMESCOPE_FOCUSED_WINDOW "$W"
    setter "$W"; rc=$?
    st="$(xstate "$W")"
    [ "$rc" = 0 ] && printf '%s' "$st" | grep -q '^WM_STATE(WM_STATE):' \
        && printf '%s' "$st" | grep -q "window state: Normal" \
        && ok "real X: the setter writes WM_STATE Normal, typed WM_STATE" \
        || bad "real X typed write" "rc=$rc; $(printf '%s' "$st" | tr '\n' ' ')"

    # End to end: the wrapper's own watch, real xprop -spy, real setter.
    W2="$(xwin)"
    xroot GAMESCOPE_FOCUSED_WINDOW "$W2"; xroot GAMESCOPE_FOCUSED_APP 769
    env -u RIME_GAMING_WM_NORMAL DISPLAY=":${XDISP}" bash "$WRAP" sleep 3 2>"${WORK}/xlog" &
    XW=$!
    sleep 1; xroot GAMESCOPE_FOCUSED_APP 1091500
    wait "$XW"
    xstate "$W2" | grep -q "window state: Normal" \
        && ok "real X: the wrapper restores the game when focus comes back from the overlay" \
        || bad "real X end to end" "$(xstate "$W2" | tr '\n' ' '); log: $(tr '\n' ' ' < "${WORK}/xlog")"
    kill "$XVFB" 2>/dev/null; wait "$XVFB" 2>/dev/null
elif [ "${RIME_TEST_REQUIRE_X:-0}" = 1 ]; then
    bad "real X setter" "Xvfb or xprop missing and RIME_TEST_REQUIRE_X=1"
else
    printf 'SKIP  real X setter (no Xvfb or xprop here)\n'
fi

# ── volume keys: Steam acts on them only with its handler on ───────────────
vol_seen() { env -u DISPLAY "$@" bash "$WRAP" bash -c 'printf %s "${STEAM_ENABLE_VOLUME_HANDLER-UNSET}"' 2>/dev/null; }
[ "$(vol_seen -u STEAM_ENABLE_VOLUME_HANDLER)" = 1 ] \
    && ok "Steam gets STEAM_ENABLE_VOLUME_HANDLER=1, so the volume keys change the volume" \
    || bad "volume handler" "seen: $(vol_seen -u STEAM_ENABLE_VOLUME_HANDLER)"
[ "$(vol_seen STEAM_ENABLE_VOLUME_HANDLER=0)" = 0 ] \
    && ok "a STEAM_ENABLE_VOLUME_HANDLER the user set is kept" \
    || bad "user volume handler" "seen: $(vol_seen STEAM_ENABLE_VOLUME_HANDLER=0)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
