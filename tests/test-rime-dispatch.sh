#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-dispatch.sh — assertions against the REAL `rime` binary for
#  §20's remote compute and handoff: `rime build --on`, `rime send`,
#  `rime open`, and the `--host` forms of `rime agent`.
#
#  Nothing here re-implements the CLI. Every case runs the real built binary.
#
#  ── The refusal this suite exists for ──────────────────────────────────────
#  `rime build --on katana` has to decide which directory on the far side is
#  this project, and the failure mode of a wrong answer is not a crash — it is
#  a build that SUCCEEDS against the wrong source. So the largest group of
#  cases below is about the same-repository check: that it happens, that it
#  refuses on a mismatch, that the refusal names both origins, and that
#  `--remote-path` skips it and says so.
#
#  ── Why the fake ssh answers in scenarios ──────────────────────────────────
#  A fake `ssh` first on PATH plays the far side. Each scenario is one thing
#  the remote could be — the same repo, a different repo, no such directory, a
#  machine at its greeter — which is the only way to exercise refusals that
#  need another computer to be in a particular state. It records every argv, so
#  the quoting of `cd <dir> && <cmd>` is checkable too.
#
#  It also means the suite touches no network. The live equivalents of these
#  cases were run by hand against the developer's katana, which has this repo
#  at the same path; that proved the design, and this proves it stays.
#
#  ── What it deliberately does NOT do ───────────────────────────────────────
#  No network, no root, no writes outside a temp directory, and no read or
#  write of the developer's own ~/.config/rime/hosts.toml. There is a final
#  assertion that the real registry is byte-identical afterwards.
#
#  PASS = every verb dispatches what it should, every refusal refuses, and the
#         same-repository check cannot be skipped by accident.
#
#  Run from anywhere: ./tests/test-rime-dispatch.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e`: this suite COUNTS failures and many cases run commands that exit
# non-zero on purpose. GitHub Actions runs a script as `bash -e {0}`, under
# which the first such command would end the run and report nothing.
set +e
cd "$(dirname "$0")" || exit 2
REPO=$(cd .. && pwd)

pass=0; fail=0
ok()  { printf 'PASS  %-60s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-60s %s\n' "$1" "$2"; fail=$((fail+1)); }
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

# A missing prerequisite is a FAILURE, never a skip: a suite reporting
# "0 passed, 0 failed" with a green tick has asserted nothing.
for tool in git python3 tar; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required" >&2; exit 2
    }
done

RIME_BIN=${RIME_BIN:-$REPO/rimed/target/debug/rime}
if [ ! -x "$RIME_BIN" ]; then
    echo "building the rime binary (not found at $RIME_BIN)…"
    ( cd "$REPO/rimed" && cargo build --locked --bin rime ) || {
        echo "FATAL: could not build the rime binary" >&2; exit 2
    }
fi
[ -x "$RIME_BIN" ] || { echo "FATAL: no rime binary at $RIME_BIN" >&2; exit 2; }

