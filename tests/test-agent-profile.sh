#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for the agent profile system (roadmap §5, P0-009) and
#  its read-only mounts (P0-010).
#
#  The unit tests cover the classification table, the redaction and the bundle
#  format. What they cannot cover is the two claims the design rests on:
#
#      a portable bundle carries no credential and no conversation, from a
#      profile on a real filesystem; and
#
#      a confined session cannot rewrite the instructions it was started with,
#      enforced by the kernel rather than by an argument list.
#
#  Both need a real profile tree, a real daemon and a real mount namespace, so
#  both are asserted here against all three.
#
#  NOTHING HERE TOUCHES YOUR OWN PROFILE. The suite builds a fixture home under
#  /var/tmp and runs with HOME, XDG_RUNTIME_DIR, XDG_STATE_HOME and
#  XDG_CONFIG_HOME all pointing inside it. /var/tmp and not /tmp because the
#  sandbox masks /tmp with a tmpfs, and a fixture the session cannot see would
#  be a suite asserting nothing.
#
#  The agent binary is a stub on PATH. `claude` is resolved through PATH at
#  spawn time, which is what lets a user's own build win — and what lets this
#  suite exercise the real mount code without an account, a network or an API
#  call.
#
#      ./tests/test-agent-profile.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# Deliberate, and the same reason test-privilege-requests.sh gives: this suite
# counts failures rather than aborting, and several assertions run commands
# that exit non-zero on purpose. Under `bash -e {0}`, which is how GitHub
# Actions invokes a script, `x="$(cmd)"` with a non-zero cmd kills the run.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Before the temp tree, the daemon or the build: this suite needs an
# environment the daemon will observe as LOCAL, and that has to be arranged
# from outside the suite. `rime-agentd` places a peer from its cgroup, and a
# process started by systemd — a CI job, a timer-dispatched agent — is in
# neither a login session nor a user service, so §7 refuses it before the
# behaviour under test is reached. Here it stops the suite starting the
# confined session the instructions are asserted against.
# The helper runs this file again with the guard set, either inside a logind
# session it created or — saying why, out loud — in place; either way the
# suite runs exactly once, so this is `exec` and not a call. Same block, and
# the same reason, as tests/test-privilege-requests.sh.
if [ -z "${RIME_LOGIN_SESSION_WRAPPED:-}" ] && [ -x "${ROOT}/tests/in-login-session.sh" ]; then
    exec "${ROOT}/tests/in-login-session.sh" "${BASH_SOURCE[0]}" "$@"
fi
WORK="$(mktemp -d -p /var/tmp rime-profile-XXXXXX)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

