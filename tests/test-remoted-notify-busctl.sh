#!/usr/bin/env bash
# rime-remoted's real notification backend (busctl --user) against a fake
# notification server on a PRIVATE session bus: post, replace, close.
#
#   tests/test-remoted-notify-busctl.sh <rime-remoted unit-test binary>
#
# The binary's own test refuses to run unless RIME_REMOTED_FAKE_NOTIFY_BUS=1,
# which only this script sets, inside dbus-run-session with a config that has
# no service directories, so nothing on this machine can be activated or reached.
set -euo pipefail
bin=${1:?usage: $0 <rime-remoted unit-test binary>}
here=$(cd "$(dirname "$0")" && pwd)
if ! python3 -c 'import gi; gi.require_version("Gio", "2.0")' 2>/dev/null; then
  if [ "${RIME_TEST_REQUIRE_GI:-0}" = 1 ]; then echo "FAIL: python3-gi missing" >&2; exit 1; fi
  echo "SKIP: python3-gi missing"; exit 0
fi
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
cat >"$work/bus.conf" <<'CONF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
export bin here work
dbus-run-session --config-file="$work/bus.conf" -- bash -c '
  set -euo pipefail
  mkfifo "$work/ready"
  python3 "$here/fake-notify-server.py" "$work/log.jsonl" >"$work/ready" &
  server=$!
  read -r -t 10 line <"$work/ready" && [ "$line" = ready ] || { echo "FAIL: fake server never ready" >&2; exit 1; }
  RIME_REMOTED_FAKE_NOTIFY_BUS=1 "$bin" --exact mirror::tests::the_real_backend_posts_replaces_and_closes_on_a_fake_server >"$work/out" 2>&1 \
    || { cat "$work/out" >&2; kill "$server"; exit 1; }
  kill "$server"
'
python3 - "$work/log.jsonl" <<'PY'
import json, sys
got = [json.loads(l) for l in open(sys.argv[1])]
ops = [(g["op"], g["id"], g.get("replaces")) for g in got]
assert ops == [("notify", 1, 0), ("notify", 1, 1), ("close", 1, None)], ops
n = got[0]
assert n["app"] == "Pixel 8 · Signal" and n["icon"] == "phone", n
assert n["summary"] == "-x ‹b›" and n["body"] == "a &amp; b", n
assert n["actions"] == [] and n["hints"] == {"urgency": "1"} and n["expire"] == -1, n
print("PASS: posted, replaced and closed through busctl; nothing but the safe fields reached the server")
PY
