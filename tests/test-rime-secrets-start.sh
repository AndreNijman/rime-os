#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-secrets-start.sh — which Secret Service provider the session
#  starts, how, and what it reports, with fake daemons and a fake bus.
#
#  files/system/libexec/rime-secrets-start runs under a PATH that holds only
#  the fakes below and the few coreutils it needs, so the developer's real
#  gnome-keyring-daemon, gdbus, busctl and systemctl are never reached. The
#  fakes are scripts named after the real programs, so /proc/<pid>/comm reads
#  as it would for the real ones (gnome-keyring-d, oo7-daemon). "The bus" is a
#  file holding the pid that owns org.freedesktop.secrets; the fake gdbus and
#  busctl answer GetConnectionUnixProcessID from it and record every call.
#
#  tests/test-rime-keyring-session-start.sh is the real-daemon proof; this one
#  covers the choices that suite cannot reach on one machine.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# The fakes' bodies and the checks are code written for later expansion.
# shellcheck disable=SC2016
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="$here/../files/system/libexec/rime-secrets-start"
[ -x "$HELPER" ] || { echo "cannot find $HELPER"; exit 2; }

pass=0 fail=0 skip=0
ok()   { echo "PASS  $1"; pass=$((pass + 1)); }
bad()  { echo "FAIL  $1${2:+  — $2}"; fail=$((fail + 1)); }
finish() { echo; echo "secrets-start: $pass passed, $fail failed, $skip skipped"; [ "$fail" -eq 0 ]; exit $?; }

T="$(mktemp -d /tmp/rss.XXXXXX)"
cleanup() {
    local p
    for p in $(cat "$T"/pids 2>/dev/null); do kill "$p" 2>/dev/null; done
    rm -rf "$T"
}
trap cleanup EXIT

# The only system tools the helper may use.
mkdir -p "$T/sys"
for t in bash sh env sed cat tail timeout sleep setsid; do
    p="$(command -v "$t")" || { echo "needs $t"; exit 2; }
    ln -s "$p" "$T/sys/$t"
done

# fake <dir> <name> <body>: an executable script whose comm is <name>.
fake() {
    mkdir -p "$1"
    { printf '#!%s\n' "$(command -v bash)"; printf 'T=%q\n' "$T"; printf '%s\n' "$3"; } > "$1/$2"
    chmod +x "$1/$2"
}
# A daemon that "owns the name": records its pid as the owner, then waits.
SERVE='echo $$ >> "$T/pids"; [ -s "$T/owner" ] || echo $$ > "$T/owner"; while :; do sleep 1; done'

GK_BODY='[ "$1" = --serve ] || echo "gnome-keyring-daemon $*" >> "$T/calls"
case "$1" in
    --serve) '"$SERVE"' ;;
    --start) [ -n "${FAKE_GK_NOSERVE:-}" ] || setsid -f "$0" --serve </dev/null >/dev/null 2>&1 ;;
esac
exit 0'
OO7_BODY='echo "oo7-daemon $*" >> "$T/calls"
'"$SERVE"
GDBUS_BODY='echo "gdbus $*" >> "$T/bus-calls"
p="$(cat "$T/owner" 2>/dev/null)"
if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then echo "(uint32 $p,)"; exit 0; fi
echo "Error: NameHasNoOwner" >&2; exit 1'
BUSCTL_BODY='echo "busctl $*" >> "$T/bus-calls"
p="$(cat "$T/owner" 2>/dev/null)"
if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then echo "u $p"; exit 0; fi
echo "Call failed: no such name" >&2; exit 1'
SYSTEMCTL_BODY='echo "systemctl $*" >> "$T/calls"
[ "$*" = "--user start oo7-daemon.service" ] && setsid -f "$OO7" </dev/null >/dev/null 2>&1
exit 0'

