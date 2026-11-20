#!/usr/bin/env python3
"""Rime's binary cache as the release build asks it: only about paths it holds (docs/nixos/updates-and-release.md
§6.3 step 1, "Ask Pages only about what it holds").

  cache_gate.py ensure                 start the gate unless it already answers; exit 1 unless it answers
  cache_gate.py serve                  the gate in the foreground (what ensure starts, detached)
  cache_gate.py retry [--ensure] -- CMD...
                                       run CMD (a nix build); run it again, after a backoff, only when it failed on a
                                       transient error of Rime's cache (HTTP 429 or 5xx, or a dropped connection)

The release build's Nix daemon trusts the gate (http://127.0.0.1:PORT) as its Rime substituter instead of
https://cache.rimeos.com. The gate holds the cache's index: the names in the root of the rime-cache repository's main
branch, which is exactly what GitHub Pages serves (one blobless git clone of github.com, never a request to Pages).
A narinfo the index holds is answered with a redirect to the cache, so Nix reads it from Pages and checks its
signature as before; one the index does not hold is answered 404 here. On 2026-11-19 GitHub Pages answered HTTP 503
instead of 404 for paths the cache does not hold (new outputs the build must make), up to an hour per URL: every one
of the 55 narinfos that got a 503 in the three attempts of run 44472232881 was a path the cache does not hold, and
every path the build substituted from it is in the index. A 503 there outlasted Nix's twelve attempts and, with
fallback = false, failed the release.

The index is now what decides whether a Rime path is substituted or built, so it fails closed: a failed listing, an
index with no nix-cache-info or with no narinfo stops the gate, and the build with it. The gate serves the cache's
own nix-cache-info (Priority 50, behind cache.nixos.org), so a path both caches hold still comes from upstream.

Configuration comes from the environment the workflow already sets, or the flags: RIME_CACHE_GATE (the gate's
http://127.0.0.1:PORT URL), CACHE_URL (the cache), CACHE_REPO (owner/name of the repository Pages serves).
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NIX32 = "0123456789abcdfghijklmnpqrsvwxyz"
HASH_FILE_RE = re.compile(r"^/([" + NIX32 + r"]{32})\.(narinfo|ls)$")
NARINFO_NAME_RE = re.compile(r"^([" + NIX32 + r"]{32})\.narinfo$")
INDEX_TRIES = 4  # the git listing
INDEX_DELAY = 5.0  # seconds, doubled per try
ENSURE_WAIT = 300.0  # seconds for a started gate to answer (the listing's retries included)
RETRY_TRIES = 4  # the whole Nix command; each try already makes Nix's own download attempts
RETRY_BASE = 90.0  # seconds before the 2nd try; doubled per try, capped, with up to 50 % jitter added
RETRY_CAP = 600.0


class Refused(Exception):
    pass


class NotACache(Refused):
    """The listing worked and shows no cache Nix could be gated by: asking again would not change it."""


# ── the index ────────────────────────────────────────────────────────────────


def index_from_listing(names):
    """The narinfo hashes in a `git ls-tree --name-only` listing of the cache's root."""
    out = set()
    for n in names:
        m = NARINFO_NAME_RE.match(n.strip())
        if m:
            out.add(m.group(1))
    return out


def check_cache_info(text):
    """The cache's own nix-cache-info, served as is: it must name /nix/store."""
    fields = dict(ln.split(":", 1) for ln in text.splitlines() if ":" in ln)
    fields = {k.strip(): v.strip() for k, v in fields.items()}
    if fields.get("StoreDir") != "/nix/store":
        raise NotACache(f"the cache's nix-cache-info names no StoreDir /nix/store ({text.strip()[:200]!r})")
    return text


