# rimed-session: the rimed cargo tests the build sandbox cannot run, run inside a logged-in user session in a VM.
#
# rime-agentd decides who a client is from /proc/<pid>/cgroup and refuses one in neither a login session nor a user
# service. Every process in the Nix build sandbox is in "/", so rimed.nix skips 26 tests by name
# (skips.needsLoginSessionCgroup) and accepts 50 that print SKIP and return (selfSkipOk.noLoginSessionCgroup). This
# check runs exactly those 76, from the same release test binaries, as a user logged in on tty1 (getty autologin:
# session-N.scope under user-1000.slice), and requires every one to have run, passed and printed no SKIP line.
#
# The negative control: the same runner as the same user from the driver's root shell (runuser, so the process is
# in backdoor.service, a system service and not a session) must refuse them: rime-agentd's refusal shows up as SKIP
# lines or failures. That proves it is the login session, not the VM, that lets them run.
#
# The totals are in $out/totals.txt (docs/nixos/p1-exit.md, cargo section).
{ pkgs, self }:

let
  lib = pkgs.lib;
  rimed = pkgs.rime.rimed;
  names = lib.unique (rimed.passthru.sessionTests or (throw "rimed.passthru.sessionTests is missing"));

  # The workspace's release test binaries, built the way rimed's checkPhase builds them (same source, same
  # RIME_PATH_* values, same profile), but only compiled. cargo bakes absolute build paths into them
  # (CARGO_MANIFEST_DIR, CARGO_BIN_EXE_*), so $out keeps the source tree and the target directory's executables, and
  # the VM recreates /build/source from it.
  testBins = rimed.overrideAttrs (old: {
    pname = "rimed-session-tests";
    doCheck = false;
    buildPhase = ''
      runHook preBuild
      echo "build dir: $PWD"
      (cd rimed && cargo test --release --offline --workspace --no-run -j "$NIX_BUILD_CORES" \
        --target ${pkgs.stdenv.hostPlatform.rust.rustcTargetSpec} --message-format=json-render-diagnostics \
        > "$NIX_BUILD_TOP/cargo-messages.json")
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p $out/tests
      echo "$PWD" > $out/build-dir
      # one line per test binary: the executable, then the directory cargo runs it in (its package's)
      python3 - "$NIX_BUILD_TOP/cargo-messages.json" > tests.tsv <<'PY'
      import json, os, sys
      for line in open(sys.argv[1]):
          if not line.startswith("{"):
              continue
          m = json.loads(line)
          if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") and m.get("executable"):
              print(m["executable"] + "\t" + os.path.dirname(m["manifest_path"]))
      PY
      [ -s tests.tsv ] || { echo "no test binaries in cargo's messages"; exit 1; }
      while IFS=$'\t' read -r exe dir; do
        cp "$exe" $out/tests/
        printf '%s\t%s\n' "$out/tests/$(basename "$exe")" "$dir" >> $out/tests.tsv
      done < tests.tsv
      # The programs the integration tests start by CARGO_BIN_EXE_<name>: every executable at the top of the profile
      # directory. The VM links the directory they were compiled with to here.
      rel=$(cd "$(dirname "$(head -n 1 tests.tsv | cut -f1)")/.." && pwd)
      mkdir -p $out/profile
      for f in "$rel"/*; do if [ -f "$f" ] && [ -x "$f" ]; then cp "$f" $out/profile/; fi; done
      echo "$rel" > $out/profile-dir
      # The source tree without any target directory, for fixtures read at run time through CARGO_MANIFEST_DIR.
      mkdir -p $out/source
      tar --exclude=./rimed/target --exclude=./target -cf - . | tar -xf - -C $out/source
      runHook postInstall
    '';
    postInstall = "";
    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.python3 ];
  });

  # Runs every test binary with the 76 names as filters (libtest runs a test matching any of them), each in its
  # package directory as cargo would, with the environment rimed's preCheck gives (a fresh HOME, git identity, CA
  # file). $1 = the output file.
  runner = pkgs.writeShellScript "rimed-session-run" ''
    set -u
    out=$1
    export HOME=$(mktemp -d) SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
    export XDG_RUNTIME_DIR=''${XDG_RUNTIME_DIR:-$(mktemp -d)}
    git config --global user.name rime-test
    git config --global user.email rime-test@localhost
    git config --global init.defaultBranch main
    {
      echo "cgroup: $(cat /proc/self/cgroup)"
      while IFS=$'\t' read -r exe dir; do
        echo "== $(basename "$exe")"
        (cd "$dir" && "$exe" --show-output --test-threads=4 ${lib.escapeShellArgs names})
        echo "== rc $?"
      done < ${testBins}/tests.tsv
    } > "$out" 2>&1
  '';

  # Reads a runner's output: per name, how many tests matching it passed, failed, or printed a SKIP line (the
  # regex rimed-test-report.py uses).
  parse = ''
    import re
    SELF_SKIP = re.compile(r"^\s*(?:SKIP|skipp(?:ed|ing))\b", re.IGNORECASE)
    NAMES = ${builtins.toJSON names}

    def matches(test, name):
        return test == name or test.endswith("::" + name)

    def parse(text):
        st = {n: {"ok": 0, "failed": 0, "skip": []} for n in NAMES}
        block = None
        for line in text.splitlines():
            m = re.match(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", line)
            if m:
                for n in NAMES:
                    if matches(m.group(1), n):
                        st[n]["ok" if m.group(2) == "ok" else "failed"] += 1
                continue
            m = re.match(r"^---- (\S+) stdout ----$", line)
            if m:
                block = m.group(1)
                continue
            if line.startswith(("successes:", "failures:", "running ", "== ")):
                block = None
            if block and SELF_SKIP.search(line):
                for n in NAMES:
                    if matches(block, n):
                        st[n]["skip"].append(line.strip())
        return st

    def clean(s):
        return s["ok"] > 0 and s["failed"] == 0 and not s["skip"]
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-rimed-session";

  nodes.machine =
    { pkgs, ... }:
    {
      virtualisation.memorySize = 3072;
      virtualisation.cores = 4;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      # A real login: getty on tty1 logs alice in (pam_systemd: session-N.scope), and her login shell runs the tests.
      services.getty.autologinUser = "alice";
      programs.bash.loginShellInit = ''
        if [ "$(tty)" = /dev/tty1 ] && [ ! -e /tmp/rst/started ]; then
          mkdir -p /tmp/rst && touch /tmp/rst/started
          ${runner} /tmp/rst/session.txt
          echo $? > /tmp/rst/rc
        fi
      '';
      # What the tests use besides their compiled-in store paths, as rimed's checkPhase provides it.
      environment.systemPackages = with pkgs; [ bubblewrap curl git openssh openssl python3 util-linux ];
      systemd.tmpfiles.rules = [
        "L+ /bin/true - - - - ${pkgs.coreutils}/bin/true"
        "L+ /bin/setpriv - - - - ${pkgs.util-linux}/bin/setpriv"
      ];
      # The build directory the binaries were compiled in, before alice can log in: the source tree (writable, hers),
      # the profile directory linked to the store copy, and cargo's per-target tmp directory.
      systemd.services.rimed-test-tree = {
        requiredBy = [ "getty@tty1.service" ];
        before = [ "getty@tty1.service" ];
        serviceConfig.Type = "oneshot";
        path = [ pkgs.coreutils ];
        script = ''
          src=$(cat ${testBins}/build-dir)
          prof=$(cat ${testBins}/profile-dir)
          mkdir -p "$(dirname "$src")"
          cp -r ${testBins}/source "$src"
          # $prof is <target>/<triple>/release; CARGO_TARGET_TMPDIR is <target>/tmp
          mkdir -p "$(dirname "$prof")" "$(dirname "$(dirname "$prof")")/tmp"
          chown alice "$(dirname "$(dirname "$prof")")/tmp"
          ln -sfn ${testBins}/profile "$prof"
          chown -R alice "$src"
          chmod -R u+w "$src"
        '';
      };
    };

  testScript = parse + ''
    import json
    machine.wait_for_file("/tmp/rst/rc", timeout=1800)
    session = machine.succeed("cat /tmp/rst/session.txt")
    print(session)
    print("runner exit status in the session:", machine.succeed("cat /tmp/rst/rc").strip())
    assert "session-" in session.splitlines()[0] and ".scope" in session.splitlines()[0], session.splitlines()[0]

    st = parse(session)
    bad = {n: s for n, s in st.items() if not clean(s)}
    for n, s in st.items():
        print(("PASS " if clean(s) else "BAD  ") + n + ": " + json.dumps(s))
    total_ok = sum(s["ok"] for s in st.values())

    with subtest("negative control: the same runner outside a login session"):
        machine.succeed("runuser -u alice -- ${runner} /tmp/rst/outside.txt || true")
        outside = machine.succeed("cat /tmp/rst/outside.txt")
        assert "backdoor.service" in outside.splitlines()[0], outside.splitlines()[0]
        ost = parse(outside)
        refused = [n for n, s in ost.items() if not clean(s)]
        print(f"outside a session: {len(refused)} of {len(NAMES)} refused (SKIP or failed)")

    totals = (
        f"rimed-session: {len(NAMES)} names; in a login session {len(NAMES) - len(bad)} passed without SKIP "
        f"({total_ok} test runs ok), {len(bad)} did not; outside one (backdoor.service) {len(refused)} refused\n"
    )
    print(totals)
    machine.succeed(f"echo {json.dumps(totals)} > /tmp/rst/totals.txt")
    machine.copy_from_vm("/tmp/rst/totals.txt", "")
    machine.copy_from_vm("/tmp/rst/session.txt", "")
    assert not bad, "not clean in a login session: " + ", ".join(sorted(bad))
    assert len(refused) >= len(NAMES) // 2, f"only {len(refused)} refused outside a session: the control proves nothing"
  '';
}
