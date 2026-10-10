#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for structured privilege requests (roadmap §4).
#
#  The unit tests cover the vocabulary, the argument validation and the grant
#  store. What they cannot cover is the claim the whole design rests on:
#
#      the daemon learns which session is asking from the KERNEL, not from
#      anything the asking process said.
#
#  That needs a real daemon, a real session and a real socket, so it is tested
#  here against all three.
#
#  NO ASSERTION IN THIS FILE NEEDS ROOT. Every approval uses `--no-run`, which
#  records the decision without performing the operation, so running the suite
#  never installs a package and never raises an authentication prompt. The
#  execution path is the one thing asserted only by unit tests, deliberately.
#
#  One thing around the assertions does use root, and only when it has to: §7
#  reserves approving a root operation for a human at this machine, and the
#  daemon establishes that from the peer's cgroup. A runner started by systemd
#  — a CI job, a timer-dispatched agent — is not in a login session and is
#  refused before the behaviour under test is reached, which is how eight of
#  these assertions went three integration rounds without executing once. So
#  the suite re-enters itself inside a real logind session first, which costs
#  one `sudo -n` (never a prompt) and drops straight back to the invoking
#  user. See tests/in-login-session.sh for the mechanism, for why it is not
#  allowed to fake one, and for what happens when it cannot get one.
#
#      ./tests/test-privilege-requests.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` is deliberate and load-bearing. This suite COUNTS failures rather
# than aborting on them, and several assertions run commands that exit non-zero
# on purpose — a refusal, a guard firing, a bad argument. GitHub Actions invokes
# a script as `bash -e {0}`, and under `-e` a `x="$(cmd)"` assignment whose
# command exits non-zero terminates the whole script. That is exactly what
# happened: the suite passed locally, and on CI it died part-way through with
# the remaining assertions reported as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Before the temp tree, the daemon or the build: the environment this suite
# needs is one the daemon will observe as local, and that has to be arranged
# from outside the suite. The helper runs this file again with the guard set,
# either inside a logind session it created or — saying why, out loud — in
# place. Either way the suite runs exactly once, so this is `exec` and not a
# call. The origin it actually got is asserted below, once the daemon is up,
# rather than assumed from the fact that this line was reached.
if [ -z "${RIME_LOGIN_SESSION_WRAPPED:-}" ] && [ -x "${ROOT}/tests/in-login-session.sh" ]; then
    exec "${ROOT}/tests/in-login-session.sh" "${BASH_SOURCE[0]}" "$@"
fi

WORK="$(mktemp -d)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
# There is deliberately no `skp` helper. It existed for exactly one caller —
# the whole-suite skip on a missing cargo — and a suite with a skip helper
# lying around invites the next one.
section() { printf '\n── %s ──\n' "$1"; }

DAEMON_PID=""
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    # Give it a moment to tear down its sessions before the tree goes away.
    [ -n "$DAEMON_PID" ] && { for _ in 1 2 3 4 5; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done; }
    [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

# ── prerequisites ────────────────────────────────────────────────────────────
#
# A missing prerequisite is a FAILURE, never a skip. This suite used to
# whole-suite-skip on a missing `cargo`, print "0 passed, 0 failed (skipped)"
# and exit 0 — a green tick over nothing asserted, which is the shape
# docs/p1-progress.md already records this repository being bitten by three
# times, most recently when the labwc keybind suite reported passed=0 failed=0
# on its first CI run.
for tool in cargo git python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

# ── build ────────────────────────────────────────────────────────────────────
section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    printf '\nprivilege: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "rime-agentd and rime build"

# Honours CARGO_TARGET_DIR, because cargo does: a checkout on a small tmpfs is
# built with the target directory pointed elsewhere, and hardcoding the path
# makes the suite look broken when the build was fine.
BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

# ── an isolated runtime ──────────────────────────────────────────────────────
# Separate XDG_RUNTIME_DIR and XDG_STATE_HOME, so this never touches the
# developer's own sessions, requests, grants or audit log.
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

section "the daemon"
"$AGENTD" > "${WORK}/agentd.log" 2>&1 &
DAEMON_PID=$!
SOCK="${XDG_RUNTIME_DIR}/rime-agentd/control.sock"
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
if [ -S "$SOCK" ]; then
    ok "the daemon came up on an isolated socket"
else
    bad "the daemon came up on an isolated socket"
    sed 's/^/      /' "${WORK}/agentd.log"
    printf '\nprivilege: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ── the origin this runner presents ──────────────────────────────────────────
#
# The precondition for everything below that decides a request, made into an
# assertion of its own. Without it, an environment the daemon cannot see a
# human in produces eight failures further down whose messages are all about
# the approval path and none of which mention the actual cause — which is
# exactly how those eight sat unexecuted through three integration rounds.
#
# `local-terminal` and `rime-shell` are §7's two local origins, the ones
# `RequestOrigin::is_local` answers true for; a shell prompt has a
# controlling terminal and the desktop shell does not, and the daemon reads
# that from `/proc/<pid>/stat`, so which of the two appears here depends on
# how the suite was entered and neither is better than the other.
#
# `origin_source` is asserted alongside it because a local origin that arrived
# by DECLARATION rather than observation would be the security hole this whole
# mechanism exists to close, not a passing precondition.
section "the origin this runner presents"
probe="$("$Rime" request ask update \
        --reason "Establishing which origin the daemon observes for this runner" \
        --no-wait 2>/dev/null)"
observed="$("$Rime" request list --all --json 2>/dev/null | python3 -c "
import json, sys
rs = json.load(sys.stdin)
r = [x for x in rs if x['id'] == int('${probe:-0}')]
print('%s/%s' % (r[0].get('request_origin'), r[0].get('origin_source')) if r else 'nothing-filed')
" 2>/dev/null)"
case "$observed" in
    local-terminal/observed|rime-shell/observed)
        ok "the daemon observes this runner as a human at this machine (${observed%%/*})" ;;
    *)
        bad "the daemon observes this runner as a human at this machine (got '${observed}')"
        cat >&2 <<'WHY'
      §7 reserves approving a root operation for a local origin, so every
      assertion below that decides a request is going to fail for THIS reason
      and not for the reason it is testing. tests/in-login-session.sh is what
      arranges a local origin; if it printed a line above saying it could not,
      that line is the cause.
