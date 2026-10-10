#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  `rime secret migrate` against a fixture home (P0-003).
#
#  The migration reads a value, stores it somewhere else, and deletes the
#  original. That order is the whole safety argument, and it is what this
#  asserts:
#
#      store, verify, and only then remove — so an interrupted run leaves a
#      machine that still has its credentials;
#
#      and the value that arrives at the far end is the value that left, which
#      is checked by a loopback MCP server recording the header it was sent.
#
#  NOTHING HERE TOUCHES YOUR OWN CREDENTIALS. HOME, XDG_STATE_HOME and
#  XDG_CONFIG_HOME all point inside a fixture under /var/tmp, the secret service
#  runs on a private socket with a private store, and every credential in it is
#  an obvious fake. The one thing asserted about the real machine is that
#  nothing under the real $HOME was read or written, which the fixture's own
#  isolation gives for free.
#
#      ./tests/test-secret-migrate.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d -p /var/tmp rime-migrate-XXXXXX)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

SECRETD_PID=""; SERVER_PID=""
cleanup() {
    for p in "$SECRETD_PID" "$SERVER_PID"; do
        [ -n "$p" ] || continue
        kill "$p" 2>/dev/null
        for _ in 1 2 3 4 5; do kill -0 "$p" 2>/dev/null || break; sleep 0.2; done
        kill -9 "$p" 2>/dev/null
    done
    rm -rf "$WORK"
}
trap cleanup EXIT

for tool in cargo python3 curl; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime --bin rime-secretd >/dev/null 2>&1; then
    bad "rime and rime-secretd build"
    printf '\nmigrate: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "rime and rime-secretd build"
BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
Rime="${BIN}/rime"
SECRETD="${BIN}/rime-secretd"

# Two obvious fakes. Every assertion below is that one of these is somewhere or
# — much more often — that it is not.
FAKE_PAT="ghp-rime-migrate-fixture-pat-do-not-use"
FAKE_BEARER="rime-migrate-fixture-bearer-do-not-use"

# ── the fixture ──────────────────────────────────────────────────────────────
section "a fixture home with plaintext credentials in it"
export HOME="${WORK}/home"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
export XDG_RUNTIME_DIR="${WORK}/run"
mkdir -p "${HOME}/.claude" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"

# A loopback MCP server that demands the bearer token and records what it got.
# `http` on a loopback host is the one scheme the store allows besides https,
# precisely so this path can be exercised without a certificate authority.
cat > "${WORK}/server.py" <<'PY'
import http.server, json, sys, threading
seen = []
def pkt(payload):
    return ("%04x" % (len(payload) + 4)).encode() + payload

