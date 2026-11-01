#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-ci-selector-parity.sh — a push to the integration branch must select
#  the same jobs the merge into `main` would select.
#
#  pr-validation.yml's `changes` job picks its diff base per event. Until
#  2026-09-22 the three arms disagreed about which question they were
#  answering:
#
#    pull_request       base.sha (i.e. `main`) .. PR head   -> the WHOLE branch
#    push               event.before .. new sha             -> ONE push
#    workflow_dispatch  merge-base(origin/main, sha) .. sha -> the WHOLE branch
#
#  So `roadmap/v2.2` could be green on every push for weeks and red the moment
#  it was proposed for merge. It was: `Installer safety and UI` ran on NONE of
#  that branch's 166 push runs, because no single push happened to touch
#  `installer/`, while every merge-shaped classification of the same tree ran
#  it — and found it red.
#
#  This gate executes the REAL `run:` block out of the workflow, against
#  synthetic repositories, and asserts the property in both directions:
#
#    * an integration-branch push selects exactly what the merge PR selects
#      -> fails on the old event.before base, which selects too little
#      -> fails on a base that selects EVERYTHING, which is the other
#         mis-scoping and would put a full matrix on every landing
#    * a TASK-branch push still classifies narrowly
#      -> fails if the fix is applied to every branch instead
#    * the all-zeros (new branch) and unreachable (force push) fallbacks, and
#      the refusal that stops `git diff "" ""` exiting 128, still hold
#    * the in-step integration list equals `on.push.branches`, so a branch
#      added to the trigger cannot silently keep the base that answers the
#      wrong question
#
#  Usage: tests/check-ci-selector-parity.sh [path/to/pr-validation.yml]
#  The argument exists so the same commit can be run against an older copy of
#  the workflow — `git show 'HEAD~1:.github/workflows/pr-validation.yml'` — to
#  demonstrate the gate red.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

WORKFLOW="${1:-.github/workflows/pr-validation.yml}"

