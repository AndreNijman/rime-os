#!/usr/bin/env python3
"""The evaluation shards of .github/workflows/nix.yml gate the plan, the check legs and the summary.

    python3 nix/ci/check-eval-shards.py .github/workflows/nix.yml

nix.yml evaluates every flake output in a matrix of shards (job `eval`, nix/ci/eval-each.sh --shard K/N), and the
plan job merges them (eval-each.sh --merge, which refuses unless every output was evaluated exactly once and OK).
A broken expression anywhere must still fail the run. This refuses a workflow in which it might not:

  E1  the `eval` job runs eval-each.sh --shard ${{ matrix.shard }}/${{ strategy.job-total }}: N is the matrix's own
      size, so adding or removing a shard can never leave items unassigned; the matrix is shard: [1..N];
  E2  neither the eval job nor its evaluation step has continue-on-error, and the eval step is unconditional;
  E3  the plan job (`plan`) needs `eval`, has no `if:` that could run it after a failed shard (always(), failure(),
      cancelled()), and has an unconditional step that runs eval-each.sh --merge and run-checks.sh plan, with no
      continue-on-error;
  E4  the check legs (`check`) need `plan`;
  E5  the summary (`summary`, "Nix summary") needs eval and plan, runs always(), and fails (rc=1) unless
      EVAL_RESULT (= needs.eval.result) and PLAN_RESULT (= needs.plan.result) are success.

Exit status = number of violations. Run by nix/ci/test-eval-shard.sh (checks.ci), with mutants.
"""
import re
import sys

import yaml


def runs(job):
    return [(s, str(s.get("run", ""))) for s in job.get("steps", []) or []]


def needs(job):
    n = job.get("needs", [])
    return [n] if isinstance(n, str) else list(n or [])


def cmd_lines(run, cmd):
    """Lines that RUN cmd: it is the command word, after any VAR=value assignments (not `true cmd`, not `# cmd`)."""
    pat = re.compile(r"^\s*(?:rc=0;\s*)?(?:[A-Z_][A-Z0-9_]*=\S*\s+)*" + re.escape(cmd) + r"(\s|$)")
    return [ln for ln in run.splitlines() if pat.search(ln)]


def unconditional(step):
    return "if" not in step and not step.get("continue-on-error")


def check(path):
    with open(path) as f:
        wf = yaml.safe_load(f)
    jobs = wf.get("jobs", {}) or {}
    bad = []

    ev = jobs.get("eval")
    if not ev:
        return ["E1: no `eval` job (the evaluation shards)"]
    if ev.get("continue-on-error"):
        bad.append("E2: the eval job has continue-on-error: a broken shard would not fail it")
    shards = (((ev.get("strategy") or {}).get("matrix") or {}).get("shard"))
    if not isinstance(shards, list) or shards != list(range(1, len(shards) + 1)):
        bad.append(f"E1: the eval job's matrix must be shard: [1..N], got {shards!r}")
    shard_steps = [s for s, r in runs(ev) if cmd_lines(r, "nix/ci/eval-each.sh")]
    want = "--shard ${{ matrix.shard }}/${{ strategy.job-total }}"
    if not any(want in r for s, r in runs(ev) if s in shard_steps):
        bad.append(f"E1: the eval job does not run nix/ci/eval-each.sh {want}")
    for s in shard_steps:
        if not unconditional(s):
            bad.append(f"E2: eval step {s.get('name')!r} is conditional or has continue-on-error")

    plan = jobs.get("plan")
    if not plan:
        bad.append("E3: no `plan` job")
    else:
        if "eval" not in needs(plan):
            bad.append("E3: the plan job does not need eval")
        cond = str(plan.get("if", ""))
        if re.search(r"always\(\)|failure\(\)|cancelled\(\)", cond):
            bad.append(f"E3: the plan job's if: could run it after a failed shard: {cond!r}")
        if plan.get("continue-on-error"):
            bad.append("E3: the plan job has continue-on-error")
        for cmd in ("nix/ci/eval-each.sh --merge", "nix/ci/run-checks.sh plan"):
            steps = [s for s, r in runs(plan) if cmd_lines(r, cmd)]
            if not steps:
                bad.append(f"E3: the plan job does not run {cmd}")
            elif not all(unconditional(s) for s in steps):
                bad.append(f"E3: the plan step running {cmd} is conditional or has continue-on-error")

    chk = jobs.get("check")
    if not chk or "plan" not in needs(chk):
        bad.append("E4: the check legs (`check`) do not need plan")

    summ = jobs.get("summary")
    if not summ or summ.get("name") != "Nix summary":
        bad.append("E5: no `summary` job named 'Nix summary'")
    else:
        for j in ("eval", "plan"):
            if j not in needs(summ):
                bad.append(f"E5: the summary does not need {j}")
        if "always()" not in str(summ.get("if", "")):
            bad.append("E5: the summary does not run always()")
        for var, job in (("EVAL_RESULT", "eval"), ("PLAN_RESULT", "plan")):
            steps = [s for s, r in runs(summ)
                     if re.search(r'\[ "\$' + var + r'" = success \] \|\| \{[^}]*\brc=1; \}', r)]
            if not steps:
                bad.append(f"E5: the summary does not fail (rc=1) unless {var} is success")
            elif not any(str((s.get("env") or {}).get(var, "")).replace(" ", "") == "${{needs." + job + ".result}}"
                         for s in steps):
                bad.append(f"E5: {var} is not needs.{job}.result in the step that checks it")
    return bad


def main(paths):
    bad = []
    for p in paths:
        bad += [f"{p}: {b}" for b in check(p)]
    for b in bad:
        print(f"check-eval-shards: {b}")
    if not bad:
        print("check-eval-shards: the eval shards gate the plan, the check legs and the summary")
    return len(bad)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1:]))