def fetch_index(repo, branch, work, run=subprocess.run, sleep=time.sleep, tries=INDEX_TRIES):
    """(hashes, nix-cache-info text, commit) of `repo`'s `branch`: one blobless, shallow clone (tree objects, and the one
    nix-cache-info blob read lazily), retried with backoff. Refuses an index Nix could not be gated by."""
    last = ""
    for n in range(1, tries + 1):
        d = os.path.join(work, f"index-{n}")
        try:
            git = ["git", "-c", "advice.detachedHead=false"]
            r = run(git + ["clone", "--quiet", "--depth", "1", "--filter=blob:none", "--no-checkout", "--single-branch",
                           "--branch", branch, repo, d], capture_output=True, text=True)
            if r.returncode != 0:
                raise Refused(f"git clone {repo}: {(r.stderr or '').strip()[-300:]}")
            ls = run(git + ["-C", d, "ls-tree", "--name-only", "HEAD"], capture_output=True, text=True)
            if ls.returncode != 0:
                raise Refused(f"git ls-tree: {(ls.stderr or '').strip()[-300:]}")
            names = ls.stdout.splitlines()
            if "nix-cache-info" not in names:
                raise NotACache(f"{repo} {branch} holds no nix-cache-info: not a binary cache")
            info = run(git + ["-C", d, "cat-file", "-p", "HEAD:nix-cache-info"], capture_output=True, text=True)
            if info.returncode != 0:
                raise Refused(f"git cat-file nix-cache-info: {(info.stderr or '').strip()[-300:]}")
            head = run(git + ["-C", d, "rev-parse", "HEAD"], capture_output=True, text=True)
            hashes = index_from_listing(names)
            if not hashes:
                # An empty index would make the build rebuild every Rime path (what fallback = false forbids).
                raise NotACache(f"{repo} {branch} holds no narinfo")
            return hashes, check_cache_info(info.stdout), head.stdout.strip()
        except NotACache:
            raise
        except Refused as e:
            last = str(e)
            if n < tries:
                print(f"cache_gate: listing the cache failed ({last}); again in {INDEX_DELAY * 2 ** (n - 1):.0f} s",
                      file=sys.stderr, flush=True)
                sleep(INDEX_DELAY * 2 ** (n - 1))
    raise Refused(f"could not list Rime's cache after {tries} tries: {last}")


# ── the gate ─────────────────────────────────────────────────────────────────


class Gate:
    """What the HTTP handler answers. `route` is pure, so the tests need no socket."""

    def __init__(self, cache, hashes, cache_info, repo="", commit=""):
        self.cache = cache.rstrip("/")
        self.hashes = frozenset(hashes)
        self.cache_info = cache_info
        self.repo, self.commit = repo, commit
        self.lock = threading.Lock()
        self.counts = {"redirected": 0, "not_held": 0, "other": 0}

    def count(self, k):
        with self.lock:
            self.counts[k] += 1

    def route(self, raw_path):
        """(status, headers, body) for a GET of `raw_path`."""
        path = urllib.parse.urlsplit(raw_path).path
        if path == "/nix-cache-info":
            return 200, {"Content-Type": "text/x-nix-cache-info"}, self.cache_info.encode()
        if path == "/_gate/health":
            with self.lock:
                body = {"cache": self.cache, "repo": self.repo, "commit": self.commit, "narinfos": len(self.hashes),
                        "asked": dict(self.counts)}
            return 200, {"Content-Type": "application/json"}, json.dumps(body).encode()
        m = HASH_FILE_RE.match(path)
        if m and m.group(1) not in self.hashes:
            self.count("not_held")
            return 404, {"Content-Type": "text/plain"}, b"not in Rime's cache\n"
        self.count("redirected" if m else "other")
        # Everything else (a held narinfo, a relative NAR URL, a log) is the cache's own answer.
        return 302, {"Location": self.cache + raw_path, "Content-Length": "0"}, b""


def handler_for(gate):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def _answer(self, with_body):
            status, headers, body = gate.route(self.path)
            self.send_response(status)
            for k, v in headers.items():
                self.send_header(k, v)
            if "Content-Length" not in headers:
                self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if with_body and body:
                self.wfile.write(body)

        def do_GET(self):
            self._answer(True)

        def do_HEAD(self):
            self._answer(False)

        def log_message(self, *_):
            pass

    return Handler


