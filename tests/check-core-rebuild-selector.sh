#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-core-rebuild-selector.sh — core rebuilds when the PUBLISHED core is
#  behind this commit, not only when the last push touched core.
#
#  build-image.yml's `changes` job decides whether the ~45-minute core tier
#  rebuilds. Until 2026-09-23 the only content trigger was dorny/paths-filter,
#  which diffs against the PREVIOUS PUSH. On main:
#
#    57f593ad  2026-09-05  last core actually published
#    d7fb8f1e  1,485-commit merge, core changes included — core build cancelled
#    9ae4ebf3  touches Containerfile.kernel and a test only
#
#  9ae4ebf3 looked core-clean against d7fb8f1e, so run 35791485108 skipped core,
#  built base FROM the three-week-old core, and base died on
#  `test -x /usr/bin/xdg-terminal-exec` — a package only the tip's core has.
#
#  This gate extracts the REAL `Decide whether core must rebuild` step out of
#  the workflow and executes it against a synthetic repository, with a fake
#  `skopeo` on PATH standing in for the registry, and asserts:
#
#    * published core older than the previous push, core paths changed between
#      them but not in the last push -> REBUILD            (the defect)
#    * published core already at / after the last core change -> REUSE
#      -> fails a gate that simply rebuilds every time, the other mis-scoping
#    * every doubt rebuilds: no image, no revision label, a non-sha label, a
#      sha not in history, a registry that never answers
#    * a published sha that is not an ancestor (main's merge commit, seen from
#      a dispatch of the integration branch) is judged by its TREE: reuse when
#      core's inputs match, rebuild when they differ
#    * the legacy-name fallback, force_core, the push filter and the weekly
#      upstream-digest comparison still behave as before
#    * the step's CORE_PATHS equals the path filter's `core:` list, so the two
#      triggers cannot silently disagree about what core's inputs are
#
#  Expressions (`${{ … }}`) are resolved by this harness, in the step's env:
#  block and in its body alike, and an unknown one is FATAL. That is what lets
#  the same harness run the pre-fix step, whose inputs were inline:
#
#      git show 'HEAD~1:.github/workflows/build-image.yml' > /var/lab-scratch/old.yml
#      tests/check-core-rebuild-selector.sh /var/lab-scratch/old.yml
#
#  `steps.filter.outputs.core` is emulated as dorny/paths-filter computes it
#  on a push: the previous push's sha .. this sha, over the filter's own list.
#
#  Usage: tests/check-core-rebuild-selector.sh [path/to/build-image.yml]
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 1

WORKFLOW="${1:-.github/workflows/build-image.yml}"

