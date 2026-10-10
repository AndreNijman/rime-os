#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  The root approval path (roadmap §7, P0-014 criterion 2).
#
#  §7's table ends both columns in the same place: "Root capability — Local:
#  local auth. Claude Remote Control: local approval required." The daemon side
#  of that is `privilege::decide`, which tests/test-privilege-requests.sh drives
#  against a real daemon, a real session and a real socket.
#
#  The CLI side was not tested at all, and its own evidence said so: approving a
#  request WITHOUT `--no-run` performs the operation, is gated on being root,
#  and testing it "needs a real sudo prompt". It does not. `unshare -r` gives a
#  process a real effective uid of 0 — the kernel's own answer, the one
#  `ops::effective_uid` reads out of /proc/self/status — and asks nobody for a
#  password. Nothing here raises a polkit or sudo prompt: no `pkexec`, no
#  `pkcheck`, and the one `sudo` is `sudo -n`, which by definition cannot ask.
#
#  ── AND IT NEEDS A LOGIN SESSION, WHICH IS NOT THE SAME AS A ROOT UID ───────
#
#  `unshare -r` answers "am I root". It does not answer "is a human at this
#  machine", and §7 asks both: `privilege::decide` refuses a non-local origin
#  before the euid is looked at. A user namespace does not change the cgroup,
#  so a runner that systemd started is still a `scheduled-job` inside it and
#  every assertion past the first approval is refused for a reason that has
#  nothing to do with the execution path being tested. Measured on 2026-09-08
#  in `…/user@1000.service/app.slice/x.scope`: **12 passed, 8 failed**, against
#  20/0 from a login session. So this suite re-enters itself through
#  tests/in-login-session.sh first, exactly as test-privilege-requests.sh
#  does. That costs one `sudo -n` and no prompt; see that file for what it
#  does when it cannot get a session, and for why it is not allowed to fake
#  one.
#
#  ── WHAT A USER NAMESPACE IS, AND WHAT IT IS NOT ────────────────────────────
#
#  Inside `unshare -r` the process is uid 0 in its own namespace and maps to
#  the invoking user outside it. Every check Rime makes about being root is a
#  real check that really passes. What it does NOT get is capability over
#  anything owned by real root — so the operation that runs afterwards has to
#  be one that cannot touch the machine. `unshare -m` gives this run its own
#  mount namespace, and a mirror of /usr/libexec carrying a stub `rime-pkg` is
#  bind-mounted over the real directory — /usr/libexec/rime-pkg is the absolute
#  path `rime install` execs. The directory rather than the file, because
#  `mount --bind` cannot create a target that is not there and only a Rime
#  machine has that file; see the inner half for what that cost. The mount is
#  invisible outside this process tree and the real engine is never run.
#
#  The two steps that genuinely need a person are named at the end, with the
#  exact commands and the exact output to expect.
#
#      ./tests/test-root-approval.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# Counts failures rather than aborting on them: several assertions run commands
# that exit non-zero on purpose. Under `-e` — which GitHub Actions applies by
# invoking a script as `bash -e {0}` — a `x="$(cmd)"` assignment whose command
# refuses would end the run and report every remaining assertion as a failure.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A local origin, before anything else — see the header. The inner half below
# is this same file re-executed inside the namespace and inherits both the
# session and the guard, so it must not try to re-enter: `sudo -n` from a uid 0
# that only exists inside a user namespace would fail anyway, and the wrapper
# would then print its degradation notice in the middle of the run.
if [ "${RIME_ROOT_APPROVAL_INNER:-}" != "1" ] \
   && [ -z "${RIME_LOGIN_SESSION_WRAPPED:-}" ] \
   && [ -x "${ROOT}/tests/in-login-session.sh" ]; then
    exec "${ROOT}/tests/in-login-session.sh" "${BASH_SOURCE[0]}" "$@"
fi

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

