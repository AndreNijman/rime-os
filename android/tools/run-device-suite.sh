#!/usr/bin/env bash
#
# Run the on-device suite: P1-060's first two criteria, which are claims about
# what Android does with this code and which no JVM test can make.
#
# It stands up `rime-agentd` and `rime-remoted` FROM THIS WORKTREE in their own
# XDG root, puts a line-JSON broker in front of `rime-remoted`'s control socket
# so the phone can ask the real daemon for a real pairing offer, builds both
# APKs, installs them, and runs the instrumentation.
#
# It NEVER touches the live runtime. Both daemons get their own
# XDG_RUNTIME_DIR, XDG_STATE_HOME and RIME_AGENT_SCRATCH_ROOT, and both are
# stopped by the pid this script started — nothing here looks a process up by
# name, because `pkill rime-agentd` would take out the session its owner is
# working in.
#
# Usage:
#   android/tools/run-device-suite.sh [-s <adb serial>] [-c <test class>]
#
# With RIME_DEVICE_SUITE_NEXD=<path to a nexd binary> it also starts a nexd of
# its own (a scripted provider, a scratch git folder, its socket inside this
# run's XDG_RUNTIME_DIR, where rime-remoted looks by default), and the nex
# tests run. Without it they are left out BY NAME and the verdict says so: nex
# lives in its own repository, so this tree cannot build one, and a run that
# reported nex as tested when no nexd was there would be the defect this
# header warns about below.
#
# The SMS tests (MessagesOnDeviceTest) run only on an emulator: they inject a
# message with `adb emu sms send` and send real SMS to emulator numbers. On any
# other serial they are left out BY NAME and the verdict says so, and the
# broker's `emu_sms` verb refuses a serial that is not `emulator-*`. The call
# tests (CallsOnDeviceTest) are the same: they ring the emulator with `adb emu
# gsm call` from an emulator number and dial emulator numbers only, so they
# are left out by name elsewhere and `emu_gsm` refuses a serial that is not one.
# The photo tests (PhotosOnDeviceTest) are the same again: the broker makes
# three test images with PIL on this computer, pushes them into the
# emulator's Pictures/RimeTest folder and deletes them after, so they are left
# out by name on any other serial and the `photos_*` verbs refuse one. No
# photo of anybody's is ever read.
#
# A SECOND throwaway computer (rime-agentd + rime-remoted + a broker, with its
# own state, runtime folder and machine key under $state/b) runs in a login
# session of its own (minted with `sudo -n systemd-run --property=PAMName=login`;
# without sudo it shares this one, and the run says so). The emulator pairs
# with both in TwoComputersOnDeviceTest: one phone, two computers, no new
# protocol. That class rings, texts and pushes photos to the emulator like the
# three above, so it is left out by name on any other serial too.
#
# A device is required and its absence is a FAILURE, not a skip: a suite that
# reports success when it could not look is the defect this program has
# shipped more than once.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
android=$(dirname "$here")
repo=$(dirname "$android")

serial=${RIME_ADB_SERIAL:-}
classes=""
while getopts "s:c:" opt; do
  case $opt in
    s) serial=$OPTARG ;;
    c) classes=$OPTARG ;;
    *) echo "usage: $0 [-s serial] [-c class]" >&2; exit 2 ;;
  esac
done

adb=${ADB:-${ANDROID_HOME:-/var/tmp/android-sdk}/platform-tools/adb}
[ -x "$adb" ] || { echo "FAIL: no adb at $adb"; exit 1; }

# A serial is not optional when more than one transport is attached. A phone on
# wireless debugging is bound twice — by IP and by mDNS — so a bare `adb shell`
# answers "more than one device" and every command in this script would fail
# for a reason that has nothing to do with the code.
if [ -z "$serial" ]; then
  mapfile -t attached < <("$adb" devices | awk '/\tdevice$/ {print $1}')
  if [ "${#attached[@]}" -ne 1 ]; then
    echo "FAIL: ${#attached[@]} devices are attached (${attached[*]:-none}); pass -s <serial>"
    exit 1
  fi
  serial=${attached[0]}
fi
A=("$adb" -s "$serial")

state=${RIME_DEVICE_SUITE_DIR:-$(mktemp -d /var/tmp/rime-device-suite-XXXXXX)}
mkdir -p "$state"
# `root` is declared here and set further down, so the EXIT trap below can
# read it even when the script fails before the daemon root exists.
root=""
agentd_pid=""; remoted_pid=""; broker_pid=""; nexd_pid=""; talkback_on=""
root2=""

# TalkBack, when this run turned it on, goes back off no matter how the script
# ends. `settings delete` and not `settings put … null`, which writes the
# four-character string "null" and leaves a phone whose screen reader is
# configured to a service that does not exist.
restore_talkback() {
  [ -n "$talkback_on" ] || return 0
  "${A[@]}" shell settings delete secure enabled_accessibility_services >/dev/null 2>&1 || true
  "${A[@]}" shell settings put secure accessibility_enabled 0 >/dev/null 2>&1 || true
  talkback_on=""
}

cleanup() {
  local rc=$?
  restore_talkback
  [ -n "$broker_pid" ] && kill "$broker_pid" 2>/dev/null || true
  # The CURRENT rime-remoted, which is not always the one this script started.
  # `restart_remoted` — the verb the reconnect test uses — stops the daemon and
  # starts another, and writes the new pid to `remoted.pid`. Killing the shell
  # variable instead left the replacement running: measured on 2026-09-19, the
  # previous round's worktree still held **seven** orphaned `rime-remoted`
  # processes and one `rime-agentd`, the oldest nearly nine hours old, each
  # holding a TCP listener. This script's own header says it stops its daemons
  # by pid; the pid it has to use is the one on disk.
  local current=""
  if [ -n "$root" ] && [ -r "$root/remoted.pid" ]; then
    current=$(cat "$root/remoted.pid" 2>/dev/null) || current=""
  fi
  for p in "$current" "$remoted_pid" "$agentd_pid" "$nexd_pid"; do
    [ -n "$p" ] && kill "$p" 2>/dev/null || true
  done
  # The second computer: its session's command ends on the stop file and
  # stops its own processes; their pids on disk are killed here as well, in
  # case it never got the chance.
  if [ -n "$root2" ] && [ -d "$root2" ]; then
    touch "$root2/stop" 2>/dev/null || true
    sleep 1
    for f in broker remoted agentd; do
      p=$(cat "$root2/$f.pid" 2>/dev/null) || p=""
      [ -n "$p" ] && kill "$p" 2>/dev/null || true
    done
  fi
  wait 2>/dev/null || true
  exit $rc
}
trap cleanup EXIT

bin=$repo/rimed/target/debug
for b in rime-agentd rime-remoted rime; do
  if [ ! -x "$bin/$b" ]; then
    echo "FAIL: $bin/$b is not built. Run: (cd $repo/rimed && cargo build --workspace)"
    exit 1
  fi
done

# A binary older than the sources it was built from would make this suite run a
# daemon that predates the code under test and report success for it. The Rust
# end-to-end suite refuses the same thing for the same reason.
#
# Each binary is compared only against the crates IT is built from, which is the
# correction the Rust version needed after its first attempt failed every run:
# cargo does not relink a binary whose own inputs are unchanged, so after a
# change to `rime-agentd` the `rime-remoted` binary is LEGITIMATELY older, and
# saying otherwise makes a true guard cry wolf on a perfectly fresh build.
stale_against() {
  local binary=$1; shift
  local found
  found=$(find "$@" -name '*.rs' -newer "$binary" -print -quit 2>/dev/null || true)
  if [ -n "$found" ]; then
    echo "FAIL: $found is newer than $binary. Run: (cd $repo/rimed && cargo build --workspace)"
    exit 1
  fi
}
stale_against "$bin/rime-agentd"  "$repo/rimed/rime-agentd/src"  "$repo/rimed/rime-agent-core/src"
stale_against "$bin/rime-remoted" "$repo/rimed/rime-remoted/src" "$repo/rimed/rime-remote-core/src"
stale_against "$bin/rime"         "$repo/rimed/rime/src"

