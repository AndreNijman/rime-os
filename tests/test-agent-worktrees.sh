#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for `rime agent worktrees` (P1-036): tests,
#  conflicts, diff and local readiness, per worktree.
#
#  The unit tests cover the parsers and the readiness rules. What they cannot
#  cover is the four claims the design rests on, every one of which is about a
#  real repository, a real conflict and a real socket:
#
#      1. THE NAMED GUARANTEE. Asking for the status of a genuinely conflicted
#         worktree leaves that worktree untouched: no MERGE_HEAD, the staged
#         content unchanged, and `git status --porcelain` byte-identical. The
#         obvious implementation — `git merge --no-commit` in the worktree —
#         fails all three, and it would fail them in a checkout an agent is
#         typing into.
#      2. The fixture really is conflicted. A suite that asserted "nothing
#         changed" against a clean repository would pass every assertion
#         while testing nothing, which is the failure mode this whole file
#         exists to avoid.
#      3. A test run is joined to the tree the SESSION lives in, not to the
#         directory the hook process happened to run from. Otherwise a
#         confined session could report a suite result against somebody
#         else's worktree.
#      4. A path-shaped project slug is refused, and the repository it points
#         at is not touched — proven by counting that repository's objects
#         either side of the call, because `merge-tree --write-tree` writes
#         objects and that is exactly what a refused call must not do.
#
#  NOTHING HERE TOUCHES A RUNNING DAEMON. Its own XDG_RUNTIME_DIR, its own
#  XDG_STATE_HOME, its own XDG_CONFIG_HOME, its own RIME_AGENT_SCRATCH_ROOT,
#  and the daemon is killed by the pid this script started — never by name.
#  The project store is under this suite's XDG_STATE_HOME, so the projects the
#  daemon can see are this suite's fixtures and not the user's real checkouts.
#
#      ./tests/test-agent-worktrees.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` for the reason test-agent-inject.sh documents: this suite counts
# failures rather than aborting, and several assertions run commands that exit
# non-zero on purpose.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Before the temp tree, the daemon or the build: this suite needs an
# environment the daemon will observe as LOCAL, and that has to be arranged
# from outside the suite. `rime-agentd` places a peer from its cgroup, and a
# process started by systemd — a CI job, a timer-dispatched agent — is in
# neither a login session nor a user service, so §7 refuses it before the
# behaviour under test is reached. Here it stops the suite starting the
# session the status call is made from.
# The helper runs this file again with the guard set, either inside a logind
# session it created or — saying why, out loud — in place; either way the
# suite runs exactly once, so this is `exec` and not a call. Same block, and
# the same reason, as tests/test-privilege-requests.sh.
if [ -z "${RIME_LOGIN_SESSION_WRAPPED:-}" ] && [ -x "${ROOT}/tests/in-login-session.sh" ]; then
    exec "${ROOT}/tests/in-login-session.sh" "${BASH_SOURCE[0]}" "$@"
fi
WORK="$(mktemp -d)"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
give_up() { printf '\nworktrees: %d passed, %d failed\n' "$pass" "$fail"; exit 1; }