class H(http.server.BaseHTTPRequestHandler):
    # git's smart-HTTP ref advertisement, so a *git* credential can be verified
    # against this same fixture. Without an Authorization header it 401s with a
    # Basic challenge, which is what makes git ask its credential helper.
    def do_GET(self):
        auth = self.headers.get("Authorization")
        with open(sys.argv[2], "a") as f:
            f.write((auth or "<none>") + "\n")
        if not auth:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="rime-test"')
            self.send_header("Content-Length", "0")
            self.end_headers(); return
        body = (pkt(b"# service=git-upload-pack\n") + b"0000"
                + pkt(b"0" * 40 + b" capabilities^{}\x00agent=rime-test\n") + b"0000")
        self.send_response(200)
        self.send_header("Content-Type", "application/x-git-upload-pack-advertisement")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        auth = self.headers.get("Authorization")
        n = int(self.headers.get("Content-Length", 0))
        self.rfile.read(n)
        with open(sys.argv[2], "a") as f:
            f.write((auth or "<none>") + "\n")
        if not auth:
            self.send_response(401); self.send_header("Content-Length", "0")
            self.end_headers(); return
        body = json.dumps({"jsonrpc": "2.0", "id": 1,
                           "result": {"protocolVersion": "2025-06-18",
                                      "serverInfo": {"name": "fixture"}}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
srv = http.server.HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
PY
python3 "${WORK}/server.py" "${WORK}/port" "${WORK}/seen" &
SERVER_PID=$!
for _ in $(seq 1 60); do [ -s "${WORK}/port" ] && break; sleep 0.1; done
PORT="$(cat "${WORK}/port" 2>/dev/null)"
[ -n "$PORT" ] && ok "the fixture MCP server is listening on 127.0.0.1:${PORT}" \
               || { bad "the fixture MCP server is listening"; exit 1; }

cat > "${HOME}/.claude/settings.json" <<JSON
{
  "model": "opus",
  "theme": "dark",
  "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "${FAKE_PAT}",
          "ACME_API_KEY": "an-unknown-provider",
          "CLAUDE_CODE_ENABLE_TELEMETRY": "1"}
}
JSON
cat > "${HOME}/.claude.json" <<JSON
{
  "machineID": "fixture",
  "mcpServers": {
    "fixture-memory": {"type": "http", "url": "http://127.0.0.1:${PORT}/mcp",
                       "headers": {"Authorization": "Bearer ${FAKE_BEARER}"}},
    "local-tool": {"command": "npx", "args": ["-y", "x"],
                   "env": {"SOME_API_TOKEN": "handed-to-a-local-process"}}
  }
}
JSON
chmod 0600 "${HOME}/.claude.json"
ok "the fixture home has a PAT in settings.json and a bearer token in .claude.json"

# ── the secret service, on a private socket and store ────────────────────────
section "a private secret service"
export RIME_SECRETD_SOCKET="${WORK}/secretd.sock"
export RIME_SECRETD_STORE="${WORK}/store"
"$SECRETD" --socket "$RIME_SECRETD_SOCKET" --store "$RIME_SECRETD_STORE" \
    > "${WORK}/secretd.log" 2>&1 &
SECRETD_PID=$!
for _ in $(seq 1 100); do [ -S "$RIME_SECRETD_SOCKET" ] && break; sleep 0.1; done
[ -S "$RIME_SECRETD_SOCKET" ] && ok "the secret service came up on a private socket" \
    || { bad "the secret service came up"; cat "${WORK}/secretd.log"; exit 1; }

# A project, because a capability is granted per project and verification is a
# use. Migration runs from inside it.
PROJ="${WORK}/proj"
mkdir -p "$PROJ"
git -C "$PROJ" init -q
git -C "$PROJ" config user.email t@t
git -C "$PROJ" config user.name t
git -C "$PROJ" commit -q --allow-empty -m init
git -C "$PROJ" remote add origin "http://127.0.0.1:${PORT}/demo.git"

# ── the old broker's leftovers ───────────────────────────────────────────────
# P0-002 named this directory and deliberately left it alone: a file that may
# hold the only copy of a token is not something a `list` command deletes on its
# own. It said a real migration belongs with P0-003. This is that, and the
# fixture is the shape the old broker actually wrote — metadata and the token in
# one 0600 JSON file, with the grants in a second one beside it.
FAKE_LEGACY="rime-migrate-fixture-legacy-do-not-use"
OLD="${XDG_STATE_HOME}/rime/agent/secrets"
mkdir -p "$OLD"
cat > "${OLD}/legacy-git.json" <<JSON
{"service": "legacy-git", "host": "127.0.0.1", "scheme": "http",
 "username": "x-access-token", "backend": "file", "added": 1,
 "token": "${FAKE_LEGACY}"}
JSON
chmod 0600 "${OLD}/legacy-git.json"
# A keyring-backed record: the metadata is here and the value is not, so there
# is nothing to carry. It must be NAMED rather than silently dropped.
cat > "${OLD}/legacy-keyring.json" <<'JSON'
{"service": "legacy-keyring", "host": "gitlab.com",
 "username": "x-access-token", "backend": "keyring", "added": 1}
JSON
chmod 0600 "${OLD}/legacy-keyring.json"
cat > "${XDG_STATE_HOME}/rime/agent/secret-grants.json" <<JSON
{"projects": {"${PROJ}": ["legacy-git:git-ls-remote", "legacy-git:git-fetch"]}}
JSON
ok "the old broker's store has one file credential, one keyring record and two grants"

# ── dry run ──────────────────────────────────────────────────────────────────
section "a dry run says what it would do and writes nothing"
out="$(cd "$PROJ" && "$Rime" secret migrate --dry-run 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'
printf '%s' "$out" | pipe_has "would  store github" \
    && ok "the dry run found the PAT in settings.json" \
    || bad "the dry run found the PAT in settings.json"
printf '%s' "$out" | pipe_has "would  store fixture-memory" \
    && ok "the dry run found the MCP bearer token" \
    || bad "the dry run found the MCP bearer token"
printf '%s' "$out" | pipe_has "hands a credential to a program it spawns" \
    && ok "the dry run named the stdio server it cannot help with" \
    || bad "the dry run named the stdio server it cannot help with"
printf '%s' "$out" | pipe_has "would  store legacy-git" \
    && ok "the dry run found the old broker's file credential" \
    || bad "the dry run found the old broker's file credential"
printf '%s' "$out" | pipe_has "legacy-keyring.json holds no value" \
    && ok "the dry run named the keyring record it cannot read" \
    || bad "the dry run named the keyring record it cannot read"
printf '%s' "$out" | pipe_has "ACME_API_KEY .*nothing here knows which host" \
    && ok "a credential whose host nobody can work out is named, not guessed at" \
    || bad "a credential whose host nobody can work out is named, not guessed at"
grep -q "$FAKE_PAT" "${HOME}/.claude/settings.json" \
    && ok "the dry run wrote nothing to settings.json" \
    || bad "the dry run changed settings.json"
"$Rime" secret list --json 2>/dev/null | pipe_has "fixture-memory" \
    && bad "the dry run stored something" \
    || ok "the dry run stored nothing"

# ── the real run ─────────────────────────────────────────────────────────────
section "the migration"
out="$(cd "$PROJ" && "$Rime" secret migrate 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'

# Both credentials are stored, whatever happened to the originals: store comes
# first, and it is the step that must never be skipped.
list="$("$Rime" secret list --json 2>/dev/null)"
printf '%s' "$list" | pipe_has '"fixture-memory"' \
    && ok "the MCP credential is in the store" || bad "the MCP credential is in the store"
printf '%s' "$list" | pipe_has '"github"' \
    && ok "the GitHub credential is in the store" || bad "the GitHub credential is in the store"
printf '%s' "$list" | pipe_has "$FAKE_BEARER\|$FAKE_PAT" \
    && bad "rime secret list printed a credential" \
    || ok "rime secret list printed neither credential"

# The MCP one had no grant on this first run, so it was stored and KEPT. That
# is the discipline working, not a failure.
grep -q "$FAKE_BEARER" "${HOME}/.claude.json" \
    && ok "an unverifiable credential was left in place, as it must be" \
    || bad "an unverifiable credential was removed anyway"

# ── the old broker's store, closed out ───────────────────────────────────────
# This one IS verifiable on the first run: its grants came across from the old
# store, and the fixture project's origin is the fixture server. So it is the
# one credential here that goes all the way through store, verify and remove in
# a single pass — which is what an upgraded machine's leftovers should do.
printf '%s' "$out" | pipe_has "grant(s) from the old broker" \
    && ok "the old broker's grants were carried across" \
    || bad "the old broker's grants were carried across"
# `git.ls-remote`, not the `git-ls-remote` the fixture above wrote. The old
# broker's grants are carried across through the daemon's own grant verb, and
# P1-001's registry canonicalises an old spelling as it goes in — so this
# asserts the stronger thing the migration actually does: it arrives, and it
# arrives spelled the one way the trail and the grant table use from now on.
"$Rime" secret grants 2>/dev/null | pipe_has "legacy-git:git.ls-remote" \
    && ok "and the secret service now holds them, under the canonical name" \
    || { bad "and the secret service now holds them, under the canonical name"; "$Rime" secret grants; }
grep -q "Basic" "${WORK}/seen" \
    && ok "the legacy credential was verified against the fixture server" \
    || { bad "the legacy credential was verified against the fixture server"; cat "${WORK}/seen" 2>/dev/null; }
[ ! -e "${OLD}/legacy-git.json" ] \
    && ok "the old broker's plaintext file is gone" \
    || bad "the old broker's plaintext file is gone"
[ -e "${OLD}/legacy-keyring.json" ] \
    && ok "the keyring record, which holds no value, was not deleted" \
    || bad "the keyring record, which holds no value, was not deleted"
if grep -rq "$FAKE_LEGACY" "$XDG_STATE_HOME" 2>/dev/null; then
    printf '      still in: %s\n' "$(grep -rl "$FAKE_LEGACY" "$XDG_STATE_HOME" 2>/dev/null | tr '\n' ' ')"
    bad "no copy of the legacy credential is left in the home"
else
    ok "no copy of the legacy credential is left in the home"
fi

# ── grant, then migrate again ────────────────────────────────────────────────
section "with a grant, the second run verifies and removes"
(cd "$PROJ" && "$Rime" secret grant fixture-memory mcp-request >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" secret migrate 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'

grep -q "Bearer ${FAKE_BEARER}" "${WORK}/seen" \
    && ok "the credential reached the MCP server, sent by the daemon" \
    || { bad "the credential never reached the MCP server"; cat "${WORK}/seen" 2>/dev/null; }
grep -q "$FAKE_BEARER" "${HOME}/.claude.json" \
    && bad "the bearer token is still in ~/.claude.json" \
    || ok "the bearer token is gone from ~/.claude.json"
python3 - "${HOME}/.claude.json" <<'PY' > "${WORK}/mcp.out" 2>&1
import json, sys
d = json.load(open(sys.argv[1]))
s = d["mcpServers"]["fixture-memory"]
assert s["type"] == "stdio", s
assert s["command"] == "rime", s
assert s["args"] == ["mcp", "bridge", "fixture-memory"], s
assert d["machineID"] == "fixture", "the rest of the document was damaged"
assert d["mcpServers"]["local-tool"]["command"] == "npx", "an untouched server was changed"
print("ok")
PY
grep -qx ok "${WORK}/mcp.out" \
    && ok "the server definition now names the bridge, and nothing else moved" \
    || { bad "the server definition now names the bridge"; cat "${WORK}/mcp.out"; }
[ "$(stat -c %a "${HOME}/.claude.json")" = "600" ] \
    && ok "the rewritten file kept its 0600 mode" \
    || bad "the rewritten file kept its 0600 mode"

# The GitHub one still cannot be verified — this fixture repository has no
# github.com remote and no network — so it is still stored and still in place.
grep -q "$FAKE_PAT" "${HOME}/.claude/settings.json" \
    && ok "the PAT, which nothing here can verify, is still in settings.json" \
    || bad "the PAT was removed without being verified"

# ── idempotence ──────────────────────────────────────────────────────────────
section "running it again changes nothing"
out="$(cd "$PROJ" && "$Rime" secret migrate 2>&1)"
printf '%s' "$out" | pipe_has "fixture-memory" \
    && bad "a migrated credential was found again" \
    || ok "a migrated credential is not found a second time"
python3 - "${HOME}/.claude.json" <<'PY' > "${WORK}/mcp2.out" 2>&1
import json, sys
d = json.load(open(sys.argv[1]))
assert d["mcpServers"]["fixture-memory"]["args"] == ["mcp", "bridge", "fixture-memory"]
print("ok")
PY
grep -qx ok "${WORK}/mcp2.out" \
    && ok "the bridged definition survived a second run" \
    || { bad "the bridged definition survived a second run"; cat "${WORK}/mcp2.out"; }

# ── what settings.json kept ──────────────────────────────────────────────────
section "nothing else was touched"
python3 - "${HOME}/.claude/settings.json" <<'PY' > "${WORK}/set.out" 2>&1
import json, sys
d = json.load(open(sys.argv[1]))
assert d["model"] == "opus", d
assert d["theme"] == "dark", d
assert d["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"] == "1", d
assert d["env"]["ACME_API_KEY"] == "an-unknown-provider", "a guess was made after all"
print("ok")
PY
grep -qx ok "${WORK}/set.out" \
    && ok "the model, the theme, the plain variable and the unguessable one are intact" \
    || { bad "settings.json lost something"; cat "${WORK}/set.out"; }

# ── nothing leaked ───────────────────────────────────────────────────────────
section "no credential is anywhere it should not be"
if grep -rqE "$FAKE_BEARER|$FAKE_LEGACY" "${WORK}/secretd.log" 2>/dev/null; then
    bad "the daemon logged the credential"
else
    ok "the daemon logged no credential"
fi
if grep -qE "$FAKE_BEARER|$FAKE_LEGACY" "${RIME_SECRETD_STORE}/audit.jsonl" 2>/dev/null; then
    bad "the audit trail carries the credential"
else
    ok "the audit trail carries no credential"
fi
# `mcp.request` for the same reason: `mcp-request` is an alias the daemon
# accepts and never a spelling it writes.
grep -q "mcp.request" "${RIME_SECRETD_STORE}/audit.jsonl" 2>/dev/null \
    && ok "the audit trail records the brokered request" \
    || bad "the audit trail records the brokered request"

printf '\nmigrate: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
