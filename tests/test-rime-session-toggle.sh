#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-session-toggle.sh — Gaming Mode as a toggle: the one-shot autologin
#  that files/system/libexec/rime-session-select leaves for greetd, and the
#  files/system/libexec/rime-greetd wrapper that spends it.
#
#  Everything privileged is a stand-in (RIME_SWITCH_TOOLS): loginctl, systemctl,
#  systemd-run and getent record their argv. Nothing touches the real greetd,
#  logind or sessions. Run from anywhere.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 2
SEL=../files/system/libexec/rime-session-select
GRT=../files/system/libexec/rime-greetd
T=$(mktemp -d /tmp/rime-toggle-test.XXXXXX); trap 'rm -rf "$T"' EXIT
pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1" "${3:-}"; fi; }

mkdir -p "$T"/{bin,sessions,state,run}
for tool in loginctl systemctl systemd-run; do
    printf '#!/bin/sh\necho "%s $*" >> "%s/calls"\n' "$tool" "$T" > "$T/bin/$tool"
done
printf '#!/bin/sh\n[ "$2" = 1000 ] && echo "andre:x:1000:1000::/home/andre:/bin/zsh"\n' > "$T/bin/getent"
chmod +x "$T"/bin/*
printf '[Desktop Entry]\nName=Desktop\nExec=/usr/bin/start-hyprland\n' > "$T/sessions/hyprland.desktop"
printf '[Desktop Entry]\nName=Gaming Mode\nExec=/usr/libexec/rime-gaming-session\n' > "$T/sessions/rime-gaming.desktop"
printf '[Desktop Entry]\nName=Odd\nExec=sh -c "evil; $(x)"\n' > "$T/sessions/odd.desktop"
printf '[terminal]\nvt = 1\n\n[default_session]\ncommand = "greeter"\nuser = "greetd"\n' > "$T/greetd.toml"

sel() {
    : > "$T/calls"
    SUDO_UID="${UIDV-1000}" RIME_SWITCH_TOOLS="$T/bin" RIME_SESSION_DIR="$T/sessions" \
    RIME_GREET_STATE_DIR="$T/state" RIME_GREET_RUN_DIR="$T/run" RIME_GREETD_CONFIG="$T/greetd.toml" \
        bash "$SEL" "$@" >"$T/out" 2>&1
}

# 1. Desktop -> Gaming Mode: one-shot for the caller's own account and session.
sel rime-gaming --switch --from hyprland
check "the switch writes a one-shot" '[ -f "$T/run/switch.toml" ]'
check "it is 0600" '[ "$(stat -c %a "$T/run/switch.toml")" = 600 ]'
check "it logs in the caller (uid 1000 = andre), not a chosen user" 'grep -qx "user = \"andre\"" "$T/run/switch.toml"'
check "it runs the session the way the greeter does" "grep -qxF \"command = \\\"sh -lc 'exec /usr/libexec/rime-gaming-session'\\\"\" \"\$T/run/switch.toml\"" "$(cat "$T/run/switch.toml")"
check "it keeps the normal config (default_session)" 'grep -q "^\[default_session\]" "$T/run/switch.toml"'
check "exactly one [initial_session]" '[ "$(grep -c "^\[initial_session\]" "$T/run/switch.toml")" = 1 ]'
check "the finisher runs outside the session (systemd-run)" 'grep -q "^systemd-run .*--finish-switch 1000" "$T/calls"' "$(cat "$T/calls")"
check "it does not terminate-user (that would end SSH logins too)" '! grep -q "terminate-user" "$T/calls"'
check "it remembers where to come back to" '[ "$(cat "$T/state/last-desktop")" = hyprland ]'
check "and preselects the target" '[ "$(cat "$T/state/last-session")" = rime-gaming ]'

# 2. Back out: --desktop names the remembered desktop.
sel --desktop
check "--desktop names the session the switch came from" '[ "$(cat "$T/out")" = hyprland ]'
echo rime-gaming > "$T/state/last-desktop"; sel --desktop
check "--desktop never names Gaming Mode itself" '[ "$(cat "$T/out")" = hyprland ]'
echo '../etc' > "$T/state/last-desktop"; sel --desktop
check "--desktop ignores a junk memory" '[ "$(cat "$T/out")" = hyprland ]'

# 3. Refusals.
rm -f "$T/run/switch.toml"
sel nosuch --switch
check "an unknown session is refused" '[ ! -f "$T/run/switch.toml" ] && grep -q "not an installed session" "$T/out"'
sel odd --switch
check "a session whose Exec is not plain gets no one-shot" '[ ! -f "$T/run/switch.toml" ]'
check "...and falls back to the greeter" 'grep -q "terminate-user 1000" "$T/calls"'
UIDV="" sel rime-gaming --switch
check "no SUDO_UID, no switch" '[ ! -f "$T/run/switch.toml" ] && grep -q "SUDO_UID" "$T/out"'
UIDV=4242 sel rime-gaming --switch
check "an account that does not exist is refused" '[ ! -f "$T/run/switch.toml" ]'
sel --finish-switch 1000
check "the finisher refuses to be run through sudo" 'grep -q "internal" "$T/out" && ! grep -q "systemctl restart" "$T/calls"'

# 4. rime-greetd spends the one-shot once, and refuses stale/unsafe ones.
printf '#!/bin/sh\necho greetd "$@" > "%s/greetd"\n' "$T" > "$T/bin/greetd"; chmod +x "$T/bin/greetd"
grt() { RIME_GREET_RUN_DIR="$T/run" RIME_GREETD_BIN="$T/bin/greetd" RIME_SWITCH_OWNER="$(id -u)" bash "$GRT" 2>/dev/null; }
sel rime-gaming --switch --from hyprland
grt
check "greetd starts on the one-shot" 'grep -qx "greetd --config $T/run/switch.active.toml" "$T/greetd"' "$(cat "$T/greetd")"
check "the one-shot is spent" '[ ! -e "$T/run/switch.toml" ]'
grt
check "the next greetd start is a normal one" 'grep -qx "greetd" "$T/greetd" && [ ! -e "$T/run/switch.active.toml" ]'
sel rime-gaming --switch --from hyprland; touch -d '-5 min' "$T/run/switch.toml"; grt
check "a stale one-shot is refused and deleted" 'grep -qx "greetd" "$T/greetd" && [ ! -e "$T/run/switch.toml" ]'
sel rime-gaming --switch --from hyprland; chmod 0644 "$T/run/switch.toml"; grt
check "a world-readable one-shot is refused" 'grep -qx "greetd" "$T/greetd" && [ ! -e "$T/run/switch.toml" ]'
sel rime-gaming --switch --from hyprland; mv "$T/run/switch.toml" "$T/real"; ln -s "$T/real" "$T/run/switch.toml"; grt
check "a symlinked one-shot is refused" 'grep -qx "greetd" "$T/greetd"'

echo
echo "rime-session-toggle: $pass passed, $fail failed"
[ "$fail" = 0 ]