DAEMON_PID=""
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    [ -n "$DAEMON_PID" ] && { for _ in 1 2 3 4 5; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.2; done; }
    [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

# ── prerequisites ────────────────────────────────────────────────────────────
# A missing prerequisite is a FAILURE, never a skip: a suite that prints
# "0 passed, 0 failed" and exits 0 is a green tick over nothing asserted.
for tool in cargo git python3 awk cmp; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binaries"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" \
        --bin rime-agentd --bin rime >/dev/null 2>&1; then
    bad "rime-agentd and rime build"
    give_up
fi
ok "rime-agentd and rime build"

BIN="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug"
AGENTD="${BIN}/rime-agentd"
Rime="${BIN}/rime"

# ── an isolated runtime ──────────────────────────────────────────────────────
export XDG_RUNTIME_DIR="${WORK}/run"
export XDG_STATE_HOME="${WORK}/state"
export XDG_CONFIG_HOME="${WORK}/config"
export RIME_AGENT_SCRATCH_ROOT="${WORK}/scratch"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"
chmod 0700 "$XDG_RUNTIME_DIR"

# What the machine's REAL daemon already has under the shared scratch root,
# recorded before this suite's daemon starts. Compared, not merely tested for
# absence: `/tmp/rime-agent/1` belongs to the real daemon on a machine where a
# session is running, and asserting it does not exist would fail for a reason
# that has nothing to do with this suite.
PRE_SCRATCH="$(ls /tmp/rime-agent 2>/dev/null | sort | tr '\n' ' ')"

# rime-agentd writes here and `rime agent worktrees` reads here. Asserted
# rather than assumed, because if the daemon resolved projects from the user's
# real store instead, this suite would run `merge-tree --write-tree` in
# Andre's own repositories on every run.
PROJECTS="${XDG_STATE_HOME}/rime/agent/projects"

git_q() { git -c advice.detachedHead=false -c init.defaultBranch=main "$@"; }

# ── the fixture project ──────────────────────────────────────────────────────
#
# One repository, a bare remote to push to, and two agent worktrees under
# `.rime/worktrees/` — which is where `project::worktrees` looks to decide a
# worktree is an agent's rather than the user's own.
#
#   main          f.txt = "base\nmain change\n"
#   agent/clash   f.txt = "base\nworktree change\n"   → CONFLICTS with main
#   agent/tidy    new.txt, f.txt untouched            → merges CLEAN, pushed
#
PROJ="${WORK}/proj"
REMOTE="${WORK}/remote.git"
mkdir -p "$PROJ"
git_q init -q "$PROJ" >/dev/null 2>&1
git_q -C "$PROJ" config user.email t@example.invalid
git_q -C "$PROJ" config user.name t
git_q init -q --bare "$REMOTE" >/dev/null 2>&1
git_q -C "$PROJ" remote add origin "$REMOTE"

printf 'base\n' > "${PROJ}/f.txt"
printf 'untouched\n' > "${PROJ}/keep.txt"
git_q -C "$PROJ" add f.txt keep.txt
git_q -C "$PROJ" commit -qm "base"

mkdir -p "${PROJ}/.rime/worktrees"
WT_CLASH="${PROJ}/.rime/worktrees/clash"
WT_TIDY="${PROJ}/.rime/worktrees/tidy"
git_q -C "$PROJ" worktree add -q -b agent/clash "$WT_CLASH" >/dev/null 2>&1
git_q -C "$PROJ" worktree add -q -b agent/tidy "$WT_TIDY" >/dev/null 2>&1

# The conflicting pair: both sides rewrite the same line of f.txt.
printf 'base\nworktree change\n' > "${WT_CLASH}/f.txt"
git_q -C "$WT_CLASH" commit -qam "the worktree's take on f.txt"

# TWO commits here, against main's one. The counts must be ASYMMETRIC: with
# `ahead` and `behind` both 1, swapping the two columns of
# `git rev-list --left-right --count` is invisible, and a mutation that
# reversed them survived this suite until the fixture was changed. 2 ahead and
# 1 behind cannot be swapped without the assertion noticing.
printf 'new file\n' > "${WT_TIDY}/new.txt"
git_q -C "$WT_TIDY" add new.txt
git_q -C "$WT_TIDY" commit -qm "add new.txt"
printf 'second new file\n' > "${WT_TIDY}/new2.txt"
git_q -C "$WT_TIDY" add new2.txt
git_q -C "$WT_TIDY" commit -qm "add new2.txt"
git_q -C "$WT_TIDY" push -q -u origin agent/tidy >/dev/null 2>&1

printf 'base\nmain change\n' > "${PROJ}/f.txt"
git_q -C "$PROJ" commit -qam "main's take on f.txt"

# A third agent worktree on an ORPHAN branch, so its history is unrelated to
# main's. `merge-tree` exits 128 on that ("refusing to merge unrelated
# histories") and the design says an exit git declines to answer degrades to
# `unknown` — carrying the reason — rather than collapsing to `clean` or
# failing the whole listing. Neither half of that had any coverage against
# real git until this fixture existed: a mutation that let `unknown` count as
# ready survived the suite.
WT_ALIEN="${PROJ}/.rime/worktrees/alien"
git_q -C "$PROJ" worktree add -q --detach "$WT_ALIEN" >/dev/null 2>&1
git_q -C "$WT_ALIEN" checkout -q --orphan agent/alien >/dev/null 2>&1
git_q -C "$WT_ALIEN" rm -q -rf . >/dev/null 2>&1
printf 'alien\n' > "${WT_ALIEN}/a.txt"
git_q -C "$WT_ALIEN" add a.txt
git_q -C "$WT_ALIEN" commit -qm "an unrelated history"

# Proof the fixture is what the rest of this file assumes. Without this, every
# "nothing changed" assertion below could be passing over a clean merge.
section "the fixture is genuinely conflicted"
probe="$(git_q -C "$PROJ" merge-tree --write-tree --name-only main agent/clash 2>&1)"
probe_rc=$?
if [ "$probe_rc" -eq 1 ] && printf '%s' "$probe" | pipe_has -x 'f.txt'; then
    ok "git itself says main and agent/clash conflict in f.txt"
else
    bad "git itself says main and agent/clash conflict in f.txt"
    echo "      merge-tree exit ${probe_rc}, output:" >&2
    printf '%s\n' "$probe" | sed 's/^/      | /' >&2
    give_up
fi
if git_q -C "$PROJ" merge-tree --write-tree --name-only main agent/tidy >/dev/null 2>&1; then
    ok "git itself says main and agent/tidy merge cleanly"
else
    bad "git itself says main and agent/tidy merge cleanly"
    give_up
fi
git_q -C "$PROJ" merge-tree --write-tree --name-only main agent/alien >/dev/null 2>&1
alien_rc=$?
if [ "$alien_rc" -ne 0 ] && [ "$alien_rc" -ne 1 ]; then
    ok "git itself declines to merge main and agent/alien (exit ${alien_rc}, not 0 or 1)"
else
    bad "git itself declines to merge main and agent/alien"
    echo "      exit ${alien_rc}; the orphan branch is not unrelated after all" >&2
    give_up
fi

# ── the daemon ───────────────────────────────────────────────────────────────
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
    give_up
fi

# ── the project store the daemon reads ───────────────────────────────────────
#
# There is no `rime project add`. A project is remembered when a session starts
# in it (rime-agentd's session.rs), which is also the only way it can happen on
# a real machine — so that is how this suite registers one, rather than
# planting a record and testing a path nothing takes.
PROJ_SESSION="$("$Rime" agent run --agent generic --sandbox unrestricted \
    --cwd "$PROJ" -d -- /bin/sh -c 'sleep 600' 2>"${WORK}/proj-run.err" \
    | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
if [ -n "$PROJ_SESSION" ]; then
    ok "a session started in the project (id ${PROJ_SESSION})"
else
    bad "a session started in the project"
    sed 's/^/      /' "${WORK}/proj-run.err" >&2
    sed 's/^/      /' "${WORK}/agentd.log" >&2
    give_up
fi
SLUG="$(ls "$PROJECTS" 2>/dev/null | sed -n 's/\.json$//p' | head -1)"
if [ -n "$SLUG" ]; then
    ok "the fixture project is remembered in this suite's own store (${SLUG})"
else
    bad "the fixture project is remembered in this suite's own store"
    echo "      nothing under ${PROJECTS}" >&2
    sed 's/^/      /' "${WORK}/agentd.log" >&2
    give_up
fi
# The isolation assertion, stated as its own line: if this store were the real
# one, the count would be Andre's project count and this suite would probe his
# repositories.
if [ "$(ls "$PROJECTS" | wc -l)" -eq 1 ]; then
    ok "the daemon can see exactly one project, this suite's fixture"
else
    bad "the daemon can see exactly one project, this suite's fixture"
    ls "$PROJECTS" | sed 's/^/      /' >&2
fi

# ── a helper that reads one worktree out of the JSON ─────────────────────────
jq_get() {  # jq_get <file> <worktree name> <python expression over `w`>
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
rows = json.load(open(sys.argv[1]))
for w in rows:
    if w["name"] == sys.argv[2]:
        v = eval(sys.argv[3], {}, {"w": w})
        print("" if v is None else v)
        sys.exit(0)
sys.exit(3)
PY
}

# ── the baseline: exactly what must not change ───────────────────────────────
#
# One settling `git status` first. In a repository this young the index entries
# are racy — their mtime matches the index's own — so the FIRST status refreshes
# the stat cache and rewrites the index file. Taking the baseline before that
# would make the index measurement change for a reason that has nothing to do
# with the code under test.
#
# And the measurement is `git ls-files --stage`, the staged CONTENT, not the
# bytes of `.git/index`: a stat-cache refresh rewrites the file without
# changing a single entry, and hashing the file would report that as a
# violation. The guarantee is "changes no entry", and this measures entries.
snapshot() {   # snapshot <dir> <tag>
    git_q -C "$1" status --porcelain >/dev/null 2>&1
    git_q -C "$1" ls-files --stage > "${WORK}/${2}.stage" 2>&1
    git_q -C "$1" status --porcelain > "${WORK}/${2}.porcelain" 2>&1
    git_q -C "$1" rev-parse HEAD > "${WORK}/${2}.head" 2>&1
    git_q -C "$1" stash list > "${WORK}/${2}.stash" 2>&1
    git_q -C "$1" for-each-ref --format='%(refname) %(objectname)' \
        > "${WORK}/${2}.refs" 2>&1
}

# MERGE_HEAD is NOT at `<worktree>/.git/MERGE_HEAD`. In a linked worktree
# `.git` is a FILE pointing at `.git/worktrees/<name>/`, so testing that path
# is always false and would pass whatever the code did. `rev-parse --git-path`
# resolves it the way git itself does.
merge_head_path() { git_q -C "$1" rev-parse --git-path MERGE_HEAD 2>/dev/null; }
merge_in_progress() {   # any of the mid-merge marker files, wherever they live
    local d
    for f in MERGE_HEAD MERGE_MSG MERGE_MODE CHERRY_PICK_HEAD REVERT_HEAD; do
        d="$(git_q -C "$1" rev-parse --git-path "$f" 2>/dev/null)"
        [ -n "$d" ] && [ -e "$d" ] && { echo "$f at $d"; return 0; }
    done
    return 1
}

snapshot "$WT_CLASH" clash_before
snapshot "$PROJ" main_before

MH="$(merge_head_path "$WT_CLASH")"
if [ -n "$MH" ] && [ ! -e "$MH" ]; then
    ok "before the call there is no MERGE_HEAD in the worktree (${MH})"
else
    bad "before the call there is no MERGE_HEAD in the worktree"
    echo "      --git-path gave '${MH}'" >&2
fi

# ── the call ─────────────────────────────────────────────────────────────────
section "the status call"
"$Rime" agent worktrees --json > "${WORK}/status.json" 2> "${WORK}/status.err"
rc=$?
if [ "$rc" -eq 0 ] && [ -s "${WORK}/status.json" ]; then
    ok "rime agent worktrees --json answered"
else
    bad "rime agent worktrees --json answered"
    echo "      exit ${rc}" >&2
    sed 's/^/      /' "${WORK}/status.err" >&2
    sed 's/^/      /' "${WORK}/agentd.log" >&2
    give_up
fi

names="$(python3 -c 'import json,sys; print(" ".join(sorted(w["name"] for w in json.load(open(sys.argv[1])))))' "${WORK}/status.json")"
if [ "$names" = "alien clash proj tidy" ]; then
    ok "all four worktrees are listed (${names})"
else
    bad "all four worktrees are listed"
    echo "      got '${names}', wanted 'alien clash proj tidy'" >&2
fi

# ── THE NAMED GUARANTEE ──────────────────────────────────────────────────────
section "the worktree it asked about was not touched"

if marker="$(merge_in_progress "$WT_CLASH")"; then
    bad "no merge was left in progress in the worktree (found ${marker})"
else
    ok "no merge was left in progress in the worktree"
fi
if marker="$(merge_in_progress "$PROJ")"; then
    bad "no merge was left in progress in the main tree (found ${marker})"
else
    ok "no merge was left in progress in the main tree"
fi

snapshot "$WT_CLASH" clash_after
snapshot "$PROJ" main_after

same() {   # same <tag a> <tag b> <suffix> <what>
    if cmp -s "${WORK}/${1}.${3}" "${WORK}/${2}.${3}"; then
        ok "$4"
    else
        bad "$4"
        diff -u "${WORK}/${1}.${3}" "${WORK}/${2}.${3}" | sed 's/^/      /' >&2
    fi
}

same clash_before clash_after stage     "the worktree's index is unchanged, entry for entry"
same clash_before clash_after porcelain "git status --porcelain in the worktree is byte-identical"
same clash_before clash_after head      "the worktree is on the same commit"
same clash_before clash_after stash     "the worktree's stash list is unchanged"
same clash_before clash_after refs      "no ref moved or appeared"
same main_before  main_after  stage     "the main tree's index is unchanged too"
same main_before  main_after  porcelain "git status --porcelain in the main tree is byte-identical"

# The honest half of the guarantee, asserted rather than only documented:
# `merge-tree --write-tree` DOES write unreferenced objects into the shared
# object store. A claim of "reads only" would be false, so the suite pins the
# true statement instead — objects may appear, nothing a working tree can see
# may change.
if [ -s "${WORK}/clash_before.stage" ]; then
    ok "the worktree had staged entries to compare in the first place"
else
    bad "the worktree had staged entries to compare in the first place"
fi

# ── what it actually said ────────────────────────────────────────────────────
section "the four answers"

state="$(jq_get "${WORK}/status.json" clash 'w["conflicts"]["state"]')"
paths="$(jq_get "${WORK}/status.json" clash '",".join(w["conflicts"].get("paths",[]))')"
if [ "$state" = "conflicted" ] && [ "$paths" = "f.txt" ]; then
    ok "the conflicted worktree is reported conflicted, in f.txt"
else
    bad "the conflicted worktree is reported conflicted, in f.txt"
    echo "      state='${state}' paths='${paths}'" >&2
fi

state="$(jq_get "${WORK}/status.json" tidy 'w["conflicts"]["state"]')"
if [ "$state" = "clean" ]; then
    ok "the clean worktree is reported clean"
else
    bad "the clean worktree is reported clean"
    echo "      state='${state}'" >&2
fi

state="$(jq_get "${WORK}/status.json" proj 'w["conflicts"]["state"]')"
if [ "$state" = "not_applicable" ]; then
    ok "the main tree has nothing to merge into itself"
else
    bad "the main tree has nothing to merge into itself"
    echo "      state='${state}'" >&2
fi

# An answer git declined to give. The one wrong answer here is `clean`, which
# reads as "ready" and costs somebody a broken merge.
state="$(jq_get "${WORK}/status.json" alien 'w["conflicts"]["state"]')"
reason="$(jq_get "${WORK}/status.json" alien 'w["conflicts"].get("reason","")')"
if [ "$state" = "unknown" ] && printf '%s' "$reason" | pipe_has -i "unrelated histories"; then
    ok "an unmergeable history is 'unknown' and carries git's reason (${reason})"
else
    bad "an unmergeable history is 'unknown' and carries git's reason"
    echo "      state='${state}' reason='${reason}'" >&2
fi
ready="$(jq_get "${WORK}/status.json" alien 'w["ready"]["ready_to_propose"]')"
blockers="$(jq_get "${WORK}/status.json" alien '" | ".join(w["ready"]["blockers"])')"
if [ "$ready" = "False" ] && printf '%s' "$blockers" | pipe_has "conflict state unknown"; then
    ok "an unknown conflict state is never treated as ready"
else
    bad "an unknown conflict state is never treated as ready"
    echo "      ready='${ready}' blockers='${blockers}'" >&2
fi
# ...and the odd worktree did not take the rest of the listing down with it,
# which is the reason the probe degrades instead of failing.
if [ "$names" = "alien clash proj tidy" ]; then
    ok "one worktree git cannot answer for does not fail the whole listing"
else
    bad "one worktree git cannot answer for does not fail the whole listing"
fi

# The diff, checked against git's own numbers rather than against a constant
# somebody typed: a hard-coded 1/1/1 would keep passing if the range were
# wrong in the same direction as the fixture.
want="$(git_q -C "$PROJ" diff --numstat main...agent/tidy \
    | awk '{f++; a+=$1; d+=$2} END {printf "%d %d %d", f, a, d}')"
got="$(jq_get "${WORK}/status.json" tidy '"%d %d %d" % (w["diff"]["files"], w["diff"]["insertions"], w["diff"]["deletions"])')"
if [ "$got" = "$want" ]; then
    ok "the diff matches git diff --numstat main...agent/tidy (${got})"
else
    bad "the diff matches git diff --numstat main...agent/tidy"
    echo "      got '${got}', git says '${want}'" >&2
fi

ahead="$(jq_get "${WORK}/status.json" tidy 'w["ahead"]')"
behind="$(jq_get "${WORK}/status.json" tidy 'w["behind"]')"
if [ "$ahead" = "2" ] && [ "$behind" = "1" ]; then
    ok "ahead/behind are the right way round (2 ahead of main, 1 behind it)"
else
    bad "ahead/behind are the right way round"
    echo "      ahead='${ahead}' behind='${behind}', wanted 2 and 1" >&2
    echo "      git says: $(git_q -C "$PROJ" rev-list --left-right --count main...agent/tidy)" >&2
fi

up="$(jq_get "${WORK}/status.json" tidy 'w["upstream"]')"
if [ "$up" = "origin/agent/tidy" ]; then
    ok "the pushed worktree's upstream is found (${up})"
else
    bad "the pushed worktree's upstream is found"
    echo "      upstream='${up}'" >&2
fi

ready="$(jq_get "${WORK}/status.json" tidy 'w["ready"]["ready_to_propose"]')"
if [ "$ready" = "True" ]; then
    ok "a clean, committed, pushed worktree IS ready to propose"
else
    bad "a clean, committed, pushed worktree IS ready to propose"
    jq_get "${WORK}/status.json" tidy 'w["ready"]["blockers"]' | sed 's/^/      /' >&2
fi

ready="$(jq_get "${WORK}/status.json" clash 'w["ready"]["ready_to_propose"]')"
blockers="$(jq_get "${WORK}/status.json" clash '" | ".join(w["ready"]["blockers"])')"
if [ "$ready" = "False" ] && printf '%s' "$blockers" | pipe_has "conflict in 1 file"; then
    ok "the conflicted worktree is not ready, and says why (${blockers})"
else
    bad "the conflicted worktree is not ready, and says why"
    echo "      ready='${ready}' blockers='${blockers}'" >&2
fi

tests="$(jq_get "${WORK}/status.json" clash 'w["tests"]["state"]')"
if [ "$tests" = "unobserved" ]; then
    ok "a worktree nobody ran a suite in is 'unobserved', not a pass"
else
    bad "a worktree nobody ran a suite in is 'unobserved', not a pass"
    echo "      tests='${tests}'" >&2
fi

# ── the test observation, end to end ─────────────────────────────────────────
#
# The join the daemon adds and a client running git itself could not make:
# a hook event from a session, matched to the worktree that session lives in.
section "a test run is observed and joined to the right worktree"

SESSION_ID="$("$Rime" agent run --agent generic --sandbox unrestricted \
    --cwd "$WT_CLASH" -d -- /bin/sh -c 'sleep 600' 2>"${WORK}/run.err" \
    | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
if [ -n "$SESSION_ID" ]; then
    ok "a session is running in the conflicted worktree (id ${SESSION_ID})"
else
    bad "a session is running in the conflicted worktree"
    sed 's/^/      /' "${WORK}/run.err" >&2
    give_up
fi

hook_event() {   # hook_event <event> <command> [cwd]
    printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$2" \
        | (cd "${3:-$WORK}" && RIME_AGENT_SESSION="$SESSION_ID" \
            "$Rime" agent hook "$1" >/dev/null 2>&1)
}

# Deliberately run from $WORK — NOT from the worktree. The daemon must use the
# session's own recorded cwd, so that a session cannot report a suite result
# against a tree it does not live in. If it trusted the hook process's
# directory instead, this observation would land nowhere (or worse, somewhere
# else) and the assertion below would fail.
hook_event pre_tool_use "cargo test --locked"
sleep 0.3
"$Rime" agent worktrees --json > "${WORK}/running.json" 2>/dev/null
tests="$(jq_get "${WORK}/running.json" clash 'w["tests"]["state"]')"
cmd="$(jq_get "${WORK}/running.json" clash 'w["tests"].get("command","")')"
if [ "$tests" = "running" ] && [ "$cmd" = "cargo test" ]; then
    ok "a suite starting in a session is reported running in that session's worktree"
else
    bad "a suite starting in a session is reported running in that session's worktree"
    echo "      state='${tests}' command='${cmd}'" >&2
fi

# ...and it landed on the SESSION's worktree, not on the hook process's cwd.
other="$(jq_get "${WORK}/running.json" tidy 'w["tests"]["state"]')"
main_t="$(jq_get "${WORK}/running.json" proj 'w["tests"]["state"]')"
if [ "$other" = "unobserved" ] && [ "$main_t" = "unobserved" ]; then
    ok "no other worktree was credited with that test run"
else
    bad "no other worktree was credited with that test run"
    echo "      tidy='${other}' proj='${main_t}'" >&2
fi

hook_event post_tool_use_failure "cargo test --locked"
sleep 0.3
"$Rime" agent worktrees --json > "${WORK}/failed.json" 2>/dev/null
tests="$(jq_get "${WORK}/failed.json" clash 'w["tests"]["state"]')"
if [ "$tests" = "failed" ]; then
    ok "a failure event turns the observation into 'failed'"
else
    bad "a failure event turns the observation into 'failed'"
    echo "      state='${tests}'" >&2
fi
blockers="$(jq_get "${WORK}/failed.json" clash '" | ".join(w["ready"]["blockers"])')"
if printf '%s' "$blockers" | pipe_has "the last test run Rime observed failed"; then
    ok "an observed failure blocks readiness, in those words"
else
    bad "an observed failure blocks readiness, in those words"
    echo "      blockers='${blockers}'" >&2
fi

# A command that merely mentions tests must not touch the state. This is the
# assertion behind the runner table: an agent runs `cargo build --tests` and
# `git commit -m 'fix the test'` constantly.
hook_event post_tool_use "cargo build --tests"
sleep 0.3
"$Rime" agent worktrees --json > "${WORK}/nottest.json" 2>/dev/null
tests="$(jq_get "${WORK}/nottest.json" clash 'w["tests"]["state"]')"
if [ "$tests" = "failed" ]; then
    ok "a command that only mentions tests does not overwrite the observation"
else
    bad "a command that only mentions tests does not overwrite the observation"
    echo "      state='${tests}', wanted the earlier 'failed' to stand" >&2
fi

sessions="$(jq_get "${WORK}/failed.json" clash '",".join(str(i) for i in w["sessions"])')"
if [ "$sessions" = "$SESSION_ID" ]; then
    ok "the worktree names the session working in it (${sessions})"
else
    bad "the worktree names the session working in it"
    echo "      sessions='${sessions}', wanted '${SESSION_ID}'" >&2
fi

# An agent worktree lives UNDER the main tree, so a plain containment test
# credits every agent session to the main tree as well — and the main tree
# then lists every agent in the repository. Each session belongs to exactly
# one row, the deepest worktree that contains it.
main_sessions="$(jq_get "${WORK}/failed.json" proj '",".join(str(i) for i in w["sessions"])')"
if [ "$main_sessions" = "$PROJ_SESSION" ]; then
    ok "the main tree does not claim the session living in its agent worktree"
else
    bad "the main tree does not claim the session living in its agent worktree"
    echo "      main tree sessions='${main_sessions}', wanted only '${PROJ_SESSION}'" >&2
fi

# `rime agent run --cwd <a worktree>` makes rime-agentd remember that WORKTREE
# as a project of its own, because `project::detect` resolves a project from
# `git rev-parse --show-toplevel` and inside a linked worktree that is the
# worktree. Every worktree of the repository is visible from in there, so
# without the skip in the daemon's handler this listing gains a second copy of
# every row — measured: six rows for a three-worktree repository, three of
# them named after one worktree.
stored="$(ls "$PROJECTS" | wc -l)"
rows="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "${WORK}/failed.json")"
if [ "$stored" -eq 2 ] && [ "$rows" -eq 4 ]; then
    ok "a linked worktree remembered as a project adds no duplicate rows (${stored} records, ${rows} rows)"
else
    bad "a linked worktree remembered as a project adds no duplicate rows"
    echo "      ${stored} project records, ${rows} rows; wanted 2 and 4" >&2
    ls "$PROJECTS" | sed 's/^/      /' >&2
fi
names="$(python3 -c 'import json,sys; print(" ".join(sorted(w["name"] for w in json.load(open(sys.argv[1])))))' "${WORK}/failed.json")"
if [ "$names" = "alien clash proj tidy" ]; then
    ok "and each worktree still appears exactly once (${names})"
else
    bad "and each worktree still appears exactly once"
    echo "      got '${names}'" >&2
fi
# ...and asking for the linked worktree's own slug says why, rather than
# answering with a second copy of the repository.
wt_slug="$(ls "$PROJECTS" | sed -n 's/\.json$//p' | grep -v "^${SLUG}$" | head -1)"
out="$("$Rime" agent worktrees --project "$wt_slug" --json 2>&1)"
if [ $? -ne 0 ] && printf '%s' "$out" | pipe_has "linked git worktree"; then
    ok "asking for a linked worktree by slug is refused with the reason"
else
    bad "asking for a linked worktree by slug is refused with the reason"
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi

# ── a session reached through a symlink is still in its worktree ─────────────
#
# A session records its cwd as the caller gave it (`session.rs` keeps it
# unresolved on purpose: it becomes a --chdir, and bwrap must not mount on a
# symlinked path), while `git worktree list` reports the real path. On an
# atomic OS every home is reached through a symlink — /home -> var/home — so
# without resolving one side the join matches nothing and the row silently
# claims no session works there.
section "a session whose cwd goes through a symlink"

ln -s "$PROJ" "${WORK}/link"
LINK_SESSION="$("$Rime" agent run --agent generic --sandbox unrestricted \
    --cwd "${WORK}/link/.rime/worktrees/tidy" -d -- /bin/sh -c 'sleep 600' \
    2>"${WORK}/link-run.err" | sed -n 's/^session \([0-9]\+\) .*/\1/p' | head -1)"
if [ -n "$LINK_SESSION" ]; then
    ok "a session started through the symlink (id ${LINK_SESSION})"
else
    bad "a session started through the symlink"
    sed 's/^/      /' "${WORK}/link-run.err" >&2
fi
"$Rime" agent worktrees --project "$SLUG" --json > "${WORK}/linked.json" 2>/dev/null
linked="$(jq_get "${WORK}/linked.json" tidy '",".join(str(i) for i in w["sessions"])')"
if [ "$linked" = "$LINK_SESSION" ]; then
    ok "the worktree claims the session even though its cwd went through a symlink"
else
    bad "the worktree claims the session even though its cwd went through a symlink"
    echo "      tidy sessions='${linked}', wanted '${LINK_SESSION}'" >&2
    echo "      the session recorded $("$Rime" agent status "$LINK_SESSION" 2>/dev/null | grep -i cwd)" >&2
fi
rows="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "${WORK}/linked.json")"
if [ "$rows" -eq 4 ]; then
    ok "and it added no row of its own (${rows})"
else
    bad "and it added no row of its own"
    echo "      ${rows} rows, wanted 4" >&2
fi

# ── the slug is not a path ───────────────────────────────────────────────────
#
# `Request::Worktrees` takes a slug precisely so that no caller names a
# directory for the daemon to run git in. A decoy project record is planted
# OUTSIDE the store, where only a traversal could reach it, and its repository
# is watched: `merge-tree --write-tree` writes objects, so an object count that
# moved is proof the daemon ran there.
section "a path-shaped slug reaches nothing"

# The decoy is built the same shape as the fixture project — a main tree AND
# an agent worktree whose branch CONFLICTS with it — for a specific reason: a
# repository with no agent worktree has nothing for `merge-tree` to probe, so
# reaching it writes no objects and an object count would prove nothing.
# Measured: with a bare decoy, a mutation that resolved the slug by joining it
# onto the store path reached this repository and the object count did not
# move. Now it would.
DECOY="${WORK}/decoy"
mkdir -p "$DECOY"
git_q init -q "$DECOY" >/dev/null 2>&1
git_q -C "$DECOY" config user.email t@example.invalid
git_q -C "$DECOY" config user.name t
printf 'decoy\n' > "${DECOY}/d.txt"
git_q -C "$DECOY" add d.txt
git_q -C "$DECOY" commit -qm "decoy"
mkdir -p "${DECOY}/.rime/worktrees"
git_q -C "$DECOY" worktree add -q -b agent/x "${DECOY}/.rime/worktrees/x" >/dev/null 2>&1
printf 'decoy\nfrom the worktree\n' > "${DECOY}/.rime/worktrees/x/d.txt"
git_q -C "${DECOY}/.rime/worktrees/x" commit -qam "the decoy worktree's take"
printf 'decoy\nfrom main\n' > "${DECOY}/d.txt"
git_q -C "$DECOY" commit -qam "the decoy main tree's take"
# Proof the decoy is a repository where a reached probe WOULD write: git
# itself says the two sides conflict, so `merge-tree --write-tree` has a
# merged tree and blobs to write.
if ! git_q -C "$DECOY" merge-tree --write-tree --name-only main agent/x >/dev/null 2>&1; then
    ok "the decoy is a repository a reached probe would write objects into"
else
    bad "the decoy is a repository a reached probe would write objects into"
    echo "      its two branches do not conflict, so the object count below proves nothing" >&2
fi
# ...and that probe just wrote some, which is why the count is taken AFTER it.
git_q -C "$DECOY" gc --quiet --prune=now >/dev/null 2>&1
mkdir -p "${PROJECTS}/../planted"
python3 - "$DECOY" "${PROJECTS}/../planted/decoy.json" <<'PY'
import json, sys, time
root = sys.argv[1]
json.dump({
    "slug": "decoy",
    "name": "decoy",
    "root": root,
    "languages": [],
    "last_opened": int(time.time()),
    "capsule": None,
}, open(sys.argv[2], "w"))
PY

decoy_objects() { find "${DECOY}/.git/objects" -type f 2>/dev/null | wc -l; }
before="$(decoy_objects)"
before_status="$(git_q -C "$DECOY" status --porcelain; git_q -C "$DECOY" ls-files --stage)"

for slug in "../planted/decoy" "../../planted/decoy" ".." "/etc" "planted/decoy"; do
    out="$("$Rime" agent worktrees --project "$slug" --json 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ok "a path-shaped slug is refused: --project ${slug} (exit ${rc})"
    else
        bad "a path-shaped slug is refused: --project ${slug}"
        printf '%s\n' "$out" | sed 's/^/      | /' >&2
    fi
done

after="$(decoy_objects)"
after_status="$(git_q -C "$DECOY" status --porcelain; git_q -C "$DECOY" ls-files --stage)"
if [ "$before" = "$after" ]; then
    ok "the decoy repository gained no objects — merge-tree never ran there (${after})"
else
    bad "the decoy repository gained no objects — merge-tree never ran there"
    echo "      ${before} objects before, ${after} after" >&2
fi
if [ "$before_status" = "$after_status" ]; then
    ok "the decoy repository's working tree and index are untouched"
else
    bad "the decoy repository's working tree and index are untouched"
fi

# A slug that IS a slug but names nothing is a different refusal, and it must
# still be a refusal rather than a silent empty listing.
out="$("$Rime" agent worktrees --project no-such-project --json 2>&1)"
if [ $? -ne 0 ] && printf '%s' "$out" | pipe_has "no remembered project"; then
    ok "an unknown slug is refused by name, not answered with an empty list"
else
    bad "an unknown slug is refused by name, not answered with an empty list"
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi

# ...and the real slug still works, so the guard did not simply break the verb.
out="$("$Rime" agent worktrees --project "$SLUG" --json 2>&1)"
if [ $? -eq 0 ] && printf '%s' "$out" | pipe_has '"name": "clash"'; then
    ok "the fixture's own slug still answers"
else
    bad "the fixture's own slug still answers"
    printf '%s\n' "$out" | sed 's/^/      | /' >&2
fi

# ── the human-readable form ──────────────────────────────────────────────────
section "the table a person reads"
out="$("$Rime" agent worktrees 2>&1)"
printf '%s\n' "$out" | sed 's/^/      | /'
if printf '%s' "$out" | pipe_has "WORKTREE" && printf '%s' "$out" | pipe_has "clash"; then
    ok "the table lists the worktrees under a header"
else
    bad "the table lists the worktrees under a header"
fi
# The claim this feature must never make. "TESTS" in a column heading invites
# "the tests pass"; the footer is where that is corrected, so its absence is a
# failure and not a cosmetic one.
if printf '%s' "$out" | pipe_has "last run Rime observed"; then
    ok "the table says the test column is the last run Rime observed, not a fresh result"
else
    bad "the table says the test column is the last run Rime observed, not a fresh result"
fi

# ── nothing of the user's was touched ────────────────────────────────────────
#
# The reason `1bb5db4` exists: session scratch used to be `/tmp/rime-agent/<id>`
# with no XDG in it, a fixture daemon numbers its sessions from 1, and a
# session reap runs `remove_dir_all` on its own id — so a suite could delete a
# live session's scratch out from under the real daemon.
section "the suite stayed inside its own fixture"
if [ -d "${RIME_AGENT_SCRATCH_ROOT}/${PROJ_SESSION}" ]; then
    ok "session scratch went where the fixture told it to (${RIME_AGENT_SCRATCH_ROOT}/${PROJ_SESSION})"
else
    bad "session scratch went where the fixture told it to"
    echo "      expected ${RIME_AGENT_SCRATCH_ROOT}/${PROJ_SESSION}" >&2
    ls -la "$RIME_AGENT_SCRATCH_ROOT" 2>&1 | sed 's/^/      /' >&2
fi
# The hazard is DELETION: a session reap runs `remove_dir_all` on its own id,
# a fixture daemon numbers its sessions from 1, and before `1bb5db4` that
# reaped the real daemon's session 1. An entry the real daemon ADDS while this
# suite runs is not this suite's doing, and comparing the whole listing made
# an unrelated session start fail this line.
gone=""
for entry in $PRE_SCRATCH; do
    [ -e "/tmp/rime-agent/${entry}" ] || gone="${gone}${entry} "
done
if [ -z "$gone" ]; then
    ok "nothing was deleted from the shared /tmp/rime-agent the real daemon uses"
else
    bad "nothing was deleted from the shared /tmp/rime-agent the real daemon uses"
    echo "      gone: ${gone}" >&2
    echo "      before: ${PRE_SCRATCH}" >&2
fi

printf '\nworktrees: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
