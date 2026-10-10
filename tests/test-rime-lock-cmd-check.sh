#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-lock-cmd-check.sh — rime-shell-firstrun's check that hypridle's
#  lock_cmd reaches a shell (lock_cmd_reaches_shell).
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  A lock_cmd that reaches no shell stops the idle lock and nothing says so,
#  which is why firstrun fails the login's provisioning over it. Since the
#  rename there are two ways to get it wrong that the old check (the first
#  `qs -c` path must hold a shell.qml) could not tell apart:
#
#    * rime-session-migrate chains a customised lock_cmd as
#      `qs -c /usr/share/apex-shell … || qs -c /usr/share/rime-shell …`, so a
#      rollback to APEX still locks. Only the SECOND call reaches this image's
#      shell; the check must read past the first. (rime-rename: keep)
#    * /usr/share/apex-shell is this image's alias of its own shell. It has a
#      shell.qml, and an IPC call through it reaches nothing: quickshell finds
#      an instance by the path it is given ("No running instances"). A
#      lock_cmd that names only the alias passed the old check and never
#      locked. (rime-rename: keep)
#
#  The function is taken out of the shipped script and run against fixture
#  shells; a copy without the alias rule must fail the alias-only case.
#
#  Run from anywhere: ./tests/test-rime-lock-cmd-check.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e
cd "$(dirname "$0")/.." || exit 2

FIRSTRUN=files/system/libexec/rime-shell-firstrun
[ -f "$FIRSTRUN" ] || { echo "cannot find $FIRSTRUN"; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-lockcmd-test.XXXXXX") || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0 fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }

extract() { sed -n '/^lock_cmd_reaches_shell() {$/,/^}$/p' "$1"; }
FN="$(extract "$FIRSTRUN")"
if [ -z "$FN" ]; then
    echo "FAIL  lock_cmd_reaches_shell() not found in $FIRSTRUN"; exit 1
fi

# The image: the real shell, and the old name as a link to it.
IMG="$WORK/usr/share"
mkdir -p "$IMG/rime-shell"; echo 'ShellRoot {}' > "$IMG/rime-shell/shell.qml"
ln -s rime-shell "$IMG/apex-shell"   # rime-rename: keep — the image's alias
REAL="$IMG/rime-shell" ALIAS="$IMG/apex-shell"   # rime-rename: keep
# A home with a copy of the user's own (the L16's live shell) and a stale one.
H="$WORK/home"; mkdir -p "$H/.local/share/live" "$H/.local/src"
echo 'ShellRoot {}' > "$H/.local/share/live/shell.qml"

# verdict <function source> <lock_cmd> → "ok" or "fail"
verdict() {
    local conf="$WORK/hypridle.conf"
    printf 'general {\n    lock_cmd = %s\n}\n' "$2" > "$conf"
    if HOME="$H" SRC_DIR="$REAL" bash -c "set -euo pipefail; $1"'
       lock_cmd_reaches_shell "$0" >/dev/null' "$conf"; then
        echo ok
    else
        echo fail
    fi
}
is() { local got; got="$(verdict "$FN" "$3")"; if [ "$got" = "$2" ]; then ok "$1"; else bad "$1" "expected $2, got $got"; fi; }

is "the migrated chain: APEX's call first, this image's second"  ok \
   "qs -c $ALIAS ipc call lockscreen lock || qs -c $REAL ipc call lockscreen lock"
is "this image's shell alone"                                     ok \
   "qs -c $REAL ipc call lockscreen lock"
is "the alias alone reaches nothing"                              fail \
   "qs -c $ALIAS ipc call lockscreen lock"
is "…nor through -p and a shell.qml path"                         fail \
   "qs -p $ALIAS/shell.qml ipc call lockscreen lock"
is "a copy of the user's own, tilde-spelled, before the alias"    ok \
   "qs -p ~/.local/share/live ipc call lockscreen lock || qs -c $ALIAS ipc call lockscreen lock"
is "the developer tree that is not there"                         fail \
   "qs -c ~/.local/src/rime-shell ipc call lockscreen lock"
is "a path APEX had and this image has not"                       fail \
   "qs -c $WORK/usr/share/nothing-here ipc call lockscreen lock"
is "qs by absolute path counts as qs"                             ok \
   "/usr/bin/qs -c $REAL ipc call lockscreen lock"
is "--path= spelling"                                             ok \
   "qs --path=$REAL ipc call lockscreen lock"
is "no quickshell call at all: nothing to check"                  ok \
   "loginctl lock-session"

# The one rule the rename added, put back to what it replaced.
MUT="$(printf '%s\n' "$FN" | sed '/^        case "\$(readlink -f -- "\${p}")\/" in$/,/^        esac$/d')"
if [ "$MUT" = "$FN" ]; then
    bad "mutant: the alias rule removed" "the sed changed nothing; the mutant tests nothing"
elif [ "$(verdict "$MUT" "qs -c $ALIAS ipc call lockscreen lock")" = ok ]; then
    ok "caught: without the alias rule, a lock_cmd naming only the alias passes"
else
    bad "mutant: the alias rule removed" "the alias-only lock_cmd still fails; the case does not test the rule"
fi
# And the read past the first call.
MUT2="$(printf '%s\n' "$FN" | sed 's/^\(    cmd="\$(sed -n .*\)$/\1\n    cmd="${cmd%%||*}"/')"
if [ "$MUT2" = "$FN" ]; then
    bad "mutant: only the first call read" "the sed changed nothing; the mutant tests nothing"
elif [ "$(verdict "$MUT2" "qs -c $ALIAS ipc call lockscreen lock || qs -c $REAL ipc call lockscreen lock")" = fail ]; then
    ok "caught: reading only the first call fails the migrated chain"
else
    bad "mutant: only the first call read" "the chain still passes"
fi

# The function is used, and a failure fails the login's provisioning.
if grep -qE '^if \[ -f "\$\{_idle_conf\}" \] && ! _lock_paths="\$\(lock_cmd_reaches_shell "\$\{_idle_conf\}"\)"; then$' "$FIRSTRUN" \
   && grep -A1 -E '_lock_paths="\$\(lock_cmd_reaches_shell' "$FIRSTRUN" | pipe_has 'fail_check "hypridle lock_cmd reaches no shell'; then
    ok "firstrun runs it and fails provisioning when it says no"
else
    bad "firstrun runs it and fails provisioning when it says no"
fi

printf '\nlock-cmd-check: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
