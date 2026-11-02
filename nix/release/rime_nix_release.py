#!/usr/bin/env python3
"""rime-nix-release: the release side of Rime OS on NixOS (docs/nixos/updates-and-release.md, D6).

One tool, every step of the pipeline, so CI and the local stand-in run the same code:

  closure      build host: closure.json for the variant toplevels (narHash, references, bootspec)
  split        build host: which paths the upstream cache already serves with the same narHash
  export       build host (S3 fallback): Rime's own paths as a `nix-store --export` stream, the artifact
  stage        build host (GitHub backend): Rime's own paths as an unsigned file:// binary cache, the artifact
  fill         publish job: verify + copy upstream paths, import + bind the artifact, copy to the
               cache, sign every narinfo (idempotent: only missing paths are uploaded)
  check-public publish job: read the closure back from the PUBLIC cache URL, anonymously
  manifest     build the canonical release manifest (§3.1)
  validate     §3.2, on any manifest file
  verify       the self-check: verify a pushed object the way rime trust does (verify.rs port)
  channels     write the next channel document (set / refresh), check a live one
  check-public-github  the same for the GitHub backend (Pages narinfos, release-asset NARs, upstream paths)
  publish      the whole publish job in the fixed order of §3.3 and §6.3 (--backend github, the production
               cache: narinfos on GitHub Pages, NARs as release assets; or --backend s3, the fallback)
  consume      a reference consumer: what a client does with a published release

Python standard library only; external programs: nix, nix-store, skopeo, cosign, openssl.
Nothing here holds a key: secrets arrive as file paths and environment variables.
"""

import argparse
import base64
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

MEDIA_RELEASE = "application/vnd.rimeos.nix-release.v1+json"
MEDIA_CHANNELS = "application/vnd.rimeos.nix-channels.v1+json"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
# Not application/vnd.oci.empty.v1+json: `skopeo inspect --format {{.Digest}}`, which is how the client resolves a
# tag (verify::resolve), refuses an artifact with the empty config ("unsupported image-specific operation on
# artifact"). Checked with skopeo 1.24.1; see docs §3.3.
OCI_CONFIG = "application/vnd.oci.image.config.v1+json"
SIG_LAYER = "application/vnd.dev.cosign.simplesigning.v1+json"
ANN_SIG = "dev.cosignproject.cosign/signature"
ANN_CERT = "dev.sigstore.cosign/certificate"
ANN_CHAIN = "dev.sigstore.cosign/chain"

NIX_SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main"
ISSUER = "https://token.actions.githubusercontent.com"
CHANNELS = ("stable", "candidate", "beta", "edge")
CHANNELS_TAG = "nix-channels"
DOC_LIFETIME = 14 * 86400
DOC_MIN_LEFT = 7 * 86400
STORE_DIR = "/nix/store"
NIX32 = "0123456789abcdfghijklmnpqrsvwxyz"
STORE_PATH_RE = re.compile(r"^/nix/store/[" + NIX32 + r"]{32}-[A-Za-z0-9+\-._?=]{1,211}$")
RELEASE_RE = re.compile(r"^([0-9]{4})\.([0-9]{2})\.([0-9]{2})(?:\.([2-9]|[1-9][0-9]+))?$")
SHA40_RE = re.compile(r"^[0-9a-f]{40}$")
KEY_RE = re.compile(r"^[A-Za-z0-9._-]+:[A-Za-z0-9+/]+={0,2}$")

# Every binary-cache read: no narinfo answer is remembered between calls. Nix keeps positive lookups for 30 days by
# default, which would let a check read a narinfo that was deleted or rewritten after the first lookup.
FRESH = ["--option", "narinfo-cache-positive-ttl", "0", "--option", "narinfo-cache-negative-ttl", "0"]
NIX = ["nix", "--extra-experimental-features", "nix-command"]


class Refused(Exception):
    """A release step that must stop the release. The message is what the log says."""


def log(msg):
    print(f"rime-nix-release: {msg}", file=sys.stderr, flush=True)


# ── small pure helpers ─────────────────────────────────────────────────────────────────────────────────────────────

def canonical(obj):
    """§3.1: UTF-8, keys sorted, no insignificant whitespace."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def nix32_decode(s):
    """Nix's base-32 (the `sha256:<52 chars>` form) to bytes; the inverse of printHash32."""
    n_bytes = len(s) * 5 // 8
    out = bytearray(n_bytes)
    for n in range(len(s)):
        c = s[len(s) - n - 1]
        d = NIX32.find(c)
        if d < 0:
            raise ValueError(f"not a nix32 character: {c!r}")
        b = n * 5
        i, j = divmod(b, 8)
        out[i] |= (d << j) & 0xFF
        carry = d >> (8 - j)
        if i + 1 < n_bytes:
            out[i + 1] |= carry
        elif carry:
            raise ValueError("nix32 string has bits past its length")
    return bytes(out)


def sri(h):
    """Normalise every narHash form Nix prints to SRI `sha256-<base64>`; ValueError if it is not a sha256."""
    if isinstance(h, dict):  # Nix's structured hash form
        if h.get("algorithm") != "sha256":
            raise ValueError(f"not a sha256 hash: {h!r}")
        fmt, val = h.get("format"), h.get("hash", "")
        if fmt == "base64":
            raw = base64.b64decode(val, validate=True)
        elif fmt == "nix32":
            raw = nix32_decode(val)
        elif fmt == "base16":
            raw = bytes.fromhex(val)
        else:
            raise ValueError(f"unknown hash format {fmt!r}")
    elif isinstance(h, str) and h.startswith("sha256-"):
        raw = base64.b64decode(h[7:], validate=True)
    elif isinstance(h, str) and h.startswith("sha256:"):
        v = h[7:]
        raw = bytes.fromhex(v) if len(v) == 64 else nix32_decode(v)
    else:
        raise ValueError(f"not a sha256 hash: {h!r}")
    if len(raw) != 32:
        raise ValueError(f"hash is {len(raw)} bytes, not 32: {h!r}")
    return "sha256-" + base64.b64encode(raw).decode()


def parse_release(rid):
    m = RELEASE_RE.match(rid or "")
    if not m:
        raise ValueError(f"not a release ID: {rid!r}")
    y, mo, d, n = m.groups()
    return (int(y), int(mo), int(d), int(n) if n else 1)


def valid_store_path(p):
    return isinstance(p, str) and bool(STORE_PATH_RE.match(p)) and not p.split("-", 1)[1].startswith(".")


def store_path_of(p):
    """The store path a file inside the store belongs to (/nix/store/x-linux/bzImage -> /nix/store/x-linux)."""
    parts = p.split("/")
    if len(parts) < 4 or "/".join(parts[:3]) != STORE_DIR:
        raise ValueError(f"not inside {STORE_DIR}: {p}")
    return "/".join(parts[:4])


def valid_key(k):
    if not isinstance(k, str) or not KEY_RE.match(k):
        return False
    try:
        return len(base64.b64decode(k.split(":", 1)[1], validate=True)) == 32
    except ValueError:
        return False


def repo_of(ref):
    """verify.rs repo_of: drop @digest, and a :tag only when the colon is after the last slash (a port stays)."""
    head = ref.split("@", 1)[0]
    if ":" in head and head.rfind(":") > head.rfind("/"):
        return head.rsplit(":", 1)[0]
    return head


def topo_order(paths, refs):
    """Dependencies first (nix-store --import and binary caches need a path's references before the path)."""
    want = set(paths)
    order, state = [], {}
    for start in sorted(want):
        stack = [(start, iter(sorted(r for r in refs.get(start, []) if r in want and r != start)))]
        if state.get(start):
            continue
        state[start] = 1
        while stack:
            node, it = stack[-1]
            nxt = next(it, None)
            if nxt is None:
                stack.pop()
                state[node] = 2
                order.append(node)
            elif not state.get(nxt):
                state[nxt] = 1
                stack.append((nxt, iter(sorted(r for r in refs.get(nxt, []) if r in want and r != nxt))))
            elif state[nxt] == 1:
                raise Refused(f"reference cycle through {nxt}")
    return order


def closure_of(start, refs):
    seen, todo = set(), [start]
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.add(p)
        todo.extend(refs.get(p, []))
    return seen


# ── running programs ───────────────────────────────────────────────────────────────────────────────────────────────

def run(cmd, *, data=None, check=True, env=None, stdout=subprocess.PIPE, quiet=False):
    if not quiet:
        log("$ " + " ".join(cmd if len(cmd) < 14 else cmd[:12] + [f"... ({len(cmd) - 12} more)"]))
    p = subprocess.run(cmd, input=data, stdout=stdout, stderr=subprocess.PIPE, env=env)
    if check and p.returncode != 0:
        what = " ".join(c for c in cmd[:8] if not c.startswith("-") and c != "nix-command" and "=" not in c)[:120]
        raise Refused(f"{what} failed ({p.returncode}): "
                      f"{p.stderr.decode(errors='replace').strip()[-2000:]}")
    return p


def nix_path_info(paths, store=None, recursive=False):
    """{path: info|None}; info = nar_hash (SRI), nar_size, file_size (binary caches), references, signatures, url."""
    if not paths:
        return {}
    out = {}
    paths = list(paths)
    for i in range(0, len(paths), 500):
        raw = _path_info_chunk(paths[i:i + 500], store, recursive, out)
        if isinstance(raw, list):  # Nix < 2.19
            raw = {e["path"]: (None if e.get("valid") is False else e) for e in raw}
        for path, info in raw.items():
            if info is None:
                out[path] = None
                continue
            refs = info.get("references", [])
            out[path] = {
                "nar_hash": sri(info["narHash"]),
                "nar_size": int(info["narSize"]),
                "file_size": int(info["downloadSize"]) if info.get("downloadSize") is not None else None,
                "references": sorted(r if r.startswith("/") else f"{STORE_DIR}/{r}" for r in refs),
                "signatures": info.get("signatures", []),
                "url": info.get("url"),
            }
        for path in paths[i:i + 500]:
            out.setdefault(path, None)
    return out


def _path_info_chunk(chunk, store, recursive, out):
    """One `nix path-info --json` call. Nix 2.35 does not answer `null` for a path the store lacks: it fails with
    "don't know how to build these paths:" and lists them. Those are recorded as missing (None in `out`) and the
    call is repeated without them; any other failure, or a listed path that was not asked about, is a refusal."""
    chunk = list(chunk)
    while chunk:
        cmd = NIX + ["path-info", "--json"] + FRESH + (["--store", store] if store else []) \
            + (["-r"] if recursive else []) + chunk
        p = run(cmd, check=False, quiet=True)
        if p.returncode == 0:
            try:
                raw = json.loads(p.stdout or b"null")
            except json.JSONDecodeError:
                raw = None
            if raw is not None:
                return raw
        err = p.stderr.decode(errors="replace")
        absent = missing_from_error(err)
        if recursive or not absent or not absent <= set(chunk):
            raise Refused(f"nix path-info failed ({p.returncode}): {err[-1500:]}")
        for path in absent:
            out[path] = None
        chunk = [c for c in chunk if c not in absent]
    return {}


def missing_from_error(err):
    """The store paths Nix lists after "don't know how to build these paths:" (indented, one per line)."""
    lines = err.splitlines()
    for n, line in enumerate(lines):
        if "don't know how to build these paths" in line:
            found = set()
            for nxt in lines[n + 1:]:
                m = re.match(r"^\s+(/nix/store/[0-9a-z]{32}-\S+)\s*$", nxt)
                if not m:
                    break
                found.add(m.group(1))
            return found
    return set()


# The zstd level of the Rime cache's NARs (`stage`); delta.md §8 measured it against level 3.
STAGE_LEVEL = 19


def store_url(base, **params):
    sep = "&" if "?" in base else "?"
    extra = "&".join(f"{k.replace('_', '-')}={v}" for k, v in params.items())
    return base + (sep + extra if extra else "")


# ── build host: closure, split, export ────────────────────────────────────────────────────────────────────────────

def read_boot(toplevel):
    """The variant's boot block from its bootspec (boot.json, org.nixos.bootspec.v1) and kernel-modules link."""
    with open(os.path.join(toplevel, "boot.json")) as f:
        spec = json.load(f)["org.nixos.bootspec.v1"]
    return {
        "kernel": store_path_of(spec["kernel"]),
        "initrd": store_path_of(spec["initrd"]),
        "kernel_modules": store_path_of(os.path.realpath(os.path.join(toplevel, "kernel-modules"))),
        "kernel_params": list(spec.get("kernelParams", [])),
    }


def cmd_closure(a):
    variants = {}
    for v in a.variant:
        name, top = v.split("=", 1)
        top = os.path.realpath(top)
        variants[name] = {"toplevel": top, "boot": read_boot(top)}
    roots = [v["toplevel"] for v in variants.values()] + [os.path.realpath(p) for p in a.catalogue_out]
    info = nix_path_info(roots, recursive=True)
    missing = [p for p, i in info.items() if i is None]
    if missing:
        raise Refused(f"not valid in the local store: {missing[:5]}")
    doc = {
        "schema": 1,
        "variants": variants,
        "catalogue_outs": sorted(set(roots[len(variants):])),
        "paths": {p: {k: i[k] for k in ("nar_hash", "nar_size", "references")} for p, i in sorted(info.items())},
    }
    for name, v in variants.items():
        clo = closure_of(v["toplevel"], {p: i["references"] for p, i in doc["paths"].items()})
        for k in ("kernel", "initrd", "kernel_modules"):
            if v["boot"][k] not in clo:
                raise Refused(f"{name}: boot.{k} {v['boot'][k]} is not in the toplevel's closure")
    write_json(a.out, doc)
    log(f"closure: {len(doc['paths'])} paths, variants {sorted(variants)}")