# A missing prerequisite is a FAILURE here, never a skip.
[ -f "$WORKFLOW" ] || { echo "FATAL: no workflow file at $WORKFLOW"; exit 1; }
command -v git >/dev/null     || { echo "FATAL: git is not installed"; exit 1; }
command -v python3 >/dev/null || { echo "FATAL: python3 is not installed"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

passed=0
failed=0
ok()  { passed=$((passed + 1)); echo "PASS: $1"; }
bad() { failed=$((failed + 1)); echo "FAIL: $1"; }

# ── pull the step out of the workflow ────────────────────────────────────────
# Text, not a YAML library, for the reason check-ci-selector-parity.sh gives:
# the runner image is not contracted to carry PyYAML. Every anchor is asserted.
python3 - "$WORKFLOW" "$WORK" <<'PY'
import json
import re
import sys

workflow, work = sys.argv[1], sys.argv[2]
lines = open(workflow, encoding='utf-8').read().splitlines()

starts = [i for i, l in enumerate(lines) if l.strip() == '- name: Decide whether core must rebuild']
if len(starts) != 1:
    sys.exit("FATAL: expected exactly one 'Decide whether core must rebuild' step, found %d" % len(starts))
s = starts[0]
step_indent = len(lines[s]) - len(lines[s].lstrip()) + 2

env, run, i = {}, None, s + 1
while i < len(lines):
    l = lines[i]
    if l.strip() and len(l) - len(l.lstrip()) < step_indent:
        break
    if re.match(r'^\s*env:\s*$', l) and len(l) - len(l.lstrip()) == step_indent:
        j = i + 1
        while j < len(lines):
            m = re.match(r'^(\s*)([A-Z_][A-Z0-9_]*):\s*(.*?)\s*$', lines[j])
            if not m or len(m.group(1)) <= step_indent:
                break
            env[m.group(2)] = m.group(3)
            j += 1
    if re.match(r'^\s*run: \|\s*$', l):
        run = i
        break
    i += 1
if run is None:
    sys.exit("FATAL: the gate step has no 'run: |' block")

body_indent = len(lines[run]) - len(lines[run].lstrip()) + 2
body = []
for line in lines[run + 1:]:
    if line.strip() == '':
        body.append('')
        continue
    if len(line) - len(line.lstrip()) < body_indent:
        break
    body.append(line[body_indent:])
if len(body) < 10 or not any('decide()' in b for b in body):
    sys.exit("FATAL: the gate block is %d lines with no decide(); the anchors have moved" % len(body))
open(work + '/gate.tmpl', 'w', encoding='utf-8').write('\n'.join(body) + '\n')
json.dump(env, open(work + '/gate.env.json', 'w'))

# The path filter's `core:` list.
f = [i for i, l in enumerate(lines) if re.match(r'^\s*core:\s*$', l) and i > 0
     and any('paths-filter' in x for x in lines[max(0, i - 12):i])]
if len(f) != 1:
    sys.exit("FATAL: expected exactly one `core:` list under the paths-filter step, found %d" % len(f))
paths = []
for l in lines[f[0] + 1:]:
    m = re.match(r"^\s*-\s*'([^']+)'\s*$", l)
    if m:
        paths.append(m.group(1))
        continue
    if l.strip().startswith('#') or not l.strip():
        continue
    break
if not paths:
    sys.exit("FATAL: the paths-filter `core:` list is empty")
# dorny globs -> git pathspecs: 'kernel/**' is the directory 'kernel/'.
spec = sorted(p[:-2] if p.endswith('/**') else p for p in paths)
open(work + '/filter-core', 'w').write('\n'.join(spec) + '\n')

cp = [b for b in body if re.match(r'^\s*CORE_PATHS="[^"]*"\s*$', b)]
open(work + '/core-paths', 'w').write(
    ('\n'.join(sorted(cp[0].split('"')[1].split())) + '\n') if len(cp) == 1 else '')
PY
[ "$?" -eq 0 ] || exit 1

# Renders the step for one case: every ${{ expr }} in env: and body resolved
# from the context in $WORK/ctx.json. An expression the harness does not know
# is FATAL, so a new input cannot be silently rendered as "".
cat > "$WORK/render.py" <<'PY'
import json
import re
import shlex
import sys

work = sys.argv[1]
ctx = json.load(open(work + '/ctx.json'))
env = json.load(open(work + '/gate.env.json'))
body = open(work + '/gate.tmpl', encoding='utf-8').read()

def sub(text):
    def one(m):
        k = m.group(1).strip()
        if k not in ctx:
            sys.exit("FATAL: the harness does not know the expression ${{ %s }}" % k)
        return ctx[k]
    return re.sub(r'\$\{\{\s*([^}]*?)\s*\}\}', one, text)

out = ['#!/usr/bin/env bash']
for k, v in env.items():
    out.append('export %s=%s' % (k, shlex.quote(sub(v))))
out.append(sub(body))
open(work + '/gate.sh', 'w', encoding='utf-8').write('\n'.join(out) + '\n')
PY

# ── the fake registry ────────────────────────────────────────────────────────
# FAKE_CORE / FAKE_LEGACY hold "revision|fedora-bootc-digest" for
# $IMAGE:core and $LEGACY_CORE; the word FAIL makes that ref unreadable.
# FAKE_UPSTREAM is the fedora-bootc digest. The --format asked for decides
# which fields are printed, so the pre-fix step (which asked for the digest
# label alone) is served correctly too. The legacy core is the pre-rebrand
# one, so its digest is served under the pre-rebrand label spelling
# (org.apexos.…) and the new name's under the new one. (rime-rename: keep)
mkdir -p "$WORK/bin"
cat > "$WORK/bin/skopeo" <<'SH'
#!/usr/bin/env bash
fmt='' ref=''
while [ $# -gt 0 ]; do
    case "$1" in
        --format) fmt="$2"; shift 2 ;;
        --creds) shift 2 ;;
        docker://*) ref="${1#docker://}"; shift ;;
        *) shift ;;
    esac
done
echo "$ref" >> "$FAKE_LOG"
case "$ref" in
    "$UPSTREAM_BASE") [ "$FAKE_UPSTREAM" = FAIL ] && exit 1; echo "$FAKE_UPSTREAM"; exit 0 ;;
    "$IMAGE:$TAG_CORE") meta="$FAKE_CORE" old=0 ;;
    "$LEGACY_CORE") meta="$FAKE_LEGACY" old=1 ;;
    *) echo "fake skopeo: unexpected ref $ref" >&2; exit 2 ;;
