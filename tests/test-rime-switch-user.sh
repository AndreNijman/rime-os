#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-switch-user.sh — fast user switching (roadmap P2-016, criterion 1).
#
#  Two rounds of P2-016 measured per-account isolation on a real second account
#  and closed it. The switching half had no surface at all, and this suite is
#  the one that says the new surface behaves. What it exercises is the SHIPPED
#  engines — files/system/libexec/rime-switch-user and rime-switch-greeter —
#  through the overrides their headers document, not a re-description of them.
#
#  ── Why fakes, and what that costs ──────────────────────────────────────────
#
#  A switch takes the seat. There is exactly one seat on the machine this was
#  written on, it has Andre's desktop on it, and a suite that called the real
#  `loginctl activate` would move his screen to another VT — in CI it would
#  find no seat at all. So `loginctl`, `systemctl` and `sudo` are stand-ins in
#  a directory, exactly as rime-guest-wipe takes RIME_GUEST_SESSIONS, and the
#  suite asserts the ARGV the engine produced and the ORDER it produced it in.
#
#  That is the whole of what is claimed: the decision is tested, the seat
#  change is not. A switch on real hardware is the one thing left open and the
#  docs say so rather than implying otherwise.
#
#  ── The assertion this suite exists for ─────────────────────────────────────
#
#  `loginctl lock-session` is a REQUEST. What makes a session locked is its own
#  locker setting logind's LockedHint, and this repository has a standing
#  record of that hint answering "no" on a session that was locked. A switch
#  that ran before the screen went black hands the next person at the keyboard
#  an unlocked desktop — the precise failure fast switching is supposed to
#  prevent, and one that looks perfect from the outside. So: a fake whose hint
#  never flips must produce a REFUSAL and no activate.
#
#      ./tests/test-rime-switch-user.sh
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
WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-switch.XXXXXX")" || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
# There is deliberately no skip helper. This repository's own notes record
# three occasions where a skip became a green tick over nothing asserted.

ENGINE="$ROOT/files/system/libexec/rime-switch-user"
HELPER="$ROOT/files/system/libexec/rime-switch-greeter"
USER_ENGINE="$ROOT/files/system/libexec/rime-user"
UNIT="$ROOT/files/system/units/rime-switch-greeter@.service"
SUDOERS="$ROOT/files/system/sudoers/rime-switch-greeter"
GREET_CONF="$ROOT/files/desktop/rime-greet/greetd-config.toml"
KIOSK_CONF="$ROOT/files/system/shared-machine/greetd-kiosk.toml"

for f in "$ENGINE" "$HELPER" "$USER_ENGINE" "$UNIT" "$SUDOERS" "$GREET_CONF" "$KIOSK_CONF"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done

# ─────────────────────────────────────────────────────────────────────────────
#  the fixture
# ─────────────────────────────────────────────────────────────────────────────
#
# One directory per scenario. `fix_new <name>` builds a machine: a passwd file,
# a login.defs, a greetd config and a set of logind sessions. The stand-ins
# read those files and append every call to state/actions, so an assertion can
# be about WHAT was run and about the ORDER.

fix_new() {  # $1 = name
    FIX="$WORK/$1"
    mkdir -p "$FIX/tools" "$FIX/state"
    : > "$FIX/state/actions"
    : > "$FIX/state/sessions"
    : > "$FIX/state/units"

    cat > "$FIX/passwd" <<'PASSWD'
root:x:0:0:root:/root:/bin/bash
bin:x:1:1:bin:/bin:/usr/sbin/nologin
greetd:x:987:987:greetd:/var/lib/greetd:/usr/sbin/nologin
andre:x:1000:1000:Andre:/var/home/andre:/bin/zsh
bob:x:1001:1001:Bob:/var/home/bob:/bin/bash
alice:x:1002:1002:Alice:/var/home/alice:/bin/bash
locked-out:x:1003:1003:No shell:/var/home/locked-out:/usr/sbin/nologin
PASSWD
    printf 'UID_MIN\t1000\nUID_MAX\t60000\n' > "$FIX/login.defs"
    printf '#NAutoVTs=6\n#ReserveVT=6\n' > "$FIX/logind.conf"

    # An ordinary login config: the greeter runs as the greetd system user.
    cat > "$FIX/greetd.toml" <<'GREETD'
[terminal]
vt = 1

[default_session]
command = "/usr/libexec/rime-greet-session sway --unsupported-gpu -c /usr/share/rime-greet/sway-greet.conf"
user = "greetd"
GREETD

    cat > "$FIX/tools/loginctl" <<'LOGINCTL'
#!/usr/bin/env bash
S="$RIME_FIX/state"
printf 'loginctl' >> "$S/actions"; printf ' %s' "$@" >> "$S/actions"; printf '\n' >> "$S/actions"
one_value() {  # $1 = file, rest = argv
    local f=$1; shift
    local key="" prev=""
    for a in "$@"; do [ "$prev" = "-p" ] && key="$a"; prev="$a"; done
    awk -F= -v k="$key" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$f"
}
case "${1:-}" in
    list-sessions) cat "$S/sessions" 2>/dev/null ;;
    show-session)
        id="${2:-}"
        [ "$id" = self ] && id="$(cat "$S/self" 2>/dev/null)"
        f="$S/session.$id"
        [ -f "$f" ] || exit 1
        case " $* " in *" --value "*) one_value "$f" "$@" ;; *) cat "$f" ;; esac
        ;;
    show-seat)
        f="$S/seat.${2:-}"
        [ -f "$f" ] || exit 1
        case " $* " in *" --value "*) one_value "$f" "$@" ;; *) cat "$f" ;; esac
        ;;
    lock-session)
        id="${2:-}"
        # The locker flipping LockedHint is a SEPARATE event from the request,
        # which is the whole point of the wait. `nolockflip` is the machine
        # where it never happens.
        [ -f "$S/nolockflip" ] && exit 0
        sed -i 's/^LockedHint=.*/LockedHint=yes/' "$S/session.$id" 2>/dev/null
        ;;
    activate) : ;;
    *) exit 64 ;;