def cmd_split(a):
    clo = load_json(a.closure)
    paths = sorted(clo["paths"])
    upstream = []
    if a.upstream:
        up = nix_path_info(paths, store=a.upstream)
        for p in paths:
            i = up.get(p)
            # Same path, different narHash upstream: this build host built it itself, non-reproducibly. It is
            # treated as a Rime path, so closure.json, the cache and the manifest agree (§6.3).
            if i is not None and i["nar_hash"] == clo["paths"][p]["nar_hash"]:
                upstream.append(p)
    up_set = set(upstream)
    rime = [p for p in paths if p not in up_set]
    # An upstream path's references are upstream paths; anything else means the upstream answer is inconsistent.
    for p in upstream:
        for r in clo["paths"][p]["references"]:
            if r not in up_set:
                raise Refused(f"upstream path {p} references {r}, which upstream does not serve")
    write_json(a.out, {"upstream": upstream, "rime": rime})
    log(f"split: {len(upstream)} upstream, {len(rime)} Rime-built")


def cmd_export(a):
    clo = load_json(a.closure)
    split = load_json(a.split)
    os.makedirs(a.out, exist_ok=True)
    refs = {p: i["references"] for p, i in clo["paths"].items()}
    order = topo_order(split["rime"], refs)
    with open(os.path.join(a.out, "rime.export"), "wb") as f:
        if order:
            run(["nix-store", "--export"] + order, stdout=f)
    shutil.copy(a.closure, os.path.join(a.out, "closure.json"))
    shutil.copy(a.split, os.path.join(a.out, "split.json"))
    log(f"export: {len(order)} Rime paths to {a.out}")


# ── publish job: fill, check-public ────────────────────────────────────────────────────────────────────────────────

def load_artifact(d):
    clo = load_json(os.path.join(d, "closure.json"))
    split = load_json(os.path.join(d, "split.json"))
    allp = set(clo["paths"])
    if set(split["upstream"]) | set(split["rime"]) != allp or set(split["upstream"]) & set(split["rime"]):
        raise Refused("split.json does not partition closure.json")
    for p, i in clo["paths"].items():
        if not valid_store_path(p):
            raise Refused(f"closure.json: not a store path: {p!r}")
        sri(i["nar_hash"])
        for r in i["references"]:
            if r not in allp:
                raise Refused(f"closure.json is not closed: {p} -> {r}")
    return clo, split


def fill(artifact, cache, secret_key_file, upstream, upstream_keys, report=None):
    """§6.3 step 2. Returns {"uploaded": n, "already": n}."""
    clo, split = load_artifact(artifact)
    up_paths, rime_paths = split["upstream"], split["rime"]
    trusted = " ".join(upstream_keys)

    if up_paths:
        if not upstream or not upstream_keys:
            raise Refused("the closure has upstream paths but no upstream cache or key was given")
        # Same narHash as the build host saw, and a signature by a key we name. Then the copy itself enforces the
        # key again (require-sigs), so this is not a check-then-use gap.
        up = nix_path_info(up_paths, store=upstream)
        for p in up_paths:
            if up.get(p) is None or up[p]["nar_hash"] != clo["paths"][p]["nar_hash"]:
                raise Refused(f"upstream no longer serves {p} with the narHash closure.json names")
        run(NIX + ["store", "verify", "--store", upstream, "--no-contents", "--sigs-needed", "1",
                   "--trusted-public-keys", trusted] + FRESH + up_paths)
        run(NIX + ["copy", "--from", upstream, "--option", "require-sigs", "true",
                   "--option", "trusted-public-keys", trusted] + FRESH + up_paths)

    if rime_paths:
        with open(os.path.join(artifact, "rime.export"), "rb") as f:
            run(["nix-store", "--import"], data=f.read())

    # Bind what is now in the local store to closure.json: an artifact changed in transit is refused here.
    local = nix_path_info(sorted(clo["paths"]))
    for p, want in clo["paths"].items():
        got = local.get(p)
        if got is None:
            raise Refused(f"{p} is not in the local store after import")
        if got["nar_hash"] != want["nar_hash"] or got["nar_size"] != want["nar_size"] \
                or sorted(got["references"]) != sorted(want["references"]):
            raise Refused(f"{p}: the imported path is not the one closure.json names "
                          f"(narHash {got['nar_hash']} != {want['nar_hash']})")

    ensure_cache_info(cache)
    there = nix_path_info(sorted(clo["paths"]), store=cache)
    missing = [p for p, i in there.items() if i is None]
    roots = [v["toplevel"] for v in clo["variants"].values()] + clo.get("catalogue_outs", [])
    log(f"fill: {len(missing)} of {len(clo['paths'])} paths missing from the cache")
    if missing:
        # Recursive from the roots: Nix skips every path the destination already has, and adds references before
        # the paths that need them (a binary cache refuses a path whose references it does not hold).
        run(NIX + ["copy", "--to", store_url(cache, secret_key=secret_key_file)] + FRESH + roots)
    # Every path, including ones uploaded by an earlier run or that came from upstream, carries the Rime signature.
    # `nix store sign` appends: upstream signatures stay, for audit. It rewrites narinfos in place on S3 (verified).
    p = run(NIX + ["store", "sign", "--store", cache, "--key-file", secret_key_file] + FRESH + sorted(clo["paths"]))
    log("sign: " + p.stderr.decode(errors="replace").strip().splitlines()[-1:][0] if p.stderr.strip() else "sign: done")
    result = {"uploaded": len(missing), "already": len(clo["paths"]) - len(missing)}
    if report:
        write_json(report, result)
    return result


CACHE_INFO = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n"


