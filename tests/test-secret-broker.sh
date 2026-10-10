#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for the Rime secret broker (roadmap §3.2, §11).
#
#  The claim is one sentence: agents use credentials without receiving them.
#  Everything else is plumbing. So the central test here stores a SENTINEL
#  credential, uses a capability from inside a real confined session, and
#  asserts the sentinel appears in
#
#      * the command's stdout
#      * the command's stderr
#      * the session's own PTY transcript
#      * the audit trail
#
#  ...in none of them. If it appears anywhere, the service has failed at the
#  only thing it exists for.
#
#  TWO DAEMONS, and which one answers is the point. rime-secretd owns the store
#  and performs the operation; rime-agentd owns the session, its secret policy
#  and its project, and forwards a capability record. Neither alone can do what
#  P0-002 asks: the agent runtime runs as the user, so a store it owned would be
#  a store the agent owned.
#
#  NO NETWORK IS USED. The fixture remote points at https://127.0.0.1:1/, which
#  refuses instantly, so git fails fast and the credential is sent nowhere. The
#  point is not that the fetch succeeds — that is proven hermetically by the
#  Rust suite in rimed/rime-secretd/tests/end_to_end.rs, against a real
#  credential-checking server — it is that the credential stayed on the
#  service's side while the attempt was made.
#
#  NO ROOT. rime-secretd is started with --store and --socket and runs as the
#  invoking user, so it reports `protected: false`. That costs this suite the
#  at-rest half of the boundary, which needs a uid it does not have; the API
#  half is the same code either way.
#
#      ./tests/test-secret-broker.sh
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

# Before the temp tree, the daemon or the build: this suite needs an
# environment the daemon will observe as LOCAL, and that has to be arranged
# from outside the suite. `rime-agentd` places a peer from its cgroup, and a
# process started by systemd — a CI job, a timer-dispatched agent — is in
# neither a login session nor a user service, so §7 refuses it before the
# behaviour under test is reached. Here it stops the suite starting the
# confined session the broker boundary is measured across.
# The helper runs this file again with the guard set, either inside a logind
# session it created or — saying why, out loud — in place; either way the
# suite runs exactly once, so this is `exec` and not a call. Same block, and
# the same reason, as tests/test-privilege-requests.sh.
if [ -z "${RIME_LOGIN_SESSION_WRAPPED:-}" ] && [ -x "${ROOT}/tests/in-login-session.sh" ]; then
    exec "${ROOT}/tests/in-login-session.sh" "${BASH_SOURCE[0]}" "$@"
fi
WORK="$(mktemp -d)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

