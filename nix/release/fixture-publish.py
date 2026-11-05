"""TEST ONLY: a minimal publisher for the update-client VM test (nix/tests/update-client.nix).

It produces exactly the objects docs/nixos/updates-and-release.md says the real release job publishes, in the same
order (§3.3: store paths in the cache first, then the manifest, signed, then the channel document that names it),
but signs them with the throwaway keys of nix/release/fixture-keys.nix instead of Sigstore. It is not the release
job; it exists so the client is tested against objects it did not write itself.

  rime-fixture-publish cache   --sign-with cache|wrong-cache TOPLEVEL...
  rime-fixture-publish release --release ID --toplevel P [--previous ID] [--min-from ID] [--identity good|wrong]
                               [--cache-key cache|wrong-cache] [--tamper] [--unsigned]   -> prints the manifest digest
  rime-fixture-publish channel --serial N --entry CHANNEL=DIGEST=RELEASE [--identity good|wrong] [--unsigned]
                               [--expires-in S]                                          -> prints the document digest

Environment: FIXTURE_KEYS (the keys derivation), FIXTURE_REPO (registry/repository), FIXTURE_CACHE (local
file:// cache directory), FIXTURE_BUCKET (the S3 alias/bucket the cache directory is mirrored to with rustfs-cli;
the test serves it from RustFS, an S3 store like R2, since nixpkgs removed MinIO in 2026-10).
"""

import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time

KEYS = os.environ["FIXTURE_KEYS"]
REPO = os.environ["FIXTURE_REPO"]
CACHE = os.environ["FIXTURE_CACHE"]
BUCKET = os.environ["FIXTURE_BUCKET"]

RELEASE_MT = "application/vnd.rimeos.nix-release.v1+json"
CHANNELS_MT = "application/vnd.rimeos.nix-channels.v1+json"
SIMPLE_SIGNING_MT = "application/vnd.dev.cosign.simplesigning.v1+json"
EMPTY = b"{}"
NIX = ["nix", "--extra-experimental-features", "nix-command"]


def run(cmd, **kw):
    print("+", " ".join(cmd), file=sys.stderr)
    return subprocess.run(cmd, check=True, **kw)


def sha256(b):
    return "sha256:" + hashlib.sha256(b).hexdigest()


def push(blob, media_type, tag, annotations=None, layer_annotations=None):
    """Push one artifact (empty config, one layer) to REPO:tag; return the manifest digest the registry holds."""
    with tempfile.TemporaryDirectory() as d:
        manifest = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": media_type,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": sha256(EMPTY), "size": len(EMPTY)},
            "layers": [{"mediaType": media_type, "digest": sha256(blob), "size": len(blob)}],
        }
        if layer_annotations:
            manifest["layers"][0]["annotations"] = layer_annotations
        if annotations:
            manifest["annotations"] = annotations
        raw = json.dumps(manifest, separators=(",", ":")).encode()
        for b in (EMPTY, blob):
            with open(os.path.join(d, hashlib.sha256(b).hexdigest()), "wb") as f:
                f.write(b)
        with open(os.path.join(d, "manifest.json"), "wb") as f:
            f.write(raw)
        with open(os.path.join(d, "version"), "w") as f:
            f.write("Directory Transport Version: 1.1\n")
        run(["skopeo", "copy", "--quiet", "--preserve-digests", "--dest-tls-verify=false", f"dir:{d}", f"docker://{REPO}:{tag}"])
    got = subprocess.run(["skopeo", "inspect", "--raw", "--tls-verify=false", f"docker://{REPO}:{tag}"],
                         check=True, capture_output=True).stdout
    if got != raw:
        sys.exit(f"the registry rewrote the manifest of {tag}")
    return sha256(raw)


def sign(digest, identity, payload_digest=None):
    """A cosign-shaped `.sig` artifact for `digest`. `payload_digest` signs a DIFFERENT digest (tampering)."""
    payload = json.dumps({
        "critical": {
            "identity": {"docker-reference": REPO},
            "image": {"docker-manifest-digest": payload_digest or digest},
            "type": "cosign container image signature",
        },
        "optional": None,
    }, separators=(",", ":")).encode()
    with tempfile.NamedTemporaryFile() as p, tempfile.NamedTemporaryFile() as s:
        p.write(payload)
        p.flush()
        run(["openssl", "dgst", "-sha256", "-sign", f"{KEYS}/{identity}.key", "-out", s.name, p.name])
        sig = base64.b64encode(open(s.name, "rb").read()).decode()
    cert = open(f"{KEYS}/{identity}.pem").read()
    tag = digest.replace(":", "-") + ".sig"
    push(payload, SIMPLE_SIGNING_MT, tag, layer_annotations={
        "dev.cosignproject.cosign/signature": sig,
        "dev.sigstore.cosign/certificate": cert,
    })


