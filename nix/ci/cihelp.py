#!/usr/bin/env python3
"""Helpers for nix/ci/run-checks.sh that need JSON or hashing (Python stdlib only).

  cihelp.py export DIR [--closure] [--unsigned-only] PATH...
      Write a file:// binary cache of PATH... into DIR: one zstd NAR and one narinfo per path, nothing else.
      --closure        take the runtime closures of PATH... (nix path-info -r)
      --unsigned-only  keep only paths with no signature from an upstream key (UPSTREAM_KEYS, default
                       cache.nixos.org-1): what this machine built, or took from an earlier dev cache. Everything
                       else is on cache.nixos.org, which every leg substitutes from anyway.
      Why not `nix copy --to file://`: a binary cache refuses a path whose references it does not hold, so the dev
      cache would have to carry every glibc and GTK of the closures (GBs). Its narinfos may name references it does
      not hold; a consumer substitutes those from cache.nixos.org. Prints one line per path written.

  cihelp.py metrics CHECK TLOG STATUS SECS DEVCACHE > CHECK.metrics.json
      What a check's build spent its time on, from run-checks.sh's timestamped log ("<epoch> <line>").

  cihelp.py metrics-table DIR
      The metrics of every DIR/*.metrics.json as a markdown table (the summary job's).
"""
import glob
import hashlib
import json
import os
import re
import subprocess
import sys

NIX32 = "0123456789abcdfghijklmnpqrsvwxyz"


def nix32(b: bytes) -> str:
    n = (len(b) * 8 - 1) // 5 + 1
    out = []
    for i in range(n - 1, -1, -1):
        bit = i * 5
        j, k = divmod(bit, 8)
        c = b[j] >> k
        if j + 1 < len(b):
            c |= b[j + 1] << (8 - k)
        out.append(NIX32[c & 0x1F])
    return "".join(out)


def sri_to_nix32(h: str) -> str:
    """'sha256-<base64>' (path-info JSON) or 'sha256:<nix32>' -> 'sha256:<nix32>'."""
    import base64
    if h.startswith("sha256-"):
        return "sha256:" + nix32(base64.b64decode(h[len("sha256-"):]))
    if h.startswith("sha256:"):
        return h
    raise SystemExit(f"cihelp: unknown hash format {h!r}")


def path_info(paths, closure):
    cmd = ["nix", "path-info", "--json-format", "1", "--json"] + (["-r"] if closure else []) + list(paths)
    out = subprocess.run(cmd, check=True, stdout=subprocess.PIPE).stdout
    data = json.loads(out)
    if isinstance(data, list):  # older Nix: a list of objects with "path"
        data = {d["path"]: d for d in data}
    return data


def upstream_signed(info, keys):
    return any(s.split(":", 1)[0] in keys for s in (info.get("signatures") or []))


