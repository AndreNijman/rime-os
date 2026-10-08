#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-game-env.sh — the game environment Steam gets, on the desktop and
#  in Gaming Mode, from one list (files/system/libexec/rime-game-env).
#
#  The helper runs against fixture PCI trees (RIME_SYSFS). The desktop path is
#  measured the way greetd starts a session: a shell sources the profile.d
#  hook and then execs the program, which reports what it inherited. Every
#  shell that sources profile.d on Rime (sh, bash, zsh; dash where present) is
#  tried. Gaming Mode's wrapper has its own suite
#  (tests/test-rime-gamescope-steam.sh), which runs this same helper.
#
#  Run from anywhere:  ./tests/test-rime-game-env.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="${ROOT}/files/system/libexec/rime-game-env"
HOOK="${ROOT}/files/system/profile.d/rime-game-env.sh"
WRAP="${ROOT}/files/system/libexec/rime-gamescope-steam"

pass=0
fail=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); [ -n "${2:-}" ] && printf '      %s\n' "$2"; }
skip() { printf 'SKIP  %s\n' "$1"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-game-env-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

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
EMPTY="${WORK}/sys-empty"; mkdir -p "$EMPTY/bus/pci/devices"

# The helper with a clean slate: none of the variables it decides on inherited
# from whoever runs the suite.
CLEAN=(-u VKD3D_DISABLE_EXTENSIONS -u __GL_SHADER_DISK_CACHE_SIZE -u RIME_GAMING_OMM -u RIME_GAME_ENV_HELPER)
helper() { env "${CLEAN[@]}" "$@" "$HELPER" 2>"${WORK}/err"; }
lines()  { tr '\n' ';'; }

# ── The helper ──────────────────────────────────────────────────────────────
out="$(helper RIME_SYSFS="$KAT" | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap;" ] \
    && ok "pre-Ada NVIDIA (GA104): 12 GB shader cache, opacity micromaps off" \
    || bad "pre-Ada NVIDIA" "out: $out"
[ ! -s "${WORK}/err" ] && ok "without -v the helper writes nothing to stderr" \
    || bad "quiet by default" "stderr: $(cat "${WORK}/err")"

out="$(helper RIME_SYSFS="$ADA" | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] \
    && ok "Ada (and its audio function, an older device id): micromaps left on" \
    || bad "Ada" "out: $out"
out="$(helper RIME_SYSFS="$AMD" | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] && ok "AMD only: micromaps left on" \
    || bad "AMD" "out: $out"
out="$(helper RIME_SYSFS="$EMPTY" | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] && ok "no PCI devices at all: no error, cache size only" \
    || bad "empty sysfs" "out: $out; err: $(cat "${WORK}/err")"

out="$(helper RIME_SYSFS="$KAT" __GL_SHADER_DISK_CACHE_SIZE=3000000000 | lines)"
[ "$out" = "VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap;" ] && ok "a cache size the user set is kept" \
    || bad "user cache size" "out: $out"
out="$(helper RIME_SYSFS="$AMD" __GL_SHADER_DISK_CACHE_SIZE= | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] && ok "an empty cache size is treated as unset" \
    || bad "empty cache size" "out: $out"
out="$(helper RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_EXT_mesh_shader | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;VKD3D_DISABLE_EXTENSIONS=VK_EXT_mesh_shader,VK_KHR_opacity_micromap;" ] \
    && ok "a list the user set is kept and appended to" || bad "append" "out: $out"
out="$(helper RIME_SYSFS="$KAT" VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap,VK_EXT_mesh_shader | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] && ok "already in the list: not added twice" \
    || bad "no duplicate" "out: $out"
out="$(helper RIME_SYSFS="$KAT" RIME_GAMING_OMM=on | lines)"
[ "$out" = "__GL_SHADER_DISK_CACHE_SIZE=12000000000;" ] && ok "RIME_GAMING_OMM=on keeps micromaps on" \
    || bad "RIME_GAMING_OMM=on" "out: $out"
out="$(helper RIME_SYSFS="$KAT" __GL_SHADER_DISK_CACHE_SIZE=12000000000 VKD3D_DISABLE_EXTENSIONS=VK_KHR_opacity_micromap | lines)"
[ -z "$out" ] && ok "an environment it already produced: nothing to print" || bad "idempotent" "out: $out"

env "${CLEAN[@]}" RIME_SYSFS="$KAT" "$HELPER" -v >/dev/null 2>"${WORK}/err"
grep -q "older than Ada" "${WORK}/err" && ok "-v says why micromaps are off" \
    || bad "-v reason" "stderr: $(cat "${WORK}/err")"
env "${CLEAN[@]}" "$HELPER" --bogus >/dev/null 2>&1; rc=$?
[ "$rc" = 2 ] && ok "an unknown argument is a usage error (2)" || bad "usage" "exit $rc"