esac
[ "$meta" = FAIL ] && { echo "manifest unknown" >&2; exit 1; }
rev="${meta%%|*}" dig="${meta#*|}"
new_dig="$dig" old_dig='<no value>'
[ "$old" = 1 ] && { new_dig='<no value>'; old_dig="$dig"; }
[ -z "$dig" ] && { new_dig='<no value>'; old_dig='<no value>'; }
case "$fmt" in
    *image.revision*rimeos.fedora-bootc*apexos.fedora-bootc*) echo "$rev|$new_dig|$old_dig" ;;  # rime-rename: keep
    *image.revision*fedora-bootc*) echo "$rev|$dig" ;;
    *fedora-bootc*image.revision*) echo "$dig|$rev" ;;
    *image.revision*) echo "$rev" ;;
    *fedora-bootc*) echo "$dig" ;;
    *) echo "fake skopeo: unexpected format $fmt" >&2; exit 2 ;;
esac
SH
chmod +x "$WORK/bin/skopeo"

# ── the fixture repository ───────────────────────────────────────────────────
#   c0  published core built here
#   c1  Containerfile.core changes        <- the previous push; its core build
#                                            was cancelled
#   c2  Containerfile.kernel + tests/ only <- the last push
#   c3  kernel/kernel.pin changes
#   c4  docs only
#   side-old, side  commits off c0 that are NOT ancestors of anything above;
#                   side-old has c0's core inputs, side has c4's
R="$WORK/repo"
(
    set -e
    git init -q -b main "$R"
    git -C "$R" config user.email 'ci@rime.test'
    git -C "$R" config user.name 'rime ci'
    git -C "$R" config commit.gpgsign false
    mkdir -p "$R/kernel" "$R/tests" "$R/docs"
    echo 'FROM x'  > "$R/Containerfile.core"
    echo 'FROM k'  > "$R/Containerfile.kernel"
    echo 'pin 1'   > "$R/kernel/kernel.pin"
    git -C "$R" add -A && git -C "$R" commit -qm c0
    echo 'RUN dnf install xdg-terminal-exec' >> "$R/Containerfile.core"
    git -C "$R" add -A && git -C "$R" commit -qm c1
    echo 'RUN curl --retry-all-errors' >> "$R/Containerfile.kernel"
    echo 't' > "$R/tests/t.rs"
    git -C "$R" add -A && git -C "$R" commit -qm c2
    echo 'pin 2' > "$R/kernel/kernel.pin"
    git -C "$R" add -A && git -C "$R" commit -qm c3
    echo 'd' > "$R/docs/d.md"
    git -C "$R" add -A && git -C "$R" commit -qm c4
    # side-old: off c0 with c0's core inputs. side: then given c4's.
    git -C "$R" checkout -q -b side HEAD~4
    echo 's' > "$R/side.txt"
    git -C "$R" add -A && git -C "$R" commit -qm side-old
    git -C "$R" checkout -q main -- Containerfile.core kernel
    git -C "$R" add -A && git -C "$R" commit -qm side
    git -C "$R" checkout -q main
) >/dev/null 2>&1 || { echo "FATAL: fixture build failed"; exit 1; }
c() { git -C "$R" rev-parse "$1"; }
C0="$(c main~4)"; C1="$(c main~3)"; C2="$(c main~2)"; C3="$(c main~1)"; C4="$(c main)"; SIDE="$(c side)"; SIDE_OLD="$(c side~1)"
mapfile -t FILTER < "$WORK/filter-core"

# Where the core lived before the rebrand: build-image.yml's LEGACY_CORE.
LEGACY_REF=ghcr.io/test/apex-os:core  # rime-rename: keep (the pre-rebrand name)

