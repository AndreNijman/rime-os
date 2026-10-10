#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  The AT-REST half of P0-002's first criterion, measured (roadmap §11).
#
#  P0-002 criterion 1 is "secrets are no longer stored in agent-readable home
#  paths". Two halves:
#
#    * the PATH half — the store is /var/lib/rime-secretd and not $HOME. That
#      is a unit test (`paths::the_defaults_are_outside_home`) and it has been
#      true since the crate landed.
#    * the AT-REST half — a process running as the owner's uid cannot open the
#      file. That is NOT a property of the path. It is a property of the mode
#      bits and of the uid the daemon runs as, and it had never been measured:
#      the item's evidence said so in as many words, "the daemon has never
#      actually run as root in testing, so the at-rest half of criterion 1 is
#      argued from the unit file rather than measured".
#
#  tests/test-secret-broker.sh starts rime-secretd as the invoking user and
#  says in its own header that this costs it the at-rest half. This suite is
#  that half, and nothing else: it starts the SAME binary as real root.
#
#  ── WHY REAL ROOT, AND NOT `unshare -r` ─────────────────────────────────────
#
#  tests/test-root-approval.sh uses `unshare -r`, correctly, because the
#  question there is "does this process believe it is root". Here the question
#  is the opposite one — whether the OWNER's uid is kept out — and a user
#  namespace answers it backwards. Inside `unshare -r` a file written by "root"
#  is owned by the invoking user outside the namespace, so the very account the
#  boundary excludes would own every byte of the store. A test that passed
#  under `unshare -r` would be a test of nothing.
#
#  So: `sudo -n`, which by definition cannot prompt. If passwordless sudo is
#  not available the suite SKIPS rather than degrading into the namespace
#  version, and says which assertions were not made.
#
#  ── WHAT IS AND IS NOT TOUCHED ──────────────────────────────────────────────
#
#  Nothing outside a fresh mktemp directory. `--store` and `--socket` point the
#  daemon there; the real /var/lib/rime-secretd and the shipped unit are never
#  started, stopped or read. The one root-owned thing created is the store, and
#  cleanup removes it with `sudo -n rm -rf` on exactly that path.
#
#      ./tests/test-secret-at-rest.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# Counts failures rather than aborting: several assertions run commands that
# are SUPPOSED to fail (that is the whole point of an EACCES test), and under
# `bash -e {0}` — which GitHub Actions uses — the first of them would end the
# run and report the rest as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

