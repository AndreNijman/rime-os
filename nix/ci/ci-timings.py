#!/usr/bin/env python3
"""Where CI time goes, from the GitHub API (Python stdlib + the `gh` CLI; read-only).

  ci-timings.py [--repo OWNER/REPO] [--gh GH] workflows [--workflow NAME]... [--branch B] [--runs N] [--job-runs M]
      Per workflow, over its last N completed runs (default 30): runs, wall-clock median and p90 (minutes), success
      rate; and over the last M of them (default 5, each costs API calls): runner-minutes per run, job queue time
      (started_at - created_at: how long a job waited for a runner) median / p90 / max, the jobs that waited > 1 min,
      and the per-job and per-step totals (matrix legs "Check x" folded into "Check *").

  ci-timings.py [--repo OWNER/REPO] [--gh GH] run RUN_ID [--json FILE]
      One run: wall clock, runner-minutes, jobs, queue times, every job's own time, and (nix.yml) its `metrics`
      artifact: legs built vs REUSED, legs that compiled rimed, dev cache state per leg.

Runner-minutes are the sum of the jobs' own run times (completed_at - started_at), i.e. what a hosted runner was
held for; a public repository is not billed for them, but its concurrent-job limit makes them the wall clock's floor.
"""
import argparse
import io
import json
import statistics
import subprocess
import sys
import zipfile
from datetime import datetime, timezone

GH = "gh"
REPO = "AndreNijman/rime-os"


def api(path, raw=False):
    cmd = [GH, "api", "-H", "Accept: application/vnd.github+json", path]
    r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if r.returncode != 0:
        raise SystemExit(f"ci-timings: gh api {path}: {r.stderr.decode(errors='replace').strip()}")
    return r.stdout if raw else json.loads(r.stdout)


def paged(path, key, limit=None):
    out, page = [], 1
    sep = "&" if "?" in path else "?"
    while True:
        d = api(f"{path}{sep}per_page=100&page={page}")
        items = d.get(key) or []
        out += items
        if len(items) < 100 or (limit and len(out) >= limit):
            return out[:limit] if limit else out
        page += 1


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")) if s else None


def mins(a, b):
    a, b = ts(a), ts(b)
    return (b - a).total_seconds() / 60 if a and b else None


def pct(xs, p):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def f1(x):
    return "" if x is None else f"{x:.1f}"


def jobs_of(run_id):
    return paged(f"repos/{REPO}/actions/runs/{run_id}/jobs?filter=latest", "jobs")


def fold(name):
    return "Check *" if name.startswith("Check ") else name


def run_jobs_stats(jobs):
    real = [j for j in jobs if j.get("started_at") and j.get("completed_at") and j.get("conclusion") != "skipped"]
    runner = sum(mins(j["started_at"], j["completed_at"]) or 0 for j in real)
    queue = [mins(j["created_at"], j["started_at"]) for j in real]
    return real, runner, queue


def workflows(a):
    runs = paged(f"repos/{REPO}/actions/runs?status=completed" + (f"&branch={a.branch}" if a.branch else ""),
                 "workflow_runs", limit=a.scan)
    byw = {}
    for r in runs:
        if a.workflow and r["name"] not in a.workflow:
            continue
        byw.setdefault(r["name"], []).append(r)
    print(f"# CI timings: {REPO}" + (f", branch {a.branch}" if a.branch else "") + f" ({datetime.now(timezone.utc):%Y-%m-%d %H:%M} UTC)\n")
    print("| workflow | runs | wall median min | wall p90 | success % | runner-min/run (last M) | queue median / p90 / max min | jobs queued > 1 min |")
    print("|---|---|---|---|---|---|---|---|")
    detail = {}
    for name, rs in sorted(byw.items()):
        rs = rs[: a.runs]
        wall = [mins(r["run_started_at"], r["updated_at"]) for r in rs]
        ok = sum(1 for r in rs if r["conclusion"] == "success")
        rm, qs, late, jobt, stept = [], [], 0, {}, {}
        for r in rs[: a.job_runs]:
            js = jobs_of(r["id"])
            real, runner, queue = run_jobs_stats(js)
            rm.append(runner)
            qs += queue
            late += sum(1 for q in queue if q is not None and q > 1)
            for j in real:
                jobt.setdefault(fold(j["name"]), []).append(mins(j["started_at"], j["completed_at"]))
                for s in j.get("steps") or []:
                    d = mins(s.get("started_at"), s.get("completed_at"))
                    if d is not None:
                        stept.setdefault((fold(j["name"]), s["name"]), []).append(d)
        detail[name] = (jobt, stept, len(rs[: a.job_runs]))
        print(f"| {name} | {len(rs)} | {f1(statistics.median([w for w in wall if w is not None]) if wall else None)} | "
              f"{f1(pct(wall, 0.9))} | {100 * ok / len(rs):.0f} | {f1(statistics.mean(rm) if rm else None)} | "
              f"{f1(pct(qs, 0.5))} / {f1(pct(qs, 0.9))} / {f1(max([q for q in qs if q is not None], default=None))} | {late} |")
    for name, (jobt, stept, n) in sorted(detail.items()):
        if not jobt:
            continue
        print(f"\n## {name}: per job (last {n} runs; minutes)\n")
        print("| job | count | total | median | max |")
        print("|---|---|---|---|---|")
        for k, v in sorted(jobt.items(), key=lambda kv: -sum(kv[1])):
            print(f"| {k} | {len(v)} | {sum(v):.1f} | {statistics.median(v):.1f} | {max(v):.1f} |")
        print(f"\n### {name}: top steps by total (minutes)\n")
        print("| job | step | count | total | median |")
        print("|---|---|---|---|---|")
        for (jn, sn), v in sorted(stept.items(), key=lambda kv: -sum(kv[1]))[:15]:
            print(f"| {jn} | {sn} | {len(v)} | {sum(v):.1f} | {statistics.median(v):.1f} |")


