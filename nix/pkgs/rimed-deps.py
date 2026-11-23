#!/usr/bin/env python3
"""The rimed workspace's third-party crates, compiled once (nix/pkgs/rimed.nix `deps`).

    rimed-deps.py dummy WORKSPACE            give every member an empty body at each target path its manifest names
    rimed-deps.py prune TARGET METADATA      remove every workspace member's artifacts from a cargo target directory
    rimed-deps.py recompiled TARGET METADATA MARKER
                                             name the third-party crates cargo compiled after MARKER was written

`dummy` runs in the deps derivation, whose source is only Cargo.toml + Cargo.lock: a source change in a member does
not change that derivation, so its compiled crates are reused until a manifest or the lock moves.

`prune` is what makes reusing them safe. cargo decides a path crate is fresh by comparing mtimes, so a dummy member
artifact left in the target directory could be taken for the real crate. With every member's fingerprint, deps and
build entries gone, cargo has no record of any member and compiles all of them; the third-party crates are
registry (vendored) sources, which cargo fingerprints by version, features and flags, never by the source's mtime.
"""
import json
import os
import re
import shutil
import sys
import tomllib


def dummy(workspace):
    with open(os.path.join(workspace, "Cargo.toml"), "rb") as f:
        members = tomllib.load(f)["workspace"]["members"]
    made = 0
    for member in members:
        root = os.path.join(workspace, member)
        with open(os.path.join(root, "Cargo.toml"), "rb") as f:
            manifest = tomllib.load(f)
        paths = {}
        lib = manifest.get("lib")
        if lib is not None:
            paths[lib.get("path", "src/lib.rs")] = ""
        for kind in ("bin", "test", "bench", "example"):
            for target in manifest.get(kind, []):
                if "path" not in target:
                    sys.exit(f"rimed-deps: {member}: a [[{kind}]] without a path cannot be given a dummy body")
                paths[target["path"]] = "fn main() {}\n"
        if "build" in manifest.get("package", {}):
            sys.exit(f"rimed-deps: {member} has a build script; the deps derivation does not model one")
        # Something cargo can build, and that has a unit test target: `cargo test` then compiles the member's
        # dev-dependencies as the real workspace's test build does.
        if not any(v == "" for v in paths.values()):
            paths.setdefault("src/lib.rs", "")
        for rel, body in paths.items():
            p = os.path.join(root, rel)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "w") as f:
                f.write(body)
            made += 1
    print(f"rimed-deps: {made} dummy targets for {len(members)} members")


def members(metadata):
    with open(metadata) as f:
        meta = json.load(f)
    ids = set(meta["workspace_members"])
    pkgs = [p for p in meta["packages"] if p["id"] in ids]
    if not pkgs:
        sys.exit("rimed-deps: cargo metadata names no workspace member")
    names = {p["name"] for p in pkgs}
    crates = {t["name"].replace("-", "_") for p in pkgs for t in p["targets"]}
    return names, crates


def profile_dirs(target):
    for dirpath, dirnames, _ in os.walk(target):
        if ".fingerprint" in dirnames:
            yield dirpath
        # never descend into a profile's own subdirectories
        dirnames[:] = [d for d in dirnames if d not in (".fingerprint", "deps", "build", "examples", "incremental")]


def prune(target, metadata):
    names, crates = members(metadata)
    by_name = re.compile(r"^(?:%s)-[0-9a-f]{16}$" % "|".join(map(re.escape, sorted(names))))
    by_crate = re.compile(r"^(?:lib)?(?:%s)-[0-9a-f]{16}(?:\..*)?$" % "|".join(map(re.escape, sorted(crates))))
    removed = 0
    profiles = list(profile_dirs(target))
    if not profiles:
        sys.exit(f"rimed-deps: no cargo profile directory under {target}")
    for prof in profiles:
        for sub, pattern in ((".fingerprint", by_name), ("build", by_name), ("deps", by_crate), ("examples", by_crate)):
            d = os.path.join(prof, sub)
            if not os.path.isdir(d):
                continue
            for entry in os.listdir(d):
                if pattern.match(entry):
                    p = os.path.join(d, entry)
                    shutil.rmtree(p) if os.path.isdir(p) and not os.path.islink(p) else os.remove(p)
                    removed += 1
        # The profile directory's own files are the members' uplifted binaries and their .d files.
        for entry in os.listdir(prof):
            p = os.path.join(prof, entry)
            if os.path.isfile(p) and entry != ".cargo-lock":
                os.remove(p)
                removed += 1
        shutil.rmtree(os.path.join(prof, "incremental"), ignore_errors=True)
    left = [e for prof in profiles for e in os.listdir(os.path.join(prof, ".fingerprint")) if by_name.match(e)]
    if left:
        sys.exit(f"rimed-deps: member fingerprints survived the prune: {left}")
    kept = sum(len(os.listdir(os.path.join(prof, ".fingerprint"))) for prof in profiles)
    print(f"rimed-deps: removed {removed} member artifacts; {kept} third-party units kept in {len(profiles)} profiles")


def recompiled(target, metadata, marker):
    names, _ = members(metadata)
    by_name = re.compile(r"^(?:%s)-[0-9a-f]{16}$" % "|".join(map(re.escape, sorted(names))))
    since = os.stat(marker).st_mtime_ns
    built, reused = [], 0
    for prof in profile_dirs(target):
        fp = os.path.join(prof, ".fingerprint")
        for entry in sorted(os.listdir(fp)):
            if by_name.match(entry):
                continue
            newest = max(os.stat(os.path.join(fp, entry, f)).st_mtime_ns for f in os.listdir(os.path.join(fp, entry)) or ["."])
            if newest > since:
                built.append(entry)
            else:
                reused += 1
    print(f"rimed-deps: {reused} third-party units reused, {len(built)} compiled here"
          + (": " + " ".join(built) if built else ""))


if __name__ == "__main__":
    cmd, args = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else ("", [])
    if cmd == "dummy" and len(args) == 1:
        dummy(*args)
    elif cmd == "prune" and len(args) == 2:
        prune(*args)
    elif cmd == "recompiled" and len(args) == 3:
        recompiled(*args)
    else:
        sys.exit(__doc__)
