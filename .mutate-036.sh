#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Mutation harness for P1-036. Break one claim, run the suite, restore,
#  report the counts. UNTRACKED on purpose — do not commit it.
#
#  Two rules learned from the P1-035 runner, both of which manufactured false
#  survivors before they were fixed:
#
#    1. A splice that did not apply (an `assert count==1` tripped, a pattern
#       drifted) would run the suite against UNMUTATED source and report the
#       clean baseline — indistinguishable from a surviving mutant. So `run`
#       refuses to proceed unless the tree actually changed, and prints what
#       it mutated.
#    2. Restore by COPY from a backup taken before the splice, never by
#       `git checkout`. Nothing in this worktree may be reverted by git while
#       the roadmap run is in progress.
#
#  Baseline at 3ec4467: tests/test-agent-worktrees.sh = 53 passed, 0 failed.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd /var/tmp/apex-work/wt-p1-035 || exit 2

FILES="apexd/apex-agent-core/src/worktree.rs
apexd/apex-agent-core/src/git.rs
apexd/apex-agent-core/src/hook.rs
apexd/apex-agentd/src/worktrees.rs
apexd/apex-agent-core/src/project.rs
apexd/apex/src/agent.rs"

BACKUP="/var/tmp/mut036-backup"

save() {
    rm -rf "$BACKUP"; mkdir -p "$BACKUP"
    for f in $FILES; do mkdir -p "$BACKUP/$(dirname "$f")"; cp "$f" "$BACKUP/$f"; done
}
restore() {
    for f in $FILES; do cp "$BACKUP/$f" "$f"; done
}

changed() {
    for f in $FILES; do
        cmp -s "$f" "$BACKUP/$f" || { echo "$f"; }
    done
}

run() {   # run <tag>
    local tag="$1" what
    what="$(changed | tr '\n' ' ')"
    if [ -z "$what" ]; then
        echo "MUT ${tag}: SPLICE DID NOT APPLY — tree matches the backup, refusing to run"
        return
    fi
    echo "MUT ${tag}: mutated ${what}"
    (cd apexd && nice -n 15 cargo build --locked --bin apex-agentd --bin apex) \
        >"/var/tmp/mut036-${tag}-build.log" 2>&1
    if [ $? -ne 0 ]; then
        echo "MUT ${tag}: BUILD FAILED"
        grep -E "^error" -A 6 "/var/tmp/mut036-${tag}-build.log" | head -20
        return
    fi
    timeout 1200 ./tests/test-agent-worktrees.sh >"/var/tmp/mut036-${tag}.log" 2>&1
    echo "MUT ${tag}: $(tail -1 /var/tmp/mut036-${tag}.log)"
    grep '^FAIL' "/var/tmp/mut036-${tag}.log" | sed 's/^/     /'
}

save

case "${1:-}" in
0)  # the baseline, unmutated, so the counts below have something to be against
    echo "BASELINE:"
    (cd apexd && nice -n 15 cargo build --locked --bin apex-agentd --bin apex) >/dev/null 2>&1
    timeout 1200 ./tests/test-agent-worktrees.sh >/var/tmp/mut036-base.log 2>&1
    echo "  $(tail -1 /var/tmp/mut036-base.log)"
    grep '^FAIL' /var/tmp/mut036-base.log | sed 's/^/     /' ;;

1)  # THE one. Ask the conflict question with a REAL merge, in the worktree.
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/worktree.rs'
s=io.open(p,encoding='utf-8').read()
old="""                match git::merge_tree_probe(repo, base, branch) {
                    git::MergeProbe::Clean => ConflictState::Clean,
                    git::MergeProbe::Conflicted(paths) => ConflictState::Conflicted { paths },
                    git::MergeProbe::Unknown(reason) => ConflictState::Unknown { reason },
                }"""
new="""                {
                    // MUTATION: the obvious implementation. A real merge, in
                    // the worktree somebody is sitting in.
                    let out = std::process::Command::new("git")
                        .current_dir(&wt.path)
                        .args(["merge", "--no-commit", "--no-ff", base])
                        .output();
                    match out {
                        Ok(o) if o.status.success() => ConflictState::Clean,
                        Ok(_) => ConflictState::Conflicted {
                            paths: vec!["f.txt".to_string()],
                        },
                        Err(e) => ConflictState::Unknown { reason: e.to_string() },
                    }
                }"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 1; restore ;;

2)  # ahead and behind, the other way round
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/git.rs'
s=io.open(p,encoding='utf-8').read()
old="""    let behind: u32 = parts.next()?.parse().ok()?;
    let ahead: u32 = parts.next()?.parse().ok()?;"""
new="""    let ahead: u32 = parts.next()?.parse().ok()?;
    let behind: u32 = parts.next()?.parse().ok()?;"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 2; restore ;;

