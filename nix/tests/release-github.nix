# The production publish shape in VMs, with THROWAWAY keys (docs/nixos/updates-and-release.md §6, §18): the binary
# cache is GitHub Pages of rime-cache (narinfos) plus that repository's release assets (NARs), and every narinfo's
# URL: is ABSOLUTE, on another origin. Upstream paths are not published: clients take them from the upstream cache.
#
#   builder  the build job: closure.json, the split against the upstream cache, `stage` (Rime's paths only)
#   server   pages.test:8090   the Pages origin: python http.server over the checkout a post-receive hook of the
#                              bare repo /var/lib/rime-cache.git deploys (main's tree, as Pages does)
#            api.test:8081     fake-github's REST API (releases, assets; writes need the TEST token)
#            github.test:8082  release downloads, 302 to objects.test:8083 (as github.com redirects to
#                              objects.githubusercontent.com)
#            server:8080       a fake upstream cache (cache.nixos.org) signed with its own TEST key
#            server:5000       the GHCR stand-in; and the publish job itself
#   client   consumes edge from pages.test + the upstream cache, with exactly the manifest's keys
#
# Refusals proven here, each leaving no manifest tag: an artifact changed in transit (nothing uploaded, nothing
# committed), a NAR asset deleted from its release. Also proven: the signature survives the URL rewrite, a
# re-publish uploads and commits nothing, a commit pushed to main during a publish is kept (no force-push), no
# upstream narinfo is ever committed, and a client without the upstream cache cannot install the release.
{ pkgs, self }:

let
  release = import ../release { inherit pkgs self; };
  fx = import ../release/fixtures.nix { inherit pkgs; };
  tool = release.rime-nix-release;
  common = { lib, nodes, ... }: {
    nix.package = pkgs.nixVersions.latest;
    nix.settings = {
      experimental-features = [ "nix-command" ];
      substituters = lib.mkForce [ ];
    };
    networking.extraHosts = "${nodes.server.networking.primaryIPAddress} pages.test api.test github.test objects.test";
  };
