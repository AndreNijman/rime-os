#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  `rime backup` end to end — P2-001 and P2-002 (roadmap §13.5, §36).
#
#  Two things this suite exists to MEASURE rather than assert, because both are
#  claims that are easy to make and easy to be wrong about:
#
#    * **the bytes on the target are not the plaintext.** A high-entropy canary
#      is planted in the source tree and every byte written to the target —
#      chunk, manifest, head, and the object NAMES — is searched for it. A
#      backup tool that quietly stored plaintext would pass every unit test in
#      the crate and fail here.
#    * **the private key is unreadable by the account that owns the backups.**
#      Measured as REAL ROOT with `sudo -n`, for the reason
#      tests/test-secret-at-rest.sh gives at length: a key written by a process
#      that is only pretending to be root is owned by the very account the
#      boundary excludes, and the assertion would pass for exactly the reason it
#      exists to rule out. Without passwordless sudo this suite SKIPS that half
#      and says which assertions were not made.
#
#  ── WHY THERE IS NO R2 SECTION HERE ─────────────────────────────────────────
#
#  Not an omission, and not a live call left out. `Api::loopback` in
#  rimed/rime-secretd/src/providers/cloudflare/api.rs is `#[cfg(test)]`, so a
#  SHIPPED rime-secretd always addresses api.cloudflare.com; and even if it did
#  not, service.rs's host pin refuses a credential stored for one host being
#  sent to another. There is no way to aim the shipped daemon at a double, which
#  is why every Cloudflare unit in this repository is proven in Rust and none of
#  them has a shell suite. The backup framework's own R2 call sequence is proven
#  the same way, against the same double that 401s an unauthenticated request:
#
#      cargo test -p rime-secretd a_backups_chunk_reaches_r2
#      cargo test -p rime-secretd a_backup_to_a_bucket_this_project_did_not_bind
#      cargo test -p rime-backup-core target::r2
#
#  ── WHAT IS AND IS NOT TOUCHED ──────────────────────────────────────────────
#
#  Nothing outside a fresh `mktemp -d -p /var/tmp`. RIME_BACKUP_KEYS points the
#  key directory inside it, so the real /var/lib/rime-backup is never read,
#  written or created. A restore only ever writes into a directory under the
#  same fixture. The one root-owned thing created is the root key fixture, and
#  cleanup removes it with `sudo -n rm -rf` on exactly that path.
#
#      ./tests/test-rime-backup.sh [--with-binary]
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# Counts failures rather than aborting: several assertions run commands that are
# SUPPOSED to fail, and under `bash -e {0}` — which GitHub Actions uses — the
# first of them would end the run and report the rest as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# /var/tmp and not /tmp: a backup fixture is megabytes and /tmp is a tmpfs
# sized for a desktop.
WORK="$(mktemp -d -p /var/tmp rime-backup-suite-XXXXXX)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }
section() { printf '\n── %s ──\n' "$1"; }

ROOT_KEYS=""
cleanup() {
    # Root-owned, so the user's own rm cannot clear it.
    [ -n "$ROOT_KEYS" ] && sudo -n rm -rf "$ROOT_KEYS" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

finish() {
    printf '\nbackup: %d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ] || exit 1
    exit 0
}

section "preconditions"
for tool in cargo python3 grep; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" --bin rime >/dev/null 2>&1 || {
    bad "rime builds"; finish; }
ok "rime builds"
Rime="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug/rime"

# The key directory, inside the fixture. NOT a supported way to run the real
# thing: what protects a backup key is the directory's ownership, and this one
# is owned by whoever runs the suite.
export RIME_BACKUP_KEYS="${WORK}/keys"

# ── the vocabulary ───────────────────────────────────────────────────────────
section "the five target kinds"
targets="$("$Rime" backup targets 2>&1)"
for kind in local nas ssh s3 r2; do
    printf '%s' "$targets" | pipe_has -w "$kind" \
        && ok "\`backup targets\` names ${kind}" \
        || { bad "\`backup targets\` names ${kind}"; printf '      %s\n' "$targets"; }
