#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-user.sh — standard vs administrator accounts (roadmap P2-016,
#  criterion 1) and the guest wiring (criterion 2).
#
#  ── Why this suite exists ───────────────────────────────────────────────────
#
#  The standard/administrator distinction was already ENFORCED on Rime before
#  P2-016 — polkit `auth_admin` with `allow_any=no` and `allow_inactive=no` on
#  both agent actions, asserted at build time — and it was not REACHABLE:
#  `installer/rime-install` puts the one account it creates in `wheel`
#  unconditionally and the GUI offers no choice. `rime user` is the surface
#  that makes a standard account, and the assertion that carries the criterion
#  is a one-line one: `add` without `--admin` must not put the account in
#  `wheel`. Everything else here is the fences around that.
#
#  ── How it runs the real engine without creating accounts ───────────────────
#
#  Two overrides the engine documents. `RIME_USER_PASSWD` and
#  `RIME_USER_GROUP` give it a fixture machine to read — a passwd and group
#  file in a temp directory, with two administrators, a standard account, a
#  system account and a guest. `RIME_USER_TOOLS` replaces every privileged tool
#  it can run (useradd, userdel, gpasswd, systemctl) with a stub that records
#  its argv, so the DECISIONS are exercised through the production path and the
#  argv is asserted rather than the effect.
#
#  That second one is also why the suite can run as an ordinary user: with
#  nothing privileged left behind the engine, its root check has nothing to
#  protect. The engine refuses the variable when it IS root, and that refusal
#  is asserted here too — otherwise the override would be a way to point a real
#  `sudo rime user` at somebody else's binaries.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#
#  It creates no account, removes no account, and writes nothing outside its
#  temp directory. Every account it reasons about is a line in a fixture file.
#
#      ./tests/test-rime-user.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# +e deliberately, like every other suite here: CI invokes a suite as
# `bash -e {0}`, and under -e an assignment from a command that exits non-zero
# ends the run silently, mid-section. This suite COUNTS failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
# There is deliberately no skip helper.

ENGINE="$ROOT/files/system/libexec/rime-user"
[ -f "$ENGINE" ] || { echo "FATAL: cannot find $ENGINE" >&2; exit 2; }

# ─────────────────────────────────────────────────────────────────────────────
section "the shipped file parses"
# ─────────────────────────────────────────────────────────────────────────────
bash -n "$ENGINE" 2>"$WORK/syntax" \
    && ok "rime-user is syntactically valid bash" \
    || bad "rime-user is syntactically valid bash" "$(cat "$WORK/syntax")"

# ─────────────────────────────────────────────────────────────────────────────
#  The fixture machine.
# ─────────────────────────────────────────────────────────────────────────────
FIX="$WORK/fix"
mkdir -p "$FIX/tools"

# owner  — administrator by MEMBERSHIP of wheel
# second — administrator by PRIMARY GID, which a members-only check misses
# plain  — standard
# guest  — standard, and in the allowlist
# svc    — a system account, below UID_MIN
cat > "$FIX/passwd" <<PASSWD
root:x:0:0:root:/root:/bin/bash
svc:x:180:180:A daemon:/var/lib/svc:/sbin/nologin
owner:x:1000:1000:The machine owner:/var/home/owner:/bin/zsh
second:x:1001:10:A second administrator:/var/home/second:/bin/zsh
plain:x:1002:1002:A standard account:/var/home/plain:/bin/zsh
guest:x:1500:1500:Guest:/var/home/guest:/bin/bash
rooted:x:1600:1600:Guest with no home:/:/bin/bash
PASSWD

cat > "$FIX/group" <<GROUP
root:x:0:
wheel:x:10:owner
svc:x:180:
owner:x:1000:
plain:x:1002:
guest:x:1500:
rooted:x:1600:
GROUP

# A machine with exactly one administrator, for the fence that matters most.
sed 's/^second:x:1001:10:/second:x:1001:1001:/' "$FIX/passwd" > "$FIX/passwd-one-admin"

printf 'UID_MIN 1000\nUID_MAX 60000\n' > "$FIX/login.defs"
printf 'guest\n' > "$FIX/allowlist"

