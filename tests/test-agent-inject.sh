#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for `rime agent send` (P1-035): handing a file to a
#  running TUI agent.
#
#  The unit tests cover the name reduction, the destination and the exact bytes.
#  What they cannot cover is the three claims the design rests on, all of which
#  are about a real PTY and a real socket:
#
#      1. the bytes arrive at the session they were addressed to, and at no
#         other session;
#      2. nothing that arrives can submit the line — the human presses Enter;
#      3. a MANAGED SESSION may not use this verb. The daemon reads the source
#         file with the daemon's own access, outside every sandbox, so a
#         session that could ask for it could ask for ~/.ssh.
#
#  Sessions here run a script that puts its own terminal into raw mode and
#  copies what it reads to a file, which is what a real TUI agent does and what
#  makes "did the bytes arrive" answerable. A session left in the default
#  canonical mode would not see an unterminated line at all.
#
#  NOTHING HERE TOUCHES A RUNNING DAEMON. Its own XDG_RUNTIME_DIR, its own
#  XDG_STATE_HOME, its own XDG_CONFIG_HOME, and the daemon is killed by the pid
#  this script started — never by name.
#
#      ./tests/test-agent-inject.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` for the reason test-privilege-requests.sh documents: this suite
# counts failures rather than aborting, and several assertions run commands
# that exit non-zero on purpose.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Before the temp tree, the daemon or the build: this suite needs an
# environment the daemon will observe as LOCAL, and that has to be arranged
# from outside the suite. `rime-agentd` places a peer from its cgroup, and a
# process started by systemd — a CI job, a timer-dispatched agent — is in
# neither a login session nor a user service, so §7 refuses it before the
# behaviour under test is reached. Here it stops the suite starting the two
# sessions the path is handed between, and a caller the daemon cannot place
# may not name a file on this machine at all.
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
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    [ -n "$DAEMON_PID" ] && { for _ in 1 2 3 4 5; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done; }
    [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

# ── prerequisites ────────────────────────────────────────────────────────────
# A missing prerequisite is a FAILURE, never a skip: a suite that prints
# "0 passed, 0 failed" and exits 0 is a green tick over nothing asserted.
for tool in cargo git python3 stty bwrap; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    printf '\ninject: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "rime-agentd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

# ── an isolated runtime ──────────────────────────────────────────────────────
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

PROJ="${WORK}/proj"
mkdir -p "$PROJ"
git -C "$PROJ" init -q 2>/dev/null
git -C "$PROJ" config user.email t@example.invalid
git -C "$PROJ" config user.name t

# ── the last thing that used to be un-fixtured ───────────────────────────────
#
# Session scratch was `/tmp/rime-agent/<id>` with no XDG in it, so a fixture
# daemon and the user's own daemon shared that namespace — and a session reap
# runs `remove_dir_all` on its own id's directory. A fixture daemon numbers its
# sessions from 1, because its reservation store IS fixtured, so a machine whose
# real daemon happened to hold session 1 would have had it deleted by a test
# suite. RIME_AGENT_SCRATCH_ROOT is what closes that, and the two assertions
# below are what say it is closed rather than that it was intended to be.
export RIME_AGENT_SCRATCH_ROOT="${WORK}/scratch"
# Pre-created, and 0755 deliberately. The scratch root is the one agent path
# whose parent is world-writable, so another account can pre-create it in /tmp
# and own it — and the daemon used to ensure only the session LEAF under it,
# which is genuinely its own, while walking past whatever the root was. A root
# the daemon MAKES is 0700 either way now, so a root it FINDS is the only shape
# that can tell the boundary call from its absence. See
# `paths::SCRATCH_ROOT_PREFIX` for what was measured with a second account.
mkdir -p "$RIME_AGENT_SCRATCH_ROOT"
chmod 0755 "$RIME_AGENT_SCRATCH_ROOT"   # not `mkdir -m`: SC2174, and the mode
                                        # is the whole point of this fixture
PRE_SCRATCH="$(ls /tmp/rime-agent 2>/dev/null | sort -n | tr '\n' ' ')"

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
    printf '\ninject: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ── a session that behaves like a TUI ────────────────────────────────────────
#
# raw mode, then copy stdin to a file. `stty raw` is what every full-screen
# agent does to its terminal; without it the kernel's line discipline holds an
# unterminated line and nothing this suite sends would ever reach the program —
# which would make every assertion below pass for the wrong reason.
make_session() {   # make_session <tag> [decset]
    local tag="$1" decset="${2:-no}"
    local script="${WORK}/${tag}.sh"
    cat > "$script" <<EOF
#!/usr/bin/env bash
$( [ "$decset" = "decset" ] && echo "printf '\\033[?2004h'" )
stty raw -echo 2>/dev/null
: > "${WORK}/${tag}.ready"
exec cat > "${WORK}/${tag}.cap"
EOF
    chmod +x "$script"
    : > "${WORK}/${tag}.cap"
    "$Rime" agent run --agent generic --sandbox unrestricted --cwd "$PROJ" -d \
        -- /bin/bash "$script" 2>"${WORK}/${tag}.err" \
        | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1
}

wait_ready() {     # wait_ready <tag>
    for _ in $(seq 1 100); do
        [ -e "${WORK}/${1}.ready" ] && return 0
        sleep 0.05
    done
    return 1
}

wait_capture() {   # wait_capture <tag> <needle>
    for _ in $(seq 1 100); do
        grep -qF "$2" "${WORK}/${1}.cap" 2>/dev/null && return 0
        sleep 0.05
    done
    return 1
}

section "two sessions, one of them the target"
A="$(make_session a)"
B="$(make_session b)"
if [ -n "$A" ] && [ -n "$B" ]; then
    ok "two sessions started (ids ${A} and ${B})"
else
    bad "two sessions started"
    sed 's/^/      /' "${WORK}/a.err" "${WORK}/b.err"
    printf '\ninject: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
wait_ready a && wait_ready b \
    && ok "both sessions put their terminals into raw mode" \
    || bad "both sessions put their terminals into raw mode"

if [ -d "${RIME_AGENT_SCRATCH_ROOT}/${A}" ]; then
    ok "the daemon put its session scratch where the fixture told it to"
else
    bad "the daemon put its session scratch where the fixture told it to"
    echo "      expected ${RIME_AGENT_SCRATCH_ROOT}/${A}" >&2
    echo "      this suite would otherwise share /tmp/rime-agent with the real daemon" >&2
    printf '\ninject: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# The root, and not only the session directory inside it (P2-016). The control
# is the line above: the session directory landing where the fixture said is
# what makes this a measurement of THIS daemon's root rather than of a chmod
# nobody ran.
ROOT_MODE="$(stat -c '%a' "$RIME_AGENT_SCRATCH_ROOT" 2>/dev/null)"
LEAF_MODE="$(stat -c '%a' "${RIME_AGENT_SCRATCH_ROOT}/${A}" 2>/dev/null)"
if [ "$ROOT_MODE" = "700" ] && [ "$LEAF_MODE" = "700" ]; then
    ok "the scratch ROOT was made private too, not only the session directory"
else
    bad "the scratch ROOT was made private too, not only the session directory"
    echo "      root ${RIME_AGENT_SCRATCH_ROOT} is ${ROOT_MODE:-<absent>}, wanted 700" >&2
    echo "      leaf is ${LEAF_MODE:-<absent>}, wanted 700" >&2
    echo "      a root another account pre-created is left in place by a leaf-only call" >&2
fi

# The source lives outside the project, which is the case the feature is for:
# a confined session cannot see ~/Pictures, and the daemon can.
SRCDIR="${WORK}/pictures"
mkdir -p "$SRCDIR"
SRC="${SRCDIR}/shot.png"
printf 'not really a png, but bytes are bytes\n' > "$SRC"

section "the path reaches the terminal it was addressed to"
out="$("$Rime" agent send "$A" "$SRC" 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'

DEST="$(printf '%s' "$out" | sed -n "s#.*as \(/[^ ]*inbox/[^ ]*\)#\1#p" | head -1)"
if [ -n "$DEST" ]; then
    ok "the command reported where the agent will find it (${DEST})"
else
    bad "the command reported where the agent will find it"
fi

wait_capture a "${DEST:-NOTHING-WAS-REPORTED}" \
    && ok "the path arrived on session ${A}'s terminal" \
    || bad "the path arrived on session ${A}'s terminal"

# THE assertion behind "correct PTY". A daemon that wrote to every master, or
# to the wrong one, passes every test above and fails this one.
if [ -s "${WORK}/b.cap" ]; then
    bad "session ${B}'s terminal received nothing"
    od -c "${WORK}/b.cap" | head -5 | sed 's/^/      /'
else
    ok "session ${B}'s terminal received nothing"
fi

# Byte-level, in python, and NOT in grep: a grep pattern containing a newline
# is two patterns, the second of them empty, and an empty pattern matches every
# line. The first version of this suite reported the non-submission assertion as
# a failure on bytes that were correct.
cap_check() {   # cap_check <file> <python expression> <name>
    python3 - "$1" "$2" <<'PY' && ok "$3" || { bad "$3"; od -c "$1" | head -5 | sed 's/^/      /'; }
import sys
b = open(sys.argv[1], 'rb').read()
sys.exit(0 if eval(sys.argv[2], {'b': b}) else 1)
PY
}
cap_check "${WORK}/a.cap" "b'\\n' not in b" \
    "nothing that arrived can submit the line"
cap_check "${WORK}/a.cap" "b'\\r' not in b" \
    "nothing that arrived carries a carriage return"
cap_check "${WORK}/a.cap" "b.endswith(b' ')" \
    "the path is followed by a separator, not a submission"
cap_check "${WORK}/a.cap" "b'\\x1b' not in b" \
    "a terminal that never asked for bracketed paste got no escapes"

section "the copy"
if [ -n "$DEST" ] && [ -f "$DEST" ]; then
    ok "the file was copied where the agent was told to look"
    cmp -s "$SRC" "$DEST" \
        && ok "the copy is byte-identical to the source" \
        || bad "the copy is byte-identical to the source"
else
    bad "the file was copied where the agent was told to look"
    bad "the copy is byte-identical to the source"
fi
case "$DEST" in
    "${RIME_AGENT_SCRATCH_ROOT}/${A}"/inbox/*)
        ok "the copy is inside that session's own scratch directory" ;;
    *)  bad "the copy is inside that session's own scratch directory (${DEST})" ;;
esac

"$Rime" agent status "$A" 2>/dev/null | pipe_has '^files sent   1$' \
    && ok "the session records that a file was handed to it" \
    || bad "the session records that a file was handed to it"

"$Rime" agent list --all --json 2>/dev/null | python3 -c "
import json,sys
ss = {s['id']: s for s in json.load(sys.stdin)}
assert ss[${A}]['injected'] == 1, ss[${A}]
assert ss[${B}]['injected'] == 0, ss[${B}]
" 2>/dev/null \
    && ok "the count is per session and readable by the shell" \
    || bad "the count is per session and readable by the shell"

section "the name that is typed is the daemon's, not the caller's"
EVIL="${SRCDIR}/we ird;\$(id) 'q' \"q\".log"
printf 'evil\n' > "$EVIL"
out="$("$Rime" agent send "$A" "$EVIL" 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'
DEST2="$(printf '%s' "$out" | sed -n "s#.*as \(/[^ ]*inbox/[^ ]*\)#\1#p" | head -1)"
if [ -n "$DEST2" ]; then
    ok "a file with a hostile name is still handed over (${DEST2})"
else
    bad "a file with a hostile name is still handed over"
fi
case "$DEST2" in
    *[\ \'\"\$\;\`\|\&\<\>]*) bad "the typed path carries no shell metacharacter" ;;
    "")                       bad "the typed path carries no shell metacharacter" ;;
    *)                        ok  "the typed path carries no shell metacharacter" ;;
esac
[ -n "$DEST2" ] && [ -f "$DEST2" ] \
    && ok "the hostile name still names the right bytes" \
    || bad "the hostile name still names the right bytes"
case "$DEST2" in
    */002-*) ok "a second file gets the next number, not the same name" ;;
    *)       bad "a second file gets the next number, not the same name (${DEST2})" ;;
