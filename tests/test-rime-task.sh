#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-task.sh — assertions against the REAL `rime` binary for
#  `rime task` (roadmap §21's Task: the binder that references a project, a
#  capsule, a worktree, agents and a checkpoint).
#
#  Nothing here re-implements the CLI: every case runs the real built binary as
#  a process. It is a DEBUG build from this tree — RIME_BIN defaults to
#  rimed/target/debug/rime and the suite builds it if it is missing. The image
#  installs rimed/target/release/rime (Containerfile.base), which is the same
#  source and a different binary. Point RIME_BIN at a release build to assert
#  against one; the header used to claim that was already happening.
#
#  ── What is actually under test ─────────────────────────────────────────────
#  A task is worth having only if it tells the truth about the things it names,
#  so the interesting assertions are all about *refusals* and *observations*:
#
#    * a task whose capsule was deleted, whose worktree was removed or whose
#      checkpoint was pruned must be REFUSED BY NAME, not partly resumed. Each
#      of those cases greps the specific message and the specific recovery
#      command, never merely a non-zero exit — "it refused" being true while
#      "it refused because of the thing under test" is false is this
#      repository's most-repeated test bug.
#    * the four keys that exist only to be refused — `windows`, `permissions`,
#      `checkpoint`, `sandbox` — are hand-edited into the file and each must
#      produce its own message saying where the thing really lives. Those four
#      refusals ARE §21's design: no window list, no permission of any kind, no
#      generated state in the file a person edits, no stored weakening of
#      confinement.
#    * the window story is honest end to end. A layout is saved through the
#      real `rime project layout save` (with a faked compositor adapter, the
#      pattern test-project-layout.sh established), and then `rime task resume`
#      must REPORT it and must NOT reopen it. The window process writes a line
#      to a log when it starts, so "resume reopened nothing" is a counted fact
#      rather than an assumption.
#
#  ── The tripwire, and what it is armed against ──────────────────────────────
#  `sudo`, `pkexec`, `secret-tool`, `podman`, `distrobox`, `hyprctl` and `niri`
#  are recording stubs first on PATH, and every one of them must go uncalled.
#  That is the "never a polkit or keyring prompt" property asserted rather than
#  claimed, and it is also what proves the capsule check reads the engine's own
#  record instead of asking a container runtime. A negative control runs one of
#  the stubs directly and fails the suite if the recorder did not notice — a
#  tripwire that is never armed is the same failure as no tripwire.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  No root, no network, no D-Bus, no agent runtime, and no writes outside a
#  temp directory. XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_DATA_HOME and
#  XDG_RUNTIME_DIR are all redirected into $WORK, GIT_CONFIG_GLOBAL and
#  GIT_CONFIG_SYSTEM are pointed at /dev/null so the developer's own git
#  configuration (hooks, templates) cannot reach the fixture repository, and
#  the last section asserts that the real ~/.config/rime/tasks.toml and
#  ~/.local/state/rime/tasks were left exactly as they were.
#
#  PASS = every verb behaves, every refusal names the part that is missing and
#         the command that fixes it, no window is reopened, and none of the
#         privileged or container tools is invoked at all.
#
#  Run from anywhere: ./tests/test-rime-task.sh
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
ok()  { printf 'PASS  %-64s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-64s %s\n' "$1" "$2"; fail=$((fail+1)); }
section() { printf '\n── %s ──\n' "$1"; }

