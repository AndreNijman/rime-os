"""Unit tests for rime_nix_release.py (no network, no Nix store). Run: python3 -m unittest test_rime_nix_release.

RIME_NIX_RELEASE_MODULE may point at another copy of the tool (the mutation checks run this suite against
deliberately broken copies and require it to fail). RIME_NEXT_RELEASE_ID points at files/scripts/next-release-id.
"""

import base64
import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest

_path = os.environ.get("RIME_NIX_RELEASE_MODULE") or os.path.join(os.path.dirname(__file__), "rime_nix_release.py")
_spec = importlib.util.spec_from_file_location("rime_nix_release", _path)
r = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(r)

H = "0123456789abcdfghijklmnpqrsvwxyz"


def sp(n, name):
    """A syntactically valid store path."""
    return "/nix/store/" + (H[n % 32] * 32) + "-" + name


def nh(seed):
    return "sha256-" + base64.b64encode(hashlib.sha256(seed.encode()).digest()).decode()


KEY = "rime-test-cache-1:" + base64.b64encode(b"k" * 32).decode()
SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_N = "c" * 40


def synthetic():
    """closure.json, plan and public sizes for a toplevel -> {kernel, initrd, modules, shell -> glibc}."""
    glibc, shell, kernel, initrd, mods, top, stray = (sp(i, n) for i, n in enumerate(
        ["glibc-2.42", "rime-shell", "linux-7.2.9", "initrd", "kernel-modules", "nixos-system-rime", "stray"]))
    refs = {glibc: [], shell: [glibc], kernel: [], initrd: [], mods: [kernel], top: [kernel, initrd, mods, shell]}
    clo = {
        "schema": 1,
        "variants": {"plain": {"toplevel": top, "boot": {"kernel": kernel, "initrd": initrd, "kernel_modules": mods,
                                                          "kernel_params": ["quiet"]}}},
        "catalogue_outs": [],
        "paths": {p: {"nar_hash": nh(p), "nar_size": 1000 + i, "references": sorted(rs)}
                  for i, (p, rs) in enumerate(sorted(refs.items()))},
    }
    plan = {"variants": {"plain": {"toplevel": top, "publishable": True,
                                   "components": {"shell": [shell], "kernel": [kernel, mods], "libraries": [glibc],
                                                  "nvidia": [stray]}}},
            "catalogue": []}
    sizes = {p: 100 + i for i, p in enumerate(sorted(refs))}
    return clo, plan, sizes, dict(glibc=glibc, shell=shell, kernel=kernel, top=top, stray=stray)


def manifest(**kw):
    clo, plan, sizes, _ = synthetic()
    args = dict(release="2026.11.01", previous=None, issued=1796260000,
                source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY])
    args.update(kw)
    return r.build_manifest(clo, plan, sizes, **args)


class Hashes(unittest.TestCase):
    def test_nix32_matches_nix(self):
        # nix hash convert --to sri sha256:0b56hsfz… (GNU hello 2.12.3's narHash, both forms printed by Nix 2.35)
        self.assertEqual(r.sri("sha256:0b56hsfzp1d4kx9j095zk64b0zmi189s3ih8mk2zyshpi48fgw76"),
                         "sha256-5vDnEIkXav/FrAjGoRMKsX6wiJm/JCBTn6SF+52Gpiw=")

    def test_sri_forms(self):
        raw = hashlib.sha256(b"x").digest()
        s = "sha256-" + base64.b64encode(raw).decode()
        self.assertEqual(r.sri(s), s)
        self.assertEqual(r.sri("sha256:" + raw.hex()), s)
        self.assertEqual(r.sri({"algorithm": "sha256", "format": "base64", "hash": base64.b64encode(raw).decode()}), s)
        for bad in ("md5-AAAA", "sha256-AAAA", "sha256:zz", 7, {"algorithm": "sha512"}):
            with self.assertRaises(ValueError):
                r.sri(bad)


class Pure(unittest.TestCase):
    def test_canonical(self):
        self.assertEqual(r.canonical({"b": 1, "a": [1, {"d": 2, "c": "é"}]}), '{"a":[1,{"c":"é","d":2}],"b":1}'.encode())

    def test_release_ids_order(self):
        ids = ["2026.11.01.10", "2026.11.01", "2026.10.31", "2026.11.01.2"]
        self.assertEqual(sorted(ids, key=r.parse_release), ["2026.10.31", "2026.11.01", "2026.11.01.2", "2026.11.01.10"])
        for bad in ("2026.11.1", "2026.11.01.1", "2026.11.01.0", "nix", ""):
            with self.assertRaises(ValueError):
                r.parse_release(bad)

    def test_repo_of_keeps_ports(self):
        self.assertEqual(r.repo_of("server:5000/andrenijman/rime-os:nix-x"), "server:5000/andrenijman/rime-os")
        self.assertEqual(r.repo_of("server:5000/andrenijman/rime-os"), "server:5000/andrenijman/rime-os")
        self.assertEqual(r.repo_of("ghcr.io/a/b@sha256:00"), "ghcr.io/a/b")

    def test_store_paths(self):
        self.assertTrue(r.valid_store_path(sp(1, "hello-2.12.3")))
        for bad in ("/nix/store/short-x", sp(1, ".hidden"), "/tmp/" + "a" * 32 + "-x", sp(1, "a b"), None):
            self.assertFalse(r.valid_store_path(bad))
        self.assertEqual(r.store_path_of(sp(2, "linux") + "/bzImage"), sp(2, "linux"))

    def test_keys(self):
        self.assertTrue(r.valid_key(KEY))
        self.assertFalse(r.valid_key("rime-test-cache-1:" + base64.b64encode(b"k" * 31).decode()))
        self.assertFalse(r.valid_key("nokey"))

    def test_topo_order_dependencies_first(self):
        refs = {"c": ["b"], "b": ["a"], "a": [], "d": ["a", "c", "x"]}
        order = r.topo_order(["d", "c", "b", "a"], refs)
        for p, rs in refs.items():
            for q in rs:
                if q in order:
                    self.assertLess(order.index(q), order.index(p))
        with self.assertRaises(r.Refused):
            r.topo_order(["a", "b"], {"a": ["b"], "b": ["a"]})