WHY
        ;;
esac

# ── the vocabulary is closed ─────────────────────────────────────────────────
section "the vocabulary is closed"
for evil in exec sh bash sudo eval run; do
    out="$("$Rime" request ask "$evil" whoami --reason "trying it on" 2>&1)"
    printf '%s' "$out" | pipe_has "not a privileged operation" \
        || { bad "'$evil' is refused"; continue; }
    ok "'$evil' is refused"
done

out="$("$Rime" request ask install 'clang; rm -rf /' --reason "smuggling" 2>&1)"
printf '%s' "$out" | pipe_has "not a valid package name" \
    && ok "a shell metacharacter in a package name is refused" \
    || bad "a shell metacharacter in a package name is refused"

out="$("$Rime" request ask install /etc/passwd --reason "path" 2>&1)"
printf '%s' "$out" | pipe_has "not a valid package name" \
    && ok "a path is not a package name" || bad "a path is not a package name"

out="$("$Rime" request ask install clang --reason "" 2>&1)"
printf '%s' "$out" | pipe_has "must not be empty" \
    && ok "a request with no reason is refused" || bad "a request with no reason is refused"

# ── filing from an ordinary terminal ─────────────────────────────────────────
# This process is NOT a managed session, so the daemon must attribute the
# request to no session at all rather than guessing.
section "a request from an ordinary terminal has no session"
id="$("$Rime" request ask install clang --reason "Required to compile the project" \
        --no-wait 2>"${WORK}/ask.err")"
if [ -n "$id" ]; then
    ok "the request was filed (id ${id})"
else
    bad "the request was filed"
    sed 's/^/      /' "${WORK}/ask.err"
fi

json="$("$Rime" request list --all --json 2>/dev/null)"
printf '%s' "$json" | python3 -c "
import json,sys
rs = json.load(sys.stdin)
r = [x for x in rs if x['id'] == int('${id:-0}')][0]
assert r['verb'] == 'install', r
assert r['packages'] == ['clang'], r
assert r['decision'] == 'pending', r
assert r['session'] is None, f\"attributed to a session it did not come from: {r}\"
assert r['reason'] == 'Required to compile the project', r
" 2>"${WORK}/attr.err" \
    && ok "it is pending, unattributed, and carries the reason" \
    || { bad "it is pending, unattributed, and carries the reason"; sed 's/^/      /' "${WORK}/attr.err"; }

# ── the prompt ───────────────────────────────────────────────────────────────
section "the approval prompt"
prompt="$("$Rime" request show "$id" 2>&1)"
for want in "rime install clang" "Reason" "Required to compile the project" "Effect"; do
    printf '%s' "$prompt" | pipe_has -F "$want" \
        && ok "the prompt shows: ${want}" || bad "the prompt shows: ${want}"
done

# ── deciding ─────────────────────────────────────────────────────────────────
# --no-run throughout: records the decision, performs nothing, needs no root.
section "deciding"
out="$(printf 'y\n' | "$Rime" request approve "$id" --no-run 2>&1)"
printf '%s' "$out" | pipe_has -E "approved" \
    && ok "an unsessioned peer may approve" || { bad "an unsessioned peer may approve"; printf '      %s\n' "$out"; }

