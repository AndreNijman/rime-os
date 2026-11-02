#!/usr/bin/python3
"""TEST ONLY. A signed release record for an installer ISO, minted the way nix-release.yml publishes one, with
THROWAWAY keys (nix/release/fixture-keys.nix in the VM check, the lab's TEST keys in nix/installer/lab/run.sh).

Writes DIR in the shape build-live-iso.sh stamps into the ISO (nixos-release/) and rime-install-nixos-release
verifies:

  DIR/manifest/   the release manifest, skopeo's dir: layout (one layer, application/vnd.rimeos.nix-release.v1+json)
  DIR/sig/        its cosign `.sig` object (simple signing, signature + certificate annotations)
  DIR/repo DIR/fulcio-root.pem DIR/signers DIR/issuer   what the signature must chain to and name

  mint-test-release.py --system TOPLEVEL --cache URL --key NAME:KEY [--upstream-cache URL --upstream-key NAME:KEY
      --upstream-paths FILE] --sign-key PEM --sign-cert PEM --ca PEM --out DIR [--bad-narhash]

The closure comes from the local store (`nix path-info --json -r`), file sizes from the caches' narinfos (FileSize),
exactly the fields nix-release.yml's manifest step takes. --bad-narhash records a wrong NarHash for the toplevel AND
signs that manifest: a release whose signature is valid and whose cache holds something else (the installer must
refuse it before the disk is touched).
"""

import argparse
import base64
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.realpath(__file__))


def load_lib(path):
    for p in (path, os.path.join(HERE, "..", "release", "rime_nix_release.py"), os.path.join(HERE, "rime_nix_release.py")):
        if p and os.path.isfile(p):
            spec = importlib.util.spec_from_file_location("rime_nix_release", p)
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            return mod
    sys.exit("mint-test-release: no rime_nix_release.py (pass --lib)")


def file_size(R, cache, path):
    h = path[len(R.STORE_DIR) + 1:].split("-", 1)[0]
    with urllib.request.urlopen(f"{cache.rstrip('/')}/{h}.narinfo", timeout=30) as r:
        ni = R.parse_narinfo(r.read().decode())
    return int(ni["FileSize"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lib")
    ap.add_argument("--system", required=True)
    ap.add_argument("--variant", default="plain")
    ap.add_argument("--release", default="2026.11.02")
    ap.add_argument("--cache", required=True)
    ap.add_argument("--key", action="append", required=True)
    ap.add_argument("--upstream-cache")
    ap.add_argument("--upstream-key", action="append", default=[])
    ap.add_argument("--upstream-paths")
    ap.add_argument("--sign-key", required=True)
    ap.add_argument("--sign-cert", required=True)
    ap.add_argument("--ca", required=True)
    ap.add_argument("--repo", default="ghcr.io/andrenijman/rime-os")
    ap.add_argument("--signers", action="append")
    ap.add_argument("--bad-narhash", action="store_true")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    R = load_lib(a.lib)

    top = os.path.realpath(a.system)
    info = R.nix_path_info([top], recursive=True)
    missing = [p for p, i in info.items() if i is None]
    if missing:
        sys.exit(f"mint-test-release: not in the local store: {missing[:3]}")
    upstream_paths = set()
    if a.upstream_paths:
        upstream_paths = {l.strip() for l in open(a.upstream_paths) if l.strip()}
    sizes = {p: file_size(R, a.upstream_cache if p in upstream_paths else a.cache, p) for p in info}
    clo = {"paths": {p: {k: i[k] for k in ("nar_hash", "nar_size", "references")} for p, i in info.items()},
           "variants": {a.variant: {"toplevel": top, "boot": R.read_boot(top)}}}
    plan = {"variants": {a.variant: {"toplevel": top, "components": {}}}, "catalogue": []}
    upstream = None
    if upstream_paths:
        upstream = {"url_hint": a.upstream_cache, "keys": a.upstream_key, "paths": upstream_paths}
    m = R.build_manifest(clo, plan, sizes, release=a.release, previous=None, issued=int(time.time()),
                         source={"rime_os": "0" * 40, "rime_shell": "0" * 40, "nixpkgs": "0" * 40},
                         cache_url_hint=a.cache, cache_keys=a.key, upstream=upstream)
    if a.bad_narhash:
        e = next(e for e in m["paths"] if e["path"] == top)
        raw = bytearray(base64.b64decode(e["nar_hash"][7:]))
        raw[0] ^= 0xFF
        e["nar_hash"] = "sha256-" + base64.b64encode(bytes(raw)).decode()
    errs = R.validate(m)
    if errs:
        sys.exit("mint-test-release: the manifest does not validate: " + "; ".join(errs[:5]))
    os.makedirs(a.out, exist_ok=True)
    digest = R.pack(R.canonical(m), R.MEDIA_RELEASE, os.path.join(a.out, "manifest"),
                    {"org.rimeos.release": m["release"]})
    payload = json.dumps({"critical": {"identity": {"docker-reference": a.repo},
                                       "image": {"docker-manifest-digest": digest},
                                       "type": "cosign container image signature"},
                          "optional": None}, separators=(",", ":")).encode()
    with tempfile.TemporaryDirectory() as w:
        open(os.path.join(w, "p"), "wb").write(payload)
        subprocess.run(["openssl", "dgst", "-sha256", "-sign", a.sign_key, "-out", os.path.join(w, "s"),
                        os.path.join(w, "p")], check=True)
        sig = base64.b64encode(open(os.path.join(w, "s"), "rb").read()).decode()
    R.pack(payload, R.SIG_LAYER, os.path.join(a.out, "sig"), {})
    # pack() puts no layer annotations; the signature and certificate live on the layer, as cosign writes them.
    man = json.load(open(os.path.join(a.out, "sig", "manifest.json")))
    man["layers"][0]["annotations"] = {R.ANN_SIG: sig, R.ANN_CERT: open(a.sign_cert).read()}
    with open(os.path.join(a.out, "sig", "manifest.json"), "wb") as f:
        f.write(R.canonical(man))
    with open(os.path.join(a.out, "repo"), "w") as f:
        f.write(a.repo + "\n")
    with open(os.path.join(a.out, "signers"), "w") as f:
        f.write("\n".join(a.signers or [R.NIX_SIGNER]) + "\n")
    with open(os.path.join(a.out, "issuer"), "w") as f:
        f.write(R.ISSUER + "\n")
    with open(a.ca) as src, open(os.path.join(a.out, "fulcio-root.pem"), "w") as dst:
        dst.write(src.read())
    with open(os.path.join(a.out, "README"), "w") as f:
        f.write("TEST release record (nix/installer/mint-test-release.py): signed by a THROWAWAY key\n")
    print(json.dumps({"release": m["release"], "digest": digest, "paths": len(m["paths"]),
                      "upstream": len(upstream_paths), "bad_narhash": a.bad_narhash}))


if __name__ == "__main__":
    main()
