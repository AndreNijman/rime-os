#!/usr/bin/env python3
"""The security shape of the Nix release workflows (docs/nixos/updates-and-release.md §15.3, §6.4, §7).

  check_workflow.py WORKFLOW.yml [...]

Refuses, for every workflow given:
  - a `uses:` that is not a local ./ path or pinned by a 40-character commit SHA;
  - a `run:` script containing ${{ inputs.* }} or ${{ github.event.* }} (inputs reach scripts only through env:);
  - a job with `id-token: write` (a signer: the client cannot tell signers apart) that is not on a GitHub-hosted
    runner, not restricted to refs/heads/main, or not in the nix-release environment;
  - a job calling nix-kernel-sign.yml (the kernel's MOK signer) that is not restricted to refs/heads/main: off main
    the release is a dry run, and the feat/nixos signing proof is a dispatch of nix-kernel-sign.yml itself.
    "Restricted" is exact for both rules (main_only): the if: is a conjunction with the term
    github.ref == 'refs/heads/main' itself, never a substring of an || or a negation;
  - a self-hosted job that lacks the fork-PR guard, or names the nix-release environment, RIME_NIX_CACHE_KEY,
    RIME_CACHE_TOKEN or the R2 secrets;
  - any job outside the nix-release environment naming those secrets (RIME_BRIDGE_PAYLOAD_KEY too), or naming them
    without being restricted to refs/heads/main;
  - for nix-release.yml, an oras push (the bridge payload) before rime-bridge-payload-release signed the payload
    against files/system/trust/nixos-bridge-payload.pub, or with no --verify of the pulled-back copy after it;
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
           "RIME_R2_ENDPOINT", "RIME_BRIDGE_PAYLOAD_KEY")
# The bridge payload's release signer, and the trusted key line it must match (docs/nixos/bridge.md).
PAYLOAD_RELEASE = "rime-bridge-payload-release"
PAYLOAD_PUB = "files/system/trust/nixos-bridge-payload.pub"
PUSH_RE = re.compile(r"oras\"?\s+push\b")
# a test key, a test configuration (rime-vm's test account) and a shell hook from the environment
TEST_ONLY = ("--test-signing", "--allow-test-variants", "RIME_TEST_BEFORE_PUSH")
FORK_GUARD = "github.event.pull_request.head.repo.full_name == github.repository"
PER_REF_GROUP = "${{ github.ref == 'refs/heads/main' && 'nix-release' || format('nix-release-{0}', github.ref) }}"
PER_REF_CANCEL = "${{ github.ref != 'refs/heads/main' }}"


MAIN_ONLY = "github.ref == 'refs/heads/main'"


def _top_level(expr, sep):
    """Split an expression on `sep` outside parentheses and quotes."""
    parts, depth, quote, cur, i = [], 0, None, "", 0
    while i < len(expr):
        ch = expr[i]
        if quote:
            cur += ch
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote, cur = ch, cur + ch
        elif ch == "(":
            depth, cur = depth + 1, cur + ch
        elif ch == ")":
            depth, cur = depth - 1, cur + ch
        elif depth == 0 and expr.startswith(sep, i):
            parts.append(cur)
            cur, i = "", i + len(sep)
            continue
        else:
            cur += ch
        i += 1
    parts.append(cur)
    return parts


def _bare(expr):
    """Whitespace collapsed, the ${{ }} wrapper and redundant outer parentheses removed."""
    e = " ".join(str(expr).split())
    m = re.fullmatch(r"\$\{\{(.*)\}\}", e)
    e = m.group(1).strip() if m else e
    while e.startswith("(") and e.endswith(")") and _balanced(e[1:-1]):
        e = e[1:-1].strip()
    return e


def _balanced(expr):
    depth, quote = 0, None
    for ch in expr:
        if quote:
            quote = None if ch == quote else quote
        elif ch in "'\"":
            quote = ch
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth < 0:
                return False
    return depth == 0 and quote is None


def main_only(cond):
    """True only when the if: is a conjunction (no top-level ||) one of whose terms is exactly MAIN_ONLY: then the job
    cannot run on any other ref, whatever the other terms say. A substring would accept `MAIN_ONLY || true`,
    `!(MAIN_ONLY)` or `github.ref == 'refs/heads/main-x'`."""
    e = _bare(cond)
    if not e or not _balanced(e) or len(_top_level(e, "||")) != 1:
        return False
    return any(_is_main_term(_bare(t)) for t in _top_level(e, "&&"))


# The same comparison written without spaces or with the operands swapped (GitHub expressions take single-quoted
# strings only). Decision (wave 4 integration): these are the same restriction, so refusing them was a false refusal;
# the match is still exact, so `refs/heads/main-x` or a negation never passes.
_MAIN_TERM_RE = re.compile(r"github\.ref\s*==\s*'refs/heads/main'|'refs/heads/main'\s*==\s*github\.ref")


def _is_main_term(term):
    return _MAIN_TERM_RE.fullmatch(term) is not None


def hosted(runs_on):
    labels = runs_on if isinstance(runs_on, list) else [runs_on]
    labels = [str(x) for x in labels]
    return "self-hosted" not in labels and all(re.match(r"^(ubuntu|windows|macos)-[0-9a-z.]+$", x) for x in labels)


def payload_order(steps):
    """A job that pushes with oras pushes the bridge payload: only after rime-bridge-payload-release signed it against
    the trusted key line (which also ran the bridge's verify_payload), and it checks the pulled-back copy with --verify
    after the push."""
    runs = [s.get("run", "") for s in steps]
    push = [i for i, r in enumerate(runs) if PUSH_RE.search(r)]
    if not push:
        return []
    bad = []
    signed = [i for i, r in enumerate(runs)
              if any(PAYLOAD_RELEASE in ln and PAYLOAD_PUB in ln and "--verify" not in ln for ln in r.splitlines())]
    if not signed or min(signed) >= push[0]:
        bad.append(f"the payload is pushed before {PAYLOAD_RELEASE} signs it against {PAYLOAD_PUB}")
    after = "\n".join(runs[push[0]:])
    after = after[PUSH_RE.search(after).end():]
    if not re.search(re.escape(PAYLOAD_RELEASE) + r"\"?\s+--verify\s+" + re.escape(PAYLOAD_PUB), after):
        bad.append(f"the pushed payload is not checked with {PAYLOAD_RELEASE} --verify after the push")
    return bad


def userns_first(steps):
    """A hosted job that may build rimed from source (a release toplevel or the payload closure) lifts Ubuntu 24.04's
    AppArmor user-namespace restriction first: rimed's checkPhase runs bubblewrap inside the Nix sandbox and fails
    with "bwrap: setting up uid map: Permission denied" without it (the first main release, 2026-11-05)."""
    runs = [s.get("run", "") for s in steps]
    builds = [i for i, r in enumerate(runs)
              if any("nix build" in ln and "--dry-run" not in ln
                     and (".#rime-release-toplevels-hosted" in ln or ".#rime-bridge-payload-closure" in ln)
                     for ln in r.splitlines())]
    if not builds:
        return []
    lifted = [i for i, r in enumerate(runs) if re.search(r"sysctl -w kernel\.apparmor_restrict_unprivileged_userns=0\b", r)]
    if not lifted or min(lifted) >= builds[0]:
        return ["builds rimed without lifting the user-namespace restriction first "
                "(sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0)"]
    return []


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
        # A real build with an out-link the closure step reads, not only the dry run's mention of it.
        lines = [ln for r in runs for ln in r.splitlines()]
        if not any("nix build" in ln and ".#rime-release-catalogue" in ln and "--dry-run" not in ln
                   and "--out-link" in ln for ln in lines):
            bad.append("the build step must build .#rime-release-catalogue (an out-link, not a dry run)")
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
            if not main_only(cond):
                bad.append(f"{where}: a signer must be restricted to refs/heads/main in its if:")
            if env != "nix-release":
                bad.append(f"{where}: a signer must run in the nix-release environment")
        if any(u.endswith("/nix-kernel-sign.yml") for u in uses) and not main_only(cond):
            bad.append(f"{where}: a caller of nix-kernel-sign.yml must be restricted to refs/heads/main in its if:")
        if runs_on is not None and not hosted(runs_on):
            if FORK_GUARD not in cond:
                bad.append(f"{where}: a self-hosted job must carry the fork-PR guard")
            if env == "nix-release" or names_secret:
                bad.append(f"{where}: a self-hosted job must never name nix-release or its secrets")
        if names_secret and env != "nix-release":
            bad.append(f"{where}: names a nix-release secret outside the nix-release environment")
        if names_secret and not main_only(cond):
            bad.append(f"{where}: a job naming a nix-release secret must be restricted to refs/heads/main in its if:")
        if path.endswith("nix-release.yml"):
            bad += [f"{where}: {b}" for b in payload_order(steps)]
            if runs_on is not None and hosted(runs_on):
                bad += [f"{where}: {b}" for b in userns_first(steps)]
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
