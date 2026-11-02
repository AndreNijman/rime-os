# The CI plumbing as flake checks (no VM):
#   ci-userns  bubblewrap can make its user namespace inside the Nix build sandbox. rimed's checkPhase depends on it,
#              and on a GitHub runner (AppArmor's userns restriction) it is the first thing to fail; the workflow
#              builds this before rimed so the failure has a name.
#   ci         nix/ci/run-checks.sh, tests/check-suites-run-in-ci.sh and nix/pkgs/rimed-test-report.py, on this
#              repository as it is and against mutants each of which must be refused; plus actionlint and
#              ci/runner/check-workflows.py over the workflows. run-checks.sh's classify is proved on real logs
#              (nix/ci/fixtures) and its single download retry with a stub nix.
{ pkgs, self }:

let
  lib = pkgs.lib;
  root = ../..;

  # The check names, from the flake itself (attrNames does not force any check, this one included).
  checkNames = pkgs.writeText "rime-flake-checks" (
    lib.concatLines (builtins.attrNames self.checks.${pkgs.stdenv.hostPlatform.system})
  );

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/flake.nix")
      (root + "/.github")
      (root + "/tests")
      (lib.fileset.fileFilter (f: lib.hasPrefix "test-" f.name || lib.hasPrefix "check-" f.name) (root + "/installer"))
      (root + "/nix/ci")
      (root + "/nix/tests")
      (root + "/nix/pkgs/rimed-test-report.py")
      (root + "/ci/runner/check-workflows.py")
    ];
  };

  report = ../pkgs/rimed-test-report.py;