# ── the inner half, run again inside the namespace ───────────────────────────
#
# One file, re-executed. The alternative — a second script under tests/ that
# only ever runs through this one — is a file nobody remembers to keep in step
# with the outer half.
if [ "${RIME_ROOT_APPROVAL_INNER:-}" = "1" ]; then
    WORK="$RIME_ROOT_APPROVAL_WORK"
    Rime="$RIME_ROOT_APPROVAL_RIME"

    printf 'inner: euid %s, /proc/self/status says %s\n' \
        "$(id -u)" "$(awk '/^Uid:/{print $3}' /proc/self/status)"

    # The engine `rime install` execs, by absolute path. Replaced inside this
    # mount namespace only; the real engine is never run.
    #
    # The DIRECTORY is replaced rather than the file, and that is not
    # fastidiousness. `mount --bind` cannot create its own target, and on a
    # machine that is not Rime there is nothing at /usr/libexec/rime-pkg to
    # bind over. An ubuntu-24.04 runner answered
    #
    #     mount: /usr/libexec/rime-pkg: mount point does not exist.
    #
    # and took eight assertions down with it — every one of them about what
    # happens AFTER the engine is reached — while this laptop, where the image
    # ships the engine at that path, reported 20 passed and 0 failed. Green
    # here and red there for as long as the suite has existed.
    #
    # Creating the target is not open to this process either. It is uid 0 in
    # its OWN user namespace, mapped to an unprivileged uid outside it, so a
    # write into /usr/libexec is EACCES on a runner and EROFS on this machine,
    # where /usr is mounted read-only.
    #
    # So the real directory is bound aside, a mirror of it is built out of
    # symlinks, the stub is dropped in as `rime-pkg`, and the mirror is bound
    # over /usr/libexec. Nothing else in the directory disappears. There is one
    # code path rather than an `if the file is missing` branch, because a
    # branch only CI takes is how the original defect survived.
    if [ ! -d /usr/libexec ]; then
        echo "INNER-FATAL: /usr/libexec does not exist, so there is nowhere to put the stub engine"
        exit 3
    fi
    mkdir -p "${WORK}/real-libexec" "${WORK}/libexec"
    if ! mount --bind /usr/libexec "${WORK}/real-libexec" 2>"${WORK}/mount.err"; then
        echo "INNER-FATAL: could not bind the real /usr/libexec aside"
        sed 's/^/      /' "${WORK}/mount.err"
        exit 3
    fi
    # `rime-pkg` is deliberately NOT mirrored, and the `rm -f` after the loop
    # is not belt and braces. On a Rime machine the real engine IS in this
    # directory, so mirroring it would leave ${WORK}/libexec/rime-pkg a symlink
    # pointing at /usr/libexec/rime-pkg — and `cp` follows a symlink and writes
    # through it. Measured here: the copy failed only because this machine
    # mounts /usr read-only, which turned overwriting the shipped engine into
    # an INNER-FATAL. On any machine where /usr is writable — a GitHub runner
    # mounts it rw — a test suite would have overwritten the real
    # /usr/libexec/rime-pkg with a stub that exits 23.
    for entry in "${WORK}/real-libexec"/* "${WORK}/real-libexec"/.[!.]*; do
        [ -e "$entry" ] || continue
        [ "$(basename "$entry")" = "rime-pkg" ] && continue
        ln -sfn "$entry" "${WORK}/libexec/$(basename "$entry")"
    done
    rm -f "${WORK}/libexec/rime-pkg"
    cp "${WORK}/fake-rime-pkg" "${WORK}/libexec/rime-pkg"
    chmod 0755 "${WORK}/libexec/rime-pkg"
    if ! mount --bind "${WORK}/libexec" /usr/libexec 2>"${WORK}/mount.err"; then
        echo "INNER-FATAL: could not bind the stub engine over /usr/libexec"
        sed 's/^/      /' "${WORK}/mount.err"
        exit 3
    fi
    # Asked of the filesystem rather than inferred from mount(8) exiting 0.
    # ops::PKG_ENGINE is this path and nothing else; if it is not the stub now,
    # everything below would be measuring the real engine.
    if [ ! -x /usr/libexec/rime-pkg ] || ! cmp -s "${WORK}/fake-rime-pkg" /usr/libexec/rime-pkg; then
        echo "INNER-FATAL: /usr/libexec/rime-pkg is not the stub after the bind"
        exit 3
    fi

    id="$1"
    printf 'y\n' | "$Rime" request approve "$id" > "${WORK}/approve.out" 2>&1
    printf 'APPROVE_EXIT=%s\n' "$?"
    sed 's/^/      | /' "${WORK}/approve.out"
    exit 0
fi

WORK="$(mktemp -d)"
DAEMON_PID=""
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    [ -n "$DAEMON_PID" ] && { for _ in 1 2 3 4 5; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done; }
    [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
    rm -rf "$WORK"
    return 0
}
trap cleanup EXIT

# ── prerequisites ────────────────────────────────────────────────────────────
# A missing prerequisite is a FAILURE, never a skip. A suite that reports
# "0 passed, 0 failed" and exits 0 is a green tick over nothing asserted, and
# this repository has been bitten by that shape three times.
for tool in cargo unshare python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

# The one honest skip: a kernel with user namespaces switched off cannot give
# this run a real uid 0, and the alternative is a password prompt. Said out
# loud rather than passed silently.
if ! unshare -r true 2>/dev/null; then
    echo "SKIP: this kernel refuses unshare -r, so there is no way to reach a"
    echo "      real effective uid 0 without asking somebody for a password."
    exit 0
fi

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    printf '\nroot-approval: %d passed, %d failed\n' "$pass" "$fail"
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

# ── the stub engine ──────────────────────────────────────────────────────────
# It records who ran it and who its parent was, because that is the claim §3.3
# makes about this whole path: the daemon holds no privilege and never executes
# anything, so the operation must be run by the approving human's own process
# with the approving human's own root.
cat > "${WORK}/fake-rime-pkg" <<'STUB'
#!/usr/bin/env bash
{
    printf 'ARGV=%s\n' "$*"
    printf 'EUID=%s\n' "$(id -u)"
    printf 'PARENT=%s\n' "$(cat "/proc/$PPID/comm" 2>/dev/null)"
} >> "$RIME_ROOT_APPROVAL_WORK/engine.log"
exit 23
STUB
chmod +x "${WORK}/fake-rime-pkg"
: > "${WORK}/engine.log"

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
    printf '\nroot-approval: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ── without root ─────────────────────────────────────────────────────────────
section "an ordinary user cannot approve an operation that will run"

id="$("$Rime" request ask install clang --reason "the project needs a compiler" \
        --no-wait 2>"${WORK}/ask.err")"
if [ -n "$id" ]; then
    ok "a request was filed (id ${id})"
else
    bad "a request was filed"
    sed 's/^/      /' "${WORK}/ask.err"
    printf '\nroot-approval: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# Not root here, and this must be true for the rest of this section to mean
# anything. Asserted rather than assumed: a suite run as root would otherwise
# report the refusal arm as a pass it never took.
if [ "$(id -u)" != "0" ]; then
    ok "this half runs as an ordinary user (uid $(id -u))"
else
    bad "this half runs as an ordinary user — it is root, so the refusal below proves nothing"
fi

out="$(printf 'y\n' | "$Rime" request approve "$id" 2>&1)"
rc=$?
printf '%s' "$out" | pipe_has "must run as root" \
    && ok "approving without --no-run is refused" \
    || { bad "approving without --no-run is refused"; printf '      %s\n' "$out"; }
[ "$rc" -ne 0 ] && ok "and it exits non-zero" || bad "and it exits non-zero"
printf '%s' "$out" | pipe_has "sudo rime request approve ${id}" \
    && ok "the refusal names the exact command to run instead" \
    || { bad "the refusal names the exact command to run instead"; printf '      %s\n' "$out"; }
printf '%s' "$out" | pipe_has "wheel group is not enough" \
    && ok "and says why being in wheel does not do it" \
    || bad "and says why being in wheel does not do it"

# The refusal is BEFORE the socket call, which is the difference between a
# guard and a message. If the daemon had been asked, this would say approved.
"$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
r = [x for x in json.load(sys.stdin) if x['id'] == ${id}][0]
assert r['decision'] == 'pending', f'the daemon was asked anyway: {r}'
" 2>/dev/null \
    && ok "the request is still pending, so the daemon was never asked" \
    || bad "the request is still pending, so the daemon was never asked"

[ -s "${WORK}/engine.log" ] \
    && bad "nothing was executed" || ok "nothing was executed"

# And the gate is on the EXECUTION, not on the decision: the same user may
# record a decision that runs nothing. Without this the refusal above would be
# consistent with a CLI that simply cannot approve at all.
id2="$("$Rime" request ask install cmake --reason "and a build system" --no-wait 2>/dev/null)"
out="$(printf 'y\n' | "$Rime" request approve "$id2" --no-run 2>&1)"
printf '%s' "$out" | pipe_has "approved" \
    && ok "the same user may approve with --no-run, so the gate is on the execution" \
    || { bad "the same user may approve with --no-run, so the gate is on the execution"; printf '      %s\n' "$out"; }

# ── with a real effective uid 0 ──────────────────────────────────────────────
section "a real uid 0, reached without asking anybody for a password"

export RIME_ROOT_APPROVAL_INNER=1
export RIME_ROOT_APPROVAL_WORK="$WORK"
export RIME_ROOT_APPROVAL_RIME="$Rime"
inner="$(unshare -r -m --propagation private "${BASH_SOURCE[0]}" "$id" 2>&1)"
unset RIME_ROOT_APPROVAL_INNER
printf '%s\n' "$inner" | sed 's/^/      /'

printf '%s' "$inner" | pipe_has "INNER-FATAL" && {
    bad "the namespace came up with a stub engine bound over the real one"
}
printf '%s' "$inner" | pipe_has "inner: euid 0, /proc/self/status says 0" \
    && ok "inside the namespace the kernel reports effective uid 0" \
    || bad "inside the namespace the kernel reports effective uid 0"

printf '%s' "$inner" | pipe_has "must run as root" \
    && bad "the root gate let a real uid 0 through" \
    || ok "the root gate let a real uid 0 through"

printf '%s' "$inner" | pipe_has -E "rime request: running: .*install clang" \
    && ok "and the approval went on to run the operation" \
    || bad "and the approval went on to run the operation"

"$Rime" request list --all --json 2>/dev/null | python3 -c "
import json,sys
r = [x for x in json.load(sys.stdin) if x['id'] == ${id}][0]
assert r['decision'] == 'allow_once', f'not approved: {r}'
assert r['executed_ms'] is not None, f'no execution recorded: {r}'
assert r['exit_code'] == 23, f\"the engine's exit code was not recorded: {r}\"
" 2>"${WORK}/rec.err" \
    && ok "the decision, the execution and the engine's exit code are all recorded" \
    || { bad "the decision, the execution and the engine's exit code are all recorded"
         sed 's/^/      /' "${WORK}/rec.err"; }

# What the engine saw. The argv is rebuilt from the TYPED verb, so it is
# `install clang` and nothing else; the euid is 0, so the operation really did
# run with privilege; and the parent is the `rime` CLI rather than
# `rime-agentd`, which is §3.3's whole point — the daemon holds no privilege
# and executes nothing, so the root exercised here is the approving human's.
grep -q '^ARGV=install clang$' "${WORK}/engine.log" \
    && ok "the engine was called with the argv rebuilt from the typed verb" \
    || { bad "the engine was called with the argv rebuilt from the typed verb"
         sed 's/^/      /' "${WORK}/engine.log"; }
grep -q '^EUID=0$' "${WORK}/engine.log" \
    && ok "it ran as root" || bad "it ran as root"
grep -q '^PARENT=rime$' "${WORK}/engine.log" \
    && ok "its parent is the rime CLI, not the unprivileged daemon" \
    || { bad "its parent is the rime CLI, not the unprivileged daemon"
         sed 's/^/      /' "${WORK}/engine.log"; }

# Exactly one execution. A gate that ran the operation on the refused attempt
# as well would leave two.
n="$(grep -c '^ARGV=' "${WORK}/engine.log")"
[ "$n" = "1" ] && ok "the engine ran exactly once across both attempts" \
    || bad "the engine ran ${n} times across both attempts, not once"

# ── and the decision cannot be taken twice ───────────────────────────────────
out="$(printf 'y\n' | "$Rime" request approve "$id" --no-run 2>&1)"
printf '%s' "$out" | pipe_has "already" \
    && ok "an executed request cannot be approved again" \
    || { bad "an executed request cannot be approved again"; printf '      %s\n' "$out"; }

# ── where this stops ─────────────────────────────────────────────────────────
section "the boundary, and the one-minute procedure past it"
cat <<'BOUNDARY'
      Everything above ran with a real effective uid 0, established by the
      kernel and read the way ops::effective_uid reads it. Two things are
      still outside it, and no test can reach either without a person:

        1. sudo's own authentication. Whether `sudo` accepts this user's
           password is PAM's business and pam_unix's, not Rime's; Rime only
           refuses when euid is not 0 and says what to type.
        2. the operation running with capability over the real machine. A
           namespace root cannot write /var/lib or talk to systemd, so the
           engine here is a stub. What was proved is that the approval reaches
           the engine, with the right argv, as root, from the CLI.

      A person closes both in about a minute, on a machine where installing a
      package is acceptable:

        rime request ask install cowsay --reason "checking the approval path" --no-wait
        rime request pending                  # the id, the argv, the reason
        sudo rime request approve <id>        # sudo asks for the password HERE

      Expect, in order:
        * sudo's own password prompt, in the terminal, before anything else;
        * the confirmation prompt showing `rime install cowsay`, its reason
          and its effect, answered y;
        * `rime request: running: /usr/bin/rime install cowsay`, then the real
          engine's own output;
        * `rime request audit` ending in a line whose event is `executed` and
          whose exit code is the engine's.

      Answering the sudo prompt wrongly must end at sudo, with no Rime line
      after it and the request still `pending` in `rime request pending`.
BOUNDARY

printf '\nroot-approval: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
