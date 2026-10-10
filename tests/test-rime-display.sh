#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Assertions for /usr/libexec/rime-display-apply.
#
#  The generators are pure functions of the model, so they are tested directly
#  and exhaustively. The parts that need real outputs — enumeration and a live
#  apply — are exercised only when this runs inside a session that has them, and
#  skipped otherwise rather than faked.
#
#      ./tests/test-rime-display.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` is deliberate and load-bearing. This suite COUNTS failures rather
# than aborting on them, and several assertions run commands that exit non-zero
# on purpose — a refusal, a guard firing, a bad argument. GitHub Actions invokes
# a script as `bash -e {0}`, and under `-e` a `x="$(cmd)"` assignment whose
# command exits non-zero terminates the whole script. That is exactly what
# happened: the suite passed locally, and on CI it died part-way through with
# the remaining assertions reported as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="${ROOT}/files/system/libexec/rime-display-apply"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s\n' "$1"; }
section() { printf '\n── %s ──\n' "$1"; }

[ -f "$GEN" ] || { printf 'missing %s\n' "$GEN" >&2; exit 1; }

section "the generators"
python3 -c "import ast; ast.parse(open('$GEN').read())" \
    && ok "rime-display-apply is valid Python" || bad "rime-display-apply is valid Python"

st="$(python3 "$GEN" --self-test 2>&1)"
printf '%s\n' "$st" | sed 's/^/      /'
printf '%s\n' "$st" | pipe_has '^FAIL' \
    && bad "the generator self-test passes" || ok "the generator self-test passes"

# Fake compositor tools, each recording its invocation. Two jobs:
#   * determinism — kanshi profiles are keyed on the CONNECTED outputs, so a
#     real enumeration would make these assertions depend on how many monitors
#     the host has plugged in;
#   * proof of isolation — see the section further down.
# They print nothing, so enumeration yields no outputs and the generator falls
# back to the model, which is what makes the expected output fixed.
#
# `pkill` is faked too, and that one is not about determinism. `save` ends with
#     pkill -HUP -x kanshi
# so that a written profile takes effect, and pkill does not care what HOME is:
# an unfaked run signals the REAL kanshi on the developer's live session. Same
# class of bug as the one this file exists because of, one process along.
FAKE="${WORK}/fakebin"; mkdir -p "$FAKE"
for tool in hyprctl wlr-randr pkill; do
    printf '#!/bin/sh\necho "$0 $*" >> "%s/called"\nexit 0\n' "$WORK" > "${FAKE}/${tool}"
    chmod +x "${FAKE}/${tool}"
done

# PATH is REPLACED, not prefixed. Prefixing leaves the real tools reachable, so
# a typo in a fake name silently falls through to the live compositor — the
# failure mode has to be "no tool at all", never "the real one". That means
# python3 has to be named absolutely, since it can no longer be found on PATH.
PY="$(command -v python3)"
[ -x "$PY" ] || { printf 'no python3\n' >&2; exit 1; }
for tool in hyprctl wlr-randr pkill; do
    [ -x "${FAKE}/${tool}" ] || { printf 'fake %s missing\n' "$tool" >&2; exit 1; }
done

# Every invocation below goes through one of these two. Nothing in this file
# calls the generator with the real PATH, so no assertion can reach a real
# compositor even if a future edit forgets which action it is using.
run_save() { PATH="$FAKE" HOME="$1" RIME_DISPLAY_NO_LIVE=1 "$PY" "$GEN" save; }
run_gen()  { local h="$1"; shift; PATH="$FAKE" HOME="$h" RIME_DISPLAY_NO_LIVE=1 "$PY" "$GEN" "$@"; }

section "the model is validated, not trusted"
H="${WORK}/home"; mkdir -p "$H/.config/rime-shell" "$H/.config/hypr" "$H/.config/kanshi"
cat > "$H/.config/rime-shell/display.json" <<'JSON'
{ "outputs": [
  { "name": "eDP-1", "enabled": true, "scale": 99, "transform": "sideways" },
  { "enabled": true, "scale": 1 } ] }
JSON
# The validation notes are printed before the action branch is reached, so they
# are readable even though the guard then refuses this `apply`.
notes="$(run_gen "$H" apply --dry-run 2>&1)"
printf '%s\n' "$notes" | pipe_has 'scale 99' \
    && ok "an out-of-range scale is corrected and reported" || bad "an out-of-range scale is corrected and reported"
printf '%s\n' "$notes" | pipe_has "unknown transform" \
    && ok "an unknown transform is corrected and reported" || bad "an unknown transform is corrected and reported"
printf '%s\n' "$notes" | pipe_has 'no name' \
    && ok "an entry with no output name is skipped and reported" || bad "an entry with no output name is skipped and reported"

section "persistence is written for both backends"
H2="${WORK}/home2"; mkdir -p "$H2/.config/rime-shell" "$H2/.config/hypr" "$H2/.config/kanshi"
cat > "$H2/.config/rime-shell/display.json" <<'JSON'
{ "outputs": [
  { "name": "eDP-1", "enabled": true, "x": 0, "y": 0, "scale": 1.5,
    "transform": "normal", "adaptive_sync": false,
    "mode": { "width": 2560, "height": 1600, "refresh": 165 } },
  { "name": "HDMI-A-1", "enabled": false } ] }
JSON
# `save`, NEVER `apply`. This is the whole reason the two actions are separate:
# `apply` reaches the RUNNING compositor through hyprctl/wlr-randr, and neither
# cares what HOME is. An earlier version of this file called `apply` here and
# pushed the fixture below — 2560x1600@165 at scale 1.5 — onto the live desktop
# it was running on, whose panel is 1920x1200. Isolating HOME is not isolation.
run_save "$H2" >/dev/null 2>&1