# ── The desktop: a session shell sources the hook, then execs the program ──
# What greetd runs: /bin/sh -c '. /etc/profile; … exec <compositor>'. The
# "compositor" here prints what it inherited; its children (Rime Shell, the
# launcher, a terminal, Steam) inherit the same.
REPORT='printf "%s|%s" "${__GL_SHADER_DISK_CACHE_SIZE-UNSET}" "${VKD3D_DISABLE_EXTENSIONS-UNSET}"'
session() {  # shell [ENV…]: the session's environment as Steam would see it
    local sh="$1"; shift
    # shellcheck disable=SC2016  # expanded by the session shell, not here
    env "${CLEAN[@]}" RIME_GAME_ENV_HELPER="$HELPER" "$@" \
        "$sh" -c '. "$1"; exec bash -c "$2"' session "$HOOK" "$REPORT" 2>"${WORK}/err"
}
twice() {  # shell: the hook sourced by the login shell and again by an interactive one
    local sh="$1"
    # shellcheck disable=SC2016
    env "${CLEAN[@]}" RIME_GAME_ENV_HELPER="$HELPER" RIME_SYSFS="$KAT" \
        "$sh" -c '. "$1"; . "$1"; exec bash -c "$2"' session "$HOOK" "$REPORT" 2>/dev/null
}

for sh in sh bash zsh dash; do
    if ! command -v "$sh" >/dev/null 2>&1; then skip "$sh is not installed"; continue; fi
    got="$(session "$sh" RIME_SYSFS="$KAT")"
    [ "$got" = "12000000000|VK_KHR_opacity_micromap" ] && [ ! -s "${WORK}/err" ] \
        && ok "$sh session, pre-Ada NVIDIA: Steam inherits both, and the hook printed nothing" \
        || bad "$sh session, pre-Ada" "got: $got; stderr: $(cat "${WORK}/err")"
    got="$(session "$sh" RIME_SYSFS="$ADA")"
    [ "$got" = "12000000000|UNSET" ] && ok "$sh session, Ada: cache size only" \
        || bad "$sh session, Ada" "got: $got"
    got="$(session "$sh" RIME_SYSFS="$KAT" __GL_SHADER_DISK_CACHE_SIZE=5 VKD3D_DISABLE_EXTENSIONS="VK_EXT_a b")"
    [ "$got" = "5|VK_EXT_a b,VK_KHR_opacity_micromap" ] && ok "$sh session: the user's values are kept (spaces too)" \
        || bad "$sh session, user values" "got: $got"
    got="$(twice "$sh")"
    [ "$got" = "12000000000|VK_KHR_opacity_micromap" ] && ok "$sh: sourced twice, nothing doubled" \
        || bad "$sh twice" "got: $got"
done

if command -v zsh >/dev/null 2>&1; then
    # Fedora's /etc/zprofile reads /etc/profile under `emulate sh`.
    # shellcheck disable=SC2016
    got="$(env "${CLEAN[@]}" RIME_GAME_ENV_HELPER="$HELPER" RIME_SYSFS="$KAT" \
        zsh -c 'emulate sh -c ". \"\$1\""; exec bash -c "$2"' session "$HOOK" "$REPORT" 2>/dev/null)"
    [ "$got" = "12000000000|VK_KHR_opacity_micromap" ] && ok "zsh under emulate sh (Fedora's zprofile)" \
        || bad "zsh emulate sh" "got: $got"
fi

# No helper (a tier without it): the hook does nothing and says nothing.
# shellcheck disable=SC2016
got="$(env "${CLEAN[@]}" RIME_GAME_ENV_HELPER="${WORK}/missing" \
    sh -c '. "$1"; exec bash -c "$2"' session "$HOOK" "$REPORT" 2>"${WORK}/err")"
[ "$got" = "UNSET|UNSET" ] && [ ! -s "${WORK}/err" ] && ok "no helper: the hook is silent and changes nothing" \
    || bad "no helper" "got: $got; stderr: $(cat "${WORK}/err")"
# The hook's loop variable does not leak into the session.
# shellcheck disable=SC2016
got="$(env "${CLEAN[@]}" RIME_GAME_ENV_HELPER="$HELPER" RIME_SYSFS="$KAT" \
    sh -c '. "$1"; printf %s "${_rime_game_env-UNSET}"' session "$HOOK")"
[ "$got" = UNSET ] && ok "the hook leaves no variable of its own behind" || bad "hook leak" "got: $got"

# ── Both paths agree ────────────────────────────────────────────────────────
# Gaming Mode's wrapper runs the same helper (it finds it next to itself), so
# Steam sees the same values whichever way it was started.
# shellcheck disable=SC2016
gaming="$(env "${CLEAN[@]}" -u DISPLAY RIME_SYSFS="$KAT" bash "$WRAP" bash -c "$REPORT" 2>/dev/null)"
desktop="$(session sh RIME_SYSFS="$KAT")"
[ -n "$gaming" ] && [ "$gaming" = "$desktop" ] && ok "Gaming Mode and the desktop give Steam the same values ($gaming)" \
    || bad "paths agree" "gaming: $gaming; desktop: $desktop"
# A Gaming Mode session read the hook through /etc/profile first: the wrapper
# then changes nothing, and still logs why micromaps are off.
# shellcheck disable=SC2016
again="$(env "${CLEAN[@]}" -u DISPLAY RIME_GAME_ENV_HELPER="$HELPER" RIME_SYSFS="$KAT" \
    sh -c '. "$1"; exec bash "$2" bash -c "$3"' session "$HOOK" "$WRAP" "$REPORT" 2>"${WORK}/err")"
[ "$again" = "12000000000|VK_KHR_opacity_micromap" ] && grep -q "older than Ada" "${WORK}/err" \
    && ok "profile then wrapper: applied once, reason still logged" \
    || bad "profile then wrapper" "got: $again; log: $(cat "${WORK}/err")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