DAEMON_PID=""
SECRETD_PID=""
# Only ever this script's own children, by recorded pid. Never by name: the
# developer's own rime-agentd is usually running, and a previous version of a
# suite like this one killed it.
cleanup() {
    for pid in "$DAEMON_PID" "$SECRETD_PID"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null
    done
    for _ in 1 2 3 4 5; do
        alive=0
        for pid in "$DAEMON_PID" "$SECRETD_PID"; do
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && alive=1
        done
        [ "$alive" = 0 ] && break
        sleep 0.2
    done
    for pid in "$DAEMON_PID" "$SECRETD_PID"; do
        [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null
    done
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
#
# The one legitimate skip in this file is the sandbox section further down: a
# confined session needs bubblewrap, and where bwrap is genuinely absent the
# boundary cannot be tested at all. That skip is loud, names bubblewrap, and is
# refused outright when RIME_REQUIRE_SANDBOX is set — which CI sets, so the job
# cannot go green having skipped it.
for tool in cargo git python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binaries"
cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
    --bin rime-agentd --bin rime-secretd --bin rime >/dev/null 2>&1 || {
    bad "rime-agentd, rime-secretd and rime build"
    printf '\nsecret-broker: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }
ok "rime-agentd, rime-secretd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
SECRETD="${BIN}/rime-secretd"
Rime="${BIN}/rime"

# ── an isolated runtime ──────────────────────────────────────────────────────
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

# THE sentinel. Distinctive enough that a grep for it cannot match by accident.
SENTINEL="rime-sentinel-7f3a91c4-do-not-leak"

section "the secret service"
# Its own socket and its own store, both under $WORK. The variable is what the
# CLI and rime-agentd both read, and rime-agentd is started afterwards so it
# inherits it.
export RIME_SECRETD_SOCKET="${WORK}/secretd.sock"
SECRET_STORE="${WORK}/secretd-store"
"$SECRETD" --socket "$RIME_SECRETD_SOCKET" --store "$SECRET_STORE" \
    > "${WORK}/secretd.log" 2>&1 &
SECRETD_PID=$!
for _ in $(seq 1 50); do [ -S "$RIME_SECRETD_SOCKET" ] && break; sleep 0.1; done
[ -S "$RIME_SECRETD_SOCKET" ] && ok "the secret service came up" || {
    bad "the secret service came up"
    sed 's/^/      /' "${WORK}/secretd.log"
    printf '\nsecret-broker: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }

# Every local account must be able to reach it; who they are is decided from
# SO_PEERCRED, not from a mode bit.
sockmode="$(stat -c '%a' "$RIME_SECRETD_SOCKET" 2>/dev/null)"
[ "$sockmode" = "666" ] && ok "the socket is reachable by any local account (is ${sockmode})" \
                        || bad "the socket is reachable by any local account (is ${sockmode})"

section "the agent runtime"
"$AGENTD" > "${WORK}/agentd.log" 2>&1 &
DAEMON_PID=$!
SOCK="${XDG_RUNTIME_DIR}/rime-agentd/control.sock"
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
[ -S "$SOCK" ] && ok "the daemon came up on an isolated socket" || {
    bad "the daemon came up on an isolated socket"
    sed 's/^/      /' "${WORK}/agentd.log"
    printf '\nsecret-broker: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }

# ── a project with an https remote that refuses instantly ────────────────────
PROJ="${WORK}/demo"
mkdir -p "$PROJ"
git -C "$PROJ" init -q
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
# 127.0.0.1:1 is closed on every machine, so git fails in milliseconds and the
# token is transmitted to nothing.
git -C "$PROJ" remote add origin "https://127.0.0.1:1/demo.git"
git -C "$PROJ" remote add elsewhere "https://example.invalid/other.git"
git -C "$PROJ" remote add viassh "git@127.0.0.1:demo.git"

# ── storing ──────────────────────────────────────────────────────────────────
section "storing a credential"
printf %s "$SENTINEL" | "$Rime" secret add demo --host 127.0.0.1 >/dev/null 2>&1
out="$("$Rime" secret list 2>&1)"
printf '%s' "$out" | pipe_has "demo" \
    && ok "the service is listed" || { bad "the service is listed"; printf '      %s\n' "$out"; }
printf '%s' "$out" | pipe_has "$SENTINEL" \
    && bad "\`list\` does not print the credential" || ok "\`list\` does not print the credential"

# The credential lives in the secret service's store, in a file of its own, and
# NOT beside the session records. That split is what lets the wire types refuse
# to serialise a value at all: nothing hands one to serde.
STORE="${SECRET_STORE}/users/$(id -u)/demo.secret"
META="${SECRET_STORE}/users/$(id -u)/demo.json"
[ -f "$STORE" ] && ok "the credential is in the service's store" \
                || bad "the credential is in the service's store"
grep -q "$SENTINEL" "$META" 2>/dev/null \
    && bad "the metadata record holds no credential" \
    || ok "the metadata record holds no credential"
grep -rq "$SENTINEL" "$XDG_STATE_HOME" 2>/dev/null \
    && bad "nothing under the agent runtime's state holds the credential" \
    || ok "nothing under the agent runtime's state holds the credential"

mode="$(stat -c '%a' "$STORE" 2>/dev/null)"
[ "$mode" = "600" ] && ok "the credential file is 0600 (is ${mode})" \
                    || bad "the credential file is 0600 (is ${mode})"
dirmode="$(stat -c '%a' "$(dirname "$STORE")" 2>/dev/null)"
[ "$dirmode" = "700" ] && ok "its directory is 0700 (is ${dirmode})" \
                       || bad "its directory is 0700 (is ${dirmode})"

printf '%s' "$("$Rime" secret list --json 2>/dev/null)" | pipe_has "$SENTINEL" \
    && bad "--json does not include the credential" || ok "--json does not include the credential"

# ── nothing is allowed by default ────────────────────────────────────────────
section "a stored credential grants nothing"
"$Rime" secret grants 2>&1 | pipe_has "nothing is granted" \
    && ok "storing a credential allows nothing" || bad "storing a credential allows nothing"

out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch origin 2>&1)"
printf '%s' "$out" | pipe_has "not granted" \
    && ok "an ungranted capability is refused" \
    || { bad "an ungranted capability is refused"; printf '      %s\n' "$out"; }
printf '%s' "$out" | pipe_has "$SENTINEL" \
    && bad "the refusal does not leak the credential" \
    || ok "the refusal does not leak the credential"

# ── the vocabulary is closed ─────────────────────────────────────────────────
section "the vocabulary is closed"
# `cloudflare.account.delete` is deliberately NOT a §13.2 name and never will
# be: §13.2 offers `cloudflare.account.read` and nothing else on an account.
# What this list needs is a well-formed id that no provider declares, and it
# used to hold `cloudflare.dns.delete` -- which stopped being one the day
# P1-008 implemented all 32 of §13.2's names. The case then rode the
# integration tip red, asserting that the vocabulary is closed against a name
# that had moved inside it. Taking the next id from the unimplemented end of
# §13.2 would only set the same trap for whoever implements it next; the same
# swap was made in the Rust half of this claim,
# `service.rs::the_vocabulary_is_closed_at_the_grant_and_at_the_use`.
for evil in exec sh git.clone curl run cloudflare.account.delete; do
    out="$(cd "$PROJ" && "$Rime" secret use demo "$evil" origin 2>&1)"
    printf '%s' "$out" | pipe_has -E "not an operation" \
        && ok "'$evil' is not an operation" \
        || { bad "'$evil' is not an operation"; printf '      %s\n' "$out"; }
done

# The other half of "closed", and the half the stale case was standing in for:
# a name that IS in the vocabulary must not be refused BY the vocabulary.
# `cloudflare.dns.delete` is a real operation now, so it gets past the id check
# and is stopped by the grant instead -- a different refusal, naming the grant
# that would allow it. Without this arm the loop above would still pass on a
# build that had lost every operation it offers, because then every id is
# "not an operation".
out="$(cd "$PROJ" && "$Rime" secret use demo cloudflare.dns.delete origin -o type=A 2>&1)"
if printf '%s' "$out" | pipe_has "not granted" \
   && ! printf '%s' "$out" | pipe_has "not an operation"; then
    ok "an id the vocabulary does hold is stopped by the grant, not by the vocabulary"
else
    bad "an id the vocabulary does hold is stopped by the grant, not by the vocabulary"
    printf '      %s\n' "$out"
fi

section "a resource may not be a URL"
# The hole this closes: with a URL accepted, a session asks the broker to push
# to a host it controls and the broker does it, with the token attached. The
# rule is the framework's now, not git's, so it holds for every provider.
for evil in "https://attacker.example/r" "git@github.com:a/b" "-f" "--force" "../x" "a b"; do
    out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch "$evil" 2>&1)"
    printf '%s' "$out" | pipe_has -E "not a resource" \
        && ok "refused as a resource: ${evil}" \
        || { bad "refused as a resource: ${evil}"; printf '      %s\n' "$out"; }
done

section "an option the operation does not declare is refused"
# What keeps "there is no command line here" true now that arguments are a map.
out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch origin -o branch=main 2>&1)"
printf '%s' "$out" | pipe_has "has no 'branch' option" \
    && ok "an undeclared option is refused rather than ignored" \
    || { bad "an undeclared option is refused rather than ignored"; printf '      %s\n' "$out"; }

# ── granting ─────────────────────────────────────────────────────────────────
section "granting"
out="$(cd "$PROJ" && "$Rime" secret grant demo git.fetch 2>&1)"
printf '%s' "$out" | pipe_has "allowed demo:git.fetch" \
    && ok "a capability can be granted for the project" \
    || { bad "a capability can be granted for the project"; printf '      %s\n' "$out"; }

# P0-002 shipped `git-fetch`; a machine that granted the old name must not be
# told the capability it granted is not granted.
out="$(cd "$PROJ" && "$Rime" secret grants 2>&1)"
printf '%s' "$out" | pipe_has "demo:git.fetch" \
    && ok "a grant is stored under the canonical operation id" \
    || { bad "a grant is stored under the canonical operation id"; printf '      %s\n' "$out"; }

out="$(cd "$PROJ" && "$Rime" secret grant nosuchservice git.fetch 2>&1)"
printf '%s' "$out" | pipe_has "no credential stored" \
    && ok "a grant for an unknown service is refused, not silently stored" \
    || bad "a grant for an unknown service is refused, not silently stored"

# A grant is per capability: git.fetch does not imply git.push.
out="$(cd "$PROJ" && "$Rime" secret use demo git.push origin 2>&1)"
printf '%s' "$out" | pipe_has "not granted" \
    && ok "granting git.fetch does not allow git.push" \
    || { bad "granting git.fetch does not allow git.push"; printf '      %s\n' "$out"; }

section "a remote must point where the credential is for"
out="$(cd "$PROJ" && "$Rime" secret use demo git-fetch elsewhere 2>&1)"  # old spelling, still accepted
printf '%s' "$out" | pipe_has "example.invalid" \
    && ok "a remote on another host is refused" \
    || { bad "a remote on another host is refused"; printf '      %s\n' "$out"; }
printf '%s' "$out" | pipe_has "$SENTINEL" \
    && bad "the host mismatch does not leak the credential" \
    || ok "the host mismatch does not leak the credential"

out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch viassh 2>&1)"
printf '%s' "$out" | pipe_has "not an http remote" \
    && ok "an ssh remote is refused with an explanation" \
    || { bad "an ssh remote is refused with an explanation"; printf '      %s\n' "$out"; }

out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch nosuchremote 2>&1)"
printf '%s' "$out" | pipe_has "no remote called" \
    && ok "an unconfigured remote is refused" || bad "an unconfigured remote is refused"