esac

# A name a terminal would act on is refused outright rather than repaired, and
# nothing is typed for it.
before="$(wc -c < "${WORK}/a.cap")"
NL_FILE="${SRCDIR}/$(printf 'break\nout.log')"
printf 'x\n' > "$NL_FILE" 2>/dev/null
if [ -f "$NL_FILE" ]; then
    out="$("$Rime" agent send "$A" "$NL_FILE" 2>&1)"
    printf '%s' "$out" | pipe_has -i "control character" \
        && ok "a file name with a newline in it is refused by name" \
        || { bad "a file name with a newline in it is refused by name"; printf '%s\n' "$out" | sed 's/^/      | /'; }
    after="$(wc -c < "${WORK}/a.cap")"
    [ "$before" = "$after" ] \
        && ok "a refused file types nothing at all" \
        || bad "a refused file types nothing at all"
else
    bad "a file name with a newline in it is refused by name"
    bad "a refused file types nothing at all"
fi

section "what cannot be handed over"
out="$("$Rime" agent send "$A" "${SRCDIR}/does-not-exist" 2>&1)"
printf '%s' "$out" | pipe_has -i "cannot hand over" \
    && ok "a file that is not there is refused before the daemon is asked" \
    || bad "a file that is not there is refused before the daemon is asked"