esac
LOGINCTL

    cat > "$FIX/tools/systemctl" <<'SYSTEMCTL'
#!/usr/bin/env bash
S="$RIME_FIX/state"
printf 'systemctl' >> "$S/actions"; printf ' %s' "$@" >> "$S/actions"; printf '\n' >> "$S/actions"
case "${1:-}" in
    list-units) cat "$S/units" 2>/dev/null ;;
    start) : ;;
    *) exit 64 ;;
esac
SYSTEMCTL

    # sudo, so the engine -> helper chain runs end to end with no privilege.
    # It sets SUDO_UID exactly as sudo would and keeps the test overrides,
    # which is what lets the helper's own fences be exercised from the engine.
    cat > "$FIX/tools/sudo" <<'SUDO'
#!/usr/bin/env bash
S="$RIME_FIX/state"
printf 'sudo' >> "$S/actions"; printf ' %s' "$@" >> "$S/actions"; printf '\n' >> "$S/actions"
[ "${1:-}" = "-n" ] && shift
export SUDO_UID="${RIME_FIX_SUDO_UID:-1000}"
export RIME_SWITCH_GREETER_TOOLS="$RIME_FIX/tools"
export RIME_SWITCH_GREETER_RUNDIR="$RIME_FIX/run"
export RIME_SWITCH_GREETER_DEVDIR="$RIME_FIX/dev"
export RIME_SWITCH_GREETD_CONFIG="$RIME_FIX/greetd.toml"
export RIME_SWITCH_LOGIND_CONF="$RIME_FIX/logind.conf"
export RIME_SWITCH_PASSWD="$RIME_FIX/passwd"
export RIME_SWITCH_LOGIN_DEFS="$RIME_FIX/login.defs"
exec "$@"
SUDO
    chmod 0755 "$FIX/tools/loginctl" "$FIX/tools/systemctl" "$FIX/tools/sudo"

    # /dev/ttyN stand-ins. Character devices cannot be made without root, so
    # the engine's `[ -c ... ]` is pointed at real ones: the fixture directory
    # symlinks to /dev/null, which IS a character device. The VT it must not
    # pick is then the one with no entry at all.
    mkdir -p "$FIX/dev"
    for n in 1 2 3 4 5 6 7 8 9 10 11 12; do ln -sf /dev/null "$FIX/dev/tty$n"; done
    mkdir -p "$FIX/run"
    printf 'seat0\n' > "$FIX/state/seatname"
}

fix_session() {  # name uid seat vtnr class state active
    local id=$1 name=$2 uid=$3 seat=$4 vt=$5 cls=$6 st=$7 act=$8
    cat > "$FIX/state/session.$id" <<SESS
Id=$id
User=$uid
Name=$name
VTNr=$vt
Seat=$seat
Type=wayland
Class=$cls
Active=$act
State=$st
LockedHint=no
SESS
    printf '%s %s %s %s\n' "$id" "$uid" "$name" "$seat" >> "$FIX/state/sessions"
}

fix_seat() {  # $1 = active session id
    cat > "$FIX/state/seat.seat0" <<SEAT
Id=seat0
ActiveSession=$1
CanTTY=yes
CanGraphical=yes
SEAT
}

