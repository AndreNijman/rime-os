#!/usr/bin/env python3
"""oci-layer.py OCI_DIR ROOT NOTE: append ONE layer holding the files under ROOT to the single image of the OCI layout
OCI_DIR (what `podman save --format oci-dir` writes), in place. prepare.sh makes the bridge lab's READINESS image this
way: the published bootc image plus a layer with exactly what the readiness release (draft rime-os #137) adds for the
move (README.md here). A `podman build` FROM the published image is not used: on this host its COPY walks the whole
129-layer rootfs (the ostree repo included) through fuse-overlayfs while holding podman's storage lock, which stalls
every other podman user for tens of minutes.

Files go in owned by root:root with ROOT's modes; directories ROOT has are written as directories (mode 0755). The
config's rootfs.diff_ids and history, the manifest's layers and config, and index.json are rewritten; the old config
and manifest blobs are left (unreferenced). Prints the new manifest digest.
"""
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile


def blob(oci, data, kind):
    d = hashlib.sha256(data).hexdigest()
    p = os.path.join(oci, "blobs", "sha256", d)
    if not os.path.exists(p):
        with open(p, "wb") as f:
            f.write(data)
    return {"mediaType": kind, "digest": f"sha256:{d}", "size": len(data)}


def read_blob(oci, digest):
    with open(os.path.join(oci, "blobs", "sha256", digest.split(":", 1)[1]), "rb") as f:
        return f.read()


def main():
    oci, root, note = sys.argv[1:4]
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as t:
        for dirpath, dirs, files in sorted(os.walk(root)):
            dirs.sort()
            rel = os.path.relpath(dirpath, root)
            for name in [None] + sorted(files):
                if name is None and rel == ".":
                    continue
                path = dirpath if name is None else os.path.join(dirpath, name)
                arc = os.path.relpath(path, root)
                ti = t.gettarinfo(path, arcname=arc)
                ti.uid = ti.gid = 0
                ti.uname = ti.gname = "root"
                ti.mtime = 0
                if ti.isdir():
                    ti.mode = 0o755
                    t.addfile(ti)
                else:
                    with open(path, "rb") as f:
                        t.addfile(ti, f)
                print(f"layer: {ti.mode:04o} {arc}")
    tar = raw.getvalue()
    diff_id = "sha256:" + hashlib.sha256(tar).hexdigest()
    layer = blob(oci, gzip.compress(tar, mtime=0), "application/vnd.oci.image.layer.v1.tar+gzip")

    with open(os.path.join(oci, "index.json")) as f:
        index = json.load(f)
    if len(index["manifests"]) != 1:
        sys.exit(f"oci-layer: {oci} holds {len(index['manifests'])} manifests, want 1")
    mdesc = index["manifests"][0]
    manifest = json.loads(read_blob(oci, mdesc["digest"]))
    config = json.loads(read_blob(oci, manifest["config"]["digest"]))
    if diff_id in config["rootfs"]["diff_ids"]:
        sys.exit("oci-layer: this layer is already in the image")
    config["rootfs"]["diff_ids"].append(diff_id)
    config.setdefault("history", []).append({"created_by": f"bridge lab: {note}", "comment": "oci-layer.py"})
    manifest["config"] = blob(oci, json.dumps(config).encode(), manifest["config"]["mediaType"])
    manifest["layers"].append(layer)
    m = blob(oci, json.dumps(manifest).encode(), mdesc.get("mediaType", "application/vnd.oci.image.manifest.v1+json"))
    mdesc.update(digest=m["digest"], size=m["size"])
    with open(os.path.join(oci, "index.json"), "w") as f:
        json.dump(index, f)
    print(f"oci-layer: layer {layer['digest']} ({layer['size']} bytes, diff_id {diff_id}); manifest {m['digest']}"
          f" ({len(manifest['layers'])} layers)")


if __name__ == "__main__":
    main()
