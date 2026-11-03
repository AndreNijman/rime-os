#!/usr/bin/python3
"""rime-install-nixos-release: the NixOS installer's release check (PLAN.md D9; docs/nixos/installer.md).

Run by rime-install-nixos.sh before anything is erased:

  rime-install-nixos-release check --release-dir D --system S --variant V [--install-variant W] --out DIR
      --cache URL --key KEY [--upstream-cache URL --upstream-key KEY] [--threads N] [--deadline S]
  rime-install-nixos-release verify --release-dir D --system S --variant V [--offer W ...] [--key KEY ...]
      (build time: 1 and 2)

1. The release manifest carried on the ISO (D/manifest, skopeo's dir: layout) is verified the way `rime update`
   verifies one (rimed verify.rs, ported in nix/release/rime_nix_release.py and kept honest by
   crosscheck-rime-trust.sh): its cosign `.sig` object (D/sig) over the manifest's digest, the leaf certificate
   chained to the pinned Fulcio root (D/fulcio-root.pem) at its notBefore, its SAN one of D/signers, its OIDC issuer
   D/issuer; then the layer read BY that digest and the manifest validated (§3.2). The verifier is the ISO's own
   copy, never one fetched from a cache.
2. The system this ISO is stamped with must be the release's `V` variant toplevel, and the cache keys the ISO trusts
   must be keys the release names (cache.keys): an ISO stamped for other caches is refused. The system INSTALLED is
   the release's own toplevel of variant W (default V; plain-gaming when the owner chose gaming), taken from the
   verified manifest, so the installed machine runs a variant toplevel of the release it records (`rime trust
   --gate`). `verify --offer W` refuses an ISO whose release has no W system (build-live-iso.sh, per choice).
3. Every path of the toplevel's closure is looked up in the cache its `origin` names (no origin: the Rime cache), in
   parallel and under a deadline: present, with the manifest's NarHash, NarSize and references, and a signature by
   the trusted key's name. The cryptographic signature check is `nix store verify` (the engine, per origin).

Writes DIR/manifest.json (the layer bytes, kept on the installed system), DIR/digest, DIR/rime.paths,
DIR/upstream.paths and DIR/summary.json. Exit 0 ok; 1 refused (stdout `refused: <reason>`); 2 a cache did not answer
(stdout `unreachable: <reason>`). Python standard library and the openssl binary only.
"""

import argparse
import concurrent.futures
import importlib.util
import json
import os
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.realpath(__file__))
LIB_CANDIDATES = (os.environ.get("RIME_NIXOS_RELEASE_LIB", ""), os.path.join(HERE, "rime_nix_release.py"),
                  "/usr/lib/rime-installer/rime_nix_release.py")


def load_lib():
    for p in LIB_CANDIDATES:
        if p and os.path.isfile(p):
            spec = importlib.util.spec_from_file_location("rime_nix_release", p)
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            return mod
    raise SystemExit("refused: this installer carries no release verifier (rime_nix_release.py)")


R = load_lib()


class Unreachable(Exception):
    pass


def read_text(path):
    with open(path) as f:
        return f.read().strip()


def verify_manifest(d):
    """(digest, signer, layer bytes, manifest) of the release object in D, or Refused."""
    for n in ("manifest/manifest.json", "sig/manifest.json", "repo", "fulcio-root.pem", "signers", "issuer"):
        if not os.path.exists(os.path.join(d, n)):
            raise R.Refused(f"the ISO's release record is incomplete ({n} is missing)")
    repo = read_text(os.path.join(d, "repo"))
    signers = [s for s in read_text(os.path.join(d, "signers")).splitlines() if s.strip()]
    issuer = read_text(os.path.join(d, "issuer"))
    raw = open(os.path.join(d, "manifest", "manifest.json"), "rb").read()
    digest = "sha256:" + R.sha256_hex(raw)
    signer = R.verify_sig_dir(os.path.join(d, "sig"), repo, digest, os.path.join(d, "fulcio-root.pem"), signers,
                              issuer)
    man = json.loads(raw)
    lay = (man.get("layers") or [{}])[0]
    if lay.get("mediaType") != R.MEDIA_RELEASE or man.get("artifactType") != R.MEDIA_RELEASE:
        raise R.Refused(f"the signed object is {lay.get('mediaType')}, not a release manifest")
    hexd = str(lay.get("digest", ":")).split(":", 1)[1]
    try:
        data = open(os.path.join(d, "manifest", hexd), "rb").read()
    except (FileNotFoundError, IsADirectoryError):
        raise R.Refused("the release manifest's layer is missing")
    if R.sha256_hex(data) != hexd:
        raise R.Refused("the release manifest does not match its signed digest")
    try:
        m = json.loads(data)
    except json.JSONDecodeError as e:
        raise R.Refused(f"the release manifest is not JSON ({e})")
    errs = R.validate(m)
    if errs:
        raise R.Refused("the release manifest is not valid: " + "; ".join(errs[:3]))
    return digest, signer, data, m