# A missing prerequisite is a FAILURE here, never a skip: a suite that prints
# "0 passed, 0 failed" and exits 0 is invisible to the aggregate gate.
[ -f "$WORKFLOW" ] || { echo "FATAL: no workflow file at $WORKFLOW"; exit 1; }
command -v git >/dev/null     || { echo "FATAL: git is not installed"; exit 1; }
command -v python3 >/dev/null || { echo "FATAL: python3 is not installed"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

passed=0
failed=0
ok()  { passed=$((passed + 1)); echo "PASS: $1"; }
bad() { failed=$((failed + 1)); echo "FAIL: $1"; }

# ── pull the step's own code out of the workflow ─────────────────────────────
# Deliberately text, not a YAML library: the runner image is not contracted to
# carry PyYAML, and a gate that cannot run is worth nothing. Every anchor it
# depends on is asserted, so a restructured workflow fails loudly here instead
# of quietly checking nothing.
python3 - "$WORKFLOW" "$WORK" <<'PY'
import re
import sys

workflow, work = sys.argv[1], sys.argv[2]
lines = open(workflow, encoding='utf-8').read().splitlines()

starts = [i for i, l in enumerate(lines) if l.strip() == '- name: Classify changed paths']
if len(starts) != 1:
    sys.exit("FATAL: expected exactly one 'Classify changed paths' step, found %d" % len(starts))
i = starts[0]

run = None
while i < len(lines):
    if re.match(r'^\s*run: \|\s*$', lines[i]):
        run = i
        break
    if i > starts[0] and re.match(r'^\s*- name:', lines[i]):
        break
    i += 1
if run is None:
    sys.exit("FATAL: 'Classify changed paths' has no 'run: |' block")

indent = len(lines[run]) - len(lines[run].lstrip())
body_indent = indent + 2
body = []
for line in lines[run + 1:]:
    if line.strip() == '':
        body.append('')
        continue
    if len(line) - len(line.lstrip()) < body_indent:
        break
    body.append(line[body_indent:])
if len(body) < 20:
    sys.exit("FATAL: the classify block is %d lines; the anchors have moved" % len(body))
open(work + '/classify.sh', 'w', encoding='utf-8').write('\n'.join(body) + '\n')

# `on.push.branches`, in either the flow or the block form.
push = [i for i, l in enumerate(lines) if re.match(r'^  push:\s*$', l)]
if len(push) != 1:
    sys.exit("FATAL: expected exactly one top-level `push:` trigger, found %d" % len(push))
branches = []
j = push[0] + 1
while j < len(lines) and (lines[j].strip() == '' or lines[j].startswith('    ')):
    m = re.match(r'^    branches:\s*\[(.*)\]\s*$', lines[j])
    if m:
        branches = [b.strip().strip('"\'') for b in m.group(1).split(',') if b.strip()]
        break
    if re.match(r'^    branches:\s*$', lines[j]):
        k = j + 1
        while k < len(lines) and re.match(r'^      - ', lines[k]):
            branches.append(lines[k].split('- ', 1)[1].strip().strip('"\''))
            k += 1
        break
    j += 1
if not branches:
    sys.exit("FATAL: could not read on.push.branches")
open(work + '/push-branches', 'w', encoding='utf-8').write('\n'.join(sorted(branches)) + '\n')

# The in-step list. Absent is a legal parse and a failing assertion, not a
# crash: that is how this gate reads the workflow it was written against.
found = [l for l in body if re.match(r'^\s*INTEGRATION_BRANCHES="[^"]*"\s*$', l)]
listed = []
if len(found) == 1:
    listed = found[0].split('"')[1].split()
open(work + '/integration-branches', 'w', encoding='utf-8').write(
    ('\n'.join(sorted(listed)) + '\n') if listed else '')
PY
rc=$?
[ "$rc" -eq 0 ] || exit 1
[ -s "$WORK/classify.sh" ] || { echo "FATAL: extracted classify block is empty"; exit 1; }

# ── the fixtures ─────────────────────────────────────────────────────────────
# `main` never moves after the fork. The pull_request arm takes a two-dot
# `git diff base.sha head`, so a moved `main` would make the PR set a superset
# for a reason that is not the defect under test.
#
# Commit 1 touches installer/ only and commit 2 docs/ only, because those two
# light DIFFERENT single selectors — installer and rust. A fixture touching
# files/ or tests/ would light rust and engine together and lose the power to
# tell "selected too little" apart from "selected everything".
mkfixture() {
    local d="$1" br="$2"
    git init -q -b main "$d" >/dev/null 2>&1 || return 1
    git -C "$d" config user.email 'ci@rime.test'
    git -C "$d" config user.name 'rime ci'
    git -C "$d" config commit.gpgsign false
    echo base > "$d/README-base"
    git -C "$d" add -A && git -C "$d" commit -qm 'base'
    git -C "$d" update-ref refs/remotes/origin/main "$(git -C "$d" rev-parse HEAD)"
    git -C "$d" checkout -q -b "$br"
    mkdir -p "$d/installer" && echo x > "$d/installer/rime-installer-fixture"
    git -C "$d" add -A && git -C "$d" commit -qm 'installer only'
    mkdir -p "$d/docs" && echo y > "$d/docs/fixture.md"
    git -C "$d" add -A && git -C "$d" commit -qm 'docs only'
}

# Runs the workflow's own code and reports the job set it selected.
classify() {
    local d="$1" event="$2" base_sha="$3" head_sha="$4" before="$5" ref="$6" sha="$7"
    local out="$WORK/out" crc k set=''
    : > "$out"
    (
        cd "$d" || exit 90
        EVENT="$event" BASE_SHA="$base_sha" HEAD_SHA="$head_sha" \
        PUSH_BEFORE="$before" REF_NAME="$ref" SHA="$sha" \
        GITHUB_OUTPUT="$out" bash "$WORK/classify.sh"
    ) > "$WORK/log" 2>&1
    crc=$?
    for k in android engine installer rust; do
        grep -qx "$k=true" "$out" && set="$set,$k"
    done
    echo "rc=$crc jobs=[${set#,}]"
}

expect() {
    local what="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then
        ok "$what — $got"
    else
        bad "$what — want $want got $got"
        sed -n '1,20p' "$WORK/log" | sed 's/^/      | /'
    fi
}

INT="$WORK/int"
mkfixture "$INT" 'roadmap/v2.2' >/dev/null 2>&1 || { echo "FATAL: fixture build failed"; exit 1; }
base_main="$(git -C "$INT" rev-parse main)"
c1="$(git -C "$INT" rev-parse HEAD~1)"
c2="$(git -C "$INT" rev-parse HEAD)"

pr="$(classify "$INT" pull_request "$base_main" "$c2" '' 'roadmap/v2.2' "$c2")"
push="$(classify "$INT" push '' '' "$c1" 'roadmap/v2.2' "$c2")"

# The control. If this stops being exactly two selectors the parity assertion
# below can be satisfied by a selector that simply runs everything.
expect 'the merge PR selects exactly installer and rust' 'rc=0 jobs=[installer,rust]' "$pr"
expect 'a push to the integration branch selects what the merge PR selects' "$pr" "$push"

TASK="$WORK/task"
mkfixture "$TASK" 'task/selector-fixture' >/dev/null 2>&1 || { echo "FATAL: fixture build failed"; exit 1; }
t1="$(git -C "$TASK" rev-parse HEAD~1)"
t2="$(git -C "$TASK" rev-parse HEAD)"

expect 'a push to a TASK branch still classifies narrowly' \
    'rc=0 jobs=[rust]' \
    "$(classify "$TASK" push '' '' "$t1" 'task/selector-fixture' "$t2")"
expect 'an all-zeros before (new branch) falls back to the merge base' \
    'rc=0 jobs=[installer,rust]' \
    "$(classify "$TASK" push '' '' '0000000000000000000000000000000000000000' 'task/selector-fixture' "$t2")"
expect 'an unreachable before (force push) falls back to the merge base' \
    'rc=0 jobs=[installer,rust]' \
    "$(classify "$TASK" push '' '' 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef' 'task/selector-fixture' "$t2")"

# No before AND no origin/main: nothing can be determined, so everything runs —
# and in particular the step does not reach `git diff "" ""` and exit 128
# before a single test has run, which is how the first push-triggered run of
# this workflow died.
ORPHAN="$WORK/orphan"
mkfixture "$ORPHAN" 'task/selector-fixture' >/dev/null 2>&1 || { echo "FATAL: fixture build failed"; exit 1; }
git -C "$ORPHAN" update-ref -d refs/remotes/origin/main
expect 'no usable range selects every suite and does not exit 128' \
    'rc=0 jobs=[android,engine,installer,rust]' \
    "$(classify "$ORPHAN" push '' '' '' 'task/selector-fixture' "$(git -C "$ORPHAN" rev-parse HEAD)")"

# A file OUTSIDE installer/ that an installer suite reads must still select the
# installer jobs. installer/test-installer-console.sh runs the 50-rime-keymap
# generator, and that suite runs only in installer-ui: a PR changing only the
# generator lit rust+engine (files/) and skipped it.
KEYGEN="$WORK/keygen"
{
    git init -q -b main "$KEYGEN" &&
    git -C "$KEYGEN" config user.email 'ci@rime.test' &&
    git -C "$KEYGEN" config user.name 'rime ci' &&
    git -C "$KEYGEN" config commit.gpgsign false &&
    echo base > "$KEYGEN/README-base" &&
    git -C "$KEYGEN" add -A && git -C "$KEYGEN" commit -qm 'base' &&
    git -C "$KEYGEN" checkout -q -b 'task/keymap-generator' &&
    mkdir -p "$KEYGEN/files/system/systemd" &&
    echo x > "$KEYGEN/files/system/systemd/rime-keymap-env" &&
    git -C "$KEYGEN" add -A && git -C "$KEYGEN" commit -qm 'generator only'
} >/dev/null 2>&1 || { echo "FATAL: fixture build failed"; exit 1; }
expect 'a PR touching only the keymap generator selects the installer jobs' \
    'rc=0 jobs=[engine,installer,rust]' \
    "$(classify "$KEYGEN" pull_request "$(git -C "$KEYGEN" rev-parse main)" \
        "$(git -C "$KEYGEN" rev-parse HEAD)" '' 'task/keymap-generator' "$(git -C "$KEYGEN" rev-parse HEAD)")"

# ── the trigger and the in-step list must agree ──────────────────────────────
if diff -q "$WORK/push-branches" "$WORK/integration-branches" >/dev/null 2>&1; then
    ok "on.push.branches equals the step's INTEGRATION_BRANCHES ($(tr '\n' ' ' < "$WORK/push-branches"))"
else
    bad 'on.push.branches equals the step INTEGRATION_BRANCHES list'
    echo "      | on.push.branches:     $(tr '\n' ' ' < "$WORK/push-branches")"
    echo "      | INTEGRATION_BRANCHES: $(tr '\n' ' ' < "$WORK/integration-branches")"
fi

echo
echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
