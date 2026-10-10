#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-gaming-discord.sh — Discord activity in Gaming Mode: the setting,
#  how Equibop is found, when it is refused, what it is started with, and that
#  it stops with its owner (Steam).
#
#  Equibop is a fake that writes its argv and environment down and sleeps, in
#  a private HOME / XDG_RUNTIME_DIR. A fake `flatpak` covers the Flathub build.
#  The wrapper is run too (rime-gamescope-steam around `sleep`), so the hook
#  that ships is the one tested.
#
#  Run from anywhere:  ./tests/test-rime-gaming-discord.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="${HELPER:-${ROOT}/files/system/libexec/rime-gaming-discord}"
WRAP="${ROOT}/files/system/libexec/rime-gamescope-steam"

pass=0
fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); [ -n "${2:-}" ] && printf '      %s\n' "$2"; }
# Conditions are strings, evaluated here: shellcheck cannot see the variables
# they read (SC2034 on rc / epid is that, not a dead assignment).
check() { if eval "$2"; then ok "$1"; else bad "$1" "${3:-}"; fi; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-gaming-discord-XXXXXX")"
PIDS=()
cleanup() {
    for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
    pkill -f "${WORK}/" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

# fresh_home: a new HOME with nothing installed and the setting absent.
fresh_home() {
    H="${WORK}/home$((++N))"
    RUN="${H}/run"
    mkdir -p "${H}/bin" "${RUN}" "${H}/.config" "${H}/.local/share/applications"
    chmod 700 "${RUN}"
    REC="${H}/rec"
}
N=0

# install_native: a fake Equibop wrapper in ~/.local/bin, a desktop entry that
# launches it with a field code, and Equibop's own settings.
install_native() {
    local arrpc="${1:-true}" first="${2:-false}"
    cat > "${H}/bin/equibop-real" <<FAKE
#!/usr/bin/env bash
{ printf 'ARGV'; printf ' %s' "\$@"; printf '\n'; env; } > "${REC}"
st="${H}/.config/equibop/state.json"
trap 'echo stopped >> "${REC}.stopped"; [ -f "\$st" ] && python3 -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d.update(windowBounds={\"width\": 1279, \"height\": 719}, maximized=False, updater={\"snoozeUntil\": 5}); json.dump(d, open(p, \"w\"))" "\$st"; exit 0' TERM
while :; do sleep 0.1; done
FAKE
    chmod +x "${H}/bin/equibop-real"
    mkdir -p "${H}/.local/bin"
    printf '#!/bin/sh\nexport WRAPPED=1\nexec "%s" "$@"\n' "${H}/bin/equibop-real" > "${H}/.local/bin/equibop"
    chmod +x "${H}/.local/bin/equibop"
    cat > "${H}/.local/share/applications/equibop.desktop" <<EOF
[Desktop Entry]
Name=Equibop
Exec=${H}/.local/bin/equibop --no-sandbox %U
Type=Application
[Desktop Action New]
Exec=${H}/bin/wrong-one
EOF
    mkdir -p "${H}/.config/equibop"
    printf '{"arRPC": %s, "minimizeToTray": true}\n' "$arrpc" > "${H}/.config/equibop/settings.json"
    if [ "$first" = false ]; then
        printf '{"firstLaunch": false, "maximized": true, "windowBounds": {"width": 1920, "height": 1080}}\n' \
            > "${H}/.config/equibop/state.json"
    else
        printf '{"lastElectronVersion": "43.7.1"}\n' > "${H}/.config/equibop/state.json"
    fi
}

hx() {
    env -i HOME="$H" XDG_RUNTIME_DIR="$RUN" PATH="${H}/bin:/usr/bin:/bin" \
        XDG_DATA_DIRS="${H}/nosys" RIME_GAMING_DISCORD_POLL=0.2 \
        RIME_GAMING_DISCORD_GRACE=2 "$@"
}
helper() { hx python3 "$HELPER" "$@"; }
# A run that should refuse, bounded: a broken refusal must FAIL, not hang.
refused() { hx timeout 10 python3 "$HELPER" "$@"; }
js() { helper status --json | python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# wait_for COND SECONDS
wait_for() {
    local i=0 n=$(( $2 * 10 ))
    while [ $i -lt $n ]; do eval "$1" && return 0; sleep 0.1; i=$((i + 1)); done
    return 1
}

# start_owner: a stand-in for Steam.
start_owner() { sleep 300 & OWNER=$!; PIDS+=("$OWNER"); }

printf '\n\033[1m── nothing installed ──\033[0m\n'
fresh_home
check "status: not installed" '[ "$(js "[\"installed\"]")" = False ]'
check "status: not ready, says why" '[ "$(js "[\"reason\"]")" = "Equibop is not installed." ]'
check "the setting is off until it is turned on" '[ "$(js "[\"enabled\"]")" = False ]'
start_owner
refused run --owner "$OWNER" 2>"${H}/log"; rc=$?
check "run with nothing installed exits 0 and starts nothing" '[ $rc = 0 ] && [ ! -e "$REC" ]'
kill "$OWNER"

printf '\n\033[1m── the setting ──\033[0m\n'
fresh_home
mkdir -p "${H}/.config/rime"
printf '{"other": 7}\n' > "${H}/.config/rime/gaming.json"
helper set on >/dev/null
check "set on writes discord_presence: true" \
    '[ "$(python3 -c "import json; print(json.load(open(\"${H}/.config/rime/gaming.json\"))[\"discord_presence\"])")" = True ]'
check "set on keeps the file's other keys" \
    '[ "$(python3 -c "import json; print(json.load(open(\"${H}/.config/rime/gaming.json\"))[\"other\"])")" = 7 ]'
check "status reads it back" '[ "$(js "[\"enabled\"]")" = True ]'
helper set off >/dev/null
check "set off turns it off" '[ "$(js "[\"enabled\"]")" = False ]'
check "set rejects anything but on|off" '! helper set maybe 2>/dev/null'

printf '\n\033[1m── off: nothing starts ──\033[0m\n'
fresh_home; install_native
start_owner
refused run --owner "$OWNER" 2>"${H}/log"
check "setting off: Equibop is not started" '[ ! -e "$REC" ]'
check "…and the log says the setting is off" 'grep -q "is off" "${H}/log"'
kill "$OWNER"

printf '\n\033[1m── refusals ──\033[0m\n'
fresh_home; install_native false; helper set on >/dev/null
check "arRPC off in Equibop: not ready" '[ "$(js "[\"ready\"]")" = False ] && [ "$(js "[\"rich_presence\"]")" = False ]'
check "…the reason names Equibop's own setting" 'js "[\"reason\"]" | grep -q "Rich Presence is off in Equibop"'
start_owner; refused run --owner "$OWNER" 2>/dev/null
check "…and run does not start it" '[ ! -e "$REC" ]'
kill "$OWNER"

fresh_home; install_native true true; helper set on >/dev/null
check "never opened (no firstLaunch=false): not ready" '[ "$(js "[\"set_up\"]")" = False ] && [ "$(js "[\"ready\"]")" = False ]'
start_owner; refused run --owner "$OWNER" 2>/dev/null
check "…and run does not start it (its welcome window would take the screen)" '[ ! -e "$REC" ]'
kill "$OWNER"

fresh_home; install_native; helper set on >/dev/null
printf '{"arRPC": true, "arRPCDisabled": true}\n' > "${H}/.config/equibop/settings.json"
check "arRPCDisabled wins over arRPC" '[ "$(js "[\"rich_presence\"]")" = False ]'

fresh_home; install_native; helper set on >/dev/null
sleep 300 & held=$!; PIDS+=("$held")
ln -s "myhost-${held}" "${H}/.config/equibop/SingletonLock"
start_owner; refused run --owner "$OWNER" 2>"${H}/log"
check "Equibop already running (live SingletonLock): no second launch" '[ ! -e "$REC" ]'
check "…and the log says so" 'grep -q "already running (pid ${held})" "${H}/log"'
kill "$OWNER" "$held"
rm -f "${H}/.config/equibop/SingletonLock"
ln -s "myhost-999999" "${H}/.config/equibop/SingletonLock"
check "a stale SingletonLock does not block it" '[ "$(js "[\"ready\"]")" = True ]'

check "run refuses an owner that is not running" '! refused run --owner 999999 2>/dev/null'

printf '\n\033[1m── run: how it is started ──\033[0m\n'
fresh_home; install_native; helper set on >/dev/null
check "ready with arRPC on and set up" '[ "$(js "[\"ready\"]")" = True ]'
check "found through its desktop entry" 'js "[\"source\"]" | grep -q "equibop.desktop$"'
start_owner
hx SteamOS=1 SteamGamepadUI=1 XDG_CURRENT_DESKTOP=gamescope WAYLAND_DISPLAY=gamescope-0 \
    DISPLAY=:1 PULSE_SERVER=unix:/run/user/1/pulse/native \
    python3 "$HELPER" run --owner "$OWNER" 2>"${H}/log" &
HP=$!; PIDS+=("$HP")
wait_for '[ -s "$REC" ]' 10
check "Equibop started" '[ -s "$REC" ]' "$(cat "${H}/log")"
check "…through the desktop entry's Exec (the user's own wrapper)" 'grep -qx "WRAPPED=1" "$REC"'
check "…with its Exec arguments, field code dropped" 'head -1 "$REC" | grep -q "^ARGV --no-sandbox --start-minimized" && ! grep -q "%U" "$REC"'
check "…hidden, on X11, without the GPU, muted" 'head -1 "$REC" | grep -q -- "--start-minimized --ozone-platform=x11 --disable-gpu --mute-audio$"'
check "…not the desktop action's Exec" '! grep -q wrong-one "$REC"'
check "…without SteamOS / SteamGamepadUI (Equibop's Deck full screen)" '! grep -q "^SteamOS=" "$REC" && ! grep -q "^SteamGamepadUI=" "$REC"'
check "…without WAYLAND_DISPLAY" '! grep -q "^WAYLAND_DISPLAY=" "$REC"'
check "…muted (PULSE_SERVER points nowhere)" 'grep -qx "PULSE_SERVER=unix:/nonexistent/rime-gaming-discord" "$REC"'
# PULSE_SERVER alone let Chromium fall back to ALSA -> PipeWire: notification
# sounds played in Gaming Mode (2026-10-10).
check "…and PIPEWIRE_REMOTE points nowhere (no ALSA fallback into PipeWire)" 'grep -qx "PIPEWIRE_REMOTE=/nonexistent/rime-gaming-discord" "$REC"'
check "…keeps DISPLAY (gamescope's Xwayland)" 'grep -qx "DISPLAY=:1" "$REC"'
# shellcheck disable=SC2034
epid="$(pgrep -f "${H}/bin/equibop-real" | head -1)"
check "…at nice 10" '[ -n "$epid" ] && [ "$(ps -o ni= -p "$epid" | tr -d " ")" = 10 ]'
check "…and cannot raise itself (RLIMIT_NICE at most 10)" \
    '[ -n "$epid" ] && awk "/^Max nice priority/ { exit !(\$4 <= 10 && \$5 <= 10) }" "/proc/${epid}/limits"'
check "…in its own session (stopped as a group)" '[ -n "$epid" ] && [ "$(ps -o sid= -p "$epid" | tr -d " ")" = "$epid" ]'
check "status reports it running" '[ "$(js "[\"running\"]")" = True ]'
check "a second run while one is running starts nothing new" \
    'refused run --owner "$OWNER" 2>&1 | grep -q "already running"'

kill "$OWNER"
wait_for '! kill -0 "$HP" 2>/dev/null' 10
check "owner (Steam) exits: the helper exits" '! kill -0 "$HP" 2>/dev/null'
check "…and Equibop was stopped" '[ -e "${REC}.stopped" ] && ! pgrep -f "${H}/bin/equibop-real" >/dev/null'
check "…and the pidfile is gone" '[ ! -e "${RUN}/rime-gaming-discord.pid" ]'
check "…and the log says why" 'grep -q "Equibop stopped: owner exited" "${H}/log"'
sj() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))$1)" "${H}/.config/equibop/state.json"; }
check "Equibop's window size is put back after the hidden run" \
    '[ "$(sj "[\"windowBounds\"][\"width\"]")" = 1920 ] && [ "$(sj "[\"maximized\"]")" = True ]'