out="$(printf 'y\n' | "$Rime" request approve "$id" --no-run 2>&1)"
printf '%s' "$out" | pipe_has "already" \
    && ok "a decided request cannot be re-decided" || bad "a decided request cannot be re-decided"

id2="$("$Rime" request ask pin --reason "pinning before an upgrade" --no-wait 2>/dev/null)"
out="$("$Rime" request deny "$id2" 2>&1)"
printf '%s' "$out" | pipe_has "denied" && ok "a request can be denied" || bad "a request can be denied"
out="$(printf 'y\n' | "$Rime" request approve "$id2" --no-run 2>&1)"
printf '%s' "$out" | pipe_has "already denied" \
    && ok "a denied request cannot be flipped to approved" \
    || { bad "a denied request cannot be flipped to approved"; printf '      %s\n' "$out"; }

# ── the audit log ────────────────────────────────────────────────────────────
section "the audit log"
LOG="${XDG_STATE_HOME}/rime/agent/privilege-audit.jsonl"
if [ -s "$LOG" ]; then
    ok "an audit log was written"
    python3 - "$LOG" <<'PY' && ok "every line is one JSON object with argv and event" \
        || bad "every line is one JSON object with argv and event"
import json,sys
n = 0
for line in open(sys.argv[1]):
    line = line.strip()
    if not line: continue
    o = json.loads(line)
    assert 'event' in o and 'argv' in o and 'ms' in o, o
    n += 1
assert n >= 3, f"expected at least requested/decided entries, got {n}"
PY
    grep -q '"event":"requested"' "$LOG" || grep -q '"event": "requested"' "$LOG" \
        && ok "the filing is recorded" || bad "the filing is recorded"
    grep -q 'decided' "$LOG" && ok "the decision is recorded" || bad "the decision is recorded"
else
    bad "an audit log was written"
fi

# ── grants ───────────────────────────────────────────────────────────────────
section "grants"
"$Rime" request grants 2>&1 | pipe_has "nothing is granted" \
    && ok "no grant is created by an allow-once" || bad "no grant is created by an allow-once"

# ── the property the design rests on ─────────────────────────────────────────
# A request filed from INSIDE a managed session must be attributed to that
# session by the daemon, and that session must not be able to approve itself.
#
# The session runs a shell script that files a request and then tries to
# approve it. Nothing it does can succeed at approving; if it can, the whole
# subsystem is decoration.
section "a session cannot approve itself"
PROJ="${WORK}/project"
mkdir -p "$PROJ"
git -C "$PROJ" init -q 2>/dev/null
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init 2>/dev/null

cat > "${WORK}/inside.sh" <<EOF
#!/bin/sh
# Runs INSIDE a managed session. \$RIME_AGENT_SESSION is set here and is
# exactly what must NOT be trusted for authorisation.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR}"
export XDG_STATE_HOME="${XDG_STATE_HOME}"
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME}"
echo "SESSION_ENV=\${RIME_AGENT_SESSION:-unset}"
inner="\$("$Rime" request ask install cmake --reason "needed by the build" --no-wait 2>/dev/null)"
echo "INNER_ID=\$inner"
echo "--- trying to approve its own request ---"
"$Rime" request approve "\$inner" --no-run 2>&1
echo "APPROVE_EXIT=\$?"
echo "--- trying to deny it ---"
"$Rime" request deny "\$inner" 2>&1
echo "DENY_EXIT=\$?"
echo "--- trying to grant itself something ---"
"$Rime" request revoke "${PROJ}" 2>&1
echo "REVOKE_EXIT=\$?"
# The negative control for peer resolution: LIE about the session id. The
# daemon must still attribute this to the real session, because it never
# reads this variable — it walks /proc from the connection's peer pid.
echo "--- filing while claiming to be a different session ---"
lied="\$(RIME_AGENT_SESSION=99999 "$Rime" request ask install ninja-build \\
          --reason "filed while lying about the session" --no-wait 2>/dev/null)"
echo "LIED_ID=\$lied"
echo "DONE"
EOF
chmod +x "${WORK}/inside.sh"

# `unrestricted` so the session can reach the built binary and this script
# without a bind allowlist; peer resolution is by /proc ancestry, which is
# identical under every policy. The confined case is covered by the sandbox
# suite in rime-agent-core.
sid="$("$Rime" agent run --agent generic --sandbox unrestricted --cwd "$PROJ" -d \
        -- /bin/sh "${WORK}/inside.sh" 2>"${WORK}/run.err" \
        | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
if [ -z "$sid" ]; then
    bad "a session started"
    sed 's/^/      /' "${WORK}/run.err"