out="$("$Rime" agent send "$A" "$SRCDIR" 2>&1)"
printf '%s' "$out" | pipe_has -i "is a directory" \
    && ok "a directory is refused, and told apart from a file" \
    || { bad "a directory is refused, and told apart from a file"; printf '%s\n' "$out" | sed 's/^/      | /'; }

out="$("$Rime" agent send 999999 "$SRC" 2>&1)"
printf '%s' "$out" | pipe_has -i "no session" \
    && ok "a session that does not exist is refused" \
    || bad "a session that does not exist is refused"

# The CLI turns a relative path into an absolute one; the daemon refuses a
# relative one, because its working directory is not the caller's.
( cd "$SRCDIR" && "$Rime" agent send "$A" shot.png ) >"${WORK}/rel.out" 2>&1
grep -q "inbox/" "${WORK}/rel.out" \
    && ok "a relative path works from the caller's own directory" \
    || { bad "a relative path works from the caller's own directory"; sed 's/^/      | /' "${WORK}/rel.out"; }

printf '{"cmd":"inject","id":%s,"source":"shot.png"}\n' "$A" \
    | python3 -c "
import socket,sys
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.settimeout(10)
s.connect('${SOCK}'); s.sendall(sys.stdin.buffer.read())
print(s.makefile().readline().strip())
" > "${WORK}/relwire.out" 2>&1
grep -q "absolute path" "${WORK}/relwire.out" \
    && ok "the daemon refuses a relative path on the wire" \
    || { bad "the daemon refuses a relative path on the wire"; sed 's/^/      | /' "${WORK}/relwire.out"; }