# A machine with andre at the keyboard on VT 1, bob logged in on VT 2, and
# alice with an account but no session. This is the shape every switching
# assertion below is about.
fix_standard() {
    fix_new "$1"
    fix_session 4 andre 1000 seat0 1 user active yes
    fix_session 7 bob   1001 seat0 2 user online no
    fix_session 2 andre 1000 ''    '' manager active yes
    fix_seat 4
    printf '4\n' > "$FIX/state/self"
}

run_engine() {  # rest = argv
    env RIME_FIX="$FIX" \
        RIME_SWITCH_TOOLS="$FIX/tools" \
        RIME_SWITCH_SESSION="${SESSION_OVERRIDE:-4}" \
        RIME_SWITCH_PASSWD="$FIX/passwd" \
        RIME_SWITCH_LOGIN_DEFS="$FIX/login.defs" \
        RIME_SWITCH_GREETD_CONFIG="$FIX/greetd.toml" \
        RIME_SWITCH_HELPER="$HELPER" \
        RIME_SWITCH_LOCK_TRIES="${LOCK_TRIES_OVERRIDE:-3}" \
        RIME_SWITCH_LOCK_SLEEP=0 \
        RIME_FIX_SUDO_UID="${FIX_SUDO_UID:-1000}" \
        bash "$ENGINE" "$@"
}

run_helper() {  # rest = argv
    env RIME_FIX="$FIX" \
        RIME_SWITCH_GREETER_TOOLS="$FIX/tools" \
        RIME_SWITCH_GREETER_RUNDIR="$FIX/run" \
        RIME_SWITCH_GREETER_DEVDIR="$FIX/dev" \
        RIME_SWITCH_GREETD_CONFIG="$FIX/greetd.toml" \
        RIME_SWITCH_LOGIND_CONF="$FIX/logind.conf" \
        RIME_SWITCH_PASSWD="$FIX/passwd" \
        RIME_SWITCH_LOGIN_DEFS="$FIX/login.defs" \
        SUDO_UID="${HELPER_SUDO_UID:-1000}" \
        bash "$HELPER" "$@"
}

actions() { cat "$FIX/state/actions"; }

# ─────────────────────────────────────────────────────────────────────────────
section "the shipped files parse"
# ─────────────────────────────────────────────────────────────────────────────
for f in "$ENGINE" "$HELPER" "$USER_ENGINE"; do
    bash -n "$f" 2>"$WORK/syn" \
        && ok "${f##*/} is syntactically valid bash" \
        || bad "${f##*/} is syntactically valid bash" "$(cat "$WORK/syn")"
done
[ -x "$ENGINE" ] && ok "rime-switch-user is executable" || bad "rime-switch-user is executable"
[ -x "$HELPER" ] && ok "rime-switch-greeter is executable" || bad "rime-switch-greeter is executable"

# ─────────────────────────────────────────────────────────────────────────────
section "the unit cannot become this machine's login path"
# ─────────────────────────────────────────────────────────────────────────────
# greetd.service is boot-critical. A switch greeter that could be enabled, be
# pulled in by a target, or answer to display-manager.service would be a second
# thing racing for VT 1, and the failure mode is a machine nobody can log in to.
grep -qE '^\[Install\]' "$UNIT" \
    && bad "the switch-greeter unit has no [Install] section" "it has one, so it can be enabled" \
    || ok "the switch-greeter unit has no [Install] section"
grep -qE '^Alias=' "$UNIT" \
    && bad "the switch-greeter unit aliases nothing (greetd.service is display-manager.service)" "$(grep -E '^Alias=' "$UNIT")" \
    || ok "the switch-greeter unit aliases nothing (greetd.service is display-manager.service)"
grep -qE '^ConditionPathExists=/run/rime-switch-greeter/%i\.toml$' "$UNIT" \
    && ok "the unit starts only when the helper has written its config" \
    || bad "the unit starts only when the helper has written its config"
grep -qE '^Conflicts=getty@tty%i\.service$' "$UNIT" \
    && ok "the unit conflicts with the getty on its own VT" \
    || bad "the unit conflicts with the getty on its own VT"
grep -qE '^Restart=no$' "$UNIT" \
    && ok "the switch greeter does not respawn for ever" \
    || bad "the switch greeter does not respawn for ever"
grep -qE '^ExecStart=/usr/bin/greetd --config /run/rime-switch-greeter/%i\.toml --socket-path /run/rime-switch-greeter/%i\.sock$' "$UNIT" \
    && ok "the unit runs greetd with its own config AND its own socket" \
    || bad "the unit runs greetd with its own config AND its own socket"