def listen_address(gate_url):
    u = urllib.parse.urlsplit(gate_url)
    if u.scheme != "http" or u.hostname not in ("127.0.0.1", "localhost") or not u.port or u.path not in ("", "/"):
        raise Refused(f"the gate listens on http://127.0.0.1:PORT only, not {gate_url!r}")
    return "127.0.0.1", u.port


def health(gate_url, timeout=5.0):
    """The gate's health document, or None when it does not answer."""
    try:
        with urllib.request.urlopen(gate_url.rstrip("/") + "/_gate/health", timeout=timeout) as r:
            return json.loads(r.read().decode())
    except (OSError, ValueError, urllib.error.URLError):
        return None


def serve(a):
    host, port = listen_address(a.gate)
    with tempfile.TemporaryDirectory(prefix="cache-gate-") as work:
        hashes, info, commit = fetch_index(a.index_repo, a.branch, work)
    gate = Gate(a.cache, hashes, info, a.index_repo, commit)
    srv = ThreadingHTTPServer((host, port), handler_for(gate))
    srv.daemon_threads = True
    print(f"cache_gate: {len(hashes)} narinfos indexed from {a.index_repo} {commit[:12]}; serving {a.gate} for "
          f"{a.cache}", flush=True)
    srv.serve_forever()


def ensure(a, popen=subprocess.Popen, sleep=time.sleep, clock=time.monotonic, ask=health):
    """Start the gate unless one for the same cache answers already. Idempotent: every step that runs Nix may call it."""
    h = ask(a.gate)
    if h and h.get("cache") == a.cache.rstrip("/") and h.get("narinfos", 0) > 0:
        print(f"cache_gate: answering, {h['narinfos']} narinfos indexed ({h.get('commit', '')[:12]})", flush=True)
        return 0
    if h:
        raise Refused(f"something else answers on {a.gate}: {h}")
    os.makedirs(a.state, exist_ok=True)
    log = os.path.join(a.state, "cache-gate.log")
    with open(log, "ab") as out, open(os.devnull, "rb") as nul:
        child = popen([sys.executable, os.path.abspath(__file__), "serve", "--gate", a.gate, "--cache", a.cache,
                       "--index-repo", a.index_repo, "--branch", a.branch],
                      stdin=nul, stdout=out, stderr=out, start_new_session=True, close_fds=True)
    end = clock() + a.wait
    while clock() < end:
        h = ask(a.gate)
        if h and h.get("narinfos", 0) > 0:
            print(f"cache_gate: started (pid {child.pid}), {h['narinfos']} narinfos indexed from {a.index_repo} "
                  f"{h.get('commit', '')[:12]}", flush=True)
            return 0
        if child.poll() is not None:
            break
        sleep(1)
    try:
        with open(log, errors="replace") as f:
            tail = f.read()[-2000:]
    except OSError:
        tail = ""
    if child.poll() is None:
        child.kill()
    raise Refused(f"the gate did not come up on {a.gate}:\n{tail}")


def require(a, ask=health):
    """Refuse unless the gate for this cache answers (a retry without --ensure: a step that must not start one)."""
    h = ask(a.gate)
    if not (h and h.get("cache") == a.cache.rstrip("/") and h.get("narinfos", 0) > 0):
        raise Refused(f"the gate on {a.gate} does not answer for {a.cache} ({h}); not running Nix without it, "
                      "since Nix would build what it cannot reach")
    return 0


# ── retry ────────────────────────────────────────────────────────────────────

ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
DOWNLOAD_RE = re.compile(r"^(?:error|warning): unable to download '([^']+)': (.*)$")
HTTP_RE = re.compile(r"HTTP error (\d{3})")
# curl's words for a connection that never answered or broke off (Nix prints them with the curl code, or as
# "HTTP error 200 (curl error: ...)").
TRANSPORT = ("curl error", "Could not connect to server", "Couldn't connect to server", "Couldn't resolve host",
             "Connection reset", "Connection refused", "Timeout was reached", "Operation timed out", "Failure when receiving data",
             "Recv failure", "Send failure", "SSL connect error", "Stream error", "HTTP/2", "Empty reply from server",
             "transfer closed")
