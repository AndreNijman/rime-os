#!/usr/bin/env python3
"""Fork-PR and key guard for every GitHub Actions job that runs on katana.

    python3 ci/runner/check-workflows.py [.github/workflows]

A job is SELF-HOSTED when its runs-on names `self-hosted` or any label starting
with `katana` (today's `katana`, the isolated `katana-nix`). For each such job:

  R1  the workflow has no `pull_request_target` trigger (it would hand the base
      repository's secrets and a write token to a fork's code) and no
      `workflow_run` trigger (it runs with base-repo rights after a fork's run);
  R2  the job carries the fork guard, exactly as kernel-build.yml and
      katana-probe.yml do (whitespace-insensitive, may be AND-ed with more):
        github.event_name != 'pull_request' ||
        github.event.pull_request.head.repo.full_name == github.repository
      Under a fork pull request the expression is false, so GitHub never
      schedules the job and it never reaches katana;
  R3  no signing or key material: no `secrets.*` other than GITHUB_TOKEN
      (job, steps or workflow-level env), no `id-token: write` (OIDC = keyless
      signing) in the job's effective permissions, and no cosign, sbsign,
      APEX_SB_*, APEX_KEY*, APEX_KEYSTORE_* or narinfo signing in it.
      Signing stays on GitHub-hosted runners (PLAN.md D8).

With --simulate-fork-pr: evaluate every self-hosted job's `if:` in a fork
pull_request context and fail for each one that could be scheduled.

Exit status = number of violations. Reusable-workflow calls (`uses:` at job
level) are reported as unchecked when they could run on katana.
"""
import os
import re
import sys

import yaml

GUARD = ("github.event_name != 'pull_request' || "
         "github.event.pull_request.head.repo.full_name == github.repository")
FORBIDDEN_TRIGGERS = ("pull_request_target", "workflow_run")
SIGNING = re.compile(r"\b(cosign|sbsign|sbverify --sign|nix(-store)? (store )?sign|"
                     r"APEX_SB_\w+|APEX_KEY\w*|APEX_KEYSTORE_\w*|secret-key-files)\b")
SECRET_REF = re.compile(r"secrets\.([A-Za-z0-9_]+)")


def norm(expr):
    expr = re.sub(r"^\s*\$\{\{\s*|\s*\}\}\s*$", "", str(expr))
    return " ".join(expr.split())


def labels_of(runs_on):
    if runs_on is None:
        return []
    if isinstance(runs_on, str):
        return [runs_on]
    if isinstance(runs_on, list):
        return [str(x) for x in runs_on]
    if isinstance(runs_on, dict):
        out = []
        lab = runs_on.get("labels")
        if isinstance(lab, str):
            out.append(lab)
        elif isinstance(lab, list):
            out += [str(x) for x in lab]
        if runs_on.get("group"):
            out.append("group:" + str(runs_on["group"]))
        return out
    return [str(runs_on)]


def self_hosted(labels):
    for l in labels:
        l = l.strip()
        if l == "self-hosted" or l.startswith("katana") or l.startswith("group:"):
            return True
        if "${{" in l:   # a matrix/expression label could be anything: treat as katana
            return True
    return False


def triggers(doc):
    on = doc.get("on", doc.get(True))
    if isinstance(on, str):
        return {on}
    if isinstance(on, list):
        return set(map(str, on))
    if isinstance(on, dict):
        return set(map(str, on.keys()))
    return set()


def id_token_write(perms):
    if perms == "write-all":
        return True
    return isinstance(perms, dict) and str(perms.get("id-token", "")).strip() == "write"


