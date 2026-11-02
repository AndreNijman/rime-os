"""TEST ONLY (nix/tests/update-e2e.nix): what an attacker with write access to the registry, and no signing key, can
publish. Built on the release tool's own functions, so the objects are byte-for-byte what the pipeline would push.

  update-e2e-tamper manifest --repo R --fulcio-root PEM --test-signing DIR --release ID
      The manifest edge offers, changed after signing (release ID and the first path's narSize), pushed under a new
      digest with the ORIGINAL signature copied next to it. The channel document naming it is signed with the test
      key, as if the channel signer had been fooled: only the manifest's own signature can catch it.
  update-e2e-tamper channels --repo R
      The live channel document, changed after signing (serial + 1, nothing else), pushed under nix-channels with
      the previous document's signature copied next to it.
  update-e2e-tamper unsigned --repo R --fulcio-root PEM --test-signing DIR --release ID
      The manifest edge offers, as a NEW object (release ID) pushed with no signature at all. The channel document
      naming it is signed with the test key: a signed channel must not carry an unsigned manifest.
  update-e2e-tamper nar --pages URL --github-dir DIR --path STOREPATH --old TEXT --new TEXT
      The NAR of STOREPATH as the release asset store (fake-github's files/) serves it, changed inside: decompressed,
      TEXT replaced by NEW (same length, so NarSize still matches), compressed again the same way. The narinfo and
      its signature are untouched, so its NarHash names the original contents: only the hash check can catch it.

Prints the new digest (manifest, channels, unsigned) or the asset file changed (nar).
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request

import rime_nix_release as r


def copy_sig(reg, old, new):
    src = f"docker://{reg.repo}:{old.replace(':', '-')}.sig"
    dst = f"docker://{reg.repo}:{new.replace(':', '-')}.sig"
    r.run(["skopeo", "--insecure-policy", "copy", "--preserve-digests"] + reg._tls("src") + reg._tls("dest") + [src, dst])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("what", choices=("manifest", "channels", "unsigned", "nar"))
    ap.add_argument("--repo")
    ap.add_argument("--fulcio-root")
    ap.add_argument("--test-signing")
    ap.add_argument("--release")
    ap.add_argument("--pages")
    ap.add_argument("--github-dir")
    ap.add_argument("--path")
    ap.add_argument("--old")
    ap.add_argument("--new")
    a = ap.parse_args()
    if a.what == "nar":
        return tamper_nar(a)
    reg = r.Registry(a.repo, tls_verify=False, cosign=os.environ["RIME_COSIGN"])
    with tempfile.TemporaryDirectory() as w:
        if a.what == "unsigned":
            cur_digest, data, _, _ = r.verify_object(reg, r.CHANNELS_TAG, r.MEDIA_CHANNELS, a.fulcio_root,
                                                     [r.NIX_SIGNER], r.ISSUER, os.path.join(w, "cur"))
            cur = json.loads(data)
            edge = next(e for e in cur["channels"] if e["channel"] == "edge")
            # Read, not verified: the edge may already be an earlier subtest's tampered manifest.
            m, ann = raw_object(reg, edge["digest"], os.path.join(w, "man"))
            m["release"] = a.release
            new = r.pack(r.canonical(m), r.MEDIA_RELEASE, os.path.join(w, "unsigned"), ann)
            reg.push(os.path.join(w, "unsigned"), "e2e-unsigned")
            # No signature is pushed for `new`; the test proves the registry has none before the client asks.
            doc = r.next_channels(cur, a.repo, time.time(), set_entry={
                "channel": "edge", "digest": new, "release": a.release, "percent": 100, "halt": False},
                entry_releases={edge["digest"]: edge.get("release")})
            d = r.pack(r.canonical(doc), r.MEDIA_CHANNELS, os.path.join(w, "doc"), {"org.rimeos.product": "nixos"})
            reg.push(os.path.join(w, "doc"), r.CHANNELS_TAG)
            reg.sign(d, a.test_signing)
            print(new)
        elif a.what == "manifest":
            cur_digest, data, _, _ = r.verify_object(reg, r.CHANNELS_TAG, r.MEDIA_CHANNELS, a.fulcio_root,
                                                     [r.NIX_SIGNER], r.ISSUER, os.path.join(w, "cur"))
            cur = json.loads(data)
            edge = next(e for e in cur["channels"] if e["channel"] == "edge")
            old, mdata, _, ann = r.verify_object(reg, edge["digest"], r.MEDIA_RELEASE, a.fulcio_root, [r.NIX_SIGNER],
                                                 r.ISSUER, os.path.join(w, "man"))
            m = json.loads(mdata)
            m["release"] = a.release
            m["paths"][0]["nar_size"] += 1
            new = r.pack(r.canonical(m), r.MEDIA_RELEASE, os.path.join(w, "bad"), ann)
            reg.push(os.path.join(w, "bad"), "e2e-tampered")
            copy_sig(reg, old, new)
            doc = r.next_channels(cur, a.repo, time.time(), set_entry={
                "channel": "edge", "digest": new, "release": a.release, "percent": 100, "halt": False},
                entry_releases={edge["digest"]: edge.get("release")})
            d = r.pack(r.canonical(doc), r.MEDIA_CHANNELS, os.path.join(w, "doc"), {"org.rimeos.product": "nixos"})
            reg.push(os.path.join(w, "doc"), r.CHANNELS_TAG)
            reg.sign(d, a.test_signing)
            print(new)
        else:
            old = reg.resolve(r.CHANNELS_TAG)
            reg.fetch(f"{reg.repo}@{old}", os.path.join(w, "obj"))
            man = json.load(open(os.path.join(w, "obj", "manifest.json")))
            hexd = man["layers"][0]["digest"].split(":", 1)[1]
            doc = json.load(open(os.path.join(w, "obj", hexd)))
            doc["serial"] += 1
            new = r.pack(r.canonical(doc), r.MEDIA_CHANNELS, os.path.join(w, "doc"), man.get("annotations") or {})
            reg.push(os.path.join(w, "doc"), r.CHANNELS_TAG)
            copy_sig(reg, old, new)
            print(new)
    return 0


def raw_object(reg, digest, d):
    """The JSON layer and the annotations of one pushed object, read without checking any signature."""
    reg.fetch(f"{reg.repo}@{digest}", d)
    man = json.load(open(os.path.join(d, "manifest.json")))
    hexd = man["layers"][0]["digest"].split(":", 1)[1]
    return json.load(open(os.path.join(d, hexd))), man.get("annotations") or {}


def tamper_nar(a):
    if len(a.old.encode()) != len(a.new.encode()):
        sys.exit("--old and --new must be the same length")
    base = os.path.basename(a.path)[:32]
    narinfo = urllib.request.urlopen(f"{a.pages}/{base}.narinfo").read().decode()
    fields = dict(line.split(": ", 1) for line in narinfo.splitlines() if ": " in line)
    name = fields["URL"].rsplit("/", 1)[1]
    state = json.load(open(os.path.join(a.github_dir, "state.json")))
    ids = [x["id"] for rel in state["releases"] for x in rel["assets"] if x["name"] == name]
    if len(ids) != 1:
        sys.exit(f"{name}: {len(ids)} assets, expected one")
    f = os.path.join(a.github_dir, "files", str(ids[0]))
    comp = fields.get("Compression", "none")
    tool = {"zstd": ["zstd", "-q"], "xz": ["xz", "-T0"], "none": None}[comp]
    raw = open(f, "rb").read()
    nar = subprocess.run(tool + ["-d", "-c"], input=raw, capture_output=True, check=True).stdout if tool else raw
    old, new = a.old.encode(), a.new.encode()
    if nar.count(old) != 1:
        sys.exit(f"{a.path}: {old!r} occurs {nar.count(old)} times in its NAR")
    nar = nar.replace(old, new)
    out = subprocess.run(tool + ["-c"], input=nar, capture_output=True, check=True).stdout if tool else nar
    open(f, "wb").write(out)
    print(f"{f} ({comp}): {old!r} -> {new!r} inside the NAR of {a.path}; narinfo {fields['NarHash']} unchanged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