# Nix's own follow-on errors after a substitution failed under fallback = false (measured, Nix 2.35: a NAR answering
# 503 prints the download error, then these). Transient only beside a transient download error, never alone.
FOLLOW_ON = ("some substitutes for the outputs of derivation",
             "is required, but there is no substituter that can build it")
# A derivation that could not start because an input failed. Its `Reason:` line tells it from a builder that failed
# ("Reason: builder failed with exit code 1"), which is never transient.
CANNOT_BUILD = "error: Cannot build '"
DEPENDENCY_FAILED_RE = re.compile(r"^\s*Reason: \d+ dependenc(?:y|ies) failed\.\s*$")
REASON_RE = re.compile(r"^\s*Reason: ")


def classify_download(line, prefixes):
    """'transient' or 'fatal' for an `unable to download` line of a gated URL; None for any other line."""
    m = DOWNLOAD_RE.match(line)
    if not m or not any(m.group(1).startswith(p) for p in prefixes):
        return None
    rest = m.group(2)
    code = HTTP_RE.search(rest)
    if code and code.group(1) != "200":
        c = int(code.group(1))
        return "transient" if c == 429 or 500 <= c <= 599 else "fatal"
    if any(t in rest for t in TRANSPORT):
        return "transient"
    return "fatal"


def transient(lines, prefixes):
    """(retry?, why) for a failed Nix command's stderr. Retry only when every top-level error is a transient download
    error of a gated URL, or one of Nix's follow-on errors after one; a build failure, a refused signature, a 404 or
    anything unrecognised fails at once. Warnings decide nothing: a download Nix retried into success is no reason."""
    errors, seen = [], []
    for raw in lines:
        ln = ANSI_RE.sub("", raw.rstrip("\r\n"))
        if ln.startswith("error:"):
            kind = classify_download(ln, prefixes)
            if kind is None and any(f in ln for f in FOLLOW_ON):
                kind = "follow-on"
            elif kind is None and ln.startswith(CANNOT_BUILD):
                kind = "cannot-build"  # decided by its Reason: line
            if kind == "transient":
                seen.append(ln)
            errors.append([ln, kind])
        elif REASON_RE.match(ln) and errors and errors[-1][1] == "cannot-build":
            errors[-1][1] = "follow-on" if DEPENDENCY_FAILED_RE.match(ln) else None
    if not errors:
        return False, "no error line from Nix"
    for ln, kind in errors:
        if kind not in ("transient", "follow-on"):
            return False, ln[:300]
    if not seen:  # follow-ons alone: no transient download error to retry
        return False, errors[-1][0][:300]
    return True, seen[-1][:300]


def backoff(n, rand=random.random, base=RETRY_BASE):
    """Seconds to wait after failed try n (1-based)."""
    return min(RETRY_CAP, base * 2 ** (n - 1)) * (1 + 0.5 * rand())


def run_streaming(cmd):
    """Run cmd with stdout inherited and stderr passed through line by line; (exit code, the lines kept). Only lines
    the classifier reads are kept (errors and download failures), so a long -L build log costs no memory."""
    p = subprocess.Popen(cmd, stderr=subprocess.PIPE)
    kept = []
    for b in p.stderr:
        sys.stderr.buffer.write(b)
        sys.stderr.buffer.flush()
        s = b.decode("utf-8", "replace")
        if s.startswith(("error:", "warning: unable to download", "\x1b")) or REASON_RE.match(s):
            kept.append(s)
            del kept[:-500]
    return p.wait(), kept