in
{
  ci-userns = pkgs.runCommand "rime-ci-userns" { nativeBuildInputs = [ pkgs.bubblewrap ]; } ''
    # The shape rimed's test shim uses: a new user namespace, the build root bound in, /run a tmpfs.
    bwrap --unshare-user --dev-bind /dev /dev --bind /proc /proc --tmpfs /run --ro-bind /nix /nix \
      --bind "$NIX_BUILD_TOP" "$NIX_BUILD_TOP" -- ${pkgs.coreutils}/bin/true
    echo "bubblewrap made a user namespace inside the build sandbox" > $out
  '';

  ci = pkgs.runCommand "rime-ci"
    {
      nativeBuildInputs = with pkgs; [ actionlint shellcheck bash coreutils findutils gawk git gnugrep gnused (python3.withPackages (p: [ p.pyyaml ])) ];
    }
    ''
      set -euo pipefail
      fails=0
      ok() { echo "PASS: $*"; }
      bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
      # expect RC DESCRIPTION -- CMD...: CMD must exit with RC (0, or "nonzero").
      expect() {
        local want=$1 what=$2; shift 3
        set +e; "$@" > "$NIX_BUILD_TOP/last.out" 2>&1; local rc=$?; set -e
        if { [ "$want" = 0 ] && [ "$rc" = 0 ]; } || { [ "$want" = nonzero ] && [ "$rc" != 0 ]; }; then ok "$what (rc $rc)"
        else bad "$what (rc $rc, wanted $want)"; sed 's/^/    | /' "$NIX_BUILD_TOP/last.out"; fi
      }
      has() { grep -F -- "$1" "$NIX_BUILD_TOP/last.out" > /dev/null || { bad "output lacks: $1"; sed 's/^/    | /' "$NIX_BUILD_TOP/last.out"; }; }
      fresh() { rm -rf "$NIX_BUILD_TOP/repo"; cp -r ${src} "$NIX_BUILD_TOP/repo"; chmod -R u+w "$NIX_BUILD_TOP/repo"; cd "$NIX_BUILD_TOP/repo"; }
      export RIME_FLAKE_CHECKS_FILE=${checkNames}

      echo "== the repository as it is"
      fresh
      expect 0 "plan accepts the skip list" -- bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/plan"
      has "SKIPPED vm-boot: needs the Rime binary cache (TODO(handover) R2)"
      has "WILL RUN rimed"
      expect 0 "every suite and flake check has a CI home" -- bash tests/check-suites-run-in-ci.sh
      has "run by .github/workflows/nix.yml"
      expect 0 "actionlint accepts nix.yml" -- actionlint .github/workflows/nix.yml
      expect 0 "check-workflows.py: no self-hosted job breaks the fork/key rules" -- python3 ci/runner/check-workflows.py .github/workflows
      # The workflow runs only on hosted images.
      if grep -E '^\s*runs-on:' .github/workflows/nix.yml | grep -vE 'runs-on: ubuntu-24\.04\s*$' > /dev/null; then
        bad "nix.yml has a job off ubuntu-24.04"
      else ok "every nix.yml job runs on ubuntu-24.04"; fi

      echo "== mutant: a skip entry for a check that does not exist"
      fresh; echo "no-such-check   needs nothing" >> nix/ci/checks-not-in-ci.txt
      expect nonzero "plan refuses it" -- bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/m1"
      has "skips 'no-such-check', which is not a flake check"
      expect nonzero "check-suites refuses it" -- bash tests/check-suites-run-in-ci.sh

      echo "== mutant: a skip entry with no reason"
      fresh; sed -i 's/^vm-boot .*/vm-boot/' nix/ci/checks-not-in-ci.txt
      expect nonzero "plan refuses it" -- bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/m2"
      expect nonzero "check-suites refuses it" -- bash tests/check-suites-run-in-ci.sh

      echo "== mutant: a skip line deleted makes the check run"
      fresh; sed -i '/^kernel-gate /d' nix/ci/checks-not-in-ci.txt
      expect 0 "plan" -- bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/m3"
      has "WILL RUN kernel-gate"
      grep -F '"kernel-gate"' "$NIX_BUILD_TOP/m3/run.json" > /dev/null && ok "kernel-gate is in the matrix" || bad "kernel-gate not in run.json"

      echo "== mutant: nix.yml no longer runs the plan"
      fresh; sed -i 's|nix/ci/run-checks.sh plan|nix/ci/run-checks.sh list|' .github/workflows/nix.yml
      expect nonzero "check-suites says no flake check runs in CI" -- bash tests/check-suites-run-in-ci.sh
      has "does not run nix/ci/run-checks.sh plan"

      echo "== a suite whose only home is a flake check"
      # Built from two halves: written whole, this file (a nix/tests file CI runs) would be its home.
      fx="tests/test-ci-""fixture-only.sh"
      fresh; printf '#!/bin/sh\ntrue\n' > "$fx"
      echo "# $fx is named here only in a comment" >> nix/tests/rimed.nix
      expect nonzero "a comment is not a home" -- bash tests/check-suites-run-in-ci.sh
      has "$fx"
      sed -i "1i let fixture = \"$fx\"; in" nix/tests/rimed.nix
      expect 0 "run by the rimed check, which CI runs" -- bash tests/check-suites-run-in-ci.sh
      has "1 suite(s) run through a flake check"
      fresh; printf '#!/bin/sh\ntrue\n' > "$fx"
      sed -i "1i let fixture = \"$fx\"; in" nix/tests/vm-boot.nix
      expect nonzero "named only by vm-boot, which CI skips: unrun" -- bash tests/check-suites-run-in-ci.sh
      has "$fx"

      echo "== no flake check list and no nix: said, not passed silently"
      fresh
      expect 0 "the bootc half still runs" -- env -u RIME_FLAKE_CHECKS_FILE bash tests/check-suites-run-in-ci.sh
      has "flake checks not verified here"
      echo "== no flake.nix (main, bootc): the flake half does not exist"
      fresh; rm flake.nix
      expect 0 "bootc behaviour" -- env -u RIME_FLAKE_CHECKS_FILE bash tests/check-suites-run-in-ci.sh
      if grep -F "flake checks" "$NIX_BUILD_TOP/last.out" > /dev/null; then bad "flake output without a flake"; else ok "no flake output"; fi

      echo "== summary: every check RAN or SKIPPED"
      fresh
      bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/p" > /dev/null
      mkdir -p "$NIX_BUILD_TOP/r"
      while read -r c; do printf 'pass\t1\n' > "$NIX_BUILD_TOP/r/$c.result"; done < "$NIX_BUILD_TOP/p/run.txt"
      expect 0 "all pass" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p" "$NIX_BUILD_TOP/r"
      has '| `vm-boot` | SKIPPED | not built | needs the Rime binary cache (TODO(handover) R2)'
      has '| `rimed` | RAN | pass (1 s) |'
      printf 'fail\t9\n' > "$NIX_BUILD_TOP/r/rimed.result"
      expect nonzero "a failed check fails the summary" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p" "$NIX_BUILD_TOP/r"
      printf 'pass\t1\n' > "$NIX_BUILD_TOP/r/rimed.result"; rm "$NIX_BUILD_TOP/r/shell.result"
      expect nonzero "a check with no result fails the summary" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p" "$NIX_BUILD_TOP/r"
      has '| `shell` | RAN | **NO RESULT** |'
      printf 'pass\t1\n' > "$NIX_BUILD_TOP/r/shell.result"
      printf 'timeout\t4501\n' > "$NIX_BUILD_TOP/r/libexec.result"
      expect nonzero "a timed-out check fails the summary" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p" "$NIX_BUILD_TOP/r"
      has '| `libexec` | RAN | **TIMED OUT** (stopped after 4501 s) |'
      printf 'setup\t0\n' > "$NIX_BUILD_TOP/r/libexec.result"
      expect nonzero "a check whose job failed in setup fails the summary" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p" "$NIX_BUILD_TOP/r"
      has '| `libexec` | NOT RUN | **FAIL** before the build |'
      has '| `hostlayer-katana` | SKIPPED | not built | needs katana'"'"'s real state'

      echo "== build: RIME_CHECK_TIMEOUT stops a build and records it as timeout, not fail"
      mkdir -p "$NIX_BUILD_TOP/stub" "$NIX_BUILD_TOP/b"
      printf '#!${pkgs.runtimeShell}\nexec sleep 30\n' > "$NIX_BUILD_TOP/stub/nix"
      chmod +x "$NIX_BUILD_TOP/stub/nix"
      expect nonzero "a build past its limit" -- env PATH="$NIX_BUILD_TOP/stub:$PATH" RIME_CHECK_TIMEOUT=1 bash nix/ci/run-checks.sh build slow "$NIX_BUILD_TOP/b"
      has "slow did not finish in 1 s"
      if grep -E '^timeout' "$NIX_BUILD_TOP/b/slow.result" > /dev/null; then ok "recorded as timeout"; else bad "recorded as $(cat "$NIX_BUILD_TOP/b/slow.result")"; fi
      printf '#!${pkgs.runtimeShell}\nexit 1\n' > "$NIX_BUILD_TOP/stub/nix"
      expect nonzero "a build that fails inside its limit" -- env PATH="$NIX_BUILD_TOP/stub:$PATH" RIME_CHECK_TIMEOUT=30 bash nix/ci/run-checks.sh build quick "$NIX_BUILD_TOP/b"
      if grep -E '^fail' "$NIX_BUILD_TOP/b/quick.result" > /dev/null; then ok "recorded as fail"; else bad "recorded as $(cat "$NIX_BUILD_TOP/b/quick.result")"; fi
      echo "== mutant: run-checks.sh without the time limit (the build runs on until the job is killed)"
      printf '#!${pkgs.runtimeShell}\nexec sleep 3\n' > "$NIX_BUILD_TOP/stub/nix"
      sed 's/if \[ "$limit" -gt 0 \]; then$/if false; then/' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-mutant.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-mutant.sh"; then bad "mutant: pattern not found"; fi
      set +e; env PATH="$NIX_BUILD_TOP/stub:$PATH" RIME_CHECK_TIMEOUT=1 bash "$NIX_BUILD_TOP/rc-mutant.sh" build slow2 "$NIX_BUILD_TOP/b" > /dev/null 2>&1; set -e
      if grep -E '^timeout' "$NIX_BUILD_TOP/b/slow2.result" > /dev/null; then bad "mutant: still recorded a timeout"; else ok "killed: no time limit, no timeout recorded ($(cut -f1 "$NIX_BUILD_TOP/b/slow2.result"))"; fi

      echo "== classify: a download failure (retried) or not (never retried); the fixtures are real logs"
      cd "$NIX_BUILD_TOP/repo"
      F=nix/ci/fixtures
      expect 0 "HTTP 416 on a resumed NAR" -- bash nix/ci/run-checks.sh classify $F/download-416.log
      has "a substitute could not be downloaded"
      expect 0 "curl: Failed sending data to the peer" -- bash nix/ci/run-checks.sh classify $F/download-curl-send.log
      expect nonzero "a builder that failed" -- bash nix/ci/run-checks.sh classify $F/build-failure.log
      has "a builder failed"
      expect nonzero "a builder that failed after nix recovered from download warnings" -- bash nix/ci/run-checks.sh classify $F/build-failure-after-download-warnings.log
      expect nonzero "an evaluation error" -- bash nix/ci/run-checks.sh classify $F/eval-error.log
      # a substitute that failed AND a builder that failed in the same build: a real failure
      cat $F/download-416.log $F/build-failure.log > "$NIX_BUILD_TOP/both.log"
      expect nonzero "a download failure and a builder failure" -- bash nix/ci/run-checks.sh classify "$NIX_BUILD_TOP/both.log"
      # A test's own output that quotes nix's words is not nix's line.
      sed 's/^/vm-test-run-x> /' $F/download-416.log > "$NIX_BUILD_TOP/quoted.log"
      expect nonzero "nix's words inside a build's -L output" -- bash nix/ci/run-checks.sh classify "$NIX_BUILD_TOP/quoted.log"

      echo "== build: retried ONCE on a download failure, never on a builder failure"
      # stub nix: the first call prints $FIRST and fails; later calls print $THEN and exit $THEN_RC
      cat > "$NIX_BUILD_TOP/stub/nix" <<EOF
      #!${pkgs.runtimeShell}
      n=\$(( \$(cat "\$CALLS" 2>/dev/null || echo 0) + 1 )); echo \$n > "\$CALLS"
      if [ \$n = 1 ]; then cat "\$FIRST"; exit 1; fi
      cat "\$THEN"; exit "\$THEN_RC"
      EOF
      chmod +x "$NIX_BUILD_TOP/stub/nix"
      echo "/nix/store/00000000000000000000000000000000-ok" > "$NIX_BUILD_TOP/ok.log"
      runb() { # NAME FIRST THEN THEN_RC [SCRIPT]
        rm -f "$NIX_BUILD_TOP/calls"
        expect "$5" "build $1" -- env PATH="$NIX_BUILD_TOP/stub:$PATH" CALLS="$NIX_BUILD_TOP/calls" FIRST="$2" THEN="$3" THEN_RC="$4" \
          RIME_CHECK_TIMEOUT=600 bash "''${6:-nix/ci/run-checks.sh}" build "$1" "$NIX_BUILD_TOP/rb"
      }
      runb dl $F/download-416.log "$NIX_BUILD_TOP/ok.log" 0 0
      has "dl failed on a download; retrying ONCE"
      [ "$(cat "$NIX_BUILD_TOP/calls")" = 2 ] && ok "two attempts" || bad "attempts: $(cat "$NIX_BUILD_TOP/calls")"
      grep -E "^pass[[:space:]]+[0-9]+[[:space:]]+retried$" "$NIX_BUILD_TOP/rb/dl.result" > /dev/null && ok "recorded as pass, retried" || bad "dl.result: $(cat "$NIX_BUILD_TOP/rb/dl.result")"
      [ -s "$NIX_BUILD_TOP/rb/dl.first.log" ] && ok "the first attempt's log is kept" || bad "no dl.first.log"
      runb dl2 $F/download-416.log $F/download-416.log 1 nonzero
      [ "$(cat "$NIX_BUILD_TOP/calls")" = 2 ] && ok "a second download failure is not retried again" || bad "attempts: $(cat "$NIX_BUILD_TOP/calls")"
      grep -E "^fail[[:space:]]+[0-9]+[[:space:]]+retried$" "$NIX_BUILD_TOP/rb/dl2.result" > /dev/null && ok "recorded as fail, retried" || bad "dl2.result: $(cat "$NIX_BUILD_TOP/rb/dl2.result")"
      runb real $F/build-failure-after-download-warnings.log "$NIX_BUILD_TOP/ok.log" 0 nonzero
      [ "$(cat "$NIX_BUILD_TOP/calls")" = 1 ] && ok "a builder failure is built once" || bad "a builder failure was retried"
      grep -E "^fail[[:space:]]+[0-9]+[[:space:]]*$" "$NIX_BUILD_TOP/rb/real.result" > /dev/null && ok "recorded as fail" || bad "real.result: $(cat "$NIX_BUILD_TOP/rb/real.result")"
      # the summary says a pass needed its retry
      bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/p2" > /dev/null
      mkdir -p "$NIX_BUILD_TOP/r2"
      while read -r c; do printf 'pass\t1\t\n' > "$NIX_BUILD_TOP/r2/$c.result"; done < "$NIX_BUILD_TOP/p2/run.txt"
      printf 'pass\t7\tretried\n' > "$NIX_BUILD_TOP/r2/rimed.result"
      expect 0 "a retried pass is a pass" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/p2" "$NIX_BUILD_TOP/r2"
      has '| `rimed` | RAN | pass (7 s) | after ONE retry: the first attempt failed on a download |'
      has '| `shell` | RAN | pass (1 s) | |'

      echo "== mutant: classify ignores builder failures (a real failure would be retried)"
      sed 's/^    if grep -E "^error: builder for /    if false \&\& grep -E "^error: builder for /' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-m-classify.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-m-classify.sh"; then bad "mutant: pattern not found"; fi
      runb mreal "$NIX_BUILD_TOP/both.log" "$NIX_BUILD_TOP/ok.log" 0 0 "$NIX_BUILD_TOP/rc-m-classify.sh"
      [ "$(cat "$NIX_BUILD_TOP/calls")" = 2 ] && ok "killed: the mutant retried a builder failure (and the check passed)" || bad "mutant not caught"
      echo "== mutant: no retry"
      sed 's/^        if \[ "\$limit" = 0 \] || \[ "\$left" -ge 60 \]; then$/        if false; then/' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-m-noretry.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-m-noretry.sh"; then bad "mutant: pattern not found"; fi
      runb mdl $F/download-416.log "$NIX_BUILD_TOP/ok.log" 0 nonzero "$NIX_BUILD_TOP/rc-m-noretry.sh"
      grep -E "^fail" "$NIX_BUILD_TOP/rb/mdl.result" > /dev/null && ok "killed: without the retry the download failure is a fail" || bad "mutant not caught"

      echo "== rimed-test-report.py"
      cd "$NIX_BUILD_TOP"
      cat > meta.json <<'EOF'
      {"packages":[{"name":"alpha","manifest_path":"/b/alpha/Cargo.toml","targets":[
        {"name":"alpha","kind":["lib"],"src_path":"/b/alpha/src/lib.rs"},
        {"name":"wire","kind":["test"],"src_path":"/b/alpha/tests/wire.rs"}]},
       {"name":"beta-d","manifest_path":"/b/beta-d/Cargo.toml","targets":[
        {"name":"beta-d","kind":["bin"],"src_path":"/b/beta-d/src/main.rs"}]}]}
      EOF
      cat > skips.json <<'EOF'
      {"skip":{"needsThing":["tests::skipped_one"]},"selfSkipOk":{}}
      EOF
      cat > skips-ok.json <<'EOF'
      {"skip":{"needsThing":["tests::skipped_one"]},"selfSkipOk":{"noDaemon":["w"]}}
      EOF
      good() {
        printf '%s\n' \
          '     Running unittests src/lib.rs (/b/target/release/deps/alpha-0123456789abcdef)' \
          'running 3 tests' 'test tests::a ... ok' 'test tests::b ... ok' 'test tests::c ... ignored, needs a GPU' \
          'test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 1 filtered out; finished in 0.01s' \
          '     Running tests/wire.rs (/b/target/release/deps/wire-fedcba9876543210)' \
          'running 1 test' 'test w ... ok' "$1" \
          'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s' \
          '     Running unittests src/main.rs (/b/target/release/deps/beta_d-00112233aabbccdd)' \
          'running 4 tests' \
          'test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s' \
          '   Doc-tests alpha' 'running 0 tests' \
          'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'
      }
      good "" > good.log
      expect 0 "a clean log" -- python3 ${report} good.log meta.json skips.json r.txt r.json
      has "alpha                      3       3        0       0        1        1"
      has "beta-d                     1       4        0       0        0        0"
      has "TOTAL                      4       7        0       0        1        1"
      has "alpha: tests::c: needs a GPU"
      sed 's/^test result: ok. 4 passed; 0 failed/test result: FAILED. 3 passed; 1 failed/' good.log > failed.log
      expect nonzero "a failed test" -- python3 ${report} failed.log meta.json skips.json r.txt r.json
      sed 's/1 filtered out/0 filtered out/' good.log > stale.log
      expect nonzero "a skip name that filtered nothing" -- python3 ${report} stale.log meta.json skips.json r.txt r.json
      has "a skip entry matches no test"
      { good "" | sed '/^test w \.\.\. ok$/a\
      successes:\
      \
      ---- w stdout ----\
      SKIP: the daemon did not come up in this environment\
      \
      successes:\
          w'; } > selfskip.log
      expect nonzero "a test that passed by skipping itself" -- python3 ${report} selfskip.log meta.json skips.json r.txt r.json
      has "w (alpha) passed by skipping itself"
      expect 0 "a self-skip named in selfSkipOk" -- python3 ${report} selfskip.log meta.json skips-ok.json r.txt r.json
      # counted as selfskip, NOT as passed (cargo said ok)
      has "alpha                      3       2        1       0        1        1"
      has "TOTAL                      4       6        1       0        1        1"
      has "noDaemon: alpha: w: SKIP: the daemon did not come up"
      # The other spelling tests use ("skipped: <why>", lower case) is a self-skip too.
      sed 's/^\s*SKIP: the daemon did not come up in this environment$/skipped: loginctl is not on PATH/' selfskip.log > selfskip2.log
      grep -F 'skipped: loginctl is not on PATH' selfskip2.log > /dev/null && ok "fixture: a lower-case skipped: line" || bad "fixture selfskip2.log has no skipped: line"
      expect nonzero "a test that passed by printing skipped:" -- python3 ${report} selfskip2.log meta.json skips.json r.txt r.json
      has "w (alpha) passed by skipping itself ('skipped: loginctl is not on PATH')"
      expect 0 "the same, named in selfSkipOk" -- python3 ${report} selfskip2.log meta.json skips-ok.json r.txt r.json
      has "TOTAL                      4       6        1       0        1        1"
      expect nonzero "a selfSkipOk name that no longer skips itself" -- python3 ${report} good.log meta.json skips-ok.json r.txt r.json
      has "selfSkipOk names w, which did not skip itself"
      grep -v 'Running tests/wire.rs' good.log > lost.log
      expect nonzero "a test binary with no header" -- python3 ${report} lost.log meta.json skips.json r.txt r.json

      echo
      if [ "$fails" -gt 0 ]; then echo "ci: $fails failure(s)"; exit 1; fi
      echo "ci: all passed" | tee $out
    '';
}