check "…and only that: what else it wrote stays" '[ "$(sj "[\"updater\"][\"snoozeUntil\"]")" = 5 ]'

printf '\n\033[1m── a session that may not raise priority at all ──\033[0m\n'
# The kernel's default hard RLIMIT_NICE is 0. Raising a hard limit is refused
# unprivileged, so the cap must only ever lower it.
rm -f "$REC" "${REC}.stopped"
start_owner
hx prlimit --nice=0:0 python3 "$HELPER" run --owner "$OWNER" 2>"${H}/log" & HP=$!; PIDS+=("$HP")
wait_for '[ -s "$REC" ]' 10
check "hard RLIMIT_NICE 0: Equibop still starts" '[ -s "$REC" ]' "$(cat "${H}/log")"
kill "$OWNER"
wait_for '! kill -0 "$HP" 2>/dev/null' 10

printf '\n\033[1m── stop ──\033[0m\n'
rm -f "$REC" "${REC}.stopped"
start_owner
helper run --owner "$OWNER" 2>"${H}/log" & HP=$!; PIDS+=("$HP")
wait_for '[ -s "$REC" ]' 10
helper stop
check "stop ends the helper and Equibop, the owner still running" \
    '! kill -0 "$HP" 2>/dev/null && [ -e "${REC}.stopped" ] && kill -0 "$OWNER"'
