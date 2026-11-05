# The CI plumbing as flake checks (no VM):
#   ci-userns  bubblewrap can make its user namespace inside the Nix build sandbox. rimed's checkPhase depends on it,
#              and on a GitHub runner (AppArmor's userns restriction) it is the first thing to fail; the workflow
#              builds this before rimed so the failure has a name.
#   ci         nix/ci/run-checks.sh, tests/check-suites-run-in-ci.sh and nix/pkgs/rimed-test-report.py, on this
#              repository as it is and against mutants each of which must be refused; plus actionlint and
#              ci/runner/check-workflows.py over the workflows. run-checks.sh's classify is proved on real logs
#              (nix/ci/fixtures) and its single download retry with a stub nix; the dev cache and check reuse
#              (prebuild, REUSED, export-result, results-merge, fresh runs, metrics) with a stub nix, and
#              nix/ci/check-cache-isolation.py (no release builds from a CI cache), each with mutants.
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
      nativeBuildInputs = with pkgs; [ actionlint shellcheck bash coreutils findutils gawk git gnugrep gnused zstd (python3.withPackages (p: [ p.pyyaml ])) ];
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

      echo "== a release never builds from a CI cache (nix/ci/check-cache-isolation.py)"
      cd "$NIX_BUILD_TOP"; fresh
      expect 0 "nix-release, nix-kernel-sign and build-image use no CI cache" -- python3 nix/ci/check-cache-isolation.py .github/workflows
      expect nonzero "the guard sees nix.yml's own caches (it can see what it guards against)" -- python3 nix/ci/check-cache-isolation.py .github/workflows nix.yml
      has "names the CI dev cache"
      has "holds Nix store paths or a CI cache"
      grep -F 'nix/ci/check-cache-isolation.py' .github/workflows/nix.yml > /dev/null && ok "nix.yml states the guard" || bad "nix.yml does not name the guard"
      mutwf() { # FILE PYTHON-EXPR-ON-d: edit a workflow as YAML (comments are lost; the guard does not read them)
        python3 -c 'import sys, yaml; p = sys.argv[1]; d = yaml.safe_load(open(p)); exec(sys.argv[2]); yaml.safe_dump(d, open(p, "w"))' "$@"
      }
      echo "== mutant: nix-release.yml restores Nix paths from the Actions cache"
      fresh; mutwf .github/workflows/nix-release.yml 'next(iter(d["jobs"].values()))["steps"].insert(0, {"uses": "actions/cache/restore@v4", "with": {"path": "/nix/store", "key": "k"}})'
      expect nonzero "killed: the guard refuses it" -- python3 nix/ci/check-cache-isolation.py .github/workflows
      has "nix-release.yml: job"
      echo "== mutant: build-image.yml builds with the dev cache as an unsigned substituter"
      fresh; mutwf .github/workflows/build-image.yml 'next(iter(d["jobs"].values()))["steps"].append({"run": "nix build --option extra-substituters file:///x/rime-dev-cache?trusted=1 .#a"})'
      expect nonzero "killed: the guard refuses it" -- python3 nix/ci/check-cache-isolation.py .github/workflows
      has "a substituter trusted without signatures"
      echo "== mutant: nix-kernel-sign.yml renamed (the guard must move with it)"
      fresh; mv .github/workflows/nix-kernel-sign.yml .github/workflows/kernel-sign.yml
      expect nonzero "killed: a missing release workflow is refused" -- python3 nix/ci/check-cache-isolation.py .github/workflows
      has "nix-kernel-sign.yml: missing"

      echo "== reuse: a check that PASSED EARLIER on the same inputs is not built again"
      # stub nix and nix-store: a binary cache is a directory of <hash>.narinfo; JSON path-info for cihelp export.
      S=$NIX_BUILD_TOP/stub2; mkdir -p "$S" "$NIX_BUILD_TOP/store"
      cat > "$S/nix" <<'EOF'
      #!/usr/bin/env bash
      cmd=$1; shift
      case "$cmd" in
        path-info)
          store= json=0 ps=()
          while [ $# -gt 0 ]; do case "$1" in --store) store=$2; shift 2 ;; --json-format) shift 2 ;; --json) json=1; shift ;; -r) shift ;; *) ps+=("$1"); shift ;; esac; done
          if [ -n "$store" ]; then d=''${store#file://}; d=''${d%%\?*}; for p in "''${ps[@]}"; do b=''${p##*/}; [ -f "$d/''${b:0:32}.narinfo" ] || exit 1; done; exit 0; fi
          if [ "$json" = 1 ]; then
            sep=; printf '{'
            for p in "''${ps[@]}"; do printf '%s"%s":{"narHash":"sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=","narSize":4,"references":[],"signatures":[],"ultimate":%s}' "$sep" "$p" "''${STUB_ULTIMATE:-false}"; sep=,; done
            printf '}\n'
          fi
          exit 0 ;;
        eval) a=$2; h=$(printf '%s' "$a$4" | sha256sum | cut -c1-32); n=''${a##*.}; printf '/nix/store/%s-%s.drv' "$h" "$n" ;;
        build) echo "stub nix build $*" >> "$NIX_BUILD_TOP/stub2.builds"; exit "''${STUB_BUILD_RC:-0}" ;;
        *) echo "stub nix: $cmd $*" >&2; exit 1 ;;
      esac
      EOF
      cat > "$S/nix-store" <<'EOF'
      #!/usr/bin/env bash
      case "$1" in
        -q) b=''${3##*/}; echo "$NIX_BUILD_TOP/store/''${b%.drv}" ;;
        --dump) printf 'nar of %s' "$2" ;;
      esac
      EOF
      chmod +x "$S/nix" "$S/nix-store"
      sed -i '1s|.*|#!${pkgs.runtimeShell}|' "$S/nix" "$S/nix-store"
      fresh
      OUT=/nix/store/0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-vm-test-run-rimed
      OUT2=/nix/store/0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-vm-test-run-shell
      D=$NIX_BUILD_TOP/drvs.tsv
      printf 'checks.x86_64-linux.rimed.drvPath\t/nix/store/1a-rimed.drv\t%s\n' "$OUT" > "$D"
      printf 'checks.x86_64-linux.shell.drvPath\t/nix/store/1b-shell.drv\t%s\n' "$OUT2" >> "$D"
      R=$NIX_BUILD_TOP/results-cache; mkdir -p "$R/rime-results"
      printf 'rimed\t%s\t111\tabc\t2026-11-04T00:00:00Z\n' "$OUT" > "$R/rime-results/rimed.tsv"
      touch "$R/0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"
      printf 'shell\t%s\t111\tabc\t2026-11-04T00:00:00Z\n' "$OUT2" > "$R/rime-results/shell.tsv"   # manifest, but no narinfo
      pl() { expect 0 "plan $1" -- env PATH="$S:$PATH" RIME_CHECK_DRVS="$D" RIME_RESULTS="$R" RIME_CI_FRESH="''${2:-0}" bash nix/ci/run-checks.sh plan "$NIX_BUILD_TOP/$1"; }
      pl rp
      has "REUSED rimed: passed earlier with the same inputs (run 111); not built again"
      has "WILL RUN shell"
      grep -qx rimed "$NIX_BUILD_TOP/rp/run.txt" && bad "rimed is still in the matrix" || ok "rimed is not in the matrix"
      grep -F '"rimed"' "$NIX_BUILD_TOP/rp/run.json" > /dev/null && bad "rimed in run.json" || ok "rimed not in run.json"
      grep -qx shell "$NIX_BUILD_TOP/rp/run.txt" && ok "a manifest without the output in the cache is not a prior pass" || bad "shell reused on a manifest alone"
      printf 'rimed\t/nix/store/0ccccccccccccccccccccccccccccccc-other\t111\n' > "$R/rime-results/rimed.tsv"
      pl rp-moved
      has "WILL RUN rimed"
      printf 'rimed\t%s\t111\tabc\t2026-11-04T00:00:00Z\n' "$OUT" > "$R/rime-results/rimed.tsv"
      echo "== summary: REUSED is its own outcome, never a pass"
      mkdir -p "$NIX_BUILD_TOP/rr"
      while read -r c; do printf 'pass\t1\t\n' > "$NIX_BUILD_TOP/rr/$c.result"; done < "$NIX_BUILD_TOP/rp/run.txt"
      expect 0 "a run with a reused check" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/rp" "$NIX_BUILD_TOP/rr"
      has '| `rimed` | REUSED | PASSED EARLIER, same inputs, run 111 |'
      has "1 REUSED (passed earlier on the same inputs)"
      if grep -F '| `rimed` | RAN' "$NIX_BUILD_TOP/last.out" > /dev/null; then bad "the reused check reads as RAN"; else ok "the reused check does not read as RAN"; fi
      echo "== mutant: the summary prints a reused check as a pass"
      sed 's/^            pass) n_ran=/            pass|reused) n_ran=/' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-m-reused.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-m-reused.sh"; then bad "mutant: pattern not found"; fi
      set +e; bash "$NIX_BUILD_TOP/rc-m-reused.sh" summary "$NIX_BUILD_TOP/rp" "$NIX_BUILD_TOP/rr" > "$NIX_BUILD_TOP/last.out" 2>&1; set -e
      if grep -F '| `rimed` | REUSED | PASSED EARLIER' "$NIX_BUILD_TOP/last.out" > /dev/null; then bad "mutant not caught"; else ok "killed: the mutant printed rimed as $(grep -F '| `rimed` |' "$NIX_BUILD_TOP/last.out" | cut -d'|' -f3,4)"; fi
      echo "== fresh run: nothing reused, and a fresh failure on inputs that passed before is a flake signal"
      pl rpf 1
      has "WILL RUN rimed (fresh run: it passed earlier with the same inputs, run 111)"
      has "FRESH run: nothing reused"
      mkdir -p "$NIX_BUILD_TOP/rf"; cp "$NIX_BUILD_TOP"/rr/*.result "$NIX_BUILD_TOP/rf/"; printf 'fail\t30\t\n' > "$NIX_BUILD_TOP/rf/rimed.result"
      expect nonzero "the fresh failure fails the run" -- bash nix/ci/run-checks.sh summary "$NIX_BUILD_TOP/rpf" "$NIX_BUILD_TOP/rf"
      has '| `rimed` | RAN | **FAIL** (30 s) | **flake signal**: the same inputs (same output path) PASSED in run 111 |'
      echo "== fresh: asked for, never guessed"
      fr() { RIME_REF=$1 RIME_COMMIT_MSG=$2 RIME_FRESH_INPUT=''${3:-false} bash nix/ci/run-checks.sh fresh; }
      [ "$(fr nix/fresh-x 'fix: y')" = "1 branch nix/fresh-x" ] && ok "a nix/fresh-* branch" || bad "branch: $(fr nix/fresh-x 'fix: y')"
      [ "$(fr nix/ci-accel "$(printf 'fix: y\n\nRime-CI: fresh\n')")" = "1 commit trailer Rime-CI: fresh" ] && ok "the trailer" || bad "trailer not seen"
      [ "$(fr nix/ci-accel "$(printf 'fix: y\n\nWe could add Rime-CI: fresh one day, said in prose.\n')")" = 0 ] && ok "prose naming the trailer is not one" || bad "prose read as a trailer"
      [ "$(fr feat/nixos 'fix: y' true)" = "1 workflow_dispatch input fresh" ] && ok "the dispatch input" || bad "input not seen"
      [ "$(fr feat/nixos 'fix: y')" = 0 ] && ok "an ordinary push reuses" || bad "an ordinary push is fresh"

      echo "== export-result keeps a PASS only; results-merge saves only passes and drops a pass that failed now"
      X=$NIX_BUILD_TOP/xres; mkdir -p "$X"
      printf 'pass\t5\t\n' > "$X/rimed.result"; echo "$OUT" > "$X/rimed.out"
      printf 'fail\t5\t\n' > "$X/shell.result"; echo "$OUT2" > "$X/shell.out"
      expect 0 "a pass is kept" -- env PATH="$S:$PATH" RIME_RUN=222 bash nix/ci/run-checks.sh export-result rimed "$X" "$X/pass"
      [ -f "$X/pass/0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ] && ok "its narinfo" || bad "no narinfo"
      [ "$(cut -f1,2,3 "$X/pass/rime-results/rimed.tsv")" = "$(printf 'rimed\t%s\t222' "$OUT")" ] && ok "its manifest names run 222" || bad "manifest: $(cat "$X/pass/rime-results/rimed.tsv" 2>&1)"
      grep -E '^NarHash: sha256:[0-9a-z]{52}$' "$X/pass/0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" > /dev/null && ok "nix32 NarHash" || bad "NarHash"
      expect nonzero "a failed check is refused" -- env PATH="$S:$PATH" bash nix/ci/run-checks.sh export-result shell "$X" "$X/pass"
      has "shell did not pass (result: fail); not kept"
      [ -f "$X/pass/rime-results/shell.tsv" ] && bad "a failed check was kept" || ok "nothing kept for it"
      echo "== mutant: export-result without its pass gate"
      sed 's/^    if \[ "\$st" != pass \]; then$/    if false; then/' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-m-export.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-m-export.sh"; then bad "mutant: pattern not found"; fi
      set +e; env PATH="$S:$PATH" RIME_CI_DIR="$PWD/nix/ci" bash "$NIX_BUILD_TOP/rc-m-export.sh" export-result shell "$X" "$NIX_BUILD_TOP/xm" > /dev/null 2>&1; set -e
      [ -f "$NIX_BUILD_TOP/xm/rime-results/shell.tsv" ] && ok "killed: the mutant kept a failed check" || bad "mutant not caught"
      # results-merge: a forged pass for shell (failed in this run) arrives with rimed's real one
      cp -r "$NIX_BUILD_TOP/xm/rime-results/shell.tsv" "$X/pass/rime-results/"; cp "$NIX_BUILD_TOP"/xm/*.narinfo "$X/pass/"; cp -r "$NIX_BUILD_TOP/xm/nar/." "$X/pass/nar/"
      C=$NIX_BUILD_TOP/merged; cp -r "$R" "$C"
      printf 'libexec\t/nix/store/0ddddddddddddddddddddddddddddddd-x\t100\n' > "$C/rime-results/libexec.tsv"
      printf 'libexec\t/nix/store/0ddddddddddddddddddddddddddddddd-x\t100\n' > "$NIX_BUILD_TOP/rp/prior.tsv"
      printf 'fail\t9\t\n' > "$X/libexec.result"
      expect 0 "merge" -- bash nix/ci/run-checks.sh results-merge "$NIX_BUILD_TOP/rp" "$X" "$X/pass" "$C"
      has "REFUSED shell: its result in this run is 'fail', not pass; not saved"
      has "DROPPED libexec: passed in run 100, fail now on the same inputs"
      has "results-merge: 1 saved, 1 refused, 1 dropped"
      [ "$(cut -f3 "$C/rime-results/rimed.tsv")" = 222 ] && ok "rimed saved as run 222" || bad "rimed not saved"
      [ -f "$C/rime-results/libexec.tsv" ] && bad "libexec still kept" || ok "libexec dropped"
      [ "$(cut -f3 "$C/rime-results/shell.tsv")" = local ] && bad "the forged shell pass was saved" || ok "the forged shell pass was not saved"
      echo "== mutant: results-merge saves without checking this run's result"
      sed 's/^        if \[ "\$mc" != "\$c" \] || \[ "\$st" != pass \]; then$/        if [ "$mc" != "$c" ]; then/' nix/ci/run-checks.sh > "$NIX_BUILD_TOP/rc-m-merge.sh"
      if cmp -s nix/ci/run-checks.sh "$NIX_BUILD_TOP/rc-m-merge.sh"; then bad "mutant: pattern not found"; fi
      C2=$NIX_BUILD_TOP/merged2; cp -r "$R" "$C2"
      bash "$NIX_BUILD_TOP/rc-m-merge.sh" results-merge "$NIX_BUILD_TOP/rp" "$X" "$X/pass" "$C2" > /dev/null 2>&1 || true
      [ "$(cut -f3 "$C2/rime-results/shell.tsv")" = local ] && ok "killed: the mutant saved shell, which failed" || bad "mutant not caught"

      echo "== packages job: prebuild-plan / prebuild / prebuild-report / dev-cache-export"
      P=$NIX_BUILD_TOP/pre; DC=$NIX_BUILD_TOP/devc
      expect 0 "prebuild-plan" -- env PATH="$S:$PATH" bash nix/ci/run-checks.sh prebuild-plan "$P"
      has "ITEM rimed /nix/store/"
      grep -E '^rime-dev-pkgs-v1-[0-9a-f]{32}$' "$P/key" > /dev/null && ok "key $(cat "$P/key")" || bad "key: $(cat "$P/key")"
      [ "$(grep -cvE '^\s*(#|$)' nix/ci/prebuild.txt)" = "$(wc -l < "$P/items.tsv")" ] && ok "one item per prebuild.txt line" || bad "item count"
      echo "dup  packages.x86_64-linux.rimed" >> nix/ci/prebuild.txt; echo "dup  packages.x86_64-linux.shell" >> nix/ci/prebuild.txt
      expect nonzero "a name listed twice" -- env PATH="$S:$PATH" bash nix/ci/run-checks.sh prebuild-plan "$NIX_BUILD_TOP/pre-dup"
      has "'dup' listed twice"
      fresh
      rimedout=$(awk -F'\t' '$1 == "rimed" { print $3 }' "$P/items.tsv")
      mkdir -p "$rimedout/share/doc/rimed"; echo "TOTAL 9 9 0 0 0 0" > "$rimedout/share/doc/rimed/cargo-tests.txt"
      expect 0 "cold: every item BUILT" -- env PATH="$S:$PATH" RIME_DEV_CACHE="$DC" STUB_ULTIMATE=true bash nix/ci/run-checks.sh prebuild "$P"
      has "BUILT  rimed: in this job"
      expect 0 "report" -- bash nix/ci/run-checks.sh prebuild-report "$P"
      has "(ran in this job)"
      has "TOTAL 9 9 0 0 0 0"
      expect 0 "export" -- env PATH="$S:$PATH" RIME_RUN=4242 bash nix/ci/run-checks.sh dev-cache-export "$P" "$DC"
      [ "$(find "$DC" -name '*.narinfo' | wc -l)" = "$(cut -f4 "$P/status.tsv" | tr ',' '\n' | wc -l)" ] && ok "a narinfo per output" || bad "narinfos: $(find "$DC" -name '*.narinfo' | wc -l)"
      grep -F "	4242" "$DC/rime-meta/rimed.tsv" > /dev/null && ok "meta: rimed built in run 4242" || bad "meta"
      expect 0 "warm: every item REUSED" -- env PATH="$S:$PATH" RIME_DEV_CACHE="$DC" STUB_ULTIMATE=false RIME_PREBUILD_ALL=0 bash nix/ci/run-checks.sh prebuild "$P"
      has "REUSED rimed: built and tested by run 4242 on identical inputs"
      expect 0 "report" -- env RIME_DEV_CACHE="$DC" bash nix/ci/run-checks.sh prebuild-report "$P"
      has "REUSED, not run in this job"
      has "passed in run 4242 on identical inputs"
      if grep -F "(ran in this job)" "$NIX_BUILD_TOP/last.out" > /dev/null; then bad "a reused report reads as run here"; else ok "not labelled as run here"; fi
      expect 0 "in the dev cache but built locally (dev cache not trusted): said" -- env PATH="$S:$PATH" RIME_DEV_CACHE="$DC" STUB_ULTIMATE=true RIME_PREBUILD_ALL=0 bash nix/ci/run-checks.sh prebuild "$P"
      has "rimed was in the dev cache but was BUILT here"
      has "BUILT  rimed: in this job"

      echo "== metrics: where a leg's time went"
      cat > "$NIX_BUILD_TOP/m.tlog" <<'EOF'
      100.0 these 3 derivations will be built:
      101.0 copying path '/nix/store/0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-rimed-0.1.0' from 'file:///tmp/devc?trusted=1'...
      101.5 copying path '/nix/store/0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-glibc-2.40' from 'https://cache.nixos.org'...
      102.0 building '/nix/store/0ccccccccccccccccccccccccccccccc-vm-test-run-gaming.drv'...
      150.0 vm-test-run-gaming> (finished: run the VM test script, in 40.50 seconds)
      150.1 vm-test-run-gaming> test script finished in 40.50s
      EOF
      sed -i 's/^ *//' "$NIX_BUILD_TOP/m.tlog"
      RIME_BUILD_STARTED=90 python3 nix/ci/cihelp.py metrics gaming "$NIX_BUILD_TOP/m.tlog" pass 61 exact > "$NIX_BUILD_TOP/m.json"
      if python3 - "$NIX_BUILD_TOP/m.json" <<'EOF'
      import json, sys
      m = json.load(open(sys.argv[1]))
      want = {"eval_s": 10.0, "substituted": 2, "local_builds": 1, "vm_test_s": 40.5, "rimed_built": False, "reused": False, "dev_cache": "exact"}
      bad = {k: (m.get(k), v) for k, v in want.items() if m.get(k) != v}
      assert m["substituted_from"].get("dev-cache") == 1, m["substituted_from"]
      assert not bad, bad
      print("metrics ok")
      EOF
      then ok "metrics: eval 10 s, 2 substituted (1 from the dev cache), 1 local build, VM 40.5 s"; else bad "metrics"; fi
      [ -s "$NIX_BUILD_TOP/rb/dl.metrics.json" ] && ok "build writes CHECK.metrics.json" || bad "no dl.metrics.json"
      mkdir -p "$NIX_BUILD_TOP/mt"; cp "$NIX_BUILD_TOP/m.json" "$NIX_BUILD_TOP/mt/gaming.metrics.json"; cp "$NIX_BUILD_TOP/rp/reused/rimed.metrics.json" "$NIX_BUILD_TOP/mt/"
      expect 0 "metrics-table" -- python3 nix/ci/cihelp.py metrics-table "$NIX_BUILD_TOP/mt"
      has '| `rimed` | REUSED | 0 |'
      has "Legs with metrics: 1 built, 1 reused."

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