class Manifest(unittest.TestCase):
    def test_good_manifest_validates_and_is_canonical(self):
        m = manifest()
        self.assertEqual(r.validate(m), [])
        b = r.canonical(m)
        self.assertEqual(r.canonical(json.loads(b)), b)
        self.assertEqual([e["path"] for e in m["paths"]], sorted(e["path"] for e in m["paths"]))

    def test_components_outside_the_closure_are_not_listed(self):
        _, _, _, n = synthetic()
        m = manifest()
        table = [e["path"] for e in m["paths"]]
        self.assertNotIn("nvidia", m["variants"]["plain"]["components"])
        self.assertEqual([table[i] for i in m["variants"]["plain"]["components"]["shell"]], [n["shell"]])

    def test_plan_for_another_build_is_refused(self):
        clo, plan, sizes, _ = synthetic()
        plan["variants"]["plain"]["toplevel"] = sp(9, "nixos-system-other")
        with self.assertRaises(r.Refused):
            r.build_manifest(clo, plan, sizes, release="2026.11.01", previous=None, issued=1,
                             source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                             cache_url_hint="", cache_keys=[KEY])

    def test_delta_and_nixpkgs_moved(self):
        prev = manifest()
        prev["source"]["nixpkgs"] = "d" * 40
        prev["paths"] = [e for e in prev["paths"] if "rime-shell" not in e["path"]]
        m = manifest(release="2026.11.02", previous="2026.11.01", previous_manifest=prev)
        self.assertEqual(m["delta"]["paths"], 1)
        self.assertEqual(m["delta"]["from"], "2026.11.01")
        self.assertTrue(m["batch"]["nixpkgs_moved"])
        self.assertEqual(r.validate(m), [])

    def mutated(self, f):
        m = manifest()
        f(m)
        return r.validate(m)

    def test_every_rule_refuses(self):
        cases = {
            "schema": lambda m: m.update(schema=2),
            "kind": lambda m: m.update(kind="rime-bootc-release"),
            "system": lambda m: m.update(system="aarch64-linux"),
            "store_dir": lambda m: m.update(store_dir="/gnu/store"),
            "release": lambda m: m.update(release="nix-1"),
            "source": lambda m: m["source"].update(rime_os="main"),
            "empty keys": lambda m: m["cache"].update(keys=[]),
            "short key": lambda m: m["cache"].update(keys=["rime-cache-1:AAAA"]),
            "bad path": lambda m: m["paths"][0].update(path="/tmp/x"),
            "duplicate": lambda m: m["paths"].insert(1, copy.deepcopy(m["paths"][0])),
            "unsorted": lambda m: m["paths"].reverse(),
            "nar hash": lambda m: m["paths"][0].update(nar_hash="sha256:nothex"),
            "nar size": lambda m: m["paths"][0].update(nar_size=0),
            "file size": lambda m: m["paths"][0].update(file_size=-1),
            "ref range": lambda m: m["paths"][0].update(references=[999]),
            "toplevel range": lambda m: m["variants"]["plain"].update(toplevel=999),
            "boot range": lambda m: m["variants"]["plain"]["boot"].update(kernel=999),
            "component range": lambda m: m["variants"]["plain"]["components"].update(bogus=[999]),
            "no variants": lambda m: m.update(variants={}),
            "catalogue class": lambda m: m.update(catalogue=[{"name": "x", "version": "1", "out": 0, "class": 1}]),
        }
        for name, f in cases.items():
            with self.subTest(name):
                self.assertNotEqual(self.mutated(f), [], name)

    def test_open_closure_is_refused(self):
        # A toplevel whose kernel is reachable only through a reference that is cut.
        m = manifest()
        top = m["variants"]["plain"]["toplevel"]
        m["paths"][top]["references"] = []
        self.assertTrue(any("not in the toplevel's closure" in e for e in r.validate(m)))

    def test_validate_cli_canonical(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "m.json")
            open(p, "wb").write(r.canonical(manifest()))
            self.assertEqual(r.main(["validate", "--canonical", p]), 0)
            open(p, "w").write(json.dumps(manifest(), indent=2))
            self.assertEqual(r.main(["validate", "--canonical", p]), 1)


class Channels(unittest.TestCase):
    D1 = "sha256:" + "1" * 64
    D2 = "sha256:" + "2" * 64

    def test_first_document(self):
        d = r.next_channels(None, "ghcr.io/andrenijman/rime-os", 1000, set_entry={
            "channel": "edge", "digest": self.D1, "release": "2026.11.01", "percent": 100})
        self.assertEqual(d["serial"], 1)
        self.assertEqual(d["expires"] - d["issued"], 14 * 86400)
        self.assertEqual(d["channels"], [{"channel": "edge", "digest": self.D1, "percent": 100, "halt": False,
                                          "release": "2026.11.01"}])

    def test_carry_forward_and_serial(self):
        d = r.next_channels(None, "R", 1000, set_entry={"channel": "beta", "digest": self.D1, "release": "2026.11.01"})
        d = r.next_channels(d, "R", 2000, set_entry={"channel": "edge", "digest": self.D2, "release": "2026.11.02"})
        self.assertEqual(d["serial"], 2)
        self.assertEqual([e["channel"] for e in d["channels"]], ["beta", "edge"])
        d2 = r.next_channels(d, "R", 3000)  # the daily refresh
        self.assertEqual(d2["serial"], 3)
        self.assertEqual(d2["channels"], d["channels"])

    def test_never_backward(self):
        d = r.next_channels(None, "R", 1000, set_entry={"channel": "edge", "digest": self.D2, "release": "2026.11.02"})
        with self.assertRaises(r.Refused):
            r.next_channels(d, "R", 2000, set_entry={"channel": "edge", "digest": self.D1, "release": "2026.11.01"})
        # An entry without a release field is compared through the verified manifest's release.
        d["channels"][0].pop("release")
        with self.assertRaises(r.Refused):
            r.next_channels(d, "R", 2000, set_entry={"channel": "edge", "digest": self.D1, "release": "2026.11.01"},
                            entry_releases={self.D2: "2026.11.02"})
        with self.assertRaises(r.Refused):  # and unknown is refused, not waved through
            r.next_channels(d, "R", 2000, set_entry={"channel": "edge", "digest": self.D1, "release": "2026.11.03"})

    def test_bad_inputs(self):
        for e in ({"channel": "nightly", "digest": self.D1, "release": "2026.11.01"},
                  {"channel": "edge", "digest": "latest", "release": "2026.11.01"},
                  {"channel": "edge", "digest": self.D1, "release": "2026.11.01", "percent": 101},
                  {"channel": "edge", "digest": self.D1, "release": "today"}):
            with self.subTest(e), self.assertRaises((r.Refused, ValueError)):
                r.next_channels(None, "R", 1, set_entry=e)
        d = r.next_channels(None, "R", 1)
        with self.assertRaises(r.Refused):
            r.next_channels(d, "OTHER", 2)

    def test_check_live(self):
        with tempfile.TemporaryDirectory() as dd:
            p = os.path.join(dd, "c.json")
            json.dump(r.next_channels(None, "R", 1000), open(p, "w"))
            self.assertEqual(r.main(["channels", "--current", p, "--check-live", "--now", str(1000 + 6 * 86400)]), 0)
            self.assertEqual(r.main(["channels", "--current", p, "--check-live", "--now", str(1000 + 8 * 86400)]), 1)