def ensure_cache_info(cache):
    """§6.1's nix-cache-info. Nix writes one only when the bucket has none, and then with StoreDir alone (measured in
    release-standin: `want-mass-query`/`priority` store parameters do not reach the file), so the fill writes it.
    s3:// through the S3 API (SigV4, the same AWS_* credentials Nix uses); file:// directly."""
    if cache.startswith("file://"):
        path = os.path.join(cache[len("file://"):].split("?", 1)[0], "nix-cache-info")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        cur = open(path).read() if os.path.exists(path) else None
        if cur != CACHE_INFO:
            open(path, "w").write(CACHE_INFO)
        return
    if not cache.startswith("s3://"):
        raise Refused(f"cannot write nix-cache-info to {cache}")
    url, headers = s3_request("PUT", cache, "nix-cache-info", CACHE_INFO.encode(), "text/x-nix-cache-info")
    import urllib.request
    import urllib.error
    req = urllib.request.Request(url, data=CACHE_INFO.encode(), method="PUT", headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            if r.status not in (200, 201, 204):
                raise Refused(f"writing nix-cache-info answered {r.status}")
    except urllib.error.HTTPError as e:
        raise Refused(f"writing nix-cache-info answered {e.code}: {e.read()[:300]!r}")


def sigv4_key(secret, date, region, service):
    import hmac
    k = ("AWS4" + secret).encode()
    for part in (date, region, service, "aws4_request"):
        k = hmac.new(k, part.encode(), hashlib.sha256).digest()
    return k


def s3_request(method, cache, key, body, content_type, now=None, env=None):
    """URL and headers for one path-style S3 request to the bucket a Nix s3:// store URL names, signed with SigV4."""
    import hmac
    import urllib.parse
    env = os.environ if env is None else env
    u = urllib.parse.urlparse(cache)
    params = dict(urllib.parse.parse_qsl(u.query))
    bucket = u.netloc
    endpoint = params.get("endpoint") or "s3.amazonaws.com"
    if "://" not in endpoint:
        endpoint = "https://" + endpoint
    e = urllib.parse.urlparse(endpoint)
    region = params.get("region") or env.get("AWS_DEFAULT_REGION") or "us-east-1"
    access, secret = env.get("AWS_ACCESS_KEY_ID"), env.get("AWS_SECRET_ACCESS_KEY")
    if not access or not secret:
        raise Refused("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not set for the cache")
    now = now or datetime.datetime.now(datetime.timezone.utc)
    amz, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
    path = "/" + bucket + "/" + urllib.parse.quote(key)
    payload = hashlib.sha256(body).hexdigest()
    headers = {"host": e.netloc, "x-amz-content-sha256": payload, "x-amz-date": amz}
    signed = ";".join(sorted(headers))
    canon = "\n".join([method, path, "", *(f"{k}:{headers[k]}" for k in sorted(headers)), "", signed, payload])
    scope = f"{day}/{region}/s3/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz, scope, hashlib.sha256(canon.encode()).hexdigest()])
    sig = hmac.new(sigv4_key(secret, day, region, "s3"), to_sign.encode(), hashlib.sha256).hexdigest()
    out = {"x-amz-content-sha256": payload, "x-amz-date": amz, "Content-Type": content_type,
           "Authorization": f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}"}
    return f"{e.scheme}://{e.netloc}{path}", out


def http_get(url):
    import urllib.request
    import urllib.error
    try:
        with urllib.request.urlopen(url, timeout=60) as r:
            return r.read()
    except urllib.error.HTTPError:
        return None


def http_status(url, method="HEAD"):
    import urllib.request
    import urllib.error
    req = urllib.request.Request(url, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def check_public(closure, public, keys, report=None):
    """Read the whole closure back through the public URL with no credentials. Returns {path: file_size}."""
    clo = closure if isinstance(closure, dict) else load_json(closure)
    paths = sorted(clo["paths"])
    pub = nix_path_info(paths, store=public)
    problems = []
    for p in paths:
        i, want = pub.get(p), clo["paths"][p]
        if i is None:
            problems.append(f"{p}: no narinfo at {public}")
        elif i["nar_hash"] != want["nar_hash"] or sorted(i["references"]) != sorted(want["references"]) \
                or i["nar_size"] != want["nar_size"]:
            problems.append(f"{p}: the public narinfo does not match closure.json")
        elif not i.get("file_size"):
            problems.append(f"{p}: the public narinfo has no FileSize")
    if problems:
        raise Refused("public check: " + "; ".join(problems[:10]) + (f" (+{len(problems) - 10} more)" if len(problems) > 10 else ""))
    p = run(NIX + ["store", "verify", "--store", public, "--no-contents", "--sigs-needed", "1",
                   "--trusted-public-keys", " ".join(keys)] + FRESH + paths, check=False)
    if p.returncode != 0:
        raise Refused("public check: narinfo signatures: " + p.stderr.decode(errors="replace").strip()[-1500:])
    # A narinfo whose NAR is gone is what a client would hit as a download failure. HEAD every NAR.
    base = public.split("?", 1)[0].rstrip("/")
    info = http_get(f"{base}/nix-cache-info")
    if info is None or info.decode(errors="replace") != CACHE_INFO:
        raise Refused(f"public check: {base}/nix-cache-info is {info!r}, not {CACHE_INFO!r}")
    gone = []
    for path in paths:
        url = pub[path].get("url")
        if not url:
            gone.append(f"{path}: narinfo names no NAR")
            continue
        code = http_status(f"{base}/{url}")
        if code != 200:
            gone.append(f"{path}: {url} answered {code}")
    if gone:
        raise Refused("public check: " + "; ".join(gone[:10]))
    sizes = {p: pub[p]["file_size"] for p in paths}
    if report:
        write_json(report, sizes)
    log(f"public check: {len(paths)} paths readable and signed at {public}")
    return sizes


# ── the GitHub backend: narinfos on GitHub Pages, NARs as release assets (production, Andre 2026-11-01) ───────────
#
# cache.rimeos.com is GitHub Pages of AndreNijman/rime-cache (main branch root): nix-cache-info and the signed
# .narinfo files. The NARs are release assets of the same repository, one release per Rime release ID, and each
# narinfo's URL: is ABSOLUTE (https://github.com/AndreNijman/rime-cache/releases/download/<tag>/<nar>). Only paths
# cache.nixos.org does not serve are published; clients use both caches.
#
# The narinfo signature does not cover URL: Nix signs ValidPathInfo::fingerprint, which is
# "1;<store path>;<narHash nix32>;<narSize>;<references>" (src/libstore/path-info.cc, Nix 2.35.3), so URL,
# Compression, FileHash and FileSize may be rewritten after signing. Nix's HTTP binary cache resolves URL: with
# parseURLRelative against the cache URI, so an absolute URL is fetched as-is (src/libstore/http-binary-cache-store.cc
# HttpBinaryCacheStore::makeRequest).

GH_CACHE_INFO = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 50\n"
GH_MAX_ASSETS = 1000              # assets per release (GitHub's limit)
GH_MAX_ASSET = 2 * 1024 ** 3 - 1  # bytes per asset (GitHub: under 2 GiB)
GH_PART_RE = re.compile(r"^(.*?)(?:-part([2-9]|[1-9][0-9]+))?$")
GH_BOT = ("github-actions[bot]", "41898282+github-actions[bot]@users.noreply.github.com")


def parse_narinfo(text):
    """{field: value} of a .narinfo; Sig may repeat (list). ValueError on a malformed line."""
    out = {"Sig": []}
    for line in text.splitlines():
        if not line.strip():
            continue
        k, sep, v = line.partition(": ")
        if not sep:
            raise ValueError(f"malformed narinfo line {line!r}")
        if k == "Sig":
            out["Sig"].append(v)
        else:
            out[k] = v
    return out


def narinfo_with_url(text, url):
    """The same narinfo with URL: replaced (every other byte kept, so the Sig lines stay valid)."""
    lines, n = [], 0
    for line in text.splitlines():
        if line.startswith("URL: "):
            line, n = "URL: " + url, n + 1
        lines.append(line)
    if n != 1:
        raise Refused(f"a narinfo has {n} URL lines")
    return "\n".join(lines) + "\n"


def narinfo_binds(ni, path, want):
    """None if narinfo fields `ni` describe exactly closure.json's entry for `path`, else the reason."""
    if ni.get("StorePath") != path:
        return f"StorePath {ni.get('StorePath')!r} is not {path}"
    try:
        if sri(ni.get("NarHash", "")) != sri(want["nar_hash"]):
            return f"NarHash {ni.get('NarHash')} is not closure.json's {want['nar_hash']}"
    except ValueError as e:
        return str(e)
    if int(ni.get("NarSize", "-1")) != int(want["nar_size"]):
        return f"NarSize {ni.get('NarSize')} is not {want['nar_size']}"
    refs = sorted(f"{STORE_DIR}/{r}" for r in ni.get("References", "").split())
    if refs != sorted(want["references"]):
        return "References differ from closure.json"
    return None


def nar_hash_nix32(h):
    """SRI or sha256:<nix32> to the sha256:<nix32> form a narinfo carries."""
    raw = base64.b64decode(sri(h)[7:])
    out = []
    for n in range(51, -1, -1):
        b = n * 5
        i, j = divmod(b, 8)
        c = raw[i] >> j
        if i + 1 < 32:
            c |= raw[i + 1] << (8 - j)
        out.append(NIX32[c & 0x1F])
    return "sha256:" + "".join(out)


def cmd_stage(a):
    """Build job, GitHub backend: a file:// binary cache holding ONLY the Rime-built paths, zstd, unsigned.

    A binary cache refuses a path whose references it does not hold (BinaryCacheStore::addToStoreCommon), and it
    counts a path as held when its .narinfo exists (isValidPathUncached). So the upstream references get placeholder
    narinfos written from closure.json, the Rime paths are copied --no-recursive, and the placeholders are deleted:
    no upstream path is ever compressed, uploaded or published."""
    clo = load_json(a.closure)
    split = load_json(a.split)
    out = a.out
    cache = os.path.join(out, "cache")
    if os.path.exists(cache):
        raise Refused(f"{cache} exists; stage writes a fresh one")
    os.makedirs(os.path.join(cache, "nar"))
    open(os.path.join(cache, "nix-cache-info"), "w").write(GH_CACHE_INFO)
    rime = split["rime"]
    rime_set = set(rime)
    placeholders = []
    for p in split["upstream"]:
        i = clo["paths"][p]
        f = os.path.join(cache, os.path.basename(p)[:32] + ".narinfo")
        open(f, "w").write(
            f"StorePath: {p}\nURL: nar/rime-placeholder.nar\nCompression: none\n"
            f"NarHash: {nar_hash_nix32(i['nar_hash'])}\nNarSize: {i['nar_size']}\n"
            f"References: {' '.join(os.path.basename(r) for r in i['references'])}\n")
        placeholders.append(f)
    refs = {p: i["references"] for p, i in clo["paths"].items()}
    order = topo_order(rime, refs)
    if order:
        # Level 19 (delta.md §8): 14-20 % fewer bytes than zstd's default level 3 for every client, and it decodes
        # with any zstd's default window (no --long). parallel-compression: the same bytes, less wall clock.
        run(NIX + ["copy", "--no-recursive", "--to", store_url("file://" + os.path.abspath(cache), compression="zstd",
                                                               compression_level=STAGE_LEVEL,
                                                               parallel_compression="true")] + FRESH + order)
    for f in placeholders:
        if "rime-placeholder" not in open(f).read():
            raise Refused(f"{f} was overwritten by the copy")
        os.remove(f)
    names = {n[:-8] for n in os.listdir(cache) if n.endswith(".narinfo")}
    want = {os.path.basename(p)[:32] for p in rime_set}
    if names != want:
        raise Refused(f"the stage holds {len(names)} narinfos for {len(want)} Rime paths")
    shutil.copy(a.closure, os.path.join(out, "closure.json"))
    shutil.copy(a.split, os.path.join(out, "split.json"))
    log(f"stage: {len(order)} Rime paths in {cache} ({len(placeholders)} upstream references not staged)")


def load_stage(artifact):
    """closure.json, split.json and {path: (narinfo text, fields)} of the stage, bound to closure.json."""
    clo, split = load_artifact(artifact)
    cache = os.path.join(artifact, "cache")
    if not os.path.isdir(cache):
        raise Refused(f"{artifact} has no stage (cache/): the build job ran `export`, not `stage`")
    by_hash = {os.path.basename(p)[:32]: p for p in split["rime"]}
    staged = {}
    for n in sorted(os.listdir(cache)):
        if not n.endswith(".narinfo"):
            continue
        p = by_hash.get(n[:-8])
        if p is None:
            raise Refused(f"the stage holds {n}, which is not a Rime-built path of this release")
        text = open(os.path.join(cache, n)).read()
        try:
            ni = parse_narinfo(text)
        except ValueError as e:
            raise Refused(f"stage {n}: {e}")
        why = narinfo_binds(ni, p, clo["paths"][p])
        if why:
            raise Refused(f"stage {n}: {why}")
        url = ni.get("URL", "")
        if not re.match(r"^nar/[0-9a-z]{52}\.nar(\.[a-z0-9]+)?$", url):
            raise Refused(f"stage {n}: URL {url!r} is not a staged NAR")
        staged[p] = (text, ni)
    lacking = [p for p in split["rime"] if p not in staged]
    if lacking:
        raise Refused(f"the stage lacks {len(lacking)} Rime paths, e.g. {lacking[0]}")
    return clo, split, staged


class GitHub:
    """The few REST calls the publisher needs (the same ones `gh release create/upload` makes). The base URLs are
    parameters so the stand-in can serve them; production is api.github.com / uploads.github.com / github.com."""

    def __init__(self, repo, token, api="https://api.github.com", uploads="https://uploads.github.com",
                 download="https://github.com", max_assets=GH_MAX_ASSETS):
        if not re.match(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", repo or ""):
            raise Refused(f"--gh-repo {repo!r} is not owner/name")
        self.repo, self.token = repo, token
        self.api, self.uploads, self.download = api.rstrip("/"), uploads.rstrip("/"), download.rstrip("/")
        self.max_assets = max_assets

    def _req(self, method, url, body=None, headers=None, ok=(200, 201, 204), length=None):
        import urllib.request
        import urllib.error
        h = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
             "User-Agent": "rime-nix-release"}
        if self.token:
            h["Authorization"] = "Bearer " + self.token
        h.update(headers or {})
        data = body
        if isinstance(body, (dict, list)):
            data = json.dumps(body).encode()
            h["Content-Type"] = "application/json"
        if length is not None:
            h["Content-Length"] = str(length)
        for attempt in range(4):
            if hasattr(data, "seek"):
                data.seek(0)
            req = urllib.request.Request(url, data=data, method=method, headers=h)
            try:
                with urllib.request.urlopen(req, timeout=600) as r:
                    raw = r.read()
                    return r.status, (json.loads(raw) if raw.strip() else None)
            except urllib.error.HTTPError as e:
                raw = e.read()
                if e.code in ok:
                    return e.code, None
                if e.code >= 500 and attempt < 3:
                    time.sleep(2 ** attempt)
                    continue
                return e.code, raw[:500]
            except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
                if attempt < 3:
                    time.sleep(2 ** attempt)
                    continue
                raise Refused(f"{method} {url}: {e}")
        raise Refused(f"{method} {url}: no answer")

    def release(self, tag, create=False):
        code, body = self._req("GET", f"{self.api}/repos/{self.repo}/releases/tags/{tag}")
        if code == 200:
            return body
        if code != 404:
            raise Refused(f"reading release {tag} of {self.repo} answered {code}: {body!r}")
        if not create:
            return None
        code, body = self._req("POST", f"{self.api}/repos/{self.repo}/releases", {
            "tag_name": tag, "name": tag, "target_commitish": "main", "draft": False, "prerelease": False,
            "make_latest": "false",
            "body": f"NAR files of Rime OS release {tag.split('-part')[0]} (narinfos: the main branch)."})
        if code != 201:
            raise Refused(f"creating release {tag} of {self.repo} answered {code}: {body!r}")
        log(f"github: created release {tag}")
        return body

    def assets(self, rel):
        out, page = [], 1
        while True:
            code, body = self._req("GET", f"{self.api}/repos/{self.repo}/releases/{rel['id']}/assets"
                                          f"?per_page=100&page={page}")
            if code != 200:
                raise Refused(f"listing the assets of {rel.get('tag_name')} answered {code}: {body!r}")
            out += body
            if len(body) < 100:
                return out
            page += 1

    def delete_asset(self, asset):
        code, body = self._req("DELETE", f"{self.api}/repos/{self.repo}/releases/assets/{asset['id']}")
        if code not in (204, 404):
            raise Refused(f"deleting the broken asset {asset.get('name')} answered {code}: {body!r}")

    def upload(self, rel, name, path):
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            code, body = self._req("POST", f"{self.uploads}/repos/{self.repo}/releases/{rel['id']}/assets"
                                           f"?name={name}", f, {"Content-Type": "application/octet-stream"},
                                   length=size)
        if code != 201 or not isinstance(body, dict) or body.get("size") != size:
            raise Refused(f"uploading {name} to {rel.get('tag_name')} answered {code}: {body!r}")
        return body

    def url(self, tag, name):
        return f"{self.download}/{self.repo}/releases/download/{tag}/{name}"

    def put_assets(self, release_id, files):
        """files: {asset name: local path}. Uploads what is not already an uploaded asset of the same size in the
        release `release_id` or its parts (<id>-part2, ...), skipping the rest; returns ({name: url}, uploaded)."""
        for name, path in files.items():
            if not re.match(r"^[0-9a-z]{52}\.(nar(\.[a-z0-9]+)?|delta)$", name):
                raise Refused(f"{name} is not a NAR or delta file name")
            if os.path.getsize(path) > GH_MAX_ASSET:
                raise Refused(f"{name} is {os.path.getsize(path)} bytes; a release asset must be under 2 GiB "
                              "(TODO(handover): split large NARs or host them elsewhere)")
        parts, n = [], 1
        while True:
            tag = release_id if n == 1 else f"{release_id}-part{n}"
            rel = self.release(tag)
            if rel is None:
                break
            parts.append([tag, rel, {x["name"]: x for x in self.assets(rel)}])
            n += 1
        urls, uploaded = {}, 0
        for name, path in sorted(files.items()):
            size = os.path.getsize(path)
            where = next((p for p in parts if name in p[2]), None)
            if where is not None:
                x = where[2][name]
                if x.get("state") == "uploaded" and x.get("size") == size:
                    urls[name] = self.url(where[0], name)
                    continue
                # A failed upload leaves the name taken (state "starter"/"new") or a wrong size: replace it.
                log(f"github: replacing the broken asset {name} in {where[0]} ({x.get('state')}, {x.get('size')} B)")
                self.delete_asset(x)
                del where[2][name]
            else:
                where = next((p for p in parts if len(p[2]) < self.max_assets), None)
                if where is None:
                    n = len(parts) + 1
                    tag = release_id if n == 1 else f"{release_id}-part{n}"
                    where = [tag, self.release(tag, create=True), {}]
                    parts.append(where)
            where[2][name] = self.upload(where[1], name, path)
            urls[name] = self.url(where[0], name)
            uploaded += 1
        return urls, uploaded


def git(args, cwd=None, env=None, check=True):
    return run(["git"] + args, check=check, env=env, quiet=True, stdout=subprocess.PIPE) if cwd is None else \
        run(["git", "-C", cwd] + args, check=check, env=env, quiet=True, stdout=subprocess.PIPE)


def git_env(remote, token):
    """The token travels as an http.extraheader for the remote (actions/checkout's way): never in the URL, never on
    a command line (environment only)."""
    env = dict(os.environ)
    env.update({"GIT_TERMINAL_PROMPT": "0", "GIT_AUTHOR_NAME": GH_BOT[0], "GIT_AUTHOR_EMAIL": GH_BOT[1],
                "GIT_COMMITTER_NAME": GH_BOT[0], "GIT_COMMITTER_EMAIL": GH_BOT[1]})
    if token and remote.startswith("https://"):
        basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
        env.update({"GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": f"http.{remote}.extraheader",
                    "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {basic}"})
    return env


def gh_fill(artifact, gh, remote, secret_key_file, release_id, workdir, report=None, before_push=None):
    """§6.3 for the GitHub backend. Order: bind + sign the stage, NARs up as release assets FIRST, then the
    narinfos (URL rewritten to the asset) committed and pushed to main. Idempotent: a path whose narinfo is on main
    is published already; an asset already uploaded is not sent again. Never force-pushes, never deletes."""
    clo, split, staged = load_stage(artifact)
    stage = os.path.join(artifact, "cache")
    rime = sorted(staged)
    pub = run(NIX + ["key", "convert-secret-to-public"], data=open(secret_key_file, "rb").read(),
              quiet=True).stdout.decode().strip()
    if rime:
        stage_url = "file://" + os.path.abspath(stage)
        run(NIX + ["store", "sign", "--store", stage_url, "--key-file", secret_key_file] + FRESH + rime)
        # Contents too (no --no-contents): every staged NAR is unpacked and hashed against its signed narHash.
        run(NIX + ["store", "verify", "--store", stage_url, "--sigs-needed", "1",
                   "--trusted-public-keys", pub] + FRESH + rime)
        for p in rime:  # re-read: the narinfos now carry the Sig line
            text = open(os.path.join(stage, os.path.basename(p)[:32] + ".narinfo")).read()
            staged[p] = (text, parse_narinfo(text))
    env = git_env(remote, gh.token)
    co = os.path.join(workdir, "rime-cache")
    shutil.rmtree(co, ignore_errors=True)
    git(["clone", "--quiet", "--depth", "1", "--branch", "main", remote, co], env=env)

    def plan():
        done, todo = [], []
        for p in rime:
            f = os.path.join(co, os.path.basename(p)[:32] + ".narinfo")
            if os.path.exists(f):
                why = narinfo_binds(parse_narinfo(open(f).read()), p, clo["paths"][p])
                if why:
                    raise Refused(f"{p} is published already with another content ({why}); the build host must "
                                  "substitute published paths from cache.rimeos.com, never rebuild them (§6.3)")
                done.append(p)
            else:
                todo.append(p)
        return done, todo

    done, todo = plan()
    files = {}
    for p in todo:
        url = staged[p][1]["URL"]
        files[os.path.basename(url)] = os.path.join(stage, url)
    urls, uploaded = gh.put_assets(release_id, files) if files else ({}, 0)
    log(f"github: {len(todo)} paths to publish, {len(done)} published already, {uploaded} NARs uploaded")

    commit = None
    for attempt in range(6):
        done, todo = plan() if attempt else (done, todo)
        for p in todo:
            text, ni = staged[p]
            open(os.path.join(co, os.path.basename(p)[:32] + ".narinfo"), "w").write(
                narinfo_with_url(text, urls[os.path.basename(ni["URL"])]))
        info = os.path.join(co, "nix-cache-info")
        if not os.path.exists(info) or open(info).read() != GH_CACHE_INFO:
            open(info, "w").write(GH_CACHE_INFO)
        git(["add", "-A"], cwd=co)
        status = git(["diff", "--cached", "--name-status"], cwd=co).stdout.decode().split("\n")
        allowed_new = {os.path.basename(p)[:32] + ".narinfo" for p in todo}
        for line in filter(None, status):
            st, name = line.split("\t", 1)
            if st == "A" and name in allowed_new or st in ("A", "M") and name == "nix-cache-info":
                continue
            raise Refused(f"refusing to commit {st} {name} to rime-cache: a publish only ADDS narinfos")
        if not any(status):
            log("github: rime-cache main already holds every narinfo; nothing to commit")
            break
        git(["commit", "--quiet", "-m", f"release {release_id}: {len(todo)} narinfos"], cwd=co, env=env)
        if before_push and attempt == 0:
            run(["sh", "-c", before_push])  # TEST ONLY: the stand-in moves main under this run here
        p = git(["push", "--quiet", "origin", "HEAD:refs/heads/main"], cwd=co, env=env, check=False)
        if p.returncode == 0:
            commit = git(["rev-parse", "HEAD"], cwd=co).stdout.decode().strip()
            log(f"github: pushed {commit} to rime-cache main ({len(todo)} narinfos)")
            break
        log(f"github: push refused ({p.stderr.decode(errors='replace').strip()[-200:]}); fetching main again")
        git(["fetch", "--quiet", "--depth", "1", "origin", "main"], cwd=co, env=env)
        git(["reset", "--quiet", "--hard", "FETCH_HEAD"], cwd=co)
    else:
        raise Refused("rime-cache main kept moving; gave up after 6 pushes")
    result = {"uploaded": len(todo), "already": len(done), "assets_uploaded": uploaded, "commit": commit,
              "upstream": len(split["upstream"])}
    if report:
        write_json(report, result)
    return result


def nar_url_problem(url, origin):
    """None if a published narinfo's URL is absolute and on another origin than the Pages one, else the reason."""
    import urllib.parse
    u = urllib.parse.urlparse(url)
    if u.scheme not in ("http", "https") or not u.netloc:
        return f"URL {url!r} is not absolute (the NARs are release assets, not on Pages)"
    if u.netloc == origin:
        return f"URL {url} is on the Pages origin itself"
    return None


def http_reachable(url):
    """A NAR URL answers: HEAD (redirects followed), else a one-byte ranged GET (HEAD is not promised on every hop)."""
    import urllib.request
    import urllib.error
    if http_status(url) == 200:
        return 200
    req = urllib.request.Request(url, headers={"Range": "bytes=0-0"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return 200 if r.status in (200, 206) else r.status
    except urllib.error.HTTPError as e:
        return e.code
    except urllib.error.URLError as e:
        return str(e.reason)


def check_public_github(closure, split, public, keys, upstream, upstream_keys, report=None, wait=0):
    """The GitHub-backend public check, with no credentials: Rime paths from the Pages origin (narinfo bound to
    closure.json, Rime signature, ABSOLUTE URL on another origin that answers), upstream paths from the upstream
    cache with its key. Pages deploys a push after a delay: the Rime side is retried for up to `wait` seconds."""
    import urllib.parse
    clo = closure if isinstance(closure, dict) else load_json(closure)
    split = split if isinstance(split, dict) else load_json(split)
    rime, up = sorted(split["rime"]), sorted(split["upstream"])
    base = public.split("?", 1)[0].rstrip("/")
    origin = urllib.parse.urlparse(base).netloc
    deadline = time.time() + wait
    while True:
        try:
            info = http_get(f"{base}/nix-cache-info")
            if info is None or info.decode(errors="replace") != GH_CACHE_INFO:
                raise Refused(f"public check: {base}/nix-cache-info is {info!r}, not {GH_CACHE_INFO!r}")
            pub = nix_path_info(rime, store=public)
            problems = []
            for p in rime:
                i = pub.get(p)
                if i is None:
                    problems.append(f"{p}: no narinfo at {public}")
                elif i["nar_hash"] != clo["paths"][p]["nar_hash"] or i["nar_size"] != clo["paths"][p]["nar_size"] \
                        or sorted(i["references"]) != sorted(clo["paths"][p]["references"]):
                    problems.append(f"{p}: the public narinfo does not match closure.json")
                elif not i.get("file_size"):
                    problems.append(f"{p}: the public narinfo has no FileSize")
            if problems:
                raise Refused("public check: " + "; ".join(problems[:10]))
            break
        except Refused:
            if time.time() >= deadline:
                raise
            log("public check: Pages does not serve the pushed narinfos yet; retrying in 20 s")
            time.sleep(20)
    if rime:
        p = run(NIX + ["store", "verify", "--store", public, "--no-contents", "--sigs-needed", "1",
                       "--trusted-public-keys", " ".join(keys)] + FRESH + rime, check=False)
        if p.returncode != 0:
            raise Refused("public check: narinfo signatures: " + p.stderr.decode(errors="replace").strip()[-1500:])
    gone = []
    for path in rime:
        url = pub[path].get("url") or ""
        why = nar_url_problem(url, origin)
        if why:
            gone.append(f"{path}: {why}")
            continue
        code = http_reachable(url)
        if code != 200:
            gone.append(f"{path}: {url} answered {code}")
    if gone:
        raise Refused("public check: " + "; ".join(gone[:10]))
    sizes = {p: pub[p]["file_size"] for p in rime}
    if up:
        if not upstream or not upstream_keys:
            raise Refused("the closure has upstream paths but no upstream cache or key was given")
        upi = nix_path_info(up, store=upstream)
        for p in up:
            i = upi.get(p)
            if i is None or i["nar_hash"] != clo["paths"][p]["nar_hash"] or not i.get("file_size"):
                raise Refused(f"public check: {upstream} no longer serves {p} as closure.json names it")
            sizes[p] = i["file_size"]
        p = run(NIX + ["store", "verify", "--store", upstream, "--no-contents", "--sigs-needed", "1",
                       "--trusted-public-keys", " ".join(upstream_keys)] + FRESH + up, check=False)
        if p.returncode != 0:
            raise Refused("public check: upstream signatures: " + p.stderr.decode(errors="replace").strip()[-1500:])
    if report:
        write_json(report, sizes)
    log(f"public check: {len(rime)} Rime paths at {public} (NARs on another origin), {len(up)} at {upstream}")
    return sizes


# ── deltas (docs/nixos/delta.md §7, §10): zstd --patch-from against the previous release ───────────────────────────
#
# The build job makes them (`deltas`): for every path of the new closure the previous release lacks, a delta against
# the same package in the previous release (pairing: delta/rime_delta.py), at zstd -19 --long=27, decoded again and
# kept only if it rebuilds the exact NAR and is at most DELTA_MAX_RATIO of the path's download size. The publish job
# binds them to the VERIFIED previous manifest and to closure.json, uploads them next to the NARs (GitHub backend
# only) and lists them in the manifest's `delta.files`, so the cosign signature over the manifest covers each delta's
# sha256 and size. A client that cannot use a delta downloads the path whole; nothing here can make a release fail
# that would have published without deltas, except a broken tool.

DELTA_LEVEL = 19
DELTA_LONG = 27
DELTA_MAX_RATIO = 0.8
DELTA_NAME_RE = re.compile(r"^[0-9a-z]{52}\.delta$")
DELTA_FILE_RE = re.compile(r"^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}/[0-9a-z]{52}\.delta$")


def delta_window_log(*sizes):
    """zstd's rule for --patch-from: the highest bit of the larger NAR plus one, at least --long=27, at most 31."""
    return max(DELTA_LONG, min(31, max(sizes).bit_length()))


def sha256_file_raw(fn):
    h = hashlib.sha256()
    with open(fn, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.digest()


def nix32_raw(digest):
    out = []
    for i in range((len(digest) * 8 + 4) // 5 - 1, -1, -1):
        b, j = i * 5 // 8, i * 5 % 8
        c = digest[b] >> j
        if b + 1 < len(digest):
            c |= digest[b + 1] << (8 - j)
        out.append(NIX32[c & 0x1F])
    return "".join(out)


def dump_checked(path, dest, nar_hash, nar_size):
    """`nix-store --dump path` into dest; Refused unless it is exactly the NAR `nar_hash`/`nar_size` name."""
    with open(dest, "wb") as f:
        p = subprocess.run(["nix-store", "--dump", path], stdout=f, stderr=subprocess.PIPE)
    if p.returncode != 0:
        raise Refused(f"nix-store --dump {path}: {p.stderr.decode(errors='replace').strip()[-300:]}")
    if os.path.getsize(dest) != int(nar_size) or "sha256-" + base64.b64encode(sha256_file_raw(dest)).decode() != sri(nar_hash):
        raise Refused(f"{path}: the local copy is not the NAR the manifest names")


def make_delta(base_nar, new_nar, new_hash, wlog, dest, level=DELTA_LEVEL):
    """One delta, decoded again with the window a client allows; Refused unless it rebuilds the exact NAR."""
    p = subprocess.run(["zstd", "-q", "-f", "-T1", f"-{level}", f"--long={DELTA_LONG}", f"--patch-from={base_nar}",
                        new_nar, "-o", dest], stderr=subprocess.PIPE)
    if p.returncode != 0:
        raise Refused(f"zstd --patch-from: {p.stderr.decode(errors='replace').strip()[-300:]}")
    h = hashlib.sha256()
    d = subprocess.Popen(["zstd", "-q", "-d", "-c", f"--long={wlog}", f"--patch-from={base_nar}", dest],
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    for chunk in iter(lambda: d.stdout.read(1 << 20), b""):
        h.update(chunk)
    err = d.stderr.read()
    if d.wait() != 0 or "sha256-" + base64.b64encode(h.digest()).decode() != sri(new_hash):
        raise Refused(f"the delta does not rebuild the NAR ({err.decode(errors='replace').strip()[-200:]})")
    return os.path.getsize(dest)


def realise_bases(bases, substituters, keys):
    """The bases this host lacks, substituted (best effort: a base that cannot be had just means no delta)."""
    have = nix_path_info(sorted(bases))
    missing = [b for b in sorted(bases) if have.get(b) is None]
    if not missing:
        return set(bases)
    opts = []
    if substituters:
        opts += ["--option", "substituters", " ".join(substituters)]
    if keys:
        opts += ["--option", "trusted-public-keys", " ".join(keys)]
    for i in range(0, len(missing), 200):
        run(["nix-store", "--realise"] + opts + ["--option", "fallback", "false"] + FRESH + missing[i:i + 200],
            check=False)
    have = nix_path_info(sorted(bases))
    return {b for b in bases if have.get(b) is not None}


def download_sizes(artifact, clo, split, upstream):
    """What a client downloads for each path today: the staged FileSize (Rime paths), the upstream cache's (the
    rest). The yardstick for DELTA_MAX_RATIO."""
    _, _, staged = load_stage(artifact)
    sizes = {p: int(ni["FileSize"]) for p, (_, ni) in staged.items() if ni.get("FileSize")}
    up = sorted(set(split["upstream"]))
    if up and upstream:
        for p, i in nix_path_info(up, store=upstream).items():
            if i and i.get("file_size"):
                sizes[p] = i["file_size"]
    return sizes


def cmd_deltas(a):
    """Build job (GitHub backend): artifact/deltas/<nix32 sha256>.delta and artifact/deltas.json."""
    import concurrent.futures
    import threading
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "delta"))
    import rime_delta  # the pairing rules G2 measured with (delta/rime_delta.py)
    clo, split = load_artifact(a.artifact)
    if a.previous_manifest:
        prev = load_json(a.previous_manifest)
    else:
        reg = Registry(a.repo, tls_verify=not a.plain_http)
        prev = channel_manifest(reg, a.channel, a.fulcio_root, a.signer or [NIX_SIGNER], a.issuer)
    out = os.path.join(a.artifact, "deltas")
    table = os.path.join(a.artifact, "deltas.json")
    if os.path.exists(out) or os.path.exists(table):
        raise Refused(f"{out} exists; `deltas` writes a fresh one")
    os.makedirs(out)
    result = {"schema": 1, "from": None, "level": a.level, "long": DELTA_LONG, "max_ratio": a.max_ratio,
              "files": [], "skipped": []}
    if prev is None:
        log("deltas: the channel offers no release yet; no deltas")
        write_json(table, result)
        return 0
    errs = validate(prev)
    if errs:
        raise Refused("the previous manifest does not validate: " + "; ".join(errs[:5]))
    result["from"] = prev["release"]
    prev_by_path = {e["path"]: e for e in prev["paths"]}
    old_rows = [{"path": e["path"], "name": rime_delta.store_name(e["path"]), "nar_size": str(e["nar_size"])}
                for e in prev["paths"]]
    old_by = {"name": {}, "family": {}, "loose": {}}
    for r in old_rows:
        old_by["name"].setdefault(r["name"], []).append(r)
        old_by["family"].setdefault(rime_delta.family(r["name"]), []).append(r)
        old_by["loose"].setdefault(rime_delta.loose_family(r["name"]), []).append(r)
    added = sorted(p for p in clo["paths"] if p not in prev_by_path)
    pairs = {}
    for p in added:
        row = {"path": p, "name": rime_delta.store_name(p), "nar_size": str(clo["paths"][p]["nar_size"])}
        _, found = rime_delta.candidates(row, old_by)
        if found:
            pairs[p] = [c["path"] for c in found]
        else:
            result["skipped"].append({"path": p, "why": "no earlier version in the previous release"})
    sizes = download_sizes(a.artifact, clo, split, a.upstream)
    bases = realise_bases({b for bs in pairs.values() for b in bs}, a.substituter, a.trusted_key)
    big = threading.Lock()  # one window over 2^29 at a time: zstd -19 with a 1 GiB base needs several GiB

    def one(p):
        new = clo["paths"][p]
        full = sizes.get(p)
        if not full:
            return None, f"no download size known for {p}"
        tmp = tempfile.mkdtemp(dir=a.tmp or None, prefix="rime-delta-")
        try:
            new_nar = os.path.join(tmp, "new.nar")
            dump_checked(p, new_nar, new["nar_hash"], new["nar_size"])
            best = None
            for b in pairs[p]:
                if b not in bases:
                    continue
                be = prev_by_path[b]
                base_nar = os.path.join(tmp, "base.nar")
                try:
                    dump_checked(b, base_nar, be["nar_hash"], be["nar_size"])
                    w = delta_window_log(be["nar_size"], new["nar_size"])
                    d = os.path.join(tmp, "try.delta")
                    if w > 29:
                        with big:
                            size = make_delta(base_nar, new_nar, new["nar_hash"], w, d, a.level)
                    else:
                        size = make_delta(base_nar, new_nar, new["nar_hash"], w, d, a.level)
                except Refused as e:
                    log(f"deltas: {p} against {b}: {e}")
                    continue
                if best is None or size < best[1]:
                    keep = os.path.join(tmp, "best.delta")
                    os.replace(d, keep)
                    best = (b, size, w)
            if best is None:
                return None, "no usable base on the build host"
            b, size, w = best
            if size > a.max_ratio * full:
                return None, f"the delta ({size} B) is over {a.max_ratio:.0%} of the whole download ({full} B)"
            digest = sha256_file_raw(os.path.join(tmp, "best.delta"))
            name = nix32_raw(digest) + ".delta"
            os.replace(os.path.join(tmp, "best.delta"), os.path.join(out, name))
            be = prev_by_path[b]
            return {"path": p, "base": b, "base_nar_hash": sri(be["nar_hash"]), "base_nar_size": be["nar_size"],
                    "name": name, "file_hash": "sha256-" + base64.b64encode(digest).decode(), "file_size": size,
                    "window_log": w, "full_size": full}, None
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, a.jobs)) as ex:
        for p, (entry, why) in zip(sorted(pairs), ex.map(one, sorted(pairs))):
            if entry:
                result["files"].append(entry)
            else:
                result["skipped"].append({"path": p, "why": why})
    write_json(table, result)
    saved = sum(e["full_size"] - e["file_size"] for e in result["files"])
    log(f"deltas: {len(result['files'])} of {len(added)} new paths against {prev['release']}, "
        f"{sum(e['file_size'] for e in result['files'])} B instead of {sum(e['full_size'] for e in result['files'])} B "
        f"({saved} B saved); {len(result['skipped'])} without")
    return 0


def bind_deltas(artifact, clo, prev, sizes, max_ratio=DELTA_MAX_RATIO):
    """The publish job's check of deltas.json: [entry] that are for THIS closure, against the VERIFIED previous
    manifest, and are the files on disk. Anything else is dropped with a log line (the path is then downloaded
    whole), never a refused release."""
    table = os.path.join(artifact, "deltas.json")
    if not os.path.exists(table):
        return []
    d = load_json(table)
    if prev is None or d.get("from") != prev["release"]:
        log(f"deltas: made against {d.get('from')}, but the channel offers "
            f"{prev['release'] if prev else 'nothing'}; publishing without deltas")
        return []
    prev_by_path = {e["path"]: e for e in prev["paths"]}
    keep, seen = [], set()
    for e in d.get("files", []):
        p, b = e.get("path"), e.get("base")
        why = None
        try:
            name = e.get("name", "")
            fn = os.path.join(artifact, "deltas", name)
            if p not in clo["paths"] or p in prev_by_path:
                why = "not a new path of this closure"
            elif p in seen:
                why = "a second delta for the path"
            elif b not in prev_by_path:
                why = "its base is not in the previous release"
            elif sri(prev_by_path[b]["nar_hash"]) != sri(e["base_nar_hash"]) \
                    or prev_by_path[b]["nar_size"] != e["base_nar_size"]:
                why = "its base is not the previous release's"
            elif not DELTA_NAME_RE.match(name) or not os.path.isfile(fn):
                why = f"no file {name!r}"
            elif os.path.getsize(fn) != e["file_size"]:
                why = "the file's size differs"
            else:
                digest = sha256_file_raw(fn)
                if "sha256-" + base64.b64encode(digest).decode() != e["file_hash"] or nix32_raw(digest) + ".delta" != name:
                    why = "the file's sha256 differs"
                elif e["window_log"] != delta_window_log(e["base_nar_size"], clo["paths"][p]["nar_size"]):
                    why = "its window is not the one the sizes need"
                elif not sizes.get(p) or e["file_size"] > max_ratio * sizes[p]:
                    why = f"over {max_ratio:.0%} of the published download size"
        except (KeyError, TypeError, ValueError) as x:
            why = f"malformed ({x})"
        if why:
            log(f"deltas: {p}: dropped: {why}")
            continue
        seen.add(p)
        keep.append(e)
    return keep


def publish_deltas(gh, release, artifact, entries):
    """Upload the bound deltas as release assets beside the NARs; [entry + file] for the ones that answer publicly."""
    if not entries:
        return []
    files = {e["name"]: os.path.join(artifact, "deltas", e["name"]) for e in entries}
    urls, uploaded = gh.put_assets(release, files)
    out = []
    for e in entries:
        url = urls[e["name"]]
        tag = url.rsplit("/", 2)[-2]
        code = http_reachable(url)
        if code != 200:
            log(f"deltas: {url} answered {code}; {e['path']} is published without its delta")
            continue
        out.append(dict(e, file=f"{tag}/{e['name']}"))
    log(f"deltas: {len(out)} published ({uploaded} uploaded now)")
    return out


def channel_manifest(reg, channel, fulcio_root, signers, issuer):
    """The manifest `channel` offers now, VERIFIED (None when the channel offers nothing)."""
    with tempfile.TemporaryDirectory() as w:
        if not reg.tag_state(CHANNELS_TAG):
            return None
        _, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, fulcio_root, signers, issuer, w)
        e = next((e for e in json.loads(data).get("channels", []) if e["channel"] == channel), None)
        if e is None:
            return None
        _, mdata, _, _ = verify_object(reg, e["digest"], MEDIA_RELEASE, fulcio_root, signers, issuer,
                                       os.path.join(w, "m"))
        return json.loads(mdata)


# ── the manifest ───────────────────────────────────────────────────────────────────────────────────────────────────

def build_manifest(clo, plan, sizes, *, release, previous, issued, source, cache_url_hint, cache_keys,
                   previous_manifest=None, security=(), mass_rebuild=False, upstream=None, deltas=()):
    """deltas: bind_deltas/publish_deltas entries (each with `file`), listed in delta.files.
    upstream: None (one cache holds the whole closure: the S3 fallback) or {"url_hint", "keys", "paths"} (the
    GitHub backend: those paths come from cache.nixos.org; cache.keys then lists its keys too, and each such path
    carries origin "upstream")."""
    refs = {p: i["references"] for p, i in clo["paths"].items()}
    table = sorted(clo["paths"])
    idx = {p: n for n, p in enumerate(table)}
    variants = {}
    for name, v in sorted(clo["variants"].items()):
        pv = plan["variants"].get(name)
        if pv is None:
            raise Refused(f"variant {name} is in closure.json but not in the release plan")
        if pv["toplevel"] != v["toplevel"]:
            raise Refused(f"variant {name}: the plan's toplevel {pv['toplevel']} is not the built {v['toplevel']}")
        clos = closure_of(v["toplevel"], refs)
        comps = {}
        dropped = 0
        for slug, ps in sorted(pv.get("components", {}).items()):
            keep = sorted({idx[p] for p in ps if p in clos})
            dropped += len([p for p in ps if p not in clos])
            if keep:
                comps[slug] = keep
        if dropped:
            log(f"variant {name}: {dropped} component paths are not in the toplevel's closure (not listed)")
        b = v["boot"]
        variants[name] = {
            "toplevel": idx[v["toplevel"]],
            "boot": {"kernel": idx[b["kernel"]], "initrd": idx[b["initrd"]],
                     "kernel_modules": idx[b["kernel_modules"]], "kernel_params": b["kernel_params"]},
            "components": comps,
        }
    catalogue = []
    for c in plan.get("catalogue", []):
        if c["out"] not in idx:
            raise Refused(f"catalogue entry {c['name']}: {c['out']} is not in closure.json")
        catalogue.append({"name": c["name"], "version": c["version"], "out": idx[c["out"]], "class": 2})
    paths = []
    for p in table:
        i = clo["paths"][p]
        fs = sizes.get(p)
        if not fs:
            raise Refused(f"{p}: no public file size")
        entry = {"path": p, "nar_hash": sri(i["nar_hash"]), "nar_size": i["nar_size"], "file_size": fs,
                 "references": sorted(idx[r] for r in i["references"])}
        if upstream is not None:
            entry["origin"] = "upstream" if p in upstream["paths"] else "rime"
        paths.append(entry)
    delta = None
    nixpkgs_moved = False
    if previous_manifest:
        prev_paths = {e["path"] for e in previous_manifest["paths"]}
        nixpkgs_moved = previous_manifest["source"]["nixpkgs"] != source["nixpkgs"]
        if "plain" in clo["variants"]:
            new = [p for p in closure_of(clo["variants"]["plain"]["toplevel"], refs) if p not in prev_paths]
            delta = {"from": previous_manifest["release"], "variant": "plain", "paths": len(new),
                     "file_size": sum(sizes[p] for p in new)}
            if deltas:
                by = {e["path"]: e for e in deltas}
                delta["files"] = [{"path": idx[e["path"]], "base": e["base"], "base_nar_hash": sri(e["base_nar_hash"]),
                                   "base_nar_size": e["base_nar_size"], "file": e["file"], "file_hash": e["file_hash"],
                                   "file_size": e["file_size"], "window_log": e["window_log"]}
                                  for e in sorted(deltas, key=lambda e: idx[e["path"]])]
                delta["delta_size"] = sum(by[p]["file_size"] if p in by else sizes[p] for p in new)
    cache = {"url_hint": cache_url_hint, "keys": list(cache_keys)}
    if upstream is not None:
        cache["keys"] = list(dict.fromkeys(list(cache_keys) + list(upstream["keys"])))
        cache["upstream"] = {"url_hint": upstream["url_hint"], "keys": list(upstream["keys"])}
    return {
        "schema": 1,
        "kind": "rime-nixos-release",
        "release": release,
        "issued": int(issued),
        "previous": previous,
        "system": "x86_64-linux",
        "store_dir": STORE_DIR,
        "source": source,
        "cache": cache,
        "variants": variants,
        "catalogue": catalogue,
        "paths": paths,
        "batch": {"mass_rebuild": bool(mass_rebuild), "nixpkgs_moved": nixpkgs_moved, "security": list(security)},
        "delta": delta,
    }


def validate(m):
    """§3.2, the publisher's copy. Returns a list of reasons; empty means valid."""
    errs = []
    if not isinstance(m, dict):
        return ["the manifest is not a JSON object"]
    if m.get("schema") != 1:
        return [f"schema {m.get('schema')!r} is not 1 (a higher one needs a newer rime)"]
    for k, want in (("kind", "rime-nixos-release"), ("system", "x86_64-linux"), ("store_dir", STORE_DIR)):
        if m.get(k) != want:
            errs.append(f"{k} is {m.get(k)!r}, not {want!r}")
    try:
        parse_release(m.get("release"))
    except ValueError as e:
        errs.append(str(e))
    if m.get("previous") is not None:
        try:
            parse_release(m["previous"])
        except ValueError as e:
            errs.append("previous: " + str(e))
    src = m.get("source") or {}
    for k in ("rime_os", "rime_shell", "nixpkgs"):
        if not SHA40_RE.match(str(src.get(k, ""))):
            errs.append(f"source.{k} is not a 40-hex revision")
    keys = (m.get("cache") or {}).get("keys")
    if not isinstance(keys, list) or not keys:
        errs.append("cache.keys is empty")
    else:
        for k in keys:
            if not valid_key(k):
                errs.append(f"cache.keys: {k!r} is not <name>:<base64 of 32 bytes>")
    upstream = (m.get("cache") or {}).get("upstream")
    if upstream is not None:
        ukeys = upstream.get("keys") if isinstance(upstream, dict) else None
        if not isinstance(ukeys, list) or not ukeys or not all(valid_key(k) for k in ukeys):
            errs.append("cache.upstream.keys is empty or malformed")
        elif not set(ukeys) <= set(keys or []):
            errs.append("cache.upstream.keys are not all in cache.keys")
    paths = m.get("paths")
    if not isinstance(paths, list) or not paths:
        return errs + ["paths is empty"]
    n = len(paths)
    seen = set()
    refs = {}

    def ok_index(i):
        return isinstance(i, int) and not isinstance(i, bool) and 0 <= i < n

    prev = None
    for i, e in enumerate(paths):
        p = e.get("path") if isinstance(e, dict) else None
        if not valid_store_path(p):
            errs.append(f"paths[{i}]: {p!r} is not a store path")
            continue
        if p in seen:
            errs.append(f"paths[{i}]: {p} appears twice")
        if prev is not None and p < prev:
            errs.append(f"paths[{i}]: the table is not sorted by path")
        prev = p
        seen.add(p)
        try:
            if sri(e.get("nar_hash")) != e.get("nar_hash"):
                errs.append(f"paths[{i}]: nar_hash is not in SRI form")
        except ValueError as x:
            errs.append(f"paths[{i}]: {x}")
        for k in ("nar_size", "file_size"):
            v = e.get(k)
            if not isinstance(v, int) or isinstance(v, bool) or v <= 0:
                errs.append(f"paths[{i}]: {k} {v!r} is not a positive integer")
        origin = e.get("origin")
        if upstream is None and origin is not None:
            errs.append(f"paths[{i}]: origin without cache.upstream")
        elif upstream is not None and origin not in ("rime", "upstream"):
            errs.append(f"paths[{i}]: origin {origin!r} is not rime or upstream")
        r = e.get("references")
        if not isinstance(r, list) or not all(ok_index(x) for x in r):
            errs.append(f"paths[{i}]: a reference is out of range")
            refs[i] = []
        else:
            refs[i] = r

    def closure(i):
        return closure_of(i, refs)

    variants = m.get("variants")
    if not isinstance(variants, dict) or not variants:
        errs.append("variants is empty")
        variants = {}
    for name, v in variants.items():
        t = v.get("toplevel") if isinstance(v, dict) else None
        if not ok_index(t):
            errs.append(f"variants.{name}.toplevel is out of range")
            continue
        clo = closure(t)
        boot = v.get("boot") or {}
        for k in ("kernel", "initrd", "kernel_modules"):
            if not ok_index(boot.get(k)):
                errs.append(f"variants.{name}.boot.{k} is out of range")
            elif boot[k] not in clo:
                errs.append(f"variants.{name}.boot.{k} is not in the toplevel's closure")
        if not isinstance(boot.get("kernel_params"), list):
            errs.append(f"variants.{name}.boot.kernel_params is not a list")
        for slug, ix in (v.get("components") or {}).items():
            for x in ix:
                if not ok_index(x):
                    errs.append(f"variants.{name}.components.{slug}: index out of range")
                elif x not in clo:
                    errs.append(f"variants.{name}.components.{slug}: {paths[x]['path']} is not in the closure")
    for c in m.get("catalogue") or []:
        if not ok_index(c.get("out")):
            errs.append(f"catalogue {c.get('name')!r}: out is out of range")
        elif c.get("class") != 2:
            errs.append(f"catalogue {c.get('name')!r}: class is not 2")
    errs += validate_delta(m.get("delta"), paths, ok_index)
    return errs


def validate_delta(d, paths, ok_index):
    """The delta table (§3.1, delta.md §7). Its `files` are what clients download in place of whole paths."""
    if d is None:
        return []
    if not isinstance(d, dict):
        return ["delta is not an object"]
    errs = []
    files = d.get("files")
    if files is None:
        return errs
    if not isinstance(files, list):
        return ["delta.files is not a list"]

    def pos_int(v):
        return isinstance(v, int) and not isinstance(v, bool) and v > 0

    seen = set()
    for i, e in enumerate(files):
        w = f"delta.files[{i}]"
        if not isinstance(e, dict):
            errs.append(f"{w} is not an object")
            continue
        if not ok_index(e.get("path")):
            errs.append(f"{w}.path is out of range")
            continue
        if e["path"] in seen:
            errs.append(f"{w}: a second delta for {paths[e['path']]['path']}")
        seen.add(e["path"])
        if not valid_store_path(e.get("base")) or e.get("base") == paths[e["path"]].get("path"):
            errs.append(f"{w}.base is not another store path")
        for k in ("base_nar_hash", "file_hash"):
            try:
                if sri(e.get(k)) != e.get(k):
                    errs.append(f"{w}.{k} is not in SRI form")
            except ValueError as x:
                errs.append(f"{w}.{k}: {x}")
        for k in ("base_nar_size", "file_size"):
            if not pos_int(e.get(k)):
                errs.append(f"{w}.{k} is not a positive integer")
        if not isinstance(e.get("file"), str) or not DELTA_FILE_RE.match(e["file"]):
            errs.append(f"{w}.file is not <release tag>/<nix32>.delta")
        wl = e.get("window_log")
        if not isinstance(wl, int) or isinstance(wl, bool) or not DELTA_LONG <= wl <= 31:
            errs.append(f"{w}.window_log is not {DELTA_LONG}..31")
    if files and not pos_int(d.get("delta_size")):
        errs.append("delta.delta_size is not a positive integer")
    return errs


def cmd_manifest(a):
    clo = load_json(a.closure)
    plan = load_json(a.plan)
    sizes = load_json(a.public)
    prev = load_json(a.previous_manifest) if a.previous_manifest else None
    m = build_manifest(clo, plan, sizes, release=a.release, previous=a.previous or None, issued=a.issued or time.time(),
                       source={"rime_os": a.rime_os, "rime_shell": a.rime_shell, "nixpkgs": a.nixpkgs},
                       cache_url_hint=a.cache_url_hint, cache_keys=a.cache_key, previous_manifest=prev,
                       security=a.security, mass_rebuild=a.mass_rebuild)
    errs = validate(m)
    if errs:
        raise Refused("the manifest does not validate: " + "; ".join(errs[:10]))
    with open(a.out, "wb") as f:
        f.write(canonical(m))


def cmd_validate(a):
    with open(a.manifest, "rb") as f:
        raw = f.read()
    try:
        m = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"invalid: not JSON: {e}")
        return 1
    errs = validate(m)
    if not errs and a.canonical and canonical(m) != raw:
        errs = ["not in canonical form (sorted keys, no whitespace)"]
    for e in errs:
        print(f"invalid: {e}")
    if not errs:
        print("valid")
    return 1 if errs else 0


# ── OCI objects ────────────────────────────────────────────────────────────────────────────────────────────────────

def pack(payload, media, out, annotations):
    """One-layer OCI image manifest in skopeo's dir: layout. Returns the manifest digest."""
    os.makedirs(out, exist_ok=True)
    cfg = b"{}"
    for blob in (payload, cfg):
        with open(os.path.join(out, sha256_hex(blob)), "wb") as f:
            f.write(blob)
    man = canonical({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "artifactType": media,
        "config": {"mediaType": OCI_CONFIG, "digest": "sha256:" + sha256_hex(cfg), "size": len(cfg)},
        "layers": [{"mediaType": media, "digest": "sha256:" + sha256_hex(payload), "size": len(payload)}],
        "annotations": dict(sorted(annotations.items())),
    })
    with open(os.path.join(out, "manifest.json"), "wb") as f:
        f.write(man)
    with open(os.path.join(out, "version"), "w") as f:
        f.write("Directory Transport Version: 1.1\n")
    return "sha256:" + sha256_hex(man)


class Registry:
    """skopeo for transport (the client's own transport), cosign for signing."""

    def __init__(self, repo, tls_verify=True, cosign="cosign"):
        self.repo = repo
        self.tls = tls_verify
        self.cosign = cosign

    def _tls(self, flag):
        return [] if self.tls else [f"--{flag}-tls-verify=false"]

    def tag_state(self, tag):
        p = run(["skopeo", "--insecure-policy", "inspect", "--raw"] + ([] if self.tls else ["--tls-verify=false"])
                + [f"docker://{self.repo}:{tag}"], check=False, quiet=True)
        if p.returncode == 0:
            return "sha256:" + sha256_hex(p.stdout)
        err = p.stderr.decode(errors="replace")
        if "manifest unknown" in err or "not found" in err.lower() or "name unknown" in err:
            return None
        raise Refused(f"cannot read {self.repo}:{tag}: {err.strip()[-500:]}")

    def resolve(self, tag):
        """Exactly verify::resolve's call."""
        p = run(["skopeo", "--insecure-policy", "inspect", "--format", "{{.Digest}}"]
                + ([] if self.tls else ["--tls-verify=false"]) + [f"docker://{self.repo}:{tag}"])
        return p.stdout.decode().strip()

    def push(self, src_dir, tag):
        local = "sha256:" + sha256_hex(open(os.path.join(src_dir, "manifest.json"), "rb").read())
        run(["skopeo", "--insecure-policy", "copy", "--preserve-digests"] + self._tls("dest")
            + [f"dir:{src_dir}", f"docker://{self.repo}:{tag}"])
        remote = self.tag_state(tag)
        if remote != local:
            raise Refused(f"pushed {local} to {tag}, the registry answers {remote}")
        return local

    def fetch(self, ref, out):
        shutil.rmtree(out, ignore_errors=True)
        run(["skopeo", "--insecure-policy", "copy"] + self._tls("src") + [f"docker://{ref}", f"dir:{out}"], quiet=True)

    def sign(self, digest, test_signing=None):
        ref = f"{self.repo}@{digest}"
        if test_signing:
            log("SIGNING WITH A TEST KEY (stand-in only): " + test_signing)
            env = dict(os.environ, COSIGN_PASSWORD="")
            cmd = [self.cosign, "sign", "--yes", "--key", os.path.join(test_signing, "cosign.key"),
                   "--certificate", os.path.join(test_signing, "leaf.pem"),
                   "--certificate-chain", os.path.join(test_signing, "chain.pem"), "--tlog-upload=false"]
            if not self.tls:
                cmd.append("--allow-http-registry")
            run(cmd + [ref], env=env)
        else:
            # Keyless: the GitHub OIDC token of this job, Fulcio, Rekor. Only nix-release.yml on main can get here.
            run([self.cosign, "sign", "--yes", ref])


# ── the self-check: verify.rs, ported ──────────────────────────────────────────────────────────────────────────────

def der_tlv(b, i):
    tag = b[i]
    ln = b[i + 1]
    j = i + 2
    if ln & 0x80:
        n = ln & 0x7F
        ln = int.from_bytes(b[j:j + n], "big")
        j += n
    return tag, j, j + ln


def der_children(b, start, end):
    out = []
    i = start
    while i < end:
        t, s, e = der_tlv(b, i)
        out.append((t, s, e))
        i = e
    return out


def oid_bytes(dotted):
    parts = [int(x) for x in dotted.split(".")]
    out = bytearray([parts[0] * 40 + parts[1]])
    for v in parts[2:]:
        enc = [v & 0x7F]
        v >>= 7
        while v:
            enc.append(0x80 | (v & 0x7F))
            v >>= 7
        out.extend(reversed(enc))
    return bytes(out)


def cert_extensions(der):
    """{oid-bytes: extnValue bytes} of an X.509 certificate."""
    _, s, e = der_tlv(der, 0)
    _, ts, te = der_children(der, s, e)[0]
    exts = {}
    for t, s2, e2 in der_children(der, ts, te):
        if t == 0xA3:  # [3] EXPLICIT Extensions
            _, ss, se = der_tlv(der, s2)
            for _, xs, xe in der_children(der, ss, se):
                kids = der_children(der, xs, xe)
                oid = der[kids[0][1]:kids[0][2]]
                val = kids[-1]
                exts[oid] = der[val[1]:val[2]]
    return exts


def pem_to_der(pem):
    body = re.search(r"-----BEGIN CERTIFICATE-----(.*?)-----END CERTIFICATE-----", pem, re.S)
    if not body:
        raise ValueError("no certificate in PEM")
    return base64.b64decode("".join(body.group(1).split()))


def cert_san_uris(der):
    v = cert_extensions(der).get(oid_bytes("2.5.29.17"))
    if v is None:
        return []
    _, s, e = der_tlv(v, 0)
    return [v[cs:ce].decode() for t, cs, ce in der_children(v, s, e) if t == 0x86]


def cert_issuer(der):
    exts = cert_extensions(der)
    raw = exts.get(oid_bytes("1.3.6.1.4.1.57264.1.8"))
    if raw is not None:
        t, s, e = der_tlv(raw, 0)
        if t == 0x0C:
            return raw[s:e].decode()
    raw = exts.get(oid_bytes("1.3.6.1.4.1.57264.1.1"))
    if raw is not None:
        # Fulcio writes .1.1 as the raw string; tolerate a DER UTF8String wrapper as verify.rs does.
        if len(raw) >= 2 and raw[0] == 0x0C and raw[1] == len(raw) - 2:
            raw = raw[2:]
        return raw.decode()
    return None


def verify_sig_dir(sigdir, repo, digest, fulcio_root, signers, issuer):
    """The `.sig` object for `digest`, already fetched into `sigdir`. Raises Refused; returns the signer."""
    man = load_json(os.path.join(sigdir, "manifest.json"))
    layers = man.get("layers") or []
    if not layers:
        raise Refused("the signature object has no layers")
    lay = layers[0]
    if lay.get("mediaType") != SIG_LAYER:
        raise Refused(f"the signature layer is {lay.get('mediaType')}, not simple signing")
    ann = lay.get("annotations") or {}
    if ANN_SIG not in ann or ANN_CERT not in ann:
        raise Refused("the signature carries no signature or no certificate (keyless cosign writes both)")
    hexd = lay["digest"].split(":", 1)[1]
    with open(os.path.join(sigdir, hexd), "rb") as f:
        payload = f.read()
    if sha256_hex(payload) != hexd:
        raise Refused("the signed payload does not match its layer digest")
    doc = json.loads(payload)
    crit = doc.get("critical") or {}
    if (crit.get("image") or {}).get("docker-manifest-digest") != digest:
        raise Refused(f"the signature covers {(crit.get('image') or {}).get('docker-manifest-digest')}, not {digest}")
    sref = (crit.get("identity") or {}).get("docker-reference", "")
    if repo_of(sref) != repo_of(repo):
        raise Refused(f"the signature covers an object in {sref}, not in {repo}")
    with tempfile.TemporaryDirectory() as w:
        leaf, pub, sig, pl, chain, root = (os.path.join(w, n) for n in ("leaf.pem", "pub.pem", "sig", "payload", "chain.pem", "root.pem"))
        open(leaf, "w").write(ann[ANN_CERT])
        open(sig, "wb").write(base64.b64decode(ann[ANN_SIG]))
        open(pl, "wb").write(payload)
        shutil.copy(fulcio_root, root)
        p = run(["openssl", "x509", "-pubkey", "-noout", "-in", leaf], check=False, quiet=True)
        if p.returncode:
            raise Refused("the certificate does not parse")
        open(pub, "wb").write(p.stdout)
        if run(["openssl", "dgst", "-sha256", "-verify", pub, "-signature", sig, pl], check=False, quiet=True).returncode:
            raise Refused("the signature does not verify against the certificate's public key")
        p = run(["openssl", "x509", "-noout", "-startdate", "-dateopt", "iso_8601", "-in", leaf], check=False, quiet=True)
        at = int(datetime.datetime.strptime(p.stdout.decode().strip().split("=", 1)[1], "%Y-%m-%d %H:%M:%SZ")
                 .replace(tzinfo=datetime.timezone.utc).timestamp())
        cmd = ["openssl", "verify", "-CAfile", root, "-attime", str(at)]
        if ann.get(ANN_CHAIN):
            open(chain, "w").write(ann[ANN_CHAIN])
            cmd += ["-untrusted", chain]
        p = run(cmd + [leaf], check=False, quiet=True)
        if p.returncode:
            raise Refused("the signing certificate does not chain to the pinned Fulcio root: "
                          + (p.stdout + p.stderr).decode(errors="replace").strip()[-300:])
    der = pem_to_der(ann[ANN_CERT])
    sans = cert_san_uris(der)
    if not sans:
        raise Refused("the signing certificate names no identity")
    if sans[0] not in signers:
        raise Refused(f"signed by {sans[0]}, and the release expects {' or '.join(signers)}")
    got = cert_issuer(der)
    if got != issuer:
        raise Refused(f"the identity was vouched for by {got}, not {issuer}")
    return sans[0]


def verify_object(reg, ref, media, fulcio_root, signers, issuer, work):
    """Resolve, verify the signature, then read the layer BY that digest (verify::artifact_layer_at)."""
    digest = ref if ref.startswith("sha256:") else reg.resolve(ref)
    if not re.match(r"^sha256:[0-9a-f]{64}$", digest):
        raise Refused(f"{ref} resolved to {digest!r}")
    os.makedirs(work, exist_ok=True)
    sigdir = os.path.join(work, "sig")
    reg.fetch(f"{reg.repo}:{digest.replace(':', '-')}.sig", sigdir)
    signer = verify_sig_dir(sigdir, reg.repo, digest, fulcio_root, signers, issuer)
    objdir = os.path.join(work, "obj")
    reg.fetch(f"{reg.repo}@{digest}", objdir)
    raw = open(os.path.join(objdir, "manifest.json"), "rb").read()
    if "sha256:" + sha256_hex(raw) != digest:
        raise Refused("the object read is not the digest that was verified")
    man = json.loads(raw)
    lay = (man.get("layers") or [{}])[0]
    if lay.get("mediaType") != media or man.get("artifactType") != media:
        raise Refused(f"the object is {lay.get('mediaType')}, not {media}")
    hexd = lay["digest"].split(":", 1)[1]
    data = open(os.path.join(objdir, hexd), "rb").read()
    if sha256_hex(data) != hexd:
        raise Refused("the layer does not match its digest")
    return digest, data, signer, man.get("annotations") or {}


def cmd_verify(a):
    reg = Registry(a.repo, tls_verify=not a.plain_http)
    media = MEDIA_CHANNELS if a.channels else MEDIA_RELEASE
    with tempfile.TemporaryDirectory() as w:
        try:
            digest, data, signer, _ = verify_object(reg, a.ref, media, a.fulcio_root, a.signer or [NIX_SIGNER],
                                                    a.issuer, w)
        except Refused as e:
            print(f"refused: {e}")
            return 1
    obj = json.loads(data)
    if not a.channels:
        errs = validate(obj)
        if errs:
            print("refused: " + "; ".join(errs[:5]))
            return 1
    else:
        left = obj.get("expires", 0) - time.time()
        if a.min_left is not None and left < a.min_left:
            print(f"refused: the channel document has {left / 86400:.1f} days left")
            return 1
    print(json.dumps({"digest": digest, "signer": signer, "release": obj.get("release")}))
    return 0


# ── the channel document ───────────────────────────────────────────────────────────────────────────────────────────

def next_channels(current, repository, now, *, set_entry=None, entry_releases=None):
    """The next channel document: serial + 1, a fresh 14-day expiry, every other entry carried forward.

    set_entry: {"channel", "digest", "release", "percent", "halt", "reason"}. entry_releases maps a digest the
    current document names to its (verified) release ID, for the never-backward rule (§4)."""
    serial = int(current["serial"]) + 1 if current else 1
    entries = {e["channel"]: dict(e) for e in (current or {}).get("channels", [])}
    if current and current.get("repository") != repository:
        raise Refused(f"the live document names {current.get('repository')}, not {repository}")
    if set_entry:
        ch = set_entry["channel"]
        if ch not in CHANNELS:
            raise Refused(f"unknown channel {ch!r}")
        if not re.match(r"^sha256:[0-9a-f]{64}$", set_entry["digest"]):
            raise Refused(f"bad digest {set_entry['digest']!r}")
        pct = set_entry.get("percent", 100)
        if not isinstance(pct, int) or isinstance(pct, bool) or not 0 <= pct <= 100:
            raise Refused(f"percent {pct!r} is not 0-100")
        new_rel = set_entry["release"]
        parse_release(new_rel)
        old = entries.get(ch)
        if old:
            old_rel = old.get("release") or (entry_releases or {}).get(old["digest"])
            if old_rel is None:
                raise Refused(f"{ch} offers {old['digest']}, whose release is unknown; refusing to compare")
            if parse_release(new_rel) < parse_release(old_rel):
                raise Refused(f"{ch} offers {old_rel}; {new_rel} is older. Fix forward: halt and release again")
        e = {"channel": ch, "digest": set_entry["digest"], "percent": pct, "halt": bool(set_entry.get("halt", False)),
             "release": new_rel}
        if set_entry.get("reason"):
            e["reason"] = str(set_entry["reason"])
        entries[ch] = e
    return {
        "schema": 1,
        "serial": serial,
        "issued": int(now),
        "expires": int(now) + DOC_LIFETIME,
        "repository": repository,
        "channels": [entries[c] for c in sorted(entries, key=lambda c: CHANNELS.index(c) if c in CHANNELS else 99)],
    }


def cmd_channels(a):
    cur = load_json(a.current) if a.current else None
    now = a.now or time.time()
    if a.check_live:
        left = cur["expires"] - now
        if left < DOC_MIN_LEFT:
            print(f"the live channel document has {left / 86400:.1f} days left (under 7): the refresh is broken")
            return 1
        print(f"the live channel document has {left / 86400:.1f} days left")
        return 0
    entry = None
    if a.set:
        entry = {"channel": a.set, "digest": a.digest, "release": a.release, "percent": a.percent,
                 "halt": a.halt == "true", "reason": a.reason}
    doc = next_channels(cur, a.repository, now, set_entry=entry)
    with open(a.out, "wb") as f:
        f.write(canonical(doc))
    return 0


# ── release IDs ────────────────────────────────────────────────────────────────────────────────────────────────────

def next_release_id(script, today, os_rev, shell_rev, candidates):
    """next-release-id with its published-* inputs = the newest of the candidates (bootc :daily, Nix edge)."""
    cands = [c for c in candidates if c and c.get("id") and RELEASE_RE.match(c["id"])]
    newest = max(cands, key=lambda c: parse_release(c["id"])) if cands else {"id": "", "os": "", "shell": ""}
    p = run([script, today, os_rev, shell_rev, newest["id"], newest.get("os", ""), newest.get("shell", "")])
    rid = p.stdout.decode().strip()
    parse_release(rid)
    return rid


# ── the publish job ────────────────────────────────────────────────────────────────────────────────────────────────

def before_push_hook(a):
    """RIME_TEST_BEFORE_PUSH (a shell command the VM test uses to move rime-cache main mid-publish) is honoured only
    in a run signed with a TEST key (--test-signing): a real publish never executes a command from its environment."""
    return os.environ.get("RIME_TEST_BEFORE_PUSH") if a.test_signing else None


def refuse_unpublishable(plan, allow_test_variants):
    """A variant marked publishable = false (a test configuration, e.g. rime-vm's test account) never ships, unless
    the stand-in passes --allow-test-variants."""
    for name, v in sorted(plan["variants"].items()):
        if not v.get("publishable") and not allow_test_variants:
            raise Refused(f"variant {name} is not publishable ({v.get('note', 'test configuration')})")


def cmd_publish(a):
    for k in ("rime_os", "rime_shell", "nixpkgs"):
        if not SHA40_RE.match(getattr(a, k)):
            raise Refused(f"--{k.replace('_', '-')} is not a 40-hex revision")
    tag = f"nix-{a.rime_os}-{a.rime_shell}"
    reg = Registry(a.repo, tls_verify=not a.plain_http, cosign=a.cosign)
    out = a.out
    os.makedirs(out, exist_ok=True)
    status = {"tag": tag, "repo": a.repo}

    existing = reg.tag_state(tag)
    if existing:
        log(f"{tag} exists ({existing}): this rime-os + rime-shell pair was released; nothing new is published")
        status.update(state="exists", manifest_digest=existing)
        write_json(os.path.join(out, "published.json"), status)
        return 0

    plan = load_json(a.plan)
    refuse_unpublishable(plan, a.allow_test_variants)

    keys = a.cache_pubkey
    # What the channels offer now: read and VERIFIED, never trusted on its face.
    signers = a.signer or [NIX_SIGNER]
    cur_doc, offered, entry_releases = None, {}, {}
    with tempfile.TemporaryDirectory() as w:
        if reg.tag_state(CHANNELS_TAG):
            _, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, a.fulcio_root, signers, a.issuer, w)
            cur_doc = json.loads(data)
            for e in cur_doc.get("channels", []):
                _, mdata, _, _ = verify_object(reg, e["digest"], MEDIA_RELEASE, a.fulcio_root, signers, a.issuer,
                                               os.path.join(w, e["channel"]))
                offered[e["channel"]] = json.loads(mdata)
                entry_releases[e["digest"]] = offered[e["channel"]]["release"]
    prev_manifest = offered.get(a.channel)
    # next-release-id's published-* inputs: the newer of bootc's :daily labels and the Nix edge release (§2).
    candidates = [{"id": a.bootc_release_id, "os": a.bootc_revision, "shell": a.bootc_shell}]
    if "edge" in offered:
        candidates.append({"id": offered["edge"]["release"], "os": offered["edge"]["source"]["rime_os"],
                           "shell": offered["edge"]["source"]["rime_shell"]})
    release = a.release or next_release_id(a.next_release_id, a.today, a.rime_os, a.rime_shell, candidates)
    # The release ID is known BEFORE the fill: the GitHub backend names the NAR release after it. A run that stops
    # after uploading gets the same ID next time (same day, same edge), so its assets are found, not sent again.
    upstream = None
    if a.backend == "github":
        token = os.environ.get("RIME_CACHE_TOKEN", "")
        gh = GitHub(a.gh_repo, token, a.gh_api, a.gh_uploads, a.gh_download, max_assets=a.gh_max_assets)
        with tempfile.TemporaryDirectory() as w:
            gh_fill(a.artifact, gh, a.gh_remote, a.secret_key_file, release, w,
                    report=os.path.join(out, "fill.json"), before_push=before_push_hook(a))
        clo, split = load_artifact(a.artifact)
        sizes = check_public_github(clo, split, a.public, keys, a.upstream, a.upstream_key,
                                    report=os.path.join(out, "public.json"), wait=a.public_wait)
        upstream = {"url_hint": a.upstream, "keys": a.upstream_key, "paths": set(split["upstream"])}
        deltas = publish_deltas(gh, release, a.artifact, bind_deltas(a.artifact, clo, prev_manifest, sizes))
    else:
        if not a.cache:
            raise Refused("--backend s3 needs --cache s3://...")
        fill(a.artifact, a.cache, a.secret_key_file, a.upstream, a.upstream_key, report=os.path.join(out, "fill.json"))
        clo, _ = load_artifact(a.artifact)
        sizes = check_public(clo, a.public, keys, report=os.path.join(out, "public.json"))
        deltas = []
        if os.path.exists(os.path.join(a.artifact, "deltas.json")):
            log("deltas: the S3 fallback publishes no deltas (delta.md §7); every path is downloaded whole")

    m = build_manifest(clo, plan, sizes, release=release,
                       previous=prev_manifest["release"] if prev_manifest else None, issued=a.issued or time.time(),
                       source={"rime_os": a.rime_os, "rime_shell": a.rime_shell, "nixpkgs": a.nixpkgs},
                       cache_url_hint=a.cache_url_hint, cache_keys=keys, previous_manifest=prev_manifest,
                       upstream=upstream, deltas=deltas)
    errs = validate(m)
    if errs:
        raise Refused("the manifest does not validate: " + "; ".join(errs[:10]))
    if prev_manifest and parse_release(release) <= parse_release(prev_manifest["release"]):
        raise Refused(f"release {release} is not newer than {prev_manifest['release']}, which {a.channel} offers")
    payload = canonical(m)
    open(os.path.join(out, "manifest.json"), "wb").write(payload)
    mdir = os.path.join(out, "manifest-oci")
    digest = pack(payload, MEDIA_RELEASE, mdir, {
        "org.rimeos.release.id": release,
        "org.opencontainers.image.revision": a.rime_os,
        "org.rimeos.rime-shell.ref": a.rime_shell,
        "org.rimeos.product": "nixos",
    })
    # Re-read right before the push: never overwrite a manifest tag.
    if reg.tag_state(tag):
        raise Refused(f"{tag} appeared while this run was filling the cache; not overwriting it")
    reg.push(mdir, tag)
    reg.sign(digest, a.test_signing)
    with tempfile.TemporaryDirectory() as w:
        got, data, signer, _ = verify_object(reg, tag, MEDIA_RELEASE, a.fulcio_root, signers, a.issuer, w)
    if got != digest or data != payload:
        raise Refused("self-check: the published manifest is not the one built")
    log(f"self-check: {tag} = {digest}, signed by {signer}")
    status.update(state="published", release=release, manifest_digest=digest, manifest_tag=tag,
                  deltas=len((m.get("delta") or {}).get("files", [])))

    doc = next_channels(cur_doc, a.repo, time.time(), set_entry={
        "channel": a.channel, "digest": digest, "release": release, "percent": a.percent, "halt": False},
        entry_releases=entry_releases)
    dpay = canonical(doc)
    ddir = os.path.join(out, "channels-oci")
    ddigest = pack(dpay, MEDIA_CHANNELS, ddir, {"org.rimeos.product": "nixos",
                                               "org.opencontainers.image.revision": a.rime_os})
    reg.push(ddir, CHANNELS_TAG)
    reg.sign(ddigest, a.test_signing)
    with tempfile.TemporaryDirectory() as w:
        got, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, a.fulcio_root, signers, a.issuer, w)
    if got != ddigest or json.loads(data)["expires"] - time.time() < DOC_MIN_LEFT:
        raise Refused("self-check: the live channel document is not the one written, or expires within 7 days")
    open(os.path.join(out, "channels.json"), "wb").write(dpay)
    status.update(channels_digest=ddigest, channels_serial=doc["serial"], channel=a.channel)
    write_json(os.path.join(out, "published.json"), status)
    log(f"published {release}: {tag} on {a.channel}, channel document serial {doc['serial']}")
    return 0


