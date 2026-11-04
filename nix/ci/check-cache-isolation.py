#!/usr/bin/env python3
"""A release never builds from a CI cache.

nix.yml keeps two caches of its own in the GitHub Actions cache: the dev cache (the packages job's locally built
paths, an UNSIGNED file:// binary cache every check leg trusts with `?trusted=1`) and the results cache (checks
that passed). Both are for CI speed only. Anything that ships, a NixOS release closure (nix-release.yml), a signed
kernel (nix-kernel-sign.yml) or the bootc image (build-image.yml), must be built from source or substituted from a
SIGNED cache, never from either of them, and must not keep Nix store paths in the Actions cache at all: a cache
entry is written by whatever run saved it last, and a release cannot vouch for that.

Fails (exit 1, one line per finding) when a release workflow
  - has an actions/cache step (any of actions/cache, /restore, /save) whose path names the Nix store or a CI cache,
    or whose key names a CI cache;
  - uses an action that caches the Nix store (magic-nix-cache, cache-nix-action, cachix/cachix-action);
  - mentions, anywhere outside a comment, the dev or results cache or a way to accept unsigned paths
    (`trusted=1`, `require-sigs = false`, `--no-require-sigs`, `--no-check-sigs`);
  - is missing (a rename must move the guard with it).
Other actions/cache steps (build-image.yml's cargo cache) are fine.

usage: check-cache-isolation.py WORKFLOW-DIR [RELEASE-WORKFLOW...]
"""
import os
import re
import sys

import yaml

RELEASES = ["nix-release.yml", "nix-kernel-sign.yml", "build-image.yml"]

# Outside comments, anywhere in the file (a run: script included).
MARKERS = [
    (re.compile(r"rime-dev-cache|rime-dev-pkgs|RIME_DEV_CACHE"), "the CI dev cache"),
    (re.compile(r"rime-check-results|rime-results|RIME_RESULTS\b"), "the CI results cache"),
    (re.compile(r"run-checks\.sh\s+(prebuild|dev-cache-export|export-result|results-merge)"), "a CI cache command"),
    (re.compile(r"trusted=(1|true)"), "a substituter trusted without signatures"),
    (re.compile(r"require-sigs\s*=\s*false|--no-require-sigs|--no-check-sigs"), "accepting unsigned paths"),
]
NIX_CACHE_ACTIONS = re.compile(
    r"^(DeterminateSystems/magic-nix-cache(-action)?|nix-community/cache-nix-action|cachix/cachix-action)(/|@|$)")
NIX_PATH = re.compile(r"(^|/)nix(/|$)|rime-dev|rime-results|rime-check")
CI_KEY = re.compile(r"rime-dev-pkgs|rime-check-results")


def strings(node):
    """Every string value in a parsed YAML document (comments are already gone)."""
    if isinstance(node, str):
        yield node
    elif isinstance(node, dict):
        for k, v in node.items():
            if isinstance(k, str):
                yield k
            yield from strings(v)
    elif isinstance(node, list):
        for v in node:
            yield from strings(v)


def steps(doc):
    for jname, job in ((doc or {}).get("jobs") or {}).items():
        for i, st in enumerate((job or {}).get("steps") or []):
            if isinstance(st, dict):
                yield jname, i, st


def check(path):
    bad = []
    name = os.path.basename(path)
    if not os.path.exists(path):
        return [f"{name}: missing (a release workflow was renamed or removed: update nix/ci/check-cache-isolation.py)"]
    with open(path) as f:
        doc = yaml.safe_load(f)
    for s in strings(doc):
        for rx, what in MARKERS:
            m = rx.search(s)
            if m:
                bad.append(f"{name}: names {what} ({m.group(0)!r})")
    for jname, i, st in steps(doc):
        uses = str(st.get("uses") or "")
        where = f"{name}: job {jname} step {i + 1} ({st.get('name') or uses})"
        if NIX_CACHE_ACTIONS.search(uses):
            bad.append(f"{where}: {uses.split('@')[0]} caches the Nix store")
        if uses.startswith("actions/cache"):
            w = st.get("with") or {}
            p = str(w.get("path") or "")
            for line in p.splitlines() or [p]:
                if NIX_PATH.search(line.strip()):
                    bad.append(f"{where}: actions/cache path {line.strip()!r} holds Nix store paths or a CI cache")
            for k in ("key", "restore-keys"):
                if CI_KEY.search(str(w.get(k) or "")):
                    bad.append(f"{where}: actions/cache {k} names a CI cache")
    return sorted(set(bad))


def main(argv):
    if not argv:
        raise SystemExit(__doc__)
    d, names = argv[0], argv[1:] or RELEASES
    bad = []
    for n in names:
        bad += check(os.path.join(d, n))
    for b in bad:
        print(f"FAIL {b}")
    if bad:
        print(f"check-cache-isolation: {len(bad)} finding(s): a release must never build from a CI cache")
        return 1
    print(f"check-cache-isolation: {len(names)} release workflow(s) use no CI cache and cache no Nix path: {' '.join(names)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