K="$H2/.config/kanshi/config"
[ -s "$K" ] && ok "a kanshi config is written" || bad "a kanshi config is written"
if [ -s "$K" ]; then
    grep -q '^profile rime-' "$K" && ok "kanshi: the profile is named for its output set" \
        || bad "kanshi: the profile is named for its output set"
    grep -q 'mode 2560x1600@165' "$K" && ok "kanshi: mode written without the Hz suffix" \
        || bad "kanshi: mode written without the Hz suffix"
    grep -q 'output "HDMI-A-1" disable' "$K" && ok "kanshi: a disabled output is disabled" \
        || bad "kanshi: a disabled output is disabled"
    grep -q 'scale 1.5' "$K" && ok "kanshi: fractional scale written" || bad "kanshi: fractional scale written"
fi

D="$H2/.config/hypr/rime/monitors.lua"
[ -s "$D" ] && ok "a Hyprland monitor file is written" || bad "a Hyprland monitor file is written"
if [ -s "$D" ]; then
    grep -q '^hl.monitor({ output = "eDP-1", mode = "2560x1600@165", position = "0x0", scale = 1.5 })$' "$D" \
        && ok "hyprland: the monitor call matches Hyprland's own Lua API" \
        || bad "hyprland: the monitor call matches Hyprland's own Lua API"
    grep -q '^hl.monitor({ output = "HDMI-A-1", disabled = true })$' "$D" \
        && ok "hyprland: a disabled output is disabled" || bad "hyprland: a disabled output is disabled"
    # The catch-all used to live in the seeded hyprland.conf. It has to be the
    # FIRST rule here now: later rules win, so a fallback emitted last would
    # override every saved layout, and one emitted nowhere leaves a display
    # plugged in after these settings were saved with no rule at all.
    # Asserted on the first RULE, not the first N lines: the comment above it
    # explains why it is there and would silently push it out of a line window.
    [ "$(grep -m1 '^hl\.monitor(' "$D")" \
      = 'hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1.0 })' ] \
        && ok "hyprland: the catch-all rule comes first" \
        || bad "hyprland: the catch-all rule comes first"
    grep -q '^monitor=' "$D" \
        && bad "hyprland: no hyprlang monitor= lines remain" \
        || ok "hyprland: no hyprlang monitor= lines remain"
fi
[ -e "$H2/.config/hypr/rime-display.conf" ] \
    && bad "hyprland: no legacy rime-display.conf is written" \
    || ok "hyprland: no legacy rime-display.conf is written"

# A written kanshi profile does nothing until kanshi re-reads it, so `save` has
# to signal it. Asserted because the failure is silent: the file is correct, the
# layout just never changes.
grep -q 'pkill .*-HUP.*kanshi' "${WORK}/called" 2>/dev/null \
    && ok "save signals kanshi to re-read the profile" \
    || bad "save signals kanshi to re-read the profile"

section "idempotence"
cp "$K" "${WORK}/k.1"; cp "$D" "${WORK}/d.1"
run_save "$H2" >/dev/null 2>&1
cmp -s "$K" "${WORK}/k.1" && cmp -s "$D" "${WORK}/d.1" \
    && ok "re-applying changes nothing" || bad "re-applying changes nothing"

section "a missing helper is a failure, not a traceback"
# The generator falls back to `pgrep` to identify the compositor when the
# environment gives no hint. `run()` did not tolerate a missing binary, so on a
# machine without pgrep on PATH `save` died with a FileNotFoundError instead of
# writing the layout — which is what happens on a CI runner and in any minimal
# container. PATH here holds ONLY the fakes, so pgrep is genuinely absent.
H6="${WORK}/home6"; mkdir -p "$H6/.config/rime-shell" "$H6/.config/hypr" "$H6/.config/kanshi"
cp "$H2/.config/rime-shell/display.json" "$H6/.config/rime-shell/display.json"
out="$(env -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u XDG_CURRENT_DESKTOP \
        -u WAYLAND_DISPLAY PATH="$FAKE" HOME="$H6" RIME_DISPLAY_NO_LIVE=1 \
        "$PY" "$GEN" save 2>&1)"
printf '%s' "$out" | pipe_has -E "Traceback|FileNotFoundError" \
    && { bad "no traceback when a helper is missing"; printf '      %s\n' "$out" | head -3; } \
    || ok "no traceback when a helper is missing"
[ -s "$H6/.config/kanshi/config" ] \
    && ok "the layout is still written with no compositor and no pgrep" \
    || bad "the layout is still written with no compositor and no pgrep"

section "an empty model does nothing"
H3="${WORK}/home3"; mkdir -p "$H3/.config/rime-shell"
echo '{"outputs":[]}' > "$H3/.config/rime-shell/display.json"
run_gen "$H3" save 2>&1 | pipe_has 'nothing to do' \
    && ok "an empty model is a no-op and says so" || bad "an empty model is a no-op and says so"
[ ! -e "$H3/.config/kanshi/config" ] \
    && ok "an empty model writes no persistence" || bad "an empty model writes no persistence"

section "a corrupt model is refused, not guessed at"
H4="${WORK}/home4"; mkdir -p "$H4/.config/rime-shell"
printf '{ not json' > "$H4/.config/rime-shell/display.json"
run_gen "$H4" save 2>&1 | pipe_has 'not usable' \
    && ok "a corrupt model is reported" || bad "a corrupt model is reported"