def export(argv):
    closure = unsigned = False
    rest = []
    for a in argv:
        if a == "--closure":
            closure = True
        elif a == "--unsigned-only":
            unsigned = True
        else:
            rest.append(a)
    if len(rest) < 2:
        raise SystemExit("usage: cihelp.py export DIR [--closure] [--unsigned-only] PATH...")
    d, paths = rest[0], rest[1:]
    keys = set((os.environ.get("UPSTREAM_KEYS") or "cache.nixos.org-1").split())
    infos = path_info(paths, closure)
    os.makedirs(os.path.join(d, "nar"), exist_ok=True)
    info_file = os.path.join(d, "nix-cache-info")
    if not os.path.exists(info_file):
        with open(info_file, "w") as f:
            f.write("StoreDir: /nix/store\nWantMassQuery: 0\nPriority: 30\n")
    written = 0
    for p in sorted(infos):
        info = infos[p]
        if unsigned and upstream_signed(info, keys):
            continue
        base = os.path.basename(p)
        h = base[:32]
        narinfo = os.path.join(d, h + ".narinfo")
        if os.path.exists(narinfo):
            print(f"have {p}")
            continue
        tmp = os.path.join(d, "nar", h + ".tmp")
        with open(tmp, "wb") as out:
            dump = subprocess.Popen(["nix-store", "--dump", p], stdout=subprocess.PIPE)
            z = subprocess.run(["zstd", "-q", "-T0", "-3", "-c"], stdin=dump.stdout, stdout=out, check=True)
            dump.stdout.close()
            if dump.wait() != 0 or z.returncode != 0:
                raise SystemExit(f"cihelp: could not dump {p}")
        sha = hashlib.sha256()
        with open(tmp, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                sha.update(chunk)
        fh = nix32(sha.digest())
        nar = os.path.join("nar", fh + ".nar.zst")
        os.replace(tmp, os.path.join(d, nar))
        lines = [
            f"StorePath: {p}",
            f"URL: {nar}",
            "Compression: zstd",
            f"FileHash: sha256:{fh}",
            f"FileSize: {os.path.getsize(os.path.join(d, nar))}",
            f"NarHash: {sri_to_nix32(info['narHash'])}",
            f"NarSize: {info['narSize']}",
            "References: " + " ".join(sorted(os.path.basename(r) for r in info.get("references") or [])),
        ]
        if info.get("deriver"):
            lines.append(f"Deriver: {os.path.basename(info['deriver'])}")
        if info.get("ca"):
            lines.append(f"CA: {info['ca']}")
        with open(narinfo + ".tmp", "w") as f:
            f.write("\n".join(lines) + "\n")
        os.replace(narinfo + ".tmp", narinfo)
        written += 1
        print(f"wrote {p} ({info['narSize']} bytes)")
    print(f"cihelp: {written} path(s) written to {d}", file=sys.stderr)


TS = re.compile(r"^(\d+(?:\.\d+)?) (.*)$")
BUILDING = re.compile(r"^building '/nix/store/[a-z0-9]{32}-(.+)\.drv'")
COPYING = re.compile(r"^copying path '(/nix/store/[^']+)' from '([^']+)'")
PREFIX = re.compile(r"^([^ >]+)> ")
EVAL_END = re.compile(r"^(these \d+ (derivations|paths) will be|this (derivation|path) will be|copying path|building ')")
FINISHED = re.compile(r"test script finished in ([0-9.]+)s")


def metrics(argv):
    if len(argv) != 5:
        raise SystemExit("usage: cihelp.py metrics CHECK TLOG STATUS SECS DEVCACHE")
    check, tlog, status, secs, devcache = argv
    lines = []
    if os.path.exists(tlog):
        with open(tlog, errors="replace") as f:
            for raw in f:
                m = TS.match(raw.rstrip("\n"))
                if m:
                    lines.append((float(m.group(1)), m.group(2)))
    t0 = lines[0][0] if lines else None
    eval_end = None
    copies = {}
    copy_t = []
    builds = {}
    last_out = {}
    vm = None
    for t, body in lines:
        if eval_end is None and EVAL_END.match(body):
            eval_end = t
        m = COPYING.match(body)
        if m:
            src = "dev-cache" if m.group(2).startswith("file://") else m.group(2)
            copies[src] = copies.get(src, 0) + 1
            copy_t.append(t)
            continue
        m = BUILDING.match(body)
        if m:
            builds.setdefault(m.group(1), t)
            continue
        m = PREFIX.match(body)
        if m:
            last_out[m.group(1)] = t
        m = FINISHED.search(body)
        if m:
            vm = float(m.group(1))
    # A derivation's time: from nix's "building '...'" to the last line its builder printed ("<name>> ...", the
    # prefix nix -L gives; pname without the version). Builds overlap, so these do not add up to the leg's time.
    spans = []
    ends = []
    for name, start in builds.items():
        short = re.sub(r"-[0-9][^-]*(-unstable.*)?$", "", name)
        end = max(last_out.get(short, start), last_out.get(name, start))
        ends.append(end)
        spans.append((round(max(end - start, 0.0), 1), name))
    spans.sort(reverse=True)
    started = float(os.environ.get("RIME_BUILD_STARTED") or (t0 or 0))
    data = {
        "check": check,
        "status": status,
        "seconds": int(secs) if str(secs).isdigit() else secs,
        "eval_s": round(eval_end - started, 1) if eval_end is not None and started else None,
        "substituted": sum(copies.values()),
        "substituted_from": copies,
        "substitute_span_s": round(copy_t[-1] - copy_t[0], 1) if copy_t else 0,
        "local_builds": len(builds),
        "build_span_s": round(max(ends) - min(builds.values()), 1) if builds else 0,
        "biggest_builds": [{"name": n, "s": s} for s, n in spans[:3]],
        "rimed_built": any(n.startswith("rimed-") and not n.startswith("rimed-weakened") for n in builds),
        "vm_test_s": vm,
        "dev_cache": devcache,
        "reused": False,
    }
    json.dump(data, sys.stdout, indent=1, sort_keys=True)
    print()


def metrics_table(argv):
    if len(argv) != 1:
        raise SystemExit("usage: cihelp.py metrics-table DIR")
    rows = []
    for f in sorted(glob.glob(os.path.join(argv[0], "*.metrics.json"))):
        try:
            with open(f) as fh:
                rows.append(json.load(fh))
        except (OSError, ValueError) as e:
            print(f"cihelp: skipping {f}: {e}", file=sys.stderr)
    print("| check | result | total s | eval s | substituted (dev cache) | local builds | biggest local builds | VM test s | dev cache |")
    print("|---|---|---|---|---|---|---|---|---|")
    tot = {"s": 0, "subst": 0, "dev": 0, "builds": 0, "rimed": 0, "reused": 0}
    for r in sorted(rows, key=lambda r: -(r.get("seconds") or 0) if isinstance(r.get("seconds"), int) else 0):
        if r.get("reused"):
            tot["reused"] += 1
            print(f"| `{r['check']}` | REUSED | 0 | | | | | | |")
            continue
        dev = (r.get("substituted_from") or {}).get("dev-cache", 0)
        big = ", ".join(f"{b['name']} {b['s']:.0f}s" for b in r.get("biggest_builds") or [])
        secs = r.get("seconds") or 0
        tot["s"] += secs if isinstance(secs, int) else 0
        tot["subst"] += r.get("substituted") or 0
        tot["dev"] += dev
        tot["builds"] += r.get("local_builds") or 0
        tot["rimed"] += 1 if r.get("rimed_built") else 0
        print(f"| `{r['check']}` | {r.get('status')} | {secs} | {r.get('eval_s') if r.get('eval_s') is not None else ''} | "
              f"{r.get('substituted') or 0} ({dev}) | {r.get('local_builds') or 0} | {big} | "
              f"{r.get('vm_test_s') if r.get('vm_test_s') is not None else ''} | {r.get('dev_cache') or ''} |")
    print()
    print(f"Legs with metrics: {len(rows) - tot['reused']} built, {tot['reused']} reused. Build seconds summed: {tot['s']}. "
          f"Paths substituted: {tot['subst']} ({tot['dev']} from the dev cache). Local builds: {tot['builds']}. "
          f"Legs that compiled rimed: {tot['rimed']}.")


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    cmd, argv = sys.argv[1], sys.argv[2:]
    {"export": export, "metrics": metrics, "metrics-table": metrics_table}.get(
        cmd, lambda _a: (_ for _ in ()).throw(SystemExit(__doc__)))(argv)


if __name__ == "__main__":
    main()