# ─────────────────────────────────────────────────────────────────────────────
section "the sudoers grant"
# ─────────────────────────────────────────────────────────────────────────────
grep -qE '^ALL ALL=\(root\) NOPASSWD: /usr/libexec/rime-switch-greeter ""$' "$SUDOERS" \
    && ok "the grant is the helper, for every local account, with no arguments" \
    || bad "the grant is the helper, for every local account, with no arguments" "$(grep -v '^#' "$SUDOERS")"
grep -qE '^[^#]*%wheel' "$SUDOERS" \
    && bad "the grant is not restricted to wheel" "a standard account must be able to switch users" \
    || ok "the grant is not restricted to wheel"
# The helper must actually refuse arguments, or the empty-string grant is the
# only thing between a caller and an argv this file never audited.
fix_standard sudoers-args
out="$(run_helper --whatever 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "the helper refuses arguments (exit 2)" \
    || bad "the helper refuses arguments (exit 2)" "rc=$rc"

# ─────────────────────────────────────────────────────────────────────────────
section "switch list"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard list
out="$(run_engine list 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "list exits 0" || bad "list exits 0" "rc=$rc: $out"
printf '%s\n' "$out" | pipe_has 'andre (this session)' \
    && ok "list marks the caller's own session" \
    || bad "list marks the caller's own session" "$out"
printf '%s\n' "$out" | pipe_has -E '^bob +session +7 +2 ' \
    && ok "list shows another account's session on this seat, with its VT" \
    || bad "list shows another account's session on this seat, with its VT" "$out"
printf '%s\n' "$out" | pipe_has -E '^alice +account' \
    && ok "list shows an account with no session as an account" \
    || bad "list shows an account with no session as an account" "$out"
printf '%s\n' "$out" | pipe_has 'locked-out' \
    && bad "list hides an account that cannot start a session" "$out" \
    || ok "list hides an account that cannot start a session"
printf '%s\n' "$out" | pipe_has 'greetd' \
    && bad "list hides system accounts" "$out" \
    || ok "list hides system accounts"
printf '%s\n' "$out" | pipe_has -E 'manager' \
    && bad "list hides the user-manager sessions logind keeps per account" "$out" \
    || ok "list hides the user-manager sessions logind keeps per account"

# A session on ANOTHER seat is not a switch target: activating is seat-local.
fix_standard list-otherseat
fix_session 9 alice 1002 seat1 3 user online no
fix_seat 4
out="$(run_engine list 2>&1)"
printf '%s\n' "$out" | pipe_has -E '^alice +session' \
    && bad "list does not offer a session on another seat as a session" "$out" \
    || ok "list does not offer a session on another seat as a session"

fix_standard list-json
out="$(run_engine list --json 2>&1)"
printf '%s' "$out" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if any(r["name"]=="bob" and r["kind"]=="session" and r["vt"]=="2" for r in d) else 1)' \
    && ok "list --json is valid JSON and carries the same rows" \
    || bad "list --json is valid JSON and carries the same rows" "$out"

# ─────────────────────────────────────────────────────────────────────────────
section "the fast path — the target is already logged in"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard to-bob
out="$(run_engine to bob 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "switching to a logged-in account exits 0" || bad "switching to a logged-in account exits 0" "rc=$rc: $out"
grep -qx 'loginctl activate 7' "$FIX/state/actions" \
    && ok "it activates the target's existing session rather than starting one" \
    || bad "it activates the target's existing session rather than starting one" "$(actions)"
grep -q 'sudo' "$FIX/state/actions" \
    && bad "it does not open a login screen for an account that is already logged in" "$(actions)" \
    || ok "it does not open a login screen for an account that is already logged in"
# Order is the assertion, not presence: locking after the switch is no lock.
lock_line="$(grep -n 'loginctl lock-session 4' "$FIX/state/actions" | head -1 | cut -d: -f1)"
act_line="$(grep -n 'loginctl activate 7' "$FIX/state/actions" | head -1 | cut -d: -f1)"
if [ -n "$lock_line" ] && [ -n "$act_line" ] && [ "$lock_line" -lt "$act_line" ]; then
    ok "this session is locked BEFORE the seat moves"
else
    bad "this session is locked BEFORE the seat moves" "lock at line ${lock_line:-none}, activate at line ${act_line:-none}"
fi
printf '%s\n' "$out" | pipe_has 'VT 2' \
    && ok "it says which VT the target is on" \
    || bad "it says which VT the target is on" "$out"

