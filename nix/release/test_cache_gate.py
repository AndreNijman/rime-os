"""Unit tests for cache_gate.py (no network, no Nix). Run: python3 -m unittest test_cache_gate.

RIME_CACHE_GATE_MODULE may point at another copy of the tool (the mutation checks run this suite against
deliberately broken copies and require it to fail). The Nix messages below are copied from real runs: run 44472232881
(the release that failed) and the local proof with Nix 2.35 in the rime-nix container.
"""

import importlib.util
import json
import os
import subprocess
import tempfile
import types
import unittest

_path = os.environ.get("RIME_CACHE_GATE_MODULE") or os.path.join(os.path.dirname(__file__), "cache_gate.py")
_spec = importlib.util.spec_from_file_location("cache_gate", _path)
g = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(g)

H = "0123456789abcdfghijklmnpqrsvwxyz"
HELD = "2p5dhmmkqmh2m3srxgy5w5b5p55v01ap"
MISSING = "fy1qjzanfywcmksp8j6v09dcdwp10i06"
CACHE = "https://cache.rimeos.com"
GATE = "http://127.0.0.1:37515"
PREFIXES = [GATE + "/", CACHE + "/", "https://github.com/AndreNijman/rime-cache/releases/download/"]
INFO = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 50\n"


def dl(kind, url, rest):
    return f"{kind}: unable to download '{url}': {rest}\n"


# Real lines.
GATE_503 = dl("error", f"{GATE}/{MISSING}.narinfo", "HTTP error 503")
CACHE_503 = dl("error", f"{CACHE}/{HELD}.narinfo", "HTTP error 503")
WARN_503 = dl("warning", f"{CACHE}/{HELD}.narinfo", "HTTP error 503")
CONNECT = dl("error", f"{GATE}/{MISSING}.narinfo",
             "Could not connect to server (7) Failed to connect to 127.0.0.1:37515 after 0 ms: Could not connect to "
             "server")
BAD_SIG = "error: signature is not valid\n"
CANNOT_BUILD = "error: Cannot build '/nix/store/" + H + "-gate-proof-held-a.drv'.\n"
BUILDER = "error: builder for '/nix/store/" + H + "-gate-proof-held-a.drv' failed with exit code 1\n"
# A NAR answering 503 in the build phase (case I of the proof, Nix 2.35): the download error, then Nix's follow-ons.
NAR_503 = [
    dl("error", f"{GATE}/nar/1dikbm0p80imkqq7pn8ppp5mwypj81b6i0yy6z60m8hd592bvdwy.nar.xz", "HTTP error 503"),
    "error: path '/nix/store/" + H + "-gate-proof-held-a' is required, but there is no substituter that can build it\n",
    "error: some substitutes for the outputs of derivation '/nix/store/" + H + "-gate-proof-held-a.drv' failed "
    "(usually happens due to networking issues); try '--fallback' to build derivation from source\n",
    "error: Cannot build '/nix/store/" + H + "-gate-proof-new.drv'.\n",
    "       Reason: 1 dependency failed.\n",
]
BUILD_FAILED = ["error: Cannot build '/nix/store/" + H + "-gate-proof-held-a.drv'.\n",
                "       Reason: builder failed with exit code 1.\n"]


class Index(unittest.TestCase):
    def test_listing_keeps_narinfo_hashes_only(self):
        names = [f"{HELD}.narinfo", f"{HELD}.ls", "nix-cache-info", "nar", "index.html", f"{MISSING}.narinfo\n",
                 "README.narinfo", "e" * 32 + ".narinfo"]  # 'e' is not a nix32 letter
        self.assertEqual(g.index_from_listing(names), {HELD, MISSING})

    def test_cache_info_must_name_the_nix_store(self):
        self.assertEqual(g.check_cache_info(INFO), INFO)
        with self.assertRaises(g.NotACache):
            g.check_cache_info("StoreDir: /gnu/store\n")
        with self.assertRaises(g.NotACache):
            g.check_cache_info("<html>503</html>")