3)  # a session belongs to every worktree that contains it, not the deepest
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/worktree.rs'
s=io.open(p,encoding='utf-8').read()
old="""    worktrees
        .iter()
        .filter(|wt| dir == wt.path || dir.starts_with(&wt.path))
        .max_by_key(|wt| wt.path.components().count())"""
new="""    // MUTATION: the first worktree that contains it. `project::worktrees`
    // puts the main tree first, and every agent worktree lives under it.
    worktrees
        .iter()
        .find(|wt| dir == wt.path || dir.starts_with(&wt.path))"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 3; restore ;;

4)  # a linked worktree remembered as a project is listed like any other
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/worktrees.rs'
s=io.open(p,encoding='utf-8').read()
old="""        if git::is_linked_worktree(Path::new(&proj.root)) {"""
new="""        if false && git::is_linked_worktree(Path::new(&proj.root)) {"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 4; restore ;;

5)  # resolve the slug by joining it onto the store path again
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/worktrees.rs'
s=io.open(p,encoding='utf-8').read()
old="""        Some(slug) => match project::list().into_iter().find(|p| p.slug == slug) {"""
new="""        Some(slug) => match project::load(slug) {"""
assert s.count(old)==1
s=s.replace(old,new)
io.open(p,'w',encoding='utf-8').write(s)

# ...and drop the second barrier too, so the mutant is the pre-fix behaviour.
p='apexd/apex-agent-core/src/project.rs'
s=io.open(p,encoding='utf-8').read()
old="""    if !is_record_slug(slug) {
        return None;
    }
    Some(paths::projects_dir().join(format!("{slug}.json")))"""
new="""    Some(paths::projects_dir().join(format!("{slug}.json")))"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 5; restore ;;

6)  # trust the hook payload's cwd instead of the session's own
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/worktrees.rs'
s=io.open(p,encoding='utf-8').read()
old="""    pub fn record(&self, cwd: &Path, note: &TestNote) {
        let Some(top) = git::toplevel(cwd) else {
            return;
        };"""
new="""    pub fn record(&self, cwd: &Path, note: &TestNote) {
        // MUTATION: the directory the hook process ran in, which is a thing
        // the caller controls, rather than the session's recorded cwd.
        let cwd = &std::env::current_dir().unwrap_or_else(|_| cwd.to_path_buf());
        let Some(top) = git::toplevel(cwd) else {
            return;
        };"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 6; restore ;;

7)  # an unknown conflict state is as good as clean
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/worktree.rs'
s=io.open(p,encoding='utf-8').read()
old="""        ConflictState::Unknown { reason } => {
            blockers.push(format!("conflict state unknown: {reason}"))
        }"""
new="""        ConflictState::Unknown { .. } => {}"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 7; restore ;;

8)  # NO LAYER resolves the session's cwd before comparing it to a worktree.
    # Both are removed on purpose: the shipped CLI canonicalizes `--cwd`
    # (agent.rs `run`), so removing only the daemon's resolution leaves the
    # property intact and the mutant survives — measured. The question worth
    # asking is whether SOME layer must do it, and this is that question.
    python3 - <<'PY2'
import io
p='apexd/apex-agentd/src/worktrees.rs'
s=io.open(p,encoding='utf-8').read()
old="""            let cwd = PathBuf::from(&s.cwd);
            let real = cwd.canonicalize().unwrap_or(cwd);
            (s.id, real)"""
new="""            // MUTATION: compare the cwd exactly as the caller gave it.
            (s.id, PathBuf::from(&s.cwd))"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))

p='apexd/apex/src/agent.rs'
s=io.open(p,encoding='utf-8').read()
old="""        Some(dir) => dir
            .canonicalize()
            .with_context(|| format!("{} does not exist", dir.display()))?,"""
new="""        Some(dir) => dir.clone(),"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY2
    run 8; restore ;;

*)  echo "usage: .mutate-036.sh <0|1|2|3|4|5|6|7|8>"; exit 2 ;;
esac

# The tree must be back to the backup before the next mutation runs.
left="$(changed | tr '\n' ' ')"
[ -z "$left" ] && echo "restored: tree matches the backup" \
               || echo "NOT RESTORED: ${left}"