def cmd_refresh(a):
    """The daily refresh and promote: re-sign the channel document (serial + 1, new expiry)."""
    reg = Registry(a.repo, tls_verify=not a.plain_http, cosign=a.cosign)
    signers = a.signer or [NIX_SIGNER]
    with tempfile.TemporaryDirectory() as w:
        _, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, a.fulcio_root, signers, a.issuer, w)
        cur = json.loads(data)
        entry, releases = None, {}
        for e in cur.get("channels", []):
            if not e.get("release"):
                _, md, _, _ = verify_object(reg, e["digest"], MEDIA_RELEASE, a.fulcio_root, signers, a.issuer,
                                            os.path.join(w, e["channel"]))
                releases[e["digest"]] = json.loads(md)["release"]
        if a.set:
            if not RELEASE_RE.match(a.release or ""):
                raise Refused(f"release {a.release!r} is not a release ID")
            # The release must name a manifest that exists and verifies.
            mtag = a.manifest_tag
            if not re.match(r"^nix-[0-9a-f]{40}-[0-9a-f]{40}$", mtag or ""):
                raise Refused(f"--manifest-tag {mtag!r} is not nix-<os sha>-<shell sha>")
            digest, md, _, _ = verify_object(reg, mtag, MEDIA_RELEASE, a.fulcio_root, signers, a.issuer,
                                             os.path.join(w, "target"))
            mm = json.loads(md)
            if mm["release"] != a.release:
                raise Refused(f"{mtag} is release {mm['release']}, not {a.release}")
            entry = {"channel": a.set, "digest": digest, "release": a.release, "percent": a.percent,
                     "halt": a.halt == "true", "reason": a.reason}
    doc = next_channels(cur, a.repo, time.time(), set_entry=entry, entry_releases=releases)
    os.makedirs(a.out, exist_ok=True)
    d = pack(canonical(doc), MEDIA_CHANNELS, os.path.join(a.out, "channels-oci"), {"org.rimeos.product": "nixos"})
    reg.push(os.path.join(a.out, "channels-oci"), CHANNELS_TAG)
    reg.sign(d, a.test_signing)
    with tempfile.TemporaryDirectory() as w:
        got, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, a.fulcio_root, signers, a.issuer, w)
    if got != d or json.loads(data)["expires"] - time.time() < DOC_MIN_LEFT:
        raise Refused("self-check: the live channel document is not the one written")
    log(f"channel document serial {doc['serial']} live")
    return 0


