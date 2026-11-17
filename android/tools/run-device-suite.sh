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
  wait 2>/dev/null || true
  exit $rc
}
trap cleanup EXIT

bin=$repo/rimed/target/debug
for b in rime-agentd rime-remoted; do
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

root=$state/daemons
rm -rf "$root"; mkdir -p "$root"/run "$root"/state "$root"/scratch

# A free port, taken by binding one and letting it go, so two runs on one
# machine do not fight over a hardcoded number.
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')
brokerport=$(python3 -c 'import socket;s=socket.socket();s.bind(("0.0.0.0",0));print(s.getsockname()[1]);s.close()')

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

start_remoted() {
  if [ "${RIME_REMOTED_MACHINE_STUB:-}" != "$root/machine" ]; then
    echo "FAIL: refusing to start rime-remoted without the machine stub: it would act on this desktop"
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
# `machine_stub_seed` and `machine_stub_actions` set and read the stub machine
# (see RIME_REMOTED_MACHINE_STUB above): what the phone will find, and what it
# asked the computer to do. Neither reaches a daemon.
cat > "$root/broker.py" <<'PY'
import json, os, socket, socketserver, subprocess, sys, time

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
            elif cmd == "file_privilege_request":
                out = agentd({"cmd": "privilege_request",
                              "verb": req["verb"],
                              "args": req.get("args", []),
                              "reason": req["reason"]})
            elif cmd == "nex_session_at_computer":
                out = nex_session_at_computer(str(req["title"]))
            elif cmd == "nex_submit_at_computer":
                out = nex_submit_at_computer(str(req["session"]), str(req["text"]))
            elif cmd == "machine_stub_seed":
                out = machine_stub_seed(req.get("state"))
            elif cmd == "machine_stub_actions":
                out = machine_stub_actions()
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
python3 "$root/broker.py" "$root/run/rime-remoted/control.sock" "$brokerport" "$root" "$bin" "$port" \
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

# Build and install. Both APKs, every run: an instrumentation APK from a
# previous build is the same stale-binary defect the Rust suite refuses.
export ANDROID_HOME=${ANDROID_HOME:-/var/tmp/android-sdk}
( cd "$android" && ./gradlew --console=plain :app:assembleDebug :app:assembleDebugAndroidTest )
"${A[@]}" install -r "$android/app/build/outputs/apk/debug/app-debug.apk" >/dev/null
"${A[@]}" install -r "$android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk" >/dev/null

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
[ -n "$classes" ] && args+=(-e class "$classes")
if [ -n "$nexroot" ]; then
  args+=(-e nexroot "$nexroot")
else
  args+=(-e notClass com.rimeos.remote.device.NexOnDeviceTest,com.rimeos.remote.device.NexColdTapOnDeviceTest)
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
