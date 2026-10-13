#!/usr/bin/env bash
# Mutation harness: break one thing, run the suite, restore, report the counts.
set -uo pipefail
cd /var/tmp/apex-work/wt-p1-035 || exit 2
export CARGO_TARGET_DIR=/var/tmp/apex-build-cache/p1-035

run() {   # run <tag>
    local tag="$1"
    # A splice that did not apply (python assert tripped, pattern drifted) would
    # otherwise run the suite against UNMUTATED source and report 48/0 — which is
    # indistinguishable from a surviving mutant. Refuse to run on a clean tree.
    if [ -z "$(git diff --name-only -- apexd/)" ]; then
        echo "MUT ${tag}: SPLICE DID NOT APPLY — tree is clean, refusing to run"
        return
    fi
    echo "MUT ${tag}: mutated $(git diff --name-only -- apexd/ | tr '\n' ' ')"
    (cd apexd && nice -n 15 cargo build --locked --bin apex-agentd --bin apex) \
        >"/var/tmp/mut-${tag}-build.log" 2>&1
    if [ $? -ne 0 ]; then
        echo "MUT ${tag}: BUILD FAILED"
        tail -20 "/var/tmp/mut-${tag}-build.log"
        return
    fi
    timeout 900 ./tests/test-agent-inject.sh >"/var/tmp/mut-${tag}.log" 2>&1
    echo "MUT ${tag}: $(tail -1 /var/tmp/mut-${tag}.log)"
    grep '^FAIL' "/var/tmp/mut-${tag}.log" | sed 's/^/     /'
}

restore() { git checkout -- apexd/ ; }

case "${1:-}" in
1)  # every live master, not the addressed one
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="""    if let Err(e) = pty::write_all(s.master, &keys) {"""
new="""    let all = daemon.registry.lock().expect("registry lock").list();
    for other in all {
        // try_lock, not lock: `s` above still holds the addressed session's
        // mutex, and a std Mutex re-locked on the same thread DEADLOCKS rather
        // than erroring. The addressed session is skipped here and written
        // below, so every live master still receives the keystrokes.
        if let Ok(o) = other.try_lock() {
            if o.info.is_live() && o.master >= 0 {
                let _ = pty::write_all(o.master, &keys);
            }
        }
    }
    if let Err(e) = pty::write_all(s.master, &keys) {"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 1; restore ;;
2)  # submit the line
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="    out.push(b' ');\n    out\n}"
new="    out.push(b'\\n');\n    out\n}"
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 2; restore ;;
3)  # no refusal for a managed session
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="    if let Some(session) = who.session {"
new="    if let Some(session) = None::<u32>.or(who.session).filter(|_| false) {"
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 3; restore ;;
4)  # no name reduction
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="""    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii() && SAFE.contains(ch) {
            out.push(ch);
        } else {
            out.push('_');
        }
    }"""
new="""    let mut out = raw.to_string();"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 4; restore ;;
5)  # always bracketed
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="    let bracketed = s.scanner.bracketed_paste();"
new="    let bracketed = true;"
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 5; restore ;;
6)  # the copy is never made outside the sandbox's reach: write it beside the source instead
    python3 - <<'PY'
import io
p='apexd/apex-agentd/src/inject.rs'
s=io.open(p,encoding='utf-8').read()
old="    let scratch = scratch_for(&s, id);"
new="    let scratch = std::path::PathBuf::from(\"/tmp/apex-inject-mutant\");"
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 6; restore; rm -rf /tmp/apex-inject-mutant ;;
7)  # the daemon ignores the scratch root override
    python3 - <<'PY'
import io
p='apexd/apex-agent-core/src/paths.rs'
s=io.open(p,encoding='utf-8').read()
old="""        if path.is_absolute() {
            return path;
        }"""
new="""        if path.is_absolute() && false {
            return path;
        }"""
assert s.count(old)==1
io.open(p,'w',encoding='utf-8').write(s.replace(old,new))
PY
    run 7; restore ;;
*)  echo "usage: .mutate.sh 1|2|3|4|5|6|7"; exit 2 ;;
esac