# Stubs for every privileged tool the engine can run. Each records its argv.
for t in useradd userdel gpasswd systemctl; do
    cat > "$FIX/tools/$t" <<STUB
#!/bin/sh
printf '%s' "$t" >> "\$RIME_TEST_LOG"
for a in "\$@"; do printf ' %s' "\$a" >> "\$RIME_TEST_LOG"; done
printf '\n' >> "\$RIME_TEST_LOG"
exit 0
STUB
    chmod +x "$FIX/tools/$t"
done

# Run the real engine against the fixture machine. $1 is the passwd file to
# use; the rest is the command line.
run() {
    local pw="$1"; shift
    : > "$WORK/log"
    RIME_USER_PASSWD="$pw" \
    RIME_USER_GROUP="$FIX/group" \
    RIME_USER_LOGIN_DEFS="$FIX/login.defs" \
    RIME_GUEST_ALLOWLIST="$FIX/allowlist" \
    RIME_USER_TOOLS="$FIX/tools" \
    RIME_USER_CALLER="${CALLER:-owner}" \
    RIME_TEST_LOG="$WORK/log" \
    bash "$ENGINE" "$@" >"$WORK/out" 2>"$WORK/err"
    echo $?
}
ran() { cat "$WORK/log"; }

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 1 — list says which accounts are administrators"
# ─────────────────────────────────────────────────────────────────────────────
rc="$(run "$FIX/passwd" list)"
[ "$rc" = 0 ] && ok "list runs without root" || bad "list runs without root" "rc=$rc"

grep -qE '^owner +1000 +administrator' "$WORK/out" \
    && ok "an account in wheel's member list is an administrator" \
    || bad "an account in wheel's member list is an administrator" "$(cat "$WORK/out")"
# The half a members-only check misses. `useradd -g wheel` makes an
# administrator whose name never appears in the group line.
grep -qE '^second +1001 +administrator' "$WORK/out" \
    && ok "and so is one whose PRIMARY group is wheel, which the member list never names" \
    || bad "and so is one whose PRIMARY group is wheel, which the member list never names" \
           "$(cat "$WORK/out")"
grep -qE '^plain +1002 +standard' "$WORK/out" \
    && ok "an account in neither is standard" \
    || bad "an account in neither is standard"
grep -qE '^guest +1500 +standard +yes' "$WORK/out" \
    && ok "and the allowlist is reported, so a disposable guest is visible" \
    || bad "and the allowlist is reported, so a disposable guest is visible"
grep -q '^root ' "$WORK/out" \
    && bad "root and system accounts are not listed as people" \
    || ok "root and system accounts are not listed as people"
grep -q '^svc ' "$WORK/out" \
    && bad "a uid below UID_MIN is not listed as a person" \
    || ok "a uid below UID_MIN is not listed as a person"

rc="$(run "$FIX/passwd" list --json)"
if [ "$rc" = 0 ] && command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys; d=json.load(open(sys.argv[1]));
assert [x["name"] for x in d] == ["owner","second","plain","guest","rooted"], d
assert [x["role"] for x in d][:3] == ["administrator","administrator","standard"], d
assert [x["guest"] for x in d] == [False,False,False,True,False], d' "$WORK/out" 2>"$WORK/jerr" \
        && ok "--json is valid JSON with the same answers" \
        || bad "--json is valid JSON with the same answers" "$(cat "$WORK/jerr")"
else
    bad "--json is valid JSON with the same answers" "rc=$rc, or no python3"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 1 — THE assertion: add makes a STANDARD account"
# ─────────────────────────────────────────────────────────────────────────────
#
# One line decides whether Rime can create a non-administrator at all. A
# dropped condition here is not a compile error and not a visible failure: it
# is every new account silently able to approve a root operation, which is the
# state the installer is in today.
rc="$(run "$FIX/passwd" add alice)"
[ "$rc" = 0 ] && ok "add succeeds" || bad "add succeeds" "rc=$rc  $(cat "$WORK/err")"
if ran | pipe_has '^useradd .*-G wheel'; then
    bad "a plain \`rime user add\` does NOT put the account in wheel" "$(ran)"
else
    ok "a plain \`rime user add\` does NOT put the account in wheel"
fi
ran | pipe_has '^useradd .* alice$' \
    && ok "and it does create the account" || bad "and it does create the account" "$(ran)"