done
# All five are carried as of P2-001's second round. `ssh` and `s3` refused
# before it; the whole of ssh is now exercised by tests/test-rime-backup-ssh.sh
# against a real sshd, and the whole of s3 by tests/test-rime-backup-s3.sh
# against a loopback double that verifies the SigV4 signature independently.
for kind in local nas ssh s3 r2; do
    printf '%s' "$targets" | grep -E "^  ${kind} +carried" -q \
        && ok "${kind} is carried" || bad "${kind} is carried"
done
# And none of them still prints a refusal it does not mean, which would send an
# operator away from a target that works.
printf '%s' "$targets" | pipe_has "REFUSED" \
    && { bad "no kind still prints a refusal"; printf '%s\n' "$targets"; } \
    || ok "no kind still prints a refusal"

# ── a key ────────────────────────────────────────────────────────────────────
section "the keypair"
init_out="$("$Rime" backup key init 2>&1)"
rc=$?
[ "$rc" -eq 0 ] && ok "\`backup key init\` makes a key" || {
    bad "\`backup key init\` makes a key"; printf '      %s\n' "$init_out"; finish; }

RECIPIENT="$(printf '%s' "$init_out" | sed -n 's/^recipient = "\(.*\)"$/\1/p')"
[ -n "$RECIPIENT" ] && ok "it prints the line to paste into rime.toml" \
                    || { bad "it prints the line to paste into rime.toml"; printf '%s\n' "$init_out"; finish; }
case "$RECIPIENT" in
    apexbk1*) ok "the recipient has the documented prefix" ;;  # rime-rename: keep (RECIPIENT_PREFIX)
    *) bad "the recipient has the documented prefix (is '${RECIPIENT}')" ;;
esac
[ "$("$Rime" backup key show 2>/dev/null)" = "$RECIPIENT" ] \
    && ok "\`backup key show\` prints the registered recipient" \
    || bad "\`backup key show\` prints the registered recipient"

# Replacing a key makes every snapshot sealed to the old one unopenable.
again="$("$Rime" backup key init 2>&1)"
[ $? -ne 0 ] && ok "a second \`key init\` refuses rather than replacing the key" \
             || bad "a second \`key init\` refuses rather than replacing the key"
printf '%s' "$again" | pipe_has "unopenable" \
    && ok "its refusal says what would have been lost" \
    || bad "its refusal says what would have been lost"

# The private half is not world-readable even in a fixture.
mode="$(stat -c '%a' "${RIME_BACKUP_KEYS}/keys/$(id -u).key" 2>/dev/null)"
[ "$mode" = "600" ] && ok "the private half is mode 600 (is ${mode})" \
                    || bad "the private half is mode 600 (is ${mode})"
kmode="$(stat -c '%a' "${RIME_BACKUP_KEYS}/keys" 2>/dev/null)"
[ "$kmode" = "700" ] && ok "the directory holding it is mode 700 (is ${kmode})" \
                     || bad "the directory holding it is mode 700 (is ${kmode})"

# ── a project and a target ───────────────────────────────────────────────────
section "a project, a target and a canary"
PROJ="${WORK}/project"
TARGET="${WORK}/target"
mkdir -p "${PROJ}/src/deep" "${PROJ}/node_modules/junk" "$TARGET"

# High-entropy and fixed, so a failure is greppable and cannot match by luck.
CANARY="rime-backup-canary-4c19e7f2b83a06d5-do-not-leak"
printf '# the project\n' > "${PROJ}/README.md"
printf 'fn main() { println!("%s"); }\n' "$CANARY" > "${PROJ}/src/main.rs"
printf 'deep notes\n' > "${PROJ}/src/deep/notes.txt"
printf 'megabytes of nothing\n' > "${PROJ}/node_modules/junk/big.bin"
ln -s src/main.rs "${PROJ}/link-to-main"

cat > "${PROJ}/rime.toml" <<TOML
[backup]
recipient = "${RECIPIENT}"
target = "local"
exclude = ["node_modules"]

[backup.local]
path = "${TARGET}"
TOML

init_target="$("$Rime" backup init --project "$PROJ" 2>&1)"
[ $? -eq 0 ] && ok "\`backup init\` marks the target" \
             || { bad "\`backup init\` marks the target"; printf '      %s\n' "$init_target"; finish; }