def retry(cmd, prefixes, tries=RETRY_TRIES, run=run_streaming, sleep=time.sleep, rand=random.random,
          before=lambda: None, say=lambda m: print(m, file=sys.stderr, flush=True), base=RETRY_BASE):
    """The exit code of the last try. Each try reruns the same command: what earlier tries substituted or built stays
    in the store, so a try only asks again about what is still missing."""
    code = 1
    for n in range(1, tries + 1):
        # Before every try, the first too: a Nix command that opens the gate's URL while nothing answers there drops
        # the substituter for that command and BUILDS what it holds (measured, Nix 2.35.4), so it must answer first.
        before()
        code, lines = run(cmd)
        if code == 0:
            return 0
        again, why = transient(lines, prefixes)
        if not again:
            if n > 1:
                say(f"cache_gate: try {n} of {tries} failed, not on a transient error of Rime's cache: {why}")
            return code
        if n == tries:
            say(f"{_annot('error')}cache_gate: Rime's cache still failed after {tries} tries ({why}); giving up")
            return code
        wait = backoff(n, rand, base)
        say(f"{_annot('warning')}cache_gate: try {n} of {tries} failed on a transient error of Rime's cache ({why}); "
            f"the same command again in {wait:.0f} s")
        sleep(wait)
    return code


def _annot(kind):
    return f"::{kind}::" if os.environ.get("GITHUB_ACTIONS") == "true" else ""


# ── command line ─────────────────────────────────────────────────────────────


def parse(argv):
    env = os.environ
    repo = env.get("CACHE_REPO", "")
    p = argparse.ArgumentParser(prog="cache_gate.py")
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("ensure", "serve", "retry"):
        s = sub.add_parser(name)
        s.add_argument("--gate", default=env.get("RIME_CACHE_GATE", ""))
        s.add_argument("--cache", default=env.get("CACHE_URL", ""))
        s.add_argument("--index-repo", default=f"https://github.com/{repo}.git" if repo else "")
        s.add_argument("--branch", default="main")
        s.add_argument("--state", default=os.path.join(env.get("RUNNER_TEMP", tempfile.gettempdir()), "cache-gate"))
        s.add_argument("--wait", type=float, default=ENSURE_WAIT)
        if name == "retry":
            s.add_argument("--tries", type=int, default=RETRY_TRIES)
            s.add_argument("--backoff", type=float, default=RETRY_BASE, help="seconds before the 2nd try (doubled after)")
            s.add_argument("--ensure", action="store_true", help="make sure the gate answers before every try")
            s.add_argument("--url", action="append", default=[],
                           help="a URL prefix whose transient errors are retried (default: the gate, the cache and "
                                "the cache repository's release assets)")
            s.add_argument("command", nargs=argparse.REMAINDER)
    a = p.parse_args(argv)
    need = ["gate", "cache", "index_repo"] if a.cmd != "retry" or a.ensure else (["cache"] if a.gate else [])
    for k in need:
        if not getattr(a, k):
            p.error(f"--{k.replace('_', '-')} (or its environment variable) is required")
    return a


def _before(a):
    if a.ensure:
        return lambda: ensure(a)
    if a.gate:
        return lambda: require(a)
    return lambda: None


def main(argv):
    a = parse(argv)
    try:
        if a.cmd == "serve":
            serve(a)
            return 0
        if a.cmd == "ensure":
            return ensure(a)
        cmd = a.command[1:] if a.command[:1] == ["--"] else a.command
        if not cmd:
            raise Refused("retry: no command after --")
        prefixes = a.url or [u.rstrip("/") + "/" for u in (a.gate, a.cache) if u]
        if not a.url and os.environ.get("CACHE_REPO"):
            prefixes.append(f"https://github.com/{os.environ['CACHE_REPO']}/releases/download/")
        if not prefixes:
            raise Refused("retry: no URL to retry on (--url, or RIME_CACHE_GATE / CACHE_URL)")
        return retry(cmd, prefixes, tries=a.tries, base=a.backoff,
                     before=_before(a))
    except Refused as e:
        print(f"{_annot('error')}cache_gate: {e}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
