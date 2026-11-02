#!/usr/bin/env python3
"""The P1 gate's judge (docs/nixos/p2-gates.md): `rime pkg translate` must report EVERY package record under a root.

It counts the records itself, from the filesystem, and compares them with the report's rows as a multiset of
(source, item): a record with no row is "unreported", a row with no record is "invented". Both fail. It also fails on
a row with no outcome or no reason.

What counts as a record does not come from the translator. It comes from the bootc package engine itself
(--engine, files/system/libexec/rime-pkg), and a sweep of the tree:

  - every path rime-pkg names under its PKG_ROOT (/var/lib/rime/pkg) must be in PKG_ENTRIES below, either counted as
    records or set aside with the reason it is not one. A name rime-pkg starts using that this table does not know
    fails the judge, before any tree is read;
  - every entry actually present under PKG_ROOT in the tree must be one of those names (or a temporary name rime-pkg
    derives from one). Anything else is UNCLASSIFIED and fails: a record no source list knows cannot hide;
  - every key rime-pkg's state.json writer emits, and every key in the tree's state.json, must be in STATE_KEYS;
  - every file in rime-pkg's DEB_DIR must carry one of the suffixes rime-pkg writes there;
  - a `.omitted` marker (a D2 snapshot's "contents left out" note) in any directory the judge reads fails: a
    snapshot directory with its contents left out cannot be judged, and the marker is not a record of the machine.

    pkg-translate-check.py --rime BIN --catalogue FILE --engine RIME-PKG --root DIR [--expect JSON] [--report OUT]

--expect is {"<item>": "<outcome>", ...} for the outcomes a fixture pins. Nothing under --root is written.
"""
import argparse
import collections
import fnmatch
import json
import os
import re
import subprocess
import sys
import tomllib

OUTCOMES = {"kept", "feature", "pending", "flatpak", "lookup", "carried", "dropped", "refused"}

# Every name rime-pkg uses directly under PKG_ROOT: None = counted as records by records() below; a string = why it
# holds no package record (it is still allowed to be there).
PKG_ENTRIES = {
    "requested": None,
    "state.json": None,
    "local": None,
    "deb": None,
    "etc": None,
    "etc.list": None,
    "var.list": None,
    "rollback": None,
    "copr-keys": None,
    "copr-keyring": "the rpm keyring rime-pkg rebuilds from copr-keys/, which are reported owner by owner",
    "work": "build staging (vendor.*/github.* downloads in flight)",
    ".lock": "the engine's lock file",
    "rpm-cache": "downloaded RPMs; their packages are reported by the names that requested them",
}
# Temporary names rime-pkg derives from those (mktemp beside the real one).
PKG_TEMP = {
    "work.??????": "a build's staging directory (rime-pkg sweeps leaked ones)",
    "copr-keyring.new.*": "a COPR keyring being rebuilt",
    "copr-keyring.old.*": "a COPR keyring being replaced",
    "state.json.tmp": "state.json being written",
}
# Every key of rime-pkg's state.json: None = names reported by records(); a string = why it names no package.
STATE_KEYS = {
    "requested": None,
    "local_files": None,
    "deb_files": None,
    "deb_depends": None,
    "unsigned_accepted": None,
    "vendor_verified": None,
    "github_sources": None,
    "resolved": "the RPM transaction resolved from the requested names, which are reported",
    "image_twins": "the image's x86_64 builds of the i686 packages resolved, about the image",
    "os_id": "the OS the extension was built for",
    "os_version_id": "the OS version the extension was built for",
    "built": "when the extension was built",
    "image": "the extension image's path",
    "image_sha256": "the extension image's hash",
    "image_bytes": "the extension image's size",
    "pkg_compat_level": "the engine's record format",
}

MARKERS = set()


def ls(path):
    try:
        names = sorted(os.listdir(path))
    except (FileNotFoundError, NotADirectoryError):
        return []
    if ".omitted" in names:
        MARKERS.add(path)
    return [n for n in names if n != ".omitted"]


def engine_facts(engine):
    """What rime-pkg itself says: the names it uses under PKG_ROOT, its state.json keys, its DEB_DIR suffixes."""
    with open(engine) as f:
        src = f.read()
    code = "\n".join(l for l in src.splitlines() if not l.lstrip().startswith("#"))
    m = re.search(r"^readonly PKG_ROOT=(\S+)$", code, re.M)
    assert m and m.group(1) == "/var/lib/rime/pkg", "rime-pkg's PKG_ROOT moved: the judge reads /var/lib/rime/pkg"
    consts = dict(re.findall(r'^readonly (\w+)="\$\{PKG_ROOT\}/([^"/$]+)"', code, re.M))
    names = set(consts.values())
    names |= set(re.findall(r"\$\{PKG_ROOT\}/([A-Za-z0-9._-]+)", code))
    names |= set(re.findall(r"\$PKG_ROOT/([A-Za-z0-9._-]+)", code))
    w = re.search(r"'\{(requested:\$requested[^']*)\}' > \"\$\{STATE\}\.tmp\"", code, re.S)
    assert w, "rime-pkg's state.json writer was not found"
    keys = set(re.findall(r"(\w+):\$", w.group(1)))
    # deb_cache() and friends: printf '%s/%s.deb' "$DEB_DIR" ...; the .depends writer: "${DEB_DIR}/${name}.depends".
    deb = {s for l in code.splitlines() if "DEB_DIR" in l for s in re.findall(r"(?:%s|\})\.([a-z]+)['\"]", l)}
    return consts, names, keys, deb