# A missing prerequisite is a FAILURE, never a skip. A suite that reports
# "0 passed, 0 failed" and a green tick has asserted precisely nothing, which
# has happened in this repository before.
command -v python3 >/dev/null 2>&1 || {
    echo "FATAL: python3 is required to validate the JSON output" >&2
    exit 2
}
command -v git >/dev/null 2>&1 || {
    echo "FATAL: git is required — a task binds a git working tree" >&2
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

WORK=$(mktemp -d /tmp/rime-task-test.XXXXXX)

# ── the developer's own tasks must be untouched ─────────────────────────────
# Captured before anything runs, compared at the end. TWO paths, because a task
# has two files: if `rime task` ever resolved either from $HOME instead of the
# XDG variables, this is what catches it — and the cost of not catching it is
# somebody's record of what they were working on.
REAL_TASKS="${XDG_CONFIG_HOME:-$HOME/.config}/rime/tasks.toml"
REAL_STATE="${XDG_STATE_HOME:-$HOME/.local/state}/rime/tasks"
REAL_TASKS_BEFORE=$WORK/real-tasks-before
REAL_STATE_BEFORE=$WORK/real-state-before
if [ -f "$REAL_TASKS" ]; then cp "$REAL_TASKS" "$REAL_TASKS_BEFORE"; else : > "$REAL_TASKS_BEFORE"; fi
if [ -d "$REAL_STATE" ]; then ls -1 "$REAL_STATE" > "$REAL_STATE_BEFORE" 2>/dev/null; else : > "$REAL_STATE_BEFORE"; fi

export XDG_CONFIG_HOME=$WORK/config
export XDG_STATE_HOME=$WORK/state
export XDG_DATA_HOME=$WORK/data
export XDG_RUNTIME_DIR=$WORK/run
mkdir -p "$XDG_RUNTIME_DIR"
# The fixture repository must not inherit the developer's hooks, templates or
# aliases. `checkpoint::create` supplies its own author, so no user.email is
# needed; a global core.hooksPath would still fire without this.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

# ── the recording stubs ─────────────────────────────────────────────────────
# Every one of these must go uncalled. They record and then FAIL, so a call
# that is somehow expected would also break the command that made it.
CALLS=$WORK/calls
FAKEBIN=$WORK/fakebin
mkdir -p "$FAKEBIN"
: > "$CALLS"
for tool in sudo pkexec secret-tool podman distrobox hyprctl niri; do
    cat > "$FAKEBIN/$tool" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "$tool" "\$*" >> "$CALLS"
exit 97
EOF
    chmod +x "$FAKEBIN/$tool"
done
export CALLS
export PATH="$FAKEBIN:$PATH"

rime() { "$RIME_BIN" "$@" 2>&1; }
calls() { wc -l < "$CALLS" | tr -d ' '; }

# The ordered steps out of a resume's printed plan: the lines after
# "resume it with:" up to the first note, which is prefixed with "- ".
plan_steps() { printf '%s\n' "$1" | awk '/^resume it with:/{f=1;next} f&&/^  - /{exit} f{print}' | sed 's/^  //'; }

# ── the fixture project ─────────────────────────────────────────────────────
PROJ=$WORK/rime-os
mkdir -p "$PROJ"
git -C "$PROJ" init -q 2>/dev/null
echo hello > "$PROJ/README.md"
git -C "$PROJ" add README.md >/dev/null 2>&1
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -qm init >/dev/null 2>&1
WT=$PROJ/.rime/worktrees/installer-bug

# ── prove the tripwire is armed ─────────────────────────────────────────────
section "the harness itself"
if [ "$(command -v podman)" = "$FAKEBIN/podman" ]; then
    ok "podman resolves to the recording stub, not a real container runtime"
else
    bad "podman resolves to the recording stub" "$(command -v podman)"
fi
# The negative control. Without it, "no privileged tool was called" could be
# true because the recorder never worked.
podman version >/dev/null 2>&1
if [ "$(calls)" = "1" ] && grep -q '^podman ' "$CALLS"; then
    ok "the recorder notices a call (negative control)"
else
    bad "the recorder notices a call (negative control)" "$(calls) recorded"
fi
: > "$CALLS"
if [ -d "$PROJ/.git" ]; then
    ok "the fixture repository was created"
else
    bad "the fixture repository was created" "no .git in $PROJ"
fi

# ── the empty state ─────────────────────────────────────────────────────────
section "no tasks yet"
out=$(rime task list); rc=$?
if [ "$rc" = "0" ] && printf '%s' "$out" | pipe_has "no tasks"; then
    ok "no task file at all is an empty list, not an error"
else
    bad "no task file at all is an empty list" "rc=$rc out=$out"
fi
if printf '%s' "$out" | pipe_has "rime task new"; then
    ok "the empty state says how to start one"
else
    bad "the empty state says how to start one" "$out"
fi

out=$(rime task show nosuch); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "rime task new"; then
    ok "an unknown task with no tasks at all says how to make one"
else
    bad "an unknown task with no tasks at all says how to make one" "rc=$rc out=$out"
fi

# ── new ─────────────────────────────────────────────────────────────────────
section "rime task new"
out=$(cd "$WORK" && rime task new outside-a-repo); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "git working tree"; then
    ok "a directory that is not a git working tree is refused, with the reason"
else
    bad "a directory that is not a git working tree is refused" "rc=$rc out=$out"
fi

out=$(cd "$PROJ" && rime task new installer-bug \
        --title "Fix Rime installer bug" \
        --env fedora-build --worktree installer-bug --agent claude); rc=$?
if [ "$rc" = "0" ]; then
    ok "a task is created inside a repository"
else
    bad "a task is created inside a repository" "rc=$rc out=$out"
fi
TASKS=$XDG_CONFIG_HOME/rime/tasks.toml
if grep -q "project = \"$PROJ\"" "$TASKS" 2>/dev/null; then
    ok "the repository root is stored, not the directory the command ran in"
else
    bad "the repository root is stored" "$(cat "$TASKS" 2>&1)"
fi
# The two parts that do not exist yet must be reported as missing straight
# away, so nobody discovers it at the first resume.
if printf '%s' "$out" | pipe_has "fedora-build  (GONE)"; then
    ok "a capsule that does not exist yet is reported as missing on creation"
else
    bad "a capsule that does not exist yet is reported on creation" "$out"
fi

out=$(cd "$PROJ" && rime task new installer-bug); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "rime task set installer-bug"; then
    ok "a duplicate id is refused and names the verb that changes it"
else
    bad "a duplicate id is refused and names the verb that changes it" "rc=$rc out=$out"
fi

out=$(cd "$PROJ" && rime task new bad-agent --agent nosuchagent); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "Known agents"; then
    ok "an agent this runtime cannot launch is refused and the real ones listed"
else
    bad "an agent this runtime cannot launch is refused" "rc=$rc out=$out"
fi
if printf '%s' "$out" | pipe_has "claude"; then
    ok "the refusal names a shipped adapter, so it is a real list"
else
    bad "the refusal names a shipped adapter" "$out"
fi

out=$(cd "$PROJ" && rime task new "../../etc/passwd"); rc=$?
if [ "$rc" -ne 0 ]; then
    ok "a traversing task id is refused"
else
    bad "a traversing task id is refused" "$out"
fi
if [ ! -e "$WORK/state/rime/tasks/../../etc" ]; then
    ok "the refused id created nothing outside the state directory"
else
    bad "the refused id created nothing outside the state directory" "it did"
fi

# ── resume, with two parts missing ──────────────────────────────────────────
section "resume refuses rather than half-working"
out=$(cd "$PROJ" && rime task resume installer-bug); rc=$?
if [ "$rc" -ne 0 ]; then
    ok "a task with missing parts exits non-zero"
else
    bad "a task with missing parts exits non-zero" "rc=$rc"
fi
# The specific message, not merely the refusal.
if printf '%s' "$out" | pipe_has 'capsule "fedora-build" has no Rime record'; then
    ok "the refusal names the capsule that is gone"
else
    bad "the refusal names the capsule that is gone" "$out"
fi
if printf '%s' "$out" | pipe_has "rime env create fedora-build"; then
    ok "the capsule refusal carries the command that makes it again"
else
    bad "the capsule refusal carries the command that makes it again" "$out"
fi
if printf '%s' "$out" | pipe_has "rime agent run --worktree installer-bug"; then
    ok "the worktree refusal carries the command that recreates it"
else
    bad "the worktree refusal carries the command that recreates it" "$out"
fi
# The property the honesty rests on: a refusal prints no plan at all.
if printf '%s' "$out" | pipe_has "resume it with"; then
    bad "a refusal prints no resume steps" "it printed a plan anyway"
else
    ok "a refusal prints no resume steps, so it cannot be half-followed"
fi
# And it must not have recorded the task as resumed.
if [ ! -s "$XDG_STATE_HOME/rime/tasks/installer-bug.json" ] \
   || ! grep -q '"last_opened": [1-9]' "$XDG_STATE_HOME/rime/tasks/installer-bug.json"; then
    ok "a refused resume does not record the task as opened"
else
    bad "a refused resume does not record the task as opened" \
        "$(cat "$XDG_STATE_HOME/rime/tasks/installer-bug.json")"
fi

# ── make the missing parts exist ────────────────────────────────────────────
section "resume when everything is there"
# The capsule engine's own record shape and location. `rime task` reads this
# and never asks podman, which the call log at the end proves.
mkdir -p "$XDG_DATA_HOME/rime/env"
printf '{"name":"fedora-build","image":"registry.fedoraproject.org/fedora:43"}\n' \
    > "$XDG_DATA_HOME/rime/env/fedora-build.json"
# A REAL git worktree, at the path `rime agent run --worktree` would use, so
# the checkpoint assertions below exercise the real toplevel resolution rather
# than a plain directory that happens to sit inside the repository.
git -C "$PROJ" worktree add -q -b agent/installer-bug "$WT" >/dev/null 2>&1
if [ -d "$WT" ]; then
    ok "the fixture worktree exists at the path the runtime would create"
else
    bad "the fixture worktree exists" "$(git -C "$PROJ" worktree list 2>&1)"
fi

out=$(cd "$PROJ" && rime task resume installer-bug); rc=$?
if [ "$rc" = "0" ]; then
    ok "a task whose parts are all present resumes"
else
    bad "a task whose parts are all present resumes" "rc=$rc out=$out"
fi
steps=$(plan_steps "$out")
first=$(printf '%s\n' "$steps" | head -1)
last=$(printf '%s\n' "$steps" | tail -1)
if [ "$first" = "cd $WT" ]; then
    ok "the first step is a cd into the worktree, not the project root"
else
    bad "the first step is a cd into the worktree" "got [$first]"
fi
if [ "$last" = "rime env enter fedora-build" ]; then
    ok "entering the capsule is the LAST step (it starts a shell in the container)"
else
    bad "entering the capsule is the last step" "got [$last] from [$steps]"
fi
if printf '%s' "$out" | pipe_has "not attaching: stdout is not a terminal"; then
    ok "a resume without a terminal attaches to nothing and says why"
else
    bad "a resume without a terminal attaches to nothing" "$out"
fi
if grep -q '"last_opened": [1-9]' "$XDG_STATE_HOME/rime/tasks/installer-bug.json"; then
    ok "a successful resume records the task as opened, in the STATE file"
else
    bad "a successful resume records the task as opened" \
        "$(cat "$XDG_STATE_HOME/rime/tasks/installer-bug.json" 2>&1)"
fi
if grep -q "last_opened" "$TASKS"; then
    bad "the user-owned file is not written by a resume" "last_opened is in tasks.toml"
else
    ok "the user-owned file is not written by a resume"
fi

# ── the windows story, end to end ───────────────────────────────────────────
section "windows: reported from the project layout, and never reopened"
# A real process whose cwd is the worktree, started through a script that
# records every launch. If anything ever reopened the layout, the log would
# gain a second line — which is how "resume reopens nothing" becomes a counted
# fact instead of an assumption.
LAUNCHES=$WORK/launches
: > "$LAUNCHES"
# `sleep` as a background child with a TERM trap, rather than `exec sleep`:
# the layout records the window's own argv, so the process must stay THIS
# script for a restore to be detectable, and killing the script must still take
# the sleep with it rather than leaving one behind.
cat > "$FAKEBIN/rime-task-test-window" <<EOF
#!/usr/bin/env bash
printf 'launched\n' >> "$LAUNCHES"
trap 'kill \$! 2>/dev/null; exit 0' TERM INT
sleep 120 &
wait
EOF
chmod +x "$FAKEBIN/rime-task-test-window"
# Its file descriptors go to /dev/null, and that is not tidiness: a background
# process that inherited this suite's stdout keeps the pipe open after the
# script exits, so `./tests/test-rime-task.sh | grep …` never sees EOF and
# hangs forever. Measured — it hung a verification run for ten minutes.
( cd "$WT" && exec "$FAKEBIN/rime-task-test-window" ) >/dev/null 2>&1 & WIN_PID=$!
trap 'kill "$WIN_PID" 2>/dev/null; rm -rf "$WORK"' EXIT
sleep 0.4

# The layout is saved through the real verb with a faked compositor adapter,
# the pattern test-project-layout.sh established, so the slug and the file
# location are whatever the shipped code computes rather than something this
# suite spells out.
ADAPTER=$WORK/fake-adapter
cat > "$ADAPTER" <<EOF
#!/bin/sh
case "\$1" in
    list)       printf '[{"handle":"0x1","pid":${WIN_PID},"app_id":"Alacritty","title":"editor","workspace":"2","floating":false}]\n' ;;
    compositor) echo fake ;;
    *)          echo "fake adapter: \$1 unsupported" >&2; exit 1 ;;