# ── THE assertion ────────────────────────────────────────────────────────────
section "the credential never reaches the caller"
# A granted capability, actually attempted. git will fail — 127.0.0.1:1 refuses
# — and that is fine: what is asserted is that the credential stayed on the
# daemon's side while the attempt was made.
out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch origin 2>"${WORK}/use.err")"
err="$(cat "${WORK}/use.err")"
printf '%s\n%s\n' "$out" "$err" | sed 's/^/      /' | head -8

printf '%s' "$out" | pipe_has "$SENTINEL" \
    && bad "the credential is not in stdout" || ok "the credential is not in stdout"
printf '%s' "$err" | pipe_has "$SENTINEL" \
    && bad "the credential is not in stderr" || ok "the credential is not in stderr"
printf '%s\n%s' "$out" "$err" | pipe_has -E "127\.0\.0\.1|Could not resolve|refused|unable to access" \
    && ok "the operation was genuinely attempted" \
    || bad "the operation was genuinely attempted (nothing suggests git ran)"

section "the audit trail records the use and not the credential"
# The trail lives with the store, not with the caller: in the image that
# directory is root-owned, so the audited party cannot rewrite the audit.
LOG="${SECRET_STORE}/audit.jsonl"
[ -s "$LOG" ] && ok "an audit trail was written" || bad "an audit trail was written"
if [ -s "$LOG" ]; then
    grep -q "$SENTINEL" "$LOG" \
        && bad "the audit trail does not contain the credential" \
        || ok "the audit trail does not contain the credential"
    grep -q '"operation":"git.fetch"' "$LOG" \
        && ok "the capability is recorded" || bad "the capability is recorded"
    grep -q '"operation":"git-fetch"' "$LOG" \
        && bad "the trail names one operation one way" \
        || ok "the trail names one operation one way"
    grep -q '"event":"refused"' "$LOG" \
        && ok "refusals are recorded too" || bad "refusals are recorded too"
    grep -q '"event":"stored"' "$LOG" \
        && ok "storing a credential is recorded too" \
        || bad "storing a credential is recorded too"
    python3 - "$LOG" <<'PYEOF' && ok "every line carries the record from section 11" || bad "every line carries the record from section 11"
import json, sys
want = {'audit_id', 'ms', 'event', 'uid', 'peer_pid', 'provider', 'operation',
        'detail', 'resource', 'project', 'agent_session', 'request_origin',
        'origin_source', 'approval_policy', 'constraints'}
for line in open(sys.argv[1]):
    if line.strip():
        o = json.loads(line)
        missing = want - set(o)
        assert not missing, (missing, o)