def fetch(url, timeout):
    """The narinfo text, or None when the cache does not hold it; Unreachable when the cache did not answer."""
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.read().decode()
    except urllib.error.HTTPError as e:
        if e.code in (403, 404):
            return None
        raise Unreachable(f"{url}: HTTP {e.code}")
    except urllib.error.URLError as e:
        if isinstance(e.reason, (FileNotFoundError, NotADirectoryError)):
            return None
        raise Unreachable(f"{url}: {e.reason}")
    except FileNotFoundError:
        return None
    except OSError as e:
        raise Unreachable(f"{url}: {e}")


def check_narinfo(path, want, cache, keyname, timeout):
    """None, or the reason this cache's narinfo for PATH is not the manifest's."""
    h = path[len(R.STORE_DIR) + 1:].split("-", 1)[0]
    text = fetch(f"{cache.rstrip('/')}/{h}.narinfo", timeout)
    if text is None:
        return f"{path} is not in {cache}"
    try:
        ni = R.parse_narinfo(text)
    except ValueError as e:
        return f"{cache}'s record of {path} is malformed ({e})"
    why = R.narinfo_binds(ni, path, want)
    if why:
        return f"{cache}'s copy of {path} is not the one the signed release names ({why})"
    if not any(s.split(":", 1)[0] == keyname for s in ni["Sig"]):
        return f"{cache}'s copy of {path} is not signed by {keyname}"
    return None


def cmd_check(a):
    os.makedirs(a.out, exist_ok=True)
    digest, signer, data, m = verify_manifest(a.release_dir)
    v = (m.get("variants") or {}).get(a.variant)
    if v is None:
        raise R.Refused(f"release {m['release']} has no {a.variant} system")
    paths = m["paths"]
    top = paths[v["toplevel"]]["path"]
    if top != a.system:
        raise R.Refused(f"this ISO installs {a.system}, and release {m['release']}'s {a.variant} system is {top}")
    want = a.install_variant or a.variant
    v = (m.get("variants") or {}).get(want)
    if v is None:
        raise R.Refused(f"release {m['release']} has no {want} system")
    top = paths[v["toplevel"]]["path"]
    keys = set(m["cache"]["keys"])
    if a.key not in keys:
        raise R.Refused(f"the ISO trusts the cache key {a.key.split(':', 1)[0]}, and release {m['release']} "
                        f"names other keys ({', '.join(sorted(k.split(':', 1)[0] for k in keys))})")
    refs = {i: e["references"] for i, e in enumerate(paths)}
    clo = sorted(R.closure_of(v["toplevel"], refs))
    rime = [i for i in clo if paths[i].get("origin", "rime") == "rime"]
    upstream = [i for i in clo if paths[i].get("origin") == "upstream"]
    if upstream:
        if not a.upstream_cache:
            raise R.Refused(f"release {m['release']} takes {len(upstream)} store paths from the upstream cache, "
                            "and this ISO names none")
        if a.upstream_key not in keys:
            raise R.Refused(f"the ISO trusts the upstream key {str(a.upstream_key).split(':', 1)[0]}, which release "
                            f"{m['release']} does not name")
    jobs = [(i, a.cache, a.key) for i in rime] + [(i, a.upstream_cache, a.upstream_key) for i in upstream]

    def one(job):
        i, cache, key = job
        e = paths[i]
        want = {"nar_hash": e["nar_hash"], "nar_size": e["nar_size"],
                "references": [paths[r]["path"] for r in e["references"]]}
        return check_narinfo(e["path"], want, cache, key.split(":", 1)[0], a.timeout)

    t0 = time.monotonic()
    ex = concurrent.futures.ThreadPoolExecutor(max_workers=a.threads)
    futs = [ex.submit(one, j) for j in jobs]
    problem = None
    try:
        for f in concurrent.futures.as_completed(futs, timeout=a.deadline):
            why = f.result()
            if why and problem is None:
                problem = why
                break
    except concurrent.futures.TimeoutError:
        ex.shutdown(wait=False, cancel_futures=True)
        raise Unreachable(f"the package cache did not answer for {len(jobs)} paths within {a.deadline} s")
    finally:
        ex.shutdown(wait=False, cancel_futures=True)
    if problem:
        raise R.Refused(problem)
    took = time.monotonic() - t0
    boot = v["boot"]
    boot_bytes = sum(paths[boot[k]]["nar_size"] for k in ("kernel", "initrd"))
    with open(os.path.join(a.out, "manifest.json"), "wb") as f:
        f.write(data)
    with open(os.path.join(a.out, "digest"), "w") as f:
        f.write(digest)
    for name, sel in (("rime.paths", rime), ("upstream.paths", upstream)):
        with open(os.path.join(a.out, name), "w") as f:
            f.write("".join(paths[i]["path"] + "\n" for i in sel))
    summary = {"release": m["release"], "digest": digest, "signer": signer, "variant": want, "toplevel": top,
               "paths": len(clo), "rime": len(rime), "upstream": len(upstream), "narinfo_seconds": round(took, 2),
               "boot_bytes": boot_bytes}
    with open(os.path.join(a.out, "summary.json"), "w") as f:
        json.dump(summary, f, sort_keys=True)
    print(json.dumps(summary, sort_keys=True))
    return 0