esac
EOF
chmod +x "$ADAPTER"
out=$(cd "$WT" && RIME_WINDOW_ADAPTER="$ADAPTER" "$RIME_BIN" project layout save 2>&1)
if printf '%s' "$out" | pipe_has "saved 1 window"; then
    ok "a layout is saved for the worktree through the shipped verb"
else
    bad "a layout is saved for the worktree" "$out"
fi

out=$(cd "$PROJ" && rime task show installer-bug)
if printf '%s' "$out" | pipe_has "windows      1 in the saved layout for this root"; then
    ok "the task reports the saved layout for its own root"
else
    bad "the task reports the saved layout for its own root" "$out"
fi
out=$(cd "$PROJ" && rime task resume installer-bug)
if printf '%s' "$out" | pipe_has -x "  rime project layout restore"; then
    ok "resume names the one command that reopens windows"
else
    bad "resume names the one command that reopens windows" "$out"
fi
# The ordering assertion, repeated HERE because this is the first plan with
# more than two steps in it. Mutation testing found that: moving the capsule
# step off the end was invisible to the earlier check, where the plan was only
# `cd` + `rime env enter` and the capsule step was last either way.
steps=$(plan_steps "$out")
n=$(printf '%s\n' "$steps" | grep -c .)
if [ "$n" -ge 3 ]; then
    ok "the plan now has $n steps, so the ordering assertion is not vacuous"