fix_standard to-bob-plan
out="$(run_engine to bob --plan 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "--plan exits 0" || bad "--plan exits 0" "rc=$rc: $out"
printf '%s\n' "$out" | pipe_has 'would run:.*loginctl activate 7' \
    && ok "--plan prints the activate it would run" \
    || bad "--plan prints the activate it would run" "$out"
# Reads are allowed under --plan and are in the log too: a plan that could not
# read the machine could not say what it would do. MUTATIONS are the assertion.
if grep -qE 'lock-session|activate|^sudo |^systemctl ' "$FIX/state/actions"; then
    bad "--plan changes nothing" "$(actions)"
else
    ok "--plan changes nothing"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "the lock is waited for, not assumed"
# ─────────────────────────────────────────────────────────────────────────────
# The reason this suite exists. `lock-session` is a request; LockedHint is the
# answer. A machine whose locker never answers must not lose the seat.
fix_standard nolock
: > "$FIX/state/nolockflip"
out="$(run_engine to bob 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "a session that will not lock refuses to switch (exit 2)" \
    || bad "a session that will not lock refuses to switch (exit 2)" "rc=$rc: $out"
grep -q 'loginctl activate' "$FIX/state/actions" \
    && bad "a session that will not lock does not move the seat" "$(actions)" \
    || ok "a session that will not lock does not move the seat"
printf '%s\n' "$out" | pipe_has 'LockedHint' \
    && ok "the refusal names the hint it waited on" \
    || bad "the refusal names the hint it waited on" "$out"

fix_standard nolock-override
: > "$FIX/state/nolockflip"
out="$(run_engine to bob --no-lock 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "--no-lock switches anyway" || bad "--no-lock switches anyway" "rc=$rc: $out"
grep -q 'loginctl lock-session' "$FIX/state/actions" \
    && bad "--no-lock does not even ask for the lock" "$(actions)" \
    || ok "--no-lock does not even ask for the lock"
printf '%s\n' "$out" | pipe_has -i 'unlocked' \
    && ok "--no-lock says what it is leaving behind" \
    || bad "--no-lock says what it is leaving behind" "$out"

# ─────────────────────────────────────────────────────────────────────────────
section "refusals"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard deny-self
out="$(run_engine to andre 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "switching to yourself is refused" || bad "switching to yourself is refused" "rc=$rc: $out"

fix_standard deny-absent
out="$(run_engine to nobodyhere 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "an account that does not exist is refused" || bad "an account that does not exist is refused" "rc=$rc: $out"

fix_standard deny-root
out="$(run_engine to root 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "root is refused" || bad "root is refused" "rc=$rc: $out"

fix_standard deny-system
out="$(run_engine to greetd 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "a system account is refused" || bad "a system account is refused" "rc=$rc: $out"

fix_standard deny-nologin
out="$(run_engine to locked-out 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "an account with a nologin shell is refused" || bad "an account with a nologin shell is refused" "rc=$rc: $out"
printf '%s\n' "$out" | pipe_has 'nologin' \
    && ok "the nologin refusal says which shell" || bad "the nologin refusal says which shell" "$out"

fix_standard deny-otherseat
fix_session 9 alice 1002 seat1 3 user online no
out="$(run_engine to alice 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "a target on another seat is refused" || bad "a target on another seat is refused" "rc=$rc: $out"
printf '%s\n' "$out" | pipe_has 'seat-local' \
    && ok "the other-seat refusal says why" || bad "the other-seat refusal says why" "$out"
grep -q 'sudo' "$FIX/state/actions" \
    && bad "a target on another seat does not fall through to a login screen" "$(actions)" \
    || ok "a target on another seat does not fall through to a login screen"

# A session with no seat — ssh, or a systemd job. There is no VT to leave.
fix_standard deny-noseat
fix_session 11 andre 1000 '' '' user active no
SESSION_OVERRIDE=11
out="$(run_engine to bob 2>&1)"; rc=$?
unset SESSION_OVERRIDE
[ "$rc" -eq 2 ] && ok "a session with no seat is refused" || bad "a session with no seat is refused" "rc=$rc: $out"

# ─────────────────────────────────────────────────────────────────────────────
section "a kiosk machine refuses to switch — criterion 3 meets criterion 1"
# ─────────────────────────────────────────────────────────────────────────────
# Fast switching is the one change in this round that could WEAKEN the kiosk
# boundary: a kiosk account that can summon a login screen has escaped. Both
# engines read the live greetd config and refuse.
fix_standard kiosk-default
cat > "$FIX/greetd.toml" <<'K'
[terminal]
vt = 1

