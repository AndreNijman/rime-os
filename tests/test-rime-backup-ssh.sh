#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  `rime backup` over a REAL ssh, against a real sshd on loopback — P2-001.
#
#  The `ssh` target kind was refused by this build for one reason, recorded in
#  the source it was refused from:
#
#      a transport proven only against a fake `ssh` on PATH is a test of the
#      argument list and not of a backup.
#
#  So this suite runs a genuine `sshd` on 127.0.0.1, with a fixture host key and
#  a fixture client key, and drives the whole of `rime backup` through it: init,
#  run, list, verify, restore, and the compare of the restored tree against the
#  original. The chunks go up an actual ssh channel and come back down one.
#
#  ── WHAT IS NOT TOUCHED, AND WHY THAT IS STRUCTURAL ─────────────────────────
#
#  Andre's `~/.ssh`, his agent and every host key on this machine are OUT OF
#  BOUNDS, and nothing here depends on remembering that:
#
#    * the sshd runs UNPRIVILEGED, as whoever runs this suite, on a high port,
#      from a config file inside the fixture. It is started by pid and killed by
#      pid. No system unit is touched, no port below 1024 is bound, and
#      `/etc/ssh` is neither read for the host key nor written.
#    * every `ssh` this exercises is built by SshTarget::argv, which carries
#      `-F /dev/null`, `IdentityAgent=none`, `IdentitiesOnly=yes` and its own
#      `UserKnownHostsFile` — so the client cannot read `~/.ssh/config`, cannot
#      reach an agent, and cannot append to `~/.ssh/known_hosts`. One assertion
#      below proves that by running the whole thing with `HOME` pointed at an
#      empty directory AND with a poisoned `SSH_AUTH_SOCK` in the environment.
#    * the fixture key is generated here, lives in the fixture, and is deleted
#      with it. It authorises exactly one account — the one running this — to
#      log into the sshd this suite started.
#
#  ── SSHD ABSENT IS "COULD NOT RUN", NEVER A GREEN SKIP ──────────────────────
#
#  A suite that quietly passes on a machine without `sshd` would make the ssh
#  target look proven everywhere while being proven nowhere. So a missing sshd
#  exits 2 with COULD NOT RUN on stdout, which is neither the 0 of a pass nor
#  the 1 of a failure, and the CI step installs openssh-server so it never
#  happens there.
#
#      ./tests/test-rime-backup-ssh.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d -p /var/tmp rime-backup-ssh-XXXXXX)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

SSHD_PID=""
cleanup() {
    # By pid, on the child this suite started. Never by name: there may be a
    # real sshd on this machine and it is not ours to signal.
    [ -n "$SSHD_PID" ] && kill "$SSHD_PID" 2>/dev/null
    [ -n "$SSHD_PID" ] && wait "$SSHD_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

finish() {
    printf '\nbackup-ssh: %d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ] || exit 1
    exit 0
}

section "preconditions"

SSHD=""
for candidate in /usr/sbin/sshd /usr/bin/sshd; do
    [ -x "$candidate" ] && { SSHD="$candidate"; break; }
done
if [ -z "$SSHD" ]; then
    printf 'COULD NOT RUN  no sshd on this machine\n'
    printf '  The ssh backup target is exercised against a real sshd and\n'
    printf '  against nothing else, deliberately. Without one, NOTHING about\n'
    printf '  the ssh target was checked here — this is not a pass.\n'
    printf '  Install openssh-server and run this again.\n'
    exit 2
fi
ok "sshd is $SSHD"

for tool in ssh ssh-keygen cargo; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'COULD NOT RUN  %s is missing\n' "$tool"; exit 2; }
done
ok "ssh, ssh-keygen and cargo are present"

cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" --bin rime >/dev/null 2>&1 \
    || { bad "rime builds"; finish; }
ok "rime builds"
Rime="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug/rime"

export RIME_BACKUP_KEYS="${WORK}/keys"

# ── the loopback sshd ────────────────────────────────────────────────────────
section "a real sshd on loopback"

SSHDIR="${WORK}/sshd"
mkdir -p "$SSHDIR"
chmod 700 "$SSHDIR"

ssh-keygen -q -t ed25519 -N '' -f "${SSHDIR}/host_key" -C rime-backup-fixture-host \
    || { bad "a fixture host key"; finish; }
ssh-keygen -q -t ed25519 -N '' -f "${SSHDIR}/client_key" -C rime-backup-fixture-client \
    || { bad "a fixture client key"; finish; }
chmod 600 "${SSHDIR}/host_key" "${SSHDIR}/client_key"
cp "${SSHDIR}/client_key.pub" "${SSHDIR}/authorized_keys"
chmod 600 "${SSHDIR}/authorized_keys"
ok "a fixture host key and client key, inside the fixture"

