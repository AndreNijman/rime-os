#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for `rime agent run --disposable` (P1-037): an agent
#  session that runs inside a disposable capsule and discards its state.
#
#  ── What is real here and what is faked, exactly ────────────────────────────
#  The REAL disposable engine runs — `files/system/libexec/rime-disposable`
#  from this checkout, reached through RIME_DISPOSABLE_ENGINE. Real name
#  validation, real copy-in, real `trap`-driven teardown, real copy-out
#  boundary. The REAL daemon spawns it on a real PTY.
#
#  Only the CAPSULE ENGINE is faked, through RIME_DISPOSABLE_ENV_ENGINE, which
#  the disposable engine already documents as overridable. The fake:
#
#    * LOGS every invocation, and that is the point rather than a convenience.
#      A fake that only exited 0 would make every assertion below pass without
#      the agent going anywhere near a capsule.
#    * for `exec`, sets HOME to the environment's throwaway home and runs the
#      command on the host — which is what a capsule does to HOME, and it is
#      what makes the daemon's `cd -- "$HOME/in/$1"` script genuinely
#      exercised instead of merely built.
#
#  So no podman, no distrobox, no image pull, and nothing here can fail in CI
#  for a reason unrelated to this code.
#
#  ── The claims ──────────────────────────────────────────────────────────────
#      1. the agent runs in the capsule, in the COPY of the worktree — not in
#         the host worktree, and not through /run/host;
#      2. the host worktree is byte-identical afterwards. The agent writes
#         into its own tree and the host never sees it;
#      3. the environment is GONE when the session ends, and nothing left it;
#      4. unless --copy-out named somewhere, in which case exactly that
#         arrives;
#      5. a confining sandbox and --disposable are REFUSED together, before
#         any environment is created;
#      6. a hostile prompt cannot escape into the shell script that changes
#         directory — it is a positional parameter, never interpolated;
#      7. --worktree and --checkpoint are REFUSED with --disposable, by the
#         daemon, before a host worktree or branch is created.
#
#  ── what this suite CANNOT prove, and does not claim to ─────────────────────
#  The fake capsule engine runs the command on the HOST. So an absolute host
#  path is still resolvable from "inside", which means the broken-checkout half
#  of the --worktree refusal (a linked worktree's .git is a pointer to a host
#  path a real capsule cannot reach) is NOT demonstrated here — only the
#  refusal is. Nor is anything about a real container: no namespaces, no
#  /run/host, no image. What is real is Rime's own chain — the daemon, the
#  engine, the argv, the copy boundary and the teardown.
#
#  NOTHING HERE TOUCHES A RUNNING DAEMON, and nothing touches the user's own
#  disposable environments: its own XDG_RUNTIME_DIR, XDG_STATE_HOME,
#  XDG_CONFIG_HOME, RIME_AGENT_SCRATCH_ROOT and RIME_DISPOSABLE_ROOT, and the
#  daemon is killed by the pid this script started — never by name.
#
#      ./tests/test-agent-disposable.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Before the temp tree, the daemon or the build: this suite needs an
# environment the daemon will observe as LOCAL, and that has to be arranged
# from outside the suite. `rime-agentd` places a peer from its cgroup, and a
# process started by systemd — a CI job, a timer-dispatched agent — is in
# neither a login session nor a user service, so §7 refuses it before the
# behaviour under test is reached. Here it stops the suite starting the
# session the capsule is built around.
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
give_up() { printf '\ndisposable: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }

DAEMON_PID=""
# The daemon-death section starts daemons of its own and kills them on purpose.
# Their pids are collected here so that a case which fails half way through
# still cannot leave a daemon, an engine or an agent behind.
EXTRA_PIDS=""
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    [ -n "$DAEMON_PID" ] && { for _ in 1 2 3 4 5; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done; }
    [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
    for p in $EXTRA_PIDS; do kill -9 "$p" 2>/dev/null; done
    # Anything still naming this suite's own scratch directory. Matched on the
    # fixture path and never on a program name: a live rime-agentd with other
    # people's sessions on it must not be reachable from here.
    for p in $(pgrep -f "$WORK" 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
    rm -rf "$WORK"
}
trap cleanup EXIT

for tool in cargo git python3 cmp; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

# Running as root would be tested against a refusal, not against the feature:
# the engine refuses to make an environment as root, on purpose.
if [ "$(id -u)" = 0 ]; then
    echo "FATAL: this suite must not run as root — the engine refuses a root environment" >&2
    exit 2
fi

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    give_up
fi
ok "rime-agentd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

ENGINE="${ROOT}/files/system/libexec/rime-disposable"
if [ -x "$ENGINE" ]; then
    ok "the real disposable engine is in this checkout and executable"
else
    bad "the real disposable engine is in this checkout and executable"
    echo "      expected ${ENGINE}" >&2
    give_up
fi

# ── an isolated runtime ──────────────────────────────────────────────────────
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
export RIME_AGENT_SCRATCH_ROOT="${WORK}/scratch"
export RIME_DISPOSABLE_ENGINE="$ENGINE"
export RIME_DISPOSABLE_ROOT="${WORK}/disp"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME" "$RIME_DISPOSABLE_ROOT"
chmod 0700 "$XDG_RUNTIME_DIR"

PRE_SCRATCH="$(ls /tmp/rime-agent 2>/dev/null | sort | tr '\n' ' ')"
REAL_DISP="${HOME}/.local/state/rime/disposable"
PRE_REAL="$(ls "$REAL_DISP" 2>/dev/null | sort | tr '\n' ' ')"

# ── the fake capsule engine ──────────────────────────────────────────────────
CAPLOG="${WORK}/capsule.log"
: > "$CAPLOG"
FAKE="${WORK}/fake-rime-env"
cat > "$FAKE" <<'FAKE_EOF'
#!/usr/bin/env bash
# A stand-in for /usr/libexec/rime-env. It records what it was asked and, for
# `exec`, runs the command with HOME pointed at the environment's throwaway
# home — which is what a real capsule does to HOME, and what makes the
# daemon's `cd -- "$HOME/in/$1"` script really run.
set -uo pipefail
log() { printf '%s\n' "$*" >> "$CAPSULE_LOG"; }
verb="${1:-}"; shift 2>/dev/null || true
case "$verb" in
    create)
        log "create $*"
        exit 0
        ;;
    exec)
        name="${1:-}"; shift
        [ "${1:-}" = -- ] && shift
        log "exec ${name} -- $*"
        home="${RIME_DISPOSABLE_ROOT}/${name}/home"
        HOME="$home" exec "$@"
        ;;
    enter)
        log "enter ${1:-}"
        exit 0
        ;;
    rm)
        log "rm ${1:-}"
        exit 0
        ;;
    *)
        log "unexpected ${verb} $*"
        exit 1
        ;;