def read_list(path):
    try:
        with open(path) as f:
            lines = [l.strip() for l in f]
    except FileNotFoundError:
        return []
    return [l for l in lines if l and not l.startswith("#")]


def sweep(root, engine):
    """The independent part: what rime-pkg names, against the judge's tables and the tree. Returns failures."""
    bad = []
    consts, names, keys, deb_suffixes = engine_facts(engine)
    for n in sorted(names - PKG_ENTRIES.keys()):
        bad.append(f"rime-pkg uses {n!r} under /var/lib/rime/pkg and the judge does not know whether it holds records")
    for k in sorted(keys - STATE_KEYS.keys()):
        bad.append(f"rime-pkg writes state.json key {k!r} and the judge does not know whether it names packages")
    if not deb_suffixes:
        bad.append("rime-pkg's DEB_DIR file suffixes were not found")
    pkg = os.path.join(root, "var/lib/rime/pkg")
    for n in ls(pkg):
        if n in PKG_ENTRIES or any(fnmatch.fnmatchcase(n, p) for p in PKG_TEMP):
            continue
        bad.append(f"UNCLASSIFIED /var/lib/rime/pkg/{n}: no name rime-pkg uses there")
    try:
        with open(os.path.join(pkg, "state.json")) as f:
            state = json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        state = {}
    for k in sorted(state.keys() - STATE_KEYS.keys()):
        bad.append(f"UNCLASSIFIED state.json key {k!r}")
    for f in ls(os.path.join(pkg, "deb")):
        if f.rsplit(".", 1)[-1] not in deb_suffixes:
            bad.append(f"UNCLASSIFIED /var/lib/rime/pkg/deb/{f}: rime-pkg writes only {sorted(deb_suffixes)} there")
    return bad, consts, deb_suffixes