PYEOF
    "$Rime" secret audit 2>&1 | pipe_has "git fetch origin" \
        && ok "\`rime secret audit\` reads the trail back" \
        || bad "\`rime secret audit\` reads the trail back"
    "$Rime" secret audit 2>&1 | pipe_has "$SENTINEL" \
        && bad "\`rime secret audit\` does not print the credential" \
        || ok "\`rime secret audit\` does not print the credential"

    # P0-013's provenance has to survive the store moving. A trail that says
    # WHERE a request came from but not whether the daemon observed that or
    # something asked for it cannot answer the only question it is for.
    grep -q '"origin_source":"observed"' "$LOG" \
        && ok "the trail says how the origin was arrived at" \
        || { bad "the trail says how the origin was arrived at"
             grep -o '"origin_source":"[^"]*"' "$LOG" | sort -u | sed 's/^/      /'; }
    python3 - "$LOG" <<'PYEOF2' && ok "no line claims a local origin it did not observe" || bad "no line claims a local origin it did not observe"
import json, sys
for line in open(sys.argv[1]):
    if not line.strip():
        continue
    o = json.loads(line)
    if o['request_origin'] in ('local-terminal', 'rime-shell'):
        assert o['origin_source'] == 'observed', o
PYEOF2
fi

# ── from inside a confined session ───────────────────────────────────────────
section "a confined session cannot read the credential, and cannot grant itself"
# The dev binary lives in the rime-os checkout, which a `project` sandbox for a
# DIFFERENT project does not bind — so the session cannot reach it, which is the
# sandbox working correctly. In a real image `rime` is at /usr/bin/rime and is
# covered by the read-only root bind. Copying it into the project reproduces
# that reachability without weakening the policy under test.
cp "$Rime" "${PROJ}/rime"
SESSION_RIME="${PROJ}/rime"

# INSIDE the project, not in /tmp: a `project` sandbox replaces /tmp with a
# fresh tmpfs, so a script there is simply not visible and the session dies with
# "No such file or directory" — which looks exactly like a broker failure.
cat > "${PROJ}/inside.sh" <<EOF
#!/bin/sh
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR}"
export XDG_STATE_HOME="${XDG_STATE_HOME}"
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME}"
cd "${PROJ}" || exit 1
echo "--- can the session read the credential file directly? ---"
cat "${STORE}" 2>&1 | head -3
echo "--- can it reach the secret service directly? ---"
"${SESSION_RIME}" secret list 2>&1 | head -3
echo "--- can it grant itself a capability? ---"
"${SESSION_RIME}" secret grant demo git.push 2>&1
echo "--- can it use the granted one? ---"
"${SESSION_RIME}" secret use demo git.fetch origin 2>&1 | head -4
echo "--- which git does the session find first? ---"
command -v git
echo "--- does the shim pass anything else through? ---"
git --version 2>&1 | head -1
echo "--- does a plain git fetch reach the broker? ---"
RIME_GIT_SERVICE=demo git fetch origin 2>&1 | head -3
echo "DONE"
EOF
chmod +x "${PROJ}/inside.sh"

# `project` policy, so $HOME really is masked — that is the property under test.
# A confined session needs bubblewrap. Where it is genuinely absent this
# section is SKIPPED — loudly, and only for that reason — because the boundary
# cannot be tested without a sandbox and reporting "failed" would be a lie
# about what was checked. CI installs bwrap precisely so this does not skip
# there; a skip in CI is itself a signal that the install step was lost.
if [ ! -x /usr/bin/bwrap ]; then
    # RIME_REQUIRE_SANDBOX turns the one legitimate skip in this file into a
    # failure, and CI sets it. Without it the `engine` job could go green on a
    # runner where the bubblewrap install step was removed or silently failed,
    # having never run the assertion §4 exists for — the same "a skipped check
    # counts as success" shape the rest of this suite no longer has.
    if [ -n "${RIME_REQUIRE_SANDBOX:-}" ]; then
        bad "bubblewrap is present (RIME_REQUIRE_SANDBOX is set)"
        printf '      /usr/bin/bwrap is missing, so the sandbox boundary — the\n'
        printf '      one thing §4 claims — cannot be tested. Refusing to skip.\n'
        printf '\nsecret-broker: %d passed, %d failed\n' "$pass" "$fail"
        exit 1
    fi
    printf 'SKIP  a confined session needs bubblewrap, which is not installed\n'
    printf '      (the sandbox-boundary assertions below cannot run here)\n'
    sid=""
    SKIPPED_SANDBOX=1
else
    SKIPPED_SANDBOX=0
    sid="$("$Rime" agent run --agent generic --sandbox project --cwd "$PROJ" -d \
            -- /bin/sh "${PROJ}/inside.sh" 2>"${WORK}/run.err" \
            | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
fi

if [ "$SKIPPED_SANDBOX" = 1 ]; then
    :
elif [ -z "$sid" ]; then
    # bwrap IS present and the session still did not start. That is a real
    # failure, not an environment limitation.
    bad "a confined session started"
    sed 's/^/      /' "${WORK}/run.err"
