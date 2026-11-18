#!/usr/bin/env bash
# rime-remoted's real phone-media backend (zbus, src/media.rs `Bus`) on a
# PRIVATE session bus, read back with playerctl: the player is listed, its
# metadata and status read as sent, its position moves with the clock, one
# --ignore-player=rime_phone leaves it out, a control call changes nothing,
# an update shows, and removal takes it off the bus.
#
#   tests/test-remoted-media-bus.sh <rime-remoted unit-test binary>
#
# The binary's test is #[ignore]d and refuses to run unless
# RIME_MEDIA_BUS_PRIVATE equals DBUS_SESSION_BUS_ADDRESS, which only this
# script arranges, inside dbus-run-session with a config that has no service
# directories, so nothing on this machine can be activated or reached.
set -euo pipefail
bin=${1:?usage: $0 <rime-remoted unit-test binary>}
if ! command -v playerctl >/dev/null || ! command -v dbus-run-session >/dev/null; then
  if [ "${RIME_TEST_REQUIRE_PLAYERCTL:-0}" = 1 ]; then echo "FAIL: playerctl or dbus-run-session missing" >&2; exit 1; fi
  echo "SKIP: playerctl or dbus-run-session missing"; exit 0
fi
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
cat >"$work/bus.conf" <<'CONF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
export bin work
# A computer player beside the phone's, so "leaves out the phone" is also
# "keeps the computer's".
dbus-run-session --config-file="$work/bus.conf" -- bash -c '
  set -euo pipefail
  export RIME_MEDIA_BUS_PRIVATE=$DBUS_SESSION_BUS_ADDRESS
  CARGO_PKG_NAME=rime-remoted "$bin" --ignored --exact \
    media::tests::the_bus_backend_exports_a_player_mpris_readers_read_and_playerctl_leaves_out >"$work/out" 2>&1 \
    || { cat "$work/out" >&2; exit 1; }
  grep -q "test result: ok. 1 passed" "$work/out" || { cat "$work/out" >&2; exit 1; }
'
echo "PASS: the phone's player is on the bus as sent, its position follows the clock, playerctl's" \
     "--ignore-player=rime_phone leaves it out, control is refused, and removal takes it off the bus"
