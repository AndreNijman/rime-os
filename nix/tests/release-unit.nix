# The release tool without a network or a VM: unit tests, the workflow's security shape, actionlint, shellcheck,
# the release plan for the real variants, and mutants that prove each of those can fail.
{ pkgs, self }:

let
  release = import ../release { inherit pkgs self; };
  py = pkgs.python3.withPackages (ps: [ ps.jsonschema ps.pyyaml ]);
  src = ../release;
  workflows = ../../.github/workflows;
in
pkgs.runCommand "rime-release-unit"
{
  nativeBuildInputs = [ py pkgs.openssl pkgs.actionlint pkgs.shellcheck pkgs.gnugrep pkgs.zstd ];
  plan = release.rime-release-plan;
  nextReleaseId = "${release.rime-nix-release}/libexec/rime-nix-release/next-release-id";
} ''
  set -euo pipefail
  cp -r ${src} rel
  chmod -R u+w rel
  export RIME_NEXT_RELEASE_ID=$nextReleaseId HOME=$TMPDIR

  echo "== unit tests"
  (cd rel && python3 -m unittest -v test_rime_nix_release 2>&1 | tail -n 60)
  (cd rel && python3 -m unittest test_rime_nix_release >/dev/null 2>&1)
  # skipped tests would be a silent pass: there must be none here (jsonschema, openssl and the script are present)
  (cd rel && python3 -m unittest -v test_rime_nix_release 2>&1 | grep -c ' skipped' >/dev/null) \
    && { echo "FAIL: a unit test was skipped"; exit 1; }

  echo "== mutants of the tool: the suite must fail against each"
  mutant() {
    local name="$1" old="$2" new="$3"
    grep -F -- "$old" rel/rime_nix_release.py >/dev/null || { echo "FAIL: mutant $name: pattern not found"; exit 1; }
    OLD="$old" NEW="$new" python3 -c '
import os
s = open("rel/rime_nix_release.py").read()
open("mutant.py", "w").write(s.replace(os.environ["OLD"], os.environ["NEW"], 1))'
    if (cd rel && RIME_NIX_RELEASE_MODULE=$PWD/../mutant.py RIME_NIX_RELEASE_SCHEMAS=$PWD \
          python3 -m unittest test_rime_nix_release >/dev/null 2>&1); then
      echo "FAIL: mutant '$name' survived"; exit 1
    fi
    echo "killed: $name"
  }
  mutant "reference indices unchecked" \
    'if not isinstance(r, list) or not all(ok_index(x) for x in r):' 'if not isinstance(r, list):'
  mutant "boot path outside the closure accepted" 'elif boot[k] not in clo:' 'elif False:'
  mutant "any signer accepted" 'if sans[0] not in signers:' 'if False:'
  mutant "any OIDC issuer accepted" 'if got != issuer:' 'if False:'
  mutant "a channel may move backwards" 'if parse_release(new_rel) < parse_release(old_rel):' 'if False:'
  mutant "the signed digest is not compared" \
    'if (crit.get("image") or {}).get("docker-manifest-digest") != digest:' 'if False:'
  mutant "nix32 bit order" 'out[i] |= (d << j) & 0xFF' 'out[i] |= (d << (7 - j)) & 0xFF'
  mutant "a path the cache lacks fails the whole query" \
    'if recursive or not absent or not absent <= set(chunk):' 'if True:'
  mutant "SigV4 signing key without its terminator" \
    'for part in (date, region, service, "aws4_request"):' 'for part in (date, region, service):'
  # the GitHub backend
  mutant "an uploaded asset is sent again" 'if x.get("state") == "uploaded" and x.get("size") == size:' 'if False:'
  mutant "a broken asset is kept" 'if x.get("state") == "uploaded" and x.get("size") == size:' 'if True:'
  mutant "a full release takes more assets" 'if len(p[2]) < self.max_assets), None)' 'if True), None)'
  mutant "the narinfo URL is not rewritten" 'line, n = "URL: " + url, n + 1' 'n = n + 1'
  mutant "a narinfo with another NarHash binds" \
    'if sri(ni.get("NarHash", "")) != sri(want["nar_hash"]):' 'if False:'
  mutant "a NAR of 2 GiB is uploaded" 'if os.path.getsize(path) > GH_MAX_ASSET:' 'if False:'
  mutant "upstream paths marked as Rime's" \
    'entry["origin"] = "upstream" if p in upstream["paths"] else "rime"' 'entry["origin"] = "rime"'
  mutant "a path that may not be redistributed is published (R14)" \
    'leaked = sorted(banned & set(clo["paths"]))' 'leaked = []'
  mutant "a device build names an output" \
    'if not c["out"].endswith(".drv"):' 'if False:'
  mutant "publish uploads before its R14 refusal" \
    'refuse_non_redistributable(load_artifact(a.artifact)[0], plan)' 'None'
  mutant "a test variant is published" \
    'if not v.get("publishable") and not allow_test_variants:' 'if False:'
  mutant "the push hook runs in a real publish" \
    'return os.environ.get("RIME_TEST_BEFORE_PUSH") if a.test_signing else None' \
    'return os.environ.get("RIME_TEST_BEFORE_PUSH")'
  mutant "a NAR URL on the Pages origin passes" 'if u.netloc == origin:' 'if False:'
  # deltas (docs/nixos/delta.md §7)
  mutant "a delta file with another sha256 binds" \
    'if "sha256-" + base64.b64encode(digest).decode() != e["file_hash"] or nix32_raw(digest) + ".delta" != name:' \
    'if False:'
  mutant "a delta over the ratio is published" \
    'elif not sizes.get(p) or e["file_size"] > max_ratio * sizes[p]:' 'elif not sizes.get(p):'
  mutant "a delta file outside its release" \
    'if not isinstance(e.get("file"), str) or not DELTA_FILE_RE.match(e["file"]):' \
    'if not isinstance(e.get("file"), str):'
  mutant "a delta that does not rebuild the NAR is kept" \
    'if d.wait() != 0 or "sha256-" + base64.b64encode(h.digest()).decode() != sri(new_hash):' 'if d.wait() != 0:'

  echo "== the workflow's security shape"
  cp -r ${workflows} wf
  chmod -R u+w wf
  python3 rel/check_workflow.py wf/nix-release.yml
  wmutant() {
    local name="$1" old="$2" new="$3"
    OLD="$old" NEW="$new" python3 -c '
import os, sys
s = open("wf/nix-release.yml").read()
if os.environ["OLD"] not in s:
    sys.exit("pattern not found")
open("mut.yml", "w").write(s.replace(os.environ["OLD"], os.environ["NEW"], 1))' \
      || { echo "FAIL: workflow mutant $name: pattern not found"; exit 1; }
    cp mut.yml wf-mut-nix-release.yml
    if python3 rel/check_workflow.py wf-mut-nix-release.yml >/dev/null; then
      echo "FAIL: workflow mutant '$name' passed the check"; exit 1
    fi
    echo "killed: $name"
  }
  wmutant "action pinned by tag" "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683" "actions/checkout@v4"
  wmutant "a release without its catalogue apps" \
    'for c in "$t"/catalogue/*; do args+=(--catalogue-out "$(readlink -f "$c")"); done' ""
  wmutant "the catalogue never built" '-L --out-link "$RUNNER_TEMP/catalogue" .#rime-release-catalogue' '-L .#rime-release-plan'
  wmutant "no release-plan check" ".#checks.x86_64-linux.release-plan" ".#checks.x86_64-linux.release-unit"
  wmutant "rimed built under the user-namespace restriction" \
    "run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" "run: echo the restriction stays"
  wmutant "the user-namespace sysctl allowed to fail" \
    "run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" "run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 || true"
  wmutant "the user-namespace sysctl behind an if:" \
    "      - name: Allow user namespaces (bubblewrap in rimed's tests)
        run:" "      - name: Allow user namespaces (bubblewrap in rimed's tests)
        if: runner.debug == '1'
        run:"
  wmutant "publish on a self-hosted runner" \
    "    environment: nix-release
    permissions:" "    runs-on: [self-hosted, katana]
    environment: nix-release
    permissions:"
  wmutant "signer outside the environment" "    environment: nix-release
" ""
  wmutant "the kernel signer called off main" \
    "if: github.ref == 'refs/heads/main' && needs.changes.outputs.action == 'release' && vars.RIME_NIX_KERNEL_HOST != '''" \
    "if: needs.changes.outputs.action == 'release' && vars.RIME_NIX_KERNEL_HOST != '''"
  # The patterns end in the job's runs-on/environment lines: the kernel-sign job's if: starts with the same text.
  wmutant "publish not restricted to main" \
    "if: github.ref == 'refs/heads/main' && needs.changes.outputs.action == 'release'
    runs-on: ubuntu-24.04
    environment: nix-release" \
    "if: needs.changes.outputs.action == 'release'
    runs-on: ubuntu-24.04
    environment: nix-release"
  # "Restricted to main" is the exact term of a conjunction (check_workflow.main_only), not a substring: each of these
  # still holds the text github.ref == 'refs/heads/main', and each passed the substring rule.
  wmutant "the kernel signer called off main through an or" \
    "vars.RIME_NIX_KERNEL_HOST != '''" "vars.RIME_NIX_KERNEL_HOST != ''' || true"
  wmutant "publish off main through an or" \
    "needs.changes.outputs.action == 'release'
    runs-on: ubuntu-24.04
    environment: nix-release" \
    "needs.changes.outputs.action == 'release' || github.event_name == 'push'
    runs-on: ubuntu-24.04
    environment: nix-release"
  wmutant "the refresh signer off main through an or" \
    "github.ref == 'refs/heads/main'
      && (github.event_name" \
    "github.ref == 'refs/heads/main' || true
      && (github.event_name"
  # The same restriction written without spaces or with the operands swapped is still main-only: refusing it was a
  # false refusal (wave 4 integration). Each rewrite must PASS; the exact-text checker before the fix refused both.
  waccept() {
    local name="$1" old="$2" new="$3"
    OLD="$old" NEW="$new" python3 -c '
import os, sys
s = open("wf/nix-release.yml").read()
if os.environ["OLD"] not in s:
    sys.exit("pattern not found")
open("acc.yml", "w").write(s.replace(os.environ["OLD"], os.environ["NEW"]))' \
      || { echo "FAIL: workflow rewrite $name: pattern not found"; exit 1; }
    cp acc.yml wf-acc-nix-release.yml
    python3 rel/check_workflow.py wf-acc-nix-release.yml >/dev/null \
      || { echo "FAIL: workflow rewrite '$name' (still main-only) was refused"; exit 1; }
    echo "accepted: $name"
  }
  waccept "main written without spaces" "if: github.ref == 'refs/heads/main' &&" "if: github.ref=='refs/heads/main' &&"
  waccept "main with the operands swapped" "if: github.ref == 'refs/heads/main' &&" "if: github.event_name != 'pull_request' && 'refs/heads/main' == github.ref &&"
  wmutant "an input expanded into a script" '          case "$CHANNEL" in' '          echo "''${{ inputs.reason }}"
          case "$CHANNEL" in'
  # The concurrency expressions hold single quotes and GitHub expressions, so they are built in single quotes.
  cancel='cancel-in-progress: ''${{ github.ref != '"'"'refs/heads/main'"'"' }}'
  wmutant "a run cancelled by the next" "$cancel" "cancel-in-progress: true"
  wmutant "a release on main cancelled by the next push" "$cancel" \
    'cancel-in-progress: ''${{ github.ref != '"'"'refs/heads/feat/nixos'"'"' }}'
  wmutant "main in a group of its own per ref" \
    'group: ''${{ github.ref == '"'"'refs/heads/main'"'"' && '"'"'nix-release'"'"' || format('"'"'nix-release-{0}'"'"', github.ref) }}' \
    'group: ''${{ format('"'"'nix-release-{0}'"'"', github.ref) }}'
  wmutant "a test variant published" '            --out "$t/out"' '            --allow-test-variants --out "$t/out"'
  wmutant "the stand-in's push hook in a workflow" '          RIME_CACHE_TOKEN: ''${{ secrets.RIME_CACHE_TOKEN }}' \
    '          RIME_CACHE_TOKEN: ''${{ secrets.RIME_CACHE_TOKEN }}
          RIME_TEST_BEFORE_PUSH: "true"'
  # the build job keeps its fork-PR guard here, so only the token can trip the check
  wmutant "the cache token on a self-hosted build job" '    runs-on: ubuntu-24.04
    if: >-
      always() && needs.changes.result' '    runs-on: [self-hosted, katana]
    env:
      LEAK: ''${{ secrets.RIME_CACHE_TOKEN }}
    if: >-
      always() && needs.changes.result'

  # the bridge payload job (RIME_BRIDGE_PAYLOAD_KEY): main only, in the environment, signed before the push against the
  # trusted key line, and the pulled-back copy checked
  wmutant "the payload job off main" \
    "if: github.ref == 'refs/heads/main' && needs.changes.outputs.action == 'release' && needs.publish.outputs.state == 'published'" \
    "if: needs.changes.outputs.action == 'release' && needs.publish.outputs.state == 'published'"
  wmutant "the payload key outside the environment" \
    "    environment: nix-release
    permissions:
      contents: read
      packages: write
    timeout-minutes: 180" \
    "    permissions:
      contents: read
      packages: write
    timeout-minutes: 180"
  wmutant "the payload pushed unsigned" \
    '"$RUNNER_TEMP/tools/bin/rime-bridge-payload-release" files/system/trust/nixos-bridge-payload.pub "$RUNNER_TEMP/payload"' 'true'
  wmutant "the payload signed against another key line" \
    'rime-bridge-payload-release" files/system/trust/nixos-bridge-payload.pub "$RUNNER_TEMP/payload"' \
    'rime-bridge-payload-release" "$RUNNER_TEMP/any.pub" "$RUNNER_TEMP/payload"'
  wmutant "the pulled-back payload not checked" \
    '"$t/tools/bin/rime-bridge-payload-release" --verify files/system/trust/nixos-bridge-payload.pub "$t/back"' 'true'
  echo "== off main, the build job is a dry-run proof (the step's own script, nix and git stubbed)"
  python3 - wf/nix-release.yml > dry.sh <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf["jobs"]
steps = jobs["build"]["steps"]
dry = [s for s in steps if s.get("if") == "needs.changes.outputs.build == 'dry-run'"]
assert len(dry) == 1, "one dry-run step"
# every step that builds, stages, uploads or frees disk for that runs only on main (build=full)
for s in steps:
    text = s.get("run", "") + s.get("uses", "")
    if any(k in text for k in ("--out-link", "rime-nix-release\" closure", "upload-artifact", "/usr/share/dotnet")):
        assert s.get("if") == "needs.changes.outputs.build == 'full'", f"step {s.get('name') or s.get('uses')} is not main-only"
what = jobs["changes"]["steps"][0]["run"]
assert "build=dry-run\n" in what and '[ "$REF" = refs/heads/main ] && build=full' in what, "build=full on main only"
print(dry[0]["run"])
PY
  mkdir -p stub rt
  cat > stub/nix <<'SH'
#!${pkgs.bash}/bin/bash
echo "nix $*" >> "$LOG"
case "$1" in
  eval) if [ "$PWD" = "$RUNNER_TEMP/before" ]; then echo -n "$WAS"; else echo -n "$NOW"; fi ;;
  build) [[ " $* " == *" --dry-run "* ]] || { echo "a real build in a dry run: $*" >&2; exit 9; }; echo "these 2 derivations will be built:" ;;
esac
SH
  cat > stub/git <<'SH'
#!${pkgs.bash}/bin/bash
echo "git $*" >> "$LOG"
case "$1" in worktree) mkdir -p "$RUNNER_TEMP/before" ;; esac
SH
  # the step evaluates through nix/ci/eval-each.sh (one evaluator per attribute), relative to the checkout
  mkdir -p nix/ci
  cat > nix/ci/eval-each.sh <<'SH'