in
pkgs.testers.runNixOSTest {
  name = "rime-release-github";

  nodes = {
    builder = { ... }: {
      imports = [ common ];
      virtualisation.additionalPaths = [ fx.plain fx.luks ];
      virtualisation.memorySize = 2048;
      environment.systemPackages = [ tool ];
    };

    server = { ... }: {
      imports = [ common ];
      virtualisation.memorySize = 3072;
      virtualisation.diskSize = 4096;
      networking.firewall.allowedTCPPorts = [ 5000 8080 8081 8082 8083 8090 ];
      services.dockerRegistry = {
        enable = true;
        listenAddress = "0.0.0.0";
        port = 5000;
      };
      systemd.services.upstream-cache = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "upstream";
          ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080 --directory /var/lib/upstream";
        };
      };
      systemd.services.pages = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "pages";
          ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8090 --directory /var/lib/pages";
        };
      };
      systemd.services.fake-github = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "fake-github";
          ExecStart = "${tool}/bin/fake-github --dir /var/lib/fake-github --repo AndreNijman/rime-cache"
            + " --token rime-test-cache-token --api 0.0.0.0:8081 --download 0.0.0.0:8082 --objects 0.0.0.0:8083"
            + " --objects-url http://objects.test:8083";
        };
      };
      environment.systemPackages = [ tool release.cosign pkgs.curl pkgs.skopeo pkgs.openssl pkgs.git ];
      environment.etc."rime-standin/plan.json".source = fx.plan;
    };

    client = { ... }: {
      imports = [ common ];
      environment.systemPackages = [ tool pkgs.curl ];
    };
  };

  testScript = ''
    import json, os
    ${builtins.readFile ./lib/read-json.py}

    SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main"
    ISSUER = "https://token.actions.githubusercontent.com"
    REPO = "server:5000/andrenijman/rime-os"
    PAGES = "http://pages.test:8090"
    API = "http://api.test:8081"
    DOWNLOAD = "http://github.test:8082"
    UPSTREAM = "http://server:8080"
    GIT = "file:///var/lib/rime-cache.git"
    TOKEN = "rime-test-cache-token"
    NIXPKGS = "1c89083e451e891152365b799595cf7790a43325"
    A = "a" * 40
    H1, H2, H3 = "1" * 40, "2" * 40, "3" * 40
    HELLO = "${pkgs.hello}"
    PLAIN, LUKS = "${fx.plain}", "${fx.luks}"

    def transfer(src, path, dst):
        d, b = os.path.dirname(path), os.path.basename(path)
        src.succeed(f"tar -C {d} -cf /tmp/xfer-{b}.tar {b}")
        src.copy_from_vm(f"/tmp/xfer-{b}.tar", "xfer")
        dst.copy_from_host(str(src.out_dir / "xfer" / f"xfer-{b}.tar"), f"/tmp/xfer-{b}.tar")
        dst.succeed(f"mkdir -p {d} && tar -C {d} -xf /tmp/xfer-{b}.tar && rm /tmp/xfer-{b}.tar")
        src.succeed(f"rm /tmp/xfer-{b}.tar")
        os.remove(src.out_dir / "xfer" / f"xfer-{b}.tar")

    def put(machine, path, text):
        local = machine.out_dir / ("put-" + os.path.basename(path))
        local.write_text(text)
        machine.copy_from_host(str(local), path)
        os.remove(local)

    def tag(t):
        rc, _ = server.execute(f"skopeo --insecure-policy inspect --raw --tls-verify=false docker://{REPO}:{t} > /tmp/raw")
        return None if rc else "sha256:" + server.succeed("sha256sum < /tmp/raw").split()[0]

    def head():
        return server.succeed("git -C /var/lib/rime-cache.git rev-parse main").strip()

    def gh_release(t):
        rc, out = server.execute(f"curl -fsS {API}/repos/AndreNijman/rime-cache/releases/tags/{t}")
        return json.loads(out) if rc == 0 else None

    def gh_assets(t):
        rel = gh_release(t)
        return json.loads(server.succeed(f"curl -fsS '{API}/repos/AndreNijman/rime-cache/releases/{rel['id']}/assets?per_page=100'"))

    def publish(os_sha, shell, artifact="/root/artifact", env=""):
        out = f"/root/out-{os_sha[:3]}-{shell[:3]}-{os.path.basename(artifact)}"
        rc, log = server.execute(
            f"{env} RIME_CACHE_TOKEN={TOKEN} rime-nix-release publish --backend github --artifact {artifact}"
            f" --plan /etc/rime-standin/plan.json --secret-key-file /root/cache.sec"
            f" --upstream {UPSTREAM} --upstream-key \"$(cat /root/up.pub)\""
            f" --gh-repo AndreNijman/rime-cache --gh-remote {GIT} --gh-api {API} --gh-uploads {API} --gh-download {DOWNLOAD}"
            f" --public {PAGES} --cache-pubkey \"$(cat /root/cache.pub)\" --cache-url-hint {PAGES}"
            f" --repo {REPO} --plain-http --fulcio-root /root/sign/fulcio-root.pem"
            f" --rime-os {os_sha} --rime-shell {shell} --nixpkgs {NIXPKGS} --today 2026.11.01"
            f" --allow-test-variants --test-signing /root/sign --out {out} 2>&1")
        print(log)
        return rc, log, out

    def narinfo_of(path):
        return os.path.basename(path)[:32] + ".narinfo"

    start_all()
    for u in ("docker-registry.service", "upstream-cache.service", "pages.service", "fake-github.service"):
        server.wait_for_unit(u)
    for p in (5000, 8080, 8081, 8082, 8083, 8090):
        server.wait_for_open_port(p)

    with subtest("throwaway keys: upstream-test-1, rime-test-cache-1, a TEST Fulcio root"):
        builder.succeed("nix key generate-secret --key-name upstream-test-1 > /root/up.sec")
        builder.succeed("nix key convert-secret-to-public < /root/up.sec > /root/up.pub")
        transfer(builder, "/root/up.pub", server)
        transfer(builder, "/root/up.pub", client)
        server.succeed("nix key generate-secret --key-name rime-test-cache-1 > /root/cache.sec")
        server.succeed("nix key convert-secret-to-public < /root/cache.sec > /root/cache.pub")
        server.succeed(f"mint-test-signing /root/ca /root/sign {SIGNER} {ISSUER}")
        transfer(server, "/root/sign/fulcio-root.pem", client)
        transfer(server, "/root/cache.pub", client)

    with subtest("the fake upstream cache, and rime-cache as bootstrapped (main: nix-cache-info, CNAME, .nojekyll)"):
        builder.succeed(f"nix copy --to file:///var/lib/upstream?compression=zstd {HELLO}")
        builder.succeed(f"nix store sign --store file:///var/lib/upstream --key-file /root/up.sec -r {HELLO}")
        transfer(builder, "/var/lib/upstream", server)
        server.succeed(
            "git init --quiet --bare -b main /var/lib/rime-cache.git",
            "printf '#!/bin/sh\\nexec git --work-tree=/var/lib/pages checkout -f main\\n' > /var/lib/rime-cache.git/hooks/post-receive",
            "chmod +x /var/lib/rime-cache.git/hooks/post-receive",
            "git init --quiet -b main /root/seed",
            "printf 'StoreDir: /nix/store\\nWantMassQuery: 1\\nPriority: 50\\n' > /root/seed/nix-cache-info",
            "echo cache.rimeos.com > /root/seed/CNAME && touch /root/seed/.nojekyll && echo TEST > /root/seed/README.md",
            "git -C /root/seed add -A && git -C /root/seed -c user.name=T -c user.email=t@invalid commit --quiet -m seed",
            "git -C /root/seed push --quiet /var/lib/rime-cache.git main",
        )
        client.succeed(f"curl -fsS {PAGES}/nix-cache-info | grep 'Priority: 50'")

    with subtest("the build job: closure.json, the split, the stage (Rime-built paths only)"):
        builder.succeed(f"rime-nix-release closure --variant plain={PLAIN} --variant luks={LUKS} --out /root/closure.json")
        builder.succeed(f"rime-nix-release split --closure /root/closure.json --upstream {UPSTREAM} --out /root/split.json")
        split = read_json(builder, "/root/split.json")
        assert HELLO in split["upstream"] and PLAIN in split["rime"], split
        SHELL = builder.succeed(f"readlink -f {PLAIN}/sw-shell").strip()
        builder.succeed("rime-nix-release stage --closure /root/closure.json --split /root/split.json --out /root/artifact")
        staged = sorted(builder.succeed("cd /root/artifact/cache && ls *.narinfo").split())
        assert staged == sorted(narinfo_of(p) for p in split["rime"]), staged
        assert narinfo_of(HELLO) not in staged
        builder.fail("grep -l '^Sig: ' /root/artifact/cache/*.narinfo")  # the build job signs nothing
        transfer(builder, "/root/artifact", server)
        server.fail(f"nix path-info {PLAIN}")  # the publish job never has the toplevels in its store
        seed = head()

    with subtest("refused: an artifact changed in transit (nothing uploaded, nothing committed, nothing tagged)"):
        server.succeed("cp -r /root/artifact /root/artifact-tampered")
        clo = read_json(server, "/root/artifact-tampered/closure.json")
        clo["paths"][PLAIN]["nar_hash"] = clo["paths"][HELLO]["nar_hash"]
        put(server, "/root/artifact-tampered/closure.json", json.dumps(clo))
        rc, txt, _ = publish(A, H1, artifact="/root/artifact-tampered")
        assert rc != 0 and "NarHash" in txt and "REFUSED" in txt, txt
        assert gh_release("2026.11.01") is None and head() == seed and tag(f"nix-{A}-{H1}") is None

    with subtest("published: NARs as release assets, narinfos on main with absolute URLs on another origin"):
        rc, txt, out = publish(A, H1)
        assert rc == 0, txt
        st = read_json(server, f"{out}/published.json")
        assert st["state"] == "published" and st["release"] == "2026.11.01", st
        fill = read_json(server, f"{out}/fill.json")
        assert fill["uploaded"] == len(split["rime"]) and fill["assets_uploaded"] == len(split["rime"]), fill
        assert len(gh_assets("2026.11.01")) == len(split["rime"])
        added = server.succeed(f"git -C /var/lib/rime-cache.git diff --name-status {seed} main").split("\n")
        assert sorted(l for l in added if l) == sorted(f"A\t{narinfo_of(p)}" for p in split["rime"]), added
        files = server.succeed("git -C /var/lib/rime-cache.git ls-tree --name-only main").split()
        assert not [p for p in split["upstream"] if narinfo_of(p) in files], "an upstream narinfo was committed"
        ni = client.succeed(f"curl -fsS {PAGES}/{narinfo_of(PLAIN)}")
        url = [l[5:] for l in ni.splitlines() if l.startswith("URL: ")][0]
        assert url.startswith(f"{DOWNLOAD}/AndreNijman/rime-cache/releases/download/2026.11.01/"), url
        # the rewrite changed URL and nothing else, and the Rime signature still verifies (fingerprint excludes URL)
        staged_ni = server.succeed(f"cat /root/artifact/cache/{narinfo_of(PLAIN)}")
        assert [l for l in ni.splitlines() if not l.startswith("URL: ")] == \
               [l for l in staged_ni.splitlines() if not l.startswith("URL: ")], (ni, staged_ni)
        client.succeed(f"nix store verify --store {PAGES} --no-contents --sigs-needed 1"
                       f" --trusted-public-keys \"$(cat /root/cache.pub)\" {PLAIN}")
        client.fail(f"nix store verify --store {PAGES} --no-contents --sigs-needed 1"
                    f" --trusted-public-keys \"$(cat /root/up.pub)\" {PLAIN}")
        # the asset URL redirects to the objects origin, and the bytes there are the staged NAR
        nar = server.succeed(f"grep '^URL: ' /root/artifact/cache/{narinfo_of(PLAIN)} | cut -c6-").strip()
        client.succeed(f"curl -fsSL -o /tmp/nar '{url}'")
        assert client.succeed("sha256sum < /tmp/nar").split()[0] == server.succeed(f"sha256sum < /root/artifact/cache/{nar}").split()[0]
        assert "objects.test:8083" in client.succeed(f"curl -sS -o /dev/null -w '%{{redirect_url}}' '{url}'")
        man = read_json(server, f"{out}/manifest.json")
        up_key = server.succeed("cat /root/up.pub").strip()
        assert man["cache"]["upstream"]["keys"] == [up_key] and up_key in man["cache"]["keys"], man["cache"]
        origins = {e["path"]: e["origin"] for e in man["paths"]}
        assert origins[HELLO] == "upstream" and origins[PLAIN] == "rime", origins
        server.succeed(f"rime-nix-release validate --canonical {out}/manifest.json")
        first = head()
        chan1 = tag("nix-channels")

    with subtest("idempotent: the same pair publishes nothing; a new shell pair uploads and commits nothing"):
        rc, txt, out = publish(A, H1)
        assert rc == 0 and read_json(server, f"{out}/published.json")["state"] == "exists", txt
        rc, txt, out = publish(A, H2)
        assert rc == 0, txt
        fill = read_json(server, f"{out}/fill.json")
        assert fill["uploaded"] == 0 and fill["assets_uploaded"] == 0 and fill["commit"] is None, fill
        assert head() == first, "a re-publish of the same closure committed"
        assert gh_release("2026.11.01.2") is None, "a re-publish of the same closure created a release"
        assert read_json(server, f"{out}/manifest.json")["release"] == "2026.11.01.2"
        assert tag("nix-channels") != chan1

    with subtest("refused: a NAR asset deleted from its release is caught from the public side"):
        chan2 = tag("nix-channels")
        shell_url = [l[5:] for l in client.succeed(f"curl -fsS {PAGES}/{narinfo_of(SHELL)}").splitlines() if l.startswith("URL: ")][0]
        name = shell_url.rsplit("/", 1)[1]
        asset = [x for x in gh_assets("2026.11.01") if x["name"] == name][0]
        server.fail(f"curl -fsS -X DELETE {API}/repos/AndreNijman/rime-cache/releases/assets/{asset['id']}")  # no token
        server.succeed(f"curl -fsS -X DELETE -H 'Authorization: Bearer {TOKEN}' {API}/repos/AndreNijman/rime-cache/releases/assets/{asset['id']}")
        rc, txt, _ = publish(A, H3)
        assert rc != 0 and "answered 404" in txt, txt
        assert tag(f"nix-{A}-{H3}") is None and tag("nix-channels") == chan2

    with subtest("repaired, while someone else pushes to main: the other commit is kept, nothing is forced"):
        # the repair a maintainer does: drop the narinfo whose NAR is gone, so the next publish uploads it again
        server.succeed(
            "rm -rf /root/admin && git clone --quiet /var/lib/rime-cache.git /root/admin",
            f"git -C /root/admin rm --quiet {narinfo_of(SHELL)}",
            "git -C /root/admin -c user.name=T -c user.email=t@invalid commit --quiet -m 'drop a narinfo whose NAR is gone'",
            "git -C /root/admin push --quiet origin main",
        )
        race = ("cd /root/admin && echo moved >> README.md && git -c user.name=T -c user.email=t@invalid commit --quiet -am concurrent"
                " && git push --quiet origin main && git rev-parse HEAD > /root/concurrent")
        rc, txt, out = publish(A, H3, env=f"RIME_TEST_BEFORE_PUSH='{race}'")
        assert rc == 0, txt
        assert "push refused" in txt, "the concurrent commit did not make the first push fail"
        concurrent = server.succeed("cat /root/concurrent").strip()
        server.succeed(f"git -C /var/lib/rime-cache.git merge-base --is-ancestor {concurrent} main")
        fill = read_json(server, f"{out}/fill.json")
        assert fill["uploaded"] == 1 and fill["assets_uploaded"] == 1, fill
        assert [x["name"] for x in gh_assets("2026.11.01.3")] == [name]
        good_tag = f"nix-{A}-{H3}"
        assert read_json(server, f"{out}/published.json")["release"] == "2026.11.01.3"

    with subtest("the client consumes edge from Pages + the upstream cache, with exactly the manifest's keys"):
        client.fail(f"nix path-info {PLAIN}")
        # without the upstream cache the closure cannot be completed: Rime publishes only its own paths
        rc, txt = client.execute(f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root /root/sign/fulcio-root.pem"
                                 f" --cache {PAGES} 2>&1")
        assert rc != 0 and os.path.basename(HELLO) in txt, txt
        res = json.loads(client.succeed(
            f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root /root/sign/fulcio-root.pem"
            f" --cache {PAGES} --cache {UPSTREAM}"))
        assert res["release"] == "2026.11.01.3" and res["toplevel"] == PLAIN, res
        client.succeed(f"grep -q 'TEST shell a' {PLAIN}/sw-shell/share/rime-shell/shell.qml")
        client.succeed(f"{HELLO}/bin/hello")

    with subtest("published.json: what a consumer needs"):
        doc = {
            "TEST": "throwaway keys generated inside this test; nothing here is a Rime key",
            "cache": {"backend": "github", "narinfo_url": PAGES, "nar_origin": DOWNLOAD, "upstream_url": UPSTREAM,
                      "narinfo_public_key": server.succeed("cat /root/cache.pub").strip(),
                      "upstream_public_key": server.succeed("cat /root/up.pub").strip()},
            "registry": {"repository": REPO, "plain_http": True, "manifest_tag": good_tag,
                         "manifest_digest": tag(good_tag), "channels_tag": "nix-channels",
                         "channels_digest": tag("nix-channels"), "release": "2026.11.01.3"},
            "trust": {"fulcio_root_pem": server.succeed("cat /root/sign/fulcio-root.pem"),
                      "signer": SIGNER, "issuer": ISSUER},
            "consume": f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root fulcio-root.pem --cache {PAGES} --cache {UPSTREAM}",
        }
        (server.out_dir / "published.json").write_text(json.dumps(doc, indent=1) + "\n")
        print(json.dumps(doc, indent=1))
  '';
}
