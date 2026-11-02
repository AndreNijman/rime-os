# `rime install claude-code` by its catalogue route (user-npm, D5; docs/nixos/p2-gates.md P3), offline: rime runs the
# catalogue's npm as the user, into ~/.local, exactly as on a machine, against a local npm registry that serves
# Anthropic's published tarballs of ONE pinned version (fixed-output fetches by the registry's own sha512 integrity).
# npm checks each tarball against the integrity the registry names, runs the package's postinstall (install.cjs puts
# the native binary from @anthropic-ai/claude-code-linux-x64 over bin/claude.exe) and links ~/.local/bin/claude; the
# native binary is the unpatched release build, whose interpreter only nix-ld serves on NixOS. checks.catalogue
# covers that binary copied by hand; this check covers the route a user takes.
#
# The registry is the user's own ~/.npmrc `registry=` (npm reads it under runuser: rime sets HOME to the user's). It
# answers the two packuments and tarballs of the pin and 404s the rest: npm skips the other platforms' optional
# packages.
#
# Mutant (passthru.mutants.bad-integrity, built by hand, must FAIL): the registry names the wrong sha512 for the main
# tarball; npm refuses it (EINTEGRITY) and `rime install claude-code` fails.
{ pkgs, self }:

let
  version = "2.1.312";
  main = pkgs.fetchurl {
    url = "https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-${version}.tgz";
    hash = "sha512-/gx8byG8VOm4YoNuT5tal/Ov5Z5oydJzZkmHCJcBFLbNGGpyYDbKA/CNdB+iIKwcLQC8pVwBoj6Dt7H3L2J5bw==";
  };
  native = pkgs.fetchurl {
    url = "https://registry.npmjs.org/@anthropic-ai/claude-code-linux-x64/-/claude-code-linux-x64-${version}.tgz";
    hash = "sha512-oj55HL9uasLWzym5eZNgC8xyk0N0H4hw8WdN0DeDuAlquhs2d4hhkQVZcRKvJarBB37IU9bbEGXZwtuhsex6EQ==";
  };

  port = 4873;

  # The registry's documents, built from the tarballs' own package.json: name -> packument, plus the tarballs.
  mkRegistry = { mainIntegrity ? main.outputHash }:
    pkgs.runCommand "claude-code-npm-registry-${version}" { nativeBuildInputs = [ pkgs.python3 ]; } ''
      mkdir -p $out/tarballs
      cp ${main} $out/tarballs/claude-code-${version}.tgz
      cp ${native} $out/tarballs/claude-code-linux-x64-${version}.tgz
      python3 - <<'EOF'
      import json, tarfile
      out = "${placeholder "out"}"
      for name, tgz, integrity in [
          ("@anthropic-ai/claude-code", "claude-code-${version}.tgz", "${mainIntegrity}"),
          ("@anthropic-ai/claude-code-linux-x64", "claude-code-linux-x64-${version}.tgz", "${native.outputHash}"),
      ]:
          with tarfile.open(f"{out}/tarballs/{tgz}") as t:
              pj = json.load(t.extractfile("package/package.json"))
          assert pj["name"] == name and pj["version"] == "${version}", pj
          pj["dist"] = {"tarball": f"http://127.0.0.1:${toString port}/tarballs/{tgz}", "integrity": integrity}
          doc = {"name": name, "dist-tags": {"latest": "${version}"}, "versions": {"${version}": pj}}
          with open(f"{out}/{name.replace('/', '__')}.json", "w") as f:
              json.dump(doc, f)
      EOF
    '';

  # Serves /<name> (npm sends scoped names as @scope%2fname) and /tarballs/<file>; 404 for anything else.
  server = pkgs.writeText "npm-registry.py" ''
    import http.server, os, sys, urllib.parse
    root = sys.argv[1]
    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            path = urllib.parse.unquote(self.path.split("?")[0]).lstrip("/")
            f = os.path.join(root, path) if path.startswith("tarballs/") else os.path.join(root, path.replace("/", "__") + ".json")
            print("GET", self.path, "->", f if os.path.isfile(f) else 404, flush=True)
            if not os.path.isfile(f):
                self.send_error(404)
                return
            with open(f, "rb") as fh:
                body = fh.read()
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream" if f.endswith(".tgz") else "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
    http.server.ThreadingHTTPServer(("127.0.0.1", ${toString port}), H).serve_forever()
  '';

  mk = { name, registry }: pkgs.testers.runNixOSTest {
    inherit name;
    node.pkgsReadOnly = false;

    nodes.machine = { config, ... }: {
      imports = [ self.nixosModules.default ];
      rime.catalogue.enable = true;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      environment.systemPackages = [ config.rime.services.rimed.package pkgs.file ];
      systemd.services.npm-registry = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${server} ${registry}";
      };
      virtualisation = {
        memorySize = 3072;
        cores = 2;
        diskSize = 4096;
      };
    };

    testScript = { nodes, ... }: ''
      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("npm-registry.service")
      machine.wait_for_open_port(${toString port})

      with subtest("offline, the catalogue's npm is new enough for the pinned package"):
          machine.fail("ping -c1 -W2 1.1.1.1")
          node = machine.succeed("${nodes.machine.rime.catalogue.npm.package}/bin/node --version").strip()
          assert int(node.lstrip("v").split(".")[0]) >= 22, node

      with subtest("rime install claude-code installs the pinned npm package into alice's ~/.local, from the registry"):
          machine.succeed("su - alice -c 'echo registry=http://127.0.0.1:${toString port}/ > ~/.npmrc; echo update-notifier=false >> ~/.npmrc'")
          out = machine.succeed("SUDO_USER=alice rime install claude-code 2>&1")
          print(out)
          print(machine.succeed("journalctl -u npm-registry --no-pager | grep GET"))
          machine.succeed("test -L /home/alice/.local/bin/claude")
          machine.succeed("stat -c %U /home/alice/.local/lib/node_modules/@anthropic-ai/claude-code/package.json | grep -x alice")

      with subtest("the claude on alice's PATH is the npm install, a native release binary that runs under nix-ld"):
          where = machine.succeed("su - alice -c 'command -v claude'").strip()
          assert where == "/home/alice/.local/bin/claude", where
          exe = machine.succeed("readlink -f /home/alice/.local/bin/claude").strip()
          print(machine.succeed(f"file -L {exe}"))
          machine.succeed(f"file -L {exe} | grep -F 'interpreter /lib64/ld-linux-x86-64.so.2'")
          out = machine.succeed("su - alice -c 'claude --version' 2>&1")
          assert "${version}" in out, out
    '';
  };

  test = mk {
    name = "rime-claude-code-npm";
    registry = mkRegistry { };
  };
in
test
// {
  mutants.bad-integrity = mk {
    name = "rime-claude-code-npm-mutant-bad-integrity";
    registry = mkRegistry { mainIntegrity = native.outputHash; };
  };
}
