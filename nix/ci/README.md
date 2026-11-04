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
- **The summary.** The last job lists every flake check as RAN (pass/fail, seconds) or SKIPPED (reason) in the run
  summary, and fails unless every planned check passed and the plan and packages jobs succeeded.
- **The checks of this machinery** are `ci` and `ci-userns` (`nix/tests/ci.nix`): the plan, the summary,
  check-suites and the rimed test report against mutants that must be refused, actionlint, ci/runner/check-workflows.py,
  and bubblewrap's user namespace inside the build sandbox.
- **Not CI: the development VM.** `nix/dev/dev-vm` (`nix run path:.#dev-vm`) is a persistent VM to try changes in
  (`docs/nixos/dev-vm.md`). It gates nothing; CI runs only `checks.dev-vm`, which tests the script without a VM.

## integrate/: what an integrator runs locally

`nix/ci/integrate/` holds the helpers every integration used to copy into `artifacts/integrate-wave<N>/`. Each one
works on the checkout it lives in (RIME_TREE overrides):

- `drive.sh TAG P check...` (in the container): evaluates each check's drvPath, then builds them P at a time; logs
  and `summary.txt` in `/work/artifacts/<checkout name>/TAG/` (RIME_ART), builds in RIME_BUILD_DIR.
- `outs.sh` (in the container): every check's outPath and whether it is BUILT, MISSING or EVALFAIL.
- `kat-run.sh` (on the host): `hostlayer-katana` on katana's real sanitised snapshot; the snapshot's digest is printed
  before and after, and nothing from it is committed.
