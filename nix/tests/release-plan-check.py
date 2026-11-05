"""checks.release-plan: see nix/tests/release-plan.nix. argv: plan.json input.json update.toml."""
import json
import sys
import tomllib

plan_path, input_path, toml_path = sys.argv[1:4]
plan = json.load(open(plan_path))
inp = json.load(open(input_path))
update = tomllib.load(open(toml_path, "rb"))

failures = []
passed = 0


def check(ok, what):
    global passed
    if ok:
        passed += 1
        print(f"ok: {what}")
    else:
        failures.append(what)
        print(f"FAIL: {what}")


# 1. The catalogue: every buildable class 2 entry has a build, every unbuilt one says why, nothing extra.
entries = inp["entries"]
buildable = sorted(n for n, e in entries.items() if e["kind"] == "nix" and e.get("attrs"))
unbuilt = sorted(n for n, e in entries.items() if e["kind"] == "nix" and not e.get("attrs"))
built = {c["name"]: c for c in plan.get("catalogue", [])}
check(len(buildable) > 0, f"the catalogue has buildable class 2 entries ({len(buildable)})")
for name in buildable:
    c = built.get(name)
    check(c is not None, f"catalogue app {name} has a build in the release plan")
    if c is not None:
        check(c["out"].startswith("/nix/store/") and len(c["out"]) > 44, f"{name}'s build is a store path ({c['out']})")
        check(bool(c.get("version")), f"{name}'s build has a version ({c.get('version')!r})")
for name in unbuilt:
    check(bool(entries[name].get("pending")), f"unbuilt catalogue app {name} says why (pending)")
    check(name not in built, f"unbuilt catalogue app {name} has no build in the plan")
extra = sorted(set(built) - set(buildable))
check(not extra, f"the plan builds nothing the catalogue does not offer (extra: {extra})")
names = [c["name"] for c in plan.get("catalogue", [])]
check(len(names) == len(set(names)), "each app is named once in the plan")

# 4 (here, next to the catalogue). Redistribution (R14): an app whose licence does not allow redistribution never has
# its output in the plan; it is device-built (its derivation) and listed in nix/redistributable.nix with a reason.
rd = inp["redist"]
banned = set(plan.get("non_redistributable", []))
check(all(p.startswith("/nix/store/") for p in banned), "non_redistributable names store paths only")
def redistributable(l):
    """A nixpkgs licence (simple, compound AND/OR, or WITH an exception) allows redistribution."""
    if not isinstance(l, dict):
        return False
    kind = l.get("licenseType", "simple")
    if kind == "compound":
        parts = [redistributable(x) for x in l.get("licenses", [])]
        return bool(parts) and (any(parts) if l.get("operator") == "OR" else all(parts))
    if kind == "exception":
        return redistributable(l.get("license"))
    return l.get("redistributable") is True


def lname(l):
    if not isinstance(l, dict):
        return repr(l)
    if "licenses" in l:
        return "(" + f" {l.get('operator')} ".join(lname(x) for x in l["licenses"]) + ")"
    if "license" in l:
        return f"{lname(l['license'])} WITH {lname(l.get('exception'))}"
    return l.get("shortName") or l.get("spdxId") or l.get("fullName") or "?"


for name in buildable:
    lic = rd["licences"][name]
    allowed = all(l["licences"] and all(redistributable(x) for x in l["licences"]) for l in lic)
    c = built.get(name) or {}
    device = c.get("build") == "device"
    print(f"  {name}: licences {[(l['attr'], [lname(x) for x in l['licences']]) for l in lic]} -> "
          f"{'redistributable' if allowed else 'NOT redistributable'}; plan: {'device' if device else 'cache'} build")
    if device:
        check(c.get("out") == rd["drvs"][name] and c["out"].endswith(".drv"),
              f"device-built {name}: the plan names its derivation {rd['drvs'][name]} ({c.get('out')})")
        check(bool(rd["listed"].get(name)), f"device-built {name} is listed in nix/redistributable.nix with a reason")
        for o in rd["outs"][name]:
            check(o in banned, f"device-built {name}: its output {o} is in non_redistributable")
    else:
        check(allowed, f"{name} is published from the cache only if its licence allows redistribution")
        check(name not in rd["listed"], f"published {name} is not listed as device-built in nix/redistributable.nix")
        check(not c.get("out", "").endswith(".drv"), f"published {name} names its built output, not a derivation")
        for o in rd["outs"][name]:
            check(o not in banned, f"published {name}'s output {o} is not in non_redistributable")
for name in rd["listed"]:
    check(name in buildable, f"nix/redistributable.nix lists {name}, a buildable catalogue app")