else
    ok "a confined session started (id ${sid})"
    # ── waiting for the script, and knowing WHY the wait ended ───────────────
    #
    # The old wait was `for _ in $(seq 1 100); sleep 0.25` and nothing else: it
    # could not tell a session that had already DIED from one that was merely
    # slow, so it spent the full 25 s either way and then reported a cause it
    # had not measured. That misreport is on the record twice — runs
    # 35616795800 and 35625128495 both printed "the sandbox did not come up"
    # when the same job's own bwrap probe had passed
    # (kernel.apparmor_restrict_unprivileged_userns = 0, "bubblewrap works: a
    # confined session can be built"), the session had started, and the FIRST
    # line of inside.sh had reached the transcript. Whatever went wrong, the
    # sandbox coming up was not it.
    #
    # Those two runs are the only reds in eight, on a byte-identical
    # tests/test-secret-broker.sh and a byte-identical rimed/ tree, and both
    # stopped at exactly one transcript line. The runtime writes transcripts
    # through an unbuffered File (`Session::write_log`), so that one line is
    # all the session ever produced. The one state that fits — and that the old
    # loop had no way to see — is a session that went away after its first
    # command.
    #
    # So the wait now ends on one of three things and says which:
    #   DONE        the script finished
    #   left        the session is gone; stop waiting AT ONCE and report its
    #               exit status, which is the answer the old loop threw away
    #   deadline    still alive, still producing nothing
    #
    # The ceiling is 90 s rather than 25 s, and that is only defensible because
    # "gone" no longer waits at all: a dead session fails in well under a
    # second, and the longer ceiling is spent only on a session that is alive.
    sb_started="$(date +%s)"
    sb_deadline="$((sb_started + 90))"
    sb_why="deadline: still alive and still producing nothing"
    while :; do
        logs="$("$Rime" agent logs "$sid" 2>/dev/null)"
        case "$logs" in *DONE*) sb_why="done"; break ;; esac
        sb_outcome="$("$Rime" agent status "$sid" 2>/dev/null | sed -n 's/^outcome  *//p')"
        if [ -n "$sb_outcome" ]; then
            sb_why="the session left before printing DONE (${sb_outcome})"
            # One more read: the transcript may have gained its last bytes
            # between the read above and the process being reaped.
            logs="$("$Rime" agent logs "$sid" 2>/dev/null)"
            break
        fi
        [ "$(date +%s)" -ge "$sb_deadline" ] && break
        sleep 0.25
    done
    sb_waited="$(( $(date +%s) - sb_started ))"
    printf '%s\n' "$logs" | sed 's/^/      | /' | head -40

    # THE SCRIPT MUST HAVE RUN. Without this gate every assertion below passes
    # vacuously when the sandbox fails to build — which is exactly what
    # happened on a runner where bubblewrap installed but could not create a
    # user namespace ("setting up uid map: Permission denied"). The session
    # record existed, so "started" passed; the script never executed, so
    # "the credential file is unreachable" passed because nothing tried to
    # read it. A test that passes for the wrong reason is worse than one that
    # fails.
    case "$logs" in *DONE*) sb_ran=1 ;; *) sb_ran=0 ;; esac
    if [ "$sb_ran" = 0 ]; then
        bad "the session's script actually ran"
        # Everything a reader needs to name the cause without another run,
        # because this fails on a machine nobody can log in to. The old text
        # asserted a cause instead; this states observations.
        printf '      nothing below was tested. the wait ended because: %s\n' "$sb_why"
        printf '      waited %ss; the transcript is %s bytes and stops at:\n' \
            "$sb_waited" "$(printf '%s' "$logs" | wc -c)"
        printf '%s\n' "$logs" | tail -3 | sed 's/^/        | /'
        printf '      rime agent status %s:\n' "$sid"
        "$Rime" agent status "$sid" 2>&1 | sed 's/^/        /'
        printf '      stderr of `rime agent run`:\n'
        sed 's/^/        /' "${WORK}/run.err"
        printf '      the agent runtime'"'"'s own log, last 20 lines:\n'
        tail -20 "${WORK}/agentd.log" 2>/dev/null | sed 's/^/        /'
        # The userns hint is printed only when it is TRUE. Printing it
        # unconditionally is how two runs came to blame a sysctl that the same
        # job had already proved was clear.
        if grep -qi 'uid map\|user namespace' "${WORK}/run.err" 2>/dev/null; then
            printf '      ("uid map: Permission denied" above means unprivileged user\n'
            printf '       namespaces are blocked — see the CI sysctl)\n'
        fi
    else
        ok "the session's script actually ran"

        printf '%s' "$logs" | pipe_has "$SENTINEL" \
            && bad "the session's transcript does not contain the credential" \
            || ok "the session's transcript does not contain the credential"
        printf '%s' "$logs" | pipe_has -E "No such file|Permission denied|cannot open" \
            && ok "the credential file is unreachable from inside the sandbox" \
            || bad "the credential file is unreachable from inside the sandbox"
        # Two locks on the same door, and both are asserted because either one
        # alone is a line away from being removed. The sandbox masks /run and
        # binds back only the agent runtime's own socket, so a confined session
        # cannot open the secret service at all; and the service refuses a
        # mutating verb from any caller inside a session, which is what covers
        # an UNCONFINED one.
        printf '%s' "$logs" | pipe_has -E "secret service is not running|cannot reach the secret service" \
            && ok "the secret service is unreachable from inside the sandbox" \
            || bad "the secret service is unreachable from inside the sandbox"
        printf '%s' "$logs" | pipe_has -E "cannot change its own capabilities|agent session cannot|secret service is not running|cannot reach the secret service" \
            && ok "the session cannot grant itself a capability" \
            || bad "the session cannot grant itself a capability"

        # And the grant it attempted was not recorded.
        "$Rime" secret grants 2>/dev/null | pipe_has "git.push" \
            && bad "the session's self-grant was not recorded" \
            || ok "the session's self-grant was not recorded"

        # ── §12: a skill's own `git` reaches the broker ──────────────────
        #
        # The point of the shim is that nothing had to be rewritten. `git
        # fetch` is what a skill types; what it must produce is the broker's
        # answer and not git's own "could not read Username".
        printf '%s' "$logs" | pipe_has 'bin/git' \
            && ok "the session's git is the shim, not /usr/bin/git" \
            || bad "the session's git is the shim, not /usr/bin/git"
        printf '%s' "$logs" | pipe_has 'git version' \
            && ok "and the shim passes everything else through to the real git" \
            || bad "and the shim passes everything else through to the real git"
        printf '%s' "$logs" | pipe_has 'against https://127.0.0.1' \
            && ok "a plain git fetch was performed by the broker" \
            || bad "a plain git fetch was performed by the broker"
        printf '%s' "$logs" | pipe_has -i 'could not read Username\|terminal prompts disabled' \
            && bad "git never asked the session for a credential" \
            || ok "git never asked the session for a credential"
    fi