# ── the reference consumer ─────────────────────────────────────────────────────────────────────────────────────────

def cmd_consume(a):
    """What a client does with a published release (the real one is D7's `rime update`). For the stand-in."""
    reg = Registry(a.repo, tls_verify=not a.plain_http)
    signers = a.signer or [NIX_SIGNER]
    with tempfile.TemporaryDirectory() as w:
        _, data, _, _ = verify_object(reg, CHANNELS_TAG, MEDIA_CHANNELS, a.fulcio_root, signers, a.issuer, w)
        doc = json.loads(data)
        if doc.get("repository") != a.repo or doc["expires"] < time.time():
            raise Refused("the channel document is for another repository or expired")
        e = next((e for e in doc["channels"] if e["channel"] == a.channel), None)
        if e is None or e.get("halt"):
            raise Refused(f"channel {a.channel} offers nothing")
        digest, mdata, _, _ = verify_object(reg, e["digest"], MEDIA_RELEASE, a.fulcio_root, signers, a.issuer,
                                            os.path.join(w, "m"))
    m = json.loads(mdata)
    errs = validate(m)
    if errs:
        raise Refused("; ".join(errs))
    v = m["variants"][a.variant]
    top = m["paths"][v["toplevel"]]["path"]
    keys = " ".join(m["cache"]["keys"])  # exactly the keys the verified manifest announces (§6.4)
    trust = ["--option", "require-sigs", "true", "--option", "trusted-public-keys", keys, "--option", "max-jobs", "0"]
    if len(a.cache) == 1:
        dest = ["--to", a.store] if a.store else []
        run(NIX + ["copy", "--from", a.cache[0]] + dest + trust + ["--option", "substituters", ""] + FRESH + [top])
    else:
        # Several caches (the GitHub backend: Rime's paths on cache.rimeos.com, the rest on cache.nixos.org): one
        # `nix copy --from` reads one store, so substitute instead, with exactly these caches and nothing to build.
        store = ["--store", a.store] if a.store else []
        run(NIX + ["build", "--no-link"] + store + trust + ["--option", "substituters", " ".join(a.cache),
                                                           "--option", "fallback", "false"] + FRESH + [top])
    refs = {i: e["references"] for i, e in enumerate(m["paths"])}
    clo = sorted(closure_of(v["toplevel"], refs))
    local = nix_path_info([m["paths"][i]["path"] for i in clo], store=a.store)
    for i in clo:
        e = m["paths"][i]
        got = local.get(e["path"])
        if got is None or got["nar_hash"] != e["nar_hash"]:
            raise Refused(f"{e['path']}: not the narHash the manifest names")
    print(json.dumps({"release": m["release"], "digest": digest, "toplevel": top, "paths": len(clo),
                      "serial": doc["serial"]}))
    return 0