#!${pkgs.bash}/bin/bash
echo "eval-each $*" >> "$LOG"
SH
  chmod +x stub/nix stub/git nix/ci/eval-each.sh
  dryrun() { # $1 = case, $2 = BEFORE, $3 = was drv, $4 = now drv
    mkdir -p "rt/$1"
    env PATH=$PWD/stub:$PATH LOG=$PWD/dry-$1.log RUNNER_TEMP=$PWD/rt/$1 GITHUB_STEP_SUMMARY=$PWD/sum-$1.md \
      GITHUB_REPOSITORY=AndreNijman/rime-os SHELL_SHA=0123 BEFORE="$2" WAS="$3" NOW="$4" bash dry.sh > out-$1.txt
    touch dry-$1.log
    cat out-$1.txt
  }
  b=1111111111111111111111111111111111111111
  dryrun same $b /nix/store/a.drv /nix/store/a.drv
  grep -F "did not move" out-same.txt >/dev/null
  if grep -E "^(nix (build|flake)|eval-each)" dry-same.log; then echo "FAIL: toplevels unchanged, but it built or checked"; exit 1; fi
  dryrun moved $b /nix/store/a.drv /nix/store/b.drv
  grep -F -- "nix build --dry-run" dry-moved.log >/dev/null
  grep -F -- "eval-each . --override-input rime-shell" dry-moved.log >/dev/null
  grep -F "Dry run, nothing built or published" sum-moved.md >/dev/null
  dryrun new-branch 0000000000000000000000000000000000000000 /nix/store/a.drv /nix/store/a.drv
  grep -F -- "nix build --dry-run" dry-new-branch.log >/dev/null
  if grep -F "git fetch" dry-new-branch.log; then echo "FAIL: fetched the all-zero commit"; exit 1; fi
  dryrun dispatched "" /nix/store/a.drv /nix/store/a.drv
  grep -F -- "nix build --dry-run" dry-dispatched.log >/dev/null
  # mutant: no comparison, so an unchanged toplevel is dry-run anyway; the "same" case must catch it
  OLD='[ -n "$was" ] && [ "$was" = "$now" ]' NEW='false' python3 -c '