def records(root):
    """Every (source, item) the translator must report under root."""
    j = lambda p: os.path.join(root, p.lstrip("/"))
    want = []
    pkg = j("/var/lib/rime/pkg")

    requested = []
    for l in read_list(os.path.join(pkg, "requested")):
        if l not in requested:
            requested.append(l)
    want += [("requested", l) for l in requested]
    try:
        with open(os.path.join(pkg, "state.json")) as f:
            state = json.load(f)
    except FileNotFoundError:
        state = {}
    except json.JSONDecodeError:
        state = {}
        want.append(("state.json", "state.json"))
    seen = list(requested)
    for l in state.get("requested", []):
        if l not in seen:
            seen.append(l)
            want.append(("state.json", l))
    bare = {l.split(":", 1)[1] if ":" in l else l for l in seen}
    for key in [k for k, why in STATE_KEYS.items() if why is None and k != "requested"]:
        v = state.get(key, [])
        for n in (sorted(v) if isinstance(v, dict) else v):
            if n not in bare:
                want.append(("state.json", n))

    want += [("pkg-local", f) for f in ls(os.path.join(pkg, "local"))]
    want += [("pkg-deb", f"deb/{s}") for s in sorted({f.rsplit(".", 1)[0] for f in ls(os.path.join(pkg, "deb"))})]
    if os.path.exists(os.path.join(pkg, "etc")):
        want.append(("etc-save", "/var/lib/rime/pkg/etc"))
    want += [("copr-keys", f) for f in ls(os.path.join(pkg, "copr-keys"))]
    want += [("repos", f) for f in ls(j("/etc/yum.repos.d")) if f.endswith(".repo")]
    for name, abs_ in [("etc-list", "/var/lib/rime/pkg/etc.list"), ("var-list", "/var/lib/rime/pkg/var.list")]:
        if os.path.exists(j(abs_)):
            want.append((name, abs_))
    if os.path.exists(os.path.join(pkg, "rollback")):
        want.append(("pkg-rollback", "/var/lib/rime/pkg/rollback"))

    ai = ls(j("/var/lib/rime/appimage"))
    images = {f[: -len(".appimage")] for f in ai if f.lower().endswith(".appimage")}
    for f in ai:
        low = f.lower()
        if low.endswith(".appimage"):
            want.append(("appimage", f))
        elif low.endswith((".trust", ".files", ".json")):
            if f.rsplit(".", 1)[0].lower() not in {i.lower() for i in images}:
                want.append(("appimage", f))
        else:
            want.append(("appimage", f))

    want += [("system-flatpak", i) for i in ls(j("/var/lib/flatpak/app"))]
    want += [("extensions", f) for f in ls(j("/var/lib/extensions"))]
    for n in ls(j("/var/usrlocal/lib")):
        if n == "rime-appimage":
            want += [("usrlocal", f"rime-appimage/{p}") for p in ls(j(f"/var/usrlocal/lib/{n}")) if p not in images]
        else:
            want.append(("usrlocal", n))

    for base in ["/var/home", "/home"]:
        b = j(base)
        if not os.path.exists(b) or os.path.islink(b):
            continue
        for user in ls(b):
            h = os.path.join(b, user)
            nm = os.path.join(h, ".local/lib/node_modules")
            for n in ls(nm):
                if n.startswith("@"):
                    want += [("user-npm", f"{user}: npm {n}/{s}") for s in ls(os.path.join(nm, n))]
                elif not n.startswith("."):
                    want.append(("user-npm", f"{user}: npm {n}"))
            want += [("user-opt", f"{user}: ~/.local/opt/{n}") for n in ls(os.path.join(h, ".local/opt"))]
            want += [("user-flatpak", f"{user}: {i}") for i in ls(os.path.join(h, ".local/share/flatpak/app"))]
            want += [("capsule", f"{user}: {f}") for f in ls(os.path.join(h, ".local/share/rime/env")) if f.endswith(".json")]
            bp = os.path.join(h, ".config/rime/blueprint.toml")
            if os.path.exists(bp):
                with open(bp, "rb") as f:
                    try:
                        t = tomllib.load(f)
                    except tomllib.TOMLDecodeError:
                        want.append(("blueprint", f"{user}: blueprint.toml"))
                        continue
                for section, v in t.items():
                    if section == "apps":
                        want += [("blueprint", n) for n in v.get("install", [])]
                    else:
                        want.append(("blueprint", f"{user}: [{section}]"))
    return want


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rime", required=True)
    ap.add_argument("--catalogue", required=True)
    ap.add_argument("--engine", required=True, help="rime-pkg, whose own names define what a record is")
    ap.add_argument("--root", required=True)
    ap.add_argument("--expect")
    ap.add_argument("--report")
    a = ap.parse_args()

    out = subprocess.run(
        [a.rime, "pkg", "translate", "--root", a.root, "--catalogue", a.catalogue, "--json"],
        check=True, capture_output=True, text=True,
    ).stdout
    report = json.loads(out)
    if a.report:
        with open(a.report, "w") as f:
            f.write(out)
    rows = report["rows"]

    unclassified, consts, deb_suffixes = sweep(a.root, a.engine)
    want = collections.Counter(records(a.root))
    got = collections.Counter((r["source"], r["item"]) for r in rows)
    unreported = want - got
    invented = got - want
    bad = [r for r in rows if r["outcome"] not in OUTCOMES or not r["detail"].strip()]

    for s in report["sources"]:
        print(f"source {s['name']:<15} {s['path']:<34} {'read' if s['present'] else 'absent'}")
    for r in rows:
        print(f"  {r['outcome']:<8} {r['source']:<14} {r['item']}: {r['detail']}")
    by = collections.Counter(r["outcome"] for r in rows)
    print(f"records counted from the tree: {sum(want.values())}; rows reported: {len(rows)}; "
          + ", ".join(f"{n} {o}" for o, n in sorted(by.items())))
    print("requested on NixOS:", " ".join(report["requested_after"]) or "(nothing)")
    print("features:", json.dumps(report["features"]))
    print("flatpaks to install:", " ".join(report["flatpak_install"]) or "(none)")

    print(f"rime-pkg names under /var/lib/rime/pkg: {len(PKG_ENTRIES)} classified ({len(consts)} readonly); "
          f"state.json keys: {len(STATE_KEYS)} classified; deb/ suffixes: {' '.join(sorted(deb_suffixes))}")
    fail = False
    for b in unclassified:
        print(b)
        fail = True
    for d in sorted(MARKERS):
        print(f"SNAPSHOT MARKER {os.path.relpath(d, a.root)}/.omitted: this directory's contents were left out; resolve it "
              "(a sidecar, or an empty directory) before judging")
        fail = True
    for (s, i), n in sorted(unreported.items()):
        print(f"UNREPORTED {s}: {i} (x{n})")
        fail = True
    for (s, i), n in sorted(invented.items()):
        print(f"INVENTED {s}: {i} (x{n})")
        fail = True
    for r in bad:
        print(f"NO OUTCOME OR REASON: {r}")
        fail = True
    if a.expect:
        for item, outcome in json.loads(a.expect).items():
            hits = [r["outcome"] for r in rows if r["item"] == item]
            if hits != [outcome]:
                print(f"EXPECTED {item} -> {outcome}, got {hits}")
                fail = True
    if fail:
        sys.exit(1)
    print(f"PASS: all {sum(want.values())} records reported, none unreported, none invented")


main()