MARKER="$(printf '%s' "$init_target" | sed -n 's/^id = "\(.*\)"$/\1/p')"
[ -n "$MARKER" ] && ok "it prints the marker id to record" || bad "it prints the marker id to record"
[ -f "${TARGET}/rime-backup-target.json" ] \
    && ok "the marker file is on the target" || bad "the marker file is on the target"
printf 'id = "%s"\n' "$MARKER" >> "${PROJ}/rime.toml"

# ── a run ────────────────────────────────────────────────────────────────────
section "taking a snapshot"
run_out="$("$Rime" backup run --project "$PROJ" --label nightly 2>&1)"
[ $? -eq 0 ] && ok "\`backup run\` takes a snapshot" \
             || { bad "\`backup run\` takes a snapshot"; printf '      %s\n' "$run_out"; finish; }
SNAP="$(printf '%s' "$run_out" | sed -n 's/^snapshot \([0-9A-Za-z-]*\) .*/\1/p')"
[ -n "$SNAP" ] && ok "it names the snapshot it made (${SNAP})" || bad "it names the snapshot it made"

SNAPDIR="${TARGET}/apex-backup/${SNAP}"  # rime-rename: keep (DEFAULT_PREFIX)
[ -f "${SNAPDIR}/head.json" ] && ok "the snapshot has a head" || bad "the snapshot has a head"
[ -f "${SNAPDIR}/data.000000" ] && ok "it has a data chunk" || bad "it has a data chunk"
[ -f "${SNAPDIR}/manifest.000000" ] && ok "it has a manifest chunk" || bad "it has a manifest chunk"

# ── THE MEASUREMENT ──────────────────────────────────────────────────────────
section "encrypted at rest, measured"
# Every byte of every object, plus every path, searched for the canary.
if grep -rqa -- "$CANARY" "$TARGET"; then
    bad "no byte written to the target holds the canary"
    printf '      the canary is in:\n'
    grep -rla -- "$CANARY" "$TARGET" | sed 's/^/        /'
else
    ok "no byte written to the target holds the canary"
fi
grep -rqa -- "# the project" "$TARGET" \
    && bad "no file's contents reach the target in the clear" \
    || ok "no file's contents reach the target in the clear"

# The manifest is encrypted too. File names in the clear is the standard hole
# in an "encrypted backup" claim.
leaked=""
for name in README.md main.rs notes.txt link-to-main; do
    grep -rqa -- "$name" "$TARGET" && leaked="${leaked} ${name}"
done
[ -z "$leaked" ] && ok "no file NAME reaches the target in the clear" \
                 || bad "these file names are readable on the target:${leaked}"

# What IS in the clear is exactly what the head documents.
python3 - "$SNAPDIR/head.json" "$SNAP" "$RECIPIENT" <<'PY'
import json, sys
head = json.load(open(sys.argv[1]))
want = {"format","snapshot","created_ms","recipient","ephemeral",
        "chunk_bytes","manifest_chunks","data_chunks","label"}
assert set(head) == want, f"the head's fields are {sorted(head)}"
assert head["snapshot"] == sys.argv[2], head["snapshot"]
assert head["recipient"] == sys.argv[3], head["recipient"]
assert head["label"] == "nightly", head["label"]
PY
[ $? -eq 0 ] && ok "the plaintext head holds only what it documents" \
             || bad "the plaintext head holds only what it documents"

grep -qa -- "$(basename "$PROJ")" "$SNAPDIR/head.json" \
    && bad "the head does not name the source directory" \
    || ok "the head does not name the source directory"

# ── listing and verifying ────────────────────────────────────────────────────
section "listing and verifying"
list_out="$("$Rime" backup list --project "$PROJ" 2>&1)"
printf '%s' "$list_out" | pipe_has "$SNAP" \
    && ok "\`backup list\` shows the snapshot" \
    || { bad "\`backup list\` shows the snapshot"; printf '      %s\n' "$list_out"; }
printf '%s' "$list_out" | pipe_has "nightly" \
    && ok "the listing carries the label" || bad "the listing carries the label"

