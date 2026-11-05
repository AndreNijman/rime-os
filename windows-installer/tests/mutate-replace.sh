#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-replace.sh — every safety step in src/replace.rs is held by a test.
#
#  Each mutant below removes or weakens one step of replacing an existing
#  partition (ARCHITECTURE.md, "Exclusivity"). The unit tests must FAIL with
#  each one applied; a mutant they survive is a step nothing checks. The
#  mutant's source text must be found exactly, so a mutant that no longer
#  applies is a failure too, not a silent pass.
#
#  Runs in the same fedora container as tests/test-windows-installer.sh.
#  Usage: windows-installer/tests/mutate-replace.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${RIME_WIN_BUILD_IMAGE:-registry.fedoraproject.org/fedora:43}"
command -v podman >/dev/null || { echo "SKIP  podman is absent"; exit 0; }

podman run --rm -v "$HERE":/src:ro,z "$IMAGE" bash -uo pipefail -c '
    if ! command -v cargo >/dev/null; then
        dnf install -y -q --setopt=install_weak_deps=False rust cargo python3 >/dev/null
    fi
    cp -r /src /build && cd /build
    cargo test --offline --locked --quiet replace:: >/tmp/base.log 2>&1 \
        || { echo "FAIL  the unmutated tests do not pass"; tail -20 /tmp/base.log; exit 1; }
    echo "PASS  unmutated: the replace tests pass"
    cp src/replace.rs /tmp/replace.rs.orig
    python3 - <<"PY" > /tmp/mutants.txt
mutants = [
    ("skip the lock: the verdict forgets which volume to lock",
     "[v] if p.type_guid == plan::WINDOWS_BASIC_DATA && v.unrecognised && v.exactly_this_partition => Some(v.clone()),",
     "[v] if p.type_guid == plan::WINDOWS_BASIC_DATA && v.unrecognised && v.exactly_this_partition => { let _ = v; None }"),
    ("skip the lock: take it only after the checks and the letter removal",
     "    let mut held = t.lock(&v.name).map_err(|e| {",
     "    for m in &v.mount_points { let _ = t.remove_mount_point(m); }\n    let _ = t.reprobe();\n    let mut held = t.lock(&v.name).map_err(|e| {"),
    ("skip the re-probe under the lock",
     "    match t.reprobe() {\n        Ok(None) => {}",
     "    match Ok::<Option<&str>, io::Error>(None) {\n        Ok(None) => {}"),
    ("allow a recognised filesystem",
     "!self.filesystem.is_empty() && !self.filesystem.eq_ignore_ascii_case(\"RAW\")",
     "false"),
    ("excuse every volume at write time, not only the locked one",
     "vols.iter().filter(|v| Some(v.name.as_str()) != ours).collect()",
     "{ let _ = ours; vols.iter().filter(|_| false).collect() }"),
]
import json
print(json.dumps(mutants))
PY
    n=$(python3 -c "import json;print(len(json.load(open(\"/tmp/mutants.txt\"))))")
    fail=0
    for i in $(seq 0 $((n-1))); do
        name=$(python3 -c "import json;print(json.load(open(\"/tmp/mutants.txt\"))[$i][0])")
        cp /tmp/replace.rs.orig src/replace.rs
        if ! python3 - "$i" <<"PY"
import json, sys
name, old, new = json.load(open("/tmp/mutants.txt"))[int(sys.argv[1])]
s = open("src/replace.rs").read()
if s.count(old) != 1:
    sys.exit(1)
open("src/replace.rs", "w").write(s.replace(old, new))
PY
        then
            echo "FAIL  mutant no longer applies: $name"; fail=$((fail+1)); continue
        fi
        if cargo test --offline --locked --quiet replace:: >/tmp/m.log 2>&1; then
            echo "FAIL  mutant SURVIVED: $name"; fail=$((fail+1))
        elif grep -q "could not compile" /tmp/m.log; then
            echo "FAIL  mutant does not compile (proves nothing): $name"; grep -m3 "^error" /tmp/m.log; fail=$((fail+1))
        else
            echo "PASS  mutant killed: $name ($(grep -c "FAILED\|panicked" /tmp/m.log) failing lines)"
        fi
    done
    cp /tmp/replace.rs.orig src/replace.rs
    echo "mutate-replace: $n mutants, $fail not killed"
    [ "$fail" -eq 0 ]
'
