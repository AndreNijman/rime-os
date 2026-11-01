"""checks.kernel-sign-workflow: .github/workflows/nix-kernel-sign.yml keeps the shape the signing design needs.

Usage: python3 -I kernel-sign-workflow.py <workflow.yml>. Exit 0 = every rule holds; prints one line per rule.
"""
import re
import sys

import yaml

path = sys.argv[1]
text = open(path, encoding="utf-8").read()
wf = yaml.safe_load(text)
failures = []


def rule(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failures.append(what)


# PyYAML reads the bare key `on` as the boolean True.
triggers = wf.get("on", wf.get(True)) or {}
rule(set(triggers) == {"workflow_dispatch", "workflow_call"},
     f"triggers are exactly workflow_dispatch + workflow_call (got {sorted(triggers)})")
rule(wf.get("permissions") == {"contents": "read"}, "workflow token is contents: read only")

jobs = wf.get("jobs") or {}
rule(bool(jobs), "has jobs")
SECRET_RE = re.compile(r"secrets\.(APEX_SB_[A-Z0-9_]+)")
names = set(SECRET_RE.findall(text))
rule(names == {"APEX_SB_KEY_B64", "APEX_SB_CRT_B64"},
     f"the signing secrets are APEX_SB_KEY_B64 + APEX_SB_CRT_B64, names unchanged (got {sorted(names)})")
rule("secrets: inherit" not in text, "no `secrets: inherit`")

for jname, job in jobs.items():
    ro = job.get("runs-on")
    rule(ro == "ubuntu-24.04", f"job {jname}: runs-on is ubuntu-24.04 (got {ro!r})")
    rule("self-hosted" not in str(ro) and "katana" not in str(ro), f"job {jname}: never a self-hosted/katana runner")
    perms = job.get("permissions")
    rule(perms in (None, {"contents": "read"}), f"job {jname}: no extra token permissions (got {perms!r})")
    rule(not SECRET_RE.search(yaml.safe_dump(job.get("env") or {})), f"job {jname}: no signing secret in job env")
    steps = job.get("steps") or []
    uses_secret = False
    for i, st in enumerate(steps):
        sname = st.get("name", f"#{i}")
        if "uses" in st:
            u = st["uses"]
            rule(u.startswith("./") or re.fullmatch(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}", u) is not None,
                 f"job {jname} step {sname!r}: action pinned by commit SHA ({u})")
        if SECRET_RE.search(yaml.safe_dump({k: v for k, v in st.items() if k != "env"})):
            rule(False, f"job {jname} step {sname!r}: signing secret outside the step env")
        if SECRET_RE.search(yaml.safe_dump(st.get("env") or {})):
            uses_secret = True
            run = st.get("run", "")
            rule("sign-detached.sh" in run and "--pinned \"$PINNED_CERT\"" in run,
                 f"job {jname} step {sname!r}: signs through sign-detached.sh against the pinned certificate")
    nix_install = [st.get("run", "") for st in steps if "/install\" --no-daemon" in st.get("run", "")]
    rule(len(nix_install) == 1
         and "kernel.apparmor_restrict_unprivileged_userns=0" in nix_install[0]
         and 'echo "sandbox = true"' in nix_install[0]
         and 'echo "sandbox-fallback = false"' in nix_install[0],
         f"job {jname}: single-user Nix gets a working build sandbox on ubuntu-24.04 (userns relaxed, no silent fallback)")
    if uses_secret:
        rule(bool(job.get("environment")), f"job {jname}: the signing job runs in a protected environment")
        rule((job.get("env") or {}).get("PINNED_CERT") == "nix/pkgs/kernel/signing/rime-mok.pem",
             f"job {jname}: PINNED_CERT is the committed nix/pkgs/kernel/signing/rime-mok.pem")
        rule(any(st.get("if") == "always()" and "shred" in st.get("run", "") for st in steps),
             f"job {jname}: the key is shredded in an always() step")
        rule("AndreNijman/rime-os" in str(job.get("if", "")), f"job {jname}: never runs in a fork")

rule(not failures, f"{path}: {len(failures)} rule(s) broken")
sys.exit(1 if failures else 0)