class ReleaseId(unittest.TestCase):
    script = os.environ.get("RIME_NEXT_RELEASE_ID")

    def setUp(self):
        if not self.script:
            self.skipTest("RIME_NEXT_RELEASE_ID not set")

    def test_rules(self):
        f = lambda cands: r.next_release_id(self.script, "2026.11.01", SHA_A, SHA_B, cands)
        self.assertEqual(f([]), "2026.11.01")
        self.assertEqual(f([{"id": "2026.10.31", "os": "x", "shell": "y"}]), "2026.11.01")
        self.assertEqual(f([{"id": "2026.11.01", "os": "x", "shell": "y"}]), "2026.11.01.2")
        # the newest of bootc and Nix decides
        self.assertEqual(f([{"id": "2026.11.01", "os": "x", "shell": "y"},
                            {"id": "2026.11.01.3", "os": "x", "shell": "y"}]), "2026.11.01.4")
        # same pair = a reissue keeps its ID
        self.assertEqual(f([{"id": "2026.11.01.3", "os": SHA_A, "shell": SHA_B}]), "2026.11.01.3")


class Oci(unittest.TestCase):
    def test_pack(self):
        with tempfile.TemporaryDirectory() as d:
            digest = r.pack(b'{"x":1}', r.MEDIA_RELEASE, d, {"org.rimeos.product": "nixos"})
            raw = open(os.path.join(d, "manifest.json"), "rb").read()
            self.assertEqual(digest, "sha256:" + hashlib.sha256(raw).hexdigest())
            m = json.loads(raw)
            self.assertEqual(m["config"]["mediaType"], "application/vnd.oci.image.config.v1+json")
            self.assertEqual(m["artifactType"], r.MEDIA_RELEASE)
            self.assertEqual(m["layers"][0]["mediaType"], r.MEDIA_RELEASE)
            self.assertTrue(os.path.exists(os.path.join(d, m["layers"][0]["digest"].split(":")[1])))


def sh(*cmd, **kw):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)


class Signature(unittest.TestCase):
    """verify_sig_dir against signatures made the way keyless cosign makes them (TEST CA, minted here)."""

    REPO = "server:5000/andrenijman/rime-os"

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        t = cls.tmp.name
        cls.ca = cls.mkca(os.path.join(t, "ca"))
        cls.other = cls.mkca(os.path.join(t, "other"))

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    @staticmethod
    def mkca(d):
        os.makedirs(d)
        sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", f"{d}/root.key")
        sh("openssl", "req", "-new", "-x509", "-key", f"{d}/root.key", "-subj", "/CN=TEST root", "-days", "2",
           "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign",
           "-out", f"{d}/root.pem")
        return d

    def leaf(self, ca, san, issuer=r.ISSUER, only_raw_issuer=False):
        d = tempfile.mkdtemp(dir=self.tmp.name)
        sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", f"{d}/leaf.key")
        sh("openssl", "req", "-new", "-key", f"{d}/leaf.key", "-subj", "/CN=TEST leaf", "-out", f"{d}/leaf.csr")
        # Fulcio writes .1.1 as the raw string and .1.8 as a DER UTF8String.
        ext = f"subjectAltName=critical,URI:{san}\n1.3.6.1.4.1.57264.1.1=DER:{issuer.encode().hex()}\n"
        if not only_raw_issuer:
            ext += f"1.3.6.1.4.1.57264.1.8=ASN1:UTF8String:{issuer}\n"
        ext += "keyUsage=critical,digitalSignature\nextendedKeyUsage=codeSigning\n"
        open(f"{d}/leaf.ext", "w").write(ext)
        sh("openssl", "x509", "-req", "-in", f"{d}/leaf.csr", "-CA", f"{ca}/root.pem", "-CAkey", f"{ca}/root.key",
           "-CAcreateserial", "-days", "1", "-extfile", f"{d}/leaf.ext", "-out", f"{d}/leaf.pem")
        return d

    def sigdir(self, leaf, digest, ref=None, tamper=False):
        payload = json.dumps({"critical": {"identity": {"docker-reference": ref or self.REPO},
                                           "image": {"docker-manifest-digest": digest},
                                           "type": "cosign container image signature"}, "optional": None}).encode()
        d = tempfile.mkdtemp(dir=self.tmp.name)
        open(f"{d}/payload", "wb").write(payload)
        sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", f"{leaf}/leaf.key", f"{d}/payload"],
                             check=True, capture_output=True).stdout
        if tamper:
            payload = payload.replace(b"cosign", b"COSIGN")
        hexd = hashlib.sha256(payload).hexdigest()
        open(f"{d}/{hexd}", "wb").write(payload)
        json.dump({"schemaVersion": 2, "mediaType": r.OCI_MANIFEST, "layers": [{
            "mediaType": r.SIG_LAYER, "size": len(payload), "digest": "sha256:" + hexd,
            "annotations": {r.ANN_SIG: base64.b64encode(sig).decode(), r.ANN_CERT: open(f"{leaf}/leaf.pem").read()}}]},
            open(f"{d}/manifest.json", "w"))
        return d

    D = "sha256:" + "e" * 64

    def check(self, sigdir, digest=None, repo=None, root=None):
        return r.verify_sig_dir(sigdir, repo or self.REPO + ":nix-x", digest or self.D,
                                root or f"{self.ca}/root.pem", [r.NIX_SIGNER], r.ISSUER)

    def test_good(self):
        self.assertEqual(self.check(self.sigdir(self.leaf(self.ca, r.NIX_SIGNER), self.D)), r.NIX_SIGNER)

    def test_raw_issuer_extension_alone(self):
        self.check(self.sigdir(self.leaf(self.ca, r.NIX_SIGNER, only_raw_issuer=True), self.D))

    def test_refusals(self):
        good = self.leaf(self.ca, r.NIX_SIGNER)
        bootc = r.NIX_SIGNER.replace("nix-release.yml", "build-image.yml")
        cases = {
            "wrong identity": lambda: self.check(self.sigdir(self.leaf(self.ca, bootc), self.D)),
            "fork": lambda: self.check(self.sigdir(self.leaf(self.ca, r.NIX_SIGNER.replace("AndreNijman", "evil")), self.D)),
            "wrong issuer": lambda: self.check(self.sigdir(self.leaf(self.ca, r.NIX_SIGNER, issuer="https://x"), self.D)),
            "other root": lambda: self.check(self.sigdir(self.leaf(self.other, r.NIX_SIGNER), self.D)),
            "other digest": lambda: self.check(self.sigdir(good, "sha256:" + "f" * 64)),
            "other repo": lambda: self.check(self.sigdir(good, self.D, ref="ghcr.io/evil/rime-os")),
            "tampered payload": lambda: self.check(self.sigdir(good, self.D, tamper=True)),
        }
        for name, f in cases.items():
            with self.subTest(name), self.assertRaises(r.Refused):
                f()

    def test_cert_parsing(self):
        der = r.pem_to_der(open(f"{self.leaf(self.ca, r.NIX_SIGNER)}/leaf.pem").read())
        self.assertEqual(r.cert_san_uris(der), [r.NIX_SIGNER])
        self.assertEqual(r.cert_issuer(der), r.ISSUER)