# 2. The variants: one per set of offered features, named as rimed names them, carrying exactly their features.
expected = sorted(inp["expectedVariants"])
# At most one feature per group (desktops.md D3): 2^(ungrouped) x (members + 1) per group.
groups = {}
for f in inp["offered"]:
    groups.setdefault(inp["groups"].get(f), []).append(f)
want_count = 2 ** len(groups.pop(None, []))
for members in groups.values():
    want_count *= len(members) + 1
check(len(expected) == want_count, f"{len(expected)} variants expected for features {inp['offered']} (groups {groups})")
for name in expected:
    feats = name.split("-")[1:]
    for g, members in groups.items():
        check(len([f for f in feats if f in members]) <= 1, f"variant {name} holds at most one {g} feature")
check(sorted(plan["variants"]) == expected, f"the plan's variants {sorted(plan['variants'])} == {expected}")
for name in expected:
    v = plan["variants"].get(name)
    if v is None:
        continue
    check(v.get("publishable") is True, f"variant {name} is publishable")
    check(v["toplevel"].startswith("/nix/store/") and "-nixos-system-rime-" + name + "-" in v["toplevel"],
          f"variant {name}'s toplevel is its own system ({v['toplevel']})")
    feats = set(name.split("-")[1:])
    carried = inp["carried"][name]
    for f in [k for k in carried if k != "passwordAccounts"]:
        check(carried[f] == (f in feats), f"variant {name}: rime.{f}.enable = {carried[f]}")
    check(carried["passwordAccounts"] == [], f"variant {name} carries no account with a password ({carried['passwordAccounts']})")
tops = [v["toplevel"] for v in plan["variants"].values()]
check(len(tops) == len(set(tops)), "every variant is a different system")

# 3. The floors: arithmetic, coverage, and what the systems render.
fl = inp["floors"]
GB = fl["GB"]
for name, m in fl["measured"].items():
    unpacked, on_disk = m["unpacked"], m["onDisk"]
    estimate = -(-unpacked * fl["ratioPermille"] // 1000)
    store = on_disk + estimate + fl["reserve"] + fl["home"]
    disk = store + fl["espBytes"] + fl["gptBytes"]
    disk_gb = -(-disk // GB)
    comp = fl["computed"][name]
    check(comp["storeBytes"] == store, f"floor {name}: store bytes {comp['storeBytes']} == {store}")
    check(comp["diskGB"] == disk_gb, f"floor {name}: disk {comp['diskGB']} GB == {disk_gb} GB")
    check(fl["published"].get(name) == disk_gb, f"floor {name}: published {fl['published'].get(name)} GB == {disk_gb} GB")
    part_gb = -(-store // GB)
    check(comp["partitionGB"] == part_gb, f"floor {name}: partition {comp['partitionGB']} GB == {part_gb} GB")
    check(fl["publishedPartition"].get(name) == part_gb,
          f"floor {name}: published partition {fl['publishedPartition'].get(name)} GB == {part_gb} GB")
    # The gate needs room for a whole second system on top of the installed one; a floor below that strands updates.
    check(store >= on_disk + estimate, f"floor {name} holds a full rebuild under the gate's own arithmetic")
# A desktop variant's floor is its base's measured system plus the desktop's measured delta (desktops.md D14).
for d, delta in fl["desktopDeltas"].items():
    for base, m in fl["base"].items():
        name = "-".join(["plain"] + sorted(base.split("-")[1:] + [d]))
        got = fl["measured"].get(name)
        want = {"unpacked": m["unpacked"] + delta, "onDisk": m["onDisk"] + -(-delta * fl["ratioPermille"] // 1000)}
        check(got == want, f"desktop floor {name} = {base} + {d}'s delta ({got} == {want})")
for name in expected:
    check(name in fl["measured"], f"variant {name} has a floor of its own (not floor_for's fallback)")
    check(name in fl["published"], f"variant {name} has a published floor")
    check(name in fl["publishedPartition"], f"variant {name} has a published partition floor")
check(fl["ratioPermille"] / 1000 >= 0.524, f"the gate's ratio {fl['ratioPermille'] / 1000} is no lower than the worst measured (0.524)")
check(fl["espBytes"] == 2048 * 1024 * 1024, "the floors count the installer's 2 GiB ESP")

rendered = inp["rendered"]["catalogue"].get("floors", {})
for name in expected:
    want = {"store_bytes": fl["computed"][name]["storeBytes"], "disk_gb": fl["computed"][name]["diskGB"]}
    check(rendered.get(name) == want, f"/etc/rime/catalogue.json floor {name} == {want}")
check(abs(update.get("on_disk_ratio", 1.0) - fl["ratioPermille"] / 1000) < 1e-9,
      f"/etc/rime/update.toml on_disk_ratio {update.get('on_disk_ratio')} == {fl['ratioPermille'] / 1000}")

print(f"release-plan: {passed} passed, {len(failures)} failed")
if failures:
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