[default_session]
command = "sway --unsupported-gpu -c /usr/share/rime/shared-machine/sway-kiosk.conf"
user = "bob"
K
out="$(run_engine to alice 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "a kiosk config refuses the switch" || bad "a kiosk config refuses the switch" "rc=$rc: $out"
printf '%s\n' "$out" | pipe_has -i 'kiosk' \
    && ok "the kiosk refusal says so" || bad "the kiosk refusal says so" "$out"
grep -qE 'activate|sudo' "$FIX/state/actions" \
    && bad "a kiosk config runs nothing" "$(actions)" \
    || ok "a kiosk config runs nothing"

fix_standard kiosk-initial
cat >> "$FIX/greetd.toml" <<'K'

[initial_session]
command = "sway"
user = "bob"
K
out="$(run_engine greeter 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "an auto-login config refuses the switch" || bad "an auto-login config refuses the switch" "rc=$rc: $out"

# The absence assertion that CAN fail: the same check, fed the shipped login
# config, must say this machine is not a kiosk. Without this the two above
# would pass on an engine that refused everything.
fix_standard kiosk-shipped
cp "$GREET_CONF" "$FIX/greetd.toml"
out="$(run_engine to bob 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the SHIPPED greeter config is not a kiosk and the switch runs" \
    || bad "the SHIPPED greeter config is not a kiosk and the switch runs" "rc=$rc: $out"
# And the shipped KIOSK recipe is one, read by the same code.
fix_standard kiosk-recipe
sed 's/^user = "rime-kiosk"/user = "bob"/' "$KIOSK_CONF" > "$FIX/greetd.toml"
out="$(run_engine to alice 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "the SHIPPED kiosk recipe is read as a kiosk" \
    || bad "the SHIPPED kiosk recipe is read as a kiosk" "rc=$rc: $out"

# ─────────────────────────────────────────────────────────────────────────────
section "the greeter path — the target is not logged in"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard to-alice
out="$(run_engine to alice 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "switching to an account with no session exits 0" || bad "switching to an account with no session exits 0" "rc=$rc: $out"
grep -q "sudo -n $HELPER" "$FIX/state/actions" \
    && ok "it reaches the helper through sudo -n, with no arguments" \
    || bad "it reaches the helper through sudo -n, with no arguments" "$(actions)"
grep -q 'systemctl start rime-switch-greeter@' "$FIX/state/actions" \
    && ok "the helper starts a switch-greeter unit" \
    || bad "the helper starts a switch-greeter unit" "$(actions)"
lock_line="$(grep -n 'loginctl lock-session 4' "$FIX/state/actions" | head -1 | cut -d: -f1)"
sudo_line="$(grep -n '^sudo ' "$FIX/state/actions" | head -1 | cut -d: -f1)"
if [ -n "$lock_line" ] && [ -n "$sudo_line" ] && [ "$lock_line" -lt "$sudo_line" ]; then
    ok "the session is locked before the login screen is opened"
else
    bad "the session is locked before the login screen is opened" "lock ${lock_line:-none}, sudo ${sudo_line:-none}"
fi

# A greeter that is already sitting on a spare VT is activated, not doubled.
fix_standard greeter-reuse
fix_session 12 greetd 987 seat0 7 greeter online no
out="$(run_engine greeter 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the greeter verb exits 0 when one is already open" || bad "the greeter verb exits 0 when one is already open" "rc=$rc: $out"
grep -qx 'loginctl activate 12' "$FIX/state/actions" \
    && ok "an open login screen is activated rather than a second one started" \
    || bad "an open login screen is activated rather than a second one started" "$(actions)"
grep -q 'sudo' "$FIX/state/actions" \
    && bad "reusing an open login screen needs no privilege at all" "$(actions)" \
    || ok "reusing an open login screen needs no privilege at all"

fix_standard greeter-new
out="$(run_engine greeter 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the greeter verb opens one when there is none" || bad "the greeter verb opens one when there is none" "rc=$rc: $out"
grep -q 'systemctl start rime-switch-greeter@' "$FIX/state/actions" \
    && ok "opening a login screen starts the unit" || bad "opening a login screen starts the unit" "$(actions)"

# ─────────────────────────────────────────────────────────────────────────────
section "the helper picks a VT, and which one is not an accident"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard helper-vt
out="$(run_helper 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the helper exits 0 for the session at the seat" || bad "the helper exits 0 for the session at the seat" "rc=$rc: $out"
vt="$(printf '%s\n' "$out" | sed -n 's/^vt=//p')"
[ -n "$vt" ] && [ "$vt" -ge 7 ] \
    && ok "the VT is above logind's autovt/reserved floor (got ${vt:-none})" \
    || bad "the VT is above logind's autovt/reserved floor" "$out"