fi

# ── revoke ───────────────────────────────────────────────────────────────────
section "revoking"
out="$(cd "$PROJ" && "$Rime" secret revoke demo git.fetch 2>&1)"
printf '%s' "$out" | pipe_has "withdrew" \
    && ok "a capability can be withdrawn" || bad "a capability can be withdrawn"
out="$(cd "$PROJ" && "$Rime" secret use demo git.fetch origin 2>&1)"
printf '%s' "$out" | pipe_has "not granted" \
    && ok "a withdrawn capability is refused again" || bad "a withdrawn capability is refused again"

section "removing"
"$Rime" secret remove demo >/dev/null 2>&1
[ ! -f "$STORE" ] && ok "removing deletes the stored credential" \
                  || bad "removing deletes the stored credential"

# Nothing anywhere under either daemon's state may still hold the sentinel. The
# audit trail is deliberately in scope: it outlives the credential, and if the
# credential were in it, deleting the credential would not have deleted it.
if grep -rq "$SENTINEL" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null; then
    printf '      still present in: %s\n' "$(grep -rl "$SENTINEL" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null | tr '\n' ' ')"
    bad "no trace of the credential remains on disk"
else
    ok "no trace of the credential remains on disk"
fi

# ── an online account's two credentials ──────────────────────────────────────
#
# `rime account rm` has to delete BOTH halves of an OAuth account, and the
# second one cannot be reached any other way: a refresh credential is filed as
# `account.<provider>.<name>.refresh`, `.` is illegal in an account name, so it
# does not parse as an account — `rime account list` never shows it and
# `rime account rm` refuses to be pointed at it directly. Deriving the name
# from the account is the only way in.
#
# This is an end-to-end test rather than a unit one on purpose. The unit test
# beside `names_to_remove` pins the derivation; nothing in the Rust suite can
# reach the removal itself, because removing needs a daemon and a store. Here
# there is one of each. The two credentials are stored with `rime secret add`
# rather than by signing in, because signing in needs Google.
section "an online account's two credentials"
ACCESS_SENTINEL="rime-access-sentinel-91b4c2ef-do-not-leak"
REFRESH_SENTINEL="rime-refresh-sentinel-2d7e60aa-do-not-leak"
printf %s "$ACCESS_SENTINEL" | "$Rime" secret add account.google.work \
    --host www.googleapis.com --auth bearer >/dev/null 2>&1
# The refresh half as `rime account add --client-id` files it: a separate name,
# the AUTHORISATION host rather than the API host, and the OAuth client in the
# non-secret username field where the daemon's refresher reads it.
printf %s "$REFRESH_SENTINEL" | "$Rime" secret add account.google.work.refresh \
    --host oauth2.googleapis.com --auth bearer --username my-client >/dev/null 2>&1

out="$("$Rime" account list 2>&1)"
printf '%s' "$out" | pipe_has 'google.work' \
    && ok "the account is listed" || bad "the account is listed"
printf '%s' "$out" | pipe_has 'refresh' \
    && bad "the refresh credential is not listed as an account" \
    || ok "the refresh credential is not listed as an account"

# Pointed at directly it is refused, which is what makes the derivation the
# only way in and this test worth having.
"$Rime" account rm google.work.refresh >/dev/null 2>&1 \
    && bad "the refresh credential cannot be removed by naming it" \
    || ok "the refresh credential cannot be removed by naming it"

"$Rime" account rm google.work >/dev/null 2>&1
out="$("$Rime" secret list 2>&1)"
printf '%s' "$out" | pipe_has 'account.google.work.refresh' \
    && bad "removing the account removed its refresh token too" \
    || ok "removing the account removed its refresh token too"
# Anchored so it cannot match the refresh line. Unanchored it did, and the
# access assertion then failed for the refresh token's absence rather than its
# own — a test that reports the right verdict off the wrong evidence.
printf '%s' "$out" | grep -Eq 'account\.google\.work([^.]|$)' \
    && bad "removing the account removed its access token" \
    || ok "removing the account removed its access token"

# And neither value survives anywhere the daemons write. A refresh token left
# behind is worse than an access token left behind: it mints new ones.
if grep -rq "$REFRESH_SENTINEL" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null; then
    printf '      still present in: %s\n' "$(grep -rl "$REFRESH_SENTINEL" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null | tr '\n' ' ')"
    bad "no trace of the refresh token remains on disk"
else
    ok "no trace of the refresh token remains on disk"
fi
if grep -rq "$ACCESS_SENTINEL" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null; then
    bad "no trace of the access token remains on disk"
else
    ok "no trace of the access token remains on disk"
fi

# The commoner case, and the one the block above could have broken: an account
# with NO refresh credential. An app password has none, and neither has an
# OAuth grant the server returned without one — so `rm` must treat "there was
# no refresh credential" as a normal outcome and not as the failure it reports
# loudly. Untested, this depends on the daemon answering a `Remove` of a
# missing service with exactly `NoSuchService`; anything else and EVERY
# non-OAuth removal would exit non-zero claiming a credential is still stored
# that never existed.
PLAIN_SENTINEL="rime-plain-sentinel-5c8d13bb-do-not-leak"
printf %s "$PLAIN_SENTINEL" | "$Rime" secret add account.nextcloud.home \
    --host cloud.example --auth raw --username me >/dev/null 2>&1