check "stop with nothing running is a no-op" 'helper stop'
kill "$OWNER"

printf '\n\033[1m── Equibop that quits by itself ──\033[0m\n'
rm -f "$REC"
cat > "${H}/bin/equibop-real" <<FAKE
#!/usr/bin/env bash
{ printf 'ARGV'; printf ' %s' "\$@"; printf '\n'; } > "${REC}"
exit 3
FAKE
start_owner
timeout 10 env -i HOME="$H" XDG_RUNTIME_DIR="$RUN" PATH="${H}/bin:/usr/bin:/bin" \
    XDG_DATA_DIRS="${H}/nosys" RIME_GAMING_DISCORD_POLL=0.2 python3 "$HELPER" run --owner "$OWNER" 2>"${H}/log"
# shellcheck disable=SC2034
rc=$?
check "the helper returns when Equibop exits (does not hold up gamescope)" '[ $rc = 0 ] && grep -q "exited by itself (3)" "${H}/log"'
kill "$OWNER"

printf '\n\033[1m── no desktop entry: ~/.local/bin/equibop ──\033[0m\n'
fresh_home; install_native; helper set on >/dev/null
rm "${H}/.local/share/applications/equibop.desktop"
check "found as ~/.local/bin/equibop" '[ "$(js "[\"source\"]")" = "${H}/.local/bin/equibop" ]'