root=$state/daemons
rm -rf "$root"; mkdir -p "$root"/run "$root"/state "$root"/scratch

# A free port, taken by binding one and letting it go, so two runs on one
# machine do not fight over a hardcoded number.
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')
brokerport=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')

# "Send to Rime" (clipboard_write, save_file) writes to this machine's
# clipboard and Downloads folder, and this rime-agentd runs as the developer,
# with their HOME and on their compositor: a real backend here would replace
# what they copied and drop files into their Downloads. With the stub set it
# writes desk/clipboard and desk/Downloads/ instead, which the broker reads back
# for the tests (`desk_clipboard`, `desk_downloads`). The broker never restarts
# rime-agentd, so this is the only place it starts.
export RIME_AGENTD_DESKTOP_STUB=$root/desk
mkdir -p "$root/desk"
if [ "${RIME_AGENTD_DESKTOP_STUB:-}" != "$root/desk" ]; then
  echo "FAIL: refusing to start rime-agentd without the desktop stub: it would write to this desktop"
  exit 1
fi
XDG_RUNTIME_DIR=$root/run XDG_STATE_HOME=$root/state \
  RIME_AGENT_SCRATCH_ROOT=$root/scratch \
  "$bin/rime-agentd" >"$root/agentd.log" 2>&1 &
agentd_pid=$!

# nex, when a nexd was named: started BEFORE rime-remoted so its first hello
# already offers `nex`. The socket is the path rime-remoted uses when it is not
# given `--nexd` ($XDG_RUNTIME_DIR/nex/nexd.sock), so a restarted rime-remoted
# finds it the same way the installed one does, with no argument to forget.
#
# The folder is a scratch git repository with one failing test. A phone may
# only start sessions in a folder that already has one, so the broker's
# `nex_session_at_computer` verb below makes them there, as the person at this
# computer.
nexd=${RIME_DEVICE_SUITE_NEXD:-}
nexsock=""; nexroot=""
if [ -n "$nexd" ]; then
  [ -x "$nexd" ] || { echo "FAIL: RIME_DEVICE_SUITE_NEXD=$nexd is not an executable"; exit 1; }
  nexhome=$root/nex
  nexroot=$nexhome/ws
  mkdir -p "$nexroot" "$root/run/nex"
  printf 'add() { echo $(( $1 - $2 )); }\n' > "$nexroot/lib.sh"
  printf '. ./lib.sh\n[ "$(add 2 3)" = 5 ]\n' > "$nexroot/test.sh"
  git -C "$nexroot" init -q -b main
  git -C "$nexroot" add -A
  git -C "$nexroot" -c user.name=suite -c user.email=suite@invalid commit -q -m init
  cat > "$nexhome/script.json" <<'JSON'
{"steps":[
 {"text":"Fixing the operator.","tool_calls":[{"name":"edit_file","args":{"path":"lib.sh","old_text":"$1 - $2","new_text":"$1 + $2"}}]},
 {"text":"Running the tests.","tool_calls":[{"name":"run_command","args":{"command":"bash test.sh"}}]},
 {"text":"Fixed add(); all tests pass."}]}
JSON
  printf '{"providers":[{"id":"scripted","kind":"scripted","script":"%s"}],"policy":{},"base_backoff_ms":20}\n' \
    "$nexhome/script.json" > "$nexhome/config.json"
  nexsock=$root/run/nex/nexd.sock
  "$nexd" --data-dir "$nexhome/data" --socket "$nexsock" --config "$nexhome/config.json" \
    >"$root/nexd.log" 2>&1 &
  nexd_pid=$!
  for _ in $(seq 1 80); do [ -S "$nexsock" ] && break; sleep 0.25; done
  [ -S "$nexsock" ] || { echo "FAIL: nexd did not come up"; tail -20 "$root/nexd.log"; exit 1; }
  echo "nex: $nexd ($(stat -c %y "$nexd" | cut -d. -f1)), folder $nexroot"
fi

