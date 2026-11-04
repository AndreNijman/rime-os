# nix/ci: what `.github/workflows/nix.yml` runs

`run-checks.sh` is every command the workflow runs, so a local run is the same command. In the `rime-nix`
container (the host has no Nix):

```
cd /work/<worktree>
nix/ci/run-checks.sh plan /work/scratch/plan        # prints WILL RUN / SKIPPED <check>: <reason>
nix/ci/run-checks.sh build rimed /work/scratch/res  # one check, as one matrix leg runs it
nix/ci/run-checks.sh summary /work/scratch/plan /work/scratch/res
nix/ci/eval-each.sh path:.                        # evaluate every output, one process each (what the plan job runs)
RIME_FLAKE_CHECKS_FILE=/work/scratch/plan/checks.txt tests/check-suites-run-in-ci.sh
```

(The container has no awk; run these under `nix shell nixpkgs#gawk nixpkgs#gnugrep nixpkgs#gnused nixpkgs#coreutils`.
A GitHub runner has them.)

- **Which checks run.** Every flake check except the ones in `checks-not-in-ci.txt`, which carries one line per
  skipped check with its reason. A new check runs in CI the day it lands. A skip entry naming a check that does not
  exist, or with no reason, fails both `plan` and `tests/check-suites-run-in-ci.sh`.
- **What is skipped today.** The five checks that build the Rime kernel (`vm-boot`, `kernel-gate`, `kernel-vm`,
  `kernel-boot-budget`, `kernel-sign`): there is no Rime binary cache yet (TODO(handover) R2), and a kernel build on a 4-vCPU hosted
  runner does not fit the hour.
- **The summary.** The last job lists every flake check as RAN (pass/fail, seconds), REUSED (passed earlier on the
  same inputs, with the run) or SKIPPED (reason) in the run summary, and fails unless every planned check passed or
  was reused and the plan and packages jobs succeeded. Below it, "where each leg's time went" (the metrics table).
- **The checks of this machinery** are `ci` and `ci-userns` (`nix/tests/ci.nix`): the plan, the summary,
  check-suites and the rimed test report against mutants that must be refused, actionlint, ci/runner/check-workflows.py,
  the dev cache and check reuse (below) with a stub nix, `check-cache-isolation.py`, and bubblewrap's user namespace
  inside the build sandbox.
- **Not CI: the development VM.** `nix/dev/dev-vm` (`nix run path:.#dev-vm`) is a persistent VM to try changes in
  (`docs/nixos/dev-vm.md`). It gates nothing; CI runs only `checks.dev-vm`, which tests the script without a VM.

## Dev cache and reuse: build once, reuse everywhere

Every leg used to start from an empty store and compile what cache.nixos.org does not serve: rimed (with its cargo
test, ~25 min) in 45 legs, the desktop's overridden xwayland / speech-dispatcher / plymouth / NetworkManager-openconnect
in 22, COSMIC's patched initial setup (~35 min) in 2. Measured: `artifacts/ci-accel/` (baseline logs, dry-runs of
all 83 checks).

