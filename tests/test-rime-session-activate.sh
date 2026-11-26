#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-session-activate.sh — graphical-session.target for a compositor
#  greetd execs directly (no uwsm), which is what xdg-desktop-portal requires.
#
#  Drives files/system/libexec/rime-session-activate against a stub systemctl
#  (a tiny fake user manager: an environment file and an active flag) and a
#  stub dbus-update-activation-environment that records what it was asked to
#  import. No session bus, no compositor.
#
#  The failures it exists to catch:
#    1. the target never started, so every portal start fails 'dependency'
#       (no screen sharing, no Flatpak file chooser);
#    2. a target left active by an EARLIER login (a lingering user manager)
#       kept, so the portals talk to a compositor that no longer exists;
#    3. a second run in the same login tearing the session down again;
#    4. the greetd session's XDG_SESSION_TYPE=tty imported as-is;
#    5. a uwsm session (NixOS) having its target stopped from under it;
#    6. a failure reported as success.
#  Plus the wiring: the unit binds graphical-session.target, session.lua and
#  the labwc autostart run the helper, and the image installs both.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$REPO/files/system/libexec/rime-session-activate"
UNIT="$REPO/files/system/units/rime-session.target"
SESSION_LUA="$REPO/files/desktop/hypr/rime/session.lua"
LABWC="$REPO/files/desktop/labwc/autostart"
BASE="$REPO/Containerfile.base"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }

for f in "$HELPER" "$UNIT" "$SESSION_LUA" "$LABWC" "$BASE"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# Fake user manager. $WORK/env = show-environment, $WORK/active = target up,
# $WORK/calls = every systemctl/dbus call, $WORK/fail-start = start fails.
cat > "$WORK/bin/systemctl" <<EOF
#!/usr/bin/env bash
W="$WORK"
printf 'systemctl %s\n' "\$*" >> "\$W/calls"
[[ "\${1:-}" == --user ]] || exit 64
shift
case "\${1:-}" in
    show-environment) cat "\$W/env" 2>/dev/null; exit 0 ;;
    -q) [[ "\$2" == is-active && "\$3" == graphical-session.target ]] || exit 64
        [[ -e "\$W/active" ]]; exit ;;
    stop) [[ "\$2" == graphical-session.target ]] || exit 64; rm -f "\$W/active"; exit 0 ;;
    start) [[ -e "\$W/fail-start" ]] && exit 1
           [[ "\$2" == rime-session.target ]] || exit 64; touch "\$W/active"; exit 0 ;;
    import-environment) shift; for v in "\$@"; do printf '%s=%s\n' "\$v" "\${!v}" >> "\$W/env"; done; exit 0 ;;
esac
exit 64
EOF
cat > "$WORK/bin/dbus-update-activation-environment" <<EOF
#!/usr/bin/env bash
W="$WORK"
printf 'dbus %s\n' "\$*" >> "\$W/calls"
[[ "\${1:-}" == --systemd ]] || exit 64
shift
[[ -e "\$W/fail-dbus" ]] && exit 1
for v in "\$@"; do
    grep -v "^\$v=" "\$W/env" > "\$W/env.new" 2>/dev/null; mv "\$W/env.new" "\$W/env"
    printf '%s=%s\n' "\$v" "\${!v}" >> "\$W/env"
done
EOF
chmod +x "$WORK/bin/systemctl" "$WORK/bin/dbus-update-activation-environment"
printf '0::/user.slice/user-1000.slice/session-3.scope\n' > "$WORK/cgroup.greetd"
printf '0::/user.slice/user-1000.slice/user@1000.service/session.slice/wayland-wm@hyprland.desktop.service\n' \
    > "$WORK/cgroup.uwsm"

reset() { rm -f "$WORK/env" "$WORK/active" "$WORK/calls" "$WORK/fail-start" "$WORK/fail-dbus"; touch "$WORK/env"; }
# run [session id] — the compositor's environment as greetd leaves it.
run() {
    env -i PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK" \
        RIME_SESSION_CGROUP="${CGROUP:-$WORK/cgroup.greetd}" \
        WAYLAND_DISPLAY="${WD-wayland-1}" XDG_CURRENT_DESKTOP=Hyprland XDG_SESSION_TYPE=tty \
        HYPRLAND_INSTANCE_SIGNATURE="${SIG:-sig-a}" XDG_SESSION_ID="${1:-3}" \
        bash "$HELPER" 2> "$WORK/stderr"
}
called() { grep -qxF "$1" "$WORK/calls" 2>/dev/null; }
in_env() { grep -qxF "$1" "$WORK/env"; }

sec "first start of a login"
reset
run 3; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc: $(cat "$WORK/stderr")"
[[ -e "$WORK/active" ]] && ok "rime-session.target started" || bad "rime-session.target started"
in_env "WAYLAND_DISPLAY=wayland-1" && ok "WAYLAND_DISPLAY imported" || bad "WAYLAND_DISPLAY imported"
in_env "HYPRLAND_INSTANCE_SIGNATURE=sig-a" && ok "HYPRLAND_INSTANCE_SIGNATURE imported" || bad "HYPRLAND_INSTANCE_SIGNATURE imported"
in_env "XDG_SESSION_TYPE=wayland" && ok "XDG_SESSION_TYPE imported as wayland, not tty" \
    || bad "XDG_SESSION_TYPE imported as wayland, not tty" "$(grep XDG_SESSION_TYPE "$WORK/env")"