section "the screenshot shortcut takes no picture"
export RIME_SCREENSHOT_DIR="${WORK}/shots"
mkdir -p "$RIME_SCREENSHOT_DIR"
out="$("$Rime" agent send "$A" --last-screenshot 2>&1)"
printf '%s' "$out" | pipe_has -i "holds no screenshots yet" \
    && ok "an empty screenshot directory says so instead of guessing" \
    || { bad "an empty screenshot directory says so instead of guessing"; printf '%s\n' "$out" | sed 's/^/      | /'; }

printf 'older\n' > "${RIME_SCREENSHOT_DIR}/Screenshot_old.png"
sleep 1.1
printf 'newest\n' > "${RIME_SCREENSHOT_DIR}/Screenshot_new.png"
out="$("$Rime" agent send "$A" --last-screenshot 2>&1)"
printf '%s' "$out" | pipe_has "Screenshot_new.png -> session ${A}" \
    && ok "the newest screenshot is the one handed over" \
    || { bad "the newest screenshot is the one handed over"; printf '%s\n' "$out" | sed 's/^/      | /'; }
unset RIME_SCREENSHOT_DIR

section "an agent reading pasted text gets the markers, and only then"
C="$(make_session c decset)"
if [ -n "$C" ] && wait_ready c; then
    ok "a session that asks for bracketed paste started (id ${C})"
    # The daemon learns the mode from the session's OUTPUT, so give the reader
    # thread a moment to have seen it.
    sleep 0.5
    "$Rime" agent send "$C" "$SRC" >"${WORK}/c.out" 2>&1
    wait_capture c "inbox/" \
        && ok "the path arrived on session ${C}'s terminal" \
        || bad "the path arrived on session ${C}'s terminal"
    python3 - "${WORK}/c.cap" <<'PY' && ok "the text arrived wrapped as a paste, because that agent asked for it" || bad "the text arrived wrapped as a paste, because that agent asked for it"