SECRETD_PID=""
AGENTD_PID=""
STALL_PID=""
USE_PID=""
STORE=""
cleanup() {
    # By recorded pid, never by name: the developer's own daemons are usually
    # running and a suite that killed by name has taken one down before.
    if [ -n "$SECRETD_PID" ]; then
        sudo -n kill "$SECRETD_PID" 2>/dev/null
        for _ in 1 2 3 4 5; do
            sudo -n kill -0 "$SECRETD_PID" 2>/dev/null || break
            sleep 0.2
        done
        sudo -n kill -9 "$SECRETD_PID" 2>/dev/null
    fi
    for pid in "$AGENTD_PID" "$STALL_PID" "$USE_PID"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null
    done
    # Root-owned, so the user's own rm cannot clear it.
    [ -n "$STORE" ] && sudo -n rm -rf "$STORE" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

section "preconditions"
if ! sudo -n true 2>/dev/null; then
    printf 'SKIP  passwordless sudo is not available, and this suite is only meaningful as\n'
    printf '      real root. NOT ASSERTED: store ownership, store mode, the owner uid\n'
    printf '      being refused by the kernel, and the per-child privilege drop.\n'
    printf '\nsecret-at-rest: 0 passed, 0 failed, SKIPPED\n'
    exit 0
fi
ok "sudo -n works, so the daemon can be started as real root without a prompt"

ME="$(id -u)"
if [ "$ME" = "0" ]; then
    printf 'SKIP  this suite is running AS root, so "the owner cannot read it" has no\n'
    printf '      subject — root can read everything. Run it as an ordinary user.\n'
    printf '\nsecret-at-rest: 0 passed, 0 failed, SKIPPED\n'
    exit 0
fi
ok "running as uid ${ME}, which is the account the boundary has to exclude"

section "the binaries"
cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
    --bin rime-secretd --bin rime-agentd --bin rime >/dev/null 2>&1 || {
    bad "rime-secretd, rime-agentd and rime build"
    printf '\nsecret-at-rest: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }
ok "rime-secretd, rime-agentd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
SECRETD="${BIN}/rime-secretd"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

SENTINEL="rime-atrest-2b7d40e9-do-not-leak"

section "the service, as real root"
export RIME_SECRETD_SOCKET="${WORK}/secretd.sock"
STORE="${WORK}/store"
# Every rime-secretd already on this machine, recorded BEFORE the spawn. There
# are none on the L16, but a developer running the shipped system unit would
# make `pgrep -x rime-secretd` return two pids, and picking one by sort order
# would read the SYSTEM daemon's uid and store while claiming to describe this
# one's. The difference is invisible in the output — both are root — so the set
# is taken first and the new pid is the one that was not in it.
PRE_SECRETD="$(pgrep -x rime-secretd 2>/dev/null | tr '\n' ' ')"
# setsid so the daemon is not in this script's process group and a stray
# Ctrl-C cannot take it down before cleanup records what it found.
# shellcheck disable=SC2024  # the redirect is this shell's, deliberately.
# "${WORK}" is this suite's own `mktemp -d`, owned by the invoking uid, and the
# whole point of the suite is to measure what that uid can and cannot read. A
# log the test user could not open would defeat it; `sudo tee` would make the
# log root-owned and the cleanup unable to remove it.
sudo -n setsid "$SECRETD" --socket "$RIME_SECRETD_SOCKET" --store "$STORE" \
    > "${WORK}/secretd.log" 2>&1 &
for _ in $(seq 1 100); do [ -S "$RIME_SECRETD_SOCKET" ] && break; sleep 0.1; done
# `pgrep -x`, matching the process NAME, not `pgrep -f` over the command line.
# `sudo -n setsid rime-secretd --socket X` leaves sudo resident with that exact
# string in its own argv, so `pgrep -f … | head -n1` returns SUDO — whose
# effective uid is also 0. The uid assertion below would then have been reading
# the uid of sudo rather than of the daemon, i.e. assuming from the fact that
# sudo was typed the very thing it claims to measure from the kernel.
SECRETD_PID=""
for _p in $(pgrep -x rime-secretd 2>/dev/null); do
    case " ${PRE_SECRETD} " in *" ${_p} "*) continue ;; esac
    SECRETD_PID="$_p"
done
[ -S "$RIME_SECRETD_SOCKET" ] || {
    bad "the secret service came up as root"
    sed 's/^/      /' "${WORK}/secretd.log"
    printf '\nsecret-at-rest: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }
ok "the secret service came up as root"

# The uid it is ACTUALLY running as, from the kernel, not from the fact that
# sudo was typed. This is the premise every assertion below rests on, so it is
# read rather than assumed.
daemon_comm="$(ps -o comm= -p "$SECRETD_PID" 2>/dev/null | tr -d ' ')"
[ "$daemon_comm" = "rime-secretd" ] \
    && ok "the pid being measured is rime-secretd itself, not the sudo that started it" \
    || bad "the pid being measured is rime-secretd itself (comm is '${daemon_comm}')"
daemon_uid="$(ps -o uid= -p "$SECRETD_PID" 2>/dev/null | tr -d ' ')"
[ "$daemon_uid" = "0" ] \
    && ok "the daemon's real uid is 0" \
    || bad "the daemon's real uid is 0 (is '${daemon_uid}')"

# And it says so on the wire, which is what stops a test instance from looking
# like a boundary. `warn_if_unprotected` prints only when protected is false.
warn="$("$Rime" secret list 2>&1 >/dev/null)"
printf '%s' "$warn" | pipe_has "not running as root" \
    && bad "the service reports itself protected" \
    || ok "the service reports itself protected"

section "storing a credential as the owner"
printf %s "$SENTINEL" | "$Rime" secret add demo --host 127.0.0.1 >/dev/null 2>&1
"$Rime" secret list 2>/dev/null | pipe_has demo \
    && ok "the credential was stored through the socket" \
    || { bad "the credential was stored through the socket"
         sed 's/^/      /' "${WORK}/secretd.log"; }

# ── the at-rest boundary ─────────────────────────────────────────────────────
#
# Read with sudo, because the whole claim is that this account cannot.
section "at rest: what the kernel says about the store"

store_stat="$(sudo -n stat -c '%a %u' "$STORE" 2>/dev/null)"
[ "$store_stat" = "700 0" ] \
    && ok "the store root is 0700 root-owned (is '${store_stat}')" \
    || bad "the store root is 0700 root-owned (is '${store_stat}')"

userdir="${STORE}/users/${ME}"
user_stat="$(sudo -n stat -c '%a %u' "$userdir" 2>/dev/null)"
[ "$user_stat" = "700 0" ] \
    && ok "the owner's subdirectory is 0700 root-owned (is '${user_stat}')" \
    || bad "the owner's subdirectory is 0700 root-owned (is '${user_stat}')"

# The file that actually holds the bytes. Named from the store's own layout
# rather than guessed: <service>.secret beside <service>.json.
secret_file="${userdir}/demo.secret"
sudo -n test -f "$secret_file" \
    && ok "the value is in a file of its own, not a field in the metadata" \
    || bad "the value is in a file of its own, not a field in the metadata"

value_stat="$(sudo -n stat -c '%a %u' "$secret_file" 2>/dev/null)"
[ "$value_stat" = "600 0" ] \
    && ok "the value file is 0600 root-owned (is '${value_stat}')" \
    || bad "the value file is 0600 root-owned (is '${value_stat}')"

# THE assertion. Not a mode bit read back — the kernel refusing this account.
section "at rest: the owner's own uid is refused by the kernel"

err="$(cat "$secret_file" 2>&1 >/dev/null)"
rc=$?
[ "$rc" != 0 ] && printf '%s' "$err" | pipe_has -i "permission denied" \
    && ok "opening the value file as uid ${ME} is denied" \
    || bad "opening the value file as uid ${ME} is denied (rc=${rc}, said '${err}')"

# And the directory cannot even be listed, so the account cannot enumerate
# which services exist by name. A 0600 file inside a 0755 directory would pass
# the check above and still leak the list.
err="$(ls "$userdir" 2>&1 >/dev/null)"
rc=$?
[ "$rc" != 0 ] && printf '%s' "$err" | pipe_has -i "permission denied" \
    && ok "listing the owner's store directory as uid ${ME} is denied" \
    || bad "listing the owner's store directory as uid ${ME} is denied (rc=${rc})"

# The audit trail is behind the same boundary. It was user-writable in the old
# broker, which meant a session could use a credential and then rewrite the
# record of having done so.
audit="${STORE}/audit.jsonl"
if sudo -n test -f "$audit"; then
    audit_stat="$(sudo -n stat -c '%a %u' "$audit" 2>/dev/null)"
    [ "$audit_stat" = "600 0" ] \
        && ok "the audit trail is 0600 root-owned (is '${audit_stat}')" \
        || bad "the audit trail is 0600 root-owned (is '${audit_stat}')"
    printf '%s' "$SENTINEL" > /dev/null
    # `2>&1 >>file`, not `>>file 2>&1`. The order is the whole point and this
    # line had it backwards: with the append applied first the capture is handed
    # the file rather than the diagnostic, so $err came back empty on every run.
    # Its two siblings above — the value file and the user directory — have
    # always had it the right way round.
    err="$(printf 'forged\n' 2>&1 >> "$audit")"
    rc=$?
    [ "$rc" != 0 ] \
        && ok "the audited account cannot append to its own audit trail" \
        || bad "the audited account cannot append to its own audit trail (rc=${rc}, said '${err}')"
else
    ok "no audit entry was written by a store-only operation (nothing was used)"
fi

# Nothing this account CAN read may contain the credential. The grep is over
# everything under $WORK that is readable as this uid — which is the whole of
# what an agent running as the owner could reach.
section "at rest: nothing readable by the owner contains the credential"
hits="$(grep -rl "$SENTINEL" "$WORK" 2>/dev/null | tr '\n' ' ')"
[ -z "$hits" ] \
    && ok "the credential appears in nothing this account can read" \
    || bad "the credential is readable at: ${hits}"

# ── the privilege drop ───────────────────────────────────────────────────────
#
# The other half of running as root: the daemon must NOT perform the operation
# as root. §11's reason is a local root escalation — git executes the
# repository's own configuration, and the repository belongs to the user.
section "the operation runs as the owner, not as root"

# `rime secret use` goes through rime-agentd as well as rime-secretd — the
# agent runtime owns the session and the project and forwards the capability
# record. Started as the ordinary user, which is what it always is.
"$AGENTD" > "${WORK}/agentd.log" 2>&1 &
AGENTD_PID=$!
AGENT_SOCK="${XDG_RUNTIME_DIR}/rime-agentd/control.sock"
for _ in $(seq 1 100); do [ -S "$AGENT_SOCK" ] && break; sleep 0.1; done

# A loopback listener that ACCEPTS and then stalls. The child has to still be
# alive to be looked at, and a fetch that completed — or one refused before git
# ran — would leave nothing to measure. No network: 127.0.0.1 on a port the
# kernel picked, talking to a socket this script owns.
cat > "${WORK}/stall.py" <<'PYEOF'
import socket, sys, time
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(8)
sys.stdout.write(str(s.getsockname()[1]) + "\n")
sys.stdout.flush()
while True:
    conn, _ = s.accept()
    time.sleep(30)
PYEOF
python3 "${WORK}/stall.py" > "${WORK}/port" 2>/dev/null &
STALL_PID=$!
for _ in $(seq 1 50); do [ -s "${WORK}/port" ] && break; sleep 0.1; done
PORT="$(cat "${WORK}/port" 2>/dev/null)"

PROJ="${WORK}/demo"
mkdir -p "$PROJ"
git -C "$PROJ" init -q
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
git -C "$PROJ" remote add origin "http://127.0.0.1:${PORT}/demo.git"

# `--scheme http` is accepted only for a loopback host, which this is. A
# second service rather than reusing `demo`, whose scheme is https.
printf %s "$SENTINEL" | "$Rime" secret add loop --host 127.0.0.1 --scheme http \
    --port "$PORT" >/dev/null 2>&1
(cd "$PROJ" && "$Rime" secret grant loop git.fetch >/dev/null 2>&1)
(cd "$PROJ" && timeout 25 "$Rime" secret use loop git.fetch origin \
    > "${WORK}/use.log" 2>&1) &
USE_PID=$!

# The child's uid, from the kernel, while it is running. Matched by exact
# process name — `pgrep -f git` also matches this script's own shell, which is
# how an earlier version of this assertion "found" a uid-1000 process that was
# not the child at all.
child_uid=""
for _ in $(seq 1 120); do
    for p in $(pgrep -x git-remote-http 2>/dev/null; pgrep -x git 2>/dev/null); do
        u="$(ps -o uid= -p "$p" 2>/dev/null | tr -d ' ')"
        [ -n "$u" ] && child_uid="$u" && break 2
    done
    sleep 0.2
done

if [ -n "$child_uid" ]; then
    [ "$child_uid" = "$ME" ] \
        && ok "the git child runs as uid ${ME}, so the root daemon dropped for it" \
        || bad "the git child runs as uid ${child_uid}, not ${ME} — the drop did not happen"
else
    bad "a git child was observed at all (see ${WORK}/use.log)"
    sed 's/^/      /' "${WORK}/use.log" 2>/dev/null | head -5
fi

# The complement, and the one that would be a real defect on a user's machine:
# a root-owned path inside the user's own repository is one they cannot fix.
kill "$USE_PID" 2>/dev/null
wait "$USE_PID" 2>/dev/null
rootowned="$(find "${PROJ}" -uid 0 -print -quit 2>/dev/null)"
[ -z "$rootowned" ] \
    && ok "the operation left nothing root-owned in the user's repository" \
    || bad "the operation left a root-owned path: ${rootowned}"

printf '\nsecret-at-rest: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
