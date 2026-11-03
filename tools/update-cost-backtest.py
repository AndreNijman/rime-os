#!/usr/bin/env python3
"""Backtest what `bootc upgrade` really downloads, release by release.

bootc (ostree-ext) fetches an image's layers whole and skips a layer only when
it already holds a blob with that exact digest. So the bytes a machine
downloads to go from release A to release B are the compressed sizes of the
layers in B's manifest whose digests are not in A's manifest. That is what
`bootc upgrade --check` prints as "Total new layers", and what this script sums.

Two steps, so the numbers reproduce without network access:

  fetch   reads the registry and the build history and writes
          <data>/ledger.tsv   one row per signed OS image: when, which run,
                              which branch, whether core was rebuilt, published
          <data>/layers.tsv   every OS image's layers: digest and size
          Needs skopeo and gh, and reads nothing but the registry and the
          GitHub API. It never touches the machine it runs on.

  report  reads those two files and prints the tables docs/update-cost.md
          quotes: every published transition, the cadence, and 30-day bytes at
          today's cadence and at a monthly core cadence, before and after the
          rechunked layout.

Where the images come from: every image build-image publishes is signed, and
the signature is stored as the tag `sha256-<digest>.sig`. Those tags are the
complete list of signed images, including the weekly cron's reissues of an
existing release, which the release records on rimeos.com do not list. Tags
like `rime-<sha>` are NOT used: a rebuild of the same commit moves them.

An image counts as published when the build-image run that produced it ran on
`main` and its `image` job succeeded. Branch builds sign their images too and
are kept in the ledger (published = n), because the rechunked layout has only
one published release so far and the model for it comes from branch pairs.

Usage:
  python3 -I tools/update-cost-backtest.py fetch  --data docs/update-cost --since 2026-09-29
  python3 -I tools/update-cost-backtest.py report --data docs/update-cost
"""

from __future__ import annotations

import argparse
import concurrent.futures as cf
import csv
import datetime as dt
import json
import os
import shutil
import statistics
import subprocess
import sys

IMAGE = "ghcr.io/andrenijman/rime-os"
REPO = "AndreNijman/rime-os"
WORKFLOW = "build-image.yml"
# The first release built with the rechunked layout (PR #123). The transition
# INTO it re-downloads the whole image once and is reported apart.
RECHUNK_FROM = "2026.11.01"
# The rechunked layout as measured on real images (see docs/update-cost.md).
# Each pair is OLD NEW digest prefixes, both in layers.tsv.
MODEL_NONCORE = ("9d1c9ca57499", "1e11040aee63")  # same core, base + image rebuilt
MODEL_CORE = ("39a376d65646", "5ae90c64ba81")     # core, base and image rebuilt
# PR #123's own figure for a core rebuild WITH real package changes
# (e5ff68d1 -> b61d46c1, rechunked locally; those images were never pushed).
# The registry pair above rebuilt core from the same core inputs (only base and
# image-tier files changed between its commits), so it is a floor.
CORE_123 = 1.74e9
GB = 1e9

LEDGER_COLS = [
    "created", "digest", "release_id", "revision", "run_id", "event",
    "branch", "core_rebuilt", "published", "layers", "bytes", "base_digest",
]


# ── fetch ────────────────────────────────────────────────────────────────────