grep -qi 'STANDARD' "$WORK/err" \
    && ok "and it says out loud which kind it made" \
    || bad "and it says out loud which kind it made" "$(cat "$WORK/err")"

# The absence above means nothing unless the check can see a presence.
rc="$(run "$FIX/passwd" add alice --admin)"
[ "$rc" = 0 ] && ok "--admin succeeds" || bad "--admin succeeds" "rc=$rc"
ran | pipe_has '^useradd .*-G wheel' \
    && ok "and --admin IS the thing that adds wheel, so the check above can fail" \
    || bad "and --admin IS the thing that adds wheel, so the check above can fail" "$(ran)"
grep -qi 'ADMINISTRATOR' "$WORK/err" \
    && ok "and it says so, in the word the user will look for" \
    || bad "and it says so, in the word the user will look for"

# The login shell is the installer's choice, for the installer's reason: an
# account with a nonexistent login shell cannot log in at all.
ran | pipe_has -E '^useradd .*-s (/bin/zsh|/bin/bash)' \
    && ok "the account gets a login shell that exists" \
    || bad "the account gets a login shell that exists" "$(ran)"
ran | pipe_has '^useradd .*-m' \
    && ok "and a home directory" || bad "and a home directory" "$(ran)"

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 1 — what add refuses"
# ─────────────────────────────────────────────────────────────────────────────
rc="$(run "$FIX/passwd" add owner)"
[ "$rc" = 2 ] && ok "an existing account is refused" || bad "an existing account is refused" "rc=$rc"
[ -s "$WORK/log" ] && bad "and nothing was run" "$(ran)" || ok "and nothing was run"

rc="$(run "$FIX/passwd" add 1042)"
[ "$rc" = 2 ] && ok "an all-digit name is refused (it cannot be told from a uid)" \
              || bad "an all-digit name is refused (it cannot be told from a uid)" "rc=$rc"
rc="$(run "$FIX/passwd" add 'Alice Smith')"
[ "$rc" = 2 ] && ok "a name with a space is refused" || bad "a name with a space is refused" "rc=$rc"
rc="$(run "$FIX/passwd" add -- --admin)"
[ "$rc" = 2 ] && ok "a name that is a flag is refused" || bad "a name that is a flag is refused" "rc=$rc"

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 1 — what rm refuses, and the one that bricks a machine"
# ─────────────────────────────────────────────────────────────────────────────
rc="$(run "$FIX/passwd" rm root)"
[ "$rc" = 2 ] && ok "rm refuses uid 0" || bad "rm refuses uid 0" "rc=$rc"
grep -q 'uid 0' "$WORK/err" \
    && ok "and says it was because of uid 0" \
    || bad "and says it was because of uid 0" "$(cat "$WORK/err")"

rc="$(run "$FIX/passwd" rm svc)"
[ "$rc" = 2 ] && ok "rm refuses a system account below UID_MIN" \
              || bad "rm refuses a system account below UID_MIN" "rc=$rc"

rc="$(run "$FIX/passwd" rm owner)"
[ "$rc" = 2 ] && ok "rm refuses the account you are running as" \
              || bad "rm refuses the account you are running as" "rc=$rc"

rc="$(run "$FIX/passwd" rm nobody-here)"
[ "$rc" = 2 ] && ok "rm refuses an account that does not exist" \
              || bad "rm refuses an account that does not exist" "rc=$rc"

# THE fence. polkit's auth_admin and sudo both resolve to wheel, so a Rime
# with no member of it left cannot approve anything or become root again —
# there is no recovery path from inside the running system.
rc="$(CALLER=plain run "$FIX/passwd-one-admin" rm owner)"
[ "$rc" = 2 ] && ok "rm refuses the LAST administrator" \
              || bad "rm refuses the LAST administrator" "rc=$rc  $(cat "$WORK/err")"
grep -q 'only administrator' "$WORK/err" \
    && ok "and the refusal says that is why" \
    || bad "and the refusal says that is why" "$(cat "$WORK/err")"
[ -s "$WORK/log" ] && bad "and userdel was not run" "$(ran)" || ok "and userdel was not run"