esac
FAKE_EOF
chmod +x "$FAKE"
export RIME_DISPOSABLE_ENV_ENGINE="$FAKE"
export CAPSULE_LOG="$CAPLOG"

# ── the fixture project ──────────────────────────────────────────────────────
git_q() { git -c advice.detachedHead=false -c init.defaultBranch=main "$@"; }
PROJ="${WORK}/proj"
mkdir -p "$PROJ"
git_q init -q "$PROJ" >/dev/null 2>&1
git_q -C "$PROJ" config user.email t@example.invalid
git_q -C "$PROJ" config user.name t
printf 'the original\n' > "${PROJ}/tracked.txt"
git_q -C "$PROJ" add tracked.txt
git_q -C "$PROJ" commit -qm "base"

# ── the daemon ───────────────────────────────────────────────────────────────
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
    give_up
fi

# ── the agent script ─────────────────────────────────────────────────────────
#
# It reports on itself to a path OUTSIDE the environment, because everything
# inside is about to be deleted — which is the feature.
AGENT_SH="${WORK}/agent.sh"
cat > "$AGENT_SH" <<'AGENT_EOF'
#!/usr/bin/env bash
# $1 is where to report, and it is an ARGUMENT rather than an environment
# variable on purpose: this process is spawned by the DAEMON, whose environment
# was fixed before the suite chose this path. An earlier version of this suite
# passed it as OBSERVE= on the `rime` command line and the agent wrote to the
# empty string.
observe="$1"; shift
{
    printf 'pwd=%s\n' "$PWD"
    printf 'home=%s\n' "$HOME"
    # Read BEFORE editing anything, so this is the proof that the copy-in
    # really happened — asserted from INSIDE, while the environment still
    # exists. Asserting it from outside afterwards cannot work: teardown has
    # deleted the directory by then, which made the old check pass whether or
    # not a single file had been copied.
    printf 'copied=%s\n' "$(cat ./tracked.txt 2>&1)"
    printf 'gitdir=%s\n' "$(test -d ./.git && echo directory || echo "$(cat ./.git 2>/dev/null)")"
    printf 'argc=%s\n' "$#"
    printf 'argv=%s\n' "$(printf '[%s]' "$@")"
} > "$observe"
# Write into the tree the agent was given. If that tree is the host's, the
# host will see this file and the suite will say so.
printf 'the agent was here\n' > ./agent-wrote-this.txt
printf 'the original, edited by the agent\n' > ./tracked.txt
# ...and one thing offered for copy-out.
mkdir -p "${HOME}/out"
printf 'the result\n' > "${HOME}/out/result.txt"
AGENT_EOF
chmod +x "$AGENT_SH"

no_create_appears() {   # no_create_appears <count before>
    # Waits for a `create` to show up and reports FAILURE if one does. A
    # refusal that really happened first has nothing to wait for; without the
    # wait, `-d` returning early made "nothing was created" true whoever won.
    local base="$1" _
    for _ in $(seq 1 30); do
        [ "$(grep -c '^create ' "$CAPLOG")" != "$base" ] && return 1
        sleep 0.1
    done
    return 0
}
session_count() {
    "$Rime" agent list --all 2>/dev/null | grep -cE '^[[:space:]]*[0-9]+[[:space:]]'
}