fake "$T/gk" gnome-keyring-daemon "$GK_BODY"
fake "$T/libexec" oo7-daemon "$OO7_BODY"
fake "$T/gdbus" gdbus "$GDBUS_BODY"
fake "$T/busctl" busctl "$BUSCTL_BODY"
fake "$T/systemctl" systemctl "OO7=$T/libexec/oo7-daemon
$SYSTEMCTL_BODY"
printf 'provider=gnome-keyring\nportal=gnome-keyring\n' > "$T/prov-gk"
printf 'provider=oo7\nportal=oo7-portal\n' > "$T/prov-oo7"
printf 'provider=kwallet\n' > "$T/prov-unknown"
mkdir -p "$T/run" && chmod 700 "$T/run"

# run <path dirs, colon-separated> [VAR=value...]: one helper run in a clean
# env; the log is $T/log, the exit status $T/rc.
run() {
    local dirs="$1"; shift
    local p
    for p in $(cat "$T"/pids 2>/dev/null); do kill "$p" 2>/dev/null; done
    rm -f "$T/owner" "$T/calls" "$T/bus-calls" "$T/pids" "$T/log"
    env -i PATH="$dirs:$T/sys" XDG_RUNTIME_DIR="$T/run" DBUS_SESSION_BUS_ADDRESS="unix:path=$T/run/bus" \
        RIME_OO7_DAEMON="$T/libexec/oo7-daemon" RIME_SECRETS_PROVIDER_FILE="$T/none" \
        RIME_SECRETS_CONFIRM_SECS=2 "$@" bash "$HELPER" 2> "$T/log"
    echo $? > "$T/rc"
}
rc()    { cat "$T/rc"; }
calls() { cat "$T/calls" 2>/dev/null; }
logged() { grep -F -- "$1" "$T/log" >/dev/null; }
owner_comm() { cat "/proc/$(cat "$T/owner" 2>/dev/null)/comm" 2>/dev/null; }
check() {  # check <desc> <condition...>
    local d="$1"; shift
    if "$@"; then ok "$d"; else bad "$d" "rc=$(rc) calls=[$(calls | tr '\n' ';')] log=[$(tr '\n' ' ' < "$T/log")]"; fi
}

echo "── which provider"
run "$T/gk:$T/gdbus"
check "no provider file: gnome-keyring, with the exact call the sessions ran before" \
    test "$(calls)" = "gnome-keyring-daemon --start --components=secrets"
check "...the owner is confirmed and logged, exit 0" \
    eval '[ "$(rc)" = 0 ] && logged "org.freedesktop.secrets is served by gnome-keyring (pid $(cat "$T/owner"))"'
check "...and the fake really reads as gnome-keyring-daemon in /proc" test "$(owner_comm)" = gnome-keyring-d

run "$T/gk:$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-gk"
check "provider file gnome-keyring, both installed: gnome-keyring and nothing else" \
    eval '[ "$(rc)" = 0 ] && [ "$(calls)" = "gnome-keyring-daemon --start --components=secrets" ]'

run "$T/gk:$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-oo7"
check "provider file oo7, both installed: oo7-daemon, and gnome-keyring is not touched" \
    eval '[ "$(rc)" = 0 ] && [ "$(calls)" = "oo7-daemon " ] && [ "$(owner_comm)" = oo7-daemon ]'
check "...logged as served by oo7" logged "org.freedesktop.secrets is served by oo7 (pid"

run "$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-gk"
check "gnome-keyring configured but gone, oo7 present: oo7 serves, exit 0" \
    eval '[ "$(rc)" = 0 ] && [ "$(owner_comm)" = oo7-daemon ]'
check "...and the log says the configured one is missing" \
    logged "the configured provider gnome-keyring is not installed; starting oo7 instead"

run "$T/gk:$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-unknown"
check "an unknown provider name: said so, and gnome-keyring is tried first" \
    eval '[ "$(rc)" = 0 ] && logged "unknown provider '"'"'kwallet'"'"'" && [ "$(calls)" = "gnome-keyring-daemon --start --components=secrets" ]'

run "$T/gdbus" RIME_OO7_DAEMON="$T/nonexistent"
check "neither installed: exit 1, nothing started, and the log says so" \
    eval '[ "$(rc)" = 1 ] && [ -z "$(calls)" ] && logged "no Secret Service provider is installed"'

