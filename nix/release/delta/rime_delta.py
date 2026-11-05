#!/usr/bin/env python3
"""rime-delta: measure, make and apply zstd --patch-from deltas between two releases' store paths (G2 delta transport).

docs/nixos/delta.md has the method and the numbers. Subcommands:

  closure  STORE TOPLEVEL OUT.tsv           the toplevel's closure: path, name, narSize, narHash (nix path-info)
  pair     OLD.tsv NEW.tsv OUT.tsv          for every path NEW has and OLD lacks, the base candidates in OLD
  measure  PAIRS.tsv OUT.tsv --old-store S --new-store S [-j N]
                                            per new path: zstd -19 --long=27 of its NAR (full) and of its delta
                                            against each candidate's NAR (best kept); every delta is decompressed
                                            again and its sha256 compared with the new NAR (a wrong delta fails)
  summary  RESULTS.tsv                      totals: full vs delta bytes, by match kind
  make     BASE.nar NEW.nar OUT.delta       the server side: one delta
  apply    --narinfo URL --delta URL --base STOREPATH --into DIR [--import-to STORE --trusted-key K]
                                            the client side: fetch the delta and the signed narinfo, rebuild the NAR
                                            from the base path, refuse unless sha256/size match the narinfo's NarHash
                                            and NarSize, then (with --import-to) hand it to `nix copy` from a local
                                            file cache so Nix checks the narinfo signature and the hash again

Only the Python standard library; `nix` and `zstd` come from PATH. No network access except `apply`'s URLs.
"""

import argparse
import base64
import concurrent.futures
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

ZSTD_LEVEL = "-19"
SHIPPED_LEVEL = "-3"  # the level the release pipeline compresses NARs at today (nix copy ?compression=zstd)
LONG = 27  # --long=27 (128 MiB), as G1/G2 specify; --patch-from raises the window to the base's size
MAX_WINDOW_LOG = 31  # zstd's limit on 64-bit
NIX32 = "0123456789abcdfghijklmnpqrsvwxyz"
NAME_RE = re.compile(r"^/nix/store/[0-9a-z]{32}-(.+)$")
OUTPUTS = {"bin", "dev", "lib", "out", "man", "doc", "info", "debug", "static", "devdoc", "py", "modules", "shrunk",
           "data", "examples", "terminfo", "udev", "sbin", "fish", "devman", "hook", "unwrapped", "env", "nar"}


# ── names ──────────────────────────────────────────────────────────────────────────────────────────────────────────

def store_name(path):
    m = NAME_RE.match(path)
    if not m:
        raise ValueError(f"not a store path: {path}")
    return m.group(1)


def parse_name(name):
    """(pname, version, output) the way builtins.parseDrvName splits a name: the version starts at the first `-`
    followed by a digit. A trailing known output name (`-bin`, `-dev`, ...) is split off the version."""
    parts = name.split("-")
    for i in range(1, len(parts)):
        if parts[i][:1].isdigit():
            pname, rest = "-".join(parts[:i]), parts[i:]
            out = []
            while len(rest) > 1 and rest[-1] in OUTPUTS:
                out.insert(0, rest.pop())
            return pname, "-".join(rest), "-".join(out)
    return name, "", ""


def family(name):
    """Same package, any version: pname + output, version tokens dropped (python3.13-foo and python3.14-foo differ)."""
    pname, _version, out = parse_name(name)
    return f"{pname}|{out}"


def loose_family(name):
    """Last resort: every digit dropped (python3.13-foo-1.2 and python3.14-foo-1.3 meet)."""
    pname, _version, out = parse_name(name)
    return re.sub(r"[0-9.]+", "", pname) + "|" + out


# ── closure ────────────────────────────────────────────────────────────────────────────────────────────────────────

def path_info(store, paths, recursive):
    cmd = ["nix", "--extra-experimental-features", "nix-command", "path-info", "--json"]
    if store:
        cmd += ["--store", store]
    if recursive:
        cmd.append("-r")
    data = json.loads(subprocess.run(cmd + list(paths), check=True, capture_output=True, text=True).stdout)
    if isinstance(data, list):  # older JSON format: a list of objects with "path"
        data = {d["path"]: d for d in data}
    if isinstance(data, dict) and "info" in data and isinstance(data["info"], dict):  # versioned envelope
        data = data["info"]
    out = {}
    for p, d in data.items():
        if d is None:
            raise SystemExit(f"rime-delta: {p} is not valid in {store or 'the default store'}")
        p = p if p.startswith("/nix/store/") else "/nix/store/" + p
        out[p] = d
    return out