section "a test can never reach the live compositor"
# Not a comment but a proof: the fake hyprctl/wlr-randr/pkill are the ONLY
# things on PATH, each recording that it was called. If `save` invokes a
# mutating one, the marker exists and this fails.
# The property is that no MUTATING call is made. `save` does enumerate — kanshi
# profiles are keyed on the connected outputs — and `hyprctl -j monitors` /
# `wlr-randr --json` are read-only, so those are expected and fine. What must
# never appear is a call that CHANGES anything: `hyprctl keyword` or a
# `wlr-randr --output`. That distinction is the whole bug: a test isolated HOME,
# assumed that was enough, and reconfigured the live desktop through hyprctl.
mutating() { grep -qE 'hyprctl .*keyword|wlr-randr .*--output' "${WORK}/called" 2>/dev/null; }

rm -f "${WORK}/called"
H5="${WORK}/home5"; mkdir -p "$H5/.config/rime-shell"
cp "$H2/.config/rime-shell/display.json" "$H5/.config/rime-shell/display.json"
run_save "$H5" >/dev/null 2>&1
if mutating; then
    printf '  it called: %s\n' "$(tr '\n' ' ' < "${WORK}/called")"
    bad "save makes no mutating compositor call"
else
    ok "save makes no mutating compositor call"
fi

rm -f "${WORK}/called"
out="$(PATH="$FAKE" HOME="$H5" RIME_DISPLAY_NO_LIVE=1 "$PY" "$GEN" apply 2>&1)"
printf '%s' "$out" | pipe_has 'refusing to touch the live compositor' \
    && ok "apply refuses when RIME_DISPLAY_NO_LIVE is set" \
    || bad "apply refuses when RIME_DISPLAY_NO_LIVE is set"
mutating \
    && bad "the refusal prevents every mutating call" \
    || ok "the refusal prevents every mutating call"

# And without the guard it WOULD mutate — otherwise the guard proves nothing.
#
# `env -u` rather than just leaving the variable out: this is the one call in
# the file that deliberately disables the safety, so it must not depend on the
# ambient environment being clean. CI exports RIME_DISPLAY_NO_LIVE for the whole
# step, which would otherwise turn this assertion into a false failure — and a
# negative control that fails for an unrelated reason gets deleted, taking the
# proof with it.
rm -f "${WORK}/called"
env -u RIME_DISPLAY_NO_LIVE PATH="$FAKE" HOME="$H5" "$PY" "$GEN" apply >/dev/null 2>&1
mutating \
    && ok "without the guard, apply does mutate (so the guard is load-bearing)" \
    || bad "without the guard, apply does mutate (so the guard is load-bearing)"

section "a layout nobody has confirmed yet leaves nothing behind"
# P0-018: `apply` persisted unconditionally, so a session that died during the
# fifteen-second countdown came back at the next login on the layout nobody
# confirmed — kanshi reapplying it on every hotplug, and no transaction left to
# say it was never confirmed. `apply --no-persist` is the fix.
#
# Every assertion here runs with the guard deliberately OFF (`env -u`), because
# the property under test is that a REAL apply — one that does reach the fake
# compositor — writes nothing. With the guard on the run returns before it could
# persist anyway, and the whole section would pass vacuously.
#
# Its own fakes, and its own XDG_CURRENT_DESKTOP. The section above only needs
# to see that a mutating call happened, so a silent hyprctl is enough for it; a
# silent hyprctl is a REJECTION to apply_hypr, which returns 1 before persisting
# and would make "nothing was written" true for the wrong reason. And
# compositor() reads XDG_CURRENT_DESKTOP, so without pinning it this section
# would take the Hyprland path on the developer's machine and the wlr-randr path
# on CI.
NPBIN="${WORK}/np-bin"; mkdir -p "$NPBIN"
cat > "${NPBIN}/hyprctl" <<'SH'
#!/bin/sh
echo "$0 $*" >> "$NP_CALLED"
case "$1" in
    -j) echo "[]" ;;
    *)  echo "ok" ;;
esac
SH
cat > "${NPBIN}/wlr-randr" <<'SH'
#!/bin/sh
echo "$0 $*" >> "$NP_CALLED"
[ "$1" = "--json" ] && echo "[]"
exit 0
SH
printf '#!/bin/sh\necho "$0 $*" >> "$NP_CALLED"\nexit 0\n' > "${NPBIN}/pkill"
chmod +x "${NPBIN}"/hyprctl "${NPBIN}"/wlr-randr "${NPBIN}"/pkill
export NP_CALLED="${WORK}/np-called"

np_home() {
    local h="${WORK}/$1"; mkdir -p "$h/.config/rime-shell"
    cp "$H5/.config/rime-shell/display.json" "$h/.config/rime-shell/display.json"
    printf '%s' "$h"
}
np_run() {
    local h="$1"; shift
    rm -f "$NP_CALLED"
    env -u RIME_DISPLAY_NO_LIVE PATH="$NPBIN" HOME="$h" \
        XDG_CURRENT_DESKTOP=Hyprland "$PY" "$GEN" "$@" 2>&1
}
np_applied() { grep -q 'hyprctl eval' "$NP_CALLED" 2>/dev/null; }
np_hupped()  { grep -q 'pkill -HUP -x kanshi' "$NP_CALLED" 2>/dev/null; }

# Positive control first. If a plain apply did not persist, "--no-persist wrote
# nothing" would be true for the wrong reason and prove nothing at all.
P1="$(np_home np-plain)"
np_run "$P1" apply >/dev/null
if [ -f "$P1/.config/kanshi/config" ] && [ -f "$P1/.config/hypr/rime/monitors.lua" ]; then
    ok "a plain apply DOES persist (so the control is load-bearing)"