WORK=$(mktemp -d /tmp/rime-dispatch-test.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

REAL_REG="${XDG_CONFIG_HOME:-$HOME/.config}/rime/hosts.toml"
REAL_BEFORE=$WORK/real-before
if [ -f "$REAL_REG" ]; then cp "$REAL_REG" "$REAL_BEFORE"; else : > "$REAL_BEFORE"; fi

export XDG_CONFIG_HOME=$WORK/config
export XDG_STATE_HOME=$WORK/state
mkdir -p "$XDG_CONFIG_HOME/rime"

# ── the registry under test ─────────────────────────────────────────────────
cat > "$XDG_CONFIG_HOME/rime/hosts.toml" <<'TOML'
[host.katana]
note = "the fake one"
TOML

# A probe cache saying the host has the agent runtime, so the `--host` forms of
# `rime agent` are not refused before they dispatch.
mkdir -p "$XDG_STATE_HOME/rime/hosts"
cat > "$XDG_STATE_HOME/rime/hosts/katana.json" <<'JSON'
{"probed_at":1,"rime_version":"0.1.0","variant":"gaming","cpus":20,"agentd":true,"ai":false,"podman":true}
JSON

# ── the local project the dispatch is about ─────────────────────────────────
# A real git repository, because the code asks git real questions.
PROJ=$WORK/project
mkdir -p "$PROJ"
( cd "$PROJ" && git init -q && git config user.email t@t && git config user.name t \
  && git remote add origin https://github.com/AndreNijman/rime-os.git \
  && printf 'all:\n\t@echo built\n' > Makefile \
  && git add -A && git commit -qm init ) || { echo "FATAL: git fixture failed" >&2; exit 2; }

# ── the fake ssh ────────────────────────────────────────────────────────────
SSHLOG=$WORK/ssh
mkdir -p "$SSHLOG"
FAKEBIN=$WORK/fakebin
mkdir -p "$FAKEBIN"
cat > "$FAKEBIN/ssh" <<'EOF'
#!/usr/bin/env bash
n=$(find "$SSHLOG" -maxdepth 1 -type f | wc -l)
out="$SSHLOG/$(printf '%03d' "$n")"
for a in "$@"; do printf '%s\n' "$a" >> "$out"; done
last="${*: -1}"

# The project-identity probe. Recognised by the tokens it asks for rather than
# by position, so a change in how the command is assembled does not silently
# turn every scenario into the default.
case "$last" in
  *MISSING*NO_ORIGIN*|*NO_ORIGIN*MISSING*)
    case "${SSH_SCENARIO:-same}" in
      same)      echo "ORIGIN git@github.com:AndreNijman/rime-os.git" ;;
      other)     echo "ORIGIN https://github.com/someone/unrelated.git" ;;
      missing)   echo MISSING ;;
      no_origin) echo NO_ORIGIN ;;
      garbage)   echo "hello there" ;;
      silent)    exit 255 ;;
    esac
    exit 0
    ;;
esac

# The graphical-session probe. Matched on NO_BUS, a token that appears in no
# other script. An earlier version matched *wayland-* and so also caught the
# launch script, which sets WAYLAND_DISPLAY='wayland-1' — the value is
# lowercase, so the glob hit it and every launch was answered with a session
# reply and read as a failure.
case "$last" in
  *NO_BUS*)
    case "${SSH_SESSION:-ok}" in
      ok)         echo "SESSION /run/user/1000/bus wayland-1" ;;
      no_bus)     echo NO_BUS ;;
      greeter)    echo NO_SESSION ;;
      no_tool)    echo "NO_TOOL xdg-open" ;;
    esac
    exit 0
    ;;
esac

# A launch. Matched on `kill -0`, which only the launch script contains.
case "$last" in
  *"kill -0"*)
    case "${SSH_LAUNCH:-running}" in
      running) echo RUNNING ;;
      ok)      echo "EXIT 0" ;;
      failed)  echo "EXIT 3"; echo "xdg-open: no method available" >&2 ;;
    esac
    exit 0
    ;;
esac

# tar receiving files.
case "$last" in
  *"tar -x"*)
    cat > /dev/null
    if [ "${SSH_TAR:-ok}" = "exists" ]; then
      echo "tar: thing: Cannot open: File exists" >&2
      exit 2
    fi
    echo "INTO /var/home/andre/Downloads"
    exit 0
    ;;
esac
exit 0
EOF
chmod +x "$FAKEBIN/ssh"
export SSHLOG
export PATH="$FAKEBIN:$PATH"

resetssh() { rm -rf "$SSHLOG"; mkdir -p "$SSHLOG"; }
sshcount() { find "$SSHLOG" -maxdepth 1 -type f | wc -l | tr -d ' '; }
lastargv()  { cat "$SSHLOG/$(find "$SSHLOG" -maxdepth 1 -type f -printf '%f\n' | sort | tail -1)"; }
rime() { ( cd "$PROJ" && "$RIME_BIN" "$@" 2>&1 ); }

# ── the harness must be armed ───────────────────────────────────────────────
section "the harness itself"
if [ "$(command -v ssh)" = "$FAKEBIN/ssh" ]; then
    ok "ssh resolves to the fake, not the system one"
else
    bad "ssh resolves to the fake" "$(command -v ssh)"
