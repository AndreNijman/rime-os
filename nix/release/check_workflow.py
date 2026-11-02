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
  - pull_request_target, a workflow-level permission grant, and the stand-in's test-only switches (--test-signing,
    --allow-test-variants, RIME_TEST_BEFORE_PUSH);
  - for nix-release.yml, a concurrency other than: group nix-release, never cancelled; or exactly PER_REF_GROUP and
    PER_REF_CANCEL below (main keeps that group and is never cancelled; other refs, which only dry-run, get a group
    of their own and cancel). Anything else could let one push cancel a release running on main;
  - for nix-release.yml, a release that leaves the class 2 catalogue out: no `closure --catalogue-out`, no build of
    .#rime-release-catalogue, or no run of checks.release-plan.
Exit status 1 with one line per finding.
"""

import re
import sys

import yaml

SHA_PIN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")
SECRETS = ("RIME_NIX_CACHE_KEY", "RIME_CACHE_TOKEN", "RIME_R2_ACCESS_KEY_ID", "RIME_R2_SECRET_ACCESS_KEY",
           "RIME_R2_ENDPOINT")
# a test key, a test configuration (rime-vm's test account) and a shell hook from the environment
TEST_ONLY = ("--test-signing", "--allow-test-variants", "RIME_TEST_BEFORE_PUSH")
FORK_GUARD = "github.event.pull_request.head.repo.full_name == github.repository"
PER_REF_GROUP = "${{ github.ref == 'refs/heads/main' && 'nix-release' || format('nix-release-{0}', github.ref) }}"
PER_REF_CANCEL = "${{ github.ref != 'refs/heads/main' }}"


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
    for t in TEST_ONLY:
        if t in text:
            bad.append(f"{t} is the stand-in's; it never appears in a workflow")
    if path.endswith("nix-release.yml"):
        c = wf.get("concurrency") or {}
        fixed = c.get("group") == "nix-release" and c.get("cancel-in-progress") is False
        per_ref = c.get("group") == PER_REF_GROUP and c.get("cancel-in-progress") == PER_REF_CANCEL
        if not (fixed or per_ref):
            bad.append("concurrency must keep main in group nix-release and never cancel it there")
        # The class 2 catalogue ships with the release (§3.1): the closure step names every app build, and the
        # build step runs checks.release-plan, which fails when a buildable catalogue app has no build.
        runs = [st.get("run", "") for job in (wf.get("jobs") or {}).values() for st in (job.get("steps") or [])]
        closure = [r for r in runs if "rime-nix-release\" closure" in r or "rime-nix-release closure" in r]
        if not closure or not all("--catalogue-out" in r for r in closure):
            bad.append("the closure step must pass every catalogue app build (--catalogue-out)")
        if not any(".#rime-release-catalogue" in r for r in runs):
            bad.append("the build step must build .#rime-release-catalogue")
        if not any("checks.x86_64-linux.release-plan" in r for r in runs):
            bad.append("the build step must run checks.x86_64-linux.release-plan")
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
