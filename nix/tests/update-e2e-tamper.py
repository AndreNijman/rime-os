"""TEST ONLY (nix/tests/update-e2e.nix): what an attacker with write access to the registry, and no signing key, can
publish. Built on the release tool's own functions, so the objects are byte-for-byte what the pipeline would push.

  update-e2e-tamper manifest --repo R --fulcio-root PEM --test-signing DIR --release ID
      The manifest edge offers, changed after signing (release ID and the first path's narSize), pushed under a new
      digest with the ORIGINAL signature copied next to it. The channel document naming it is signed with the test
      key, as if the channel signer had been fooled: only the manifest's own signature can catch it.
  update-e2e-tamper channels --repo R
      The live channel document, changed after signing (serial + 1, nothing else), pushed under nix-channels with
      the previous document's signature copied next to it.

Prints the new digest.
"""

import argparse
import json
import os
import sys
import tempfile
import time

import rime_nix_release as r


def copy_sig(reg, old, new):
    src = f"docker://{reg.repo}:{old.replace(':', '-')}.sig"
    dst = f"docker://{reg.repo}:{new.replace(':', '-')}.sig"
    r.run(["skopeo", "--insecure-policy", "copy", "--preserve-digests"] + reg._tls("src") + reg._tls("dest") + [src, dst])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("what", choices=("manifest", "channels"))
    ap.add_argument("--repo", required=True)
    ap.add_argument("--fulcio-root")
    ap.add_argument("--test-signing")
    ap.add_argument("--release")
    a = ap.parse_args()
    reg = r.Registry(a.repo, tls_verify=False, cosign=os.environ["RIME_COSIGN"])
    with tempfile.TemporaryDirectory() as w:
        if a.what == "manifest":
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


if __name__ == "__main__":
    sys.exit(main())