fi
resetssh
rime build --on katana --dry-run >/dev/null
if [ "$(sshcount)" -ge 1 ]; then
    ok "a dispatch reaches the fake ssh (so the argv assertions mean something)"
else
    bad "a dispatch reaches the fake ssh" "no invocation recorded — every case below would be vacuous"
fi

# ── the same-repository check ───────────────────────────────────────────────
section "which directory on the far side is this project"
resetssh
out=$(SSH_SCENARIO=same rime build --on katana --dry-run)
if printf '%s' "$out" | pipe_has "building on katana at $PROJ"; then
    ok "the same absolute path is used when the remote agrees it is the same repo"
else
    bad "the same absolute path is used on agreement" "$out"
fi
if printf '%s' "$out" | pipe_has "Makefile is here"; then
    ok "the detected build command says which marker chose it"
else
    bad "the detected build command names its marker" "$out"
fi

resetssh
out=$(SSH_SCENARIO=other rime build --on katana --dry-run)
if printf '%s' "$out" | pipe_has "different repository"; then
    ok "a different repository at the same path is refused"
else
    bad "a different repository at the same path is refused" "$out"
fi
if printf '%s' "$out" | pipe_has "rime-os" && printf '%s' "$out" | pipe_has "unrelated"; then
    ok "the refusal prints both origins, so the user can see which is wrong"
else
    bad "the refusal prints both origins" "$out"
fi
if printf '%s' "$out" | pipe_has "nothing was run"; then
    ok "the refusal says nothing was run"
else
    bad "the refusal says nothing was run" "$out"
fi

# The ssh that would have run the build must NOT have happened. One invocation
# is the probe; a second would be the build itself.
if [ "$(sshcount)" = "1" ]; then
    ok "a refused dispatch runs the probe and nothing else"
else
    bad "a refused dispatch runs nothing else" "$(sshcount) ssh invocations"
fi

for scenario in missing no_origin; do
    resetssh
    out=$(SSH_SCENARIO=$scenario rime build --on katana --dry-run)
    if [ -n "$out" ] && ! printf '%s' "$out" | pipe_has "building on"; then
        ok "the remote answering $scenario refuses rather than dispatching"
    else
        bad "the remote answering $scenario refuses" "$out"
    fi
done

resetssh
out=$(SSH_SCENARIO=garbage rime build --on katana --dry-run)
if printf '%s' "$out" | pipe_has "unrecognised" && ! printf '%s' "$out" | pipe_has "building on"; then
    ok "an unrecognised probe answer refuses instead of being read as a state"
else
    bad "an unrecognised probe answer refuses" "$out"
fi

# ── --remote-path ───────────────────────────────────────────────────────────
section "the explicit override"
resetssh
out=$(SSH_SCENARIO=other rime build --on katana --dry-run --remote-path /somewhere/else)
if printf '%s' "$out" | pipe_has "building on katana at /somewhere/else"; then
    ok "--remote-path is obeyed even when the repo check would have failed"
else
    bad "--remote-path is obeyed" "$out"
fi
if printf '%s' "$out" | pipe_has "was not checked"; then
    ok "--remote-path says out loud that the repository was not checked"
else
    bad "--remote-path says the check was skipped" "$out"
fi
if [ "$(sshcount)" = "0" ]; then
    ok "--remote-path with --dry-run needs no round trip at all"
else
    bad "--remote-path with --dry-run needs no round trip" "$(sshcount) invocations"
fi
out=$(rime build --on katana --dry-run --remote-path relative/path)
if [ -n "$out" ] && printf '%s' "$out" | pipe_has "absolute"; then
    ok "a relative --remote-path is refused"
else
    bad "a relative --remote-path is refused" "$out"
fi

# ── the dirty worktree gate ─────────────────────────────────────────────────
section "uncommitted changes"
printf 'dirty\n' > "$PROJ/newfile"
resetssh
out=$(SSH_SCENARIO=same rime build --on katana --dry-run)
if printf '%s' "$out" | pipe_has "uncommitted change"; then
    ok "a dirty worktree refuses by default"
else
    bad "a dirty worktree refuses by default" "$out"
fi
if printf '%s' "$out" | pipe_has "would NOT be included"; then
    ok "the refusal explains that the remote builds its own committed state"