out="$("$Rime" account rm nextcloud.home 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "removing an account that has no refresh token succeeds" \
                || bad "removing an account that has no refresh token succeeds (rc=${rc})"
printf '%s' "$out" | pipe_has 'still stored' \
    && bad "and says nothing about a refresh token that never existed" \
    || ok "and says nothing about a refresh token that never existed"
printf '%s' "$("$Rime" secret list 2>&1)" | pipe_has 'account.nextcloud.home' \
    && bad "and the credential is gone" || ok "and the credential is gone"

# ── a Google account's one grantable scope, through the real daemon ──────────
#
# P2-017 round 30. Two halves of the same claim, and the second is the one no
# Rust test can make.
#
# The FIRST half is that the vapour-scope refusal is gone in the only honest
# direction. `rime account grant google.<name> files.read` was accepted before
# round 3 and recorded a grant for `gdrive.file.read`, an operation nothing
# offered; round 3 emptied the table so it was refused; round 30 built the
# provider, so it is accepted again — and this time the grant that lands is one
# the daemon can perform. Measured through the real CLI and the real daemon
# socket, which is what the Rust tests cannot do: they call
# `Service::use_capability` in process.
#
# The SECOND half is that the shipped daemon carries NO test injection. The
# gdrive provider refuses any credential not pinned to Google's own API host,
# and the loopback route a unit test uses is `GdriveProvider::at`, which is
# `#[cfg(test)]`. A credential stored for 127.0.0.1 and used here goes through
# the binary this suite built — if that binary had the injection, the refusal
# below would not happen.
#
# NO NETWORK. The googleapis-pinned credential is granted and never USED; the
# credential that is used is pinned to loopback and is refused at `bind`,
# before a request is composed. Nothing is dialled either way.
#
# `grep <<<` rather than `printf | grep -q`: under `pipefail` a `grep -q` that
# matches can return 141 when the writer is killed by SIGPIPE, which has
# mis-seeded suites in this repository before. A herestring is not a pipeline.
section "a Google account's grantable scope"
DRIVE_SENTINEL="rime-drive-sentinel-7f3a19dc-do-not-leak"
printf %s "$DRIVE_SENTINEL" | "$Rime" secret add account.google.drive \
    --host www.googleapis.com --auth bearer --path /drive/v3 >/dev/null 2>&1

out="$("$Rime" account scopes google 2>&1)"
grep -q 'files.read' <<<"$out" && grep -q 'gdrive.file.read' <<<"$out" \
    && ok "rime account scopes google lists the scope and the operation it names" \
    || bad "rime account scopes google lists the scope and the operation it names"
# WEAKENED, DELIBERATELY, AND SAID OUT LOUD. Until round 31 this negative had a
# witness: `rime account scopes microsoft` printed exactly this string, asserted
# twenty lines below, so a build that had stopped printing it anywhere would
# have been caught. `msgraph` landed and no provider says it any more, so this
# assertion now passes on any build and the POSITIVE one above it carries the
# whole claim. Kept rather than deleted because the string is still what an
# empty table prints and a regression would put it back — but it is no longer
# evidence on its own, and the routing control that replaced it is in the
# Microsoft section below.
grep -q 'no grantable scopes' <<<"$out" \
    && bad "and no longer says a Google account has nothing grantable" \
    || ok "and no longer says a Google account has nothing grantable"

out="$(cd "$PROJ" && "$Rime" account grant google.drive files.read 2>&1)"; rc=$?
[ "$rc" -eq 0 ] \
    && ok "rime account grant google.drive files.read is accepted" \
    || bad "rime account grant google.drive files.read is accepted (rc=${rc}): ${out}"

# The grant is stored CANONICALLY. A scope recorded under its own name would
# be a grant the daemon never matches — the defect the scope table's routing
# rule exists to prevent, here as an end-to-end fact rather than a unit one.
out="$(cd "$PROJ" && "$Rime" secret grants 2>&1)"
grep -q 'gdrive.file.read' <<<"$out" \
    && ok "and the grant is recorded under the canonical operation id" \
    || bad "and the grant is recorded under the canonical operation id: ${out}"

# Microsoft is the control, and after round 31 it is a BETTER one than it was.
# It used to be "the same command, refused, because no msgraph transport
# exists" — a control that stopped existing the moment the transport landed.
# What replaces it is a control the landing cannot take away: the two
# device-code providers offer the SAME scope name and it must route into
# DIFFERENT operations. `google files.read` -> `gdrive.file.read` above,
# `microsoft files.read` -> `msgraph.file.read` here. A build that recorded
# grants under the scope's own name, or that routed both into one transport —
# which is exactly what a copied table entry does — fails one of the two.
section "a Microsoft account's grantable scope"
out="$("$Rime" account scopes microsoft 2>&1)"
grep -q 'files.read' <<<"$out" && grep -q 'msgraph.file.read' <<<"$out" \
    && ok "rime account scopes microsoft lists the scope and the operation it names" \
    || bad "rime account scopes microsoft lists the scope and the operation it names: ${out}"
grep -q 'gdrive' <<<"$out" \
    && bad "and does not list Google's transport under Microsoft" \
    || ok "and does not list Google's transport under Microsoft"
grep -q 'no grantable scopes' <<<"$out" \
    && bad "and no longer says a Microsoft account has nothing grantable" \
    || ok "and no longer says a Microsoft account has nothing grantable"