# Presence without a key is a real answer. Contents without a key is not.
shallow="$("$Rime" backup verify "$SNAP" --project "$PROJ" --json 2>&1)"
printf '%s' "$shallow" | python3 -c '
import json,sys
v=json.load(sys.stdin)
assert v["presence"]["state"]=="intact", v
assert v["contents"]["state"]=="could-not-run", v
' 2>/dev/null \
    && ok "without a key, contents is could-not-run and never intact" \
    || { bad "without a key, contents is could-not-run and never intact"; printf '      %s\n' "$shallow"; }

deep="$("$Rime" backup verify "$SNAP" --project "$PROJ" --deep --json 2>&1)"
printf '%s' "$deep" | python3 -c '
import json,sys
v=json.load(sys.stdin)
assert v["presence"]["state"]=="intact", v
assert v["contents"]["state"]=="intact", v
' 2>/dev/null \
    && ok "with the key, a good snapshot verifies" \
    || { bad "with the key, a good snapshot verifies"; printf '      %s\n' "$deep"; }

# ── restoring ────────────────────────────────────────────────────────────────
section "restoring"
INTO="${WORK}/restored"
restore_out="$("$Rime" backup restore "$SNAP" --into "$INTO" --project "$PROJ" 2>&1)"
[ $? -eq 0 ] && ok "\`backup restore\` restores into a new directory" \
             || { bad "\`backup restore\` restores into a new directory"; printf '      %s\n' "$restore_out"; }
grep -q "$CANARY" "${INTO}/src/main.rs" 2>/dev/null \
    && ok "a restored file is byte for byte what it was" \
    || bad "a restored file is byte for byte what it was"
[ -L "${INTO}/link-to-main" ] && ok "a symlink comes back as a symlink" \
                              || bad "a symlink comes back as a symlink"
[ -d "${INTO}/node_modules" ] && bad "an excluded directory is not in the snapshot" \
                             || ok "an excluded directory is not in the snapshot"

# A restore writes a whole tree. Over the thing you were trying to recover is
# one keystroke away from beside it.
refused="$("$Rime" backup restore "$SNAP" --into "$INTO" --project "$PROJ" 2>&1)"
[ $? -ne 0 ] && ok "a restore into a non-empty directory refuses" \
             || bad "a restore into a non-empty directory refuses"
printf '%s' "$refused" | pipe_has "is not empty" \
    && ok "its refusal says why" || bad "its refusal says why"
"$Rime" backup restore "$SNAP" --into "$INTO" --project "$PROJ" --into-non-empty >/dev/null 2>&1
[ $? -eq 0 ] && ok "and goes ahead when told to in as many words" \
             || bad "and goes ahead when told to in as many words"

# ── damage ───────────────────────────────────────────────────────────────────
section "damage, and the four verdicts"
cp -a "$SNAPDIR" "${WORK}/pristine-snapshot"

# One bit.
python3 - "${SNAPDIR}/data.000000" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[-1] ^= 0x01
open(p, "wb").write(bytes(b))
PY
bitflip="$("$Rime" backup verify "$SNAP" --project "$PROJ" --deep --json 2>&1)"
printf '%s' "$bitflip" | python3 -c '
import json,sys
v=json.load(sys.stdin)
assert v["presence"]["state"]=="intact", v
assert v["contents"]["state"]=="failed", v
' 2>/dev/null \
    && ok "a flipped bit is failed and never intact" \
    || { bad "a flipped bit is failed and never intact"; printf '      %s\n' "$bitflip"; }
"$Rime" backup verify "$SNAP" --project "$PROJ" --deep >/dev/null 2>&1
[ $? -ne 0 ] && ok "a verdict that is not intact exits non-zero" \
             || bad "a verdict that is not intact exits non-zero"
cp "${WORK}/pristine-snapshot/data.000000" "${SNAPDIR}/data.000000"

# A chunk gone.
rm -f "${SNAPDIR}/data.000000"
missing="$("$Rime" backup verify "$SNAP" --project "$PROJ" --deep --json 2>&1)"
printf '%s' "$missing" | python3 -c '
import json,sys
v=json.load(sys.stdin)
assert v["presence"]["state"]=="failed", v
assert "never taken" in v["presence"]["reason"], v
' 2>/dev/null \
    && ok "a chunk the head declares and is gone is a failure, not an absence" \
    || { bad "a chunk the head declares and is gone is a failure, not an absence"; printf '      %s\n' "$missing"; }
