# The vendor apps on Rime on NixOS (D5, docs/nixos/p2-gates.md P4): `rime install chatgpt claude-desktop` puts the
# catalogue's builds (nix/pkgs/rime-vendor-apps: one pinned version of each, verified in the build by rime-pkg's own
# vendor-key check) into the rime-apps profile from a release record, offline, and both apps start.
#
# The VM has no network: the apps come from the store the release record names, as on a machine that substitutes
# them from Rime's cache. Neither app can sign in offline, which is not what is tested. What is tested: the record
# installs both, they are on the user's PATH and their launcher entries start them, `chatgpt --version` names the pinned version, and each Electron app's
# main process starts under X and is still running 25 s later with its renderer up and no missing library.
#
# The build-time half (the bytes are the pinned ones and verify against the vendor key) is checked here too, outside
# the VM: the verified file's sha256 is the pin's. Mutants (passthru.mutants, built by hand; each must FAIL to build):
#
#   inrelease-tampered   Anthropic's kept InRelease with the Packages hash line changed: gpgv refuses the signature.
#   rpm-tampered         OpenAI's pinned rpm with one payload byte flipped: rpmkeys refuses the digest.
{ pkgs, self }:

let
  r = pkgs.rime;
  sources = r.rime-vendor-sources;
  inherit (sources) pins;

  test = pkgs.testers.runNixOSTest {
    name = "rime-vendor-apps";
    node.pkgsReadOnly = false;

    nodes.machine = { config, pkgs, ... }: {
      imports = [ self.nixosModules.default ];
      rime.catalogue.enable = true;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        linger = true;
      };
      environment.systemPackages = [ config.rime.services.rimed.package pkgs.xvfb-run pkgs.procps ];
      # The release record: the two vendor apps of this system's catalogue (what the update client writes).
      environment.etc."rime-test/vendor-outs.json".text = builtins.toJSON {
        apps = {
          chatgpt = "${config.system.build.rimeCatalogue.passthru.apps.chatgpt}";
          claude-desktop = "${config.system.build.rimeCatalogue.passthru.apps.claude-desktop}";
        };
      };
      fonts.enableDefaultPackages = true;
      virtualisation = {
        memorySize = 6144;
        cores = 4;
        diskSize = 4096;
      };
    };

    testScript = ''
      import hashlib, json

      def sha256(path):
          h = hashlib.sha256()
          with open(path, "rb") as f:
              for b in iter(lambda: f.read(1 << 20), b""):
                  h.update(b)
          return h.hexdigest()

      with subtest("the packaged bytes are the pinned ones, verified against the vendor key in the build"):
          assert sha256("${sources.chatgpt}/chatgpt.rpm") == "${pins.chatgpt.sha256}"
          assert sha256("${sources.claude-desktop}/claude-desktop.deb") == "${pins.claude-desktop.sha256}"
          assert "${r.rime-chatgpt.passthru.verified}" == "${sources.chatgpt}"
          assert "${r.rime-claude-desktop.passthru.verified}" == "${sources.claude-desktop}"

      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("user@1000.service")
      RUN = "/run/user/1000"
      def alice(cmd):
          return machine.succeed(f"su - alice -c 'export XDG_RUNTIME_DIR={RUN}; {cmd}' 2>&1")

      with subtest("the catalogue offers both as class 2 apps of the pinned versions"):
          cat = json.loads(machine.succeed("cat /etc/rime/catalogue.json"))
          for name, version in [("chatgpt", "${pins.chatgpt.version}"), ("claude-desktop", "${pins.claude-desktop.version}")]:
              e = cat["entries"][name]
              assert e["kind"] == "nix" and e["buildable"] and e["version"] == version, (name, e)
              assert "pending" not in e or not e["pending"], e

      with subtest("rime install puts both into rime-apps from the release record, offline"):
          machine.fail("ping -c1 -W2 1.1.1.1")
          machine.succeed("install -D -m 644 /etc/rime-test/vendor-outs.json /var/lib/rime/nix/catalogue-outs.json")
          out = machine.succeed("rime install chatgpt claude-desktop 2>&1")
          print(out)
          for name in ["chatgpt", "claude-desktop"]:
              assert f"{name}: installed (in /nix/var/nix/profiles/rime-apps" in out, out
              where = alice(f"command -v {name}").strip()
              assert where.startswith("/nix/var/nix/profiles/rime-apps/"), where
          # The launcher entries start what the profile put on alice's PATH (no /usr/bin path left from the vendor
          # package), and Claude Desktop's entry keeps the claude:// handler.
          apps = "/nix/var/nix/profiles/rime-apps/share/applications"
          for entry, exe in [("com.anthropic.Claude.desktop", "claude-desktop"), ("chatgpt.desktop", "chatgpt")]:
              execs = machine.succeed(f"grep '^Exec=' {apps}/{entry} | cut -d= -f2- | cut -d' ' -f1 | sort -u").split()
              assert execs == [exe], (entry, execs)
              alice(f"command -v {exe}")
          machine.succeed(f"grep -x 'MimeType=x-scheme-handler/claude;.*' {apps}/com.anthropic.Claude.desktop")

      with subtest("chatgpt --version names the pinned version"):
          out = alice("chatgpt --version")
          assert "${pins.chatgpt.version}" in out, out

      for name, unit in [("claude-desktop", "claude"), ("chatgpt", "chatgpt")]:
          with subtest(f"{name} starts under X and keeps running, renderer up, no missing library"):
              alice(f"systemd-run --user --unit={unit} --setenv=PATH ${pkgs.xvfb-run}/bin/xvfb-run -a {name}")
              machine.wait_until_succeeds("ps -u alice -o args= | grep -F -- --type=renderer >/dev/null", timeout=120)
              machine.sleep(25)
              alice(f"systemctl --user is-active {unit}")
              log = alice(f"journalctl --user -u {unit} --no-pager")
              print(log[-4000:])
              for bad in ("error while loading shared libraries", "cannot open shared object file", "Segmentation fault"):
                  assert bad not in log, f"{bad!r} in {name}'s output"
              alice(f"systemctl --user stop {unit}")
              machine.wait_until_fails("ps -u alice -o args= | grep -F -- --type=renderer >/dev/null", timeout=60)
    '';
  };

  tamper = name: files: path: script:
    r.rime-vendor.fetchVerified {
      inherit name;
      files = files // {
        ${path} = pkgs.runCommand "tampered-${baseNameOf path}" { } script;
      };
    };
in
test
// {
  mutants = {
    inrelease-tampered =
      tamper "claude-desktop" sources.files.claude-desktop "dists/stable/InRelease" ''
        sed -E '0,/^ [0-9a-f]{64} +[0-9]+ main\/binary-amd64\/Packages$/s//\ 0000000000000000000000000000000000000000000000000000000000000000 1 main\/binary-amd64\/Packages/' \
          ${pins.claude-desktop.inRelease} > $out
        ! cmp -s ${pins.claude-desktop.inRelease} $out
      '';
    rpm-tampered =
      tamper "chatgpt" sources.files.chatgpt pins.chatgpt.file ''
        cp ${sources.files.chatgpt.${pins.chatgpt.file}} $out
        chmod u+w $out
        size=$(stat -c %s $out)
        # One byte flipped well inside the payload (the header and signature are at the front).
        off=$((size / 2))
        b=$(od -An -tu1 -j $off -N1 $out | tr -d ' ')
        if [ "$b" = 65 ]; then c=B; else c=A; fi
        printf %s "$c" | dd of=$out bs=1 seek=$off conv=notrunc status=none
        ! cmp -s ${sources.files.chatgpt.${pins.chatgpt.file}} $out
      '';
  };
}