def check_file(path):
    problems, report = [], []
    with open(path) as f:
        doc = yaml.safe_load(f) or {}
    trig = triggers(doc)
    wf_perms = doc.get("permissions")
    wf_env = yaml.safe_dump(doc.get("env") or {})
    for name, job in (doc.get("jobs") or {}).items():
        where = f"{os.path.basename(path)}:{name}"
        if "uses" in job and "runs-on" not in job:
            report.append(f"note {where}: reusable workflow call, runs-on not visible here")
            continue
        labels = labels_of(job.get("runs-on"))
        if not self_hosted(labels):
            continue
        report.append(f"self-hosted {where}: runs-on {labels}")
        for t in FORBIDDEN_TRIGGERS:
            if t in trig:
                problems.append(f"R1 {where}: workflow is triggered by {t}")
        cond = norm(job.get("if", ""))
        if GUARD not in cond:
            problems.append(f"R2 {where}: missing fork guard `if: {GUARD}`")
        body = yaml.safe_dump(job) + wf_env
        bad_secrets = sorted({s for s in SECRET_REF.findall(body) if s != "GITHUB_TOKEN"})
        if bad_secrets:
            problems.append(f"R3 {where}: uses secrets {', '.join(bad_secrets)} on a self-hosted runner")
        perms = job["permissions"] if "permissions" in job else wf_perms
        if id_token_write(perms):
            problems.append(f"R3 {where}: id-token: write (OIDC signing) on a self-hosted runner")
        hits = sorted({m.group(0) for m in SIGNING.finditer(yaml.safe_dump(job))})
        if hits:
            problems.append(f"R3 {where}: signing/key material {', '.join(repr(h) for h in hits)} on a self-hosted runner")
    return problems, report


# ── fork-PR dry run: evaluate each self-hosted job's `if:` as GitHub would ──
FORK_PR = {
    "github.event_name": "pull_request",
    "github.repository": "AndreNijman/rime-os",
    "github.event.pull_request.head.repo.full_name": "someone-else/rime-os",
    "github.ref": "refs/pull/1/merge",
}
TOKEN = re.compile(r"\s*(\|\||&&|==|!=|\(|\)|!|'(?:[^']|'')*'|[A-Za-z_][A-Za-z0-9_.\-]*|true|false)")


def evaluate(expr, ctx):
    """True/False for an expression made of context refs, string literals,
    == != && || ! and parentheses; None for anything else (function calls...)."""
    expr = norm(expr)
    if not expr:
        return True
    pos, out = 0, []
    while pos < len(expr):
        m = TOKEN.match(expr, pos)
        if not m:
            return None
        tok = m.group(1)
        pos = m.end()
        if tok in ("||", "&&", "==", "!=", "(", ")", "!"):
            out.append({"||": " or ", "&&": " and ", "!": " not "}.get(tok, f" {tok} "))
        elif tok.startswith("'"):
            out.append(repr(tok[1:-1].replace("''", "'")))
        elif tok in ("true", "false"):
            out.append(tok.capitalize())
        elif tok.startswith(("github.", "env.", "inputs.", "vars.", "needs.", "matrix.")):
            out.append(repr(ctx.get(tok, "")))
        else:
            return None
    try:
        return bool(eval("".join(out), {"__builtins__": {}}, {}))
    except Exception:
        return None


def simulate(files, ctx):
    """Exit status = number of self-hosted jobs a fork PR could schedule."""
    leaks = 0
    for p in files:
        with open(p) as f:
            doc = yaml.safe_load(f) or {}
        pr_triggered = bool({"pull_request", "pull_request_target"} & triggers(doc))
        for name, job in (doc.get("jobs") or {}).items():
            if not self_hosted(labels_of(job.get("runs-on"))):
                continue
            if not pr_triggered:
                print(f"skipped {os.path.basename(p)}:{name}: workflow has no pull_request trigger")
                continue
            r = evaluate(job.get("if", ""), ctx)
            where = f"{os.path.basename(p)}:{name}"
            if r is False:
                print(f"skipped {where}: a fork pull request does not schedule it")
            else:
                leaks += 1
                print(f"FAIL {where}: a fork pull request {'WOULD' if r else 'might'} schedule it on katana")
    print(f"fork-PR dry run: {leaks} self-hosted job(s) reachable from a fork")
    return leaks


def main():
    args = sys.argv[1:]
    sim = "--simulate-fork-pr" in args
    args = [a for a in args if a != "--simulate-fork-pr"]
    root = args[0] if args else ".github/workflows"
    files = sorted(os.path.join(root, f) for f in os.listdir(root) if f.endswith((".yml", ".yaml")))
    if sim:
        return min(simulate(files, FORK_PR), 125)
    problems = []
    for p in files:
        pr, rep = check_file(p)
        for line in rep:
            print(line)
        problems += pr
    for p in problems:
        print("FAIL " + p)
    print(f"checked {len(files)} workflow file(s): {len(problems)} violation(s)")
    return min(len(problems), 125)


if __name__ == "__main__":
    sys.exit(main())