class PathInfo(unittest.TestCase):
    """Nix 2.35 fails `path-info` for a path a binary cache lacks instead of answering null (seen in release-standin)."""

    ERR = ("don't know how to build these paths:\n  {a}\n  {b}\n"
           "error: path '{a}' is not valid\n")

    def fake(self, have):
        calls = []

        class P:
            def __init__(self, rc, out, err):
                self.returncode, self.stdout, self.stderr = rc, out, err

        def run(cmd, **kw):
            asked = [c for c in cmd if c.startswith("/nix/store/")]
            calls.append(asked)
            gone = [a for a in asked if a not in have]
            if gone:
                err = "don't know how to build these paths:\n" + "".join(f"  {g}\n" for g in gone) \
                    + f"error: path '{gone[0]}' is not valid\n"
                return P(1, b"", err.encode())
            return P(0, json.dumps({a: {"narHash": nh(a), "narSize": 1, "references": []} for a in asked}).encode(), b"")
        return run, calls

    def test_missing_paths_are_none(self):
        a, b, c = sp(1, "a"), sp(2, "b"), sp(3, "c")
        run, calls = self.fake({b})
        saved = r.run
        r.run = run
        try:
            got = r.nix_path_info([a, b, c], store="http://cache")
        finally:
            r.run = saved
        self.assertIsNone(got[a])
        self.assertIsNone(got[c])
        self.assertEqual(got[b]["nar_hash"], nh(b))
        self.assertEqual(calls, [[a, b, c], [b]])

    def test_other_failures_refuse(self):
        a = sp(1, "a")
        saved = r.run

        class P:
            returncode, stdout, stderr = 1, b"", b"error: unable to download 'http://cache/x.narinfo': HTTP error 500"
        r.run = lambda cmd, **kw: P()
        try:
            with self.assertRaises(r.Refused):
                r.nix_path_info([a], store="http://cache")
        finally:
            r.run = saved

    def test_error_parser(self):
        a, b = sp(1, "a"), sp(2, "b")
        self.assertEqual(r.missing_from_error(self.ERR.format(a=a, b=b)), {a, b})
        self.assertEqual(r.missing_from_error("error: something else"), set())



class S3Signing(unittest.TestCase):
    def test_signing_key_matches_aws_example(self):
        # AWS's documented key derivation example (Signature Version 4, "Examples of how to derive a signing key").
        k = r.sigv4_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215", "us-east-1", "iam")
        self.assertEqual(k.hex(), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d")

    def test_request_shape(self):
        import datetime as dt
        env = {"AWS_ACCESS_KEY_ID": "AKID", "AWS_SECRET_ACCESS_KEY": "secret"}
        now = dt.datetime(2026, 11, 1, 2, 3, 4, tzinfo=dt.timezone.utc)
        url, h = r.s3_request("PUT", "s3://rime-cache?endpoint=http://server:9000&region=auto&compression=zstd",
                              "nix-cache-info", b"x", "text/plain", now=now, env=env)
        self.assertEqual(url, "http://server:9000/rime-cache/nix-cache-info")
        self.assertIn("Credential=AKID/20261101/auto/s3/aws4_request", h["Authorization"])
        self.assertIn("SignedHeaders=host;x-amz-content-sha256;x-amz-date", h["Authorization"])
        url2, _ = r.s3_request("PUT", "s3://rime-cache?endpoint=acct.r2.cloudflarestorage.com&region=auto",
                               "nix-cache-info", b"x", "text/plain", now=now, env=env)
        self.assertEqual(url2, "https://acct.r2.cloudflarestorage.com/rime-cache/nix-cache-info")
        with self.assertRaises(r.Refused):
            r.s3_request("PUT", "s3://b", "k", b"", "text/plain", env={})

    def test_file_cache_info(self):
        with tempfile.TemporaryDirectory() as d:
            r.ensure_cache_info("file://" + d + "?compression=zstd")
            with open(os.path.join(d, "nix-cache-info")) as f:
                self.assertEqual(f.read(), "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n")


def upstream_manifest(**kw):
    clo, plan, sizes, x = synthetic()
    up = {"url_hint": "https://cache.nixos.org", "keys": [UPKEY], "paths": {x["glibc"]}}
    args = dict(release="2026.11.01", previous=None, issued=1796260000,
                source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY], upstream=up)
    args.update(kw)
    return r.build_manifest(clo, plan, sizes, **args), x