def run(cmd: list[str]) -> str:
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def parse_time(s: str) -> dt.datetime:
    """RFC 3339 in UTC, as skopeo and the GitHub API write it (fractions dropped)."""
    return dt.datetime.strptime(s[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=dt.timezone.utc)


def signed_digests() -> list[str]:
    tags = json.loads(run(["skopeo", "list-tags", f"docker://{IMAGE}"]))["Tags"]
    out = set()
    for t in tags:
        if t.startswith("sha256-") and t.endswith(".sig") and len(t) == 7 + 64 + 4:
            out.add("sha256:" + t[7:71])
    return sorted(out)


def inspect(digest: str) -> tuple[str, dict, dict]:
    ref = f"docker://{IMAGE}@{digest}"
    manifest = json.loads(run(["skopeo", "inspect", "--raw", ref]))
    config = json.loads(run(["skopeo", "inspect", "--config", ref]))
    return digest, manifest, config


def runs_since(gh: str, since: str) -> list[dict]:
    raw = run([gh, "run", "list", "-R", REPO, "-w", WORKFLOW, "-L", "1000",
               "--created", f">={since}", "--json",
               "databaseId,event,headSha,headBranch,conclusion,createdAt,updatedAt"])
    return json.loads(raw)


def jobs_of(gh: str, run_id: int) -> dict[str, dict]:
    raw = run([gh, "api", "--paginate", f"repos/{REPO}/actions/runs/{run_id}/jobs",
               "-q", ".jobs[] | {name, conclusion, started_at, completed_at}"])
    jobs = {}
    for line in raw.splitlines():
        if line.strip():
            j = json.loads(line)
            jobs[j["name"]] = j
    return jobs


SLACK = dt.timedelta(minutes=2)


def owner_of(img: dict, runs: list[dict], all_jobs: dict[int, dict[str, dict]]) -> dict | None:
    """The run of the same commit whose image job was running when the image's
    config was written (within SLACK), or None."""
    for r in runs:
        if r["headSha"] != img["revision"]:
            continue
        job = all_jobs[r["databaseId"]].get("image")
        if not job or not job.get("started_at") or not job.get("completed_at"):
            continue
        if parse_time(job["started_at"]) - SLACK <= img["created"] <= parse_time(job["completed_at"]) + SLACK:
            return r
    return None


def classify(run_: dict, jobs: dict[str, dict]) -> tuple[str, str]:
    """(core_rebuilt, published) for an image built by run_.

    Core was rebuilt only when the run's `core` job succeeded; a skipped core
    job means the image reused the previous core. Published = a `main` run
    whose `image` job succeeded (a branch run signs its image too, but never
    moves a tag a machine follows).
    """
    core = (jobs.get("core") or {}).get("conclusion")
    image_ok = (jobs.get("image") or {}).get("conclusion") == "success"
    return ("y" if core == "success" else "n",
            "y" if run_["headBranch"] == "main" and image_ok else "n")


def fetch(args: argparse.Namespace) -> int:
    for tool in ("skopeo", args.gh):
        if shutil.which(tool) is None and not os.path.exists(tool):
            print(f"fetch: {tool} not found", file=sys.stderr)
            return 2
    since = dt.datetime.fromisoformat(args.since + "T00:00:00+00:00")

    digests = signed_digests()
    with cf.ThreadPoolExecutor(max_workers=16) as pool:
        inspected = list(pool.map(inspect, digests))

    images = []
    for digest, manifest, config in inspected:
        labels = (config.get("config") or {}).get("Labels") or {}
        # Only the bootable OS image; core and base are its parents. Before the
        # rename (2026-09-28) the image tier wrote "apex" here.
        if labels.get("org.opencontainers.image.version") not in ("rime", "apex"):
            continue
        if "layers" not in manifest:
            continue
        created = parse_time(config["created"])
        if created < since:
            continue
        images.append({
            "digest": digest, "created": created, "manifest": manifest,
            "release_id": labels.get("org.rimeos.release.id", ""),
            "revision": labels.get("org.opencontainers.image.revision", ""),
            "base_digest": (manifest.get("annotations") or {}).get(
                "org.opencontainers.image.base.digest", ""),
        })

    runs = runs_since(args.gh, args.since)
    with cf.ThreadPoolExecutor(max_workers=8) as pool:
        all_jobs = dict(zip([r["databaseId"] for r in runs],
                            pool.map(lambda r: jobs_of(args.gh, r["databaseId"]), runs)))

    # An image belongs to the run of the same commit whose image job was
    # running when the image's config was written.
    rows, unmatched = [], []
    for img in sorted(images, key=lambda i: i["created"]):
        owner = owner_of(img, runs, all_jobs)
        if owner is None:
            unmatched.append(img)
            continue
        core_rebuilt, published = classify(owner, all_jobs[owner["databaseId"]])
        layers = img["manifest"]["layers"]
        rows.append({
            "created": img["created"].strftime("%Y-%m-%dT%H:%M:%SZ"),
            "digest": img["digest"],
            "release_id": img["release_id"],
            "revision": img["revision"],
            "run_id": owner["databaseId"],
            "event": owner["event"],
            "branch": owner["headBranch"],
            "core_rebuilt": core_rebuilt,
            "published": published,
            "layers": len(layers),
            "bytes": sum(l["size"] for l in layers),
            "base_digest": img["base_digest"],
        })

    # A main run whose image job succeeded must have produced a signed image.
    matched_runs = {r["run_id"] for r in rows}
    missing = [r for r in runs
               if r["headBranch"] == "main"
               and all_jobs[r["databaseId"]].get("image", {}).get("conclusion") == "success"
               and r["databaseId"] not in matched_runs]

    # ... and exactly one: two published images from one run would be counted
    # as two releases.
    per_run: dict[int, int] = {}
    for r in rows:
        if r["published"] == "y":
            per_run[r["run_id"]] = per_run.get(r["run_id"], 0) + 1
    doubled = sorted(k for k, n in per_run.items() if n > 1)

    os.makedirs(args.data, exist_ok=True)
    with open(os.path.join(args.data, "ledger.tsv"), "w", newline="") as f:
        f.write(f"# written by tools/update-cost-backtest.py fetch --since {args.since} "
                f"at {dt.datetime.now(dt.timezone.utc):%Y-%m-%dT%H:%M:%SZ}\n")
        w = csv.DictWriter(f, LEDGER_COLS, delimiter="\t", lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
    with open(os.path.join(args.data, "layers.tsv"), "w") as f:
        f.write("image\tindex\tlayer\tsize\n")
        by_digest = {i["digest"]: i for i in images}
        for r in rows:
            for n, l in enumerate(by_digest[r["digest"]]["manifest"]["layers"]):
                f.write(f"{r['digest']}\t{n}\t{l['digest']}\t{l['size']}\n")

    print(f"fetch: {len(rows)} OS images ({sum(r['published'] == 'y' for r in rows)} published), "
          f"{len(runs)} runs since {args.since}")
    for img in unmatched:
        print(f"fetch: WARNING no run found for {img['digest']} ({img['revision'][:9]}, "
              f"{img['created']:%FT%TZ})", file=sys.stderr)
    for r in missing:
        print(f"fetch: WARNING main run {r['databaseId']} succeeded but no signed image "
              f"matched it", file=sys.stderr)
    for k in doubled:
        print(f"fetch: WARNING main run {k} owns {per_run[k]} published images", file=sys.stderr)
    return 1 if missing or doubled else 0


# ── report ───────────────────────────────────────────────────────────────────

def load(data: str) -> tuple[list[dict], dict[str, dict[str, int]]]:
    with open(os.path.join(data, "ledger.tsv")) as f:
        lines = [l for l in f if not l.startswith("#")]
    ledger = list(csv.DictReader(lines, delimiter="\t"))
    layers: dict[str, dict[str, int]] = {}
    with open(os.path.join(data, "layers.tsv")) as f:
        for row in csv.DictReader(f, delimiter="\t"):
            layers.setdefault(row["image"], {})[row["layer"]] = int(row["size"])
    return ledger, layers


def delta(layers: dict[str, dict[str, int]], old: str, new: str) -> tuple[int, int]:
    """Bytes and layer count a machine holding OLD downloads to reach NEW."""
    have = layers[old]
    fresh = {d: s for d, s in layers[new].items() if d not in have}
    return sum(fresh.values()), len(fresh)


def find(layers: dict[str, dict[str, int]], prefix: str) -> str:
    hits = [d for d in layers if d.split(":")[-1].startswith(prefix)]
    if len(hits) != 1:
        raise SystemExit(f"report: model image {prefix} matches {len(hits)} images in layers.tsv")
    return hits[0]


def release_key(rid: str) -> tuple[int, ...]:
    return tuple(int(p) for p in rid.split(".")) if rid and rid[0].isdigit() else (0,)


def transitions(ledger: list[dict], layers: dict, rechunk_from: str) -> list[dict]:
    pub = [r for r in ledger if r["published"] == "y"]
    pub.sort(key=lambda r: r["created"])
    out = []
    for prev, cur in zip(pub, pub[1:]):
        b, n = delta(layers, prev["digest"], cur["digest"])
        rechunked_prev = release_key(prev["release_id"]) >= release_key(rechunk_from)
        rechunked_cur = release_key(cur["release_id"]) >= release_key(rechunk_from)
        if rechunked_cur and not rechunked_prev:
            kind = "rechunk-switch"
        elif cur["core_rebuilt"] == "y":
            kind = "core"
        else:
            kind = "non-core"
        out.append({
            "from": prev, "to": cur, "bytes": b, "new_layers": n,
            "of_layers": int(cur["layers"]), "kind": kind,
            "reissue": cur["release_id"] == prev["release_id"] and cur["release_id"] != "",
            "layout": "rechunked" if rechunked_cur and rechunked_prev else "tiered",
        })
    return out


def at_interval(ledger: list[dict], layers: dict, days: int, rechunk_from: str,
                core_b: float, non_b: float) -> tuple[int, int, float, float]:
    """A machine that updates once every `days`, to whatever is newest then.

    Walks the tiered (pre-rechunk) history. Returns the bytes it really
    downloaded, the number of updates, the days walked, and what the same
    updates would cost in the rechunked layout: core_b when any image in the
    step rebuilt core, non_b otherwise (the rechunked non-core layers are the
    same ones every time, so two non-core releases cost one).
    """
    pub = sorted((r for r in ledger if r["published"] == "y"
                  and release_key(r["release_id"]) < release_key(rechunk_from)),
                 key=lambda r: r["created"])
    t = parse_time(pub[0]["created"])
    end = parse_time(pub[-1]["created"])
    held = 0
    total = updates = 0
    modelled = 0.0
    while t < end:
        t += dt.timedelta(days=days)
        newest = max(i for i, r in enumerate(pub) if parse_time(r["created"]) <= t)
        if newest != held:
            total += delta(layers, pub[held]["digest"], pub[newest]["digest"])[0]
            core = any(r["core_rebuilt"] == "y" for r in pub[held + 1:newest + 1])
            modelled += core_b if core else non_b
            updates += 1
            held = newest
    return total, updates, (end - parse_time(pub[0]["created"])).total_seconds() / 86400, modelled


def report(args: argparse.Namespace) -> int:
    ledger, layers = load(args.data)
    trans = transitions(ledger, layers, args.rechunk_from)
    if not trans:
        print("report: fewer than two published images", file=sys.stderr)
        return 1
    pub = sorted((r for r in ledger if r["published"] == "y"), key=lambda r: r["created"])
    first, last = parse_time(pub[0]["created"]), parse_time(pub[-1]["created"])
    span = (last - first).total_seconds() / 86400
    win_start = last - dt.timedelta(days=30)
    p = print

    p("## Every published transition (a machine that installs every release)\n")
    p("| # | published | release | core rebuilt | kind | new layers | download |")
    p("|---|---|---|---|---|---|---|")
    for i, t in enumerate(trans, 1):
        rid = t["to"]["release_id"] or t["to"]["revision"][:9]
        if t["reissue"]:
            rid += " (reissue)"
        p(f"| {i} | {t['to']['created'][:16].replace('T', ' ')} | {rid} | {t['to']['core_rebuilt']} "
          f"| {t['kind']} | {t['new_layers']} of {t['of_layers']} | {t['bytes'] / GB:.3f} GB |")

    tiered = [t for t in trans if t["layout"] == "tiered" and t["kind"] != "rechunk-switch"]
    core_t = [t["bytes"] for t in tiered if t["kind"] == "core"]
    non_t = [t["bytes"] for t in tiered if t["kind"] == "non-core"]
    switch = [t for t in trans if t["kind"] == "rechunk-switch"]

    p("\n## Per release, tiered layout (before rechunking)\n")
    p("| kind | transitions | median | mean | min | max |")
    p("|---|---|---|---|---|---|")
    for name, xs in (("core rebuild", core_t), ("no core rebuild", non_t)):
        if xs:
            p(f"| {name} | {len(xs)} | {statistics.median(xs) / GB:.3f} GB | {statistics.mean(xs) / GB:.3f} GB "
              f"| {min(xs) / GB:.3f} GB | {max(xs) / GB:.3f} GB |")
    for t in switch:
        p(f"\nExcluded, once per machine: the switch to the rechunked layout, "
          f"{t['from']['release_id']} → {t['to']['release_id']}: {t['new_layers']} of {t['of_layers']} "
          f"layers, **{t['bytes'] / GB:.3f} GB**.")

    # Cadence over the whole window and over the last 30 days.
    def cadence(rows: list[dict]) -> tuple[int, int]:
        return len(rows), sum(r["core_rebuilt"] == "y" for r in rows)

    all_rel, all_core = cadence(pub[1:])
    last30 = [r for r in pub if parse_time(r["created"]) > win_start]
    r30, c30 = cadence(last30)
    p("\n## Cadence (published images, reissues included)\n")
    p("| window | days | updates (images after the first) | core rebuilds | per 30 days: updates | per 30 days: core |")
    p("|---|---|---|---|---|---|")
    p(f"| {pub[0]['created'][:10]} → {pub[-1]['created'][:10]} | {span:.1f} | {all_rel} | {all_core} "
      f"| {all_rel * 30 / span:.1f} | {all_core * 30 / span:.1f} |")
    p(f"| last 30 days ({win_start:%Y-%m-%d} → {last:%Y-%m-%d}) | 30.0 | {r30} | {c30} | {r30} | {c30} |")

    # The rechunked layout, from measured image pairs.
    nc_old, nc_new = find(layers, MODEL_NONCORE[0]), find(layers, MODEL_NONCORE[1])
    co_old, co_new = find(layers, MODEL_CORE[0]), find(layers, MODEL_CORE[1])
    nc_b, nc_n = delta(layers, nc_old, nc_new)
    co_b, co_n = delta(layers, co_old, co_new)

    # What machines really paid, over the tiered history (rechunk switch excluded).
    walked = (parse_time(tiered[-1]["to"]["created"]) - parse_time(tiered[0]["from"]["created"])).total_seconds() / 86400
    every = sum(t["bytes"] for t in tiered)
    every_m = sum(co_b if t["kind"] == "core" else nc_b for t in tiered)
    p("\n## What a machine really downloaded, tiered history (rechunk switch excluded)\n")
    p("The last column replays the same updates in the rechunked layout: the core pair's bytes when the "
      "step contains a core rebuild, the non-core pair's otherwise.\n")
    p("| updates | count | downloaded | per 30 days | same updates, rechunked, per 30 days |")
    p("|---|---|---|---|---|")
    p(f"| every release ({walked:.1f} days) | {len(tiered)} | {every / GB:.2f} GB "
      f"| **{every / GB * 30 / walked:.2f} GB** | {every_m / GB * 30 / walked:.2f} GB |")
    for days in (1, 7):
        b, n, walked, m = at_interval(ledger, layers, days, args.rechunk_from, co_b, nc_b)
        p(f"| once every {days} day{'s' if days > 1 else ''} ({walked:.1f} days) | {n} "
          f"| {b / GB:.2f} GB | **{b / GB * 30 / walked:.2f} GB** | {m / GB * 30 / walked:.2f} GB |")

    tier_core = statistics.median(core_t) if core_t else 0
    tier_non = statistics.median(non_t) if non_t else 0

    p("\n## The rechunked layout, measured on real image pairs\n")
    p("| pair | what was rebuilt | new layers | download |")
    p("|---|---|---|---|")
    p(f"| `{MODEL_NONCORE[0]}` → `{MODEL_NONCORE[1]}` | base + image, same core | "
      f"{nc_n} of {len(layers[nc_new])} | **{nc_b / GB:.3f} GB** |")
    p(f"| `{MODEL_CORE[0]}` → `{MODEL_CORE[1]}` | core + base + image | "
      f"{co_n} of {len(layers[co_new])} | **{co_b / GB:.3f} GB** |")

    def month(n_rel: float, n_core: float, core_b: float, non_b: float) -> float:
        return (n_core * core_b + (n_rel - n_core) * non_b) / GB

    rate_rel = all_rel * 30 / span
    rate_core = all_core * 30 / span
    p("\n## 30-day download for a machine that installs every release\n")
    p("Cadence = published images per 30 days. A core rebuild costs the core figure, every other "
      "release the non-core figure. \"Monthly core\" keeps the same number of releases and rebuilds "
      "core once.\n")
    p("| cadence | releases / 30 d | core / 30 d | tiered layout (medians above) "
      "| rechunked layout (pairs above) | rechunked, core = #123's 1.74 GB |")
    p("|---|---|---|---|---|---|")
    for name, rel, core in (
        (f"today, whole window ({span:.0f} days)", rate_rel, rate_core),
        ("today, last 30 days", r30, c30),
        ("monthly core, releases as whole window", rate_rel, 1.0),
        ("monthly core, releases as last 30 days", r30, 1.0),
    ):
        p(f"| {name} | {rel:.1f} | {core:.1f} | {month(rel, core, tier_core, tier_non):.2f} GB "
          f"| **{month(rel, core, co_b, nc_b):.2f} GB** | {month(rel, core, CORE_123, nc_b):.2f} GB |")
    return 0


# ── self-test ────────────────────────────────────────────────────────────────

def self_test() -> int:
    """delta(), transitions(), release_key() and fetch's classify()/owner_of()
    on fixtures with a known answer."""
    layers = {
        "sha256:a": {"l1": 100, "l2": 200, "l3": 300},
        "sha256:b": {"l1": 100, "l2": 200, "l4": 400},          # +l4
        "sha256:c": {"l5": 50, "l6": 60, "l4": 400, "l7": 70},  # +l5 l6 l7
        "sha256:d": {"l5": 50, "l6": 60, "l8": 80, "l7": 70},   # rechunk switch? no: id below
    }
    ledger = [
        {"created": "2026-01-01T00:00:00Z", "digest": "sha256:a", "release_id": "2026.01.01",
         "core_rebuilt": "y", "published": "y", "layers": "3", "revision": "a" * 40},
        {"created": "2026-01-02T00:00:00Z", "digest": "sha256:b", "release_id": "2026.01.02",
         "core_rebuilt": "n", "published": "y", "layers": "3", "revision": "b" * 40},
        {"created": "2026-01-02T12:00:00Z", "digest": "sha256:x", "release_id": "dev",
         "core_rebuilt": "y", "published": "n", "layers": "0", "revision": "c" * 40},
        {"created": "2026-01-03T00:00:00Z", "digest": "sha256:c", "release_id": "2026.01.03",
         "core_rebuilt": "y", "published": "y", "layers": "4", "revision": "d" * 40},
        {"created": "2026-01-04T00:00:00Z", "digest": "sha256:d", "release_id": "2026.01.03",
         "core_rebuilt": "n", "published": "y", "layers": "4", "revision": "d" * 40},
    ]
    failures = 0

    def check(name: str, got, want) -> None:
        nonlocal failures
        ok = got == want
        failures += not ok
        print(f"{'PASS' if ok else 'FAIL'}: {name}: got {got!r}, want {want!r}")

    check("delta a->b", delta(layers, "sha256:a", "sha256:b"), (400, 1))
    check("delta b->c", delta(layers, "sha256:b", "sha256:c"), (180, 3))
    check("delta c->c", delta(layers, "sha256:c", "sha256:c"), (0, 0))
    t = transitions(ledger, layers, "2026.01.03")
    check("unpublished images are skipped", len(t), 3)
    check("kinds", [x["kind"] for x in t], ["non-core", "rechunk-switch", "non-core"])
    check("same release id = reissue", [x["reissue"] for x in t], [False, False, True])
    check("bytes", [x["bytes"] for x in t], [400, 180, 80])
    t = transitions(ledger, layers, "2099.01.01")
    check("no switch before the cutoff", [x["kind"] for x in t], ["non-core", "core", "non-core"])

    # Release IDs order by number, not as text: .10 comes after .2.
    check("release .10 is after .2", release_key("2026.10.08.10") > release_key("2026.10.08.2"), True)
    check("2026.10.08 is before 2026.10.08.2", release_key("2026.10.08") < release_key("2026.10.08.2"), True)

    # fetch's decisions, on run/job records shaped like the GitHub API's.
    main_run = {"databaseId": 1, "headBranch": "main", "headSha": "e" * 40}
    branch_run = {"databaseId": 2, "headBranch": "perf/x", "headSha": "e" * 40}
    ok = {"conclusion": "success"}
    check("core success = rebuilt, main + image ok = published",
          classify(main_run, {"core": ok, "image": ok}), ("y", "y"))
    check("core skipped = not rebuilt",
          classify(main_run, {"core": {"conclusion": "skipped"}, "image": ok}), ("n", "y"))
    check("branch run = not published",
          classify(branch_run, {"core": ok, "image": ok}), ("y", "n"))
    check("main run, image job failed = not published",
          classify(main_run, {"core": ok, "image": {"conclusion": "failure"}}), ("y", "n"))
    window = {"image": {"started_at": "2026-01-05T10:00:00Z", "completed_at": "2026-01-05T10:30:00Z"}}
    early = {"image": {"started_at": "2026-01-05T08:00:00Z", "completed_at": "2026-01-05T08:30:00Z"}}
    jobs_by_run = {3: early, 1: window}
    other = {"databaseId": 3, "headBranch": "main", "headSha": "e" * 40}
    img = {"revision": "e" * 40, "created": parse_time("2026-01-05T10:31:00Z")}
    check("owner = the run whose image job was running (2 min slack)",
          (owner_of(img, [other, main_run], jobs_by_run) or {}).get("databaseId"), 1)
    img_late = {"revision": "e" * 40, "created": parse_time("2026-01-05T10:33:00Z")}
    check("no owner outside the slack", owner_of(img_late, [other, main_run], jobs_by_run), None)
    img_other = {"revision": "f" * 40, "created": parse_time("2026-01-05T10:10:00Z")}
    check("no owner for another commit", owner_of(img_other, [other, main_run], jobs_by_run), None)
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--self-test", action="store_true")
    sub = ap.add_subparsers(dest="cmd")
    f = sub.add_parser("fetch")
    f.add_argument("--data", default="docs/update-cost")
    f.add_argument("--since", default="2026-09-29")
    f.add_argument("--gh", default="gh")
    r = sub.add_parser("report")
    r.add_argument("--data", default="docs/update-cost")
    r.add_argument("--rechunk-from", default=RECHUNK_FROM)
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if args.cmd == "fetch":
        return fetch(args)
    if args.cmd == "report":
        return report(args)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