def cmd_closure(a):
    info = path_info(a.store, [a.toplevel], recursive=True)
    with open(a.out, "w") as f:
        f.write("path\tname\tnar_size\tnar_hash\n")
        for p in sorted(info):
            d = info[p]
            f.write(f"{p}\t{store_name(p)}\t{d['narSize']}\t{d['narHash']}\n")
    print(f"{a.out}: {len(info)} paths, {sum(int(d['narSize']) for d in info.values())} B NAR")


def read_tsv(fn):
    with open(fn) as f:
        head = f.readline().rstrip("\n").split("\t")
        return [dict(zip(head, line.rstrip("\n").split("\t"))) for line in f if line.strip()]


# ── pair ───────────────────────────────────────────────────────────────────────────────────────────────────────────

MAX_CANDIDATES = 4


def candidates(new, old_by):
    """Base candidates for one new path, best rule first. Several candidates with the same rule are all returned
    (closest NAR size first); `measure` tries each and keeps the smallest delta."""
    size = int(new["nar_size"])
    name = new["name"]
    for kind, key in (("same-name", ("name", name)), ("same-package", ("family", family(name))),
                      ("loose-package", ("loose", loose_family(name)))):
        found = old_by[key[0]].get(key[1], [])
        if found:
            found = sorted(found, key=lambda r: abs(int(r["nar_size"]) - size))[:MAX_CANDIDATES]
            return kind, found
    return "none", []


def cmd_pair(a):
    old, new = read_tsv(a.old), read_tsv(a.new)
    old_paths = {r["path"] for r in old}
    old_by = {"name": {}, "family": {}, "loose": {}}
    for r in old:
        old_by["name"].setdefault(r["name"], []).append(r)
        old_by["family"].setdefault(family(r["name"]), []).append(r)
        old_by["loose"].setdefault(loose_family(r["name"]), []).append(r)
    added = [r for r in new if r["path"] not in old_paths]
    with open(a.out, "w") as f:
        f.write("path\tname\tnar_size\tnar_hash\tmatch\tcandidates\n")
        for r in added:
            kind, found = candidates(r, old_by)
            f.write(f"{r['path']}\t{r['name']}\t{r['nar_size']}\t{r['nar_hash']}\t{kind}\t"
                    f"{','.join(c['path'] + '=' + c['nar_size'] for c in found)}\n")
    kept = len(new) - len(added)
    print(f"{a.out}: new closure {len(new)} paths, kept {kept}, added {len(added)}, old had {len(old)}")


# ── measure ────────────────────────────────────────────────────────────────────────────────────────────────────────

def physical(store, path):
    """Where `path` lives on disk: a chroot store (a directory, like `nix --store /x`) keeps it under /x/nix/store."""
    if store and not store.startswith("/"):
        raise SystemExit(f"rime-delta: store {store!r}: give the default store (empty) or a chroot store directory")
    return os.path.join(store, path.lstrip("/")) if store else path


def dump_nar(store, path, dest):
    """The path's NAR. `nix-store --dump` serialises a directory without opening the store (works in a build
    sandbox); a chroot store's copy gives the same NAR because symlink targets are stored as written."""
    with open(dest, "wb") as f:
        subprocess.run(["nix-store", "--dump", physical(store, path)], check=True, stdout=f)
    return os.path.getsize(dest)


def window_log(*sizes):
    """The smallest window that covers the largest of `sizes` (zstd's own rule: highbit(size) + 1), within limits."""
    biggest = max(sizes)
    return max(LONG, min(MAX_WINDOW_LOG, max(10, biggest.bit_length())))


def zstd_size(args, src, dest, level=ZSTD_LEVEL):
    t = time.monotonic()
    p = subprocess.run(["zstd", "-q", "-f", "-T1", level] + args + [src, "-o", dest], capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"zstd {' '.join(args)} {src}: {p.stderr.strip()}")
    return os.path.getsize(dest), time.monotonic() - t