UPKEY = "cache.nixos.org-1:" + base64.b64encode(b"u" * 32).decode()
NAR = "1" * 52 + ".nar.zst"
NAR2 = "2" * 52 + ".nar.zst"
NAR3 = "3" * 52 + ".nar.zst"


class Narinfo(unittest.TestCase):
    TEXT = ("StorePath: /nix/store/" + "a" * 32 + "-x\nURL: nar/" + NAR + "\nCompression: zstd\n"
            "FileHash: sha256:" + "1" * 52 + "\nFileSize: 10\nNarHash: {nh}\nNarSize: 1000\n"
            "References: " + "b" * 32 + "-y\nSig: k:abc\n")

    def test_nix32_encoding_round_trips(self):
        for seed in ("a", "b", "glibc"):
            h = nh(seed)
            n32 = r.nar_hash_nix32(h)
            self.assertTrue(n32.startswith("sha256:") and len(n32) == 7 + 52, n32)
            self.assertEqual(r.sri(n32), h)
        # Nix's own value for sha256("") (nix hash convert --to nix32): 0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73
        self.assertEqual(r.nar_hash_nix32("sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="),
                         "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73")

    def test_url_rewrite_keeps_every_other_byte(self):
        t = self.TEXT.format(nh=nh("x"))
        out = r.narinfo_with_url(t, "https://github.com/o/n/releases/download/2026.11.01/" + NAR)
        self.assertEqual([l for l in out.splitlines() if not l.startswith("URL: ")],
                         [l for l in t.splitlines() if not l.startswith("URL: ")])
        self.assertIn("URL: https://github.com/o/n/releases/download/2026.11.01/" + NAR + "\n", out)
        with self.assertRaises(r.Refused):
            r.narinfo_with_url("StorePath: x\n", "u")

    def test_binding(self):
        p = "/nix/store/" + "a" * 32 + "-x"
        want = {"nar_hash": nh("x"), "nar_size": 1000, "references": ["/nix/store/" + "b" * 32 + "-y"]}
        ni = r.parse_narinfo(self.TEXT.format(nh=r.nar_hash_nix32(nh("x"))))
        self.assertEqual(ni["Sig"], ["k:abc"])
        self.assertIsNone(r.narinfo_binds(ni, p, want))
        self.assertIn("NarHash", r.narinfo_binds(ni, p, dict(want, nar_hash=nh("other"))))
        self.assertIn("NarSize", r.narinfo_binds(ni, p, dict(want, nar_size=1)))
        self.assertIn("References", r.narinfo_binds(ni, p, dict(want, references=[])))
        self.assertIn("StorePath", r.narinfo_binds(ni, p + "z", want))