# The relay the daemon holds a rendezvous at, so the RELAY LEG is exercised by
# this suite and not only the LAN one.
#
# The real deployment by default, because a relay double on loopback would not
# answer the question the relay leg exists to answer: whether a phone can reach
# this computer when it is NOT on this network. Cloudflare terminates the TLS,
# routes the custom domain and runs the Durable Object; a double proves none of
# that. `rime-remoted` treats an unusable relay as a warning rather than a
# refusal to start (`main.rs`: "a bad relay address must not cost the machine
# its LAN path"), so a run with no internet loses the relay test and keeps the
# other thirty-odd.
#
# Set `RIME_DEVICE_SUITE_RELAY=` to run without one: the other tests all pass
# and the three relay tests fail naming the argument they were not given, which
# is the right answer on a machine with no internet — a skip would be a suite
# reporting success for a leg it never touched.
relay=${RIME_DEVICE_SUITE_RELAY-wss://apex-relay.andrenijman.com}  # rime-rename: keep (the deployed relay)

# The machine verbs (status, lock, media, volume) act on whatever desktop the
# daemon can reach, and this rime-remoted runs as the developer, on the
# developer's session bus and logind session: a real backend here would lock
# their screen and pause their music. Every rime-remoted this script starts,
# including the broker's replacement after `restart_remoted`, answers them from
# the stub instead. The stub keeps its state in machine/state.json and logs
# every action to machine/actions.jsonl, which the broker hands to the tests.
export RIME_REMOTED_MACHINE_STUB=$root/machine
mkdir -p "$root/machine"
# The same for the phone's notifications (contract §2.7): they go to
# notify/notifications.jsonl, never to this desktop's notification server.
export RIME_REMOTED_NOTIFY_STUB=$root/notify
mkdir -p "$root/notify"
# And what the phone is playing (contract §2.8): media/media.jsonl, never a
# player on this desktop's session bus.
export RIME_REMOTED_MEDIA_STUB=$root/media
mkdir -p "$root/media"

start_remoted() {
  if [ "${RIME_REMOTED_MACHINE_STUB:-}" != "$root/machine" ]; then
    echo "FAIL: refusing to start rime-remoted without the machine stub: it would act on this desktop"
    exit 1
  fi
  if [ "${RIME_REMOTED_NOTIFY_STUB:-}" != "$root/notify" ]; then
    echo "FAIL: refusing to start rime-remoted without the notification stub: it would post on this desktop"
    exit 1
  fi
  if [ "${RIME_REMOTED_MEDIA_STUB:-}" != "$root/media" ]; then
    echo "FAIL: refusing to start rime-remoted without the media stub: it would put a player on this desktop"
    exit 1
  fi
  local extra=()
  [ -n "$relay" ] && extra=(--relay "$relay")
  XDG_RUNTIME_DIR=$root/run XDG_STATE_HOME=$root/state \
    "$bin/rime-remoted" --port "$port" --allow-foreground "${extra[@]}" \
    >>"$root/remoted.log" 2>&1 &
  remoted_pid=$!
  echo "$remoted_pid" > "$root/remoted.pid"
}
start_remoted

ready=0
for _ in $(seq 1 80); do
  if [ -S "$root/run/rime-agentd/control.sock" ] && [ -S "$root/run/rime-remoted/control.sock" ]; then
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then ready=1; break; fi
  fi
  sleep 0.25
done
[ "$ready" = 1 ] || { echo "FAIL: the daemons did not come up"; tail -20 "$root"/*.log; exit 1; }

# The broker. `rime-remoted`'s control socket is a unix socket on this machine
# and a phone can never touch one, so this forwards a line of JSON to it and
# forwards the answer back. It decides NOTHING: every offer, every device list
# and every revocation is the daemon's.
#
# `restart_remoted` is the one verb that is not a forward, and it is here
# because "the desktop's service went away" cannot be asked of the service
# itself. It restarts the daemon on the SAME port with the SAME state
# directory, which is what a `systemctl --user restart` does.
#
# `file_privilege_request` and `decide_locally` are the other two, and they are
# the HUMAN AT THIS MACHINE — the person a phone is not. §7 reserves deciding a
# root operation for a local origin, `rime-agentd` enforces that on the wire,
# and a phone therefore cannot produce the state an approvals screen exists to
# show. These two produce it from the computer, where it belongs.
#
# They are deliberately two named verbs with fixed shapes rather than a general
# "forward anything to rime-agentd", which would be a way for a test to borrow a
# local origin for any request at all — and this suite's whole value is that the
# phone's own origin is real.
#
# `send_file_at_computer` is `rime remote send <device> <file>` as the CLI does
# it: the request line with the open file passed beside it (SCM_RIGHTS). The
# file is one the broker writes into this run's own directory from the bytes
# the test gives it; nothing of the developer's is read. A text send needs no
# verb of its own: `{"cmd":"send",...,"kind":"text"}` is forwarded like ring.
#
# `machine_stub_seed` and `machine_stub_actions` set and read the stub machine
# (see RIME_REMOTED_MACHINE_STUB above): what the phone will find, and what it
# asked the computer to do. Neither reaches a daemon.
cat > "$root/broker.py" <<'PY'
import base64, json, os, socket, socketserver, subprocess, sys, time

SOCK, PORT, ROOT, BIN = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
DPORT, AGENTD = sys.argv[5], sys.argv[6]
RELAY = sys.argv[7] if len(sys.argv) > 7 else ""
NEXD = sys.argv[8] if len(sys.argv) > 8 else ""
NEXROOT = sys.argv[9] if len(sys.argv) > 9 else ""

def control(line):
    s = socket.socket(socket.AF_UNIX); s.settimeout(60); s.connect(SOCK)
    s.sendall((line + "\n").encode())
    f = s.makefile("rb"); out = f.readline().decode().strip(); s.close()
    return out

def send_file_at_computer(device, name, data_b64):
    """`rime remote send DEVICE FILE`, from THIS process: the person at the
    computer. The file lives in the run directory and is gone once sent: the
    service holds its own descriptor."""
    path = os.path.join(ROOT, "send-" + str(time.time_ns()))
    with open(path, "wb") as f:
        f.write(base64.b64decode(data_b64))
    fd = os.open(path, os.O_RDONLY)
    try:
        os.unlink(path)
        s = socket.socket(socket.AF_UNIX); s.settimeout(60); s.connect(SOCK)
        line = json.dumps({"cmd": "send", "device": device, "kind": "file", "name": name}) + "\n"
        socket.send_fds(s, [line.encode()], [fd])
        f = s.makefile("rb"); out = f.readline().decode().strip(); s.close()
        return out
    finally:
        os.close(fd)

def agentd(request):
    """One request to rime-agentd, from THIS process — a local origin."""
    s = socket.socket(socket.AF_UNIX); s.settimeout(60); s.connect(AGENTD)
    s.sendall((json.dumps(request) + "\n").encode())
    f = s.makefile("rb"); out = f.readline().decode().strip(); s.close()
    return out

def nexd_at_computer(method, params):
    """One write to nexd from THIS process: a local origin, the person at the
    computer. Only the two fixed shapes below call it."""
    if not NEXD:
        return {"error": {"message": "this run has no nexd"}}
    import uuid
    s = socket.socket(socket.AF_UNIX); s.settimeout(30); s.connect(NEXD)
    params = dict(params, request_id=str(uuid.uuid4()))
    s.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n").encode())
    f = s.makefile("rb")
    while True:
        r = json.loads(f.readline())
        if r.get("id") == 1:
            break
    s.close()
    return r

def nex_reply(r, field=None):
    if "error" in r:
        return json.dumps({"reply": "error", "message": r["error"].get("message", "")})
    out = {"reply": "ok"}
    if field:
        out[field] = r["result"][field]
    return json.dumps(out)

def nex_session_at_computer(title):
    """A nex session in the suite's folder: the person at the computer choosing
    a folder, which nexd does not let a phone do."""
    return nex_reply(nexd_at_computer("sessions.create", {
        "root": NEXROOT, "mode": "build", "title": title, "provider": "scripted"}), "session_id")

def nex_submit_at_computer(session, text):
    """A message typed at the computer, so whatever the phone sees next came to
    it by itself and not as the answer to something it sent."""
    return nex_reply(nexd_at_computer("turns.submit", {"session_id": session, "text": text}))

def restart():
    pidfile = os.path.join(ROOT, "remoted.pid")
    with open(pidfile) as f:
        old = int(f.read().strip())
    try:
        os.kill(old, 15)
    except ProcessLookupError:
        pass
    for _ in range(80):
        try:
            os.kill(old, 0); time.sleep(0.05)
        except ProcessLookupError:
            break
    if os.environ.get("RIME_REMOTED_MACHINE_STUB") != os.path.join(ROOT, "machine"):
        return json.dumps({"reply": "error",
                           "message": "refusing to start rime-remoted without the machine stub"})
    if os.environ.get("RIME_REMOTED_NOTIFY_STUB") != os.path.join(ROOT, "notify"):
        return json.dumps({"reply": "error",
                           "message": "refusing to start rime-remoted without the notification stub"})
    if os.environ.get("RIME_REMOTED_MEDIA_STUB") != os.path.join(ROOT, "media"):
        return json.dumps({"reply": "error",
                           "message": "refusing to start rime-remoted without the media stub"})
    env = dict(os.environ,
               XDG_RUNTIME_DIR=os.path.join(ROOT, "run"),
               XDG_STATE_HOME=os.path.join(ROOT, "state"))
    log = open(os.path.join(ROOT, "remoted.log"), "ab")
    # The relay goes to the REPLACEMENT daemon as well. `restart_remoted` is
    # the verb the reconnect test uses, and a replacement that held no
    # rendezvous would leave half this suite testing a machine the relay has
    # never heard of — a failure that looks like the phone's.
    argv = [os.path.join(BIN, "rime-remoted"), "--port", DPORT, "--allow-foreground"]
    if RELAY:
        argv += ["--relay", RELAY]
    p = subprocess.Popen(argv, env=env, stdout=log, stderr=log)
    with open(pidfile, "w") as f:
        f.write(str(p.pid))
    for _ in range(200):
        try:
            c = socket.create_connection(("127.0.0.1", int(DPORT)), 0.25); c.close()
            return json.dumps({"reply": "ok", "pid": p.pid})
        except OSError:
            time.sleep(0.05)
    return json.dumps({"reply": "error", "message": "rime-remoted did not come back"})

def stop_remoted():
    """The computer going away: its rime-remoted killed (SIGKILL, so nothing
    is said goodbye) and NOT started again. `restart_remoted` brings it
    back on the same port and state."""
    pidfile = os.path.join(ROOT, "remoted.pid")
    with open(pidfile) as f:
        old = int(f.read().strip())
    try:
        os.kill(old, 9)
    except ProcessLookupError:
        pass
    for _ in range(80):
        try:
            os.kill(old, 0); time.sleep(0.05)
        except ProcessLookupError:
            return json.dumps({"reply": "ok", "pid": old})
    return json.dumps({"reply": "error", "message": "rime-remoted %d did not die" % old})

STUB = os.path.join(ROOT, "machine")

def machine_stub_seed(state):
    """Replace the stub machine's state: what the phone will read next."""
    if not isinstance(state, dict):
        return json.dumps({"reply": "error", "message": "state must be an object"})
    tmp = os.path.join(STUB, "state.json.new")
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, os.path.join(STUB, "state.json"))
    return json.dumps({"reply": "ok"})

def machine_stub_actions():
    """Every action the stub machine was asked to take, oldest first."""
    try:
        with open(os.path.join(STUB, "actions.jsonl")) as f:
            actions = [json.loads(l) for l in f if l.strip()]
    except FileNotFoundError:
        actions = []
    return json.dumps({"reply": "ok", "actions": actions})

DESK = os.path.join(ROOT, "desk")

def desk_clipboard():
    """What the stub desktop's clipboard holds, or null when nothing was written."""
    try:
        with open(os.path.join(DESK, "clipboard"), encoding="utf-8") as f:
            text = f.read()
    except FileNotFoundError:
        text = None
    return json.dumps({"reply": "ok", "text": text})

def desk_downloads():
    """Every finished file in the stub Downloads folder, with its size and sha256."""
    import hashlib
    d = os.path.join(DESK, "Downloads")
    files = []
    try:
        names = sorted(os.listdir(d))
    except FileNotFoundError:
        names = []
    for n in names:
        p = os.path.join(d, n)
        if not os.path.isfile(p):
            continue
        h = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 16), b""):
                h.update(chunk)
        files.append({"name": n, "size": os.path.getsize(p), "sha256": h.hexdigest()})
    return json.dumps({"reply": "ok", "dir": d, "files": files})

def desk_reset():
    """Empty the stub clipboard and Downloads folder, so a test sees only its own writes."""
    import shutil
    shutil.rmtree(os.path.join(DESK, "Downloads"), ignore_errors=True)
    try:
        os.remove(os.path.join(DESK, "clipboard"))
    except FileNotFoundError:
        pass
    return json.dumps({"reply": "ok"})

def notify_log():
    """What rime-remoted posted to the stub notification server, complete lines only."""
    entries = []
    try:
        with open(os.path.join(ROOT, "notify", "notifications.jsonl"), encoding="utf-8") as f:
            for l in f:
                if not l.endswith("\n"):
                    break
                entries.append(json.loads(l))
    except FileNotFoundError:
        pass
    return json.dumps({"reply": "ok", "entries": entries})

def media_log():
    """What rime-remoted showed as the phone's media player (stub), complete lines only."""
    entries = []
    try:
        with open(os.path.join(ROOT, "media", "media.jsonl"), encoding="utf-8") as f:
            for l in f:
                if not l.endswith("\n"):
                    break
                entries.append(json.loads(l))
    except FileNotFoundError:
        pass
    return json.dumps({"reply": "ok", "entries": entries})

def focus_file():
    """The phone's Do Not Disturb as rime-remoted wrote it for the Shell
    (contract §2.9): $XDG_RUNTIME_DIR/rime-remote/focus.json, where the
    runtime directory is this run's own (every rime-remoted here is started
    with XDG_RUNTIME_DIR=$ROOT/run), so the desktop's real file is never read
    or written. Null until a phone's change was written."""
    try:
        with open(os.path.join(ROOT, "run", "rime-remote", "focus.json"), encoding="utf-8") as f:
            return json.dumps({"reply": "ok", "focus": json.load(f)})
    except FileNotFoundError:
        return json.dumps({"reply": "ok", "focus": None})

# Emulator numbers only: the emulator's own console ports and the number it
# gives itself. A message is never injected from, or about, a real number.
EMU_NUMBERS = {"5554", "5556", "15555215556", "+15555215556"}

def emu_sms(number, text):
    """An SMS arriving on the EMULATOR from [number], through its console
    (`adb emu sms send`). Refused unless this run's serial is an emulator,
    the number is an emulator number, and the text is short plain words.
    argv, never a shell string."""
    serial = os.environ.get("RIME_SUITE_SERIAL", "")
    adb = os.environ.get("RIME_SUITE_ADB", "")
    if not serial.startswith("emulator-") or not adb:
        return json.dumps({"reply": "error", "message": "emu_sms runs only on an emulator"})
    if number not in EMU_NUMBERS:
        return json.dumps({"reply": "error", "message": "emu_sms takes emulator numbers only"})
    if not isinstance(text, str) or not (0 < len(text) <= 120) or not all(c.isalnum() or c == " " for c in text):
        return json.dumps({"reply": "error", "message": "emu_sms takes short plain text only"})
    r = subprocess.run([adb, "-s", serial, "emu", "sms", "send", number, text], capture_output=True, text=True, timeout=30)
    if r.returncode != 0 or "OK" not in r.stdout:
        return json.dumps({"reply": "error", "message": (r.stdout + r.stderr).strip()[:300]})
    return json.dumps({"reply": "ok"})

EMU_GSM_ACTIONS = {"call", "accept", "cancel", "busy", "hold"}

def emu_gsm(action, number):
    """A call on the EMULATOR's modem, through its console (`adb emu gsm
    <action> <number>`): an incoming call from [number] (call), or that call
    answered, ended, refused busy or held by the far end. Refused unless this
    run's serial is an emulator and the number is an emulator number. argv,
    never a shell string."""
    serial = os.environ.get("RIME_SUITE_SERIAL", "")
    adb = os.environ.get("RIME_SUITE_ADB", "")
    if not serial.startswith("emulator-") or not adb:
        return json.dumps({"reply": "error", "message": "emu_gsm runs only on an emulator"})
    if action not in EMU_GSM_ACTIONS:
        return json.dumps({"reply": "error", "message": "emu_gsm takes call, accept, cancel, busy or hold"})
    if number not in EMU_NUMBERS:
        return json.dumps({"reply": "error", "message": "emu_gsm takes emulator numbers only"})
    r = subprocess.run([adb, "-s", serial, "emu", "gsm", action, number], capture_output=True, text=True, timeout=30)
    if r.returncode != 0 or "OK" not in r.stdout:
        return json.dumps({"reply": "error", "message": (r.stdout + r.stderr).strip()[:300]})
    return json.dumps({"reply": "ok"})

def notify_action(nid, key):
    """The stub notification server's ActionInvoked: the person pressed the
    button [key] on notification [nid]. A real server closes it after, which
    the test sends itself with notify_closed."""
    if not isinstance(nid, int) or not isinstance(key, str):
        return json.dumps({"reply": "error", "message": "id must be a number and action a string"})
    with open(os.path.join(ROOT, "notify", "closed.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": nid, "action": key}) + "\n")
    return json.dumps({"reply": "ok"})

def notify_closed(nid, reason):
    """The stub notification server's NotificationClosed: one complete line
    rime-remoted's stub reads, as if the person (2) or a timeout (1) closed it."""
    if not isinstance(nid, int) or not isinstance(reason, int):
        return json.dumps({"reply": "error", "message": "id and reason must be numbers"})
    with open(os.path.join(ROOT, "notify", "closed.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": nid, "reason": reason}) + "\n")
    return json.dumps({"reply": "ok"})

def notify_reply(nid, text):
    """The stub notification server's NotificationReplied: the person typed
    [text] into notification [nid]'s reply field. A real server closes the
    notification after it, which the test sends itself with notify_closed."""
    if not isinstance(nid, int) or not isinstance(text, str):
        return json.dumps({"reply": "error", "message": "id must be a number and reply a string"})
    with open(os.path.join(ROOT, "notify", "closed.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": nid, "reply": text}) + "\n")
    return json.dumps({"reply": "ok"})

def remote_devices_json():
    """`rime remote devices --json`, from THIS process: the person at the
    computer asking what is paired, through the same CLI they would type.
    It reaches this run's rime-remoted only: XDG_RUNTIME_DIR is the run's own."""
    env = dict(os.environ, XDG_RUNTIME_DIR=os.path.join(ROOT, "run"),
               XDG_STATE_HOME=os.path.join(ROOT, "state"))
    p = subprocess.run([os.path.join(BIN, "rime"), "remote", "devices", "--json"],
                       env=env, capture_output=True, timeout=60)
    if p.returncode != 0:
        return json.dumps({"reply": "error", "message": p.stderr.decode(errors="replace").strip()})
    return json.dumps({"reply": "ok", "devices": json.loads(p.stdout)})

def media_call(call):
    """A method call on a phone's stub media player (contract §2.8), as a
    reader on the computer's bus would make it: one complete line
    rime-remoted's stub reads from control.jsonl. Its outcome lands in
    media.jsonl as {"op":"call",...,"result":...}, read with media_log."""
    if not isinstance(call, dict) or not isinstance(call.get("player"), str) or not isinstance(call.get("method"), str):
        return json.dumps({"reply": "error", "message": "player and method must be strings"})
    with open(os.path.join(ROOT, "media", "control.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps(call) + "\n")
    return json.dumps({"reply": "ok"})

# The photo tests' own folder on the EMULATOR, and the names its images carry.
PHOTOS_DIR = "/sdcard/Pictures/RimeTest"
PHOTOS_URI = "content://media/external/images/media"
PHOTOS_PUSHED = {}

def emulator():
    """(adb, serial) when this run's serial is an emulator, else None."""
    serial = os.environ.get("RIME_SUITE_SERIAL", "")
    adb = os.environ.get("RIME_SUITE_ADB", "")
    if not serial.startswith("emulator-") or not adb:
        return None
    return adb, serial

def adb_shell(adb, serial, *argv, timeout=30):
    return subprocess.run([adb, "-s", serial, "shell", *argv], capture_output=True, text=True, timeout=timeout)

def photos_rows(adb, serial):
    """The emulator's MediaStore rows under Pictures/RimeTest, by name:
    {name: {"_size": ..., "width": ...}} with "NULL" kept as None."""
    r = adb_shell(adb, serial, "content", "query", "--uri", PHOTOS_URI,
                  "--projection", "_display_name:_size:width",
                  "--where", "\"relative_path LIKE 'Pictures/RimeTest%'\"")
    rows = {}
    for line in r.stdout.splitlines():
        if not line.startswith("Row:"):
            continue
        fields = {}
        for part in line.split(" ", 2)[2].split(", "):
            k, _, v = part.partition("=")
            fields[k.strip()] = None if v == "NULL" else v
        if fields.get("_display_name"):
            rows[fields["_display_name"]] = fields
    return rows

def gps_of(data):
    """Whether a JPEG still says where it was taken: a GPS latitude or
    longitude with a finite, non-zero value. Android may keep the GPS block
    and blank its values, which is no location; a blanked rational reads as
    0/0, which PIL gives as NaN."""
    import io, math
    from PIL import Image
    try:
        gps = Image.open(io.BytesIO(data)).getexif().get_ifd(0x8825)
    except Exception:
        return False
    for tag in (2, 4):
        for v in gps.get(tag, ()) or ():
            try:
                f = float(v)
            except Exception:
                continue
            if math.isfinite(f) and f != 0.0:
                return True
    return False

def photos_push():
    """Three test images made here with PIL, each different, the third with a
    GPS location in its EXIF, pushed into the EMULATOR's Pictures/RimeTest and
    indexed in its MediaStore. Old RimeTest images are deleted first. Returns
    each one's name, length, sha256 and whether it carries GPS."""
    emu = emulator()
    if emu is None:
        return json.dumps({"reply": "error", "message": "photos_push runs only on an emulator"})
    adb, serial = emu
    try:
        import io, hashlib
        from PIL import Image, ImageDraw
        from PIL.TiffImagePlugin import IFDRational
    except ImportError:
        return json.dumps({"reply": "error", "message": "PIL (python3-pillow) is not installed on this computer, "
                           "and the photos test needs it to make its test images"})
    photos_cleanup()
    outdir = os.path.join(ROOT, "photos-pushed")
    os.makedirs(outdir, exist_ok=True)
    tag = "%06d" % (int(time.time() * 1000) % 1_000_000)
    made = []
    for i, colour in enumerate([(200, 60, 40), (40, 160, 90), (50, 80, 200)]):
        img = Image.new("RGB", (1600, 1200), colour)
        draw = ImageDraw.Draw(img)
        for x in range(0, 1600, 37 + i * 11):
            draw.line([(x, 0), (1600 - x, 1200)], fill=((x * 3) % 256, 255 - colour[1], (x + 90 * i) % 256), width=5)
        gps = i == 2
        extra = {}
        if gps:
            # Only this one carries EXIF at all: a camera make and a location.
            exif = Image.Exif()
            exif[0x010F] = "RimeTest"
            exif.get_ifd(0x8825).update({
                1: "S", 2: (IFDRational(31, 1), IFDRational(57, 1), IFDRational(1234, 100)),
                3: "E", 4: (IFDRational(115, 1), IFDRational(51, 1), IFDRational(7, 1)),
            })
            extra["exif"] = exif
        buf = io.BytesIO()
        img.save(buf, "JPEG", quality=88, **extra)
        data = buf.getvalue()
        if gps and not gps_of(data):
            return json.dumps({"reply": "error", "message": "the GPS test image was made without a location"})
        name = "RimeTest_%s_%d.jpg" % (tag, i)
        local = os.path.join(outdir, name)
        with open(local, "wb") as f:
            f.write(data)
        r = subprocess.run([adb, "-s", serial, "push", local, PHOTOS_DIR + "/" + name], capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            return json.dumps({"reply": "error", "message": "adb push failed: " + (r.stdout + r.stderr).strip()[:300]})
        adb_shell(adb, serial, "am", "broadcast", "-a", "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
                  "-d", "file://" + PHOTOS_DIR + "/" + name)
        item = {"name": name, "len": len(data), "sha256": hashlib.sha256(data).hexdigest(), "gps": gps}
        PHOTOS_PUSHED[name] = item
        made.append(item)
    # A pushed file is in the store at once but without its size or
    # dimensions until the scan reaches it; the phone lists neither.
    end = time.time() + 30
    while time.time() < end:
        rows = photos_rows(adb, serial)
        if all(rows.get(m["name"], {}).get("_size") and rows.get(m["name"], {}).get("width") for m in made):
            return json.dumps({"reply": "ok", "photos": made})
        time.sleep(0.5)
    return json.dumps({"reply": "error", "message": "the test images were not indexed in 30 s: %s" % photos_rows(adb, serial)})

def photos_cleanup():
    """Delete the photo tests' folder on the EMULATOR, and wait for its
    MediaStore rows to go."""
    emu = emulator()
    if emu is None:
        return json.dumps({"reply": "error", "message": "photos_cleanup runs only on an emulator"})
    adb, serial = emu
    adb_shell(adb, serial, "rm", "-rf", PHOTOS_DIR)
    end = time.time() + 15
    while time.time() < end:
        rows = photos_rows(adb, serial)
        if not rows:
            return json.dumps({"reply": "ok"})
        # A row whose file is gone is removed by naming it to the scanner.
        for name in rows:
            adb_shell(adb, serial, "am", "broadcast", "-a", "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
                      "-d", "file://" + PHOTOS_DIR + "/" + name)
        time.sleep(0.5)
    return json.dumps({"reply": "error", "message": "RimeTest rows outlived their files: %s" % list(photos_rows(adb, serial))})

def rime_cli(argv):
    """`rime` from THIS process, at this run's rime-remoted (XDG_RUNTIME_DIR
    is the run's own): (exit code, stdout parsed as JSON or None, stderr).
    The JSON is kept on a failure too: `--json` prints {"error"} and exits 1."""
    env = dict(os.environ, XDG_RUNTIME_DIR=os.path.join(ROOT, "run"),
               XDG_STATE_HOME=os.path.join(ROOT, "state"))
    p = subprocess.run([os.path.join(BIN, "rime"), *argv], env=env, capture_output=True, timeout=110)
    try:
        out = json.loads(p.stdout) if p.stdout.strip() else None
    except Exception:
        out = None
    return p.returncode, out, p.stderr.decode(errors="replace").strip()[:600]

# What the person at a computer types for introductions (contract §2.15),
# and only that: the listing, ping, allow, remove and forget. Each argument
# is a plain id or flag word; argv, never a shell string.
REMOTE_CLI = {("computers", "list"), ("computers", "ping"), ("computers", "allow"), ("computers", "remove"),
              ("computers", "forget"), ("devices", "allow")}

def remote_cli(args):
    """`rime remote <args>` at this computer, as the person there would type
    it, for the introduction tests: exit code, JSON (with --json) and stderr."""
    if (not isinstance(args, list) or len(args) < 2 or not all(isinstance(a, str) for a in args)
            or (args[0], args[1]) not in REMOTE_CLI
            or not all(a in ("--json", "--via") or (0 < len(a) <= 64 and all(c.isalnum() or c in "-_" for c in a))
                       for a in args)):
        return json.dumps({"reply": "error", "message": "remote_cli takes computers list|ping|allow|remove|forget "
                           "and devices allow, with plain ids"})
    code, out, err = rime_cli(["remote", *args])
    return json.dumps({"reply": "ok", "code": code, "out": out, "stderr": err})

def remoted_log(start):
    """This run's rime-remoted output (its stderr) from byte `start` on, at
    most 256 KiB, and the size of the whole: for a test that checks why
    something did NOT happen, in what was written after a point it chose."""
    try:
        start = max(0, int(start or 0))
        with open(os.path.join(ROOT, "remoted.log"), "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(min(start, size))
            text = f.read(262144).decode(errors="replace")
    except FileNotFoundError:
        size, text = 0, ""
    except (TypeError, ValueError):
        return json.dumps({"reply": "error", "message": "remoted_log takes a byte offset"})
    return json.dumps({"reply": "ok", "size": size, "log": text})

def thumb_facts(path):
    """What the test asserts about one thumbnail on this computer: where it
    is, its mode, and whether any EXIF or APP1 segment survived in it."""
    facts = {"path": path, "exists": False}
    if not isinstance(path, str):
        return facts
    root = os.path.join(ROOT, "run", "rime-remote", "photos") + os.sep
    facts["under_run"] = os.path.realpath(path).startswith(os.path.realpath(root[:-1]) + os.sep)
    try:
        st = os.lstat(path)
        with open(path, "rb") as f:
            data = f.read()
    except OSError:
        return facts
    facts.update({"exists": True, "mode": "%o" % (st.st_mode & 0o7777), "len": len(data),
                  "jpeg": data[:2] == b"\xff\xd8", "exif": b"Exif" in data, "app1": False})
    # Walk the header segments up to the image data, as a reader would.
    i = 2
    while i + 4 <= len(data) and data[i] == 0xFF:
        marker = data[i + 1]
        if marker == 0xFF:
            i += 1
            continue
        if marker == 0xE1:
            facts["app1"] = True
        if marker == 0xDA:
            break
        i += 2 + int.from_bytes(data[i + 2:i + 4], "big")
    return facts

def photos_list(device, limit):
    """`rime remote photos --device D --json --limit N` at the computer, with
    the facts about every thumbnail it names."""
    if not isinstance(device, str) or not isinstance(limit, int):
        return json.dumps({"reply": "error", "message": "device must be a string and limit a number"})
    code, out, err = rime_cli(["remote", "photos", "--device", device, "--json", "--limit", str(limit)])
    thumbs = []
    if isinstance(out, dict):
        for p in out.get("photos") or []:
            if p.get("thumb") is not None:
                thumbs.append(dict(thumb_facts(p.get("thumb")), id=p.get("id")))
    return json.dumps({"reply": "ok", "code": code, "out": out, "stderr": err, "thumbs": thumbs})

def photos_save(device, photo):
    """`rime remote photos save ID --device D --to <run>/saved --json` at the
    computer, with the saved file's length, sha256 and whether it says where
    it was taken. Which test image it was, and what Android did to the one
    with a location, goes in broker.log."""
    if not isinstance(device, str) or not isinstance(photo, int):
        return json.dumps({"reply": "error", "message": "device must be a string and photo a number"})
    import hashlib
    code, out, err = rime_cli(["remote", "photos", "save", str(photo), "--device", device,
                               "--to", os.path.join(ROOT, "saved"), "--json"])
    saved = None
    if code == 0 and isinstance(out, dict) and isinstance(out.get("path"), str):
        try:
            with open(out["path"], "rb") as f:
                data = f.read()
            saved = {"len": len(data), "sha256": hashlib.sha256(data).hexdigest(), "gps": gps_of(data)}
            for item in PHOTOS_PUSHED.values():
                if item["len"] == len(data) or item["sha256"] == saved["sha256"]:
                    if item["gps"]:
                        sys.stderr.write("photos: the image with a location came back %s, location %s\n" % (
                            "byte for byte" if item["sha256"] == saved["sha256"] else "changed",
                            "present" if saved["gps"] else "absent"))
                        sys.stderr.flush()
        except OSError as e:
            saved = {"error": str(e)}
    return json.dumps({"reply": "ok", "code": code, "out": out, "stderr": err, "saved": saved})

def photos_thumbs_dir(device):
    """Whether this run's rime-remoted holds a thumbnail folder for [device]."""
    if not isinstance(device, str) or "/" in device or device in ("", ".", ".."):
        return json.dumps({"reply": "error", "message": "device must be a plain id"})
    d = os.path.join(ROOT, "run", "rime-remote", "photos", device)
    return json.dumps({"reply": "ok", "exists": os.path.lexists(d)})

class H(socketserver.StreamRequestHandler):
    timeout = 120
    def handle(self):
        for raw in self.rfile:
            line = raw.decode().strip()
            if not line:
                continue
            try:
                req = json.loads(line)
                cmd = req.get("cmd")
            except Exception as e:
                self.wfile.write((json.dumps({"reply": "error", "message": str(e)}) + "\n").encode())
                continue
            if cmd == "restart_remoted":
                out = restart()
            elif cmd == "stop_remoted":
                out = stop_remoted()
            elif cmd == "file_privilege_request":
                out = agentd({"cmd": "privilege_request",
                              "verb": req["verb"],
                              "args": req.get("args", []),
                              "reason": req["reason"]})
            elif cmd == "nex_session_at_computer":
                out = nex_session_at_computer(str(req["title"]))
            elif cmd == "nex_submit_at_computer":
                out = nex_submit_at_computer(str(req["session"]), str(req["text"]))
            elif cmd == "send_file_at_computer":
                out = send_file_at_computer(str(req["device"]), str(req["name"]), str(req["data"]))
            elif cmd == "machine_stub_seed":
                out = machine_stub_seed(req.get("state"))
            elif cmd == "machine_stub_actions":
                out = machine_stub_actions()
            elif cmd == "desk_clipboard":
                out = desk_clipboard()
            elif cmd == "desk_downloads":
                out = desk_downloads()
            elif cmd == "desk_reset":
                out = desk_reset()
            elif cmd == "notify_log":
                out = notify_log()
            elif cmd == "media_log":
                out = media_log()
            elif cmd == "remote_devices_json":
                out = remote_devices_json()
            elif cmd == "media_call":
                out = media_call(req.get("call"))
            elif cmd == "focus_file":
                out = focus_file()
            elif cmd == "emu_sms":
                out = emu_sms(str(req.get("number")), req.get("text"))
            elif cmd == "emu_gsm":
                out = emu_gsm(str(req.get("action")), str(req.get("number")))
            elif cmd == "notify_action":
                out = notify_action(req.get("id"), req.get("action"))
            elif cmd == "notify_closed":
                out = notify_closed(req.get("id"), req.get("reason"))
            elif cmd == "notify_reply":
                out = notify_reply(req.get("id"), req.get("text"))
            elif cmd == "photos_push":
                out = photos_push()
            elif cmd == "photos_cleanup":
                out = photos_cleanup()
            elif cmd == "photos_list":
                out = photos_list(req.get("device"), req.get("limit"))
            elif cmd == "photos_save":
                out = photos_save(req.get("device"), req.get("photo"))
            elif cmd == "remote_cli":
                out = remote_cli(req.get("args"))
            elif cmd == "remoted_log":
                out = remoted_log(req.get("from"))
            elif cmd == "photos_thumbs_dir":
                out = photos_thumbs_dir(req.get("device"))
            elif cmd == "decide_locally":
                out = agentd({"cmd": "decide", "id": req["id"], "decision": req["decision"]})
            else:
                out = control(line)
            self.wfile.write((out + "\n").encode())
            self.wfile.flush()

class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True

S(("0.0.0.0", PORT), H).serve_forever()
PY
RIME_SUITE_SERIAL="$serial" RIME_SUITE_ADB="$adb" python3 "$root/broker.py" "$root/run/rime-remoted/control.sock" "$brokerport" "$root" "$bin" "$port" \
  "$root/run/rime-agentd/control.sock" "$relay" "$nexsock" "$nexroot" >"$root/broker.log" 2>&1 &
broker_pid=$!

# Which of this machine's addresses the PHONE can actually reach. Measured from
# the phone rather than guessed from `ip addr`: a laptop can hold half a dozen
# addresses and the one a phone on the same Wi-Fi can dial is a fact about the
# network, not about the interface list.
lan=""
for ip in $(python3 - <<'PY'
import socket, subprocess
out = subprocess.run(["ip", "-o", "addr", "show"], capture_output=True, text=True).stdout
for line in out.splitlines():
    parts = line.split()
    if parts[2] in ("inet", "inet6"):
        a = parts[3].split("/")[0]
        if a.startswith(("127.", "::1", "fe80")):
            continue
        print(a)
PY
); do
  # Captured into a variable and matched with `[[ ]]`, NOT piped into
  # `grep -q`: under `pipefail` a `grep -q` that matches closes the pipe, the
  # producer takes SIGPIPE, and the pipeline's status is 141 — so a successful
  # probe would be read as a failure and this script would say the phone cannot
  # reach the machine when it plainly can.
  probe=$("${A[@]}" shell "echo | toybox nc -w 2 $ip $brokerport >/dev/null 2>&1 && echo up" 2>/dev/null || true)
  if [[ "$probe" == *up* ]]; then
    lan="$ip:$port"; break
  fi
done
[ -n "$lan" ] || { echo "FAIL: the phone could not reach this machine on any address"; exit 1; }
echo "desktop reachable from the phone at $lan (broker on $brokerport)"

# ── can this process pair at all? ───────────────────────────────────────────
#
# Asked HERE, before the two-minute APK build, because the answer depends on
# how this script was STARTED and not on anything in the tree.
#
# `rime-remoted` decides whether a human is at the machine by reading the
# connecting peer's cgroup (`origin::classify`), and pairing is one of the
# things §7 reserves for one — pairing hands a phone standing access to every
# agent here. A suite launched from a terminal is observed `local-terminal` and
# pairs; a suite launched by a systemd user unit — a timer, a dispatched agent,
# CI — is observed `scheduled-job` and every offer is refused.
#
# Measured on 2026-09-19 from `rime-roadmap-resume.service`: 24 tests ran and 8
# failed, all eight on `rime-remoted did not mint an offer`. Nothing was wrong
# with the app, the phone or the daemon. The same commit from a login session
# is OK (24 tests). That is two minutes of gradle and twenty seconds of
# instrumentation spent to produce eight failures about the launcher, so the
# question is asked once, up front, through the same broker the tests use.
#
# It is a FAILURE and not a skip, and it names the wrapper that fixes it.
preflight=$(python3 - "$brokerport" <<'PY'
import json, socket, sys
try:
    s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), 10)
    s.sendall(b'{"cmd":"pair"}\n')
    print(s.makefile("rb").readline().decode().strip())
    s.close()
except OSError as e:
    print(json.dumps({"reply": "error", "message": f"the broker did not answer: {e}"}))
PY
)
if [[ "$preflight" != *'"qr"'* ]]; then
  echo "FAIL: rime-remoted will not mint a pairing offer for this process, so the"
  echo "      end-to-end tests cannot run. It answered:"
  echo "      $preflight"
  if [[ "$preflight" == *scheduled-job* ]]; then
    echo
    echo "      This script is running under a systemd user unit — $(cat /proc/self/cgroup)"
    echo "      — which rime-remoted observes as \`scheduled-job\`, not as a human at the"
    echo "      keyboard. Run it inside a real login session instead:"
    echo
    echo "        tests/in-login-session.sh /bin/bash -c \\"
    echo "          'export JAVA_HOME=\$JAVA_HOME ANDROID_HOME=\$ANDROID_HOME; \\"
    echo "           exec android/tools/run-device-suite.sh $*'"
    echo
    echo "      The wrapper forwards PATH HOME LANG LC_ALL TMPDIR and RIME_*/CARGO_*/"
    echo "      RUST*/XDG_*_HOME only, so JAVA_HOME and ANDROID_HOME must be exported"
    echo "      inside it."
  fi
  exit 1
fi

# ── a second computer ───────────────────────────────────────────────────────
#
# TwoComputersOnDeviceTest pairs the phone with TWO computers at once (one
# phone, two rime-remoted, no new protocol). The second is a whole stack of
# its own from the same binaries: its own rime-agentd and rime-remoted, its
# own state, runtime and scratch folders (so its own machine key), its own
# stub desktop, machine, notifications and media, its own port, and its own
# broker in front of it (the same broker.py, given the second root).
#
# It runs in a login session of its OWN, minted through PAM as
# tests/in-login-session.sh does, because two computers are two logins and
# its broker must be observed as a human there for its offers to be minted.
# That wrapper cannot be used from here: inside a session it runs its
# command in place. The command in the session keeps running until the
# `stop` file appears, because the session's unit takes its processes with
# it when the command ends. Without sudo it runs in this session instead,
# and says so.
#
# The same rules as for the first: the stubs are checked before anything
# starts, every process is stopped by its pid, nothing is looked up by name,
# and nothing here touches the desktop's own runtime folder.
root2=$state/b
rm -rf "$root2"; mkdir -p "$root2"/run "$root2"/state "$root2"/scratch "$root2"/desk "$root2"/machine "$root2"/notify "$root2"/media
port2=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')
brokerport2=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')
{
  echo '#!/bin/bash'
  echo 'set -u'
  printf 'R=%q BIN=%q PORT=%q BPORT=%q RELAY=%q BROKER=%q\n' "$root2" "$bin" "$port2" "$brokerport2" "$relay" "$root/broker.py"
  printf 'export RIME_SUITE_SERIAL=%q RIME_SUITE_ADB=%q\n' "$serial" "$adb"
  cat <<'B'
export RIME_AGENTD_DESKTOP_STUB=$R/desk RIME_REMOTED_MACHINE_STUB=$R/machine \
  RIME_REMOTED_NOTIFY_STUB=$R/notify RIME_REMOTED_MEDIA_STUB=$R/media
{ cat /proc/self/cgroup; echo "session ${XDG_SESSION_ID:-none}"; } > "$R/session.txt"
XDG_RUNTIME_DIR=$R/run XDG_STATE_HOME=$R/state RIME_AGENT_SCRATCH_ROOT=$R/scratch \
  "$BIN/rime-agentd" >>"$R/agentd.log" 2>&1 &
echo $! > "$R/agentd.pid"
extra=(); [ -n "$RELAY" ] && extra=(--relay "$RELAY")
XDG_RUNTIME_DIR=$R/run XDG_STATE_HOME=$R/state \
  "$BIN/rime-remoted" --port "$PORT" --allow-foreground "${extra[@]}" >>"$R/remoted.log" 2>&1 &
echo $! > "$R/remoted.pid"
for _ in $(seq 1 80); do
  [ -S "$R/run/rime-agentd/control.sock" ] && [ -S "$R/run/rime-remoted/control.sock" ] && break
  sleep 0.25
done
python3 "$BROKER" "$R/run/rime-remoted/control.sock" "$BPORT" "$R" "$BIN" "$PORT" \
  "$R/run/rime-agentd/control.sock" "$RELAY" "" "" >"$R/broker.log" 2>&1 &
echo $! > "$R/broker.pid"
echo up > "$R/up"
while [ ! -e "$R/stop" ]; do sleep 0.5; done
for f in broker remoted agentd; do
  p=$(cat "$R/$f.pid" 2>/dev/null) && [ -n "$p" ] && kill "$p" 2>/dev/null
done
wait
B
} > "$root2/start.sh"
chmod +x "$root2/start.sh"
second_unit=""
if sudo -n true >/dev/null 2>&1 && [ -d /run/systemd/sessions ]; then
  second_unit=rime-suite-b-$$
  # The log is written by this script, as the developer, on purpose: only
  # systemd-run needs root, and its output belongs in the run's own folder.
  # shellcheck disable=SC2024
  sudo -n env TERM=dumb systemd-run --quiet --wait --pty --collect --unit="$second_unit" \
    --uid="$(id -u)" --gid="$(id -g)" --property=PAMName=login --working-directory="$root2" \
    --setenv=PATH="$PATH" --setenv=HOME="$HOME" --setenv=TERM=dumb \
    -- /bin/bash -c 'stty -onlcr 2>/dev/null; exec "$1" </dev/null' second "$root2/start.sh" \
    </dev/null >"$root2/session.log" 2>&1 &
else
  echo "NOTE: no sudo -n here, so the second computer runs in THIS login session, not one of its own"
  "$root2/start.sh" </dev/null >"$root2/session.log" 2>&1 &
fi
for _ in $(seq 1 120); do
  if [ -e "$root2/up" ] && (exec 3<>"/dev/tcp/127.0.0.1/$port2") 2>/dev/null \
     && (exec 3<>"/dev/tcp/127.0.0.1/$brokerport2") 2>/dev/null; then break; fi
  sleep 0.25
done
(exec 3<>"/dev/tcp/127.0.0.1/$brokerport2") 2>/dev/null || {
  echo "FAIL: the second computer did not come up"; tail -20 "$root2"/*.log; exit 1; }
echo "second computer: $(tr '\n' ' ' < "$root2/session.txt")"
preflight2=$(python3 - "$brokerport2" <<'PY'
import json, socket, sys
try:
    s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), 10)
    s.sendall(b'{"cmd":"pair"}\n')
    print(s.makefile("rb").readline().decode().strip())
    s.close()
except OSError as e:
    print(json.dumps({"reply": "error", "message": f"the broker did not answer: {e}"}))
PY
)
if [[ "$preflight2" != *'"qr"'* ]]; then
  echo "FAIL: the second computer's rime-remoted will not mint a pairing offer: $preflight2"
  exit 1
fi
lan2=$(echo "$lan" | sed "s/:[0-9]*\$/:$port2/")
broker2=$(echo "$lan" | sed "s/:[0-9]*\$/:$brokerport2/")
echo "second computer reachable from the phone at $lan2 (broker on $brokerport2)"

# Build and install. Both APKs, every run: an instrumentation APK from a
# previous build is the same stale-binary defect the Rust suite refuses.
export ANDROID_HOME=${ANDROID_HOME:-/var/tmp/android-sdk}
( cd "$android" && ./gradlew --console=plain :app:assembleDebug :app:assembleDebugAndroidTest :testhelper:assembleDebug )
"${A[@]}" install -r "$android/app/build/outputs/apk/debug/app-debug.apk" >/dev/null
"${A[@]}" install -r "$android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk" >/dev/null
# The stand-in messaging app the reply test answers (android/testhelper). It
# posts notifications, which Android 13 and later ask the person for; here
# the suite grants it. Older Android has no such permission to grant.
"${A[@]}" install -r "$android/testhelper/build/outputs/apk/debug/testhelper-debug.apk" >/dev/null
if [ "$("${A[@]}" shell getprop ro.build.version.sdk | tr -d '\r')" -ge 33 ]; then
  "${A[@]}" shell pm grant com.rimeos.remote.testhelper android.permission.POST_NOTIFICATIONS
fi

brokeraddr=$(echo "$lan" | sed "s/:[0-9]*\$/:$brokerport/")
# The name rime-remoted answers with is the kernel hostname (`machine_name`
# in rime-remoted's main.rs), so the tests are told it rather than assuming one.
args=(-e lan "$lan" -e broker "$brokeraddr" -e machine "$(cat /proc/sys/kernel/hostname)")
# Only when there is one. `am instrument -e relay ""` is not an empty argument:
# it makes `am` print its usage and exit, so the whole run produces no verdict —
# measured, and it cost a run to find. Absent instead, which `Desktop.arg` turns
# into a failure that names the missing argument, so the relay tests fail loudly
# rather than passing while asserting nothing.
[ -n "$relay" ] && args+=(-e relay "$relay")
args+=(-e lan2 "$lan2" -e broker2 "$broker2")
[ -n "$classes" ] && args+=(-e class "$classes")
skip=()
if [ -n "$nexroot" ]; then
  args+=(-e nexroot "$nexroot")
else
  skip+=(com.rimeos.remote.device.NexOnDeviceTest com.rimeos.remote.device.NexColdTapOnDeviceTest)
fi
# SMS, calls and photos only on an emulator (see the header): never a real
# phone's messages, number or photos.
case $serial in
  emulator-*) ;;
  *) skip+=(com.rimeos.remote.device.MessagesOnDeviceTest com.rimeos.remote.device.CallsOnDeviceTest com.rimeos.remote.device.PhotosOnDeviceTest com.rimeos.remote.device.TwoComputersOnDeviceTest) ;;
esac
if [ ${#skip[@]} -gt 0 ]; then
  args+=(-e notClass "$(IFS=,; echo "${skip[*]}")")
fi

# ── TalkBack ────────────────────────────────────────────────────────────────
#
# Turned ON for the run, because "the semantics tree carries a label" and "a
# screen reader is actually running while this code composes" are different
# claims and only the second one is what a user has. GrapheneOS ships AOSP
# TalkBack (`com.android.talkback`), which is worth saying: it is not a Google
# app on this phone and it is present anyway.
#
# Its absence is NOT a failure — a phone without it is a phone without it — but
# a silent skip would let "accessibility is tested" rest on nothing, so the
# state is printed either way and the verdict below says which ran.
talkback=com.android.talkback/com.google.android.marvin.talkback.TalkBackService
if "${A[@]}" shell pm list packages 2>/dev/null | grep -q 'com.android.talkback'; then
  "${A[@]}" shell settings put secure enabled_accessibility_services "$talkback" >/dev/null
  "${A[@]}" shell settings put secure accessibility_enabled 1 >/dev/null
  talkback_on=yes
  bound=""
  for _ in $(seq 1 40); do
    t=$("${A[@]}" shell dumpsys accessibility 2>/dev/null | grep -c 'TalkBackService' || true)
    if [ "${t:-0}" -gt 0 ]; then bound=yes; break; fi
    sleep 0.25
  done
  if [ -n "$bound" ]; then
    echo "TalkBack: enabled and bound for this run"
  else
    echo "FAIL: TalkBack was enabled and never bound, so this run would claim a "
    echo "      screen reader was watching when none was."
    exit 1
  fi
else
  echo "TalkBack: not installed on this device; the suite runs without one"
fi

out=$state/instrument.txt
set +e
"${A[@]}" shell am instrument -w -r "${args[@]}" \
  com.rimeos.remote.test/androidx.test.runner.AndroidJUnitRunner | tee "$out"
set -e
restore_talkback

# `am instrument` exits 0 whatever happens inside it, so the verdict is read out
# of the stream. `OK (n tests)` and nothing else is a pass; anything with a
# FAILURES!! line, and a run that produced no verdict at all, are failures.
if grep -q '^FAILURES!!' "$out" || grep -q 'INSTRUMENTATION_RESULT: shortMsg' "$out"; then
  echo "DEVICE SUITE: FAILED"; exit 1
fi
if ! grep -qE '^OK \([0-9]+ tests?\)' "$out"; then
  echo "DEVICE SUITE: produced no verdict — treating that as a failure"; exit 1
fi
count=$(grep -oE '^OK \([0-9]+ tests?\)' "$out" | grep -oE '[0-9]+')
echo "DEVICE SUITE: $count tests passed on $serial"
[ -n "$nexroot" ] || echo "DEVICE SUITE: nex NOT tested (no RIME_DEVICE_SUITE_NEXD)"
case $serial in
  emulator-*) ;;
  *) echo "DEVICE SUITE: SMS, calls, photos and two computers NOT tested (not an emulator)" ;;
esac