cp "${WORK}/pristine-snapshot/data.000000" "${SNAPDIR}/data.000000"

# The head gone: the head is written last, so this is an interrupted run.
mv "${SNAPDIR}/head.json" "${WORK}/head-aside.json"
headless="$("$Rime" backup verify "$SNAP" --project "$PROJ" --json 2>&1)"
printf '%s' "$headless" | python3 -c '
import json,sys
v=json.load(sys.stdin)
assert v["presence"]["state"]=="absent", v
assert "interrupted" in v["presence"]["reason"], v
assert v["contents"]["state"]=="could-not-run", v
' 2>/dev/null \
    && ok "a snapshot with no head is an interrupted write and says so" \
    || { bad "a snapshot with no head is an interrupted write and says so"; printf '      %s\n' "$headless"; }
cp "${WORK}/head-aside.json" "${SNAPDIR}/head.json"

# ── permission denied is not absence ─────────────────────────────────────────
section "an unreadable target is not an empty one"
if [ "$(id -u)" -eq 0 ]; then
    skip "an unreadable snapshot directory is could-not-run: running as root,"
    printf '      which mode bits do not stop. NOT ASSERTED: that a directory the\n'
    printf '      kernel refuses is reported as could-not-run rather than as a\n'
    printf '      target with no snapshots in it.\n'
else
    chmod 000 "${TARGET}/apex-backup"  # rime-rename: keep (DEFAULT_PREFIX)
    denied="$("$Rime" backup list --project "$PROJ" 2>&1)"
    rc=$?
    chmod 700 "${TARGET}/apex-backup"  # rime-rename: keep (DEFAULT_PREFIX)
    [ "$rc" -ne 0 ] && ok "listing an unreadable target fails rather than printing nothing" \
                    || { bad "listing an unreadable target fails rather than printing nothing"
                         printf '      %s\n' "$denied"; }
    printf '%s' "$denied" | pipe_has -i "holds no snapshots" \
        && bad "an unreadable target is not reported as an empty one" \
        || ok "an unreadable target is not reported as an empty one"
fi

# ── the NAS mount check ──────────────────────────────────────────────────────
section "a nas target that is not mounted"
NASPROJ="${WORK}/nasproject"
mkdir -p "$NASPROJ"
cat > "${NASPROJ}/rime.toml" <<TOML
[backup]
recipient = "${RECIPIENT}"
target = "nas"

[backup.nas]
path = "${TARGET}"
TOML
# $TARGET is a perfectly good, perfectly writable directory that is not a mount
# point — which is exactly what an unmounted NAS looks like.
nas_out="$("$Rime" backup run --project "$NASPROJ" 2>&1)"
[ $? -ne 0 ] && ok "a nas target that is not a mount point refuses" \
             || bad "a nas target that is not a mount point refuses"
printf '%s' "$nas_out" | pipe_has "not a mount point" \
    && ok "its refusal names the cause" \
    || { bad "its refusal names the cause"; printf '      %s\n' "$nas_out"; }
printf '%s' "$nas_out" | pipe_has "gone with the machine" \
    && ok "and says what would have happened" || bad "and says what would have happened"

# ── the recipient an agent could have written ────────────────────────────────
section "a recipient this machine never registered"
STRANGER="${WORK}/stranger"
mkdir -p "$STRANGER"
# A second key directory, so this is a real recipient of a real key — just not
# one this machine has registered. Exactly what an agent that can edit rime.toml
# would put there.
other_out="$(RIME_BACKUP_KEYS="${WORK}/other-keys" "$Rime" backup key init 2>&1)"
OTHER="$(printf '%s' "$other_out" | sed -n 's/^recipient = "\(.*\)"$/\1/p')"
[ -n "$OTHER" ] && ok "a second, unrelated key exists to test with" \
                || bad "a second, unrelated key exists to test with"
sed "s|^recipient = .*|recipient = \"${OTHER}\"|" "${PROJ}/rime.toml" > "${PROJ}/rime.toml.swapped"
mv "${PROJ}/rime.toml.swapped" "${PROJ}/rime.toml"
swapped="$("$Rime" backup run --project "$PROJ" 2>&1)"
[ $? -ne 0 ] && ok "a swapped recipient refuses the run" \
             || bad "a swapped recipient refuses the run"