else
    bad "a plain apply DOES persist (so the control is load-bearing)"
fi
np_hupped \
    && ok "a plain apply signals kanshi" \
    || bad "a plain apply signals kanshi"

NP="$(np_home np)"
np_run "$NP" apply --no-persist >/dev/null
np_applied \
    && ok "apply --no-persist still reaches the compositor" \
    || bad "apply --no-persist still reaches the compositor"
[ -e "$NP/.config/kanshi/config" ] \
    && bad "apply --no-persist writes no kanshi profile" \
    || ok "apply --no-persist writes no kanshi profile"
[ -e "$NP/.config/hypr/rime/monitors.lua" ] \
    && bad "apply --no-persist writes no Hyprland monitor module" \
    || ok "apply --no-persist writes no Hyprland monitor module"
np_hupped \
    && bad "apply --no-persist sends kanshi no SIGHUP" \
    || ok "apply --no-persist sends kanshi no SIGHUP"

# Keep still has to work: `save` after an unpersisted apply is what promotes the
# layout, and it is the only thing that can.
np_run "$NP" save >/dev/null
if [ -f "$NP/.config/kanshi/config" ] && [ -f "$NP/.config/hypr/rime/monitors.lua" ]; then
    ok "save after --no-persist still persists (Keep is not broken)"
else
    bad "save after --no-persist still persists (Keep is not broken)"
fi

# `save --no-persist` asks for nothing at all. Doing nothing quietly is
# indistinguishable from a successful save to the caller that just promoted a
# model, so it is refused by name.
S1="$(np_home np-save)"
out="$(np_run "$S1" save --no-persist)"; rc=$?
{ [ "$rc" -eq 2 ] && printf '%s' "$out" | pipe_has -- '--no-persist applies to `apply`'; } \
    && ok "save --no-persist is refused, not silently ignored" \
    || bad "save --no-persist is refused, not silently ignored (rc=${rc})"
[ -e "$S1/.config/kanshi/config" ] \
    && bad "the refused save wrote nothing" \
    || ok "the refused save wrote nothing"

# Rime Shell and the OS image land independently, so the shell must be able to
# ask whether the engine it found understands the flag before passing it. --help
# is that answer; an engine without the flag exits 2 on the flag itself.
PATH="$NPBIN" HOME="$NP" "$PY" "$GEN" --help 2>&1 | pipe_has -- '--no-persist' \
    && ok "--no-persist is discoverable in --help (the shell probes for it)" \
    || bad "--no-persist is discoverable in --help (the shell probes for it)"

section "colour management"
# colord is the store; this program is the adapter. The suite never talks to the
# real colord and never creates a real device — `colormgr create-device` at
# `normal` scope writes /var/lib/colord/mapping.db, and MEASURED on 2026-09-07:
# `colormgr delete-device` does NOT remove the device-to-profile rows it leaves
# behind, so a suite that used the real daemon would silently accumulate
# assignments in the developer's own colour database.
#
# /sys is the other thing that cannot be isolated by HOME or PATH, so the
# program names its DRM root and this hands it a fixture tree. Without that,
# every EDID assertion would be an assertion about whatever panel the developer
# happens to have.
CROOT="${WORK}/colour"
mkdir -p "$CROOT/bin" "$CROOT/drm" "$CROOT/icc" "$CROOT/home/.config/rime-shell"

"$PY" - "$CROOT" <<'FIXTURES'
import os, struct, sys
root = sys.argv[1]

def icc(path, tags):
    """A minimal but structurally real ICC profile: 128-byte header with 'acsp'
    at offset 36, a uint32 tag count, then 12-byte (sig, offset, size) entries."""
    header = bytearray(128)
    header[36:40] = b"acsp"
    body = struct.pack(">I", len(tags))
    off = 132 + 12 * len(tags)
    for sig in tags:
        body += sig + struct.pack(">II", off, 8)
        off += 8
    blob = bytes(header) + body + b"\x00" * (8 * len(tags))
    blob = struct.pack(">I", len(blob)) + blob[4:]
    open(path, "wb").write(blob)

icc(os.path.join(root, "icc/with-curve.icc"),  [b"desc", b"vcgt", b"wtpt"])
icc(os.path.join(root, "icc/no-curve.icc"),    [b"desc", b"wtpt"])
open(os.path.join(root, "icc/not-an-icc.icc"), "wb").write(b"this is not a profile" * 16)

# A second not-an-ICC file, and the reason it exists: the one above is caught by
# the tag-count sanity check on its own, because its bytes at offset 128 read as
# 1.77 billion tags. So it does NOT exercise the 'acsp' file-signature check —
# MEASURED, by deleting that check and watching the suite stay green at 67/0.
# This one is a PNG whose bytes at 128..132 happen to say "two tags", which is
# entirely plausible. Without the signature check it parses as a real tag table,
# finds no vcgt among the zeroed entries, and answers "no curve" — silently
# turning "I cannot read this file" into "this file has no calibration in it",
# which are opposite things to tell someone about their monitor profile.
plausible = bytearray(b"\x89PNG\r\n\x1a\n" + b"\x00" * 192)
plausible[128:132] = struct.pack(">I", 2)
open(os.path.join(root, "icc/png-with-plausible-tagcount.icc"), "wb").write(bytes(plausible))

