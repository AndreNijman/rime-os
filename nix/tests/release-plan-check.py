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

# 2. The variants: one per set of offered features, named as rimed names them, carrying exactly their features.
expected = sorted(inp["expectedVariants"])
check(len(expected) == 2 ** len(inp["offered"]), f"{len(expected)} variants expected for features {inp['offered']}")
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
    for f in ("gaming", "virtualization"):
        check(carried[f] == (f in feats), f"variant {name}: rime.{f}.enable = {carried[f]}")
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
    # The gate needs room for a whole second system on top of the installed one; a floor below that strands updates.
    check(store >= on_disk + estimate, f"floor {name} holds a full rebuild under the gate's own arithmetic")
for name in expected:
    check(name in fl["published"], f"variant {name} has a published floor")
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