DAEMON_PID=""
cleanup() {
    if [ -n "$DAEMON_PID" ]; then
        kill "$DAEMON_PID" 2>/dev/null
        for _ in 1 2 3 4 5; do kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done
        kill -9 "$DAEMON_PID" 2>/dev/null
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# ── prerequisites ────────────────────────────────────────────────────────────
# A missing prerequisite is a failure, never a skip. A suite that reports
# "0 passed, 0 failed" is a green tick over nothing asserted.
for tool in cargo python3 bwrap; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    printf '\nprofile: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "rime-agentd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

# ── a fixture profile ────────────────────────────────────────────────────────
# The shape of a real ~/.claude: instructions, a skill, a slash command, a
# status line, marketplace definitions, and beside them the three things a
# bundle must never carry — a transcript, a shell snapshot and a credential.
section "a fixture profile"
export HOME="${WORK}/home"
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

C="${HOME}/.claude"
# `agents` is here so the read-only assertion on it is a real one: a `-try`
# bind of a path that is not on the machine mounts nothing, and a write that
# fails with ENOENT would pass a test written for EROFS.
mkdir -p "${C}/skills/demo" "${C}/commands" "${C}/agents" "${C}/projects/proj" \
         "${C}/shell-snapshots" "${C}/plugins/marketplaces/mkt" "${C}/daemon"
printf 'be brief\n'                      > "${C}/CLAUDE.md"
printf '# demo skill\n'                  > "${C}/skills/demo/SKILL.md"
printf 'go\n'                            > "${C}/commands/go.md"
printf '---\nname: helper\n---\n'        > "${C}/agents/helper.md"
printf '#!/bin/sh\necho status\n'        > "${C}/statusline.sh"
chmod 0755 "${C}/statusline.sh"
printf '{"conversation":"PRIVATE-TRANSCRIPT"}\n' > "${C}/projects/proj/chat.jsonl"
printf 'export SHELL_STATE=PRIVATE-SNAPSHOT\n'   > "${C}/shell-snapshots/snap.sh"
printf '{"claudeAiOauth":"PRIVATE-OAUTH"}\n'     > "${C}/.credentials.json"
printf 'PRIVATE-DAEMON-KEY\n'                    > "${C}/daemon/control.key"
cat > "${C}/settings.json" <<'JSON'
{
  "model": "opus",
  "env": {"GITHUB_TOKEN": "ghp_PRIVATE-ENV-VALUE"},
  "enabledPlugins": {"demo@mkt": true},
  "statusLine": {"type": "command", "command": "~/.claude/statusline.sh"},
  "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "true"}]}]}
}
JSON
cat > "${C}/plugins/known_marketplaces.json" <<JSON
{"mkt": {"source": {"source": "github", "repo": "acme/mkt"},
         "installLocation": "${C}/plugins/marketplaces/mkt",
         "lastUpdated": "2026-01-01T00:00:00Z"}}
JSON
cat > "${HOME}/.claude.json" <<'JSON'
{
  "userID": "PRIVATE-USER-ID",
  "machineID": "PRIVATE-MACHINE-ID",
  "oauthAccount": {"emailAddress": "PRIVATE-EMAIL"},
  "mcpServers": {
    "vault": {"type": "http", "url": "https://vault.example/mcp",
              "headers": {"Authorization": "Bearer PRIVATE-BEARER"}},
    "memory": {"command": "npx", "args": ["-y", "server"],
               "env": {"API": "PRIVATE-MCP-ENV"}}
  }
}
JSON
ok "the fixture profile is in place"

# Every string that must never leave the machine. Asserted as a set rather
# than one by one, so a bundle format that grew a new file is covered by the
# same check.
SECRETS=(PRIVATE-TRANSCRIPT PRIVATE-SNAPSHOT PRIVATE-OAUTH PRIVATE-DAEMON-KEY
         PRIVATE-ENV-VALUE PRIVATE-USER-ID PRIVATE-MACHINE-ID PRIVATE-EMAIL
         PRIVATE-BEARER PRIVATE-MCP-ENV)

# ── list and inspect ─────────────────────────────────────────────────────────
section "list and inspect"
out="$("$Rime" agent profile list 2>&1)"
printf '%s' "$out" | pipe_has '^claude' \
    && ok "list names the claude profile" || { bad "list names the claude profile"; echo "$out"; }

"$Rime" agent profile inspect claude --json > "${WORK}/inspect.json" 2>"${WORK}/inspect.err"
python3 - "${WORK}/inspect.json" <<'PY' > "${WORK}/inspect.out" 2>&1
import json, sys
rows = {r["path"]: r for r in json.load(open(sys.argv[1]))}
want = {
    "~/.claude/CLAUDE.md":     ("reusable", "read-only"),
    "~/.claude/skills":        ("reusable", "read-only"),
    "~/.claude/commands":      ("reusable", "read-only"),
    "~/.claude/settings.json": ("mixed",    "read-only"),
    "~/.claude/projects":      ("machine-local", "writable"),
    "~/.claude/shell-snapshots": ("machine-local", "writable"),
    "~/.claude/.credentials.json": ("secret", "writable"),
}
bad = [f"{p}: {rows.get(p)} wanted {v}" for p, v in want.items()
       if (rows.get(p, {}).get("class"), rows.get(p, {}).get("mount")) != v]
print("\n".join(bad) if bad else "ok")
# The profile root is never itself an entry: binding the directory is what
# P0-010 replaced.
print("root-bound" if "~/.claude" in rows else "ok")
PY
grep -qx 'ok' "${WORK}/inspect.out" && [ "$(sort -u "${WORK}/inspect.out" | tr -d '\n')" = "ok" ] \
    && ok "inspect gives every entry a class and a mount" \
    || { bad "inspect gives every entry a class and a mount"; cat "${WORK}/inspect.out"; }

# ── doctor ───────────────────────────────────────────────────────────────────
section "doctor"
out="$("$Rime" agent profile doctor claude 2>&1)"; rc=$?
missing=""
for want in config statusline hooks commands skills plugins mcp credentials; do
    printf '%s' "$out" | pipe_has -x "$want" || missing="${missing} ${want}"
done
[ -z "$missing" ] && ok "doctor reports config, hooks, plugins, MCP and skills" \
    || { bad "doctor is missing sections:${missing}"; echo "$out"; }

printf '%s' "$out" | pipe_has '1 skills' \
    && ok "doctor counts the skills" || { bad "doctor counts the skills"; echo "$out"; }
printf '%s' "$out" | pipe_has 'SessionStart' \
    && ok "doctor names the hook events" || { bad "doctor names the hook events"; echo "$out"; }
printf '%s' "$out" | pipe_has 'vault.*needs Authorization' \
    && ok "doctor says an HTTP MCP server needs its header" \
    || { bad "doctor says an HTTP MCP server needs its header"; echo "$out"; }
printf '%s' "$out" | pipe_has '.credentials.json' && printf '%s' "$out" | pipe_has 'excluded' \
    && ok "doctor names the credentials and says they are not exported" \
    || { bad "doctor names the credentials and says they are not exported"; echo "$out"; }
[ "$rc" = 0 ] && ok "a healthy profile exits zero" || { bad "a healthy profile exits zero (got $rc)"; echo "$out"; }

# A skill directory with no SKILL.md does not load, and nothing upstream says so.
mkdir -p "${C}/skills/broken"
out="$("$Rime" agent profile doctor claude 2>&1)"; rc=$?
[ "$rc" != 0 ] && printf '%s' "$out" | pipe_has 'broken' \
    && ok "a skill that will not load is a problem and a non-zero exit" \
    || { bad "a skill that will not load is a problem and a non-zero exit"; echo "$out"; }
rmdir "${C}/skills/broken"

out="$("$Rime" agent profile doctor codex 2>&1)"
printf '%s' "$out" | pipe_has 'no profile description' \
    && ok "an agent Rime has no profile for is refused by name" \
    || { bad "an agent Rime has no profile for is refused by name"; echo "$out"; }

# ── export ───────────────────────────────────────────────────────────────────
section "export"
BUNDLE="${WORK}/bundle"
out="$("$Rime" agent profile export claude --to "$BUNDLE" 2>&1)"; rc=$?
[ "$rc" = 0 ] && ok "export writes a bundle" || { bad "export writes a bundle"; echo "$out"; }

for want in profile/CLAUDE.md profile/settings.json profile/skills/demo/SKILL.md \
            profile/commands/go.md profile/statusline.sh \
            profile/plugins/known_marketplaces.json home/.claude.json manifest.json; do
    [ -e "${BUNDLE}/${want}" ] || { bad "the bundle carries ${want}"; continue; }
done
ok "the bundle carries the reusable half"

leaked=""
for s in "${SECRETS[@]}"; do
    grep -rq -- "$s" "$BUNDLE" 2>/dev/null && leaked="${leaked} ${s}"
done
[ -z "$leaked" ] && ok "no credential, transcript or machine identity reached the bundle" \
    || bad "the bundle carries:${leaked}"

[ -x "${BUNDLE}/profile/statusline.sh" ] \
    && ok "the status line keeps its executable bit" \
    || bad "the status line keeps its executable bit"

python3 - "$BUNDLE" <<'PY' > "${WORK}/manifest.out" 2>&1
import json, os, sys
b = sys.argv[1]
doc = json.load(open(os.path.join(b, "manifest.json")))
assert doc["agent"] == "claude", doc
assert doc["version"] == 1, doc
bad = [f for f in doc["files"] if f["class"] not in ("reusable", "mixed")]
assert not bad, bad
# The names survive so the importing machine knows what to supply.
s = json.load(open(os.path.join(b, "profile/settings.json")))
assert s["env"] == {"GITHUB_TOKEN": None}, s["env"]
assert s["model"] == "opus", s
m = json.load(open(os.path.join(b, "home/.claude.json")))
assert list(m) == ["mcpServers"], list(m)
assert m["mcpServers"]["vault"]["headers"] == {"Authorization": None}, m
assert m["mcpServers"]["vault"]["url"] == "https://vault.example/mcp", m
assert "oauthAccount" not in m["mcpServers"]["vault"], m
k = json.load(open(os.path.join(b, "profile/plugins/known_marketplaces.json")))
assert "installLocation" not in k["mkt"], k
assert k["mkt"]["source"]["repo"] == "acme/mkt", k
print("ok")
PY
grep -qx ok "${WORK}/manifest.out" \
    && ok "the bundle keeps the names and drops the values" \
    || { bad "the bundle keeps the names and drops the values"; cat "${WORK}/manifest.out"; }

out="$("$Rime" agent profile export claude --to "$BUNDLE" 2>&1)"
printf '%s' "$out" | pipe_has 'pass --force' \
    && ok "export refuses to write over an occupied directory" \
    || { bad "export refuses to write over an occupied directory"; echo "$out"; }

# ── sync onto another machine ────────────────────────────────────────────────
section "sync"
OTHER="${WORK}/other"
mkdir -p "${OTHER}/.claude"
printf '{"model":"sonnet","env":{"GITHUB_TOKEN":"ghp_THEIR-OWN-VALUE"}}\n' \
    > "${OTHER}/.claude/settings.json"

out="$(HOME="$OTHER" "$Rime" agent profile sync claude --from "$BUNDLE" --dry-run 2>&1)"
printf '%s' "$out" | pipe_has 'would change' \
    && ok "a dry run says what it would do and does nothing" \
    || { bad "a dry run says what it would do and does nothing"; echo "$out"; }
[ ! -e "${OTHER}/.claude/CLAUDE.md" ] \
    && ok "a dry run wrote nothing" || bad "a dry run wrote nothing"

out="$(HOME="$OTHER" "$Rime" agent profile sync claude --from "$BUNDLE" 2>&1)"; rc=$?
[ "$rc" = 0 ] && ok "sync applies the bundle" || { bad "sync applies the bundle"; echo "$out"; }
[ -f "${OTHER}/.claude/skills/demo/SKILL.md" ] \
    && ok "the skills arrived" || bad "the skills arrived"
[ ! -e "${OTHER}/.claude/projects" ] && [ ! -e "${OTHER}/.claude/.credentials.json" ] \
    && ok "nothing machine-local followed them" || bad "nothing machine-local followed them"

python3 - "$OTHER" <<'PY' > "${WORK}/sync.out" 2>&1
import json, os, sys
s = json.load(open(os.path.join(sys.argv[1], ".claude/settings.json")))
assert s["model"] == "opus", s
assert s["env"]["GITHUB_TOKEN"] == "ghp_THEIR-OWN-VALUE", s
print("ok")
PY
grep -qx ok "${WORK}/sync.out" \
    && ok "the import merged and did not overwrite the local value with a blank" \
    || { bad "the import merged and did not overwrite the local value with a blank"; cat "${WORK}/sync.out"; }
printf '%s' "$out" | pipe_has 'GITHUB_TOKEN' \
    && ok "the operator is told which values the bundle could not carry" \
    || { bad "the operator is told which values the bundle could not carry"; echo "$out"; }

out="$(HOME="$OTHER" "$Rime" agent profile sync claude --from "$BUNDLE" 2>&1)"
printf '%s' "$out" | pipe_has 'nothing to change' \
    && ok "a second sync of the same bundle changes nothing" \
    || { bad "a second sync of the same bundle changes nothing"; echo "$out"; }

# A bundle is a file somebody sent. A hand-edited manifest must reach nothing.
HOSTILE="${WORK}/hostile"
mkdir -p "${HOSTILE}/home/.ssh"
printf 'ssh-rsa AAAA attacker\n' > "${HOSTILE}/home/.ssh/authorized_keys"
printf '{"version":1,"agent":"claude","files":[{"path":"home/.ssh/authorized_keys","class":"reusable"}]}\n' \
    > "${HOSTILE}/manifest.json"
HOME="$OTHER" "$Rime" agent profile sync claude --from "$HOSTILE" >/dev/null 2>&1
[ ! -e "${OTHER}/.ssh/authorized_keys" ] \
    && ok "a manifest naming a path outside the profile reaches nothing" \
    || bad "a manifest naming a path outside the profile reaches nothing"

# ── the mounts a confined session actually gets (P0-010) ─────────────────────
#
# A real daemon, a real bwrap namespace and a real agent process. The agent is
# a stub on PATH, because `claude` is resolved through PATH at spawn time —
# which is what lets a user's own build win, and what lets this assert the
# mount code without an account or a network.
section "the mounts a confined session gets"
STUB="${WORK}/stub"
mkdir -p "$STUB"
# The probe reports into the project directory, which is bound read-write and
# outlives the session — a transcript does not: the runtime drops a finished
# session's log, so a suite that read one would be asserting on an empty file.
#
# Every write is attempted in a subshell. A redirection failure on a POSIX
# special built-in (`:` is one) exits a non-interactive shell outright, so a
# probe written the obvious way stops at the first read-only mount and reports
# nothing at all — which reads as a session that never ran.
cat > "${STUB}/claude" <<'STUBEOF'
#!/bin/sh
# Not an agent: a probe that reports what the mount namespace let it do.
OUT="${PWD}/probe.out"
: > "$OUT"
p() { printf '%s=%s\n' "$1" "$2" >> "$OUT"; }
w() { ( printf 'probe\n' > "$2" ) >/dev/null 2>&1; p "$1" $?; }
a() { ( printf 'tampered\n' >> "$2" ) >/dev/null 2>&1; p "$1" $?; }
r() { ( cat "$2" ) >/dev/null 2>&1; p "$1" $?; }

w write_skills          "$HOME/.claude/skills/probe"
w write_commands        "$HOME/.claude/commands/probe"
w write_agents          "$HOME/.claude/agents/probe"
a append_instructions   "$HOME/.claude/CLAUDE.md"
w overwrite_settings    "$HOME/.claude/settings.json"
w overwrite_marketplace "$HOME/.claude/plugins/known_marketplaces.json"

w write_projects        "$HOME/.claude/projects/session-probe"
w write_plugin_cache    "$HOME/.claude/plugins/cache/session-probe"
w write_plugin_data     "$HOME/.claude/plugins/data/session-probe"
w write_todos           "$HOME/.claude/todos/session-probe"
w write_sidecar         "$HOME/.claude.json"
w write_unlisted        "$HOME/.claude/invented-by-a-later-release"

r read_skills           "$HOME/.claude/skills/demo/SKILL.md"
r read_instructions     "$HOME/.claude/CLAUDE.md"
r read_credentials      "$HOME/.claude/.credentials.json"
r read_ssh              "$HOME/.ssh/id_ed25519"

# P0-003 criterion 1. The settings document as the session reads it, and the
# whole environment, both copied out so the sentinel can be looked for rather
# than reasoned about. The fixture's settings.json carries a PAT in its `env`
# block, which Claude would otherwise apply to every tool it runs.
( cat "$HOME/.claude/settings.json" ) > "${PWD}/probe-settings.json" 2>/dev/null
( env ) > "${PWD}/probe-env.txt" 2>/dev/null

# P0-003 criterion 3, observed rather than reasoned about. `git` must be the
# shim, `git --version` must still be the real git's answer, and whatever `gh`
# says about its own authentication is recorded verbatim — the claim in the
# report is about what it actually says, not about what the mount table implies
# it would say.
( command -v git ) > "${PWD}/probe-git.txt" 2>&1
( git --version ) >> "${PWD}/probe-git.txt" 2>&1
( command -v gh && gh auth status ) > "${PWD}/probe-gh.txt" 2>&1
p done 0
STUBEOF
chmod 0755 "${STUB}/claude"
export PATH="${STUB}:${PATH}"

# `todos` is deliberately absent from the fixture: the daemon has to create the
# writable directories before the session, or bwrap's `-try` bind is a no-op
# and everything written there lands in the tmpfs that masks $HOME.
[ ! -e "${C}/todos" ] || rmdir "${C}/todos"
# A private key that must stay out of reach, so the sandbox's own default-deny
# is asserted around all of this rather than assumed.
mkdir -p "${HOME}/.ssh"; printf 'PRIVATE-SSH-KEY\n' > "${HOME}/.ssh/id_ed25519"

mkdir -p "${WORK}/proj"
"$AGENTD" > "${WORK}/agentd.log" 2>&1 &
DAEMON_PID=$!
SOCK="${XDG_RUNTIME_DIR}/rime-agentd/control.sock"
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
if [ -S "$SOCK" ]; then
    ok "the daemon came up on an isolated socket"
else
    bad "the daemon came up on an isolated socket"
    sed 's/^/      /' "${WORK}/agentd.log"
    printf '\nprofile: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

out="$("$Rime" agent run --detach --agent claude --sandbox project --network offline \
        --cwd "${WORK}/proj" 2>&1)"
id="$(printf '%s' "$out" | sed -n 's/^session \([0-9]*\) .*/\1/p' | head -1)"
if [ -n "$id" ]; then
    ok "a confined claude session started (id ${id})"
else
    bad "a confined claude session started"
    printf '%s\n' "$out" | sed 's/^/      /'
    sed 's/^/      /' "${WORK}/agentd.log"
    printf '\nprofile: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

LOG="${WORK}/proj/probe.out"
for _ in $(seq 1 150); do
    grep -q '^done=' "$LOG" 2>/dev/null && break
    sleep 0.1
done
if ! grep -q '^done=' "$LOG" 2>/dev/null; then
    bad "the session ran to completion"
    cat "$LOG" 2>/dev/null | sed 's/^/      /'
    sed 's/^/      /' "${WORK}/agentd.log"
    printf '\nprofile: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "the session ran to completion"
probe() { grep -o "^$1=[0-9]*" "$LOG" | tail -1 | cut -d= -f2; }

# Criterion 1: config, skills and commands are mounted read-only.
refused=""
for name in write_skills write_commands write_agents append_instructions \
            overwrite_settings overwrite_marketplace; do
    [ "$(probe "$name")" = "0" ] && refused="${refused} ${name}"
done
[ -z "$refused" ] && ok "config, skills, commands and agents are read-only to the session" \
    || { bad "the session could write:${refused}"; cat "$LOG"; }
[ "$(probe read_skills)" = "0" ] && [ "$(probe read_instructions)" = "0" ] \
    && ok "and still readable" || { bad "and still readable"; cat "$LOG"; }
grep -q tampered "${C}/CLAUDE.md" \
    && bad "the session rewrote the instructions it was started with" \
    || ok "the instructions on disk are untouched"

# Criterion 2: writable session and plugin state, isolated from the rest.
denied=""
for name in write_projects write_plugin_cache write_plugin_data write_todos \
            write_sidecar write_unlisted; do
    [ "$(probe "$name")" = "0" ] || denied="${denied} ${name}"
done
[ -z "$denied" ] && ok "session and plugin state is writable" \
    || { bad "the session could not write:${denied}"; cat "$LOG"; }
[ -e "${C}/projects/session-probe" ] && [ -e "${C}/plugins/cache/session-probe" ] \
    && ok "what the session wrote to its session and plugin state persisted" \
    || bad "what the session wrote to its session and plugin state persisted"
[ -d "${C}/todos" ] && [ -e "${C}/todos/session-probe" ] \
    && ok "a missing writable directory was created before the session" \
    || bad "a missing writable directory was created before the session"
[ ! -e "${C}/invented-by-a-later-release" ] \
    && ok "a path the table does not name stayed in the session's own overlay" \
    || bad "a path the table does not name stayed in the session's own overlay"

# The sandbox's own default-deny still holds around all of it.
[ "$(probe read_credentials)" = "0" ] \
    && ok "the agent still reaches its own credentials" \
    || { bad "the agent still reaches its own credentials"; cat "$LOG"; }
[ "$(probe read_ssh)" != "0" ] \
    && ok "the rest of the home is still not there" \
    || { bad "the rest of the home is still not there"; cat "$LOG"; }

# ── P0-003 criterion 1: the credential in settings.json is not in the session ─
#
# Asserted as an absence of the sentinel, in the file the session reads and in
# the environment it runs with. The value is a fake; what is real is the path it
# would take — Claude reads its own `env` block after the process has started,
# so `--clearenv` never sees it.
SETTINGS_SEEN="${WORK}/proj/probe-settings.json"
SESSION_ENV="${WORK}/proj/probe-env.txt"
if [ -s "$SETTINGS_SEEN" ]; then
    ok "the session read a settings document"
    grep -q 'PRIVATE-ENV-VALUE' "$SETTINGS_SEEN" \
        && bad "the settings document the session read still carries the PAT" \
        || ok "the settings document the session read carries no credential"
    grep -q '"GITHUB_TOKEN"' "$SETTINGS_SEEN" \
        && bad "the name is still there, so the session exports an empty token" \
        || ok "the credential's name went with its value"
    grep -q 'opus' "$SETTINGS_SEEN" \
        && ok "and the model, hooks and theme survived the copy" \
        || { bad "the copy lost settings the session needs"; cat "$SETTINGS_SEEN"; }
else
    bad "the session read a settings document"
fi
if [ -s "$SESSION_ENV" ]; then
    grep -q 'PRIVATE-ENV-VALUE' "$SESSION_ENV" \
        && bad "the PAT is in the session's environment" \
        || ok "no credential reached the session's environment"
else
    bad "the session reported its environment"
fi
# ── P0-003 criterion 3: git is the shim, and gh is whatever gh is ────────────
GIT_SEEN="${WORK}/proj/probe-git.txt"
GH_SEEN="${WORK}/proj/probe-gh.txt"
if [ -s "$GIT_SEEN" ]; then
    grep -q 'bin/git' "$GIT_SEEN" \
        && ok "the session's git is Rime's shim" \
        || { bad "the session's git is Rime's shim"; cat "$GIT_SEEN"; }
    grep -q 'git version' "$GIT_SEEN" \
        && ok "and it still answers as git for everything it does not broker" \
        || { bad "the shim did not pass through"; cat "$GIT_SEEN"; }
else
    bad "the session reported which git it found"
fi
# Recorded, not asserted either way: ~/.config/gh is not in the profile table,
# so a session sees no gh configuration and gh is unauthenticated inside one —
# before this change and after it. Printed so the claim is observed.
printf '      gh inside the session: %s\n' \
    "$(head -3 "$GH_SEEN" 2>/dev/null | tr '\n' ' ' | sed 's/  */ /g')"
grep -q 'PRIVATE-SSH-KEY\|oauth_token' "$GH_SEEN" 2>/dev/null \
    && bad "gh printed a credential inside the session" \
    || ok "gh printed no credential inside the session"

# Nothing under the fixture's own ~/.claude was edited to achieve any of this.
grep -q 'PRIVATE-ENV-VALUE' "${C}/settings.json" \
    && ok "the user's own settings.json is untouched on disk" \
    || bad "the daemon edited the user's settings.json"

printf '\nprofile: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