def cmd_verify(a):
    """Build time (build-live-iso.sh): the release record alone, no cache lookups."""
    digest, signer, data, m = verify_manifest(a.release_dir)
    v = (m.get("variants") or {}).get(a.variant)
    if v is None:
        raise R.Refused(f"release {m['release']} has no {a.variant} system")
    top = m["paths"][v["toplevel"]]["path"]
    if top != a.system:
        raise R.Refused(f"this ISO installs {a.system}, and release {m['release']}'s {a.variant} system is {top}")
    for k in a.key:
        if k not in m["cache"]["keys"]:
            raise R.Refused(f"the ISO trusts the cache key {k.split(':', 1)[0]}, which release {m['release']} does not name")
    offered = {}
    for w in a.offer:
        o = (m.get("variants") or {}).get(w)
        if o is None:
            raise R.Refused(f"this ISO offers the {w} system, and release {m['release']} has none")
        offered[w] = m["paths"][o["toplevel"]]["path"]
    print(json.dumps({"release": m["release"], "digest": digest, "signer": signer, "toplevel": top, "offered": offered},
                     sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="rime-install-nixos-release")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("check")
    p.add_argument("--release-dir", required=True)
    p.add_argument("--system", required=True)
    p.add_argument("--variant", default="plain")
    p.add_argument("--install-variant", help="the variant to install (default --variant): its toplevel is the release's")
    p.add_argument("--out", required=True)
    p.add_argument("--cache", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--upstream-cache")
    p.add_argument("--upstream-key")
    p.add_argument("--threads", type=int, default=32)
    p.add_argument("--timeout", type=int, default=30, help="seconds per narinfo request")
    p.add_argument("--deadline", type=int, default=300, help="seconds for the whole lookup")
    p = sub.add_parser("verify")
    p.add_argument("--release-dir", required=True)
    p.add_argument("--system", required=True)
    p.add_argument("--variant", default="plain")
    p.add_argument("--offer", action="append", default=[], help="a variant the ISO lets the owner choose")
    p.add_argument("--key", action="append", default=[])
    a = ap.parse_args(argv)
    try:
        return cmd_check(a) if a.cmd == "check" else cmd_verify(a)
    except R.Refused as e:
        print(f"refused: {e}")
        return 1
    except Unreachable as e:
        print(f"unreachable: {e}")
        return 2


if __name__ == "__main__":
    sys.exit(main())