# A free port, chosen by the kernel and then released. A race is possible and
# the retry below covers it; hard-coding a port would collide with whatever
# else is on this machine.
pick_port() {
    python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

start_sshd() {
    local port="$1"
    cat > "${SSHDIR}/sshd_config" <<EOF
Port ${port}
ListenAddress 127.0.0.1
HostKey ${SSHDIR}/host_key
PidFile none
# Unprivileged: this sshd never changes uid, so it only ever serves the account
# that started it, and needs none of the machinery that privilege separation and
# PAM exist for.
UsePAM no
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile ${SSHDIR}/authorized_keys
PermitUserEnvironment no
PrintMotd no
Subsystem sftp /bin/false
EOF
    "$SSHD" -t -f "${SSHDIR}/sshd_config" >"${SSHDIR}/config-check.log" 2>&1 || return 1
    "$SSHD" -D -e -f "${SSHDIR}/sshd_config" >"${SSHDIR}/sshd.log" 2>&1 &
    SSHD_PID=$!
    for _ in $(seq 1 100); do
        if python3 - "$port" <<'PY'
import socket, sys
s = socket.socket()
s.settimeout(0.2)
sys.exit(0 if s.connect_ex(("127.0.0.1", int(sys.argv[1]))) == 0 else 1)
PY
        then
            return 0
        fi
        kill -0 "$SSHD_PID" 2>/dev/null || return 1
        sleep 0.1
    done
    return 1
}

PORT=""
for _ in 1 2 3; do
    candidate="$(pick_port)"
    if start_sshd "$candidate"; then PORT="$candidate"; break; fi
    [ -n "$SSHD_PID" ] && kill "$SSHD_PID" 2>/dev/null
    SSHD_PID=""
done
if [ -z "$PORT" ]; then
    bad "the fixture sshd started"
    printf '      config check: %s\n' "$(cat "${SSHDIR}/config-check.log" 2>/dev/null)"
    printf '      sshd log: %s\n' "$(cat "${SSHDIR}/sshd.log" 2>/dev/null)"
    finish
fi
ok "the fixture sshd is listening on 127.0.0.1:${PORT} (pid ${SSHD_PID})"

# The host key pin. Built from the fixture host key's own public half rather
# than scanned off the wire, so the pin is what we generated and not what
# answered.
printf '[127.0.0.1]:%s %s\n' "$PORT" "$(cat "${SSHDIR}/host_key.pub")" \
    > "${SSHDIR}/known_hosts"
chmod 600 "${SSHDIR}/known_hosts"
ok "a known_hosts holding exactly the fixture host key"

# ── the project ──────────────────────────────────────────────────────────────
section "a project bound to an ssh host group"

REMOTE="${WORK}/remote"          # what the "far side" writes into
mkdir -p "$REMOTE"
PROJECT="${WORK}/project"
mkdir -p "${PROJECT}/src/deep"
printf 'fn main() {}\n' > "${PROJECT}/src/main.rs"
printf 'deep file\n' > "${PROJECT}/src/deep/note.txt"
# Binary, non-UTF-8, so the round trip is proved on bytes a text path would eat.
python3 -c "
import sys
sys.stdout.buffer.write(bytes(range(256)) * 64)
" > "${PROJECT}/blob.bin"
ln -sf src/main.rs "${PROJECT}/link"

"$Rime" backup key init >/dev/null 2>&1
RECIPIENT="$("$Rime" backup key show 2>/dev/null)"
[ -n "$RECIPIENT" ] || { bad "a backup key exists"; finish; }
ok "a backup keypair in the fixture"

write_config() {
    local host="$1"
    cat > "${PROJECT}/rime.toml" <<EOF
[backup]
recipient = "${RECIPIENT}"
target = "ssh"

[backup.ssh]
host = "${host}"
port = ${PORT}
path = "${REMOTE}"
identity = "${SSHDIR}/client_key"
known_hosts = "${SSHDIR}/known_hosts"

[identity.ssh]
host_group = "fixture"

[ssh.host_groups]
fixture = ["127.0.0.1"]
EOF
}
write_config 127.0.0.1
ok "rime.toml names the fixture host and binds a group containing it"

# ── §36, enforced ────────────────────────────────────────────────────────────
section "§36: a host outside the bound group"

write_config localhost
out="$("$Rime" backup init --project "$PROJECT" 2>&1)"
if [ $? -eq 0 ]; then
    bad "a host outside the bound group is refused"
else
    ok "a host outside the bound group is refused"
    printf '%s' "$out" | pipe_has "localhost" \
        && ok "the refusal names the host that was asked for" \
        || { bad "the refusal names the host that was asked for"; printf '      %s\n' "$out"; }
    printf '%s' "$out" | pipe_has "fixture" \
        && ok "and the group the project binds" || bad "and the group the project binds"
    printf '%s' "$out" | pipe_has "rime.toml" \
        && ok "and where to change it" || bad "and where to change it"
fi
# Nothing reached the far side: the refusal is at configuration time.
[ -z "$(ls -A "$REMOTE" 2>/dev/null)" ] \
    && ok "nothing was written to the far side before the refusal" \
    || bad "nothing was written to the far side before the refusal"

write_config 127.0.0.1

# ── the round trip, over a real ssh ──────────────────────────────────────────
section "init, run, list, verify, restore"

out="$("$Rime" backup init --project "$PROJECT" 2>&1)"
if [ $? -eq 0 ]; then
    ok "\`rime backup init\` reached the far side"
else
    bad "\`rime backup init\` reached the far side"
    printf '      %s\n' "$out"
    printf '      sshd log: %s\n' "$(tail -5 "${SSHDIR}/sshd.log" 2>/dev/null)"
    finish
fi

out="$("$Rime" backup run --project "$PROJECT" --label first 2>&1)"
if [ $? -eq 0 ]; then ok "\`rime backup run\` wrote a snapshot over ssh"
else bad "\`rime backup run\` wrote a snapshot over ssh"; printf '      %s\n' "$out"; finish; fi

# The objects really are on the "far side", under the prefix. `head.json` is
# written LAST, so its presence is also the evidence that the run completed.
# Materialised rather than piped, for the same reason as the canary check
# below, but failing in the opposite direction: `find … | grep -q .` dies of
# SIGPIPE under `pipefail` precisely when there IS a match, so a far side big
# enough to overflow the 64 KB pipe buffer would report the head MISSING while
# it was there — a false red rather than a hidden leak.
find "$REMOTE" -type f -name 'head.json' > "${WORK}/head-paths.txt"
[ -s "${WORK}/head-paths.txt" ] \
    && ok "a completed snapshot's head is on the far side" \
    || { bad "a completed snapshot's head is on the far side"
         find "$REMOTE" | head -10; }
[ "$(find "$REMOTE" -type f | wc -l)" -ge 3 ] \
    && ok "the snapshot's chunks are on the far side too" \
    || bad "the snapshot's chunks are on the far side too"

# ── the plaintext is not there ───────────────────────────────────────────────
#
# The same measurement the local suite makes, repeated here because the
# transport is different and a transport is exactly where a plaintext copy
# would get made.
CANARY="ssh-canary-3f9a2b7c41d85e60"
printf '%s\n' "$CANARY" > "${PROJECT}/secret.txt"
out="$("$Rime" backup run --project "$PROJECT" --label second 2>&1)"
[ $? -eq 0 ] || { bad "a second snapshot with a canary in it"; printf '      %s\n' "$out"; }
ok "a second snapshot, with a canary planted in the source"

found=0
while IFS= read -r f; do
    if grep -qaF "$CANARY" "$f" 2>/dev/null; then found=1; printf '      in %s\n' "$f"; fi
done < <(find "$REMOTE" -type f)
[ "$found" -eq 0 ] \
    && ok "no byte written over ssh holds the plaintext" \
    || bad "no byte written over ssh holds the plaintext"
# The object NAMES too, not only their contents — and materialised, never
# piped. `find TREE | grep -qF X` makes grep exit at its first match, find then
# dies of SIGPIPE, and under `pipefail` the PIPELINE is 141: this assertion
# reported "no object NAME … holds the plaintext" exactly WHEN one did, and
# passed only when the canary was absent and grep read to EOF. A negative leak
# assertion that fails OPEN. Measured in bash on a 1.36 MB listing: 5
# inversions in 5; over a 717-byte listing, correct 5 times in 5. The size of
# the far side, not the code, decided whether a leak could be detected.
find "$REMOTE" > "${WORK}/remote-names.txt"
grep -qF "$CANARY" "${WORK}/remote-names.txt" \
    && bad "no object NAME written over ssh holds the plaintext" \
    || ok "no object NAME written over ssh holds the plaintext"

# ── listing and restoring ────────────────────────────────────────────────────
listing="$("$Rime" backup list --project "$PROJECT" 2>&1)"
count="$(printf '%s' "$listing" | grep -cE '^[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}')"
[ "$count" -eq 2 ] \
    && ok "both snapshots are listed over ssh" \
    || { bad "both snapshots are listed over ssh (saw $count)"; printf '      %s\n' "$listing"; }

# `--deep` fetches and decrypts every chunk, so this is the assertion that the
# bytes come back off the far side and authenticate — not merely that the head
# parses.
out="$("$Rime" backup verify latest --project "$PROJECT" --deep 2>&1)"
[ $? -eq 0 ] && ok "\`rime backup verify --deep\` passes over ssh" \
    || { bad "\`rime backup verify --deep\` passes over ssh"; printf '      %s\n' "$out"; }

DEST="${WORK}/restored"
out="$("$Rime" backup restore latest --project "$PROJECT" --into "$DEST" 2>&1)"
restore_rc=$?
if [ "$restore_rc" -eq 0 ]; then
    ok "\`rime backup restore\` pulled the snapshot back over ssh"
    # The bytes, compared. `diff -r` follows nothing and compares content.
    if diff -r --no-dereference "$PROJECT" "$DEST" >"${WORK}/diff.txt" 2>&1; then
        ok "the restored tree is identical to the original, byte for byte"
    else
        # rime.toml and the key directory are not part of the compare in the
        # local suite either; show what differed so a real regression is
        # readable rather than a wall.
        if grep -vE 'rime\.toml' "${WORK}/diff.txt" | pipe_has .; then
            bad "the restored tree is identical to the original, byte for byte"
            head -20 "${WORK}/diff.txt"
        else
            ok "the restored tree is identical to the original, byte for byte"
        fi
    fi
    cmp -s "${PROJECT}/blob.bin" "${DEST}/blob.bin" \
        && ok "a non-UTF-8 file survived the ssh round trip byte for byte" \
        || bad "a non-UTF-8 file survived the ssh round trip byte for byte"
    [ -L "${DEST}/link" ] && ok "a symlink came back as a symlink" \
        || bad "a symlink came back as a symlink"
else
    bad "\`rime backup restore\` pulled the snapshot back over ssh"
    printf '      %s\n' "$out"
fi

# ── the client cannot reach the owner's ssh anything ─────────────────────────
section "the owner's ssh agent, config and known_hosts are unreachable"

# HOME points at an empty directory and SSH_AUTH_SOCK at a path that does not
# exist. If the client were reading `~/.ssh/config`, using an agent, or
# appending to `~/.ssh/known_hosts`, this run would behave differently from the
# one above — and the empty HOME would make it fail.
FAKEHOME="${WORK}/empty-home"
mkdir -p "$FAKEHOME"
out="$(HOME="$FAKEHOME" SSH_AUTH_SOCK="${WORK}/there-is-no-agent-here" \
    "$Rime" backup list --project "$PROJECT" 2>&1)"
if [ $? -eq 0 ]; then
    ok "a run with an empty HOME and a poisoned SSH_AUTH_SOCK works the same"
else
    bad "a run with an empty HOME and a poisoned SSH_AUTH_SOCK works the same"
    printf '      %s\n' "$out"
fi
[ -e "${FAKEHOME}/.ssh" ] \
    && bad "nothing was created under the home directory" \
    || ok "nothing was created under the home directory"

# ── the host key pin ─────────────────────────────────────────────────────────
section "a host key that is not the pinned one"

printf '[127.0.0.1]:%s %s\n' "$PORT" "$(ssh-keygen -q -t ed25519 -N '' \
    -f "${SSHDIR}/other_key" -C other >/dev/null 2>&1; cat "${SSHDIR}/other_key.pub")" \
    > "${SSHDIR}/wrong_known_hosts"
sed "s|known_hosts = .*|known_hosts = \"${SSHDIR}/wrong_known_hosts\"|" \
    "${PROJECT}/rime.toml" > "${PROJECT}/rime.toml.new"
mv "${PROJECT}/rime.toml.new" "${PROJECT}/rime.toml"

out="$("$Rime" backup list --project "$PROJECT" 2>&1)"
if [ $? -eq 0 ]; then
    bad "a host key that is not the pinned one refuses"
    printf '      %s\n' "$out"
else
    ok "a host key that is not the pinned one refuses"
    # And it is CouldNotRun — never "there are no backups".
    printf '%s' "$out" | pipe_has -iE "could not|host key|verification" \
        && ok "and says the far side was not reached, not that there are no backups" \
        || { bad "and says the far side was not reached, not that there are no backups"
             printf '      %s\n' "$out"; }
fi

# ── a far side that is not there ─────────────────────────────────────────────
section "a far side that does not answer"

kill "$SSHD_PID" 2>/dev/null
wait "$SSHD_PID" 2>/dev/null
SSHD_PID=""
sed "s|known_hosts = .*|known_hosts = \"${SSHDIR}/known_hosts\"|" \
    "${PROJECT}/rime.toml" > "${PROJECT}/rime.toml.new"
mv "${PROJECT}/rime.toml.new" "${PROJECT}/rime.toml"

out="$("$Rime" backup list --project "$PROJECT" 2>&1)"
if [ $? -eq 0 ]; then
    bad "a listing with the far side down is never an empty history"
    printf '      %s\n' "$out"
else
    ok "a listing with the far side down is never an empty history"
    printf '%s' "$out" | pipe_has -i "did not reach" \
        && ok "and names the connection as the thing that failed" \
        || { bad "and names the connection as the thing that failed"
             printf '      %s\n' "$out"; }
fi

finish
