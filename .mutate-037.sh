#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Mutation harness for P1-037. Break one claim, run the suite, restore,
#  report the counts. UNTRACKED on purpose — do not commit it.
#
#  Same two rules as the 036 runner, both of which manufactured false
#  survivors on the 035 one before they were fixed:
#    1. refuse to run a suite unless the tree actually differs from the backup;
#    2. restore by COPY from that backup, never by `git checkout`.
#
#  Baseline at f5879df: tests/test-agent-disposable.sh = 43 passed, 0 failed;
#  cargo test -p apex-agentd (disposable unit tests) = all green.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd /var/tmp/apex-work/wt-p1-035 || exit 2

FILES="apexd/apex-agentd/src/disposable.rs
apexd/apex-agentd/src/session.rs
apexd/apex-agentd/src/registry.rs
files/system/libexec/apex-disposable"
BACKUP="/var/tmp/mut037-backup"

save()    { rm -rf "$BACKUP"; mkdir -p "$BACKUP"
            for f in $FILES; do mkdir -p "$BACKUP/$(dirname "$f")"; cp -a "$f" "$BACKUP/$f"; done; }
restore() { for f in $FILES; do cp "$BACKUP/$f" "$f"; done; chmod +x files/system/libexec/apex-disposable; }
changed() { for f in $FILES; do cmp -s "$f" "$BACKUP/$f" || return 0; done; return 1; }

splice() { python3 - "$@"; }

# run <n> <description> [unit]
#   With "unit" it runs the disposable unit tests instead of the shell suite —
#   for a claim only a unit test can see.
run() {
    local n="$1" desc="$2" mode="${3:-suite}"
    if ! changed; then
        echo "MUTATION ${n}: SPLICE DID NOT APPLY — refusing to run (would be a false survivor)"
        restore; return 1
    fi
    echo "=== MUTATION ${n}: ${desc} ==="
    for f in $FILES; do diff -u "$BACKUP/$f" "$f" | sed -n '3,12p' | sed 's/^/    /'; done
    if [ "$mode" = unit ]; then
        cargo test --locked --manifest-path apexd/Cargo.toml -p apex-agentd disposable 2>&1 \
            | grep -E '^test disposable|^test result|^error' | sed 's/^/    /' | tail -20
    else
        timeout 1500 bash tests/test-agent-disposable.sh 2>&1 \
            | grep -E '^FAIL|^disposable:' | sed 's/^/    /'
    fi
    restore
    echo
}

D=apexd/apex-agentd/src/disposable.rs
S=apexd/apex-agentd/src/session.rs

m1() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = 'format!("--name={}", name_suffix_for(id)),'
new = 'format!("--name={}", name_for(id)),'
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 1 "--name gets the FULL name instead of the suffix the engine validates"; }

m2() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = '''    v.push(CHDIR_SCRIPT.to_string());
    // `$0` for the shell. A dash makes some shells read it as a flag.
    v.push("sh".to_string());
    v.push(base);
    v.push(program.to_string());
    v.extend(args.iter().cloned());'''
new = '''    let mut script = format!("cd -- \\"$HOME/in/{base}\\" || exit 1; exec {program}");
    for a in args {
        script.push(' ');
        script.push_str(a);
    }
    v.push(script);'''
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 2 "the adapter argv is INTERPOLATED into the shell script instead of passed"; }

m3() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = "    if disposable && confined {"
new = "    if false && disposable && confined {"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 3 "a confining sandbox and a capsule are allowed together"; }

m4() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/session.rs"); s = p.read_text()
old = "        capsule: capsule.clone(),"
new = "        capsule: None,"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 4 "the session record never says it is in a capsule"; }

m5() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = "    if disposable && worktree {"
new = "    if false && disposable && worktree {"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 5 "--worktree and a capsule are allowed together"; }

m6() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = '        format!("--copy-in={}", workdir.to_string_lossy()),\n'
assert s.count(old) == 1
s = s.replace(old, "")
old2 = '''    v.push(CHDIR_SCRIPT.to_string());
    // `$0` for the shell. A dash makes some shells read it as a flag.
    v.push("sh".to_string());
    v.push(base);'''
new2 = '''    v.push(r#"cd -- "$1" || exit 1; shift; exec "$@""#.to_string());
    v.push("sh".to_string());
    v.push(workdir.to_string_lossy().into_owned());
    let _ = base;'''
assert s.count(old2) == 1
p.write_text(s.replace(old2, new2))
EOF
run 6 "the worktree is REACHED on the host instead of copied in (state is not discarded)"; }

m7() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = '        v.push(format!("--copy-out={dest}"));'
new = '        let _ = dest;'
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 7 "--copy-out is accepted and never passed to the engine"; }

m8() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/disposable.rs"); s = p.read_text()
old = '''    ["APEX_DISPOSABLE_ROOT", "APEX_DISPOSABLE_ENV_ENGINE"]
        .iter()
        .filter_map(|name| lookup(name).map(|v| ((*name).to_string(), v)))
        .collect()'''
new = '''    let _ = lookup;
    Vec::new()'''
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 8 "the engine's root and capsule engine are left to inheritance" unit; }

m9() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("files/system/libexec/apex-disposable"); s = p.read_text()
old = """    trap 'exit 130' INT
    trap 'exit 143' TERM"""
new = """    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap '' HUP"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
chmod +x files/system/libexec/apex-disposable
run 9 "the engine ignores SIGHUP (the whole mechanism for the SIGKILL half)"; }

m10() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/registry.rs"); s = p.read_text()
old = """        let _ = pty::signal_group(session.pgid, libc::SIGHUP);
        let _ = pty::signal_group(session.pgid, libc::SIGTERM);"""
new = """        let _ = pty::signal_group(session.pgid, libc::SIGTERM);"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 10 "registry::terminate sends only SIGTERM, no SIGHUP"; }

m11() { save; splice <<'EOF'
import pathlib
p = pathlib.Path("apexd/apex-agentd/src/registry.rs"); s = p.read_text()
old = """        let _ = pty::signal_group(session.pgid, libc::SIGHUP);
        let _ = pty::signal_group(session.pgid, libc::SIGTERM);"""
new = """        let _ = (session.pgid, libc::SIGHUP, libc::SIGTERM);"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
EOF
run 11 "registry::terminate signals nobody at all"; }

for want in "$@"; do "m${want}"; done