# ── plumbing ───────────────────────────────────────────────────────────────────────────────────────────────────────

def load_json(path):
    with open(path) as f:
        return json.load(f)


def write_json(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f, indent=1, sort_keys=True)
        f.write("\n")


def main(argv=None):
    ap = argparse.ArgumentParser(prog="rime-nix-release", description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def registry_args(p):
        p.add_argument("--repo", required=True)
        p.add_argument("--plain-http", action="store_true", help="registry over plain HTTP (stand-in only)")
        p.add_argument("--fulcio-root", required=True)
        p.add_argument("--signer", action="append", default=[], help=f"expected identity (default {NIX_SIGNER})")
        p.add_argument("--issuer", default=ISSUER)

    p = sub.add_parser("closure")
    p.add_argument("--variant", action="append", required=True, help="name=toplevel")
    p.add_argument("--catalogue-out", action="append", default=[])
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_closure)

    p = sub.add_parser("split")
    p.add_argument("--closure", required=True)
    p.add_argument("--upstream", default="https://cache.nixos.org")
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_split)

    p = sub.add_parser("export")
    p.add_argument("--closure", required=True)
    p.add_argument("--split", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_export)

    def fill_args(p, cache_required=True):
        p.add_argument("--artifact", required=True)
        p.add_argument("--cache", required=cache_required,
                       help="S3 backend: s3://rime-cache?endpoint=...&region=auto&compression=zstd")
        p.add_argument("--secret-key-file", required=True)
        p.add_argument("--upstream", default="https://cache.nixos.org")
        p.add_argument("--upstream-key", action="append", default=[])

    p = sub.add_parser("stage")
    p.add_argument("--closure", required=True)
    p.add_argument("--split", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_stage)

    p = sub.add_parser("deltas", help="build job: zstd --patch-from deltas against the previous release")
    p.add_argument("--artifact", required=True, help="the stage (`stage --out`); writes deltas/ and deltas.json")
    p.add_argument("--previous-manifest", help="the previous release's manifest (default: verified from --repo)")
    p.add_argument("--repo")
    p.add_argument("--plain-http", action="store_true")
    p.add_argument("--fulcio-root")
    p.add_argument("--signer", action="append", default=[])
    p.add_argument("--issuer", default=ISSUER)
    p.add_argument("--channel", default="edge", choices=CHANNELS)
    p.add_argument("--upstream", default="https://cache.nixos.org", help="for the upstream paths' download sizes")
    p.add_argument("--substituter", action="append", default=[], help="where the previous release's paths are")
    p.add_argument("--trusted-key", action="append", default=[])
    p.add_argument("--level", type=int, default=DELTA_LEVEL)
    p.add_argument("--max-ratio", type=float, default=DELTA_MAX_RATIO)
    p.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    p.add_argument("--tmp", help="scratch for the NARs (default: TMPDIR)")
    p.set_defaults(fn=cmd_deltas)

    p = sub.add_parser("fill")
    fill_args(p)
    p.add_argument("--report")
    p.set_defaults(fn=lambda a: fill(a.artifact, a.cache, a.secret_key_file, a.upstream, a.upstream_key, a.report) and 0)

    p = sub.add_parser("check-public")
    p.add_argument("--closure", required=True)
    p.add_argument("--public", required=True)
    p.add_argument("--key", action="append", required=True)
    p.add_argument("--report")
    p.set_defaults(fn=lambda a: check_public(a.closure, a.public, a.key, a.report) and 0)

    p = sub.add_parser("check-public-github")
    p.add_argument("--artifact", required=True)
    p.add_argument("--public", required=True)
    p.add_argument("--key", action="append", required=True)
    p.add_argument("--upstream", default="https://cache.nixos.org")
    p.add_argument("--upstream-key", action="append", default=[])
    p.add_argument("--wait", type=int, default=0)
    p.add_argument("--report")
    p.set_defaults(fn=lambda a: check_public_github(*load_artifact(a.artifact), a.public, a.key, a.upstream,
                                                    a.upstream_key, a.report, a.wait) and 0)

    p = sub.add_parser("manifest")
    for k in ("closure", "plan", "public", "release", "rime-os", "rime-shell", "nixpkgs", "out"):
        p.add_argument("--" + k, required=True)
    p.add_argument("--previous")
    p.add_argument("--previous-manifest")
    p.add_argument("--issued", type=int)
    p.add_argument("--cache-url-hint", default="https://cache.rimeos.com")
    p.add_argument("--cache-key", action="append", required=True)
    p.add_argument("--security", action="append", default=[])
    p.add_argument("--mass-rebuild", action="store_true")
    p.set_defaults(fn=cmd_manifest)

    p = sub.add_parser("validate")
    p.add_argument("manifest")
    p.add_argument("--canonical", action="store_true")
    p.set_defaults(fn=cmd_validate)

    p = sub.add_parser("verify")
    registry_args(p)
    p.add_argument("--ref", required=True, help="a tag or a sha256: digest")
    p.add_argument("--channels", action="store_true")
    p.add_argument("--min-left", type=int)
    p.set_defaults(fn=cmd_verify)

    p = sub.add_parser("channels")
    p.add_argument("--current")
    p.add_argument("--repository")
    p.add_argument("--now", type=int)
    p.add_argument("--check-live", action="store_true")
    p.add_argument("--set", choices=CHANNELS)
    p.add_argument("--digest")
    p.add_argument("--release")
    p.add_argument("--percent", type=int, default=100)
    p.add_argument("--halt", choices=("true", "false"), default="false")
    p.add_argument("--reason")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_channels)

    p = sub.add_parser("publish")
    fill_args(p, cache_required=False)
    registry_args(p)
    p.add_argument("--backend", choices=("github", "s3"), default="github",
                   help="github (production): narinfos on Pages, NARs as release assets; s3: the fallback")
    p.add_argument("--gh-repo", default="AndreNijman/rime-cache")
    p.add_argument("--gh-remote", default="https://github.com/AndreNijman/rime-cache.git",
                   help="git remote of the narinfo repository (token: RIME_CACHE_TOKEN)")
    p.add_argument("--gh-api", default="https://api.github.com")
    p.add_argument("--gh-uploads", default="https://uploads.github.com")
    p.add_argument("--gh-download", default="https://github.com")
    p.add_argument("--gh-max-assets", type=int, default=GH_MAX_ASSETS)
    p.add_argument("--public-wait", type=int, default=0,
                   help="seconds to wait for Pages to serve the pushed narinfos (GitHub backend)")
    p.add_argument("--plan", required=True)
    p.add_argument("--public", required=True, help="the cache's public, anonymous URL")
    p.add_argument("--cache-pubkey", action="append", required=True)
    p.add_argument("--cache-url-hint", default="https://cache.rimeos.com")
    for k in ("rime-os", "rime-shell", "nixpkgs"):
        p.add_argument("--" + k, required=True)
    p.add_argument("--channel", default="edge", choices=CHANNELS)
    p.add_argument("--percent", type=int, default=100)
    p.add_argument("--today", default=datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).strftime("%Y.%m.%d"))
    p.add_argument("--release", help="force the release ID (tests); default: next-release-id")
    p.add_argument("--next-release-id", default=os.environ.get("RIME_NEXT_RELEASE_ID", "files/scripts/next-release-id"))
    p.add_argument("--bootc-release-id", default="")
    p.add_argument("--bootc-revision", default="")
    p.add_argument("--bootc-shell", default="")
    p.add_argument("--issued", type=int)
    p.add_argument("--allow-test-variants", action="store_true")
    p.add_argument("--test-signing", help="TEST ONLY: a directory with cosign.key, leaf.pem, chain.pem")
    p.add_argument("--cosign", default=os.environ.get("RIME_COSIGN", "cosign"))
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_publish)

    p = sub.add_parser("refresh")
    registry_args(p)
    p.add_argument("--set", choices=CHANNELS)
    p.add_argument("--release")
    p.add_argument("--manifest-tag", help="nix-<os>-<shell> of the release to point --set at")
    p.add_argument("--percent", type=int, default=100)
    p.add_argument("--halt", choices=("true", "false"), default="false")
    p.add_argument("--reason")
    p.add_argument("--test-signing")
    p.add_argument("--cosign", default=os.environ.get("RIME_COSIGN", "cosign"))
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_refresh)

    p = sub.add_parser("consume")
    registry_args(p)
    p.add_argument("--channel", default="edge", choices=CHANNELS)
    p.add_argument("--variant", default="plain")
    p.add_argument("--cache", action="append", required=True,
                   help="a binary cache to substitute from; repeat it (GitHub backend: cache.rimeos.com, then "
                        "cache.nixos.org)")
    p.add_argument("--store", help="the Nix store to download into (default: this machine's)")
    p.set_defaults(fn=cmd_consume)

    a = ap.parse_args(argv)
    try:
        return a.fn(a) or 0
    except Refused as e:
        log(f"REFUSED: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