def edid(path, mfg, name, serial, hdr):
    b = bytearray(128)
    b[0:8] = b"\x00\xff\xff\xff\xff\xff\xff\x00"
    packed = 0
    for ch in mfg:
        packed = (packed << 5) | (ord(ch) - 64)
    b[8], b[9] = packed >> 8, packed & 0xFF
    def descriptor(at, tag, text):
        b[at:at+5] = bytes([0, 0, 0, tag, 0])
        t = (text + "\n").ljust(13)[:13].encode("ascii")
        b[at+5:at+18] = t
    descriptor(54, 0xFC, name)
    descriptor(72, 0xFF, serial)
    b[126] = 1
    ext = bytearray(128)
    ext[0] = 0x02          # CTA-861
    ext[1] = 3             # revision
    blocks = b""
    if hdr:
        blocks += bytes([(7 << 5) | 3, 6, 0x0F, 0x00])   # HDR static metadata
        blocks += bytes([(7 << 5) | 3, 5, 0x00, 0x00])   # colorimetry
    else:
        blocks += bytes([(1 << 5) | 3, 0x01, 0x02, 0x03])  # an audio block
    ext[4:4+len(blocks)] = blocks
    ext[2] = 4 + len(blocks)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "wb").write(bytes(b) + bytes(ext))

edid(os.path.join(root, "drm/card0-eDP-1/edid"),   "APX", "PANEL-SDR", "SN0001", False)
edid(os.path.join(root, "drm/card0-DP-2/edid"),    "APX", "PANEL-HDR", "SN0002", True)
os.makedirs(os.path.join(root, "drm/card0-HDMI-A-1"), exist_ok=True)
open(os.path.join(root, "drm/card0-HDMI-A-1/edid"), "wb").write(b"")   # unplugged
FIXTURES

cat > "$CROOT/home/.config/rime-shell/display.json" <<'MODEL'
{"outputs":[
  {"name":"eDP-1","enabled":true,"x":0,"y":0,"scale":1.0,"transform":"normal",
   "adaptive_sync":false,"mode":{"width":1920,"height":1200,"refresh":60}},
  {"name":"DP-2","enabled":true,"x":1920,"y":0,"scale":1.0,"transform":"normal",
   "adaptive_sync":false,"mode":{"width":3840,"height":2160,"refresh":60}},
  {"name":"HDMI-A-1","enabled":true,"x":0,"y":1200,"scale":1.0,"transform":"normal",
   "adaptive_sync":false,"mode":{"width":1280,"height":720,"refresh":60}}
]}
MODEL

# A fake colormgr with a state file, so "the device already exists" is a state
# the suite can reach rather than a branch it has to trust.
cat > "$CROOT/bin/colormgr" <<'CMGR'
#!/bin/sh
S="$CM_STATE"
echo "$*" >> "$S.calls"
# Shell builtins ONLY below — no grep, no tail, no cat. The engine is invoked
# with PATH set to exactly this one directory, which is what guarantees it can
# never reach the real colormgr; the same setting means nothing external is on
# PATH for the fake either. An earlier version of this fake used `grep -qx` to
# ask whether a device already existed, and because grep was not reachable the
# lookup exited 127, which reads as "no such device" — so `find-device` and
# `device-get-default-profile` always failed, `colour_assign` always took the
# create branch, and two assertions here were red against correct engine code.
# A fake that depends on the PATH it is being isolated from is not isolated.
have_device() {
    _hd=1
    [ -f "$S.devices" ] || return 1
    while read -r _d; do [ "$_d" = "$1" ] && _hd=0; done < "$S.devices"
    return $_hd
}
case "$1" in
get-devices)
    [ -s "$S.devices" ] || exit 0
    while read -r d; do printf 'Device ID:   %s\n\n' "$d"; done < "$S.devices"
    ;;
find-device)
    have_device "$2" || { echo "device not found" >&2; exit 1; }
    printf 'Device ID:   %s\n\n' "$2"
    ;;
create-device)
    have_device "$2" && { echo "exists" >&2; exit 1; }
    echo "$2" >> "$S.devices"
    printf 'Device ID:   %s\n\n' "$2"
    ;;
device-make-profile-default)
    printf '%s %s\n' "$2" "$3" >> "$S.defaults"
    ;;
device-get-default-profile)
    # Last writer wins, the way colord's own default does. Device ids are
    # sanitised to alnum and dash by the engine, so splitting on IFS is safe.
    pid=""
    if [ -f "$S.defaults" ]; then
        while read -r _dev _pid; do
            [ "$_dev" = "$2" ] && pid="$_pid"
        done < "$S.defaults"
    fi
    [ -n "$pid" ] || { echo "no profile" >&2; exit 1; }
    case "$pid" in
      icc-with) printf 'Title:   Fixture With Curve\nFilename:   %s\nProfile ID:   icc-with\n\n' "$CM_ICC/with-curve.icc" ;;
      icc-none) printf 'Title:   Fixture No Curve\nFilename:   %s\nProfile ID:   icc-none\n\n' "$CM_ICC/no-curve.icc" ;;
      *) exit 1 ;;
    esac
    ;;
get-profiles)
    printf 'Title:   Fixture With Curve\nType:   display-device\nFilename:   %s\nProfile ID:   icc-with\n\n' "$CM_ICC/with-curve.icc"
    printf 'Title:   Fixture No Curve\nType:   display-device\nFilename:   %s\nProfile ID:   icc-none\n\n' "$CM_ICC/no-curve.icc"
    # colord really does hand out non-display profiles from the same call: the
    # image ships x11-colors.icc, kind named-color, alongside the six display
    # ones. Measured live on the L16.
    printf 'Title:   Fixture Spot Colours\nType:   named-color\nFilename:   %s\nProfile ID:   icc-spot\n\n' "$CM_ICC/with-curve.icc"
    ;;