else
    bad "the plan has enough steps to order" "only $n: $steps"
fi
if [ "$(printf '%s\n' "$steps" | tail -1)" = "rime env enter fedora-build" ]; then
    ok "entering the capsule is still last with a layout step in front of it"
else
    bad "entering the capsule is still last" "$steps"
fi
if [ "$(printf '%s\n' "$steps" | grep -n "^rime project layout restore$" | cut -d: -f1)" -lt "$n" ]; then
    ok "the windows come back before the capsule shell opens"
else
    bad "the windows come back before the capsule shell opens" "$steps"
fi
# THE assertion. Reopening stays explicit; a resume that launched things would
# put windows on the developer's desktop.
if [ "$(wc -l < "$LAUNCHES" | tr -d ' ')" = "1" ]; then
    ok "resume reopened nothing: the window was launched exactly once, by this suite"
else
    bad "resume reopened nothing" "$(wc -l < "$LAUNCHES") launches recorded"
fi
if [ "$(calls)" = "0" ]; then
    ok "no compositor tool was invoked either (hyprctl, niri)"
else
    bad "no compositor tool was invoked" "$(cat "$CALLS")"
fi

# ── checkpoints ─────────────────────────────────────────────────────────────
section "the checkpoint binding"
out=$(cd "$PROJ" && rime task checkpoint installer-bug "before changes"); rc=$?
if [ "$rc" = "0" ] && printf '%s' "$out" | pipe_has "before changes"; then
    ok "a checkpoint is captured and labelled"