wait_gone() {   # wait_gone <path>
    for _ in $(seq 1 200); do [ -e "$1" ] || return 0; sleep 0.1; done
    return 1
}
wait_file() {   # wait_file <path>
    for _ in $(seq 1 200); do [ -s "$1" ] && return 0; sleep 0.1; done
    return 1
}

run_disposable() {   # run_disposable <observe file> [extra args...]
    local observe="$1"; shift
    "$Rime" agent run --agent generic --sandbox unrestricted \
        --disposable --cwd "$PROJ" -d "$@" \
        -- /bin/bash "$AGENT_SH" "$observe" hello 'a b' '$(id -un)' ';touch pwned' \
        2>"${WORK}/run.err" \
        | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1
}

# ── the run ──────────────────────────────────────────────────────────────────
section "an agent session inside a disposable capsule"

BEFORE_STAGE="${WORK}/before.stage"
BEFORE_PORCELAIN="${WORK}/before.porcelain"
git_q -C "$PROJ" status --porcelain >/dev/null 2>&1
git_q -C "$PROJ" ls-files --stage > "$BEFORE_STAGE"
git_q -C "$PROJ" status --porcelain > "$BEFORE_PORCELAIN"

OBS1="${WORK}/observed1"
SID="$(run_disposable "$OBS1")"
if [ -n "$SID" ]; then
    ok "the session started (id ${SID})"
else
    bad "the session started"
    sed 's/^/      /' "${WORK}/run.err" >&2
    sed 's/^/      /' "${WORK}/agentd.log" >&2
    give_up
fi
CAPSULE="disp-agent${SID}"

if wait_file "$OBS1"; then
    ok "the agent ran and reported on itself"
else
    bad "the agent ran and reported on itself"
    sed 's/^/      /' "$CAPLOG" >&2
    sed 's/^/      /' "${WORK}/agentd.log" >&2
    give_up
fi

# 1. it went through the capsule engine, into the capsule named for the session
if grep -q "^exec ${CAPSULE} -- " "$CAPLOG"; then
    ok "the agent command was executed inside ${CAPSULE}"
else
    bad "the agent command was executed inside ${CAPSULE}"
    sed 's/^/      | /' "$CAPLOG" >&2
fi
if grep -q "^create ${CAPSULE} " "$CAPLOG"; then
    ok "the capsule was created through the capsule engine, not a second runtime"
else
    bad "the capsule was created through the capsule engine, not a second runtime"
    sed 's/^/      | /' "$CAPLOG" >&2
fi
# --home is what makes it disposable: without it the capsule shares the real
# home and "delete the environment" would mean deleting the user's home.
if grep -q "^create ${CAPSULE} .*--home=${RIME_DISPOSABLE_ROOT}/${CAPSULE}/home" "$CAPLOG"; then
    ok "the capsule was given a throwaway home, not the user's"
else
    bad "the capsule was given a throwaway home, not the user's"
    grep "^create" "$CAPLOG" | sed 's/^/      | /' >&2
fi

# 2. the agent's working directory is the COPY, not the host worktree
observed_pwd="$(sed -n 's/^pwd=//p' "$OBS1")"
want_pwd="${RIME_DISPOSABLE_ROOT}/${CAPSULE}/home/in/proj"
if [ "$observed_pwd" = "$want_pwd" ]; then
    ok "the agent started in the COPY of the worktree (${observed_pwd})"
else
    bad "the agent started in the COPY of the worktree"
    echo "      pwd was '${observed_pwd}'" >&2
    echo "      wanted  '${want_pwd}'" >&2
fi
if [ "$observed_pwd" != "$PROJ" ]; then
    ok "and NOT in the host worktree"
else
    bad "and NOT in the host worktree"
