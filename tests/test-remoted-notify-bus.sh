#!/usr/bin/env bash
# rime-remoted's real notification backend (zbus, src/mirror.rs `Bus`) against
# a fake notification server on a PRIVATE session bus: post, replace, close,
# and the server's NotificationClosed (dismissed, expired, a stranger's fake),
# and a ringing call's buttons (ActionInvoked from the server, and a stranger's fake).
#
#   tests/test-remoted-notify-bus.sh <rime-remoted unit-test binary>
#
# The binary's own test is #[ignore]d and refuses to run unless RIME_REMOTED_FAKE_NOTIFY_BUS=1,
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
# Twice: a server that draws reply fields, and one that does not.
for inline in 1 0; do
export inline
FAKE_NOTIFY_INLINE_REPLY=$inline dbus-run-session --config-file="$work/bus.conf" -- bash -c '
  set -euo pipefail
  rm -f "$work/ready"; mkfifo "$work/ready"
  python3 "$here/fake-notify-server.py" "$work/log$inline.jsonl" >"$work/ready" &
  server=$!
  read -r -t 10 line <"$work/ready" && [ "$line" = ready ] || { echo "FAIL: fake server never ready" >&2; exit 1; }
  RIME_REMOTED_FAKE_NOTIFY_BUS=1 "$bin" --ignored --exact mirror::tests::the_real_backend_posts_replaces_closes_and_hears_dismissals_on_a_fake_server >"$work/out" 2>&1 \
    || { cat "$work/out" >&2; kill "$server"; exit 1; }
  kill "$server"
'
done
python3 - "$work/log1.jsonl" "$work/log0.jsonl" <<'PY'
import json, sys
for path, inline in ((sys.argv[1], True), (sys.argv[2], False)):
    got = [json.loads(l) for l in open(path)]
    ops = [(g["op"], g["id"], g.get("replaces", g.get("reason"))) for g in got]
    replies = []
    for i in range(10):
        replies += [("notify", 6 + i, 0), ("replied", 6 + i, None), ("closed", 6 + i, 2)]
    assert ops == [("notify", 1, 0), ("notify", 1, 1), ("close", 1, None), ("closed", 1, 3),
                   ("notify", 3, 0), ("closed", 3, 1), ("notify", 4, 0), ("closed", 4, 2),
                   ("notify", 5, 0), ("closed", 5, 2)] + replies + [
                   ("notify", 16, 0), ("invoked", 16, None), ("closed", 16, 2)], ops  # the fake's counter also moves on a replace
    n = got[0]
    assert n["app"] == "Pixel 8 · Signal" and n["icon"] == "phone", n
    assert n["summary"] == "-x ‹b›" and n["body"] == "a &amp; b", n
    assert n["actions"] == [] and n["hints"] == {"urgency": "1"} and n["expire"] == -1, n
    # A reply field only where the server draws one.
    r = got[10]
    if inline:
        assert r["actions"] == ["inline-reply", "Reply"], r
        assert r["hints"] == {"urgency": "1", "x-kde-reply-placeholder-text": "Reply"}, r
    else:
        assert r["actions"] == [] and r["hints"] == {"urgency": "1"}, r
    # A ringing call: shown until closed, with buttons only where the server draws them.
    c = got[-3]
    assert c["summary"] == "Call from 5556" and c["expire"] == 0, c
    assert c["actions"] == (["answer", "Answer", "decline", "Decline"] if inline else []), c
print("PASS: posted, replaced and closed over D-Bus; nothing but the safe fields reached the server; "
      "only the server's own dismissals reached the phone; a reply field only where the server draws one, "
      "every answer reached the phone ahead of its close, and only the server's button presses were heard")
PY