import sys
b = open(sys.argv[1], 'rb').read()
assert b.startswith(b'\x1b[200~'), b
assert b.endswith(b'\x1b[201~ '), b
PY
    grep -q "reads pasted text as a paste" "${WORK}/c.out" \
        && ok "the command says the agent will see it as a paste" \
        || { bad "the command says the agent will see it as a paste"; sed 's/^/      | /' "${WORK}/c.out"; }
else
    bad "a session that asks for bracketed paste started"
    bad "the path arrived on session c's terminal"
    bad "the text arrived wrapped as a paste, because that agent asked for it"
    bad "the command says the agent will see it as a paste"
fi

# ── the boundary ─────────────────────────────────────────────────────────────
#
# A session asking the daemon to read a file and drop it somewhere a session
# can read is a complete escape from the sandbox, whichever session it names.
# The daemon must resolve the caller from the KERNEL — SO_PEERCRED and /proc
# ancestry — and refuse, exactly the way it refuses a session deciding its own
# privilege request.
section "a session may not hand a file to a session"
SECRET="${WORK}/pretend-private-key"
printf 'PRIVATE KEY MATERIAL\n' > "$SECRET"
cat > "${WORK}/inside.sh" <<EOF
#!/usr/bin/env bash
echo "--- a session naming another session ---"
"$Rime" agent send ${A} "$SECRET" 2>&1
echo "OTHER_EXIT=\$?"
echo "--- a session naming itself ---"
"$Rime" agent send \${RIME_AGENT_SESSION:-0} "$SECRET" 2>&1
echo "SELF_EXIT=\$?"
echo "DONE"
EOF
chmod +x "${WORK}/inside.sh"

before_a="$(wc -c < "${WORK}/a.cap")"
sid="$("$Rime" agent run --agent generic --sandbox unrestricted --cwd "$PROJ" -d \
        -- /bin/bash "${WORK}/inside.sh" 2>"${WORK}/inside.err" \
        | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
if [ -z "$sid" ]; then
    bad "a session started to try it from the inside"
    sed 's/^/      /' "${WORK}/inside.err"
else
    ok "a session started to try it from the inside (id ${sid})"
    for _ in $(seq 1 80); do
        "$Rime" agent logs "$sid" 2>/dev/null | pipe_has DONE && break
        sleep 0.25
    done
    logs="$("$Rime" agent logs "$sid" 2>/dev/null)"
    printf '%s\n' "$logs" | sed 's/^/      | /'

    printf '%s' "$logs" | pipe_has "may not hand a file to a session" \
        && ok "the daemon refused a session naming another session" \
        || bad "the daemon refused a session naming another session"
    printf '%s' "$logs" | pipe_has "OTHER_EXIT=0" \
        && bad "the refusal is an error exit, not a quiet no-op" \
        || ok "the refusal is an error exit, not a quiet no-op"
    printf '%s' "$logs" | pipe_has "SELF_EXIT=0" \
        && bad "a session naming ITSELF is refused too" \
        || ok "a session naming ITSELF is refused too"

    after_a="$(wc -c < "${WORK}/a.cap")"
    [ "$before_a" = "$after_a" ] \
        && ok "nothing was typed into the session it named" \
        || bad "nothing was typed into the session it named"
    if grep -rqF "PRIVATE KEY MATERIAL" "${RIME_AGENT_SCRATCH_ROOT}/${A}/inbox" 2>/dev/null; then
        bad "the file the session asked for never crossed the boundary"
    else
        ok "the file the session asked for never crossed the boundary"
    fi
fi

# ── the case the feature exists for ──────────────────────────────────────────
#
# Everything above runs unconfined, which proves the plumbing and proves
# nothing about the boundary. A CONFINED session has /tmp replaced by a tmpfs
# with only its own scratch bound back and $HOME masked, so it cannot open the
# source file at all — and must be able to open the copy. Both halves are
# asserted from inside the session, by the session.
section "a confined session can read the copy and not the original"
if [ "$(sysctl -n dev.tty.legacy_tiocsti 2>/dev/null || echo 0)" = "1" ]; then
    bad "this kernel still honours TIOCSTI, so the runtime refuses to confine anything"
else
    D="${PROJ}/d"
    mkdir -p "$D"
    cat > "${D}/run.sh" <<EOF
