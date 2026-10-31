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
#  second time. The launcher now does it through rime-secrets-start.
#
#  RIME_SECRETS_PROVIDER picks the provider under test:
#
#    gnome-keyring (default)  what Rime ships. A real gnome-keyring-daemon on a
#      private bus, a throwaway HOME and a throwaway password:
#        1. without the session step, nobody owns org.freedesktop.secrets: the
#           first app would activate a new, locked daemon;
#        2. after the shipped launcher runs, the daemon PAM started owns it and
#           the login collection is unlocked;
#        3. the launcher's log names the owner (rime-secrets-start confirms it);
#        4. that daemon is still there after its two-minute login timeout
#           (RIME_KEYRING_QUICK=1 skips this one wait).
#
#    oo7  Fedora 45's provider, on a machine with NO gnome-keyring (CI runs it
#      in a fedora:45 container with only oo7-daemon installed). A guarded Rime
#      image cannot be this machine (the build refuses oo7-daemon); this is the
#      helper's fallback, proved against the real daemon. Nothing starts oo7 at
#      login in Rime, and oo7-daemon ships no D-Bus service file, so a session
#      that only knows gnome-keyring leaves apps with no Secret Service at all:
#        0. no D-Bus service file on this system claims org.freedesktop.secrets
#           (so on a real session bus, too, nothing would activate one);
#        1. before the session step nobody owns org.freedesktop.secrets;
#        2. after the launcher, oo7-daemon owns it;
#        3. it answers as a Secret Service (it has a login collection);
#        4. the launcher's log names the owner.
#      Its login keyring is locked (Rime gives oo7 no password): that is the
#      fallback's stated cost, and not asserted away.
#
#  RIME_TEST_LAUNCHER runs another copy of the launcher (an older release's,
#  to show what this suite catches). quickshell, hypridle and sudo are stubbed;
#  nothing of the developer's session is read or touched.
#
#  SKIPS without the provider's daemon, dbus-daemon or gdbus, and in oo7 mode
#  when gnome-keyring-daemon is installed (the helper would rightly pick it).
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# RIME_TEST_LAUNCHER is read against the caller's directory, before the cd.
LAUNCHER="${RIME_TEST_LAUNCHER:-$(dirname "$0")/../files/system/libexec/rime-shell-autostart}"
[ -f "$LAUNCHER" ] || { echo "cannot find $LAUNCHER"; exit 2; }
LAUNCHER="$(realpath -- "$LAUNCHER")"
cd "$(dirname "$0")" || exit 2
HELPER="$(pwd)/../files/system/libexec/rime-secrets-start"
PROVIDER_FILE="$(pwd)/../files/system/rime-secrets-provider"
[ -x "$HELPER" ] || { echo "cannot find $HELPER"; exit 2; }
PROVIDER="${RIME_SECRETS_PROVIDER:-gnome-keyring}"
OO7_DAEMON="${RIME_OO7_DAEMON:-/usr/libexec/oo7-daemon}"

pass=0; fail=0; skip=0
ok()   { echo "PASS  $1"; pass=$((pass + 1)); }
bad()  { echo "FAIL  $1${2:+  — $2}"; fail=$((fail + 1)); }
skp()  { echo "SKIP  $1${2:+  — $2}"; skip=$((skip + 1)); }
finish() { echo; echo "keyring-session-start: $pass passed, $fail failed, $skip skipped"; [ "$fail" -eq 0 ]; exit $?; }

echo "provider under test: $PROVIDER"
for t in dbus-daemon gdbus; do
    command -v "$t" >/dev/null 2>&1 || { skp "the session gets a Secret Service" "no $t"; finish; }
done
case "$PROVIDER" in
    gnome-keyring)
        command -v gnome-keyring-daemon >/dev/null 2>&1 \
            || { skp "the login keyring reaches the session" "no gnome-keyring-daemon"; finish; } ;;
    oo7)
        [ -x "$OO7_DAEMON" ] || { skp "oo7 serves the session" "no $OO7_DAEMON"; finish; }
        if command -v gnome-keyring-daemon >/dev/null 2>&1; then
            skp "oo7 serves the session" "gnome-keyring-daemon is installed; run this where only oo7-daemon is"
            finish
        fi ;;
    *) echo "RIME_SECRETS_PROVIDER must be gnome-keyring or oo7, not '$PROVIDER'"; exit 2 ;;
esac