def cmd_cache(a):
    os.makedirs(CACHE, exist_ok=True)
    run(NIX + ["copy", "--to", f"file://{CACHE}?compression=zstd&secret-key={KEYS}/{a.sign_with}.sec", *a.toplevels])
    run(["rc", "--quiet", "mirror", CACHE, BUCKET])


def boot_of(toplevel, index):
    spec = json.load(open(f"{toplevel}/boot.json"))["org.nixos.bootspec.v1"]

    def sp(f):
        return "/nix/store/" + f.removeprefix("/nix/store/").split("/")[0]

    boot = {"kernel": index[sp(spec["kernel"])], "initrd": index[sp(spec["initrd"])],
            "kernel_params": spec.get("kernelParams", [])}
    mods = os.path.join(toplevel, "kernel-modules")
    if os.path.islink(mods):
        boot["kernel_modules"] = index[sp(os.readlink(mods))]
    return boot


def cmd_release(a):
    info = json.loads(run(NIX + ["path-info", "--json", "--json-format", "1", "--recursive", "--store", f"file://{CACHE}",
                                 a.toplevel], capture_output=True).stdout)
    order = [a.toplevel] + sorted(p for p in info if p != a.toplevel)
    index = {p: i for i, p in enumerate(order)}
    paths = []
    for p in order:
        i = info[p]
        paths.append({"path": p, "nar_hash": i["narHash"], "nar_size": i["narSize"], "file_size": i["downloadSize"],
                      "references": sorted(index[r] for r in i["references"])})
    manifest = {
        "schema": 1, "kind": "rime-nixos-release", "release": a.release, "issued": int(time.time()),
        "previous": a.previous, "system": "x86_64-linux", "store_dir": "/nix/store",
        "source": {"rime_os": "0" * 40, "rime_shell": "0" * 40, "nixpkgs": "0" * 40},
        "cache": {"url_hint": "fixture", "keys": [open(f"{KEYS}/{a.cache_key}.pub").read().strip()]},
        "variants": {"plain": {"toplevel": 0, "boot": boot_of(a.toplevel, index), "components": {}}},
        "catalogue": [], "paths": paths,
        "batch": {"mass_rebuild": False, "nixpkgs_moved": False, "security": []},
    }
    if a.min_from:
        manifest["min_from"] = a.min_from
    blob = json.dumps(manifest, indent=1).encode()
    ann = {"org.rimeos.release.id": a.release, "org.rimeos.product": "nixos"}
    digest = push(blob, RELEASE_MT, f"release-{a.release}", annotations=ann)
    if not a.unsigned:
        sign(digest, a.identity)
    if a.tamper:
        # The signed manifest, changed after signing (its toplevel's narSize), under a new digest carrying a copy
        # of the original signature: the payload still names the original digest.
        manifest["paths"][0]["nar_size"] += 1
        bad = push(json.dumps(manifest, indent=1).encode(), RELEASE_MT, f"release-{a.release}", annotations=ann)
        sign(bad, a.identity, payload_digest=digest)
        digest = bad
    print(digest)


def cmd_channel(a):
    now = int(time.time())
    entries = []
    for e in a.entry:
        channel, digest, release = e.split("=")
        entries.append({"channel": channel, "digest": digest, "percent": 100, "halt": False, "release": release})
    doc = {"schema": 1, "serial": a.serial, "issued": now, "expires": now + a.expires_in, "repository": REPO,
           "channels": entries}
    digest = push(json.dumps(doc, indent=1).encode(), CHANNELS_MT, "nix-channels")
    if not a.unsigned:
        sign(digest, a.identity)
    print(digest)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("cache")
    c.add_argument("--sign-with", default="cache", choices=["cache", "wrong-cache"])
    c.add_argument("toplevels", nargs="+")
    r = sub.add_parser("release")
    r.add_argument("--release", required=True)
    r.add_argument("--toplevel", required=True)
    r.add_argument("--previous")
    r.add_argument("--min-from")
    r.add_argument("--identity", default="good", choices=["good", "wrong"])
    r.add_argument("--cache-key", default="cache", choices=["cache", "wrong-cache"])
    r.add_argument("--tamper", action="store_true")
    r.add_argument("--unsigned", action="store_true")
    ch = sub.add_parser("channel")
    ch.add_argument("--serial", type=int, required=True)
    ch.add_argument("--entry", action="append", required=True)
    ch.add_argument("--identity", default="good", choices=["good", "wrong"])
    ch.add_argument("--unsigned", action="store_true")
    ch.add_argument("--expires-in", type=int, default=14 * 86400)
    a = ap.parse_args()
    {"cache": cmd_cache, "release": cmd_release, "channel": cmd_channel}[a.cmd](a)


if __name__ == "__main__":
    main()