in_env "RIME_SESSION_ID=3" && ok "the login's session id recorded" || bad "the login's session id recorded"
called "systemctl --user stop graphical-session.target" && bad "nothing stopped on a clean start" || ok "nothing stopped on a clean start"
grep -q '^DISPLAY=$' "$WORK/env" && bad "an unset name is not imported as empty" || ok "an unset name is not imported as empty"
# Import strictly before start: a portal started first gets no WAYLAND_DISPLAY.
imp=$(grep -n '^dbus ' "$WORK/calls" | head -1 | cut -d: -f1)
st=$(grep -n 'start rime-session.target' "$WORK/calls" | head -1 | cut -d: -f1)
[[ -n "$imp" && -n "$st" && "$imp" -lt "$st" ]] && ok "environment imported before the target starts" \
    || bad "environment imported before the target starts" "import line ${imp:-none}, start line ${st:-none}"

sec "second run in the same login"
: > "$WORK/calls"
run 3; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc"
called "systemctl --user stop graphical-session.target" && bad "the live session is not torn down" || ok "the live session is not torn down"
grep -q '^dbus ' "$WORK/calls" && bad "nothing re-imported" || ok "nothing re-imported"

sec "target left active by an earlier login"
reset
printf 'WAYLAND_DISPLAY=wayland-1\nHYPRLAND_INSTANCE_SIGNATURE=sig-old\nRIME_SESSION_ID=2\n' > "$WORK/env"
touch "$WORK/active"
SIG=sig-b run 7; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc"
called "systemctl --user stop graphical-session.target" && ok "the stale target is stopped" || bad "the stale target is stopped"
stop=$(grep -n 'stop graphical-session.target' "$WORK/calls" | cut -d: -f1)
imp=$(grep -n '^dbus ' "$WORK/calls" | head -1 | cut -d: -f1)
[[ -n "$stop" && -n "$imp" && "$stop" -lt "$imp" ]] && ok "stopped before the new environment is imported" \
    || bad "stopped before the new environment is imported"
in_env "HYPRLAND_INSTANCE_SIGNATURE=sig-b" && ok "the new compositor's signature replaces the old" || bad "the new compositor's signature replaces the old"
in_env "RIME_SESSION_ID=7" && ok "the new session id recorded" || bad "the new session id recorded"
[[ -e "$WORK/active" ]] && ok "the target is up again" || bad "the target is up again"

sec "a target someone else started (no RIME_SESSION_ID)"
reset; touch "$WORK/active"
run 3; rc=$?
called "systemctl --user stop graphical-session.target" && ok "treated as stale and restarted" || bad "treated as stale and restarted"

sec "uwsm session (NixOS)"
reset; touch "$WORK/active"
CGROUP="$WORK/cgroup.uwsm" run 3; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc"
[[ -s "$WORK/calls" ]] && bad "uwsm's session is left alone" "$(tr '\n' ';' < "$WORK/calls")" || ok "uwsm's session is left alone"

sec "failures are reported"
reset
WD='' run 3; rc=$?
[[ $rc -ne 0 ]] && ok "no WAYLAND_DISPLAY: non-zero" || bad "no WAYLAND_DISPLAY: non-zero"
[[ -e "$WORK/active" ]] && bad "no WAYLAND_DISPLAY: nothing started" || ok "no WAYLAND_DISPLAY: nothing started"
reset; touch "$WORK/fail-start"
run 3; rc=$?
[[ $rc -ne 0 ]] && ok "start failure: non-zero" || bad "start failure: non-zero"
grep -q 'could not start rime-session.target' "$WORK/stderr" && ok "start failure: says so" || bad "start failure: says so"
reset; touch "$WORK/fail-dbus"
run 3; rc=$?
[[ $rc -eq 0 ]] && ok "D-Bus import failure: falls back to systemd import" || bad "D-Bus import failure: falls back to systemd import" "rc=$rc"
in_env "WAYLAND_DISPLAY=wayland-1" && ok "D-Bus import failure: systemd still gets the environment" || bad "D-Bus import failure: systemd still gets the environment"

sec "wiring"
grep -qx 'BindsTo=graphical-session.target' "$UNIT" && ok "unit binds graphical-session.target" || bad "unit binds graphical-session.target"
grep -q '^\[Install\]' "$UNIT" && bad "unit is never enabled (the helper starts it)" || ok "unit is never enabled (the helper starts it)"
# The helper must be the FIRST thing the hyprland.start handler runs.
first=$(awk '/^hl.on\("hyprland.start"/{f=1; next} f && /hl.exec_cmd\(/{print; exit}' "$SESSION_LUA")
[[ "$first" == *'/usr/libexec/rime-session-activate'* ]] && ok "session.lua runs it first at start" \
    || bad "session.lua runs it first at start" "$first"
grep -qE '^\[ -x /usr/libexec/rime-session-activate \] && /usr/libexec/rime-session-activate &$' "$LABWC" \
    && ok "labwc autostart runs it" || bad "labwc autostart runs it"
grep -q 'COPY --chmod=0755 files/system/libexec/rime-session-activate' "$BASE" && ok "image installs the helper" || bad "image installs the helper"
grep -q 'COPY files/system/units/rime-session.target /usr/lib/systemd/user/rime-session.target' "$BASE" \
    && ok "image installs the target" || bad "image installs the target"
if command -v systemd-analyze >/dev/null; then
    systemd-analyze --user verify "$UNIT" >/dev/null 2>&1 && ok "systemd-analyze verify" || bad "systemd-analyze verify"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail -eq 0 ]]