else
    bad "the refusal explains what the remote would build" "$out"
fi
if [ "$(sshcount)" = "0" ]; then
    ok "the local check happens before any round trip"
else
    bad "the local check happens before any round trip" "$(sshcount) invocations"
fi
out=$(SSH_SCENARIO=same rime build --on katana --dry-run --allow-dirty)
if printf '%s' "$out" | pipe_has "building on katana"; then
    ok "--allow-dirty proceeds"
else
    bad "--allow-dirty proceeds" "$out"
fi
rm -f "$PROJ/newfile"

# ── quoting of the dispatched command ───────────────────────────────────────
section "what actually reaches the remote shell"
resetssh
# The dry run PRINTS the command it would run, which is the thing to assert on.
# An earlier version read `lastargv` here, but with --dry-run the build never
# dispatches, so that was the identity probe's multi-line script and the
# assertion was reading its closing quote.
dry=$(SSH_SCENARIO=same rime build --on katana --dry-run -- make "a target")
remote=$(printf '%s\n' "$dry" | grep "^dry run: ")
argv=$(lastargv)
# Doubly quoted, and that is correct rather than a bug: the build is
# `sh -c '<inner>'`, so every quote inside <inner> is itself escaped for the
# outer layer. `a target` therefore appears as '\''a target'\'' — one argument
# at the inner level, which is what has to survive.
if printf '%s' "$remote" | pipe_has -F "'\''a target'\''"; then
    ok "an argument with a space survives as ONE argument through both quoting layers"
else
    bad "an argument with a space survives" "got [$remote]"
fi
# And it must not have become two arguments at the inner level.
if printf '%s' "$remote" | pipe_has -F "'\''a'\'' '\''target'\''"; then
    bad "the spaced argument did not split" "it split into two"
else
    ok "the spaced argument did not split into two"
fi
if printf '%s' "$remote" | pipe_has "cd '"; then
    ok "the project directory is quoted in the remote command"
else
    bad "the project directory is quoted" "got [$remote]"
fi
if printf '%s\n' "$argv" | pipe_has -x "BatchMode=yes"; then
    ok "a dispatch cannot block on a password prompt"
else
    bad "a dispatch passes BatchMode=yes" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi
if printf '%s\n' "$argv" | pipe_has "StrictHostKeyChecking"; then
    bad "a dispatch does not weaken host-key checking" "argv sets StrictHostKeyChecking"
else
    ok "a dispatch does not weaken host-key checking"
fi
# Under a test harness stdin is not a tty, so no pty must be requested — the
# thing that was printing "Pseudo-terminal will not be allocated".
if printf '%s\n' "$argv" | pipe_has -x -- "-T"; then
    ok "no terminal is requested when there is none to forward"
else
    bad "no terminal is requested when stdin is not a tty" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

# ── rime build, locally ─────────────────────────────────────────────────────
section "rime build without --on"
out=$(rime build --dry-run)
if printf '%s' "$out" | pipe_has "building here" && printf '%s' "$out" | pipe_has "make"; then
    ok "without --on it builds here, with the same detected command"
else
    bad "without --on it builds here" "$out"
fi
NOBUILD=$WORK/nobuild; mkdir -p "$NOBUILD"
out=$( cd "$NOBUILD" && "$RIME_BIN" build --dry-run 2>&1 )
if printf '%s' "$out" | pipe_has "Looked for"; then
    ok "a project with no recognised build system is a refusal that lists what it looked for"
else
    bad "an unrecognised project lists what it looked for" "$out"
fi