def fake_git(listing, info=INFO, fail_clone=0):
    """A subprocess.run stand-in for the four git commands fetch_index runs."""
    state = {"clones": 0, "calls": []}

    def run(cmd, **_):
        state["calls"].append(cmd)
        out = types.SimpleNamespace(returncode=0, stdout="", stderr="")
        if "clone" in cmd:
            state["clones"] += 1
            if state["clones"] <= fail_clone:
                out.returncode, out.stderr = 128, "fatal: unable to access: The requested URL returned error: 503"
        elif "ls-tree" in cmd:
            out.stdout = "\n".join(listing) + "\n"
        elif "cat-file" in cmd:
            out.stdout = info
        elif "rev-parse" in cmd:
            out.stdout = "c" * 40 + "\n"
        return out

    return run, state


class FetchIndex(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.mkdtemp()
        self.sleeps = []

    def fetch(self, run, tries=4):
        return g.fetch_index("https://github.com/o/c.git", "main", self.work, run=run, sleep=self.sleeps.append,
                             tries=tries)

    def test_reads_hashes_info_and_commit(self):
        run, state = fake_git(["nix-cache-info", f"{HELD}.narinfo", "nar"])
        hashes, info, commit = self.fetch(run)
        self.assertEqual((hashes, info, commit), ({HELD}, INFO, "c" * 40))
        clone = state["calls"][0]
        for flag in ("--depth", "--filter=blob:none", "--no-checkout", "--single-branch"):
            self.assertIn(flag, clone)
        self.assertEqual(self.sleeps, [])

    def test_a_failed_clone_is_retried_with_backoff(self):
        run, state = fake_git(["nix-cache-info", f"{HELD}.narinfo"], fail_clone=2)
        self.assertEqual(self.fetch(run)[0], {HELD})
        self.assertEqual(state["clones"], 3)
        self.assertEqual(self.sleeps, [g.INDEX_DELAY, g.INDEX_DELAY * 2])

    def test_gives_up_after_its_tries(self):
        run, state = fake_git(["nix-cache-info", f"{HELD}.narinfo"], fail_clone=99)
        with self.assertRaises(g.Refused) as e:
            self.fetch(run, tries=3)
        self.assertNotIsInstance(e.exception, g.NotACache)
        self.assertEqual(state["clones"], 3)
        self.assertEqual(len(self.sleeps), 2)

    def test_no_cache_info_is_refused_at_once(self):
        run, state = fake_git([f"{HELD}.narinfo"])
        with self.assertRaises(g.NotACache):
            self.fetch(run)
        self.assertEqual(state["clones"], 1)

    def test_an_empty_index_is_refused_at_once(self):
        # An empty index would answer 404 for every Rime path, so the build would rebuild them all.
        run, state = fake_git(["nix-cache-info", "nar", "index.html"])
        with self.assertRaises(g.NotACache):
            self.fetch(run)
        self.assertEqual(state["clones"], 1)

    def test_a_cache_info_for_another_store_is_refused(self):
        run, _ = fake_git(["nix-cache-info", f"{HELD}.narinfo"], info="StoreDir: /gnu/store\n")
        with self.assertRaises(g.NotACache):
            self.fetch(run)


class Route(unittest.TestCase):
    def setUp(self):
        self.gate = g.Gate(CACHE + "/", {HELD}, INFO, "https://github.com/o/c.git", "c" * 40)

    def test_a_held_narinfo_is_redirected_to_the_cache(self):
        status, headers, body = self.gate.route(f"/{HELD}.narinfo")
        self.assertEqual(status, 302)
        self.assertEqual(headers["Location"], f"{CACHE}/{HELD}.narinfo")
        self.assertEqual(body, b"")

    def test_a_narinfo_not_held_is_404_here(self):
        for path in (f"/{MISSING}.narinfo", f"/{MISSING}.ls"):
            self.assertEqual(self.gate.route(path)[0], 404, path)

    def test_nars_and_other_paths_are_redirected(self):
        for path in ("/nar/" + "0" * 52 + ".nar.zst", f"/log/{MISSING}-x.drv", "/index.html"):
            status, headers, _ = self.gate.route(path)
            self.assertEqual((status, headers["Location"]), (302, CACHE + path), path)

    def test_the_query_string_is_kept_and_not_matched(self):
        status, headers, _ = self.gate.route(f"/{HELD}.narinfo?x=1")
        self.assertEqual((status, headers["Location"]), (302, f"{CACHE}/{HELD}.narinfo?x=1"))
        self.assertEqual(self.gate.route(f"/{MISSING}.narinfo?x=1")[0], 404)

    def test_cache_info_is_the_caches_own(self):
        status, _, body = self.gate.route("/nix-cache-info")
        self.assertEqual((status, body), (200, INFO.encode()))

    def test_health_counts_what_was_asked(self):
        self.gate.route(f"/{HELD}.narinfo")
        self.gate.route(f"/{MISSING}.narinfo")
        self.gate.route(f"/{MISSING}.narinfo")
        self.gate.route("/nar/x.nar.zst")
        status, _, body = self.gate.route("/_gate/health")
        h = json.loads(body)
        self.assertEqual(status, 200)
        self.assertEqual(h["cache"], CACHE)
        self.assertEqual(h["narinfos"], 1)
        self.assertEqual(h["asked"], {"redirected": 1, "not_held": 2, "other": 1})


class Listen(unittest.TestCase):
    def test_loopback_http_only(self):
        self.assertEqual(g.listen_address("http://127.0.0.1:37515"), ("127.0.0.1", 37515))
        self.assertEqual(g.listen_address("http://localhost:37515/"), ("127.0.0.1", 37515))
        for bad in ("https://127.0.0.1:37515", "http://0.0.0.0:37515", "http://10.0.0.1:37515", "http://127.0.0.1",
                    "http://127.0.0.1:37515/cache", CACHE):
            with self.assertRaises(g.Refused, msg=bad):
                g.listen_address(bad)


class Classify(unittest.TestCase):
    def test_retryable(self):
        cases = {
            "503": GATE_503,
            "503 from the cache itself": CACHE_503,
            "429": dl("error", f"{CACHE}/{HELD}.narinfo", "HTTP error 429"),
            "500": dl("error", f"{GATE}/nar/x.nar.zst", "HTTP error 500"),
            "502": dl("error", f"{CACHE}/nar/x.nar.zst", "HTTP error 502"),
            "connection refused": CONNECT,
            "connection reset": dl("error", f"{CACHE}/nar/x.nar.zst",
                                   "HTTP error 200 (curl error: Recv failure: Connection reset by peer)"),
            "timeout": dl("error", f"{CACHE}/{HELD}.narinfo", "curl error: Timeout was reached"),
            "HTTP/2 stream": dl("error", f"{CACHE}/nar/x.nar.zst",
                                "HTTP error 200 (curl error: Stream error in the HTTP/2 framing layer)"),
            "release asset": dl("error", PREFIXES[2] + "nar-1/x.nar.zst", "HTTP error 503"),
        }
        for name, line in cases.items():
            self.assertEqual(g.classify_download(line, PREFIXES), "transient", name)
            self.assertEqual(g.transient([line], PREFIXES)[0], True, name)

    def test_not_retryable(self):
        cases = {
            "404 on a NAR": [dl("error", f"{CACHE}/nar/x.nar.zst", "HTTP error 404")],
            "403": [dl("error", f"{CACHE}/{HELD}.narinfo", "HTTP error 403")],
            "signature mismatch": [WARN_503, BAD_SIG],
            "build failure": [CANNOT_BUILD],
            "builder failed": [BUILDER],
            "upstream does not serve": ["error: REFUSED: upstream does not serve /nix/store/" + H + "-x\n"],
            "a 503 from somewhere else": [dl("error", "https://cache.nixos.org/" + HELD + ".narinfo",
                                             "HTTP error 503")],
            "unknown download error": [dl("error", f"{CACHE}/{HELD}.narinfo", "file is not a narinfo")],
            "no error line": [WARN_503, "these 3 derivations will be built:\n"],
            "nothing": [],
        }
        for name, lines in cases.items():
            self.assertEqual(g.transient(lines, PREFIXES)[0], False, name)

    def test_a_nar_503_with_nix_follow_ons_is_retried(self):
        self.assertEqual(g.transient(NAR_503, PREFIXES)[0], True)

    def test_cannot_build_is_decided_by_its_reason(self):
        self.assertFalse(g.transient([GATE_503] + BUILD_FAILED, PREFIXES)[0])
        self.assertFalse(g.transient(NAR_503[:-1] + ["       Reason: builder failed with exit code 1.\n"],
                                     PREFIXES)[0])
        self.assertFalse(g.transient(NAR_503[:-1], PREFIXES)[0])  # no Reason line: unknown, not retried
        self.assertFalse(g.transient(BUILD_FAILED, PREFIXES)[0])

    def test_follow_ons_alone_never_retry(self):
        self.assertFalse(g.transient(NAR_503[1:], PREFIXES)[0])
        self.assertFalse(g.transient([WARN_503] + NAR_503[1:], PREFIXES)[0])

    def test_a_build_failure_after_a_transient_download_is_not_retried(self):
        # Every top-level error must be transient: a builder that failed is a real failure.
        self.assertFalse(g.transient([GATE_503, BUILDER], PREFIXES)[0])

    def test_warnings_alone_never_retry(self):
        self.assertFalse(g.transient([WARN_503, WARN_503, CANNOT_BUILD], PREFIXES)[0])

    def test_follow_on_summary_counts_only_with_a_transient_download(self):
        follow = "error: some substitutes for the outputs of derivation '/nix/store/" + H + "-x.drv' failed\n"
        self.assertTrue(g.transient([GATE_503, follow], PREFIXES)[0])
        self.assertFalse(g.transient([follow], PREFIXES)[0])
        self.assertFalse(g.transient([WARN_503, follow], PREFIXES)[0])
        self.assertFalse(g.transient([BAD_SIG, follow], PREFIXES)[0])

    def test_colour_codes_are_ignored(self):
        self.assertTrue(g.transient(["\x1b[31;1merror:\x1b[0m" + GATE_503[len("error:"):]], PREFIXES)[0])

    def test_why_names_the_line(self):
        again, why = g.transient([GATE_503], PREFIXES)
        self.assertTrue(again)
        self.assertIn("HTTP error 503", why)
        again, why = g.transient([BAD_SIG], PREFIXES)
        self.assertFalse(again)
        self.assertIn("signature is not valid", why)


class Backoff(unittest.TestCase):
    def test_doubles_and_caps(self):
        self.assertEqual([g.backoff(n, rand=lambda: 0.0, base=90) for n in (1, 2, 3, 4, 5)], [90, 180, 360, 600, 600])

    def test_jitter_adds_up_to_half(self):
        self.assertEqual(g.backoff(1, rand=lambda: 1.0, base=90), 135)
        self.assertEqual(g.backoff(9, rand=lambda: 1.0, base=90), 900)


class Retry(unittest.TestCase):
    def go(self, results, tries=4, before=None):
        ran, slept, said, befores = [], [], [], []
        it = iter(results)

        def run(cmd):
            ran.append(cmd)
            return next(it)

        code = g.retry(["nix", "build"], PREFIXES, tries=tries, run=run, sleep=slept.append, rand=lambda: 0.0,
                       before=before or (lambda: befores.append(1)), say=said.append, base=10)
        return code, ran, slept, said, befores

    def test_success_first_time(self):
        code, ran, slept, _, befores = self.go([(0, [])])
        self.assertEqual((code, len(ran), slept, len(befores)), (0, 1, [], 1))

    def test_transient_then_success(self):
        code, ran, slept, said, befores = self.go([(1, [GATE_503]), (1, [CONNECT]), (0, [])])
        self.assertEqual(code, 0)
        self.assertEqual(len(ran), 3)
        self.assertEqual(slept, [10, 20])
        self.assertEqual(len(befores), 3)  # the gate is checked before every try, the first too
        self.assertIn("transient", said[0])

    def test_bounded(self):
        code, ran, slept, said, _ = self.go([(1, [GATE_503])] * 10, tries=3)
        self.assertEqual((code, len(ran), slept), (1, 3, [10, 20]))
        self.assertIn("giving up", said[-1])

    def test_a_real_failure_stops_at_once(self):
        code, ran, slept, _, _ = self.go([(1, [BAD_SIG])])
        self.assertEqual((code, len(ran), slept), (1, 1, []))
        code, ran, slept, _, _ = self.go([(1, [GATE_503]), (1, [BUILDER])])
        self.assertEqual((code, len(ran), slept), (1, 2, [10]))

    def test_the_last_exit_code_is_returned(self):
        self.assertEqual(self.go([(1, [GATE_503]), (100, [GATE_503])], tries=2)[0], 100)
        self.assertEqual(self.go([(100, [BUILDER])])[0], 100)

    def test_a_refusal_before_a_try_stops_without_running_nix(self):
        ran = []

        def before():
            raise g.Refused("gate down")

        with self.assertRaises(g.Refused):
            g.retry(["nix"], PREFIXES, run=lambda c: ran.append(c) or (0, []), sleep=lambda s: None, before=before)
        self.assertEqual(ran, [])


def args(**kw):
    a = types.SimpleNamespace(gate=GATE, cache=CACHE, index_repo="https://github.com/o/c.git", branch="main",
                              state=tempfile.mkdtemp(), wait=3.0)
    a.__dict__.update(kw)
    return a


class FakeChild:
    pid = 4242

    def __init__(self, exits=None):
        self.exits, self.killed = exits, False

    def poll(self):
        return self.exits

    def kill(self):
        self.killed = True


class Ensure(unittest.TestCase):
    def test_an_answering_gate_is_left_alone(self):
        spawned = []
        ok = {"cache": CACHE, "narinfos": 3725, "commit": "c" * 40}
        self.assertEqual(g.ensure(args(), popen=lambda *a, **k: spawned.append(a), ask=lambda u: ok), 0)
        self.assertEqual(spawned, [])

    def test_a_gate_for_another_cache_is_refused(self):
        with self.assertRaises(g.Refused):
            g.ensure(args(), popen=lambda *a, **k: FakeChild(), ask=lambda u: {"cache": "http://x", "narinfos": 9})

    def test_starts_one_detached_and_waits_for_it(self):
        answers = iter([None, None, {"cache": CACHE, "narinfos": 5}])
        spawned = []

        def popen(cmd, **kw):
            spawned.append((cmd, kw))
            return FakeChild()

        clock = iter(range(100))
        self.assertEqual(g.ensure(args(), popen=popen, sleep=lambda s: None, clock=lambda: next(clock),
                                  ask=lambda u: next(answers)), 0)
        cmd, kw = spawned[0]
        self.assertIn("serve", cmd)
        self.assertTrue(kw["start_new_session"])

    def test_a_gate_that_never_answers_is_killed_and_refused(self):
        child = FakeChild()
        clock = iter(range(100))
        with self.assertRaises(g.Refused):
            g.ensure(args(wait=3), popen=lambda *a, **k: child, sleep=lambda s: None, clock=lambda: next(clock),
                     ask=lambda u: None)
        self.assertTrue(child.killed)

    def test_a_gate_that_exits_is_refused(self):
        clock = iter(range(100))
        with self.assertRaises(g.Refused):
            g.ensure(args(wait=50), popen=lambda *a, **k: FakeChild(exits=1), sleep=lambda s: None,
                     clock=lambda: next(clock), ask=lambda u: None)


class Require(unittest.TestCase):
    def test_answers_or_refuses(self):
        self.assertEqual(g.require(args(), ask=lambda u: {"cache": CACHE, "narinfos": 1}), 0)
        for h in (None, {"cache": CACHE, "narinfos": 0}, {"cache": "http://elsewhere", "narinfos": 9}):
            with self.assertRaises(g.Refused, msg=str(h)):
                g.require(args(), ask=lambda u, h=h: h)


class Command(unittest.TestCase):
    """The command line, end to end in a subprocess (no gate is running, so a check refuses)."""

    def run_tool(self, *argv, env=None):
        e = {k: v for k, v in os.environ.items() if k not in ("RIME_CACHE_GATE", "CACHE_URL", "CACHE_REPO")}
        e.update(env or {})
        return subprocess.run(["python3", _path, *argv], capture_output=True, text=True, env=e, timeout=60)

    def test_retry_without_a_command_is_refused(self):
        r = self.run_tool("retry", "--url", "http://x/", "--")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no command", r.stderr)

    def test_retry_refuses_to_run_nix_when_the_gate_does_not_answer(self):
        mark = os.path.join(tempfile.mkdtemp(), "ran")
        r = self.run_tool("retry", "--", "touch", mark,
                          env={"RIME_CACHE_GATE": "http://127.0.0.1:9", "CACHE_URL": CACHE})
        self.assertEqual(r.returncode, 1)
        self.assertIn("does not answer", r.stderr)
        self.assertFalse(os.path.exists(mark))

    def test_retry_passes_the_exit_code_through(self):
        r = self.run_tool("retry", "--url", "http://x/", "--", "sh", "-c", "exit 7")
        self.assertEqual(r.returncode, 7)


if __name__ == "__main__":
    unittest.main()