esac
exit 0
CMGR
chmod +x "$CROOT/bin/colormgr"
for tool in hyprctl wlr-randr pkill; do
    printf '#!/bin/sh\n[ "$1" = "--json" ] && echo "[]"\n[ "$1" = "-j" ] && echo "[]"\nexit 0\n' \
        > "$CROOT/bin/$tool"
    chmod +x "$CROOT/bin/$tool"
done

CM_STATE="${WORK}/cm"
: > "${CM_STATE}.devices"; : > "${CM_STATE}.defaults"; : > "${CM_STATE}.calls"
colour() {
    PATH="$CROOT/bin" HOME="$CROOT/home" \
        RIME_DISPLAY_DRM_ROOT="$CROOT/drm" RIME_DISPLAY_NO_LIVE=1 \
        CM_STATE="$CM_STATE" CM_ICC="$CROOT/icc" \
        XDG_CURRENT_DESKTOP="${1:-Hyprland}" "$PY" "$GEN" "${@:2}"
}
# The same, with nothing named colormgr anywhere on PATH.
colour_nocolord() {
    PATH="${WORK}/emptybin" HOME="$CROOT/home" \
        RIME_DISPLAY_DRM_ROOT="$CROOT/drm" RIME_DISPLAY_NO_LIVE=1 \
        XDG_CURRENT_DESKTOP="${1:-Hyprland}" "$PY" "$GEN" "${@:2}"
}
mkdir -p "${WORK}/emptybin"