def sha256_file(fn):
    h = hashlib.sha256()
    with open(fn, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.digest()


def nix32(digest):
    """Nix's base-32 (nix-hash --to-base32): least significant 5-bit group last."""
    n = len(digest) * 8
    out = []
    for i in range((n + 4) // 5 - 1, -1, -1):
        b, j = i * 5 // 8, i * 5 % 8
        c = digest[b] >> j
        if b + 1 < len(digest):
            c |= digest[b + 1] << (8 - j)
        out.append(NIX32[c & 0x1F])
    return "".join(out)


def nar_hash_string(digest):
    return "sha256:" + nix32(digest)


def reconstruct(base_nar, delta, out, wlog):
    p = subprocess.run(["zstd", "-q", "-d", "-f", f"--long={wlog}", f"--patch-from={base_nar}", delta, "-o", out],
                       capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"zstd -d --patch-from: {p.stderr.strip()}")


def try_delta(base_nar, new_nar, digest, wlog, dest):
    """One delta (zstd -19 --long=27 --patch-from); it only counts if it rebuilds the exact NAR. `wlog` is the window
    the decoder allows."""
    size, secs = zstd_size([f"--long={LONG}", f"--patch-from={base_nar}"], new_nar, dest)
    back = dest + ".back"
    t = time.monotonic()
    reconstruct(base_nar, dest, back, wlog)
    apply_s = time.monotonic() - t
    ok = sha256_file(back) == digest
    os.unlink(back)
    if not ok:
        raise RuntimeError(f"delta {dest} against {base_nar} does not rebuild the NAR")
    return size, secs, apply_s


def measure_one(row, a, tmp_root):
    tmp = tempfile.mkdtemp(dir=tmp_root)
    try:
        new_nar = os.path.join(tmp, "new.nar")
        size = dump_nar(a.new_store, row["path"], new_nar)
        if size != int(row["nar_size"]):
            raise RuntimeError(f"{row['path']}: dumped {size} B, path-info says {row['nar_size']}")
        digest = sha256_file(new_nar)
        if not narinfo_hash_matches(row["nar_hash"], digest):
            raise RuntimeError(f"{row['path']}: dumped NAR hash {nar_hash_string(digest)} != {row['nar_hash']}")
        full, full_s = zstd_size([f"--long={LONG}"], new_nar, os.path.join(tmp, "full.zst"))
        # What the D6 pipeline writes today: `nix copy --to file://...?compression=zstd` with compression-level
        # unset = libzstd's default level 3, no long-distance window.
        full_nix, _ = zstd_size([], new_nar, os.path.join(tmp, "full3.zst"), level=SHIPPED_LEVEL)
        res = {**row, "full_zstd": full, "full_s": round(full_s, 2), "tried": 0, "base": "", "base_size": 0,
               "delta": full, "wlog": LONG, "delta_s": 0, "apply_s": 0, "full_nix": full_nix}
        best_file = None
        for n, cand in enumerate(filter(None, row["candidates"].split(","))):
            base_path = cand.rsplit("=", 1)[0]
            base_nar = os.path.join(tmp, "base.nar")
            bsize = dump_nar(a.old_store, base_path, base_nar)
            dfile = os.path.join(tmp, f"d{n}.zst")
            # zstd raises the window to cover the base in --patch-from mode whatever --long says (measured on a 550 MiB
            # llvm lib with --long=27: frame window 550 MiB), so one compression at the specified flags is enough;
            # the decoder must allow that window (`wlog`).
            wl = window_log(bsize, size)
            dsize, dsecs, applys = try_delta(base_nar, new_nar, digest, wl, dfile)
            res["tried"] += 1
            if not res["base"] or dsize < res["delta"]:
                res.update(base=base_path, base_size=bsize, delta=dsize, wlog=wl, delta_s=round(dsecs, 2),
                           apply_s=round(applys, 3))
                best_file = dfile
        if a.keep and best_file:
            # The server side of a release: the best delta, named by the new path's hash part.
            shutil.copyfile(best_file, os.path.join(a.keep, os.path.basename(row["path"])[:32] + ".delta"))
        return res
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


RESULT_COLS = ["path", "name", "nar_size", "match", "tried", "base", "base_size", "full_zstd", "delta", "wlog",
               "full_s", "delta_s", "apply_s", "full_nix"]


def cmd_measure(a):
    rows = read_tsv(a.pairs)
    rows.sort(key=lambda r: -int(r["nar_size"]))  # largest first: the long tail finishes inside the pool
    done = set()
    if os.path.exists(a.out):  # resumable
        done = {r["path"] for r in read_tsv(a.out)}
    else:
        with open(a.out, "w") as f:
            f.write("\t".join(RESULT_COLS) + "\n")
    todo = [r for r in rows if r["path"] not in done]
    tmp_root = a.tmp or os.path.dirname(os.path.abspath(a.out))
    if a.keep:
        os.makedirs(a.keep, exist_ok=True)
    print(f"measure: {len(todo)} of {len(rows)} paths to do, {a.jobs} jobs", flush=True)
    failed = 0
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool, open(a.out, "a") as out:
        futs = {pool.submit(measure_one, r, a, tmp_root): r for r in todo}
        for i, fut in enumerate(concurrent.futures.as_completed(futs), 1):
            r = futs[fut]
            try:
                res = fut.result()
            except Exception as e:  # noqa: BLE001 - one bad path must not lose the run
                failed += 1
                print(f"FAIL {r['path']}: {e}", file=sys.stderr, flush=True)
                continue
            out.write("\t".join(str(res[c]) for c in RESULT_COLS) + "\n")
            out.flush()
            if i % 50 == 0 or int(r["nar_size"]) > 100_000_000:
                print(f"  {i}/{len(todo)} {res['name']} full {res['full_zstd']} delta {res['delta']}", flush=True)
    if failed:
        raise SystemExit(f"measure: {failed} paths failed")


# ── summary ────────────────────────────────────────────────────────────────────────────────────────────────────────

def cmd_summary(a):
    rows = read_tsv(a.results)
    G = 1e9

    def tot(rs, col):
        return sum(int(r[col]) for r in rs)

    full, delta = tot(rows, "full_zstd"), tot(rows, "delta")
    print(f"paths added {len(rows)}, NAR {tot(rows, 'nar_size') / G:.3f} GB")
    if rows and "full_nix" in rows[0]:
        print(f"full zstd -3 (shipped)    {tot(rows, 'full_nix') / G:.3f} GB")
    print(f"full zstd -19 --long=27   {full / G:.3f} GB")
    print(f"delta (--patch-from)      {delta / G:.3f} GB  ({delta / full * 100 if full else 0:.1f} % of full)")
    for kind in ("same-name", "same-package", "loose-package", "none"):
        rs = [r for r in rows if r["match"] == kind]
        if rs:
            print(f"  {kind:14} {len(rs):5} paths  full {tot(rs, 'full_zstd') / G:.3f} GB  delta {tot(rs, 'delta') / G:.3f} GB")
    big = sorted(rows, key=lambda r: -int(r["delta"]))[: a.top]
    print(f"largest {a.top} after delta (MB full -> MB delta, window log):")
    for r in big:
        print(f"  {int(r['full_zstd']) / 1e6:9.1f} -> {int(r['delta']) / 1e6:9.1f}  w{r['wlog']}  {r['match']:13} {r['name']}")
    multi = sum(1 for r in rows if int(r["tried"]) > 1)
    print(f"paths with more than one candidate tried: {multi}; max window log: {max((int(r['wlog']) for r in rows), default=LONG)}")


# ── make / apply (server and client) ──────────────────────────────────────────────────────────────────────────────

def cmd_make(a):
    wl = window_log(os.path.getsize(a.base_nar), os.path.getsize(a.new_nar))
    size, secs = zstd_size([f"--long={wl}", f"--patch-from={a.base_nar}"], a.new_nar, a.out)
    print(json.dumps({"delta": a.out, "bytes": size, "window_log": wl, "seconds": round(secs, 2)}))


def fetch(url, dest):
    if "://" not in url:
        url = "file://" + os.path.abspath(url)
    with urllib.request.urlopen(url) as r, open(dest, "wb") as f:  # noqa: S310 - file/http(s) only, by design
        shutil.copyfileobj(r, f, 1 << 20)
    return os.path.getsize(dest)


def parse_narinfo(text):
    info = {}
    for line in text.splitlines():
        if ": " in line:
            k, v = line.split(": ", 1)
            if k == "Sig":
                info.setdefault("Sig", []).append(v)
            else:
                info.setdefault(k, v)
    for k in ("StorePath", "NarHash", "NarSize"):
        if k not in info:
            raise SystemExit(f"rime-delta: narinfo has no {k}")
    return info


def narinfo_hash_matches(expected, digest):
    """`expected` as narinfos and `nix path-info` write it: sha256:<nix32|hex> or SRI sha256-<base64>."""
    sep = ":" if ":" in expected else "-"
    algo, _, val = expected.partition(sep)
    if algo != "sha256":
        return False
    return val in (nix32(digest), digest.hex(), base64.b64encode(digest).decode())


def apply_one(narinfo_url, delta_url, base, store, into, import_to="", keys=()):
    t0 = time.monotonic()
    os.makedirs(into, exist_ok=True)
    work = tempfile.mkdtemp(dir=into)
    try:
        narinfo_file = os.path.join(work, "x.narinfo")
        fetch(narinfo_url, narinfo_file)
        with open(narinfo_file) as f:
            narinfo_text = f.read()
        info = parse_narinfo(narinfo_text)
        delta = os.path.join(work, "x.delta")
        delta_bytes = fetch(delta_url, delta)
        base_nar = os.path.join(work, "base.nar")
        dump_nar(store, base, base_nar)
        new_nar = os.path.join(work, "new.nar")
        wl = window_log(os.path.getsize(base_nar), int(info["NarSize"]))
        try:
            reconstruct(base_nar, delta, new_nar, wl)
        except RuntimeError as e:
            raise SystemExit(f"rime-delta: refused {info['StorePath']}: the delta does not decode against {base}: {e}")
        os.unlink(base_nar)
        size = os.path.getsize(new_nar)
        digest = sha256_file(new_nar)
        if size != int(info["NarSize"]) or not narinfo_hash_matches(info["NarHash"], digest):
            raise SystemExit(f"rime-delta: refused {info['StorePath']}: rebuilt NAR is {size} B {nar_hash_string(digest)}, "
                             f"the signed narinfo says {info['NarSize']} B {info['NarHash']}")
        # A local file binary cache holding exactly this path: the narinfo as signed, with only the fields the
        # signature does not cover (URL, Compression, FileHash, FileSize) pointing at the rebuilt NAR.
        cache = os.path.join(into, "cache")
        os.makedirs(os.path.join(cache, "nar"), exist_ok=True)
        with open(os.path.join(cache, "nix-cache-info"), "w") as f:
            f.write("StoreDir: /nix/store\nWantMassQuery: 0\nPriority: 10\n")
        file_hash = nix32(digest)
        nar_file = os.path.join(cache, "nar", f"{file_hash}.nar")
        os.replace(new_nar, nar_file)
        lines = [ln for ln in narinfo_text.splitlines()
                 if ln.split(": ", 1)[0] not in ("URL", "Compression", "FileHash", "FileSize")]
        lines += [f"URL: nar/{file_hash}.nar", "Compression: none", f"FileHash: sha256:{file_hash}", f"FileSize: {size}"]
        narinfo_out = os.path.join(cache, os.path.basename(info["StorePath"])[:32] + ".narinfo")
        with open(narinfo_out, "w") as f:
            f.write("\n".join(lines) + "\n")
        rebuilt = time.monotonic() - t0
        imported = None
        if import_to:
            t1 = time.monotonic()
            nix_copy(f"file://{os.path.abspath(cache)}", import_to, info["StorePath"], keys)
            imported = round(time.monotonic() - t1, 3)
            os.unlink(nar_file)
            os.unlink(narinfo_out)
        return {"path": info["StorePath"], "delta_bytes": delta_bytes, "nar_bytes": size, "window_log": wl,
                "rebuild_s": round(rebuilt, 3), "import_s": imported}
    finally:
        shutil.rmtree(work, ignore_errors=True)


def nix_copy(src, dest, path, keys):
    cmd = ["nix", "--extra-experimental-features", "nix-command", "copy", "--no-recursive", "--from", src,
           "--to", dest, path, "--option", "trusted-public-keys", " ".join(keys)]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        raise SystemExit(f"rime-delta: nix copy refused {path}: {p.stderr.strip()}")


def cmd_apply(a):
    print(json.dumps(apply_one(a.narinfo, a.delta, a.base, a.store, a.into, a.import_to, a.trusted_key or [])))


def peak_rss_mib():
    import resource

    return {"children_max_rss_mib": round(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024, 1),
            "self_max_rss_mib": round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024, 1)}


def cmd_apply_all(a):
    """G3 benchmark: move a store from the old release to the new one with deltas only. Every path `measure` gave a
    base is rebuilt from its delta and checked against the server's signed narinfo; a path with no base is copied
    whole from the server cache. References go first, so `nix copy --no-recursive` always finds them valid."""
    rows = {r["path"]: r for r in read_tsv(a.results)}
    infos = {}
    for p in rows:
        with open(os.path.join(a.server, os.path.basename(p)[:32] + ".narinfo")) as f:
            infos[p] = parse_narinfo(f.read())
    order, seen = [], set()

    def visit(p):  # depth-first, references first (store paths form a DAG)
        if p in seen:
            return
        seen.add(p)
        for ref in infos[p].get("References", "").split():
            rp = "/nix/store/" + ref
            if rp in rows and rp != p:
                visit(rp)
        order.append(p)

    for p in sorted(rows):
        visit(p)
    t0 = time.monotonic()
    log = open(a.log, "w") if a.log else None
    totals = {"paths": 0, "delta_paths": 0, "full_paths": 0, "delta_bytes": 0, "full_nar_bytes": 0, "nar_bytes": 0,
              "rebuild_s": 0.0, "import_s": 0.0}
    for p in order:
        r = rows[p]
        hp = os.path.basename(p)[:32]
        if r["base"]:
            res = apply_one(os.path.join(a.server, hp + ".narinfo"), os.path.join(a.deltas, hp + ".delta"), r["base"],
                            a.store, a.into, a.store, a.trusted_key or [])
            totals["delta_paths"] += 1
            totals["delta_bytes"] += res["delta_bytes"]
            totals["rebuild_s"] += res["rebuild_s"]
            totals["import_s"] += res["import_s"]
        else:
            t = time.monotonic()
            nix_copy(f"file://{os.path.abspath(a.server)}", a.store, p, a.trusted_key or [])
            res = {"path": p, "full": True, "import_s": round(time.monotonic() - t, 3)}
            totals["full_paths"] += 1
            totals["full_nar_bytes"] += int(infos[p]["NarSize"])
            totals["import_s"] += res["import_s"]
        totals["paths"] += 1
        totals["nar_bytes"] += int(infos[p]["NarSize"])
        if log:
            log.write(json.dumps(res) + "\n")
    totals["wall_s"] = round(time.monotonic() - t0, 1)
    totals["rebuild_s"] = round(totals["rebuild_s"], 1)
    totals["import_s"] = round(totals["import_s"], 1)
    totals.update(peak_rss_mib())
    print(json.dumps(totals))


def main(argv=None):
    ap = argparse.ArgumentParser(prog="rime-delta", description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("closure")
    s.add_argument("store")
    s.add_argument("toplevel")
    s.add_argument("out")
    s.set_defaults(func=cmd_closure)
    s = sub.add_parser("pair")
    s.add_argument("old")
    s.add_argument("new")
    s.add_argument("out")
    s.set_defaults(func=cmd_pair)
    s = sub.add_parser("measure")
    s.add_argument("pairs")
    s.add_argument("out")
    s.add_argument("--old-store", default="")
    s.add_argument("--new-store", default="")
    s.add_argument("-j", "--jobs", type=int, default=os.cpu_count() or 4)
    s.add_argument("--tmp", default="")
    s.add_argument("--keep", default="", help="keep each path's best delta here as <hash>.delta")
    s.set_defaults(func=cmd_measure)
    s = sub.add_parser("summary")
    s.add_argument("results")
    s.add_argument("--top", type=int, default=15)
    s.set_defaults(func=cmd_summary)
    s = sub.add_parser("make")
    s.add_argument("base_nar")
    s.add_argument("new_nar")
    s.add_argument("out")
    s.set_defaults(func=cmd_make)
    s = sub.add_parser("apply")
    s.add_argument("--narinfo", required=True)
    s.add_argument("--delta", required=True)
    s.add_argument("--base", required=True, help="the old store path the delta was made against")
    s.add_argument("--store", default="", help="store holding --base (default: the default store)")
    s.add_argument("--into", required=True, help="work directory; the rebuilt path's local file cache goes in DIR/cache")
    s.add_argument("--import-to", default="", help="store to import into with nix copy (omit: rebuild and check only)")
    s.add_argument("--trusted-key", action="append", help="narinfo public key nix copy trusts (repeatable)")
    s.set_defaults(func=cmd_apply)
    s = sub.add_parser("apply-all")
    s.add_argument("results", help="measure's TSV (path, base)")
    s.add_argument("--server", required=True, help="the release's file cache: <hash>.narinfo (signed) and nar/")
    s.add_argument("--deltas", required=True, help="measure --keep's directory")
    s.add_argument("--store", required=True, help="the machine's store (chroot directory) on the old release")
    s.add_argument("--into", required=True)
    s.add_argument("--trusted-key", action="append")
    s.add_argument("--log", default="")
    s.set_defaults(func=cmd_apply_all)
    a = ap.parse_args(argv)
    a.func(a)


if __name__ == "__main__":
    main()