fi
case "$observed_pwd" in
    */run/host/*) bad "the copy is not reached through /run/host" ;;
    *) ok "the copy is not reached through /run/host" ;;
esac

# The copy really is a copy of the project. Read by the agent from inside,
# BEFORE it edited anything and before teardown deleted the directory.
observed_copied="$(sed -n 's/^copied=//p' "$OBS1")"
if [ "$observed_copied" = "the original" ]; then
    ok "the project's files were copied in (the agent read them from inside)"
else
    bad "the project's files were copied in (the agent read them from inside)"
    echo "      it read: '${observed_copied}'" >&2
fi

# The capsule's HOME is the throwaway home, not the user's. Without that the
# whole feature is a rename: "delete the environment" would mean deleting
# $HOME, which is why the engine makes --home mandatory.
observed_home="$(sed -n 's/^home=//p' "$OBS1")"
if [ "$observed_home" = "${RIME_DISPOSABLE_ROOT}/${CAPSULE}/home" ]; then
    ok "the agent's HOME was the throwaway home (${observed_home})"
else
    bad "the agent's HOME was the throwaway home"
    echo "      home was '${observed_home}', wanted '${RIME_DISPOSABLE_ROOT}/${CAPSULE}/home'" >&2
fi
if [ "$observed_home" != "$HOME" ]; then
    ok "and NOT the user's real home"
else
    bad "and NOT the user's real home"
fi

# 6. the hostile arguments arrived as ARGUMENTS, verbatim and uninterpreted
#
# The count matters as much as the text. `$*` joins with spaces, so an argv
# folded into a shell command string would report the same `argv=` line for
# `hello 'a b'` as a correct one does — the assertion would be blind to the
# exact defect it exists to catch. Four arguments in, four out, each bracketed.
observed_argc="$(sed -n 's/^argc=//p' "$OBS1")"
observed_argv="$(sed -n 's/^argv=//p' "$OBS1")"
want_argv='[hello][a b][$(id -un)][;touch pwned]'
if [ "$observed_argc" = 4 ]; then
    ok "the agent got exactly the 4 arguments it was given, unsplit"
else
    bad "the agent got exactly the 4 arguments it was given, unsplit"
    echo "      argc was '${observed_argc}', wanted 4" >&2
fi
if [ "$observed_argv" = "$want_argv" ]; then
    ok "the agent's arguments arrived verbatim, uninterpreted by any shell"
else
    bad "the agent's arguments arrived verbatim, uninterpreted by any shell"
    echo "      argv was '${observed_argv}'" >&2
    echo "      wanted   '${want_argv}'" >&2
fi
# `;touch pwned` as a command would have created a file. The copy is gone by
# now, so this looks where such a file would SURVIVE: the host tree the agent
# was copied from, and the daemon's working directory.
if [ ! -e "${PROJ}/pwned" ] && [ ! -e "${WORK}/pwned" ] && [ ! -e ./pwned ]; then
    ok "no argument ran as a command anywhere it could have left a trace"
else
    bad "no argument ran as a command anywhere it could have left a trace"
    ls -la "${PROJ}/pwned" "${WORK}/pwned" ./pwned 2>/dev/null | sed 's/^/      /' >&2
fi

# ── teardown ─────────────────────────────────────────────────────────────────
section "the environment is gone when the session ends"

if wait_gone "${RIME_DISPOSABLE_ROOT}/${CAPSULE}"; then
    ok "the environment directory was deleted"
else
    bad "the environment directory was deleted"
    ls -la "$RIME_DISPOSABLE_ROOT" 2>&1 | sed 's/^/      /' >&2
fi
if grep -q "^rm ${CAPSULE}$" "$CAPLOG"; then
    ok "the container was removed through the capsule engine too"
else
    bad "the container was removed through the capsule engine too"
    sed 's/^/      | /' "$CAPLOG" >&2
fi

# 3. the host worktree never saw any of it
if [ ! -e "${PROJ}/agent-wrote-this.txt" ]; then
    ok "the file the agent created is NOT in the host worktree"
else
    bad "the file the agent created is NOT in the host worktree"
fi
if [ "$(cat "${PROJ}/tracked.txt")" = "the original" ]; then
    ok "the file the agent edited is unchanged on the host"
else
    bad "the file the agent edited is unchanged on the host"
    echo "      it says: $(cat "${PROJ}/tracked.txt")" >&2
fi
git_q -C "$PROJ" ls-files --stage > "${WORK}/after.stage"
git_q -C "$PROJ" status --porcelain > "${WORK}/after.porcelain"
if cmp -s "$BEFORE_STAGE" "${WORK}/after.stage"; then
    ok "the host worktree's index is unchanged, entry for entry"
else
    bad "the host worktree's index is unchanged, entry for entry"
    diff -u "$BEFORE_STAGE" "${WORK}/after.stage" | sed 's/^/      /' >&2
fi
if cmp -s "$BEFORE_PORCELAIN" "${WORK}/after.porcelain"; then
    ok "git status --porcelain on the host worktree is byte-identical"
else
    bad "git status --porcelain on the host worktree is byte-identical"
    diff -u "$BEFORE_PORCELAIN" "${WORK}/after.porcelain" | sed 's/^/      /' >&2
fi

# 4. nothing left, because nothing was asked to
found="$(find "$WORK" -name result.txt -not -path "*/disp/*" 2>/dev/null | head -3)"
if [ -z "$found" ]; then
    ok "nothing left the environment, because no --copy-out asked for it"
else
    bad "nothing left the environment, because no --copy-out asked for it"
    printf '      %s\n' $found >&2
fi

# ── with --copy-out, exactly that leaves ─────────────────────────────────────
section "--copy-out is the only way out"

RESULTS="${WORK}/results"
OBS2="${WORK}/observed2"
SID2="$(run_disposable "$OBS2" --copy-out "$RESULTS")"
if [ -n "$SID2" ] && wait_file "$OBS2"; then
    ok "a second session ran with --copy-out (id ${SID2})"
else
    bad "a second session ran with --copy-out"
    sed 's/^/      /' "${WORK}/run.err" >&2
fi
wait_gone "${RIME_DISPOSABLE_ROOT}/disp-agent${SID2}"
if [ -f "${RESULTS}/result.txt" ] \
   && [ "$(cat "${RESULTS}/result.txt")" = "the result" ]; then
    ok "what the agent put in ~/out arrived at the --copy-out destination"
else
    bad "what the agent put in ~/out arrived at the --copy-out destination"
    ls -la "$RESULTS" 2>&1 | sed 's/^/      /' >&2
fi
# ...and only that. The tree the agent edited is not in the destination.
if [ ! -e "${RESULTS}/agent-wrote-this.txt" ] && [ ! -e "${RESULTS}/tracked.txt" ]; then
    ok "and only ~/out left — not the working tree the agent edited"
else
    bad "and only ~/out left — not the working tree the agent edited"
    ls -la "$RESULTS" 2>&1 | sed 's/^/      /' >&2
fi
if [ ! -e "${PROJ}/agent-wrote-this.txt" ]; then
    ok "the host worktree is still untouched after the copy-out run"
else
    bad "the host worktree is still untouched after the copy-out run"
fi

# ── the refusals ─────────────────────────────────────────────────────────────
section "two mechanisms that are refused together, not combined"

before_entries="$(ls "$RIME_DISPOSABLE_ROOT" 2>/dev/null | wc -l)"
before_creates="$(grep -c '^create ' "$CAPLOG")"
before_sessions="$(session_count)"
# Guard against the way this assertion was vacuous once already: if the count
# is 0 here, three sessions into the suite, then it is counting nothing and
# comparing it to itself proves nothing.
if [ "$before_sessions" -ge 2 ]; then
    ok "the session count reads the daemon's list (${before_sessions} so far)"
else
    bad "the session count reads the daemon's list"
    echo "      counted ${before_sessions}; the suite has started 2 sessions by now" >&2
    "$Rime" agent list --all 2>&1 | sed 's/^/      | /' >&2
fi
out="$("$Rime" agent run --agent generic --sandbox strict --disposable \
        --cwd "$PROJ" -d -- /bin/true 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "different mechanisms"; then
    ok "a confining sandbox with --disposable is refused, with the reason"
else
    bad "a confining sandbox with --disposable is refused, with the reason"
    echo "      exit ${rc}" >&2
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi
if printf '%s' "$out" | pipe_has "not the agent"; then
    ok "and the refusal says WHY the pair would deliver neither"
else
    bad "and the refusal says WHY the pair would deliver neither"
fi
if no_create_appears "$before_creates"; then
    ok "the refusal happened BEFORE any environment was created"
else
    bad "the refusal happened BEFORE any environment was created"
    grep '^create' "$CAPLOG" | sed 's/^/      | /' >&2
fi
after_entries="$(ls "$RIME_DISPOSABLE_ROOT" 2>/dev/null | wc -l)"
if [ "$before_entries" = "$after_entries" ]; then
    ok "and no environment directory was left behind by the refusal"
else
    bad "and no environment directory was left behind by the refusal"
    echo "      dirs ${before_entries} -> ${after_entries}" >&2
fi
# A refused run must not leave a session either. Deterministic: the CLI has
# already returned, so the count cannot still be catching up.
if [ "$(session_count)" = "$before_sessions" ]; then
    ok "and the refused run started no session at all"
else
    bad "and the refused run started no session at all"
    "$Rime" agent list 2>&1 | sed 's/^/      | /' >&2
fi

# This one is refused by CLAP, not by the daemon: `--copy-out` is declared
# `requires = "disposable"`, so the request never leaves the CLI. Named for
# what it is — the daemon's own arm answers every other client of the socket
# and is covered by the `copy_out_without_a_capsule_is_refused_not_ignored`
# unit test, not by this line.
out="$("$Rime" agent run --agent generic --sandbox unrestricted \
        --copy-out "$RESULTS" --cwd "$PROJ" -d -- /bin/true 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has -- "--disposable"; then
    ok "--copy-out without --disposable is refused at the CLI, naming the flag it needs"
else
    bad "--copy-out without --disposable is refused at the CLI, naming the flag it needs"
    echo "      exit ${rc}" >&2
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi

# ── the two refusals the DAEMON owns ────────────────────────────────────────
# No clap conflict declared for these, deliberately, so what is exercised here
# is the daemon's own check — the answer every client of the socket gets.
before_creates="$(grep -c '^create ' "$CAPLOG")"
WT_BEFORE="$(ls "${PROJ}/.rime/worktrees" 2>/dev/null | sort | tr '\n' ' ')"
out="$("$Rime" agent run --agent generic --sandbox unrestricted --disposable \
        --worktree throwaway --cwd "$PROJ" -d -- /bin/true 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "left empty"; then
    ok "--worktree with --disposable is refused by the daemon, with the reason"
else
    bad "--worktree with --disposable is refused by the daemon, with the reason"
    echo "      exit ${rc}" >&2
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi
# The refusal is BEFORE ensure_worktree, which is the point: a refusal that
# happened afterwards would leave the empty branch it warns about.
WT_AFTER="$(ls "${PROJ}/.rime/worktrees" 2>/dev/null | sort | tr '\n' ' ')"
if [ "$WT_BEFORE" = "$WT_AFTER" ]; then
    ok "and no host worktree was created before refusing (the empty branch it warns about)"
else
    bad "and no host worktree was created before refusing"
    echo "      before: '${WT_BEFORE}' after: '${WT_AFTER}'" >&2
fi
if ! git_q -C "$PROJ" rev-parse --verify -q refs/heads/agent/throwaway >/dev/null 2>&1; then
    ok "and no branch agent/throwaway exists (the name ensure_worktree would use)"
else
    bad "and no branch agent/throwaway exists (the name ensure_worktree would use)"
    git_q -C "$PROJ" branch -a | sed 's/^/      | /' >&2
fi

out="$("$Rime" agent run --agent generic --sandbox unrestricted --disposable \
        --checkpoint --cwd "$PROJ" -d -- /bin/true 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "never touched"; then
    ok "--checkpoint with --disposable is refused by the daemon, with the reason"
else
    bad "--checkpoint with --disposable is refused by the daemon, with the reason"
    echo "      exit ${rc}" >&2
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi
if no_create_appears "$before_creates"; then
    ok "neither refusal created an environment first"
else
    bad "neither refusal created an environment first"
    grep '^create' "$CAPLOG" | sed 's/^/      | /' >&2
fi

# ── what a person sees ───────────────────────────────────────────────────────
section "the session says it is disposable"

OBS3="${WORK}/observed3"
HOLD="${WORK}/hold.sh"
cat > "$HOLD" <<'HOLD_EOF'
#!/usr/bin/env bash
printf 'pwd=%s\n' "$PWD" > "$1"
sleep 600
HOLD_EOF
chmod +x "$HOLD"
SID3="$("$Rime" agent run --agent generic --sandbox unrestricted \
    --disposable --cwd "$PROJ" -d -- /bin/bash "$HOLD" "$OBS3" 2>"${WORK}/run3.err" \
    | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
wait_file "$OBS3"
status="$("$Rime" agent status "$SID3" 2>&1)"
printf '%s\n' "$status" | sed 's/^/      | /'
if printf '%s' "$status" | pipe_has "disp-agent${SID3}" \
   && printf '%s' "$status" | pipe_has "disposable"; then
    ok "rime agent status names the disposable capsule"
else
    bad "rime agent status names the disposable capsule"
fi
if printf '%s' "$status" | pipe_has -i "COPY"; then
    ok "and says the working tree is a copy that is deleted with the session"
else
    bad "and says the working tree is a copy that is deleted with the session"
fi
# Killing the session must tear the environment down: the engine's TERM trap.
"$Rime" agent kill "$SID3" >/dev/null 2>&1
if wait_gone "${RIME_DISPOSABLE_ROOT}/disp-agent${SID3}"; then
    ok "killing the session tears the environment down (the engine's TERM trap)"
else
    bad "killing the session tears the environment down (the engine's TERM trap)"
    ls -la "$RIME_DISPOSABLE_ROOT" 2>&1 | sed 's/^/      /' >&2
fi

# ── the DAEMON's own death, measured rather than asserted ─────────────────
#
# Three places in this feature used to say the engine's teardown also runs when
# the DAEMON dies, and none of them had measured it. A cleanup that is claimed
# to run on daemon death is exactly the kind of claim that passes by never
# being exercised, so this section gets daemons of its own and really kills
# them.
#
# Two deaths are run because they LOOK like two routes. They are not, and that
# is this section's finding rather than its premise — the first draft said the
# SIGTERM half was Rime's own shutdown doing the work, and nothing had checked
# it:
#
#   SIGKILL — none of Rime's code runs. What reaches the engine is the kernel's
#   doing: the dead daemon's PTY master fd closes, and the kernel sends SIGHUP
#   to the foreground process group of the slave. The engine traps EXIT, INT
#   and TERM but NOT HUP, and bash runs an EXIT trap even while it is dying of
#   an untrapped fatal signal (measured: exit status 129, trap body executed).
#
#   SIGTERM — the SAME kernel route, because Rime's own shutdown DOES NOT RUN.
#   `block_termination_signals` (rime-agentd/src/main.rs:169) installs the mask
#   AFTER `spawn_expiry_thread` at 164 has already started a thread. Threads do
#   inherit a mask, so that one thread — and only it — runs unmasked, and a
#   process-directed signal goes to the first thread that does not block it.
#   MEASURED on /proc/<pid>/task/*/status, a scratch daemon of its own:
#
#       rime-agentd      SigBlk=0000000000004003  syscall=288
#       rime-agentd-gra  SigBlk=0000000000000000  syscall=230  ← spawned at 164
#       rime-agentd-sig  SigBlk=0000000000000000  syscall=128  ← see below
#
#   so SIGTERM lands on the grants thread and the daemon dies by DEFAULT
#   DISPOSITION: measured exit status 143, the log holding only its `listening
#   on` line — "stopping sessions" never prints, the control socket is never
#   removed, and `shutdown` → `registry::terminate` never runs for any session.
#
#   Do NOT read the signal thread's zero as a second unmasked thread: that one
#   is a DISPLAY ARTIFACT of the syscall it is in. `sigwait` is
#   `rt_sigtimedwait` (syscall 128), which parks the waited-for bits out of
#   `blocked` for the duration of the wait, and /proc prints `blocked`.
#   MEASURED with a probe of three threads: spawned before the mask reads 0,
#   spawned after it and merely sleeping reads 0000000000004003, spawned after
#   it and sitting in sigwait reads 0. Same mask, different syscall.
#
# Which is why the mutation that proves this section can go red is "make the
# ENGINE ignore SIGHUP", and why it reddens BOTH halves, four assertions each.
# Two mutations inside `registry::terminate` — send only SIGTERM, and signal
# nobody at all — both SURVIVE with everything green: Rime's own shutdown
# signalling is not what tears a capsule down. (`rime agent kill` is a third
# path again, `Request::Signal` → `pty::signal_group`, never `terminate`, and
# it is unaffected by either mutation.)
#
# And it stays that way even once the ordering is fixed, which is worth knowing
# before anyone treats this section as the guard on that fix. MEASURED by
# moving line 169 above 164, rebuilding, and running both of these cases again:
# the shutdown then really does run (exit 0, "signal 15, stopping sessions",
# socket removed) — and "terminate signals nobody" STILL survives 58/0,
# because `shutdown` is followed by `process::exit`, the PTY master closes
# anyway, and the kernel's SIGHUP arrives just the same. What these twelve
# assertions guarantee is the OUTCOME — the capsule goes, whoever sends the
# signal — and the engine's traps are the only code that can break it.
#
# Both cases stay regardless: they cost one daemon each and they are what will
# notice if the outcome ever stops holding on either death.
#
# Each case gets its OWN daemon, runtime directory, disposable root and capsule
# log. Sharing the suite's log would have made every assertion here satisfiable
# by an EARLIER session's lines: session ids restart at 1 with a new daemon, so
# "rm disp-agent1" is already in the shared log by this point and a grep for it
# would pass without this daemon having done anything whatsoever.
#
# Still NOT proven here, and still hedged in the docs: a machine that loses
# power, where nothing gets to run.
section "the capsule when the DAEMON dies"

