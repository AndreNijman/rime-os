#!/usr/bin/env python3
"""Record what nexd really answers a phone, for core/src/test/resources/nex/.

    python3 -I tools/record-nex-fixtures.py <path to nexd> core/src/test/resources/nex

Starts a nexd with a scripted provider in a fresh temporary directory, makes
one session from "this machine" (a device may only work in a folder that
already has a session), then does everything else as a declared device,
`system.declare_origin {kind: device}`, which is exactly what rime-remoted's
bridge does before forwarding a phone's request. So the refusals in the
fixtures are the ones a phone gets.

Nothing personal is recorded: the session lives under /tmp and the scripted
provider needs no account.
"""
import json, os, socket, subprocess, sys, tempfile, time, uuid, pathlib
NEXD = sys.argv[1]; OUT = pathlib.Path(sys.argv[2]); OUT.mkdir(exist_ok=True)
d = pathlib.Path(tempfile.mkdtemp(prefix="nexcap-", dir="/tmp"))
ws = d / "ws"; ws.mkdir()
(ws / "lib.sh").write_text('add() { echo $(( $1 - $2 )); }\n')
(ws / "test.sh").write_text('. ./lib.sh\n[ "$(add 2 3)" = 5 ]\n')
for a in (["init","-q","-b","main"],["add","-A"],["-c","user.name=t","-c","user.email=t@e","commit","-q","-m","init"]):
    subprocess.run(["git",*a],cwd=ws,check=True)
(d/"script.json").write_text(json.dumps({"steps":[
 {"text":"Fixing the operator.","tool_calls":[{"name":"edit_file","args":{"path":"lib.sh","old_text":"$1 - $2","new_text":"$1 + $2"}}]},
 {"text":"Running the tests.","tool_calls":[{"name":"run_command","args":{"command":"bash test.sh"}}]},
 {"text":"Fixed add(); all tests pass."}]}))
(d/"empty.json").write_text(json.dumps({"steps":[]}))
(d/"config.json").write_text(json.dumps({"providers":[{"id":"scripted","kind":"scripted","script":str(d/"script.json")},{"id":"reviewer","kind":"scripted","script":str(d/"empty.json")}],"policy":{},"base_backoff_ms":20}))
sock = d/"s.sock"
p = subprocess.Popen([NEXD,"--data-dir",str(d/"data"),"--socket",str(sock),"--config",str(d/"config.json")],stderr=open(d/"err","w"))
for _ in range(100):
    if sock.exists(): break
    time.sleep(0.05)
def conn():
    s = socket.socket(socket.AF_UNIX); s.connect(str(sock)); return s, s.makefile("rw")
ls, lf = conn()
ds, df = conn()
n = [0]
f = df
def call(m, params=None, write=False, save=None, local=False):
    f = lf if local else df
    params = dict(params or {})
    if write: params["request_id"] = str(uuid.uuid4())
    n[0]+=1; f.write(json.dumps({"jsonrpc":"2.0","id":n[0],"method":m,"params":params})+"\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if r.get("id") == n[0]: break
    if save: (OUT/f"{save}.json").write_text(json.dumps(r, indent=1))
    return r
def wait(pred, t=30):
    end=time.time()+t
    while time.time()<end:
        r = call("sessions.stream",{"session_id":sid,"after":0})["result"]
        if any(pred(e) for e in r["events"]): return r
        time.sleep(0.2)
    raise SystemExit("timeout")
n[0]+=1; df.write(json.dumps({"jsonrpc":"2.0","id":"declare","method":"system.declare_origin","params":{"kind":"device","id":"phone-fixture"}})+"\n"); df.flush()
print("declare", df.readline().strip())
call("system.hello", save="system.hello")
call("models.list", save="models.list")
sid = call("sessions.create",{"root":str(ws),"mode":"build","title":"Fix add()","provider":"scripted"},write=True,local=True)["result"]["session_id"]
call("sessions.create",{"root":str(ws),"mode":"build","title":"Second look","provider":"scripted"},write=True,save="sessions.create")
call("sessions.create",{"root":"/tmp","mode":"build","provider":"scripted"},write=True,save="error.sessions.create.unknown_root")
call("turns.submit",{"session_id":sid,"text":"add() subtracts; fix it and run the tests"},write=True,save="turns.submit")
wait(lambda e: e["kind"]=="tool.approval_requested")
pend = call("tools.pending",{"session_id":sid},save="tools.pending")["result"]
call("tools.pending",{},save="tools.pending.all")
call("sessions.list",{},save="sessions.list.waiting")
call("sessions.get",{"session_id":sid},save="sessions.get.waiting")
a = pend[-1]
call("tools.approve",{"approval_id":a["approval_id"],"fingerprint":a["fingerprint"]},write=True,save="tools.approve")
wait(lambda e: e["kind"]=="turn.completed")
call("sessions.stream",{"session_id":sid,"after":0},save="sessions.stream")
call("sessions.stream",{"session_id":"*","after":0},save="sessions.stream.global")
call("sessions.list",{},save="sessions.list")
call("sessions.get",{"session_id":sid},save="sessions.get")
call("sessions.diff",{"session_id":sid},save="sessions.diff")
mid = call("missions.plan",{"root":str(ws),"spec":{"objective":"Add a subtract() helper","tasks":[{"id":"impl","role":"implementer","prompt":"write sub","agent":{"provider":"scripted"},"touches":["lib.sh"]}],"review":{"agent":{"provider":"reviewer"}},"verify":["true"]}},write=True,save="missions.plan")["result"]["mission_id"]
call("missions.list",{},save="missions.list")
call("missions.get",{"mission_id":mid},save="missions.get")
call("projects.list",{},save="projects.list")
call("connections.list",{},save="connections.list")
call("usage.list",{},save="usage.list")
call("budgets.list",{},save="budgets.list")
call("turns.cancel",{"session_id":sid,"turn_id":"nope"},write=True,save="error.turns.cancel")
call("tools.approve",{"approval_id":"nope"},save="error.missing_request_id")
call("system.doctor",{},save="error.local_only")
# one live notification on a following feed
fs, ff = conn()
ff.write(json.dumps({"jsonrpc":"2.0","id":"declare","method":"system.declare_origin","params":{"kind":"device","id":"phone-fixture"}})+"\n"); ff.flush(); ff.readline()
ff.write(json.dumps({"jsonrpc":"2.0","id":1,"method":"sessions.stream","params":{"session_id":"*","after":0,"follow":True,"tail":True}})+"\n"); ff.flush()
(OUT/"feed.first.json").write_text(ff.readline())
call("sessions.set_mode",{"session_id":sid,"mode":"plan"},write=True,save="sessions.set_mode")
(OUT/"feed.event.json").write_text(ff.readline())
p.terminate(); p.wait()
print("ok", d)