jqp() { "$PY" -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

state="$(colour Hyprland color 2>/dev/null)"
printf '%s' "$state" | "$PY" -c 'import json,sys; json.load(sys.stdin)' \
    && ok "color emits valid JSON" || bad "color emits valid JSON"

# ── the vcgt question, answered out of the file ──────────────────────────────
# colord reports a "Gamma Table" line, but the page has to be right on a profile
# colord has never seen, and "could not read it" is a third answer that must not
# collapse into "no curve".
v_with="$(printf '%s' "$state" | jqp '[p["vcgt"] for p in d["profiles"] if p["id"]=="icc-with"]')"
v_none="$(printf '%s' "$state" | jqp '[p["vcgt"] for p in d["profiles"] if p["id"]=="icc-none"]')"
[ "$v_with" = "[True]" ]  && ok "a profile carrying a vcgt tag is reported as having one" \
                          || bad "a profile carrying a vcgt tag is reported as having one (got $v_with)"
[ "$v_none" = "[False]" ] && ok "a profile with no vcgt tag is reported as having none" \
                          || bad "a profile with no vcgt tag is reported as having none (got $v_none)"
noticc="$(PATH="$CROOT/bin" "$PY" -c "
import importlib.util as u, importlib.machinery as mach, sys
s = u.spec_from_loader('e', mach.SourceFileLoader('e', '$GEN')); m = u.module_from_spec(s); s.loader.exec_module(m)
print(m.icc_has_vcgt('$CROOT/icc/not-an-icc.icc'), m.icc_has_vcgt('$CROOT/icc/absent.icc'))")"
[ "$noticc" = "None None" ] \
    && ok "a file that is not an ICC profile answers 'unknown', not 'no curve'" \
    || bad "a file that is not an ICC profile answers 'unknown', not 'no curve' (got $noticc)"
# Specifically the file-signature check, which the fixture above does not reach.
sigcheck="$(PATH="$CROOT/bin" "$PY" -c "
import importlib.util as u, importlib.machinery as mach, sys
s = u.spec_from_loader('e', mach.SourceFileLoader('e', '$GEN')); m = u.module_from_spec(s); s.loader.exec_module(m)
print(m.icc_has_vcgt('$CROOT/icc/png-with-plausible-tagcount.icc'))")"
[ "$sigcheck" = "None" ] \
    && ok "a non-ICC file with a plausible tag count is still 'unknown', not 'no curve'" \
    || bad "a non-ICC file with a plausible tag count is still 'unknown', not 'no curve' (got $sigcheck)"

# ── HDR is read off the panel, not guessed ──────────────────────────────────
hdr_sdr="$(printf '%s' "$state" | jqp '[o["hdr"] for o in d["outputs"] if o["name"]=="eDP-1"][0]["static_metadata"]')"
hdr_hdr="$(printf '%s' "$state" | jqp '[o["hdr"] for o in d["outputs"] if o["name"]=="DP-2"][0]["static_metadata"]')"
col_hdr="$(printf '%s' "$state" | jqp '[o["hdr"] for o in d["outputs"] if o["name"]=="DP-2"][0]["colorimetry"]')"
no_edid="$(printf '%s' "$state" | jqp '[o["hdr"] for o in d["outputs"] if o["name"]=="HDMI-A-1"][0]["edid"]')"
[ "$hdr_sdr" = "False" ] && ok "a panel with no HDR block is not reported as HDR" \
                         || bad "a panel with no HDR block is not reported as HDR"
[ "$hdr_hdr" = "True" ]  && ok "a panel with a CTA HDR static metadata block IS reported as HDR" \
                         || bad "a panel with a CTA HDR static metadata block IS reported as HDR"
[ "$col_hdr" = "True" ]  && ok "the colorimetry block is reported separately from the HDR one" \
                         || bad "the colorimetry block is reported separately from the HDR one"
[ "$no_edid" = "False" ] && ok "an unplugged connector reports no EDID rather than 'not HDR'" \
                         || bad "an unplugged connector reports no EDID rather than 'not HDR'"

# ── the device id travels with the panel, not the socket ────────────────────
# Unplug a calibrated monitor from DP-1 and put a different one there: an id
# keyed on the connector hands the second monitor the first one's profile.
id_edp="$(printf '%s' "$state" | jqp '[o["device"] for o in d["outputs"] if o["name"]=="eDP-1"][0]')"
id_hdmi="$(printf '%s' "$state" | jqp '[o["device"] for o in d["outputs"] if o["name"]=="HDMI-A-1"][0]')"
# rime-rename: keep — the colord device id prefix (colord stores assignments by it)
[ "$id_edp" = "apex-display-APX-PANEL-SDR-SN0001" ] \
    && ok "the colord device id is built from the EDID" \
    || bad "the colord device id is built from the EDID (got $id_edp)"
# rime-rename: keep — the colord device id prefix (colord stores assignments by it)
[ "$id_hdmi" = "apex-display-HDMI-A-1" ] \
    && ok "with no EDID the id falls back to the connector" \
    || bad "with no EDID the id falls back to the connector (got $id_hdmi)"

# ── the curve verdict is per compositor ─────────────────────────────────────
# On the wlroots compositors the gamma LUT is one slot and the night light is
# already in it, so a calibration curve and a night light are the same control.
# Hyprland's night light is a colour matrix, a different slot.
lut_h="$(printf '%s' "$state" | jqp 'd["curve"]["lut_shared_with_night_light"]')"
lut_l="$(colour labwc color 2>/dev/null | jqp 'd["curve"]["lut_shared_with_night_light"]')"
[ "$lut_h" = "False" ] && ok "on Hyprland the night light does not occupy the gamma LUT" \
                       || bad "on Hyprland the night light does not occupy the gamma LUT"
[ "$lut_l" = "True" ]  && ok "on a wlroots compositor a curve and a night light are the same slot" \
                       || bad "on a wlroots compositor a curve and a night light are the same slot"
# Hyprland loads the profile itself (an `icc` monitor rule), so there the
# loader is the compositor; the external-loader search is for the others.
loader_h="$(printf '%s' "$state" | jqp 'd["curve"]["loader"]')"
[ "$loader_h" = "Hyprland" ] && ok "on Hyprland the compositor itself applies the profile" \
                           || bad "on Hyprland the compositor itself applies the profile (got $loader_h)"
loader="$(colour labwc color 2>/dev/null | jqp 'd["curve"]["loader"]')"
[ "$loader" = "None" ] && ok "with no ICC loader installed the page is told so by name" \
                       || bad "with no ICC loader installed the page is told so by name (got $loader)"
printf '#!/bin/sh\nexit 0\n' > "$CROOT/bin/xcalib"; chmod +x "$CROOT/bin/xcalib"
loader2="$(colour labwc color 2>/dev/null | jqp 'd["curve"]["loader"]')"
rm -f "$CROOT/bin/xcalib"
[ "$loader2" = "xcalib" ] \
    && ok "a loader that IS installed is found (so 'none' is a measurement)" \
    || bad "a loader that IS installed is found (so 'none' is a measurement, got $loader2)"

# ── colord absent is a state, not a crash ───────────────────────────────────
nc="$(colour_nocolord Hyprland color 2>/dev/null)"
printf '%s' "$nc" | jqp 'd["colord"]["available"]' | pipe_has -x False \
    && ok "with no colormgr on PATH, colord is reported unavailable" \
    || bad "with no colormgr on PATH, colord is reported unavailable"
printf '%s' "$nc" | jqp 'len(d["outputs"])' | pipe_has -x 3 \
    && ok "the outputs and their EDID verdicts survive colord being absent" \
    || bad "the outputs and their EDID verdicts survive colord being absent"

# ── assignment ──────────────────────────────────────────────────────────────
colour Hyprland --dry-run color-assign eDP-1 icc-with >/dev/null 2>&1
grep -qE 'create-device|make-profile-default' "${CM_STATE}.calls" \
    && bad "--dry-run color-assign changes nothing in colord" \
    || ok "--dry-run color-assign changes nothing in colord"

out="$(colour Hyprland color-assign eDP-1 icc-with 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "color-assign succeeds" || bad "color-assign succeeds (rc=$rc)"
# rime-rename: keep — the colord device id prefix (colord stores assignments by it)
grep -qx "apex-display-APX-PANEL-SDR-SN0001" "${CM_STATE}.devices" \
    && ok "the output was registered with colord under its EDID id" \
    || bad "the output was registered with colord under its EDID id"
# rime-rename: keep — the colord device id prefix (colord stores assignments by it)
grep -q "device-make-profile-default apex-display-APX-PANEL-SDR-SN0001 icc-with" "${CM_STATE}.calls" \
    && ok "the profile was made the device default" \
    || bad "the profile was made the device default"

after="$(colour Hyprland color 2>/dev/null)"
printf '%s' "$after" | jqp '[o["profile"]["title"] for o in d["outputs"] if o["name"]=="eDP-1"][0]' \
    | pipe_has -x "Fixture With Curve" \
    && ok "the assignment is read back by the same verb the page uses" \
    || bad "the assignment is read back by the same verb the page uses"
printf '%s' "$after" | jqp '[o["profile"] for o in d["outputs"] if o["name"]=="DP-2"][0]' \
    | pipe_has -x "None" \
    && ok "an output with no assignment says so rather than inheriting one" \
    || bad "an output with no assignment says so rather than inheriting one"

# Twice. create-device fails on an existing id, so an assign that always creates
# would work exactly once — which is the shape of bug a single-run test misses.
: > "${CM_STATE}.calls"
out2="$(colour Hyprland color-assign eDP-1 icc-none 2>&1)"; rc2=$?
[ "$rc2" -eq 0 ] && ok "assigning again to an already-registered output succeeds" \
                 || bad "assigning again to an already-registered output succeeds (rc=$rc2)"
grep -q "create-device" "${CM_STATE}.calls" \
    && bad "the second assign does not try to create the device again" \
    || ok "the second assign does not try to create the device again"
printf '%s' "$out2" | pipe_has "carries no vcgt" \
    && ok "assigning a profile with no curve says there is no curve to load" \
    || bad "assigning a profile with no curve says there is no curve to load"

# ── on Hyprland an assignment reaches the monitor rule ──────────────────────
# colord only records a profile; nothing reads it on this session. The rule is
# what makes the choice change the picture, so it must land in the persisted
# module — and in the output's FULL rule, not a bare { output, icc }.
mkdir -p "$CROOT/home/.config/rime-shell"
printf '{"outputs":[{"name":"eDP-1","enabled":true,"x":0,"y":0,"scale":1.5}]}' \
    > "$CROOT/home/.config/rime-shell/display.json"
colour Hyprland color-assign eDP-1 icc-none >/dev/null 2>&1
mod="$CROOT/home/.config/hypr/rime/monitors.lua"
grep -qF "icc = \"$CROOT/icc/no-curve.icc\"" "$mod" 2>/dev/null \
    && ok "a Hyprland assignment writes the profile into the output's monitor rule" \
    || bad "a Hyprland assignment writes the profile into the output's monitor rule"
grep -F 'output = "eDP-1"' "$mod" 2>/dev/null | pipe_has -F 'scale = 1.5' \
    && ok "the rule keeps the saved layout around the profile" \
    || bad "the rule keeps the saved layout around the profile"
colour labwc color-assign eDP-1 icc-with 2>&1 | pipe_has "nothing applies an ICC profile" \
    && ok "off Hyprland the page is told the profile does not reach the screen" \
    || bad "off Hyprland the page is told the profile does not reach the screen"
rm -f "$CROOT/home/.config/rime-shell/display.json" "$mod"

# ── a named-colour profile is not a monitor profile ─────────────────────────
# The image ships x11-colors.icc, kind named-color, from the same get-profiles
# call as the six display profiles. It is a table of spot colours; there is
# nothing sensible to do with it on a display. Deliberately given the SAME
# filename as the with-curve fixture, so a filter keying on the file rather than
# on the kind cannot pass this.
kinds="$(printf '%s' "$state" | jqp 'sorted(p["id"] for p in d["profiles"])')"
[ "$kinds" = "['icc-none', 'icc-with']" ] \
    && ok "a named-colour profile is not offered as a monitor profile" \
    || bad "a named-colour profile is not offered as a monitor profile (got $kinds)"
out6="$(colour Hyprland color-assign DP-2 icc-spot 2>&1)"; rc6=$?
{ [ "$rc6" -eq 1 ] && printf '%s' "$out6" | pipe_has "no colord profile matches"; } \
    && ok "assigning a named-colour profile to a display is refused" \
    || bad "assigning a named-colour profile to a display is refused (rc=$rc6)"

out3="$(colour Hyprland color-assign eDP-1 nonsuch 2>&1)"; rc3=$?
{ [ "$rc3" -eq 1 ] && printf '%s' "$out3" | pipe_has "no colord profile matches"; } \
    && ok "an unknown profile is refused and named" \
    || bad "an unknown profile is refused and named (rc=$rc3)"
out4="$(colour_nocolord Hyprland color-assign eDP-1 icc-with 2>&1)"; rc4=$?
{ [ "$rc4" -eq 1 ] && printf '%s' "$out4" | pipe_has "colord is not answering"; } \
    && ok "assignment without colord fails loudly" \
    || bad "assignment without colord fails loudly (rc=$rc4)"
# shellcheck disable=SC2034  # unlike out1..out4 this one is never grepped: the
# assertion below is about the exit status alone. The capture stays because it
# is what keeps the usage message out of the suite's output.
out5="$(colour Hyprland color-assign eDP-1 2>&1)"; rc5=$?
[ "$rc5" -eq 2 ] && ok "color-assign with one argument is a usage error" \
                 || bad "color-assign with one argument is a usage error (rc=$rc5)"

section "live enumeration"
# Needs a real session. Reported honestly rather than stubbed: the whole point
# of enumeration is that it reflects hardware.
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    skp "no Wayland session; cannot enumerate real outputs"
elif ! command -v wlr-randr >/dev/null 2>&1 && ! command -v hyprctl >/dev/null 2>&1; then
    skp "neither wlr-randr nor hyprctl available"
else
    listed="$(python3 "$GEN" list 2>/dev/null)"
    if printf '%s' "$listed" | python3 -c "
import json,sys
o = json.load(sys.stdin)
assert isinstance(o, list) and o, 'no outputs'
for m in o:
    assert m.get('name'), 'an output has no name'
    assert isinstance(m.get('modes'), list), 'modes is not a list'
    assert m.get('transform') is not None
" 2>/dev/null; then
        n="$(printf '%s' "$listed" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
        ok "enumerated ${n} real output(s) with modes"
    else
        bad "enumeration returned usable output data"
    fi

    c="$(python3 "$GEN" compositor 2>/dev/null)"
    case "$c" in
        hyprland|niri|labwc) ok "the compositor is identified (${c})" ;;
        *) bad "the compositor is identified (got '${c}')" ;;
    esac
fi

printf '\nrime-display: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