class PublishGuards(unittest.TestCase):
    def test_a_test_variant_is_never_published(self):
        _, plan, _, _ = synthetic()
        r.refuse_unpublishable(plan, False)
        plan["variants"]["plain"]["publishable"] = False
        plan["variants"]["plain"]["note"] = "rime-vm has a test account"
        with self.assertRaisesRegex(r.Refused, "plain is not publishable .rime-vm has a test account."):
            r.refuse_unpublishable(plan, False)
        del plan["variants"]["plain"]["publishable"]
        with self.assertRaisesRegex(r.Refused, "not publishable"):
            r.refuse_unpublishable(plan, False)
        r.refuse_unpublishable(plan, True)  # the stand-in only

    def test_a_path_that_may_not_be_redistributed_is_never_published(self):
        # R14 (nix/redistributable.nix): the plan names the vendor app's paths; the closure must not hold one.
        clo, plan, sizes, n = synthetic()
        vendor = sp(20, "rime-chatgpt-26.1027.51404")
        plan["non_redistributable"] = [vendor, sp(21, "chatgpt-26.1027.51404-1.x86_64.rpm")]
        r.refuse_non_redistributable(clo, plan)  # not in the closure: fine
        clo["paths"][vendor] = {"nar_hash": nh(vendor), "nar_size": 5, "references": []}
        clo["catalogue_outs"] = [vendor]
        sizes[vendor] = 5
        plan["catalogue"] = [{"name": "chatgpt", "version": "26.1027.51404", "out": vendor}]
        with self.assertRaisesRegex(r.Refused, "may not be redistributed.*rime-chatgpt"):
            r.refuse_non_redistributable(clo, plan)
        with self.assertRaisesRegex(r.Refused, "may not be redistributed"):
            r.build_manifest(clo, plan, sizes, release="2026.11.01", previous=None, issued=1,
                             source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                             cache_url_hint="", cache_keys=[KEY])
        plan["non_redistributable"] = ["chatgpt"]
        with self.assertRaisesRegex(r.Refused, "not a store path"):
            r.refuse_non_redistributable(clo, plan)

    def test_publish_refuses_a_vendor_path_before_anything_is_uploaded(self):
        # R14: `publish` refuses before its fill; build_manifest's own refusal comes only after the upload.
        import types

        class Uploaded(Exception):
            pass

        class Reg:
            def __init__(self, *a, **k):
                pass

            def tag_state(self, tag):
                return None

        clo, plan, _, _ = synthetic()
        vendor = sp(20, "rime-chatgpt-26.1027.51404")
        clo["paths"][vendor] = {"nar_hash": nh(vendor), "nar_size": 5, "references": []}
        plan["non_redistributable"] = [vendor]
        uploads = []

        def upload(*a, **k):
            uploads.append(a)
            raise Uploaded()

        saved = (r.Registry, r.fill, r.gh_fill)
        r.Registry, r.fill, r.gh_fill = Reg, upload, upload
        try:
            with tempfile.TemporaryDirectory() as t:
                art = os.path.join(t, "artifact")
                os.makedirs(art)
                with open(os.path.join(art, "closure.json"), "w") as f:
                    json.dump(clo, f)
                with open(os.path.join(art, "split.json"), "w") as f:
                    json.dump({"upstream": [], "rime": sorted(clo["paths"])}, f)
                with open(os.path.join(t, "plan.json"), "w") as f:
                    json.dump(plan, f)
                a = types.SimpleNamespace(
                    rime_os=SHA_A, rime_shell=SHA_B, nixpkgs=SHA_N, repo="localhost:5000/rime", plain_http=True,
                    cosign="cosign", out=os.path.join(t, "out"), plan=os.path.join(t, "plan.json"),
                    allow_test_variants=False, artifact=art, cache_pubkey=[KEY], signer=None, fulcio_root=None,
                    issuer=None, channel="edge", bootc_release_id=None, bootc_revision=None, bootc_shell=None,
                    release="2026.11.01", next_release_id=None, today=None, backend="s3", cache="s3://rime-test",
                    secret_key_file=None, upstream=None, upstream_key=[], public=None)
                with self.assertRaisesRegex(r.Refused, "may not be redistributed.*rime-chatgpt"):
                    r.cmd_publish(a)
        finally:
            r.Registry, r.fill, r.gh_fill = saved
        self.assertEqual(uploads, [], "the publish uploaded before it refused")

    def test_a_device_built_app_is_published_as_its_derivation(self):
        clo, plan, sizes, n = synthetic()
        drv = sp(22, "rime-chatgpt-26.1027.51404.drv")
        out = sp(20, "rime-chatgpt-26.1027.51404")
        clo["paths"][drv] = {"nar_hash": nh(drv), "nar_size": 7, "references": []}
        clo["catalogue_outs"] = [drv]
        sizes[drv] = 7
        plan["non_redistributable"] = [out]
        plan["catalogue"] = [{"name": "chatgpt", "version": "26.1027.51404", "out": drv, "build": "device"}]
        args = dict(release="2026.11.01", previous=None, issued=1,
                    source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                    cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY])
        m = r.build_manifest(clo, plan, sizes, **args)
        self.assertEqual(r.validate(m), [])
        (c,) = m["catalogue"]
        self.assertEqual((m["paths"][c["out"]]["path"], c["build"]), (drv, "device"))
        # A device build that names an output, and a cache build that names a derivation, are refused.
        plan["catalogue"][0].update(out=drv, build="cache")
        with self.assertRaisesRegex(r.Refused, "build 'cache'"):
            r.build_manifest(clo, plan, sizes, **args)
        plan["catalogue"][0].update(out=n["shell"], build="device")
        with self.assertRaisesRegex(r.Refused, "built on the device, but .* is not a derivation"):
            r.build_manifest(clo, plan, sizes, **args)
        bad = copy.deepcopy(m)
        del bad["catalogue"][0]["build"]
        self.assertTrue(any("device build names a derivation" in e for e in r.validate(bad)))
        bad["catalogue"][0]["build"] = "elsewhere"
        self.assertTrue(any("neither cache nor device" in e for e in r.validate(bad)))

    def test_the_push_hook_needs_a_test_signed_run(self):
        import types
        old = os.environ.get("RIME_TEST_BEFORE_PUSH")
        os.environ["RIME_TEST_BEFORE_PUSH"] = "touch /tmp/pwned"
        try:
            self.assertIsNone(r.before_push_hook(types.SimpleNamespace(test_signing=None)))
            self.assertEqual(r.before_push_hook(types.SimpleNamespace(test_signing="/root/sign")), "touch /tmp/pwned")
        finally:
            if old is None:
                del os.environ["RIME_TEST_BEFORE_PUSH"]
            else:
                os.environ["RIME_TEST_BEFORE_PUSH"] = old

    def test_nar_urls_are_absolute_and_off_the_pages_origin(self):
        o = "cache.rimeos.com"
        self.assertIsNone(r.nar_url_problem("https://github.com/o/n/releases/download/2026.11.01/" + NAR, o))
        self.assertIn("not absolute", r.nar_url_problem("nar/" + NAR, o))
        self.assertIn("not absolute", r.nar_url_problem("file:///nix/" + NAR, o))
        self.assertIn("Pages origin", r.nar_url_problem("https://cache.rimeos.com/nar/" + NAR, o))


class UpstreamManifest(unittest.TestCase):
    def test_origin_and_keys(self):
        m, x = upstream_manifest()
        self.assertEqual(r.validate(m), [])
        self.assertEqual(m["cache"]["keys"], [KEY, UPKEY])
        self.assertEqual(m["cache"]["upstream"], {"url_hint": "https://cache.nixos.org", "keys": [UPKEY]})
        origin = {e["path"]: e["origin"] for e in m["paths"]}
        self.assertEqual(origin.pop(x["glibc"]), "upstream")
        self.assertEqual(set(origin.values()), {"rime"})

    def test_refusals(self):
        m, _ = upstream_manifest()
        bad = copy.deepcopy(m)
        bad["cache"]["keys"] = [KEY]
        self.assertTrue(any("not all in cache.keys" in e for e in r.validate(bad)))
        bad = copy.deepcopy(m)
        bad["paths"][0]["origin"] = "elsewhere"
        self.assertTrue(any("origin" in e for e in r.validate(bad)))
        plain = manifest()
        plain["paths"][0]["origin"] = "rime"
        self.assertTrue(any("origin without cache.upstream" in e for e in r.validate(plain)))