[ -f "$FIX/run/$vt.toml" ] && ok "the helper wrote the instance config" || bad "the helper wrote the instance config"
grep -qE "^vt = $vt$" "$FIX/run/$vt.toml" && ok "the config names the VT it picked" || bad "the config names the VT it picked" "$(cat "$FIX/run/$vt.toml" 2>/dev/null)" "$FIX/state/actions"
grep -qE '^switch = true$' "$FIX/run/$vt.toml" && ok "the config switches to that VT" || bad "the config switches to that VT" "$FIX/state/actions"
grep -qE "^runfile = \"$FIX/run/$vt.run\"$" "$FIX/run/$vt.toml" && ok "the instance has its own runfile, not greetd's" || bad "the instance has its own runfile, not greetd's" "$(cat "$FIX/run/$vt.toml" 2>/dev/null)" "$FIX/state/actions"
grep -q 'initial_session' "$FIX/run/$vt.toml" "$FIX/state/actions" \
    && bad "the generated config has no auto-login stanza" "$(cat "$FIX/run/$vt.toml")" \
    || ok "the generated config has no auto-login stanza"
grep -qE '^command = "/usr/libexec/rime-greet-session sway --unsupported-gpu -c /usr/share/rime-greet/sway-greet.conf"$' "$FIX/run/$vt.toml" "$FIX/state/actions" \
    && ok "the greeter command is copied verbatim from the live config" \
    || bad "the greeter command is copied verbatim from the live config" "$(cat "$FIX/run/$vt.toml" 2>/dev/null)"
grep -qE '^user = "greetd"$' "$FIX/run/$vt.toml" "$FIX/state/actions" \
    && ok "the greeter user is copied from the live config" || bad "the greeter user is copied from the live config"
grep -qx "systemctl start rime-switch-greeter@$vt.service" "$FIX/state/actions" \
    && ok "the unit it starts is the instance for that VT" || bad "the unit it starts is the instance for that VT" "$(actions)"

# A VT already carrying a session is skipped.
fix_standard helper-vt-taken
fix_session 13 bob 1001 seat0 7 user online no
out="$(run_helper 2>&1)"
vt="$(printf '%s\n' "$out" | sed -n 's/^vt=//p')"
[ "$vt" = 8 ] && ok "a VT that already has a session is skipped (picked $vt)" || bad "a VT that already has a session is skipped" "$out"

# The floor is read off logind's configuration rather than hardcoded.
fix_standard helper-vt-floor
printf 'NAutoVTs=8\nReserveVT=8\n' > "$FIX/logind.conf"
out="$(run_helper 2>&1)"
vt="$(printf '%s\n' "$out" | sed -n 's/^vt=//p')"
[ "$vt" = 9 ] && ok "NAutoVTs moves the floor (picked $vt with NAutoVTs=8)" || bad "NAutoVTs moves the floor" "$out"

# Nothing left to try is a failure, not a silent pick.
fix_standard helper-vt-none
printf 'NAutoVTs=6\nReserveVT=6\n' > "$FIX/logind.conf"
fix_session 14 bob 1001 seat0 7 user online no
out="$(env RIME_FIX="$FIX" RIME_SWITCH_GREETER_TOOLS="$FIX/tools" RIME_SWITCH_GREETER_RUNDIR="$FIX/run" \
        RIME_SWITCH_GREETER_DEVDIR="$FIX/dev" RIME_SWITCH_GREETD_CONFIG="$FIX/greetd.toml" \
        RIME_SWITCH_LOGIND_CONF="$FIX/logind.conf" RIME_SWITCH_PASSWD="$FIX/passwd" \
        RIME_SWITCH_LOGIN_DEFS="$FIX/login.defs" RIME_SWITCH_GREETER_VT_MAX=7 SUDO_UID=1000 \
        bash "$HELPER" 2>&1)"; rc=$?
[ "$rc" -eq 1 ] && ok "no free VT is a failure (exit 1), not a silent pick" || bad "no free VT is a failure (exit 1), not a silent pick" "rc=$rc: $out"
grep -q 'systemctl start' "$FIX/state/actions" \
    && bad "no free VT starts nothing" "$(actions)" || ok "no free VT starts nothing"

# ─────────────────────────────────────────────────────────────────────────────
section "the helper refuses a caller who is not at the seat"
# ─────────────────────────────────────────────────────────────────────────────
# Without this, any local account — one on another VT, one over ssh — could
# take the screen from whoever is at the keyboard, and the lock the engine
# takes first would never have happened.
fix_standard helper-notatseat
out="$(HELPER_SUDO_UID=1001 run_helper 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "a caller who does not hold the seat is refused" || bad "a caller who does not hold the seat is refused" "rc=$rc: $out"
grep -q 'systemctl start' "$FIX/state/actions" \
    && bad "a refused caller starts nothing" "$(actions)" || ok "a refused caller starts nothing"