# Which must not mean "an administrator can never be removed": with a second
# one present the same command goes through. Without this the fence above
# would pass with `is_admin` hard-wired to true.
rc="$(CALLER=plain run "$FIX/passwd" rm owner)"
[ "$rc" = 0 ] && ok "but an administrator IS removable while another one remains" \
              || bad "but an administrator IS removable while another one remains" \
                     "rc=$rc  $(cat "$WORK/err")"
ran | pipe_has '^userdel -r owner$' \
    && ok "and userdel takes the home with it by default" \
    || bad "and userdel takes the home with it by default" "$(ran)"
rc="$(CALLER=plain run "$FIX/passwd" rm owner --keep-home)"
ran | pipe_has '^userdel owner$' \
    && ok "and --keep-home is the only way to keep it" \
    || bad "and --keep-home is the only way to keep it" "$(ran)"

# THE OTHER fence whose failure only shows up on somebody else's machine-day.
# The guest wiring has two halves keyed differently -- the allowlist names a
# NAME, rime-guest-session@<uid>.service names a UID -- so removing a guest
# without disabling it leaves an enabled hook pointed at a uid that will be
# handed to the next account, whose logout then fails the unit for a reason
# nothing explains.
rc="$(run "$FIX/passwd" rm guest)"
[ "$rc" = 2 ] && ok "rm refuses an account that is still a configured guest" \
              || bad "rm refuses an account that is still a configured guest" "rc=$rc"
grep -q 'guest disable' "$WORK/err" \
    && ok "and names the command that would make it safe" \
    || bad "and names the command that would make it safe" "$(cat "$WORK/err")"
[ -s "$WORK/log" ] && bad "and userdel was not run for it" "$(ran)" \
                   || ok "and userdel was not run for it"
# Which must not mean a guest can never be removed: disable it and it goes.
rc="$(run "$FIX/passwd" guest disable guest)"
rc="$(run "$FIX/passwd" rm guest)"
[ "$rc" = 0 ] && ok "but it IS removable once it is no longer a guest" \
              || bad "but it IS removable once it is no longer a guest" "rc=$rc  $(cat "$WORK/err")"
fresh_allowlist_early() { printf 'guest\n' > "$FIX/allowlist"; }
fresh_allowlist_early

# The credential namespace is not in the home (P0-002), userdel knows nothing
# about it, and a uid is reusable. Saying nothing would hand the next account
# at that uid the last one's credentials.
rc="$(run "$FIX/passwd" rm plain)"
grep -q 'rime-secretd/users/1002' "$WORK/err" \
    && ok "removing an account names the credential namespace userdel leaves behind" \
    || bad "removing an account names the credential namespace userdel leaves behind" \
           "$(cat "$WORK/err")"

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 2 — guest enable is the README's four steps, fenced"
# ─────────────────────────────────────────────────────────────────────────────
fresh_allowlist() { printf 'guest\n' > "$FIX/allowlist"; }

fresh_allowlist
rc="$(run "$FIX/passwd" guest enable plain)"
[ "$rc" = 0 ] && ok "guest enable adds a standard account" \
              || bad "guest enable adds a standard account" "rc=$rc  $(cat "$WORK/err")"
grep -qx 'plain' "$FIX/allowlist" \
    && ok "and writes it to the allowlist the wipe engine reads" \
    || bad "and writes it to the allowlist the wipe engine reads" "$(cat "$FIX/allowlist")"
# BY UID. The unit hangs off logind's per-user units and those are named by
# uid; a name here would enable a unit instance that never fires.
ran | pipe_has -x 'systemctl enable rime-guest-session@1002.service' \
    && ok "and enables the session hook BY UID, which is how logind names it" \
    || bad "and enables the session hook BY UID, which is how logind names it" "$(ran)"

# Twice must not duplicate: the wipe reads whole lines, but a file that grows
# a line per run is a file somebody eventually edits wrong.
rc="$(run "$FIX/passwd" guest enable plain)"
[ "$(grep -cx 'plain' "$FIX/allowlist")" = 1 ] \
    && ok "enabling twice does not duplicate the line" \
    || bad "enabling twice does not duplicate the line" "$(cat "$FIX/allowlist")"