import os
s = open("dry.sh").read(); assert os.environ["OLD"] in s
open("dry.sh", "w").write(s.replace(os.environ["OLD"], os.environ["NEW"], 1))'
  dryrun mut-same $b /nix/store/a.drv /nix/store/a.drv
  if grep -F "did not move" out-mut-same.txt >/dev/null; then echo "FAIL: the mutant did not change the outcome"; exit 1; fi
  grep -F -- "nix build --dry-run" dry-mut-same.log >/dev/null
  echo "killed: a dry run although the toplevels did not move"

  echo "== actionlint"
  mkdir -p repo/.github/workflows
  cp wf/*.yml repo/.github/workflows/
  if [ -e repo/.github/workflows/nix-kernel-sign.yml ]; then
    (cd repo && actionlint -shellcheck= .github/workflows/nix-release.yml)
  else
    # nix-kernel-sign.yml is written by the kernel-signing worker (another branch). Until both are merged, the one
    # finding allowed is that its file is absent; anything else fails.
    echo "nix-kernel-sign.yml is not in this tree: ignoring only 'could not read reusable workflow file'"
    (cd repo && actionlint -shellcheck= -ignore 'could not read reusable workflow file' .github/workflows/nix-release.yml)
  fi
  (cd repo && actionlint .github/workflows/nix-release.yml 2>&1 | grep -E 'shellcheck' || true)

  echo "== shellcheck"
  shellcheck -s bash rel/standin.sh rel/crosscheck-rime-trust.sh rel/mint-test-signing.sh

  echo "== the release plan for the real variants"
  python3 - "$plan" <<'PY'
import json, re, sys
plan = json.load(open(sys.argv[1]))
# The generic installed system per feature set (nix/release/variants.nix); checks.release-plan proves the set,
# the features each carries, and that none carries a test account.
assert "plain" in plan["variants"], sorted(plan["variants"])
for name, v in plan["variants"].items():
    assert re.match(r"^/nix/store/[0-9a-z]{32}-nixos-system-rime-" + re.escape(name) + "-", v["toplevel"]), v["toplevel"]
    assert v["publishable"] is True, f"{name}: the generic installed system is publishable"
    for slug in ("shell", "rime-daemon", "rime-tools", "compositor", "shell-runtime", "systemd", "libraries", "kernel"):
        assert v["components"].get(slug), f"{name}: component {slug} is empty"
assert plan["catalogue"], "the plan names no catalogue builds"
print("plan ok:", sorted(plan["variants"]), sorted(c["name"] for c in plan["catalogue"]))
PY

  touch $out
''