# gate EVENT BEFORE SHA FORCE CORE_META LEGACY_META UPSTREAM
gate() {
    local event="$1" before="$2" sha="$3" force="$4" filter=false out="$WORK/out" rc
    FAKE_CORE="$5" FAKE_LEGACY="$6" FAKE_UPSTREAM="$7"
    # What dorny/paths-filter would say on a push: previous push .. this sha.
    if [ -n "$before" ] && [ -n "$(git -C "$R" diff --name-only "$before" "$sha" -- "${FILTER[@]}")" ]; then
        filter=true
    fi
    python3 - "$WORK" "$event" "$sha" "$force" "$filter" <<'PY'
import json, sys
w, event, sha, force, filt = sys.argv[1:]
json.dump({
    'inputs.force_core': force,
    'steps.filter.outputs.core': filt,
    'github.event_name': event,
    'github.sha': sha,
    'github.actor': 'rime-ci',
    'secrets.GITHUB_TOKEN': 'not-a-token',
}, open(w + '/ctx.json', 'w'))
PY
    python3 "$WORK/render.py" "$WORK" || { echo "rc=render"; return; }
    : > "$out"; : > "$WORK/skopeo.log"
    (
        cd "$R" && git checkout -q --detach "$sha" || exit 90
        PATH="$WORK/bin:$PATH" GITHUB_OUTPUT="$out" RUNNER_TEMP="$WORK" \
        FAKE_LOG="$WORK/skopeo.log" FAKE_CORE="$FAKE_CORE" FAKE_LEGACY="$FAKE_LEGACY" \
        FAKE_UPSTREAM="$FAKE_UPSTREAM" INSPECT_BACKOFF=0 \
        IMAGE=ghcr.io/test/rime-os TAG_CORE=core LEGACY_CORE="$LEGACY_REF" \
        UPSTREAM_BASE=quay.io/test/fedora-bootc:43 \
        bash "$WORK/gate.sh"
    ) > "$WORK/log" 2>&1
    rc=$?
    git -C "$R" checkout -q main 2>/dev/null
    # The reason must be SAID, not just the verdict written.
    grep -q '^core rebuild: \(true\|false\) (' "$WORK/log" || { echo "rc=$rc core=<no reason logged>"; return; }
    echo "rc=$rc core=$(sed -n 's/^core=//p' "$out" | tail -1)"
}

# expect WHAT WANT GOT [REASON] — REASON, when given, must appear in the logged
# decision, so a case cannot pass on a different arm than the one it is for.
expect() {
    local what="$1" want="$2" got="$3" reason="${4:-}"
    if [ -n "$reason" ] && ! grep '^core rebuild:' "$WORK/log" | pipe_has -F -- "$reason"; then
        got="$got reason=[$(grep -m1 '^core rebuild:' "$WORK/log" | sed 's/^core rebuild: //')]"
        want="$want reason~[$reason]"
    fi
    if [ "$want" = "$got" ]; then
        ok "$what — $got ($(grep -m1 '^core rebuild:' "$WORK/log" | sed 's/^core rebuild: //'))"
    else
        bad "$what — want $want got $got"
        sed -n '1,25p' "$WORK/log" | sed 's/^/      | /'
    fi
}

D1=sha256:1111 D2=sha256:2222

# ── the defect ───────────────────────────────────────────────────────────────
expect 'published core older than the previous push, core changed between them, last push core-clean -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C1" "$C2" false "$C0|$D1" FAIL "$D1")" 'since published core'
expect 'the same, with a docs-only last push further on -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false "$C0|$D1" FAIL "$D1")"
expect 'kernel/** changed since the published core, not in the last push -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false "$C2|$D1" FAIL "$D1")" 'since published core'

# ── the controls: it must not simply rebuild every time ──────────────────────
expect 'published core at the last core change, last push core-clean -> REUSE' \
    'rc=0 core=false' "$(gate push "$C1" "$C2" false "$C1|$D1" FAIL "$D1")"
expect 'published core at this very commit -> REUSE' \
    'rc=0 core=false' "$(gate push "$C3" "$C4" false "$C4|$D1" FAIL "$D1")"
expect 'a dispatch with nothing core-relevant since the published core -> REUSE' \
    'rc=0 core=false' "$(gate workflow_dispatch '' "$C4" false "$C3|$D1" FAIL "$D1")"

# ── every doubt rebuilds ─────────────────────────────────────────────────────
expect 'published core carries no revision label -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false "|$D1" FAIL "$D1")" 'no org.opencontainers.image.revision'
expect "published core revision is 'dev' -> REBUILD" \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false "dev|$D1" FAIL "$D1")" 'is not a commit sha'
expect 'published core revision is not in this history -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef|$D1" FAIL "$D1")" 'not in this repository'
# A dispatch of the integration branch: :core is stamped with a MAIN merge
# commit, which is never an ancestor of the branch. Identical core inputs must
# reuse; an ancestry requirement would rebuild core on every such dispatch.
expect 'published core stamped with a non-ancestor sha, identical core inputs -> REUSE' \
    'rc=0 core=false' "$(gate workflow_dispatch '' "$C4" false "$SIDE|$D1" FAIL "$D1")" 'no core-relevant change since published core'
expect 'published core stamped with a non-ancestor sha, differing core inputs -> REBUILD' \
    'rc=0 core=true' "$(gate workflow_dispatch '' "$C4" false "$SIDE_OLD|$D1" FAIL "$D1")" 'since published core'
