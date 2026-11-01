#!/usr/bin/env python3
"""The security shape of the Nix release workflows (docs/nixos/updates-and-release.md §15.3, §6.4, §7).

  check_workflow.py WORKFLOW.yml [...]

Refuses, for every workflow given:
  - a `uses:` that is not a local ./ path or pinned by a 40-character commit SHA;
  - a `run:` script containing ${{ inputs.* }} or ${{ github.event.* }} (inputs reach scripts only through env:);
  - a job with `id-token: write` (a signer: the client cannot tell signers apart) that is not on a GitHub-hosted
    runner, not restricted to refs/heads/main, or not in the nix-release environment;
  - a self-hosted job that lacks the fork-PR guard, or names the nix-release environment, RIME_NIX_CACHE_KEY,
    RIME_CACHE_TOKEN or the R2 secrets;
  - any job outside the nix-release environment naming those secrets;
  - pull_request_target, a workflow-level permission grant, a concurrency group other than nix-release (for
    nix-release.yml) or one that cancels in progress, and the stand-in's --test-signing.
Exit status 1 with one line per finding.
"""

import re
import sys

import yaml

SHA_PIN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")
SECRETS = ("RIME_NIX_CACHE_KEY", "RIME_CACHE_TOKEN", "RIME_R2_ACCESS_KEY_ID", "RIME_R2_SECRET_ACCESS_KEY",
           "RIME_R2_ENDPOINT")
FORK_GUARD = "github.event.pull_request.head.repo.full_name == github.repository"


def hosted(runs_on):
    labels = runs_on if isinstance(runs_on, list) else [runs_on]
    labels = [str(x) for x in labels]
    return "self-hosted" not in labels and all(re.match(r"^(ubuntu|windows|macos)-[0-9a-z.]+$", x) for x in labels)


def check(path):
    text = open(path).read()
    wf = yaml.safe_load(text)
    on = wf.get("on", wf.get(True)) or {}
    bad = []
    if "pull_request_target" in (on if isinstance(on, (dict, list)) else [on]):
        bad.append("pull_request_target is never used")
    if wf.get("permissions") not in ({}, None, "read-all") or isinstance(wf.get("permissions"), dict) and wf["permissions"]:
        bad.append("workflow-level permissions must grant nothing (permissions: {})")
    if "--test-signing" in text:
        bad.append("--test-signing is the stand-in's; it never appears in a workflow")
    if path.endswith("nix-release.yml"):
        c = wf.get("concurrency") or {}
        if c.get("group") != "nix-release" or c.get("cancel-in-progress") is not False:
            bad.append("concurrency must be group nix-release, cancel-in-progress false")
    for name, job in (wf.get("jobs") or {}).items():
        where = f"job {name}"
        steps = job.get("steps") or []
        uses = [job["uses"]] if "uses" in job else []
        uses += [s["uses"] for s in steps if "uses" in s]
        for u in uses:
            if not (u.startswith("./") or SHA_PIN.match(u)):
                bad.append(f"{where}: {u} is not pinned by commit SHA")
        for s in steps:
            run = s.get("run", "")
            if re.search(r"\$\{\{\s*(inputs\.|github\.event\.)", run):
                bad.append(f"{where}: a run: script expands ${{{{ inputs/github.event }}}} (use env:)")
        perms = job.get("permissions") or {}
        signer = isinstance(perms, dict) and perms.get("id-token") == "write"
        env = job.get("environment")
        env = env.get("name") if isinstance(env, dict) else env
        cond = str(job.get("if", ""))
        jtext = yaml.safe_dump(job)
        names_secret = any(s in jtext for s in SECRETS)
        runs_on = job.get("runs-on")
        if signer:
            if runs_on is None or not hosted(runs_on):
                bad.append(f"{where}: a signer (id-token: write) must run on a GitHub-hosted runner")
            if "github.ref == 'refs/heads/main'" not in cond:
                bad.append(f"{where}: a signer must be restricted to refs/heads/main in its if:")
            if env != "nix-release":
                bad.append(f"{where}: a signer must run in the nix-release environment")
        if runs_on is not None and not hosted(runs_on):
            if FORK_GUARD not in cond:
                bad.append(f"{where}: a self-hosted job must carry the fork-PR guard")
            if env == "nix-release" or names_secret:
                bad.append(f"{where}: a self-hosted job must never name nix-release or its secrets")
        if names_secret and env != "nix-release":
            bad.append(f"{where}: names a nix-release secret outside the nix-release environment")
    return bad


def main(paths):
    bad = [f"{p}: {b}" for p in paths for b in check(p)]
    for b in bad:
        print(b)
    if not bad:
        print(f"ok: {len(paths)} workflow(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