printf '\n\033[1m── Flatpak ──\033[0m\n'
fresh_home; helper set on >/dev/null
FPID=io.github.equicord.equibop
cat > "${H}/bin/flatpak" <<FAKE
#!/usr/bin/env bash
if [ "\$1" = kill ]; then echo "kill \$2" >> "${REC}.kill"; pkill -f "${H}/bin/flatpak run" ; exit 0; fi
{ printf 'ARGV'; printf ' %s' "\$@"; printf '\n'; env; } > "${REC}"
mkdir -p "${RUN}/app/${FPID}"; : > "${RUN}/app/${FPID}/discord-ipc-0"
trap 'exit 0' TERM
while :; do sleep 0.1; done
FAKE
chmod +x "${H}/bin/flatpak"
mkdir -p "${H}/.local/share/flatpak/exports/share/applications" "${H}/.var/app/${FPID}/config/equibop"
cat > "${H}/.local/share/flatpak/exports/share/applications/${FPID}.desktop" <<EOF
[Desktop Entry]
Name=Equibop
Exec=${H}/bin/flatpak run --branch=stable --arch=x86_64 --command=equibop --file-forwarding ${FPID} @@u %U @@
EOF
printf '{"arRPC": true}\n' > "${H}/.var/app/${FPID}/config/equibop/settings.json"
printf '{"firstLaunch": false}\n' > "${H}/.var/app/${FPID}/config/equibop/state.json"
check "Flatpak found, its settings read from ~/.var/app" '[ "$(js "[\"kind\"]")" = flatpak ] && [ "$(js "[\"ready\"]")" = True ]'
start_owner
helper run --owner "$OWNER" 2>"${H}/log" & HP=$!; PIDS+=("$HP")
wait_for '[ -L "${RUN}/discord-ipc-0" ]' 10
check "flatpak run gets --nosocket=pulseaudio --socket=x11 before the app id" \
    'head -1 "$REC" | grep -q "^ARGV run --nosocket=pulseaudio --socket=x11 --env=PIPEWIRE_REMOTE=/nonexistent/rime-gaming-discord --branch=stable"'