# An unknown scope is still refused, and with the OTHER message — the one that
# sends the reader to the list rather than away from it. Without this, the
# assertions above would pass on a build that accepted every scope name it was
# handed.
GRAPH_SENTINEL="rime-graph-sentinel-2b6e4401-do-not-leak"
printf %s "$GRAPH_SENTINEL" | "$Rime" secret add account.microsoft.work \
    --host graph.microsoft.com --auth bearer --path /v1.0/me >/dev/null 2>&1

out="$(cd "$PROJ" && "$Rime" account grant microsoft.work nosuchscope 2>&1)"; rc=$?
[ "$rc" -ne 0 ] \
    && ok "rime account grant microsoft.work nosuchscope is refused" \
    || bad "rime account grant microsoft.work nosuchscope is refused (rc=${rc}): ${out}"
grep -q 'no scope' <<<"$out" \
    && ok "and the refusal is 'no such scope', not 'nothing to grant'" \
    || bad "and the refusal is 'no such scope', not 'nothing to grant': ${out}"

out="$(cd "$PROJ" && "$Rime" account grant microsoft.work files.read 2>&1)"; rc=$?
[ "$rc" -eq 0 ] \
    && ok "rime account grant microsoft.work files.read is accepted" \
    || bad "rime account grant microsoft.work files.read is accepted (rc=${rc}): ${out}"

# Canonical, and into msgraph rather than gdrive. This is the routing control.
out="$(cd "$PROJ" && "$Rime" secret grants 2>&1)"
grep -q 'msgraph.file.read' <<<"$out" \
    && ok "and the Microsoft grant is recorded as msgraph.file.read" \
    || bad "and the Microsoft grant is recorded as msgraph.file.read: ${out}"

# The shipped binary has no loopback route into the Graph transport either.
# `MsgraphProvider::at` is `#[cfg(test)]`; if it were not, this would not be
# refused. NO NETWORK: the credential that is used is pinned to loopback and
# dies at `bind`, and the graph.microsoft.com one above is granted and never
# used.
printf %s "$GRAPH_SENTINEL" | "$Rime" secret add account.microsoft.local \
    --host 127.0.0.1 --scheme http --port 9 --auth bearer --path /v1.0/me \
    >/dev/null 2>&1
(cd "$PROJ" && "$Rime" secret grant account.microsoft.local msgraph.file.read >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" secret use account.microsoft.local msgraph.file.read \
    01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K 2>&1)"
grep -q '127.0.0.1' <<<"$out" && grep -q 'graph.microsoft.com' <<<"$out" \
    && ok "the shipped daemon refuses a Graph credential pinned off Microsoft" \
    || bad "the shipped daemon refuses a Graph credential pinned off Microsoft: ${out}"
grep -q "$GRAPH_SENTINEL" <<<"$out" \
    && bad "and that refusal carries no credential" \
    || ok "and that refusal carries no credential"

# NOT ASSERTED HERE, AND THE REASON IS THE SAME ONE gdrive's FOUND ENTRY GIVES.
# A consumer OneDrive id is {driveId}!{n} — Microsoft's own documented example
# is 12319191!11919 — and `valid_name` refuses `!`. That refusal CANNOT be
# measured through the shipped binary without dialling Microsoft: on the
# loopback credential above the host pin fires first, so `secret use` exits
# non-zero for a reason that has nothing to do with the id, and an assertion on
# the exit code alone would pass on a build that had dropped the check entirely
# — which is exactly what it did when it was tried, so it was taken out rather
# than left reading green. The only credential whose host pin lets the id check
# run is one pinned to graph.microsoft.com, and a build without the check would
# then send a request to Microsoft. It is measured in Rust instead, twice:
# `a_resource_is_a_onedrive_item_id_and_nothing_that_could_leave_the_endpoint`
# through `OperationSpec::check`, and `the_url_is_the_stored_endpoint_with_the_
# item_id_under_it` through `graph_url` itself.

"$Rime" account rm microsoft.work >/dev/null 2>&1
"$Rime" secret remove account.microsoft.local >/dev/null 2>&1

# The shipped binary has no loopback route into the Drive transport.
printf %s "$DRIVE_SENTINEL" | "$Rime" secret add account.google.local \
    --host 127.0.0.1 --scheme http --port 9 --auth bearer --path /drive/v3 \
    >/dev/null 2>&1
(cd "$PROJ" && "$Rime" secret grant account.google.local gdrive.file.read >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" secret use account.google.local gdrive.file.read \
    1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms 2>&1)"
grep -q '127.0.0.1' <<<"$out" && grep -q 'www.googleapis.com' <<<"$out" \
    && ok "the shipped daemon refuses a Drive credential pinned off Google" \
    || bad "the shipped daemon refuses a Drive credential pinned off Google: ${out}"
grep -q "$DRIVE_SENTINEL" <<<"$out" \
    && bad "and the refusal carries no credential" \
    || ok "and the refusal carries no credential"

"$Rime" account rm google.drive >/dev/null 2>&1
"$Rime" secret remove account.google.local >/dev/null 2>&1
# Both device-code tokens, each with its own sentinel: a single one shared
# between them could be removed by either `rm` and read as both being gone.
for pair in "Drive:$DRIVE_SENTINEL" "Graph:$GRAPH_SENTINEL"; do
    what="${pair%%:*}"; needle="${pair#*:}"
    if grep -rq "$needle" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null; then
        printf '      still present in: %s\n' "$(grep -rl "$needle" "$SECRET_STORE" "$XDG_STATE_HOME" 2>/dev/null | tr '\n' ' ')"
        bad "no trace of the ${what} token remains on disk"
    else
        ok "no trace of the ${what} token remains on disk"
    fi
done

printf '\nsecret-broker: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
