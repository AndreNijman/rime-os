# The whole D6 publish pipeline in VMs, with THROWAWAY keys (docs/nixos/updates-and-release.md §15.2
# `fill-and-publish`, plus a client that consumes the result).
#
#   builder  the build job: holds the toplevels, writes closure.json, splits against the upstream cache, exports
#   server   R2 stand-in (versitygw, S3 API + anonymous reads at http://server:9000/rime-cache), GHCR stand-in (the
#            NixOS docker-registry, server:5000), a fake upstream cache signed with its own key (http://server:8080),
#            and the publish job itself (rime-nix-release publish), which never had the toplevels in its store
#   client   verifies the channel document and the manifest, downloads the toplevel from the cache with exactly the
#            keys the manifest announces, binds every path to the manifest's narHash, and asks the real
#            `rime trust --gate` (verify.rs) about the signature
#
# Refusals proven here, each leaving no manifest tag and no channel change: a bad upstream signature, an artifact
# changed in transit, a NAR deleted from the bucket, an unsigned narinfo (then repaired by the next fill), and a
# manifest signed by the wrong workflow (pushed, but the self-check stops the channel document; rime trust refuses).
{ pkgs, self }:

let
  release = import ../release { inherit pkgs self; };
  fx = import ../release/fixtures.nix { inherit pkgs; };
  tool = release.rime-nix-release;
  nixSettings = { lib, ... }: {
    nix.package = pkgs.nixVersions.latest;
    nix.settings = {
      experimental-features = [ "nix-command" ];
      substituters = lib.mkForce [ ];
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "rime-release-standin";

  nodes = {
    builder = { ... }: {
      imports = [ nixSettings ];
      virtualisation.additionalPaths = [ fx.plain fx.luks ];
      virtualisation.memorySize = 2048;
      environment.systemPackages = [ tool ];
    };

    server = { ... }: {
      imports = [ nixSettings ];
      virtualisation.memorySize = 3072;
      virtualisation.diskSize = 4096;
      networking.firewall.allowedTCPPorts = [ 5000 8080 9000 ];
      services.dockerRegistry = {
        enable = true;
        listenAddress = "0.0.0.0";
        port = 5000;
      };
      systemd.services.versitygw = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "versitygw";
          ExecStart = "${pkgs.versitygw}/bin/versitygw --access rimetestaccess --secret rimetestsecretkey"
            + " --region auto --port :9000 posix /var/lib/versitygw";
        };
      };
      systemd.services.upstream-cache = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          StateDirectory = "upstream";
          ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080 --directory /var/lib/upstream";
        };
      };
      environment.systemPackages = [ tool release.cosign pkgs.awscli2 pkgs.curl pkgs.skopeo pkgs.openssl ];
      environment.etc."rime-standin/plan.json".source = fx.plan;
    };

    client = { ... }: {
      imports = [ nixSettings ];
      environment.systemPackages = [ tool pkgs.rime.rimed pkgs.curl pkgs.openssl ];
    };
  };

  testScript = ''
    import json, os

    SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main"
    BOOTC = SIGNER.replace("nix-release.yml", "build-image.yml")
    ISSUER = "https://token.actions.githubusercontent.com"
    REPO = "server:5000/andrenijman/rime-os"
    S3 = "s3://rime-cache?endpoint=http://server:9000&region=auto&compression=zstd"
    PUBLIC = "http://server:9000/rime-cache"
    UPSTREAM = "http://server:8080"
    AWS = "AWS_ACCESS_KEY_ID=rimetestaccess AWS_SECRET_ACCESS_KEY=rimetestsecretkey AWS_DEFAULT_REGION=auto"
    NIXPKGS = "1c89083e451e891152365b799595cf7790a43325"
    A, B = "a" * 40, "b" * 40
    H1, H2, H3, H4 = "1" * 40, "2" * 40, "3" * 40, "4" * 40
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

    def aws(cmd):
        return server.succeed(f"{AWS} aws --endpoint-url http://server:9000 {cmd}")

    def bucket():
        out = aws("s3api list-objects-v2 --bucket rime-cache --query 'Contents[].Key' --output json").strip()
        return sorted(json.loads(out) or [])

    def tag(t):
        rc, _ = server.execute(f"skopeo --insecure-policy inspect --raw --tls-verify=false docker://{REPO}:{t} > /tmp/raw")
        return None if rc else "sha256:" + server.succeed("sha256sum < /tmp/raw").split()[0]

    def publish(os_sha, shell, signing="/root/sign-good", artifact="/root/artifact"):
        out = f"/root/out-{os_sha[:3]}-{shell[:3]}-{os.path.basename(signing)}-{os.path.basename(artifact)}"
        rc, log = server.execute(
            f"{AWS} rime-nix-release publish --artifact {artifact} --plan /etc/rime-standin/plan.json"
            f" --cache '{S3}' --secret-key-file /root/cache.sec --upstream {UPSTREAM} --upstream-key \"$(cat /root/up.pub)\""
            f" --public {PUBLIC} --cache-pubkey \"$(cat /root/cache.pub)\" --cache-url-hint {PUBLIC}"
            f" --repo {REPO} --plain-http --fulcio-root /root/sign-good/fulcio-root.pem"
            f" --rime-os {os_sha} --rime-shell {shell} --nixpkgs {NIXPKGS} --today 2026.11.01"
            f" --allow-test-variants --test-signing {signing} --out {out} 2>&1")
        print(log)
        return rc, log, out

    def narinfo_of(path):
        h = os.path.basename(path)[:32]
        return f"{h}.narinfo"

    start_all()
    for u in ("versitygw.service", "docker-registry.service", "upstream-cache.service"):
        server.wait_for_unit(u)
    for p in (9000, 5000, 8080):
        server.wait_for_open_port(p)

    with subtest("throwaway keys: upstream, rime-test-cache-1, and a TEST Fulcio root with two identities"):
        builder.succeed("nix key generate-secret --key-name upstream-test-1 > /root/up.sec")
        builder.succeed("nix key convert-secret-to-public < /root/up.sec > /root/up.pub")
        transfer(builder, "/root/up.pub", server)
        server.succeed("nix key generate-secret --key-name rime-test-cache-1 > /root/cache.sec")
        server.succeed("nix key convert-secret-to-public < /root/cache.sec > /root/cache.pub")
        server.succeed(f"mint-test-signing /root/ca /root/sign-good {SIGNER} {ISSUER}")
        server.succeed(f"mint-test-signing /root/ca /root/sign-bootc {BOOTC} {ISSUER}")
        transfer(server, "/root/sign-good/fulcio-root.pem", client)
        client.succeed("mv /root/sign-good/fulcio-root.pem /root/fulcio-root.pem")  # the client holds the root only
        assert "rime TEST" in server.succeed("openssl x509 -noout -subject -in /root/sign-good/leaf.pem")

    with subtest("the fake upstream cache: GNU hello's closure, signed by upstream-test-1"):
        builder.succeed(f"nix copy --to file:///var/lib/upstream?compression=zstd {HELLO}")
        builder.succeed(f"nix store sign --store file:///var/lib/upstream --key-file /root/up.sec -r {HELLO}")
        transfer(builder, "/var/lib/upstream", server)
        server.succeed("curl -fsS http://server:8080/nix-cache-info")

    with subtest("the build job: closure.json, the split, the export"):
        builder.succeed(f"rime-nix-release closure --variant plain={PLAIN} --variant luks={LUKS} --out /root/closure.json")
        builder.succeed(f"rime-nix-release split --closure /root/closure.json --upstream {UPSTREAM} --out /root/split.json")
        split = json.loads(builder.succeed("cat /root/split.json"))
        assert HELLO in split["upstream"], split
        assert PLAIN in split["rime"] and LUKS in split["rime"], split
        assert not any("rime-standin" in p for p in split["upstream"]), split
        SHELL = builder.succeed(f"readlink -f {PLAIN}/sw-shell").strip()
        assert SHELL in split["rime"], split
        builder.succeed("rime-nix-release export --closure /root/closure.json --split /root/split.json --out /root/artifact")
        transfer(builder, "/root/artifact", server)
        server.fail(f"nix path-info {PLAIN}")  # the publish job starts without the toplevels

    with subtest("the bucket: anonymous reads, no anonymous writes"):
        aws("s3api create-bucket --bucket rime-cache --create-bucket-configuration LocationConstraint=auto")
        aws("s3api put-bucket-policy --bucket rime-cache --policy "
            "'{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":\"*\","
            "\"Action\":[\"s3:GetObject\"],\"Resource\":[\"arn:aws:s3:::rime-cache/*\"]}]}'")
        client.fail("curl -fsS -X PUT --data x http://server:9000/rime-cache/x")

    with subtest("refused: a bad upstream signature (nothing uploaded, nothing tagged)"):
        ni = f"/var/lib/upstream/{narinfo_of(HELLO)}"
        server.succeed(f"cp {ni} /root/hello.narinfo.good")
        server.succeed(f"sed -i -E 's/^(Sig: upstream-test-1:)(.)/\\1A\\2/' {ni}")
        server.succeed(f"! cmp -s {ni} /root/hello.narinfo.good")
        rc, log, _ = publish(A, H1)
        assert rc != 0 and "REFUSED" in log, log
        assert tag(f"nix-{A}-{H1}") is None
        assert not [k for k in bucket() if k.endswith(".narinfo")], bucket()
        server.succeed(f"cp /root/hello.narinfo.good {ni}")

    with subtest("refused: an artifact changed in transit (closure.json no longer matches what was imported)"):
        server.succeed("cp -r /root/artifact /root/artifact-tampered")
        clo = json.loads(server.succeed("cat /root/artifact-tampered/closure.json"))
        clo["paths"][PLAIN]["nar_hash"] = clo["paths"][HELLO]["nar_hash"]
        put(server, "/root/artifact-tampered/closure.json", json.dumps(clo))
        rc, log, _ = publish(A, H1, artifact="/root/artifact-tampered")
        assert rc != 0 and "is not the one closure.json names" in log, log
        assert tag(f"nix-{A}-{H1}") is None
        assert not [k for k in bucket() if k.endswith(".narinfo")], bucket()

    with subtest("published: release 2026.11.01 on edge"):
        rc, log, out = publish(A, H1)
        assert rc == 0, log
        st = json.loads(server.succeed(f"cat {out}/published.json"))
        assert st["state"] == "published" and st["release"] == "2026.11.01", st
        hello_ni = server.succeed(f"curl -fsS {PUBLIC}/{narinfo_of(HELLO)}")
        assert "Sig: upstream-test-1:" in hello_ni and "Sig: rime-test-cache-1:" in hello_ni, hello_ni
        info = server.succeed(f"curl -fsS {PUBLIC}/nix-cache-info")
        assert "WantMassQuery: 1" in info and "Priority: 30" in info, info
        man = json.loads(server.succeed(f"cat {out}/manifest.json"))
        assert sorted(man["variants"]) == ["luks", "plain"]
        assert man["previous"] is None and man["delta"] is None
        server.succeed(f"rime-nix-release validate --canonical {out}/manifest.json")
        server.succeed(f"rime-nix-release verify --repo {REPO} --plain-http --fulcio-root /root/sign-good/fulcio-root.pem --ref nix-{A}-{H1}")
        first_bucket = bucket()
        chan1 = tag("nix-channels")
        assert chan1 is not None

    with subtest("idempotent: the same pair publishes nothing; a new shell pair uploads no NAR"):
        rc, log, out = publish(A, H1)
        assert rc == 0 and json.loads(server.succeed(f"cat {out}/published.json"))["state"] == "exists", log
        assert bucket() == first_bucket and tag("nix-channels") == chan1
        rc, log, out = publish(A, H2)
        assert rc == 0, log
        fill = json.loads(server.succeed(f"cat {out}/fill.json"))
        assert fill["uploaded"] == 0, fill
        assert bucket() == first_bucket, "a re-publish of the same closure wrote objects"
        man = json.loads(server.succeed(f"cat {out}/manifest.json"))
        assert man["release"] == "2026.11.01.2" and man["previous"] == "2026.11.01", man["release"]
        assert man["delta"] == {"from": "2026.11.01", "variant": "plain", "paths": 0, "file_size": 0}, man["delta"]
        chan = json.loads(server.succeed(f"cat {out}/channels.json"))
        assert chan["serial"] == 2 and chan["channels"][0]["release"] == "2026.11.01.2", chan

    with subtest("refused: a NAR deleted from the bucket is caught from the public side"):
        chan2 = tag("nix-channels")
        shell_ni = server.succeed(f"curl -fsS {PUBLIC}/{narinfo_of(SHELL)}")
        nar = [l.split(": ", 1)[1] for l in shell_ni.splitlines() if l.startswith("URL: ")][0]
        aws(f"s3 rm s3://rime-cache/{nar}")
        rc, log, _ = publish(A, H3)
        assert rc != 0 and "answered 404" in log, log
        assert tag(f"nix-{A}-{H3}") is None and tag("nix-channels") == chan2
        # repaired by removing the narinfo: the next fill finds the path missing and uploads it again
        aws(f"s3 rm s3://rime-cache/{narinfo_of(SHELL)}")
        rc, log, out = publish(A, H3)
        assert rc == 0, log
        assert json.loads(server.succeed(f"cat {out}/fill.json"))["uploaded"] >= 1
        good_tag = f"nix-{A}-{H3}"
        assert json.loads(server.succeed(f"cat {out}/published.json"))["release"] == "2026.11.01.3"

    with subtest("an unsigned narinfo fails the public check; the next fill re-signs it and the next read sees it"):
        key = narinfo_of(HELLO)
        aws(f"s3 cp s3://rime-cache/{key} /root/stripped.narinfo")
        server.succeed("sed -i '/^Sig: /d' /root/stripped.narinfo")
        aws(f"s3 cp /root/stripped.narinfo s3://rime-cache/{key}")
        server.fail(f"rime-nix-release check-public --closure /root/artifact/closure.json --public {PUBLIC} --key \"$(cat /root/cache.pub)\"")
        server.succeed(f"{AWS} rime-nix-release fill --artifact /root/artifact --cache '{S3}' --secret-key-file /root/cache.sec"
                       f" --upstream {UPSTREAM} --upstream-key \"$(cat /root/up.pub)\"")
        server.succeed(f"rime-nix-release check-public --closure /root/artifact/closure.json --public {PUBLIC} --key \"$(cat /root/cache.pub)\"")
        assert "Sig: rime-test-cache-1:" in server.succeed(f"curl -fsS {PUBLIC}/{key}")

    with subtest("refused: a manifest signed by build-image.yml stops at the self-check; the channel is unchanged"):
        chan3 = tag("nix-channels")
        rc, log, _ = publish(B, H4, signing="/root/sign-bootc")
        assert rc != 0 and f"signed by {BOOTC}" in log, log
        assert tag("nix-channels") == chan3
        assert tag(f"nix-{B}-{H4}") is not None  # pushed and signed, never offered

    with subtest("the client consumes edge: verified, downloaded with the manifest's keys, bound to its narHashes"):
        client.fail(f"nix path-info {PLAIN}")
        res = json.loads(client.succeed(
            f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root /root/fulcio-root.pem --cache {PUBLIC}"))
        assert res["release"] == "2026.11.01.3" and res["toplevel"] == PLAIN, res
        client.succeed(f"nix path-info {PLAIN}")
        client.succeed(f"grep -q 'TEST shell a' {PLAIN}/sw-shell/share/rime-shell/shell.qml")
        client.succeed(f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root /root/fulcio-root.pem --cache {PUBLIC} --variant luks")
        # a client that trusts only the TEST Fulcio root of ANOTHER CA refuses
        client.succeed("openssl ecparam -name prime256v1 -genkey -noout -out /root/o.key && openssl req -new -x509 -key /root/o.key -subj /CN=other -days 1 -out /root/other-root.pem")
        client.fail(f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root /root/other-root.pem --cache {PUBLIC}")

    with subtest("the real client verifier (rime trust --gate, verify.rs) agrees with the self-check"):
        ok = client.execute(f"crosscheck-rime-trust --rime $(command -v rime) --repo {REPO} --tag {good_tag}"
                            f" --fulcio-root /root/fulcio-root.pem --signer {SIGNER} --issuer {ISSUER} --plain-http 2>&1")
        print(ok[1])
        assert ok[0] == 0, "rime trust --gate refused the published manifest"
        bad = client.execute(f"crosscheck-rime-trust --rime $(command -v rime) --repo {REPO} --tag nix-{B}-{H4}"
                             f" --fulcio-root /root/fulcio-root.pem --signer {SIGNER} --issuer {ISSUER} --plain-http 2>&1")
        print(bad[1])
        assert bad[0] != 0, "rime trust --gate accepted a manifest signed by build-image.yml"
        assert f"signed by {BOOTC}, and this machine expects {SIGNER}" in bad[1], "refused for another reason"
        other = client.execute(f"crosscheck-rime-trust --rime $(command -v rime) --repo {REPO} --tag {good_tag}"
                               f" --fulcio-root /root/fulcio-root.pem --signer {BOOTC} --issuer {ISSUER} --plain-http 2>&1")
        assert other[0] != 0, "rime trust --gate accepted nix-release.yml where build-image.yml was expected"

    with subtest("published.json: what a consumer needs"):
        doc = {
            "TEST": "throwaway keys generated inside this test; nothing here is a Rime key",
            "cache": {"public_url": PUBLIC, "s3_url": S3, "bucket": "rime-cache",
                      "narinfo_public_key": server.succeed("cat /root/cache.pub").strip()},
            "registry": {"repository": REPO, "plain_http": True, "manifest_tag": good_tag,
                         "manifest_digest": tag(good_tag), "channels_tag": "nix-channels",
                         "channels_digest": tag("nix-channels"), "release": "2026.11.01.3"},
            "trust": {"fulcio_root_pem": server.succeed("cat /root/sign-good/fulcio-root.pem"),
                      "signer": SIGNER, "issuer": ISSUER},
            "consume": f"rime-nix-release consume --repo {REPO} --plain-http --fulcio-root fulcio-root.pem --cache {PUBLIC}",
        }
        (server.out_dir / "published.json").write_text(json.dumps(doc, indent=1) + "\n")
        print(json.dumps(doc, indent=1))
  '';
}
