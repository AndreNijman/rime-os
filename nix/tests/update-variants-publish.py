"""TEST ONLY (nix/tests/update-variants.nix): a release that names SEVERAL variants, as the real release job publishes
the four of nix/release/variants.nix (docs/nixos/updates-and-release.md §3.1 `variants`).

Everything but the variants table is nix/release/fixture-publish.py, loaded from its own file: the same cache,
manifest fields, push and signature as checks.update-client uses.

  rime-variants-publish FIXTURE_PUBLISH_PY --release ID --variant NAME=TOPLEVEL [--variant ...]
                        [--swap A,B]      -> prints the manifest digest

`--swap A,B` (the check's mutant) publishes variant A with B's toplevel and B with A's.
"""

import argparse
import importlib.util
import json
import sys
import time

spec = importlib.util.spec_from_file_location("fixture_publish", sys.argv[1])
fp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fp)

ap = argparse.ArgumentParser()
ap.add_argument("--release", required=True)
ap.add_argument("--variant", action="append", required=True)
ap.add_argument("--swap")
a = ap.parse_args(sys.argv[2:])

tops = dict(v.split("=", 1) for v in a.variant)
if "plain" not in tops:
    sys.exit("a release names its base variant, plain")
if a.swap:
    x, y = a.swap.split(",")
    tops[x], tops[y] = tops[y], tops[x]

info = json.loads(fp.run(fp.NIX + ["path-info", "--json", "--json-format", "1", "--recursive", "--store",
                                   f"file://{fp.CACHE}", *sorted(set(tops.values()))], capture_output=True).stdout)
roots = sorted(set(tops.values()))
order = roots + sorted(p for p in info if p not in roots)
index = {p: i for i, p in enumerate(order)}
paths = [{"path": p, "nar_hash": info[p]["narHash"], "nar_size": info[p]["narSize"],
          "file_size": info[p]["downloadSize"], "references": sorted(index[r] for r in info[p]["references"])}
         for p in order]
manifest = {
    "schema": 1, "kind": "rime-nixos-release", "release": a.release, "issued": int(time.time()),
    "previous": None, "system": "x86_64-linux", "store_dir": "/nix/store",
    "source": {"rime_os": "0" * 40, "rime_shell": "0" * 40, "nixpkgs": "0" * 40},
    "cache": {"url_hint": "fixture", "keys": [open(f"{fp.KEYS}/cache.pub").read().strip()]},
    "variants": {n: {"toplevel": index[t], "boot": fp.boot_of(t, index), "components": {}} for n, t in tops.items()},
    "catalogue": [], "paths": paths,
    "batch": {"mass_rebuild": False, "nixpkgs_moved": False, "security": []},
}
blob = json.dumps(manifest, indent=1).encode()
digest = fp.push(blob, fp.RELEASE_MT, f"release-{a.release}",
                 annotations={"org.rimeos.release.id": a.release, "org.rimeos.product": "nixos"})
fp.sign(digest, "good")
print(digest)