MAIN_RUNTIME="$XDG_RUNTIME_DIR"
MAIN_STATE="$XDG_STATE_HOME"
MAIN_CONFIG="$XDG_CONFIG_HOME"
MAIN_SCRATCH="$RIME_AGENT_SCRATCH_ROOT"
MAIN_DISP="$RIME_DISPOSABLE_ROOT"
MAIN_CAPLOG="$CAPSULE_LOG"

HOLD2="${WORK}/hold-for-death.sh"
cat > "$HOLD2" <<'HOLD2_EOF'
#!/usr/bin/env bash
printf 'pwd=%s\n' "$PWD" > "$1"
sleep 600
HOLD2_EOF
chmod +x "$HOLD2"

daemon_death_case() {   # daemon_death_case <label> <signal> <how it reads>
    local label="$1" signal="$2" reads="$3"
    local dir="${WORK}/death-${label}"
    mkdir -p "$dir"
    export XDG_RUNTIME_DIR="${dir}/run"
    export XDG_STATE_HOME="${dir}/state"
    export XDG_CONFIG_HOME="${dir}/config"
    export RIME_AGENT_SCRATCH_ROOT="${dir}/scratch"
    export RIME_DISPOSABLE_ROOT="${dir}/disp"
    export CAPSULE_LOG="${dir}/capsule.log"
    mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME" "$RIME_DISPOSABLE_ROOT"
    chmod 0700 "$XDG_RUNTIME_DIR"
    : > "$CAPSULE_LOG"

    "$AGENTD" > "${dir}/agentd.log" 2>&1 &
    local dpid=$!
    EXTRA_PIDS="${EXTRA_PIDS} ${dpid}"
    local sock="${XDG_RUNTIME_DIR}/rime-agentd/control.sock"
    local _
    for _ in $(seq 1 60); do [ -S "$sock" ] && break; sleep 0.1; done
    if [ ! -S "$sock" ]; then
        bad "a daemon of its own came up for the ${reads} case"
        sed 's/^/      /' "${dir}/agentd.log" >&2
        kill -9 "$dpid" 2>/dev/null
        return 1
    fi

    local obs="${dir}/observed"
    local sid
    sid="$("$Rime" agent run --agent generic --sandbox unrestricted \
        --disposable --cwd "$PROJ" -d -- /bin/bash "$HOLD2" "$obs" \
        2>"${dir}/run.err" | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
    local capsule="disp-agent${sid}"
    local envdir="${RIME_DISPOSABLE_ROOT}/${capsule}"
    # The precondition, asserted rather than assumed. Everything below is a
    # statement about a capsule that was running, and if none was running then
    # "the environment is gone" is true of a directory that never existed.
    if [ -n "$sid" ] && wait_file "$obs" && [ -d "$envdir" ] \
       && [ -n "$(pgrep -f "$dir" 2>/dev/null)" ]; then
        ok "a disposable session was really running before the daemon was ${reads}"
    else
        bad "a disposable session was really running before the daemon was ${reads}"
        echo "      session=[${sid}] envdir=${envdir}" >&2
        sed 's/^/      /' "${dir}/run.err" >&2
        sed 's/^/      | /' "$CAPSULE_LOG" >&2
        for p in $(pgrep -f "$dir" 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
        kill -9 "$dpid" 2>/dev/null
        return 1
    fi

    { kill -"$signal" "$dpid" 2>/dev/null; } 2>/dev/null
    for _ in $(seq 1 60); do kill -0 "$dpid" 2>/dev/null || break; sleep 0.1; done
    { wait "$dpid"; } 2>/dev/null
    if kill -0 "$dpid" 2>/dev/null; then
        bad "the daemon is dead after ${reads}"
    else
        ok "the daemon is dead after ${reads}"
    fi

    if wait_gone "$envdir"; then
        ok "the environment is gone after the daemon was ${reads}"
    else
        bad "the environment is gone after the daemon was ${reads}"
        ls -la "$RIME_DISPOSABLE_ROOT" 2>&1 | sed 's/^/      /' >&2
    fi
    # A directory that merely vanished is not a teardown. The engine's own
    # removal pass is what asks the capsule engine to remove the container, so
    # this line is the one that says the trap body ran to the end.
    if grep -q "^rm ${capsule}$" "$CAPSULE_LOG"; then
        ok "and the teardown really ran: the capsule engine was asked to remove ${capsule}"
    else
        bad "and the teardown really ran: the capsule engine was asked to remove ${capsule}"
        sed 's/^/      | /' "$CAPSULE_LOG" >&2
    fi
    # `pgrep -f "$dir"` reaches the ENGINE and not only the agent because this
    # case's own directory rides in the engine's own argv: the observation file
    # is a POSITIONAL adapter argument, so it appears on the engine's command
    # line and again on the agent's. Do not "simplify" this to a match on the
    # engine's name — that would also catch the user's own environments, which
    # this suite must never touch.
    local survivors
    survivors="$(pgrep -f "$dir" 2>/dev/null | tr '\n' ' ')"
    if [ -z "$survivors" ]; then
        ok "no engine and no agent process outlived the daemon"
    else
        bad "no engine and no agent process outlived the daemon"
        echo "      still alive: ${survivors}" >&2
        ps -o pid=,args= -p ${survivors} 2>&1 | sed 's/^/      | /' >&2
    fi
    if [ -z "$(ls "$RIME_DISPOSABLE_ROOT" 2>/dev/null)" ]; then
        ok "and that daemon's disposable root is empty, not just missing one entry"
    else
        bad "and that daemon's disposable root is empty, not just missing one entry"
        ls -la "$RIME_DISPOSABLE_ROOT" 2>&1 | sed 's/^/      /' >&2
    fi

    for p in $(pgrep -f "$dir" 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
    kill -9 "$dpid" 2>/dev/null
}

daemon_death_case term TERM "stopped with SIGTERM"
daemon_death_case kill KILL "SIGKILLed, with no chance to run any code"

export XDG_RUNTIME_DIR="$MAIN_RUNTIME"
export XDG_STATE_HOME="$MAIN_STATE"
export XDG_CONFIG_HOME="$MAIN_CONFIG"
export RIME_AGENT_SCRATCH_ROOT="$MAIN_SCRATCH"
export RIME_DISPOSABLE_ROOT="$MAIN_DISP"
export CAPSULE_LOG="$MAIN_CAPLOG"

# ── nothing of the user's was touched ────────────────────────────────────────
section "the suite stayed inside its own fixture"
if [ -z "$(ls "$RIME_DISPOSABLE_ROOT" 2>/dev/null)" ]; then
    ok "the fixture's disposable root is empty — every environment was torn down"
else
    bad "the fixture's disposable root is empty — every environment was torn down"
    ls -la "$RIME_DISPOSABLE_ROOT" | sed 's/^/      /' >&2
fi
POST_REAL="$(ls "$REAL_DISP" 2>/dev/null | sort | tr '\n' ' ')"
if [ "$PRE_REAL" = "$POST_REAL" ]; then
    ok "the user's OWN disposable root is exactly as it was"
else
    bad "the user's OWN disposable root is exactly as it was"
    echo "      before: ${PRE_REAL}" >&2
    echo "      after:  ${POST_REAL}" >&2
fi
gone=""
for entry in $PRE_SCRATCH; do
    [ -e "/tmp/rime-agent/${entry}" ] || gone="${gone}${entry} "
done
if [ -z "$gone" ]; then
    ok "nothing was deleted from the shared /tmp/rime-agent the real daemon uses"
else
    bad "nothing was deleted from the shared /tmp/rime-agent the real daemon uses"
    echo "      gone: ${gone}" >&2
fi

printf '\ndisposable: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