printf '%s' "$swapped" | pipe_has "a declaration and never an authority" \
    && ok "its refusal says why a file the project owns is not enough" \
    || { bad "its refusal says why a file the project owns is not enough"; printf '      %s\n' "$swapped"; }
# And a typo is caught before anything is written, not at the restore that
# needed it.
#
# The typo must be a DIFFERENT character. An age recipient is bech32, which is
# case-insensitive and has `x` in its alphabet, so the old `…X` substitution was
# the same key whenever the last character already was `x` — about one run in
# 32, and the run then correctly succeeded and this assertion failed in CI.
# `q` and `p` are both bech32 characters, so the result stays well-formed and
# only the checksum is wrong.
_last="$(printf '%s' "$RECIPIENT" | tail -c 1 | tr '[:upper:]' '[:lower:]')"
if [ "$_last" = "q" ]; then _typo_char=p; else _typo_char=q; fi
sed "s|^recipient = .*|recipient = \"${RECIPIENT%?}${_typo_char}\"|" "${PROJ}/rime.toml" > "${PROJ}/rime.toml.typo"
mv "${PROJ}/rime.toml.typo" "${PROJ}/rime.toml"
typo="$("$Rime" backup run --project "$PROJ" 2>&1)"
[ $? -ne 0 ] && ok "a one-character typo in the recipient refuses the run" \
             || { bad "a one-character typo in the recipient refuses the run"
                  printf '      %s\n' "$typo"; }

# ── the at-rest half, as real root ───────────────────────────────────────────
section "the private key, measured as real root"
if ! sudo -n true 2>/dev/null; then
    skip "the private key is unreadable by its owner: passwordless sudo is not"
    printf '      available, and this half is only meaningful as real root — a key\n'
    printf '      written by a process pretending to be root is owned by the very\n'
    printf '      account the boundary excludes. NOT ASSERTED: key file ownership,\n'
    printf '      and the owner uid being refused by the kernel.\n'
else
    ROOT_KEYS="${WORK}/root-keys"
    sudo -n env "RIME_BACKUP_KEYS=${ROOT_KEYS}" "SUDO_UID=$(id -u)" \
        "$Rime" backup key init >/dev/null 2>&1
    KEYFILE="${ROOT_KEYS}/keys/$(id -u).key"
    sudo -n test -f "$KEYFILE" 2>/dev/null \
        && ok "root made a key for uid $(id -u)" || { bad "root made a key for uid $(id -u)"; finish; }

    owner="$(sudo -n stat -c '%U' "$KEYFILE" 2>/dev/null)"
    [ "$owner" = "root" ] && ok "the private half is owned by root (is ${owner})" \
                          || bad "the private half is owned by root (is ${owner})"
    kmode="$(sudo -n stat -c '%a' "$KEYFILE" 2>/dev/null)"
    [ "$kmode" = "600" ] && ok "the private half is mode 600 as root wrote it" \
                         || bad "the private half is mode 600 as root wrote it (is ${kmode})"

    # THE assertion: the account that owns the backups cannot open the file.
    # Not `sudo`'s uid — this shell's, which is the account an agent runs as.
    if cat "$KEYFILE" >/dev/null 2>&1; then
        bad "uid $(id -u) is refused the private key by the kernel"
    else
        ok "uid $(id -u) is refused the private key by the kernel"
    fi

    # The public half is not a secret, and backing up needs only that.
    PUBFILE="${ROOT_KEYS}/recipients/$(id -u).pub"
    if cat "$PUBFILE" >/dev/null 2>&1; then
        ok "the public half is readable, which is what lets a backup run unprivileged"
    else
        bad "the public half is readable, which is what lets a backup run unprivileged"
    fi

    # And the refusal is reported as a refusal, never as "there is no key".
    denied="$(RIME_BACKUP_KEYS="${ROOT_KEYS}" "$Rime" backup restore latest \
        --into "${WORK}/nope" --project "$PROJ" 2>&1)"
    printf '%s' "$denied" | pipe_has "refusal and not an absence" \
        && ok "a key the caller may not read is a refusal and not an absence" \
        || { bad "a key the caller may not read is a refusal and not an absence"
             printf '      %s\n' "$denied"; }
fi

finish