else
    ok "a session started (id ${sid})"
    # Wait for the script to finish.
    for _ in $(seq 1 80); do
        "$Rime" agent logs "$sid" 2>/dev/null | pipe_has DONE && break
        sleep 0.25
    done
    logs="$("$Rime" agent logs "$sid" 2>/dev/null)"
    printf '%s\n' "$logs" | sed 's/^/      | /'

    printf '%s' "$logs" | pipe_has "SESSION_ENV=${sid}" \
        && ok "the session sees its own id in the environment" \
        || bad "the session sees its own id in the environment"

    inner="$(printf '%s' "$logs" | sed -n 's/^INNER_ID=\([0-9]*\).*/\1/p' | tr -d '\r')"
    if [ -n "$inner" ]; then
        ok "the session filed a request (id ${inner})"
        # THE assertion: the daemon attributed it from the peer credentials.
        "$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
rs = json.load(sys.stdin)
r = [x for x in rs if x['id'] == ${inner}][0]
assert r['session'] == ${sid}, f\"session not resolved from peer credentials: {r}\"
assert r['project'] is not None, f'project not resolved: {r}'
" 2>"${WORK}/peer.err" \
            && ok "the daemon resolved the session from the peer's credentials" \
            || { bad "the daemon resolved the session from the peer's credentials"; sed 's/^/      /' "${WORK}/peer.err"; }
    else
        bad "the session filed a request"
    fi

    printf '%s' "$logs" | pipe_has "cannot decide its own privilege request" \
        && ok "the session's own approval was REFUSED" \
        || bad "the session's own approval was REFUSED"
    printf '%s' "$logs" | pipe_has "cannot change its own grants" \
        && ok "the session cannot change its own grants" \
        || bad "the session cannot change its own grants"

    # The negative control for peer resolution. The session filed a request
    # with RIME_AGENT_SESSION=99999. If the daemon were reading that variable
    # — or any other client-supplied value — the record would say 99999, and a
    # lying agent could attribute its requests to someone else's session or to
    # no session at all (which is the identity permitted to APPROVE).
    lied="$(printf '%s' "$logs" | sed -n 's/^LIED_ID=\([0-9]*\).*/\1/p' | tr -d '\r')"
    if [ -n "$lied" ]; then
        "$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
r = [x for x in json.load(sys.stdin) if x['id'] == ${lied}][0]
assert r['session'] == ${sid}, \\
    f'a client-supplied session id was believed: {r}'
assert r['session'] != 99999, f'the daemon read RIME_AGENT_SESSION: {r}'
" 2>"${WORK}/lie.err" \
            && ok "a session lying about its id is still attributed correctly" \
            || { bad "a session lying about its id is still attributed correctly"; sed 's/^/      /' "${WORK}/lie.err"; }
    else
        bad "the lying request was filed"
    fi

    # And it is still pending afterwards — the refusal is not cosmetic.
    if [ -n "${inner:-}" ]; then
        "$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
r = [x for x in json.load(sys.stdin) if x['id'] == ${inner}][0]
assert r['decision'] == 'pending', f'the session changed its own decision: {r}'
assert r['executed_ms'] is None, r
" 2>/dev/null \
            && ok "its request is still pending, so the refusal was real" \
            || bad "its request is still pending, so the refusal was real"
    fi
fi

# ── allow-for-project ────────────────────────────────────────────────────────
section "allow for project"
if [ -n "${inner:-}" ]; then
    out="$(printf 'y\n' | "$Rime" request approve "$inner" --for-project --no-run 2>&1)"
    printf '%s' "$out" | pipe_has "allow_for_project" \
        && ok "an approval can be scoped to the project" \
        || { bad "an approval can be scoped to the project"; printf '      %s\n' "$out"; }

    "$Rime" request grants 2>/dev/null | pipe_has "install:cmake" \
        && ok "the grant is recorded against the project and the exact package" \
        || bad "the grant is recorded against the project and the exact package"

    # The point of the grant: the identical request no longer prompts.
    again="$("$Rime" request ask install cmake --reason "again" --no-wait 2>/dev/null)"
    if [ -n "$again" ]; then
        # Filed from an unsessioned peer, so it has no project and must NOT be
        # auto-granted — a grant with no project to match is not a match.
        "$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
r = [x for x in json.load(sys.stdin) if x['id'] == ${again}][0]
assert r['decision'] == 'pending', \\
    f'a grant matched a request with no project: {r}'
" 2>/dev/null \
            && ok "a project grant does not match a request with no project" \
            || bad "a project grant does not match a request with no project"
    fi

    # A DIFFERENT package in the same project must still prompt.
    "$Rime" request revoke "${PROJ}" >/dev/null 2>&1
    "$Rime" request grants 2>/dev/null | pipe_has "nothing is granted" \
        && ok "a grant can be revoked" || bad "a grant can be revoked"
fi

printf '\nprivilege: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