else
    bad "a checkpoint is captured and labelled" "rc=$rc out=$out"
fi
CPID=$(printf '%s' "$out" | grep -o '[0-9]\{10,\}-[0-9a-f]\{6,\}' | head -1)
if [ -n "$CPID" ]; then
    ok "the checkpoint has an engine id"
else
    bad "the checkpoint has an engine id" "$out"
fi
if grep -q "\"checkpoint\": \"$CPID\"" "$XDG_STATE_HOME/rime/tasks/installer-bug.json"; then
    ok "the id is recorded in the STATE file, not in the file you edit"
else
    bad "the id is recorded in the state file" \
        "$(cat "$XDG_STATE_HOME/rime/tasks/installer-bug.json")"
fi
# Grepping for "checkpoint" here would be satisfied by the file's own header
# comment, which explains that a checkpoint id does NOT belong in it — the
# exact failure this repository has now shipped five times. So the assertion is
# the id itself, which no comment contains, plus the absence of a key
# assignment at the start of a line.
if grep -q "$CPID" "$TASKS" || grep -q "^checkpoint" "$TASKS"; then
    bad "no checkpoint id reaches the user-owned file" "$(grep -n "checkpoint" "$TASKS")"
else
    ok "no checkpoint id reaches the user-owned file"
fi
out=$(cd "$PROJ" && rime task show installer-bug)
if printf '%s' "$out" | pipe_has "checkpoint   $CPID  (present)"; then
    ok "the checkpoint is found from the worktree, where it was taken"