- **Dev cache.** `prebuild.txt` lists those derivations (`NAME ATTRIBUTE [PNAME]`; the attribute must give the SAME
  drvPath the checks build: overrides are reached through a check's own node). The packages job runs
  `prebuild-plan` (key `rime-dev-pkgs-v1-<hash of every item's drvPath>`), restores the Actions cache under that key
  (or the newest older one), `prebuild` (an item the cache holds is REUSED: substituted, its cargo test not run
  again, and the report says which run built and tested it; the others BUILT), `prebuild-report`, and on a miss
  `dev-cache-export`: the items' runtime closures, ONLY the paths no cache.nixos.org key signed, as a zstd
  `file://` binary cache (`cihelp.py export`: narinfos may name references the cache does not hold; Nix fetches those
  from cache.nixos.org). Measured: a few hundred MB. Every leg restores it (exact key, else the newest) and builds
  with `--option extra-substituters file://…?trusted=1`: unsigned, so `trusted=1`, which Nix accepts only from a
  trusted user (install-nix-action makes `runner` one; the packages job prints `trusted-users`). Nix still checks
  every NAR against its narinfo hash. There is no global `require-sigs = false`.
- **Results cache (check reuse).** A leg whose check passed runs `export-result` (refuses anything but `pass`): the
  output path's narinfo + NAR and a manifest line `check outPath run sha date`. The summary job's `results-merge`
  adds only checks whose result in this run is `pass` (and whose output is the one the plan evaluated), drops a prior
  pass whose check failed now on the same inputs, and saves `rime-check-results-v1-<ISO week>-<run>-<attempt>`. The
  plan job restores the newest of this week's, has `eval-each.sh` write every drvPath (`RIME_EVAL_OUT`), and `plan`
  (`RIME_CHECK_DRVS`, `RIME_RESULTS`) takes a check out of the matrix when the manifest names the output path it just
  evaluated AND `nix path-info --store file://<results cache> <outPath>` finds it: same output path = same derivation
  = same inputs. Never decided from a log or a time. The summary row is `REUSED | PASSED EARLIER, same inputs, run N`,
  never `pass`. A new ISO week starts empty (every check runs at least once a week).
- **Fresh run (reuse nothing).** A branch named `nix/fresh-*`, the trailer `Rime-CI: fresh` in the head commit's
  message (a trailer, not prose), or Run workflow with `fresh` ticked. The plan still records prior passes, so a
  fresh FAILURE of a check that passed on the same inputs says `flake signal: ... PASSED in run N`. The packages job
  then skips the dev cache restore too (rimed's cargo test runs again).
- **Scope.** A push restores caches of its own branch and of the default branch (main, which has none of these); a
  `pull_request` run also restores its base branch's (feat/nixos). So the first run of a new `nix/*` branch is cold
  and a PR to feat/nixos starts from feat/nixos's caches. A PR's saves stay in its merge ref (`refs/pull/N/merge`):
  a fork cannot write a cache another run reads, and the workflow keeps `contents: read`, no secrets, no
  `actions: read` (nothing queries the caches API).
- **Budget.** The repository's Actions cache is capped at 10 GB (3.9 GB used by cargo caches before this). One dev
  cache per prebuild key (a rimed or nixpkgs change makes a new one; older ones age out after 7 days unused) and one
  small results cache per run.
- **Never in a release.** `check-cache-isolation.py` (run by `checks.ci`, with mutants) fails if nix-release.yml,
  nix-kernel-sign.yml or build-image.yml names either cache, `trusted=1`, unsigned-path options, or caches a Nix path
  in the Actions cache (build-image's cargo cache is fine).
- **Metrics.** `run-checks.sh build` time-stamps its log and writes `CHECK.metrics.json` (`cihelp.py metrics`):
  eval seconds, paths substituted (and how many from the dev cache), local builds with the three longest, whether it
  compiled rimed, the VM test's own time, the dev cache state (exact / fallback / none). The summary job tables them
  (`cihelp.py metrics-table`) and uploads them as the `metrics` artifact.
- **Timings across runs.** `ci-timings.py` (stdlib + `gh api`): `workflows` (per workflow median / p90 wall, success,
  runner-minutes, queue times, per-job and per-step totals) and `run ID` (one run, with its metrics artifact).

## integrate/: what an integrator runs locally

`nix/ci/integrate/` holds the helpers every integration used to copy into `artifacts/integrate-wave<N>/`. Each one
works on the checkout it lives in (RIME_TREE overrides):

- `drive.sh TAG P check...` (in the container): evaluates each check's drvPath, then builds them P at a time; logs
  and `summary.txt` in `/work/artifacts/<checkout name>/TAG/` (RIME_ART), builds in RIME_BUILD_DIR. A check whose
  output was already valid in the store before its build is `REUSED` (it passed earlier on the same inputs; nothing
  ran), not `PASS`.
- `outs.sh` (in the container): every check's outPath and whether it is BUILT, MISSING or EVALFAIL.
- `kat-run.sh` (on the host): `hostlayer-katana` on katana's real sanitised snapshot; the snapshot's digest is printed
  before and after, and nothing from it is committed.