expect 'no published core readable under either name -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false FAIL FAIL "$D1")" 'no published core could be read'

# ── the existing triggers still hold ─────────────────────────────────────────
expect 'core absent under its new name, legacy name current -> REUSE (transitional fallback)' \
    'rc=0 core=false' "$(gate push "$C3" "$C4" false FAIL "$C4|$D1" "$D1")" 'no core-relevant change since published core'
expect 'core absent under its new name, legacy name stale -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C3" "$C4" false FAIL "$C0|$D1" "$D1")"
expect 'force_core with a current published core -> REBUILD' \
    'rc=0 core=true' "$(gate workflow_dispatch '' "$C4" true "$C4|$D1" FAIL "$D1")"
expect 'the last push itself touched core -> REBUILD' \
    'rc=0 core=true' "$(gate push "$C0" "$C1" false "$C1|$D1" FAIL "$D1")"
expect 'weekly cron, published core current, upstream unmoved -> REUSE' \
    'rc=0 core=false' "$(gate schedule '' "$C4" '' "$C4|$D1" FAIL "$D1")"
expect 'weekly cron, published core current, upstream moved -> REBUILD' \
    'rc=0 core=true' "$(gate schedule '' "$C4" '' "$C4|$D1" FAIL "$D2")"
# The rebrand: the first weekly run finds core only under its pre-rebrand
# name, stamped with the pre-rebrand label. Reading only org.rimeos.… would call
# that "no fedora-bootc digest" and rebuild a current core for nothing.
expect 'weekly cron, core only under the legacy name with the old label, upstream unmoved -> REUSE' \
    'rc=0 core=false' "$(gate schedule '' "$C4" '' FAIL "$C4|$D1" "$D1")" 'upstream fedora-bootc has not moved'
expect 'weekly cron, core only under the legacy name with the old label, upstream moved -> REBUILD' \
    'rc=0 core=true' "$(gate schedule '' "$C4" '' FAIL "$C4|$D1" "$D2")" 'upstream fedora-bootc moved'
expect 'weekly cron, published core carries no fedora-bootc digest -> REBUILD' \
    'rc=0 core=true' "$(gate schedule '' "$C4" '' "$C4|" FAIL "$D1")"
expect 'weekly cron, published core stale -> REBUILD whatever upstream did' \
    'rc=0 core=true' "$(gate schedule '' "$C4" '' "$C0|$D1" FAIL "$D1")"

# ── the two triggers agree about what core's inputs are ──────────────────────
if [ -s "$WORK/core-paths" ] && diff -q "$WORK/filter-core" "$WORK/core-paths" >/dev/null; then
    ok "the step's CORE_PATHS equals the path filter's core list ($(tr '\n' ' ' < "$WORK/core-paths"))"
else
    bad "the step's CORE_PATHS equals the path filter's core list"
    echo "      | filter core: $(tr '\n' ' ' < "$WORK/filter-core")"
    echo "      | CORE_PATHS:  $(tr '\n' ' ' < "$WORK/core-paths")"
fi

# ── every file core COPYs is one of core's inputs ────────────────────────────
# A COPY source outside the list changes core's content without rebuilding it:
# the change ships only when something else happens to rebuild core.
uncovered="$(python3 - "$WORK/filter-core" "${CONTAINERFILE:-Containerfile.core}" <<'PY2'
import re, shlex, sys
spec = [l.strip() for l in open(sys.argv[1]) if l.strip()]
text = open(sys.argv[2], encoding='utf-8').read().replace('\\\n', ' ')
srcs = []
for line in text.splitlines():
    m = re.match(r'^\s*(COPY|ADD)\s+(.*)$', line)
    if not m:
        continue
    words = [w for w in shlex.split(m.group(2), comments=False)]
    if any(w.startswith('--from') for w in words):
        continue
    args = [w for w in words if not w.startswith('--')]
    for src in args[:-1]:
        if '://' not in src:
            srcs.append(src)
if not srcs:
    sys.exit("FATAL: found no COPY source in %s" % sys.argv[2])
def covered(src):
    return any(src == p or src + '/' == p or (p.endswith('/') and src.startswith(p)) for p in spec)
print(' '.join(s for s in srcs if not covered(s)))
PY2
)"
if [ $? -ne 0 ]; then
    bad "every COPY source of Containerfile.core is a core input (could not parse it)"
elif [ -z "$uncovered" ]; then
    ok "every COPY source of Containerfile.core is a core input"
else
    bad "every COPY source of Containerfile.core is a core input"
    echo "      | not in the core list: $uncovered"
fi

echo
echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