# Short: a unix socket path must fit in 108 bytes.
T=$(mktemp -d /tmp/rkss.XXXXXX)
bus_pid=""; login_pid=""
cleanup() {
    [ -n "$login_pid" ] && kill "$login_pid" 2>/dev/null
    for p in $(pgrep -u "$(id -u)" -x gnome-keyring-d 2>/dev/null) $(pgrep -u "$(id -u)" -x oo7-daemon 2>/dev/null); do
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
bus_pid=$(dbus-daemon --config-file="$T/bus.conf" --fork --print-pid) \
    || { skp "the session gets a Secret Service" "dbus-daemon did not start"; finish; }
BUS="unix:path=$T/run/bus"
sess() { env -i HOME="$T/home" XDG_RUNTIME_DIR="$T/run" DBUS_SESSION_BUS_ADDRESS="$BUS" PATH="$T/bin:/usr/bin:/bin" "$@"; }

owner_pid() {
    sess gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.GetConnectionUnixProcessID org.freedesktop.secrets 2>/dev/null \
        | sed -n 's/^(uint32 \([0-9]*\),)$/\1/p'
}

# The launcher, as a session runs it, with the repo's helper and provider file.
launch() {
    sess RIME_SHELL_DIR="$T/shell" RIME_SHELL_WAIT_TIMEOUT=2 \
        RIME_SECRETS_START="$HELPER" RIME_SECRETS_PROVIDER_FILE="$PROVIDER_FILE" \
        RIME_OO7_DAEMON="$OO7_DAEMON" \
        bash "$LAUNCHER" >"$T/out" 2>&1 </dev/null
}

if [ "$PROVIDER" = oo7 ]; then
    # 0. The installed system's activation files (the private bus below has
    #    no service directory, so step 1 alone could not show this).
    claims=""
    for d in /usr/share/dbus-1/services /usr/local/share/dbus-1/services; do
        [ -d "$d" ] || continue
        for f in "$d"/*.service; do
            [ -f "$f" ] && pipe_has -x 'Name=org.freedesktop.secrets' "$f" && claims="$claims $f"
        done
    done
    if [ -z "$claims" ]; then ok "no D-Bus service file claims org.freedesktop.secrets (oo7-daemon ships none)"
    else bad "no D-Bus service file claims org.freedesktop.secrets" "claimed by:$claims"; fi

    # 1. Before the session step.
    if [ -z "$(owner_pid)" ]; then ok "without the session step nobody serves secrets"
    else bad "without the session step nobody serves secrets" "owned by pid $(owner_pid)"; fi

    # 2. The launcher.
    launch
    got=$(owner_pid)
    comm=$(cat "/proc/${got:-0}/comm" 2>/dev/null)
    if [ -n "$got" ] && [ "$comm" = oo7-daemon ]; then ok "the session's secrets come from oo7-daemon"
    else bad "the session's secrets come from oo7-daemon" "owner '${got}' ('${comm}'); $(tail -3 "$T/out" | tr '\n' ' ')"; fi

    # 3. It answers as a Secret Service.
    cols=$(sess timeout 5 gdbus call --session --dest org.freedesktop.secrets --object-path /org/freedesktop/secrets \
        --method org.freedesktop.DBus.Properties.Get org.freedesktop.Secret.Service Collections 2>&1)
    if [[ "$cols" == *"/org/freedesktop/secrets/collection/login"* ]]; then ok "it answers as a Secret Service with a login collection"
    else bad "it answers as a Secret Service with a login collection" "$cols"; fi

    # 4. The log says who serves it.
    if pipe_has -F "org.freedesktop.secrets is served by oo7 (pid ${got:-?})" "$T/out"; then ok "the launcher's log names the owner"
    else bad "the launcher's log names the owner" "$(tr '\n' ' ' < "$T/out")"; fi
    finish
fi

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
launch
got=$(owner_pid)
if [ "$got" = "$login_pid" ]; then ok "the session's secrets come from the daemon PAM unlocked"
else bad "the session's secrets come from the daemon PAM unlocked" "owner '$got', login daemon $login_pid; $(tail -2 "$T/out")"; fi
locked=$(sess gdbus call --session --dest org.freedesktop.secrets \
    --object-path /org/freedesktop/secrets/collection/login \
    --method org.freedesktop.DBus.Properties.Get org.freedesktop.Secret.Collection Locked 2>&1)
if [[ "$locked" == *"false"* ]]; then ok "the login keyring is unlocked, with no prompt"
else bad "the login keyring is unlocked, with no prompt" "$locked"; fi

# 3. The log says who serves it.
if pipe_has -F "org.freedesktop.secrets is served by gnome-keyring (pid $login_pid)" "$T/out"; then
    ok "the launcher's log names the owner"
else
    bad "the launcher's log names the owner" "$(tr '\n' ' ' < "$T/out")"
fi

# 4. Past the login daemon's own timeout.
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