#!/usr/bin/env bash
stty raw -echo 2>/dev/null
: > "${D}/ready"
# The runtime types a path and one trailing space, and never a newline, so a
# line-oriented read would wait forever. Reading to the separator is what a
# terminal program with an input line does with the bytes it is given.
IFS= read -r -d ' ' path
{
  printf 'GOT=%s\n' "\$path"
  if [ -r "\$path" ]; then printf 'COPY=readable\n'; else printf 'COPY=unreadable\n'; fi
  if [ -r "$SRC" ]; then printf 'SOURCE=visible\n'; else printf 'SOURCE=hidden\n'; fi
  if [ -r "\$HOME/.ssh" ]; then printf 'SSH=visible\n'; else printf 'SSH=hidden\n'; fi
  cat "\$path" > "${D}/copy" 2>/dev/null
  printf 'DONE\n'
} > "${D}/report"
sleep 60
EOF
    chmod +x "${D}/run.sh"
    out="$("$Rime" agent run --agent generic --sandbox project --network offline -d \
            --cwd "$PROJ" -- /bin/bash "${D}/run.sh" 2>&1)"
    E="$(printf '%s' "$out" | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
    if [ -z "$E" ]; then
        bad "a confined session started"
        printf '%s\n' "$out" | sed 's/^/      /'
    else
        ok "a confined session started (id ${E})"
        for _ in $(seq 1 150); do [ -e "${D}/ready" ] && break; sleep 0.1; done
        out="$("$Rime" agent send "$E" "$SRC" 2>&1)"
        printf '%s\n' "$out" | sed 's/^/      | /'
        for _ in $(seq 1 150); do
            grep -q '^DONE$' "${D}/report" 2>/dev/null && break
            sleep 0.1
        done
        sed 's/^/      | /' "${D}/report" 2>/dev/null

        grep -q "^GOT=${RIME_AGENT_SCRATCH_ROOT}/" "${D}/report" 2>/dev/null \
            && ok "the confined session received the path" \
            || bad "the confined session received the path"
        grep -q '^COPY=readable$' "${D}/report" 2>/dev/null \
            && ok "the confined session can open the copy" \
            || bad "the confined session can open the copy"
        # The half that makes the copy worth making. Without it this feature is
        # a long way round to a path the session already had.
        grep -q '^SOURCE=hidden$' "${D}/report" 2>/dev/null \
            && ok "the confined session cannot open the original" \
            || bad "the confined session cannot open the original"
        grep -q '^SSH=hidden$' "${D}/report" 2>/dev/null \
            && ok "the sandbox is the real one (~/.ssh is out of reach)" \
            || bad "the sandbox is the real one (~/.ssh is out of reach)"
        cmp -s "$SRC" "${D}/copy" 2>/dev/null \
            && ok "what the confined session read is the file that was sent" \
            || bad "what the confined session read is the file that was sent"
        "$Rime" agent kill "$E" --signal kill >/dev/null 2>&1
    fi
fi

# The real root is untouched: same entries as before, and no new ones. This is
# the assertion that would have failed on every version of this suite written
# before RIME_AGENT_SCRATCH_ROOT existed.
now_scratch="$(ls /tmp/rime-agent 2>/dev/null | sort -n | tr '\n' ' ')"
[ "$PRE_SCRATCH" = "$now_scratch" ] \
    && ok "the real /tmp/rime-agent was neither written to nor emptied" \
    || { bad "the real /tmp/rime-agent was neither written to nor emptied"
         echo "      before: ${PRE_SCRATCH}" >&2
         echo "      after:  ${now_scratch}" >&2; }

section "a session that has gone"
"$Rime" agent kill "$B" --signal kill >/dev/null 2>&1
for _ in $(seq 1 40); do
    "$Rime" agent list --all --json 2>/dev/null | python3 -c "
import json,sys
s=[x for x in json.load(sys.stdin) if x['id']==${B}][0]
sys.exit(0 if s['exit_code'] is not None or s['exit_signal'] is not None else 1)
" && break
    sleep 0.25
done
out="$("$Rime" agent send "$B" "$SRC" 2>&1)"
printf '%s' "$out" | pipe_has -i "already exited" \
    && ok "an exited session is refused, and told apart from a missing one" \
    || { bad "an exited session is refused, and told apart from a missing one"; printf '%s\n' "$out" | sed 's/^/      | /'; }

printf '\ninject: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