def metrics_of(run_id):
    arts = api(f"repos/{REPO}/actions/runs/{run_id}/artifacts?per_page=100").get("artifacts") or []
    m = [x for x in arts if x["name"] == "metrics" and not x.get("expired")]
    if not m:
        return None
    blob = api(f"repos/{REPO}/actions/artifacts/{m[0]['id']}/zip", raw=True)
    out = []
    with zipfile.ZipFile(io.BytesIO(blob)) as z:
        for n in z.namelist():
            if n.endswith(".metrics.json"):
                out.append(json.loads(z.read(n)))
    return out


def run(a):
    r = api(f"repos/{REPO}/actions/runs/{a.run_id}")
    js = jobs_of(a.run_id)
    real, runner, queue = run_jobs_stats(js)
    wall = mins(r["run_started_at"], r["updated_at"])
    legs = [j for j in real if j["name"].startswith("Check ")]
    res = {
        "run": a.run_id, "url": r["html_url"], "branch": r["head_branch"], "sha": r["head_sha"][:9],
        "status": r["status"], "conclusion": r["conclusion"], "wall_min": wall, "runner_min": runner,
        "jobs": len(real), "legs": len(legs), "jobs_queued_over_1min": sum(1 for q in queue if q and q > 1),
        "queue_max_min": max([q for q in queue if q is not None], default=None),
        "queue_median_min": pct(queue, 0.5),
        "job_min": {j["name"]: round(mins(j["started_at"], j["completed_at"]) or 0, 1) for j in real},
        "job_queue_min": {j["name"]: round(mins(j["created_at"], j["started_at"]) or 0, 1) for j in real},
        "job_conclusion": {j["name"]: j["conclusion"] for j in real},
    }
    met = metrics_of(a.run_id)
    if met is not None:
        built = [m for m in met if not m.get("reused")]
        res["metrics"] = {
            "legs_built": len(built), "legs_reused": len(met) - len(built),
            "legs_compiled_rimed": sorted(m["check"] for m in built if m.get("rimed_built")),
            "dev_cache": sorted({str(m.get("dev_cache")) for m in built}),
            "dev_cache_substituted": sum((m.get("substituted_from") or {}).get("dev-cache", 0) for m in built),
            "local_builds": sum(m.get("local_builds") or 0 for m in built),
        }
    print(f"# Run {a.run_id} ({res['branch']} {res['sha']}): {res['status']}/{res['conclusion']}  {res['url']}\n")
    print(f"- wall {f1(wall)} min; runner {f1(runner)} min over {res['jobs']} jobs ({res['legs']} check legs)")
    print(f"- queue: median {f1(res['queue_median_min'])} min, max {f1(res['queue_max_min'])} min; "
          f"{res['jobs_queued_over_1min']} jobs waited > 1 min for a runner")
    if "metrics" in res:
        m = res["metrics"]
        print(f"- legs built {m['legs_built']}, REUSED {m['legs_reused']}; legs that compiled rimed: "
              f"{len(m['legs_compiled_rimed'])} {m['legs_compiled_rimed'] or ''}; dev cache {', '.join(m['dev_cache'])}; "
              f"{m['dev_cache_substituted']} paths from the dev cache; {m['local_builds']} local builds")
    else:
        print("- no `metrics` artifact (a run before the dev cache, or the summary did not run)")
    print("\n| job | queue min | run min | conclusion |\n|---|---|---|---|")
    for j in sorted(real, key=lambda j: -(mins(j["started_at"], j["completed_at"]) or 0)):
        print(f"| {j['name']} | {res['job_queue_min'][j['name']]} | {res['job_min'][j['name']]} | {j['conclusion']} |")
    if a.json:
        with open(a.json, "w") as f:
            json.dump(res, f, indent=1, default=str)


def main():
    global GH, REPO
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--repo", default=REPO)
    p.add_argument("--gh", default=GH)
    sub = p.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("workflows")
    w.add_argument("--workflow", action="append")
    w.add_argument("--branch")
    w.add_argument("--runs", type=int, default=30)
    w.add_argument("--job-runs", type=int, default=5)
    w.add_argument("--scan", type=int, default=400, help="how many recent runs to read (all workflows)")
    r = sub.add_parser("run")
    r.add_argument("run_id")
    r.add_argument("--json")
    a = p.parse_args()
    GH, REPO = a.gh, a.repo
    {"workflows": workflows, "run": run}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