else
    bad "the checkpoint is found from the worktree" "$out"
fi

# Prune it the way `rime agent undo`'s housekeeping would, and the task must
# say so rather than resuming as though the safety net were there.
git -C "$WT" update-ref -d "refs/apex/checkpoints/$CPID" >/dev/null 2>&1  # rime-rename: keep (REF_PREFIX)
out=$(cd "$PROJ" && rime task resume installer-bug); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "the recorded checkpoint is no longer"; then
    ok "a pruned checkpoint refuses the resume and says what happened"
else
    bad "a pruned checkpoint refuses the resume" "rc=$rc out=$out"
fi
if printf '%s' "$out" | pipe_has "rime task checkpoint installer-bug --forget"; then
    ok "the pruned-checkpoint refusal offers the way out"
else
    bad "the pruned-checkpoint refusal offers the way out" "$out"
fi
out=$(cd "$PROJ" && rime task checkpoint installer-bug --forget); rc=$?
if [ "$rc" = "0" ] && printf '%s' "$out" | pipe_has "untouched"; then
    ok "--forget drops the reference and says the checkpoint itself is untouched"
else
    bad "--forget drops the reference" "rc=$rc out=$out"
fi
out=$(cd "$PROJ" && rime task resume installer-bug); rc=$?
if [ "$rc" = "0" ]; then
    ok "the task resumes again once the dangling reference is dropped"
else
    bad "the task resumes again once the reference is dropped" "rc=$rc out=$out"
fi

# ── the keys that exist only to be refused ──────────────────────────────────
section "keys that exist only to be refused"
check_refusal() {
    local key=$1 value=$2 want=$3 label=$4
    # Back up before touching a config, per AGENTS.md — even a test's own.
    cp "$TASKS" "$TASKS.bak"
    printf '\n[task.refused]\nproject = "%s"\n%s = %s\n' "$PROJ" "$key" "$value" >> "$TASKS"
    local out; out=$(rime task list 2>&1)
    local rc=$?
    cp "$TASKS.bak" "$TASKS"
    if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "$want"; then
        ok "$label"
    else
        bad "$label" "rc=$rc out=$out"
    fi
}
check_refusal windows '["editor", "browser", "logs"]' "rime project layout save" \
    "a window list is refused and points at the project layout"
check_refusal permissions '["project files", "network"]' "grants nothing" \
    "a permission list is refused because a task grants nothing"
check_refusal permissions '["network"]' "rime secret grant" \
    "the permission refusal points at the broker that does grant"
check_refusal checkpoint '"1788439662000-a1b2c3d"' "state file" \
    "a checkpoint id in the user-owned file is refused as generated state"
check_refusal sandbox '"unrestricted"' "weakening" \
    "a stored sandbox policy is refused as an unreviewed weakening"

# An unknown key is a typo in this file, because it has exactly one program
# writer. deny_unknown_fields must be load-bearing.
cp "$TASKS" "$TASKS.bak"
printf '\n[task.typo]\nprojekt = "/tmp"\n' >> "$TASKS"
out=$(rime task list 2>&1); rc=$?
cp "$TASKS.bak" "$TASKS"
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "projekt"; then
    ok "an unknown key in the task file is refused and named, not ignored"
else
    bad "an unknown key in the task file is refused and named" "rc=$rc out=$out"
fi

# A future version must refuse rather than guess — and say what to do about it.
#
# This case shipped red for three rounds and the PRODUCT was never wrong. It
# asserted the phrase "understands up to", which is what the message said when
# this file was written (874de9aa, 2026-09-04). P1-045 (5c1a9795, 2026-09-07)
# rewrote it: §25's point is that a task list from a newer Rime is what a
# `bootc rollback` leaves behind, so the refusal has to name the remedy, not
# only the mismatch. That commit updated the Rust unit test beside the message
# (`task.rs::a_future_version_is_refused_rather_than_guessed_at`) and did not
# update this one, so the two halves of the same claim disagreed.
#
# Asserted in the same four parts the Rust test asserts, so they go stale
# together rather than one at a time, and so a message that named the problem
# and dropped the remedy fails here too.
cp "$TASKS" "$TASKS.bak"
python3 - "$TASKS" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1])
p.write_text("version = 99\n" + p.read_text())
PY
out=$(rime task list 2>&1); rc=$?
cp "$TASKS.bak" "$TASKS"
# `reads version [0-9]+` rather than `reads version 1`: the version this build
# understands is the one thing here that is expected to change, and pinning it
# is how the case above went stale in the first place.
missing=
[ "$rc" -ne 0 ] || missing="$missing rc=$rc"
for part in 'version 99' 'reads version [0-9]+' 'rollback' 'Boot the newer deployment'; do
    printf '%s' "$out" | pipe_has -E "$part" || missing="$missing [$part]"