check "…and the hidden flags after it, without @@ markers" \
    'head -1 "$REC" | grep -q "${FPID} --start-minimized --ozone-platform=x11 --disable-gpu --mute-audio$" && ! grep -q "@@" "$REC"'
check "the Flatpak's socket is linked where games look" \
    '[ "$(readlink "${RUN}/discord-ipc-0")" = "${RUN}/app/${FPID}/discord-ipc-0" ]'
kill "$OWNER"
wait_for '! kill -0 "$HP" 2>/dev/null' 10
check "owner exits: flatpak kill was used" 'grep -q "kill ${FPID}" "${REC}.kill"'
check "…and the link is removed" '[ ! -L "${RUN}/discord-ipc-0" ]'

fresh_home; helper set on >/dev/null
cp "${WORK}/home$((N-1))/bin/flatpak" "${H}/bin/flatpak"
sed -i "s|${WORK}/home$((N-1))|${H}|g" "${H}/bin/flatpak"
mkdir -p "${H}/.local/share/flatpak/exports/share/applications" "${H}/.var/app/${FPID}/config/equibop"
cat > "${H}/.local/share/flatpak/exports/share/applications/${FPID}.desktop" <<EOF
[Desktop Entry]
Exec=${H}/bin/flatpak run ${FPID} @@u %U @@
EOF
printf '{"arRPC": true}\n' > "${H}/.var/app/${FPID}/config/equibop/settings.json"
printf '{"firstLaunch": false}\n' > "${H}/.var/app/${FPID}/config/equibop/state.json"
: > "${RUN}/discord-ipc-0"
start_owner
helper run --owner "$OWNER" 2>"${H}/log" & HP=$!; PIDS+=("$HP")
wait_for '[ -s "$REC" ]' 10; sleep 0.6
check "an existing discord-ipc-0 is never replaced" '[ -f "${RUN}/discord-ipc-0" ] && [ ! -L "${RUN}/discord-ipc-0" ]'
kill "$OWNER"
wait_for '! kill -0 "$HP" 2>/dev/null' 10
check "…nor removed" '[ -f "${RUN}/discord-ipc-0" ]'

printf '\n\033[1m── the Gaming Mode wrapper starts it ──\033[0m\n'
fresh_home; install_native; helper set on >/dev/null
hx RIME_GAME_ENV_HELPER=/nonexistent DISPLAY= RIME_GAMING_HDR=steam RIME_GAMING_FORCE_COMPOSITE=0 \
    bash "$WRAP" sleep 3 2>"${H}/log" &
WP=$!
wait_for '[ -s "$REC" ]' 10
check "rime-gamescope-steam starts Equibop next to Steam" '[ -s "$REC" ]' "$(cat "${H}/log")"
wait "$WP"
wait_for '[ -e "${REC}.stopped" ]' 10
check "…and it is stopped when Steam (the wrapper's exec) exits" '[ -e "${REC}.stopped" ]'

fresh_home; install_native
hx RIME_GAME_ENV_HELPER=/nonexistent DISPLAY= RIME_GAMING_HDR=steam RIME_GAMING_FORCE_COMPOSITE=0 \
    bash "$WRAP" sleep 1 2>"${H}/log"
sleep 0.5
check "setting off: the wrapper starts nothing" '[ ! -e "$REC" ]'

printf '\n\033[1m── session cleanup ──\033[0m\n'
check "rime-gaming-session's cleanup stops it" \
    'grep -q "rime-gaming-discord}\" stop" "${ROOT}/files/system/libexec/rime-gaming-session"'

printf '\n%s: %d passed, %d failed\n' "$(basename "$0" .sh)" "$pass" "$fail"
[ "$fail" -eq 0 ]
