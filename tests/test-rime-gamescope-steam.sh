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
esac
exit 0
FAKE
chmod +x "${BIN}/xprop"

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
    : > "$EV"; : > "$SETS"; rm -f "${WORK}/spypid"
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

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