done
if [ -z "$missing" ]; then
    ok "a task file from a newer rime is refused, with both versions and the remedy"
else
    bad "a task file from a newer rime is refused, with both versions and the remedy" \
        "missing:$missing out=$out"
fi

# A hand-edited worktree name that is not its own slug would point the record
# at a directory that is not the worktree's.
cp "$TASKS" "$TASKS.bak"
printf '\n[task.slugcheck]\nproject = "%s"\nworktree = "Issue-217"\n' "$PROJ" >> "$TASKS"
out=$(rime task list 2>&1); rc=$?
cp "$TASKS.bak" "$TASKS"
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "slug"; then
    ok "a worktree name that is not its own slug is refused with the reason"
else
    bad "a worktree name that is not its own slug is refused" "rc=$rc out=$out"
fi

# ── set ─────────────────────────────────────────────────────────────────────
section "rime task set"
out=$(cd "$PROJ" && rime task set installer-bug); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | pipe_has "nothing to change"; then
    ok "a set that names no field is refused rather than silently doing nothing"
else
    bad "a set that names no field is refused" "rc=$rc out=$out"
fi
out=$(cd "$PROJ" && rime task set installer-bug --no-env); rc=$?
if [ "$rc" = "0" ] && ! grep -q "^env = " "$TASKS"; then
    ok "--no-env unbinds the capsule"
else
    bad "--no-env unbinds the capsule" "rc=$rc $(grep '^env' "$TASKS")"
fi
if [ -f "$XDG_DATA_HOME/rime/env/fedora-build.json" ]; then
    ok "unbinding the capsule does not touch the capsule"
else
    bad "unbinding the capsule does not touch the capsule" "the record is gone"
fi
out=$(cd "$PROJ" && rime task set installer-bug --env fedora-build); rc=$?
if [ "$rc" = "0" ] && grep -q "^env = \"fedora-build\"" "$TASKS"; then
    ok "the capsule can be bound again"
else
    bad "the capsule can be bound again" "rc=$rc out=$out"
fi

# ── json ────────────────────────────────────────────────────────────────────
section "machine-readable output"
out=$(cd "$PROJ" && "$RIME_BIN" task list --json 2>/dev/null)
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
t=[x for x in d if x['id']=='installer-bug']
assert t, 'the task is missing'
t=t[0]
assert t['env'] == 'fedora-build', t['env']
assert t['found']['environment'] == 'present', t['found']
assert t['found']['worktree'] == 'present', t['found']
assert t['working_root'].endswith('installer-bug'), t['working_root']
assert t['layout_windows'] == 1, t['layout_windows']
" "$out" 2>/dev/null; then
    ok "list --json carries the bindings and what was found for each"
else
    bad "list --json carries the bindings and what was found" "$out"
fi

out=$(cd "$PROJ" && "$RIME_BIN" task show installer-bug --json 2>/dev/null)
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert d['resume']['resumable'] is True, d['resume']
assert any(s.startswith('cd ') for s in d['resume']['steps']), d['resume']['steps']
assert d['resume']['steps'][-1].startswith('rime env enter'), d['resume']['steps']
" "$out" 2>/dev/null; then
    ok "show --json carries the resume plan in the same shape"
else
    bad "show --json carries the resume plan" "$out"
fi

# A --json resume of a broken task must be non-zero AND carry no steps.
git -C "$PROJ" worktree remove --force "$WT" >/dev/null 2>&1
out=$(cd "$PROJ" && "$RIME_BIN" task resume installer-bug --json 2>/dev/null); rc=$?
if [ "$rc" -ne 0 ]; then
    ok "resume --json of a broken task still exits non-zero"
else
    bad "resume --json of a broken task exits non-zero" "rc=$rc"
