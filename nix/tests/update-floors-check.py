"""checks.update-floors (nix/tests/update-floors.nix): the floors of nix/release/floors.nix against a VM install of
every release variant. argv: summary.tsv (the measurement), floors.json (floors.nix as data). Prints every check;
exits 1 when any fails."""

import json
import sys

summary, floors_json = sys.argv[1], sys.argv[2]
fl = json.load(open(floors_json))
rows = [line.rstrip("\n").split("\t") for line in open(summary) if line.strip()]
head, rows = rows[0], rows[1:]
m = {(r[head.index("variant")], r[head.index("fs")]): dict(zip(head, r)) for r in rows}

GB = fl["GB"]
ratio = fl["ratioPermille"]
failed = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failed.append(what)


def ceil_div(a, b):
    return (a + b - 1) // b


variants = sorted({v for v, _ in m})
check(variants == sorted(fl["published"]), f"every published variant was measured: {variants}")

for v in variants:
    for fs in ("btrfs", "ext4"):
        check((v, fs) in m, f"{v}: measured on {fs}")
    b, e = m[(v, "btrfs")], m[(v, "ext4")]
    nar = int(b["nar_bytes"])
    check(nar == int(e["nar_bytes"]) and b["toplevel"] == e["toplevel"], f"{v}: both installs are the same closure")
    on_disk = int(b["df_used_optimised"])
    ext4 = int(e["df_used_optimised"])

    # 1. The published floors hold for the system as built (floors.nix arithmetic, measured inputs).
    store = on_disk + ceil_div(nar * ratio, 1000) + fl["reserve"] + fl["home"]
    disk = store + fl["espBytes"] + fl["gptBytes"]
    check(ceil_div(disk, GB) <= fl["published"][v],
          f"{v}: whole disk needs {disk:,} B = {ceil_div(disk, GB)} GB <= published {fl['published'][v]} GB")
    check(ceil_div(store, GB) <= fl["publishedPartition"][v],
          f"{v}: root partition needs {store:,} B = {ceil_div(store, GB)} GB <= published "
          f"{fl['publishedPartition'][v]} GB")

    # 2. The gate's compressed estimate errs high on the real system.
    check(on_disk * 1000 <= nar * ratio,
          f"{v}: btrfs compress-force=zstd:3 holds {on_disk:,} B for {nar:,} B of NAR "
          f"(ratio {on_disk / nar:.3f}) <= the gate's {ratio / 1000:.3f}")

    # 3. On ext4 the gate counts the NAR size (ratio 1.0) plus its 1 GB reserve. ext4 really needs a few percent more
    #    than the NAR size (4 KiB blocks, directories; measured 3.6-4.5 %, closure.md §12), so what holds the promise
    #    is the reserve: the whole closure on ext4 must fit in what the gate asks for it, NAR + reserve.
    check(ext4 <= nar + fl["reserve"],
          f"{v}: ext4 holds {ext4:,} B for {nar:,} B of NAR (ratio {ext4 / nar:.3f}, {ext4 - nar:+,} B) <= NAR + the "
          f"{fl['reserve']:,} B reserve")

    # 4. floors.nix records this measurement (within 3 %), so the documents and the rendered floors are current.
    rec = fl["measured"][v]
    for key, got in (("onDisk", on_disk), ("unpacked", nar)):
        want = rec[key]
        check(abs(want - got) * 100 <= 3 * got,
              f"{v}: floors.nix measured.{key} {want:,} B is within 3 % of the measured {got:,} B "
              f"({(want - got) / got * 100:+.2f} %)")

if failed:
    print(f"update-floors: {len(failed)} check(s) failed")
    sys.exit(1)
print("update-floors: every published floor holds for the measured install")