printf '%s\n' "$out" | pipe_has 'at the keyboard' \
    && ok "the refusal says what the rule is" || bad "the refusal says what the rule is" "$out"

fix_standard helper-atseat
out="$(HELPER_SUDO_UID=1000 run_helper 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "the caller who DOES hold the seat is accepted" || bad "the caller who DOES hold the seat is accepted" "rc=$rc: $out"

fix_standard helper-kiosk
sed -i 's/^user = "greetd"$/user = "bob"/' "$FIX/greetd.toml"
out="$(run_helper 2>&1)"; rc=$?
[ "$rc" -eq 2 ] && ok "the helper refuses on a kiosk machine too" || bad "the helper refuses on a kiosk machine too" "rc=$rc: $out"

fix_standard helper-reuse
fix_session 12 greetd 987 seat0 7 greeter online no
out="$(run_helper 2>&1)"; rc=$?
printf '%s\n' "$out" | pipe_has -x 'vt=7' && ok "the helper reuses an open greeter" || bad "the helper reuses an open greeter" "$out"
grep -q 'systemctl start' "$FIX/state/actions" \
    && bad "reusing an open greeter starts no second greetd" "$(actions)" || ok "reusing an open greeter starts no second greetd"

# The BOOT greeter on VT 1 is not a switch target: it is the login screen this
# user's own session replaced, and activating it would land them on their own VT.
fix_standard helper-bootgreeter
fix_session 3 greetd 987 seat0 1 greeter online no
out="$(run_helper 2>&1)"
vt="$(printf '%s\n' "$out" | sed -n 's/^vt=//p')"
[ "$vt" -ge 7 ] 2>/dev/null \
    && ok "a greeter on VT 1 is not reused as the switch greeter (picked $vt)" \
    || bad "a greeter on VT 1 is not reused as the switch greeter" "$out"

fix_standard helper-unitreuse
printf 'rime-switch-greeter@9.service loaded active running Rime fast-user-switch login screen on VT 9\n' > "$FIX/state/units"
out="$(run_helper 2>&1)"
printf '%s\n' "$out" | pipe_has -x 'vt=9' \
    && ok "a unit that is up but has no session yet is reused" || bad "a unit that is up but has no session yet is reused" "$out"

# ─────────────────────────────────────────────────────────────────────────────
section "both engines refuse their test override when run as root"
# ─────────────────────────────────────────────────────────────────────────────
# Shape, not behaviour: this suite does not run as root. The refusal is what
# stops RIME_SWITCH_TOOLS being a way to point a privileged run at somebody
# else's binaries, and it is the same fence rime-user carries.
grep -q 'RIME_SWITCH_TOOLS is set and this is running as root' "$ENGINE" "$FIX/state/actions" \
    && ok "rime-switch-user refuses RIME_SWITCH_TOOLS as root" || bad "rime-switch-user refuses RIME_SWITCH_TOOLS as root"
grep -q 'RIME_SWITCH_GREETER_TOOLS is set and this is running as root' "$HELPER" "$FIX/state/actions" \
    && ok "rime-switch-greeter refuses RIME_SWITCH_GREETER_TOOLS as root" || bad "rime-switch-greeter refuses RIME_SWITCH_GREETER_TOOLS as root"

# ─────────────────────────────────────────────────────────────────────────────
section "rime user switch reaches this engine"
# ─────────────────────────────────────────────────────────────────────────────
fix_standard dispatch
cat > "$FIX/stub-engine" <<'STUB'
#!/usr/bin/env bash
printf 'stub-engine got:%s\n' "$(printf ' %s' "$@")"
STUB
chmod 0755 "$FIX/stub-engine"
out="$(RIME_SWITCH_ENGINE="$FIX/stub-engine" bash "$USER_ENGINE" switch to bob --plan 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && printf '%s\n' "$out" | pipe_has -x 'stub-engine got: to bob --plan' \
    && ok "rime user switch execs the switch engine with the argv unchanged" \
    || bad "rime user switch execs the switch engine with the argv unchanged" "rc=$rc: $out"
usage_out="$(bash "$USER_ENGINE" 2>&1)"
case "$usage_out" in
    *'switch list|to <name>|greeter'*) ok "rime-user's usage documents switch" ;;
    *) bad "rime-user's usage documents switch" "$usage_out" ;;
esac

printf '\nrime-switch-user: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
