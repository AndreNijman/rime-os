#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-keyring-session-start.sh — the keyring PAM unlocked at login is
#  the one the session's apps get, against the SHIPPED rime-shell-autostart.
#
#  pam_gnome_keyring starts `gnome-keyring-daemon --daemonize --login` and
#  unlocks the login keyring. That daemon joins the session bus only when the
#  session runs `gnome-keyring-daemon --start`, and quits on its own after
#  about two minutes if nobody does. Rime's sessions never did, so every app
#  that asked for a secret later (Helium, 2026-10-08) D-Bus-activated a fresh
#  daemon with the keyring LOCKED and the user was asked for their password a
#  second time.
#
#  A real gnome-keyring-daemon on a private bus, a throwaway HOME and a
#  throwaway password. quickshell, hypridle and sudo are stubbed; nothing of
#  the developer's session is read or touched.
#
#    1. without the session step, nobody owns org.freedesktop.secrets: the
#       first app would activate a new, locked daemon;
#    2. after the shipped launcher runs, the daemon PAM started owns it and the
#       login collection is unlocked;
#    3. that daemon is still there after its two-minute login timeout
#       (RIME_KEYRING_QUICK=1 skips this one wait).
#
#  SKIPS without gnome-keyring-daemon, dbus-daemon or busctl/gdbus.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")" || exit 2
LAUNCHER=../files/system/libexec/rime-shell-autostart
[ -f "$LAUNCHER" ] || { echo "cannot find $LAUNCHER"; exit 2; }

pass=0; fail=0; skip=0
ok()   { echo "PASS  $1"; pass=$((pass + 1)); }
bad()  { echo "FAIL  $1${2:+  — $2}"; fail=$((fail + 1)); }
skp()  { echo "SKIP  $1${2:+  — $2}"; skip=$((skip + 1)); }
finish() { echo; echo "keyring-session-start: $pass passed, $fail failed, $skip skipped"; [ "$fail" -eq 0 ]; exit $?; }

for t in gnome-keyring-daemon dbus-daemon gdbus; do
    command -v "$t" >/dev/null 2>&1 || { skp "the login keyring reaches the session" "no $t"; finish; }
done

# Short: a unix socket path must fit in 108 bytes.
T=$(mktemp -d /tmp/rkss.XXXXXX)
bus_pid=""; login_pid=""
cleanup() {
    [ -n "$login_pid" ] && kill "$login_pid" 2>/dev/null
    for p in $(pgrep -u "$(id -u)" -x gnome-keyring-d 2>/dev/null); do
        tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | pipe_has -x "XDG_RUNTIME_DIR=$T/run" && kill "$p" 2>/dev/null
    done
    [ -n "$bus_pid" ] && kill "$bus_pid" 2>/dev/null
    rm -rf "$T"
}
trap cleanup EXIT
mkdir -p "$T/home" "$T/run" "$T/bin" "$T/shell"
chmod 0700 "$T/run"
: > "$T/shell/shell.qml"
for s in quickshell hypridle sudo; do printf '#!/bin/sh\nexit 0\n' > "$T/bin/$s"; chmod +x "$T/bin/$s"; done

# A session bus with NO service directory: nothing can be D-Bus-activated, so
# whoever owns org.freedesktop.secrets got there on its own.
cat > "$T/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path=$T/run/bus</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy>
</busconfig>
EOF
bus_pid=$(dbus-daemon --config-file="$T/bus.conf" --fork --print-pid) || { skp "the login keyring reaches the session" "dbus-daemon did not start"; finish; }
BUS="unix:path=$T/run/bus"
sess() { env -i HOME="$T/home" XDG_RUNTIME_DIR="$T/run" DBUS_SESSION_BUS_ADDRESS="$BUS" PATH="$T/bin:/usr/bin:/bin" "$@"; }

owner_pid() {
    sess gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.GetConnectionUnixProcessID org.freedesktop.secrets 2>/dev/null \
        | sed -n 's/^(uint32 \([0-9]*\),)$/\1/p'
}

# What pam_gnome_keyring does at login: start the daemon, hand it the password.
printf 'rime-test-pw' | env -i HOME="$T/home" XDG_RUNTIME_DIR="$T/run" PATH=/usr/bin:/bin \
    gnome-keyring-daemon --daemonize --login >/dev/null 2>&1
for _ in $(seq 1 30); do [ -S "$T/run/keyring/control" ] && break; sleep 0.1; done
for p in $(pgrep -u "$(id -u)" -x gnome-keyring-d); do
    tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | pipe_has -x "XDG_RUNTIME_DIR=$T/run" && login_pid=$p
done
[ -n "$login_pid" ] || { bad "the login daemon started" "no daemon under $T/run"; finish; }

# 1. Before the session step.
if [ -z "$(owner_pid)" ]; then ok "without the session step nobody serves secrets (an app would start a locked daemon)"
else bad "without the session step nobody serves secrets" "owned by pid $(owner_pid)"; fi

# 2. The shipped launcher, as a session runs it.
sess RIME_SHELL_DIR="$T/shell" RIME_SHELL_WAIT_TIMEOUT=2 bash "$LAUNCHER" >"$T/out" 2>&1 </dev/null
got=$(owner_pid)
if [ "$got" = "$login_pid" ]; then ok "the session's secrets come from the daemon PAM unlocked"
else bad "the session's secrets come from the daemon PAM unlocked" "owner '$got', login daemon $login_pid; $(tail -2 "$T/out")"; fi
locked=$(sess gdbus call --session --dest org.freedesktop.secrets \
    --object-path /org/freedesktop/secrets/collection/login \
    --method org.freedesktop.DBus.Properties.Get org.freedesktop.Secret.Collection Locked 2>&1)
if [[ "$locked" == *"false"* ]]; then ok "the login keyring is unlocked, with no prompt"
else bad "the login keyring is unlocked, with no prompt" "$locked"; fi

# 3. Past the login daemon's own timeout.
if [ "${RIME_KEYRING_QUICK:-0}" = 1 ]; then
    skp "the login daemon outlives its two-minute timeout" "RIME_KEYRING_QUICK=1"
else
    sleep 135
    if kill -0 "$login_pid" 2>/dev/null && [ "$(owner_pid)" = "$login_pid" ]; then
        ok "the login daemon outlives its two-minute timeout"
    else
        bad "the login daemon outlives its two-minute timeout" "alive: $(kill -0 "$login_pid" 2>/dev/null && echo yes || echo no), owner '$(owner_pid)'"
    fi
fi
finish