class GitHubAssets(unittest.TestCase):
    """The asset uploader against fake_github.py (the same server the stand-in runs)."""

    def setUp(self):
        import sys
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import fake_github
        self.fg = fake_github
        self.tmp = tempfile.TemporaryDirectory()
        d = self.tmp.name
        self.state = fake_github.State(os.path.join(d, "gh"), "o/n", "TOKEN")
        self.srv = [fake_github.serve(self.state, role, "127.0.0.1:0", "") for role in ("api", "download", "objects")]
        api, dl, objs = (f"http://127.0.0.1:{s.server_address[1]}" for s in self.srv)
        self.srv[1].RequestHandlerClass = fake_github.handler(self.state, "download", objs)
        self.api, self.dl = api, dl
        self.files = {}
        for name, body in ((NAR, b"one"), (NAR2, b"two!"), (NAR3, b"three")):
            p = os.path.join(d, name)
            open(p, "wb").write(body)
            self.files[name] = p

    def tearDown(self):
        for s in self.srv:
            s.shutdown()
            s.server_close()
        self.tmp.cleanup()

    def gh(self, token="TOKEN", max_assets=1000):
        return r.GitHub("o/n", token, api=self.api, uploads=self.api, download=self.dl, max_assets=max_assets)

    def test_upload_then_idempotent(self):
        urls, n = self.gh().put_assets("2026.11.01", self.files)
        self.assertEqual(n, 3)
        self.assertEqual(urls[NAR], f"{self.dl}/o/n/releases/download/2026.11.01/{NAR}")
        import urllib.request
        with urllib.request.urlopen(urls[NAR2]) as resp:  # 302 to the objects origin, followed
            self.assertEqual(resp.read(), b"two!")
            self.assertNotEqual(resp.geturl(), urls[NAR2])
        self.assertEqual(r.http_reachable(urls[NAR3]), 200)
        urls2, n2 = self.gh().put_assets("2026.11.01", self.files)
        self.assertEqual((urls2, n2), (urls, 0))

    def test_broken_asset_is_replaced(self):
        self.gh().put_assets("2026.11.01", {NAR: self.files[NAR]})
        self.state.data["releases"][0]["assets"][0]["state"] = "starter"
        urls, n = self.gh().put_assets("2026.11.01", {NAR: self.files[NAR]})
        self.assertEqual(n, 1)
        self.assertEqual([a["state"] for a in self.state.data["releases"][0]["assets"]], ["uploaded"])

    def test_parts_when_a_release_is_full(self):
        urls, n = self.gh(max_assets=2).put_assets("2026.11.01", self.files)
        self.assertEqual(n, 3)
        tags = sorted(x["tag_name"] for x in self.state.data["releases"])
        self.assertEqual(tags, ["2026.11.01", "2026.11.01-part2"])
        self.assertTrue(urls[NAR3].endswith("/2026.11.01-part2/" + NAR3), urls)
        self.assertEqual(self.gh(max_assets=2).put_assets("2026.11.01", self.files)[1], 0)

    def test_refusals(self):
        with self.assertRaises(r.Refused):
            self.gh(token="WRONG").put_assets("2026.11.01", self.files)  # 401 on create
        with self.assertRaises(r.Refused):
            self.gh().put_assets("2026.11.01", {"../evil": self.files[NAR]})
        with self.assertRaises(r.Refused):
            r.GitHub("not a repo", "t")
        self.assertEqual(r.http_reachable(f"{self.dl}/o/n/releases/download/2026.11.01/{NAR}"), 404)

    def test_a_nar_over_the_asset_limit_is_refused_before_any_upload(self):
        old = r.GH_MAX_ASSET
        r.GH_MAX_ASSET = 4  # "three" is 5 bytes: over the (scaled-down) 2 GiB limit
        try:
            with self.assertRaises(r.Refused) as cm:
                self.gh().put_assets("2026.11.01", self.files)
            self.assertIn("under 2 GiB", str(cm.exception))
        finally:
            r.GH_MAX_ASSET = old
        self.assertEqual(self.state.data["releases"], [])  # nothing was created or uploaded


def delta_case(d, payload=b"delta-bytes" * 3, full=100):
    """A previous release with an older rime-shell, and one delta for the new rime-shell in artifact `d`."""
    clo, plan, sizes, n = synthetic()
    old_shell = sp(20, "rime-shell")
    prev = manifest(release="2026.11.01")
    for e in prev["paths"]:
        if e["path"] == n["shell"]:
            e["path"], e["nar_hash"], e["nar_size"] = old_shell, nh(old_shell), 900
    prev["paths"].sort(key=lambda e: e["path"])
    digest = hashlib.sha256(payload).digest()
    name = r.nix32_raw(digest) + ".delta"
    os.makedirs(os.path.join(d, "deltas"), exist_ok=True)
    open(os.path.join(d, "deltas", name), "wb").write(payload)
    entry = {"path": n["shell"], "base": old_shell, "base_nar_hash": nh(old_shell), "base_nar_size": 900,
             "name": name, "file_hash": "sha256-" + base64.b64encode(digest).decode(), "file_size": len(payload),
             "window_log": 27, "full_size": full}
    with open(os.path.join(d, "deltas.json"), "w") as f:
        json.dump({"schema": 1, "from": "2026.11.01", "files": [entry], "skipped": []}, f)
    sizes[n["shell"]] = full
    return clo, plan, sizes, prev, entry, n