echo "── how oo7 is started"
# A user service manager is "there" when its private socket is.
mkdir -p "$T/run/systemd"
python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$T/run/systemd/private"
run "$T/gdbus:$T/systemctl" RIME_SECRETS_PROVIDER_FILE="$T/prov-oo7"
check "with a user service manager: systemctl --user start oo7-daemon.service, not a stray daemon" \
    eval '[ "$(rc)" = 0 ] && [ "$(calls | head -1)" = "systemctl --user start oo7-daemon.service" ] && [ "$(calls | wc -l)" = 2 ]'
rm -f "$T/run/systemd/private"

run "$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-oo7"
check "without one: the daemon itself, detached, and the helper does not wait for it to exit" \
    eval '[ "$(rc)" = 0 ] && [ "$(calls)" = "oo7-daemon " ]'

# Something else already owns the name: no second oo7, and the mismatch is said.
run "$T/gdbus" RIME_SECRETS_PROVIDER_FILE="$T/prov-oo7" RIME_OO7_DAEMON="$T/nonexistent"
setsid -f "$T/gk/gnome-keyring-daemon" --serve </dev/null >/dev/null 2>&1
for _ in $(seq 50); do [ -s "$T/owner" ] && break; sleep 0.05; done
: > "$T/calls"
env -i PATH="$T/gdbus:$T/sys" XDG_RUNTIME_DIR="$T/run" DBUS_SESSION_BUS_ADDRESS="unix:path=$T/run/bus" \
    RIME_OO7_DAEMON="$T/libexec/oo7-daemon" RIME_SECRETS_PROVIDER_FILE="$T/prov-oo7" \
    RIME_SECRETS_CONFIRM_SECS=2 bash "$HELPER" 2> "$T/log"; echo $? > "$T/rc"
check "name already owned by another provider: no second daemon, exit 1, and the log names both" \
    eval '[ "$(rc)" = 1 ] && [ -z "$(calls)" ] && logged "served by gnome-keyring (pid $(cat "$T/owner")), not by oo7"'

echo "── what it confirms"
t0=$(date +%s%N)
run "$T/gk:$T/gdbus" FAKE_GK_NOSERVE=1 RIME_SECRETS_CONFIRM_SECS=1
ms=$(( ($(date +%s%N) - t0) / 1000000 ))
check "the provider starts but nobody owns the name: exit 1 and the log says so" \
    eval '[ "$(rc)" = 1 ] && logged "nothing owns org.freedesktop.secrets after 1s (started gnome-keyring)"'
check "...within the confirm bound (${ms} ms for a 1 s bound)" test "$ms" -lt 3000

t0=$(date +%s%N)
run "$T/gk:$T/gdbus" DBUS_SESSION_BUS_ADDRESS= XDG_RUNTIME_DIR=
ms=$(( ($(date +%s%N) - t0) / 1000000 ))
check "no session bus: the provider is still started, exit 1, logged, no bus call" \
    eval '[ "$(rc)" = 1 ] && [ "$(calls)" = "gnome-keyring-daemon --start --components=secrets" ] && [ ! -s "$T/bus-calls" ] && logged "no session bus to confirm on"'
check "...and nothing waits (${ms} ms)" test "$ms" -lt 1000

run "$T/gk:$T/gdbus"
check "it asks the bus, never the name itself (a call to the name would activate a locked daemon)" \
    eval '! grep -v -- "--dest org.freedesktop.DBus " "$T/bus-calls" | grep . >/dev/null && grep -F "GetConnectionUnixProcessID org.freedesktop.secrets" "$T/bus-calls" >/dev/null'

run "$T/gk:$T/busctl"
check "without gdbus, busctl answers the same question" \
    eval '[ "$(rc)" = 0 ] && logged "served by gnome-keyring" && grep -F "busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus GetConnectionUnixProcessID s org.freedesktop.secrets" "$T/bus-calls" >/dev/null'

run "$T/gk"
check "with neither gdbus nor busctl: started, exit 1, and the log says the owner is unknown" \
    eval '[ "$(rc)" = 1 ] && logged "neither gdbus nor busctl is installed"'

finish