rc="$(run "$FIX/passwd" guest disable plain)"
[ "$rc" = 0 ] && ok "guest disable succeeds" || bad "guest disable succeeds" "rc=$rc"
grep -qx 'plain' "$FIX/allowlist" \
    && bad "and takes the name back out of the allowlist" "$(cat "$FIX/allowlist")" \
    || ok "and takes the name back out of the allowlist"
grep -qx 'guest' "$FIX/allowlist" \
    && ok "and leaves the other guest alone" \
    || bad "and leaves the other guest alone" "$(cat "$FIX/allowlist")"
ran | pipe_has -x 'systemctl disable rime-guest-session@1002.service' \
    && ok "and disables the unit" || bad "and disables the unit" "$(ran)"

fresh_allowlist
rc="$(CALLER=second run "$FIX/passwd" guest enable owner)"
[ "$rc" = 2 ] && ok "guest enable refuses an ADMINISTRATOR" \
              || bad "guest enable refuses an ADMINISTRATOR" "rc=$rc"
grep -q 'administrator' "$WORK/err" \
    && ok "and says that is why" || bad "and says that is why" "$(cat "$WORK/err")"

rc="$(CALLER=plain run "$FIX/passwd" guest enable plain)"
[ "$rc" = 2 ] && ok "guest enable refuses the account you are running as" \
              || bad "guest enable refuses the account you are running as" "rc=$rc"

rc="$(run "$FIX/passwd" guest enable rooted)"
[ "$rc" = 2 ] && ok "guest enable refuses an account whose home the wipe would not clear" \
              || bad "guest enable refuses an account whose home the wipe would not clear" "rc=$rc"

rc="$(run "$FIX/passwd" guest enable root)"
[ "$rc" = 2 ] && ok "guest enable refuses uid 0" || bad "guest enable refuses uid 0" "rc=$rc"

rc="$(run "$FIX/passwd" guest enable svc)"
[ "$rc" = 2 ] && ok "guest enable refuses a system account" \
              || bad "guest enable refuses a system account" "rc=$rc"

[ "$(grep -c . "$FIX/allowlist")" = 1 ] \
    && ok "and none of those five refusals wrote to the allowlist" \
    || bad "and none of those five refusals wrote to the allowlist" "$(cat "$FIX/allowlist")"

# ─────────────────────────────────────────────────────────────────────────────
section "the test override is not a way into a privileged run"
# ─────────────────────────────────────────────────────────────────────────────
#
# RIME_USER_TOOLS turns off the root check, which is only safe because there is
# then nothing privileged left to protect. As root there is, so it must be
# refused rather than honoured — otherwise it is a way to point
# `sudo rime user add` at somebody else's useradd.
if grep -q 'RIME_USER_TOOLS is set and this is running as root' "$ENGINE"; then
    ok "the engine refuses RIME_USER_TOOLS when it is running as root"
else
    bad "the engine refuses RIME_USER_TOOLS when it is running as root"
fi
# And without the override, a non-root mutating run is refused outright rather
# than half-done. Run with the variable UNSET, as an ordinary user.
if [ "$(id -u)" -eq 0 ]; then
    bad "a non-root \`add\` is refused before anything runs" "this suite is running as root"
else
    RIME_USER_PASSWD="$FIX/passwd" RIME_USER_GROUP="$FIX/group" \
    RIME_USER_LOGIN_DEFS="$FIX/login.defs" \
        bash "$ENGINE" add alice >"$WORK/out" 2>"$WORK/err"
    rc=$?
    [ "$rc" = 2 ] && grep -q 'needs root' "$WORK/err" \
        && ok "a non-root \`add\` is refused before anything runs" \
        || bad "a non-root \`add\` is refused before anything runs" "rc=$rc  $(cat "$WORK/err")"
fi

# `list` must stay usable by anybody: "who are the administrators on this
# machine" is not a privileged question, and making it one would mean the
# answer is only ever seen by somebody who already knows it.
RIME_USER_PASSWD="$FIX/passwd" RIME_USER_GROUP="$FIX/group" \
RIME_USER_LOGIN_DEFS="$FIX/login.defs" \
    bash "$ENGINE" list >"$WORK/out" 2>&1
[ $? = 0 ] && ok "\`list\` needs no root" || bad "\`list\` needs no root" "$(cat "$WORK/out")"

printf '\nrime-user: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