class Deltas(unittest.TestCase):
    def test_bound_deltas_are_listed_and_validate(self):
        with tempfile.TemporaryDirectory() as d:
            clo, plan, sizes, prev, entry, n = delta_case(d)
            kept = r.bind_deltas(d, clo, prev, sizes)
            self.assertEqual([e["path"] for e in kept], [n["shell"]])
            pub = [dict(kept[0], file="2026.11.02/" + kept[0]["name"])]
            m = r.build_manifest(clo, plan, sizes, release="2026.11.02", previous="2026.11.01", issued=1,
                                 source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                                 cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY],
                                 previous_manifest=prev, deltas=pub)
            self.assertEqual(r.validate(m), [])
            f = m["delta"]["files"]
            self.assertEqual(len(f), 1)
            self.assertEqual(m["paths"][f[0]["path"]]["path"], n["shell"])
            self.assertEqual(f[0]["file"], "2026.11.02/" + entry["name"])
            self.assertEqual(m["delta"]["paths"], 1)
            self.assertEqual(m["delta"]["delta_size"], entry["file_size"])
            self.assertEqual(m["delta"]["file_size"], 100)

    def test_bind_drops_what_does_not_match(self):
        cases = {
            "made against another release": lambda prev, e, d, sizes: prev.update(release="2026.10.31"),
            "base differs": lambda prev, e, d, sizes: e.update(base_nar_hash=nh("other")),
            "base not in the previous release": lambda prev, e, d, sizes: e.update(base=sp(21, "rime-shell")),
            "file differs": lambda prev, e, d, sizes: open(os.path.join(d, "deltas", e["name"]), "ab").write(b"x"),
            "sha differs": lambda prev, e, d, sizes: e.update(file_hash=nh("x")),
            "over the ratio": lambda prev, e, d, sizes: sizes.update({e["path"]: 40}),
            "wrong window": lambda prev, e, d, sizes: e.update(window_log=31),
            "not a new path": lambda prev, e, d, sizes: e.update(path=sorted(clo_paths(prev))[0]),
        }
        for name, f in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as d:
                clo, plan, sizes, prev, entry, n = delta_case(d)
                t = json.load(open(os.path.join(d, "deltas.json")))
                f(prev, t["files"][0], d, sizes)
                json.dump(t, open(os.path.join(d, "deltas.json"), "w"))
                self.assertEqual(r.bind_deltas(d, clo, prev, sizes), [], name)

    def test_every_delta_rule_refuses(self):
        with tempfile.TemporaryDirectory() as d:
            clo, plan, sizes, prev, entry, n = delta_case(d)
            pub = [dict(entry, file="2026.11.02/" + entry["name"])]
            good = r.build_manifest(clo, plan, sizes, release="2026.11.02", previous="2026.11.01", issued=1,
                                    source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                                    cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY],
                                    previous_manifest=prev, deltas=pub)
        cases = {
            "path range": lambda f, dd: f.update(path=999),
            "base is the path": lambda f, dd: f.update(base=good["paths"][f["path"]]["path"]),
            "base hash": lambda f, dd: f.update(base_nar_hash="sha256:nothex"),
            "file hash form": lambda f, dd: f.update(file_hash="sha256:" + "0" * 52),
            "size": lambda f, dd: f.update(file_size=0),
            "file escapes": lambda f, dd: f.update(file="../" + entry["name"]),
            "file kind": lambda f, dd: f.update(file="2026.11.02/" + entry["name"][:-6] + ".nar.zst"),
            "window": lambda f, dd: f.update(window_log=26),
            "duplicate": lambda f, dd: dd["files"].append(dict(f)),
            "no delta_size": lambda f, dd: dd.pop("delta_size"),
        }
        for name, mut in cases.items():
            with self.subTest(name):
                m = copy.deepcopy(good)
                mut(m["delta"]["files"][0], m["delta"])
                self.assertNotEqual(r.validate(m), [], name)

    def test_make_delta_rebuilds_or_refuses(self):
        import random
        rnd = random.Random(7)
        base = bytes(rnd.getrandbits(8) for _ in range(300_000))
        new = base[:100_000] + b"changed" * 100 + base[100_000:]
        with tempfile.TemporaryDirectory() as d:
            b, nw, out = (os.path.join(d, x) for x in ("base.nar", "new.nar", "x.delta"))
            open(b, "wb").write(base)
            open(nw, "wb").write(new)
            h = "sha256-" + base64.b64encode(hashlib.sha256(new).digest()).decode()
            size = r.make_delta(b, nw, h, 27, out)
            self.assertLess(size, len(new) // 20)
            self.assertEqual(r.delta_window_log(len(base), len(new)), 27)
            self.assertEqual(r.delta_window_log(600 << 20, 10), 30)
            with self.assertRaises(r.Refused):
                r.make_delta(b, nw, nh("not the new NAR"), 27, out)



class StageCompression(unittest.TestCase):
    def test_stage_writes_level_19(self):
        """delta.md §8: the Rime cache's NARs are zstd level 19 (not Nix's default 3), compressed in parallel."""
        clo, _, _, x = synthetic()
        calls = []
        real_run = r.run
        r.run = lambda cmd, **kw: calls.append(cmd)
        try:
            with tempfile.TemporaryDirectory() as d:
                for n, v in (("closure.json", clo), ("split.json", {"rime": [x["shell"]], "upstream": [x["glibc"]]})):
                    json.dump(v, open(os.path.join(d, n), "w"))
                a = type("A", (), {"closure": os.path.join(d, "closure.json"), "split": os.path.join(d, "split.json"),
                                   "out": os.path.join(d, "stage")})()
                with self.assertRaises(r.Refused):  # nothing was copied: the narinfo count check refuses
                    r.cmd_stage(a)
        finally:
            r.run = real_run
        copies = [c for c in calls if "copy" in c]
        self.assertEqual(len(copies), 1, calls)
        url = next(c for c in copies[0] if str(c).startswith("file://"))
        self.assertIn("compression=zstd", url)
        self.assertIn("compression-level=19", url)
        self.assertIn("parallel-compression=true", url)

def clo_paths(m):
    return [e["path"] for e in m["paths"]]


class Schema(unittest.TestCase):
    """The JSON Schema files describe what the tool writes."""

    def test_schema_accepts_tool_output(self):
        try:
            import jsonschema
        except ImportError:
            self.skipTest("no jsonschema")
        here = os.path.dirname(_path) if "RIME_NIX_RELEASE_SCHEMAS" not in os.environ else os.environ["RIME_NIX_RELEASE_SCHEMAS"]
        rel = json.load(open(os.path.join(here, "schema", "nix-release.v1.schema.json")))
        chans = json.load(open(os.path.join(here, "schema", "nix-channels.v1.schema.json")))
        prev = manifest()
        jsonschema.validate(manifest(release="2026.11.02", previous="2026.11.01", previous_manifest=prev), rel)
        jsonschema.validate(r.next_channels(None, "R", 1, set_entry={
            "channel": "edge", "digest": "sha256:" + "1" * 64, "release": "2026.11.01", "reason": "x"}), chans)
        with tempfile.TemporaryDirectory() as d:
            clo, plan, sizes, dprev, entry, n = delta_case(d)
            jsonschema.validate(r.build_manifest(
                clo, plan, sizes, release="2026.11.02", previous="2026.11.01", issued=1,
                source={"rime_os": SHA_A, "rime_shell": SHA_B, "nixpkgs": SHA_N},
                cache_url_hint="https://cache.rimeos.com", cache_keys=[KEY], previous_manifest=dprev,
                deltas=[dict(entry, file="2026.11.02/" + entry["name"])]), rel)
        bad = manifest()
        bad["paths"][0]["nar_hash"] = "md5:x"
        with self.assertRaises(jsonschema.ValidationError):
            jsonschema.validate(bad, rel)


if __name__ == "__main__":
    unittest.main()
