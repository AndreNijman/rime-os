#!/usr/bin/env python3
"""The P1 gate's judge (docs/nixos/p2-gates.md): `rime pkg translate` must report EVERY package record under a root.

It counts the records itself, from the filesystem, by the same source list the translator documents
(rimed/rime/src/nixos/translate.rs), and compares them with the report's rows as a multiset of (source, item): a
record with no row is "unreported", a row with no record is "invented". Both fail. It also fails on a row with no
outcome or no reason.

    pkg-translate-check.py --rime BIN --catalogue FILE --root DIR [--expect JSON] [--report OUT]

--expect is {"<item>": "<outcome>", ...} for the outcomes a fixture pins. Nothing under --root is written.
"""
import argparse
import collections
import json
import os
import subprocess
import sys
import tomllib

OUTCOMES = {"kept", "feature", "pending", "flatpak", "lookup", "carried", "dropped", "refused"}


def ls(path):
    try:
        return sorted(os.listdir(path))
    except (FileNotFoundError, NotADirectoryError):
        return []


def read_list(path):
    try:
        with open(path) as f:
            lines = [l.strip() for l in f]
    except FileNotFoundError:
        return []
    return [l for l in lines if l and not l.startswith("#")]


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
    for key in ["local_files", "deb_files", "vendor_verified", "unsigned_accepted", "github_sources"]:
        v = state.get(key, [])
        for n in (sorted(v) if isinstance(v, dict) else v):
            if n not in bare:
                want.append(("state.json", n))

    want += [("pkg-local", f) for f in ls(os.path.join(pkg, "local"))]
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

    fail = False
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