# ── rime open ───────────────────────────────────────────────────────────────
section "rime open"
resetssh
out=$(SSH_SESSION=ok SSH_LAUNCH=running rime open katana https://example.com)
if printf '%s' "$out" | pipe_has "opened on katana"; then
    ok "a launch that is still running is reported as opened"
else
    bad "a running launch is reported as opened" "$out"
fi
resetssh
out=$(SSH_SESSION=ok SSH_LAUNCH=failed rime open katana https://example.com)
if [ -n "$out" ] && ! printf '%s' "$out" | pipe_has "opened on"; then
    ok "a launch that exited non-zero is NOT reported as opened"
else
    bad "a failed launch is not reported as opened" "$out"
fi
if printf '%s' "$out" | pipe_has "no method available"; then
    ok "the remote's own error is shown as the reason"
else
    bad "the remote's error is shown" "$out"
fi
out=$(SSH_SESSION=greeter rime open katana https://example.com)
if printf '%s' "$out" | pipe_has "greeter"; then
    ok "a machine at its greeter is refused, not reported as opened"
else
    bad "a machine at its greeter is refused" "$out"
fi
out=$(SSH_SESSION=no_bus rime open katana https://example.com)
if printf '%s' "$out" | pipe_has -i "logged in"; then
    ok "a machine with nobody logged in says so"
else
    bad "a machine with nobody logged in says so" "$out"
fi
out=$(rime open katana -- "-oProxyCommand=x")
if [ -n "$out" ]; then
    ok "an option-like target is refused"
else
    bad "an option-like target is refused" "(accepted)"
fi

# The launch must not block, and must redirect the child's stdout. Both were
# real bugs: `--wait` hung for two minutes, and an unredirected stdout held the
# ssh channel open for a minute after a successful launch.
resetssh
SSH_SESSION=ok SSH_LAUNCH=running rime open katana https://example.com >/dev/null
# The whole recorded argv, not its last line: the launch script is multi-line,
# so `tail -1` was reading its closing quote and asserting nothing.
launch=$(lastargv)
# Matched as the redirect ON THE LAUNCHED COMMAND, not merely somewhere in the
# script. The first version of this grepped for ">/dev/null" alone and was
# satisfied by `kill -0 $pid 2>/dev/null` further down — so a mutant that
# removed the child's stdout redirect passed 55/0. The assertion has to name
# the shape it cares about.
if printf '%s' "$launch" | pipe_has -F '>/dev/null 2>"$err" &'; then
    ok "the backgrounded child has BOTH streams redirected, so ssh can close"
else
    bad "the child's stdout is redirected" "got [$launch]"
fi
if printf '%s' "$launch" | pipe_has -- "--wait"; then
    bad "the launch does not block on the program exiting" "argv contains --wait"
else
    ok "the launch does not block on the program exiting"
fi
if printf '%s' "$launch" | pipe_has "WAYLAND_DISPLAY"; then
    ok "WAYLAND_DISPLAY is set, without which a GUI has no display to reach"
else
    bad "WAYLAND_DISPLAY is set" "got [$launch]"
fi

# ── rime send ───────────────────────────────────────────────────────────────
section "rime send"
printf 'x\n' > "$WORK/thing.txt"
resetssh
out=$(SSH_TAR=ok "$RIME_BIN" send katana "$WORK/thing.txt" 2>&1)
if printf '%s' "$out" | pipe_has "sent 1 item"; then
    ok "a file is sent and the destination is reported"
else
    bad "a file is sent" "$out"
fi
# tar must be given -C <parent> <basename>, so the sender's directory layout is
# not recreated on the far side.
argv=$(lastargv)
if printf '%s\n' "$argv" | pipe_has "tar -x"; then
    ok "the remote extracts with tar"
else
    bad "the remote extracts with tar" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi
if printf '%s\n' "$argv" | pipe_has "keep-old-files"; then
    ok "the default refuses to overwrite (--keep-old-files)"
else
    bad "the default refuses to overwrite" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi
resetssh
out=$(SSH_TAR=ok "$RIME_BIN" send katana --force "$WORK/thing.txt" 2>&1)
if ! lastargv | pipe_has "keep-old-files"; then
    ok "--force drops the overwrite guard"
else
    bad "--force drops the overwrite guard" "still present"
fi
resetssh
out=$(SSH_TAR=exists "$RIME_BIN" send katana "$WORK/thing.txt" 2>&1)
if printf '%s' "$out" | pipe_has "Nothing was overwritten"; then
    ok "a conflict is reported as nothing overwritten"
else
    bad "a conflict is reported" "$out"
fi
if printf '%s' "$out" | pipe_has "Cannot open"; then
    ok "the remote tar's own message is folded in as the detail"
else
    bad "the remote message is folded in" "$out"
fi
out=$("$RIME_BIN" send katana "$WORK/does-not-exist" 2>&1)
if printf '%s' "$out" | pipe_has "does not exist"; then
    ok "a local file that does not exist is caught before any network use"
else
    bad "a missing local file is caught locally" "$out"
fi
out=$("$RIME_BIN" send katana 2>&1)
if printf '%s' "$out" | pipe_has "clipboard" || printf '%s' "$out" | pipe_has -i "nothing to send"; then
    ok "send with no paths and no --clipboard explains what is missing"
else
    bad "send with nothing says what is missing" "$out"
fi

# ── rime agent --host ───────────────────────────────────────────────────────
section "agent verbs forwarded to a device"
resetssh
"$RIME_BIN" agent list --host katana --json >/dev/null 2>&1
argv=$(lastargv)
if printf '%s\n' "$argv" | tail -1 | pipe_has "'rime' 'agent' 'list' '--json'"; then
    ok "agent list --host forwards the whole verb to the remote's own rime"
else
    bad "agent list --host forwards the verb" "$(printf '%s' "$argv" | tail -1)"
fi
if printf '%s\n' "$argv" | pipe_has -x -- "-T"; then
    ok "a forwarded listing asks for no terminal"
else
    bad "a forwarded listing asks for no terminal" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi
resetssh
"$RIME_BIN" agent attach --host katana 7 >/dev/null 2>&1
argv=$(lastargv)
if printf '%s\n' "$argv" | tail -1 | pipe_has "'rime' 'agent' 'attach' '7'"; then
    ok "agent attach --host forwards the remote's own session id"
else
    bad "agent attach --host forwards the id" "$(printf '%s' "$argv" | tail -1)"
fi
if printf '%s\n' "$argv" | pipe_has -x -- "-t"; then
    ok "attaching asks for a terminal, because it is the interactive case"
else
    bad "attaching asks for a terminal" "$(printf '%s' "$argv" | tr '\n' ' ')"
fi

# A host probed and found to lack the runtime is refused without a round trip.
cat > "$XDG_STATE_HOME/rime/hosts/katana.json" <<'JSON'
{"probed_at":1,"rime_version":"0.1.0","cpus":20,"agentd":false,"ai":false,"podman":true}
JSON
resetssh
out=$("$RIME_BIN" agent list --host katana 2>&1)
if printf '%s' "$out" | pipe_has "agent runtime"; then
    ok "a host known to lack the agent runtime is refused by name"
else
    bad "a host lacking the agent runtime is refused" "$out"
fi
if [ "$(sshcount)" = "0" ]; then
    ok "that refusal costs no ssh, because the answer was already on disk"
else
    bad "the refusal costs no ssh" "$(sshcount) invocations"
fi
# A host that has never been probed must NOT be refused: unknown is not absent.
rm -f "$XDG_STATE_HOME/rime/hosts/katana.json"
resetssh
"$RIME_BIN" agent list --host katana >/dev/null 2>&1
if [ "$(sshcount)" -ge 1 ]; then
    ok "an unprobed host is tried rather than refused (unknown is not absent)"
else
    bad "an unprobed host is tried" "refused without asking"
fi

# ── unknown hosts ───────────────────────────────────────────────────────────
section "unknown devices"
for verb in "build --on nosuch --dry-run" "send nosuch $WORK/thing.txt" "open nosuch https://x"; do
    # shellcheck disable=SC2086
    out=$( cd "$PROJ" && "$RIME_BIN" $verb 2>&1 )
    if printf '%s' "$out" | pipe_has "katana"; then
        ok "'$verb' names the devices that do exist"
    else
        bad "'$verb' names known devices" "$out"
    fi
done

# ── the machine running the tests ───────────────────────────────────────────
section "the machine running the tests"
if [ -f "$REAL_REG" ]; then cp "$REAL_REG" "$WORK/real-after"; else : > "$WORK/real-after"; fi
if same_file "$REAL_BEFORE" "$WORK/real-after"; then
    ok "the developer's own hosts.toml was not touched"
else
    bad "the developer's own hosts.toml was not touched" "IT CHANGED — see $REAL_REG"
fi

printf '\nrime dispatch: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