fi
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert d['resumable'] is False, d
assert d['steps'] == [], d['steps']
assert any(g['part']=='worktree' for g in d['gone']), d['gone']
" "$out" 2>/dev/null; then
    ok "resume --json names the missing part and carries no steps"
else
    bad "resume --json names the missing part and carries no steps" "$out"
fi

# ── rm ──────────────────────────────────────────────────────────────────────
section "rime task rm"
out=$(rime task rm installer-bug); rc=$?
if [ "$rc" = "0" ] && printf '%s' "$out" | pipe_has "nothing it referenced was touched"; then
    ok "rm removes the task and says what it did not touch"
else
    bad "rm removes the task and says what it did not touch" "rc=$rc out=$out"
fi
if [ ! -f "$XDG_STATE_HOME/rime/tasks/installer-bug.json" ]; then
    ok "rm also drops the state file"
else
    bad "rm also drops the state file" "it is still there"
fi
if [ -f "$XDG_DATA_HOME/rime/env/fedora-build.json" ] && [ -d "$PROJ/.git" ]; then
    ok "rm left the capsule record and the repository alone"
else
    bad "rm left the capsule record and the repository alone" "one of them is gone"
fi
out=$(rime task rm installer-bug); rc=$?
if [ "$rc" -ne 0 ]; then
    ok "removing a task that does not exist is an error, not a no-op"
else
    bad "removing a task that does not exist is an error" "$out"
fi

# ── the files on disk ───────────────────────────────────────────────────────
section "the files on disk"
if grep -q "Hand-editable" "$TASKS"; then
    ok "the written task file explains itself to whoever opens it"
else
    bad "the written task file explains itself" "$(head -3 "$TASKS")"
fi
# No temp file may survive a write. A glob rather than `ls | grep`: it answers
# the question directly and does not care what the filenames contain.
shopt -s nullglob
leftover=("$XDG_CONFIG_HOME"/rime/*.tmp.* "$XDG_STATE_HOME"/rime/tasks/*.tmp.*)
shopt -u nullglob
if [ "${#leftover[@]}" -eq 0 ]; then
    ok "no temp file is left behind by an atomic write"
else
    bad "no temp file is left behind" "${leftover[*]}"
fi
mode=$(stat -c %a "$XDG_STATE_HOME/rime/tasks" 2>/dev/null)
if [ "$mode" = "700" ]; then
    ok "the task state directory is private (700)"
else
    bad "the task state directory is private" "mode $mode"
fi

# ── nothing privileged, nothing containerised ───────────────────────────────
section "no prompt is possible"
# The whole run, from the negative control onwards. `rime task` checks a
# capsule by reading the engine's record; if it ever shelled out to podman, or
# to sudo/pkexec/secret-tool, this is where it would show — and a keyring or
# polkit prompt is exactly what the developer has twice asked never to see.
if [ "$(calls)" = "0" ]; then
    ok "sudo, pkexec, secret-tool, podman and distrobox were never invoked"
else
    bad "no privileged or container tool was invoked" "$(cat "$CALLS")"
fi

# ── the developer's own tasks ───────────────────────────────────────────────
section "the machine running the tests"
if [ -f "$REAL_TASKS" ]; then cp "$REAL_TASKS" "$WORK/real-tasks-after"; else : > "$WORK/real-tasks-after"; fi
if [ -d "$REAL_STATE" ]; then ls -1 "$REAL_STATE" > "$WORK/real-state-after" 2>/dev/null; else : > "$WORK/real-state-after"; fi
# Compared as strings rather than with `diff`: diffutils is not in every
# environment this suite is expected to run in — the project's own Rust
# container has no `diff` — and the last assertion in the file is the wrong one
# to lose to a missing tool.
if [ "$(cat "$REAL_TASKS_BEFORE")" = "$(cat "$WORK/real-tasks-after")" ]; then
    ok "the developer's own tasks.toml was not touched"
else
    bad "the developer's own tasks.toml was not touched" "IT CHANGED — see $REAL_TASKS"
fi
if [ "$(cat "$REAL_STATE_BEFORE")" = "$(cat "$WORK/real-state-after")" ]; then
    ok "the developer's own task state directory was not touched"
else
    bad "the developer's own task state was not touched" "IT CHANGED — see $REAL_STATE"
fi

printf '\nrime task: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
