# P1 exit: CI and the cargo test total

Evidence for the P1 exit items "GitHub-hosted Nix CI" and "the real `cargo test` total" (PLAN.md, PR 7). Every number
below comes from a command run on 2026-11-01 in the `rime-nix` build container on katana. The workflow itself has
**not run on GitHub**: this branch has not been pushed. Its first hosted run is the remaining proof (see "Not proven").

## CI

### What runs

`.github/workflows/nix.yml`, on push to `feat/nixos` and `nix/**`, pull requests to `feat/nixos`, and
`workflow_dispatch`. GitHub-hosted `ubuntu-24.04` only, never the self-hosted `katana` runner. `permissions: contents:
read`, no secrets, so fork pull requests are safe to run.

| job | what it does |
|---|---|
| plan | `nix flake check path:. --no-build` (evaluates every check, the skipped ones too), then `nix/ci/run-checks.sh plan`, then `tests/check-suites-run-in-ci.sh` |
| packages | builds `rimed` (its checkPhase is the whole `cargo test --workspace --no-fail-fast`), `rime-shell`, `rime-libexec`; puts the cargo report in the run summary |
| check (matrix) | one leg per flake check the plan runs, with KVM proven first |
| summary | a table of every flake check, RAN (pass/FAIL/NO RESULT, seconds) or SKIPPED (reason); fails unless every planned check passed and plan and packages succeeded |

The check list is not written in the workflow. `run-checks.sh plan` takes every check in the flake and removes the ones
in `nix/ci/checks-not-in-ci.txt`, so a check added to the flake runs in CI the day it lands. A skip entry with no reason,
a name listed twice, or a name that is not a flake check fails the plan. Every workflow step is a `run-checks.sh` call, so
a local run is the same command (`nix/ci/README.md`).

Local plan, this branch:

```
plan: 14 checks, 10 run by CI, 4 skipped
WILL RUN ci, ci-userns, desktop, eval, gaming, kernel-config-fresh, libexec, rimed, security, shell
SKIPPED vm-boot: needs the Rime binary cache (TODO(handover) R2): the system test boots the Rime kernel
SKIPPED kernel-gate: needs the Rime binary cache (TODO(handover) R2): it reads the built Rime vmlinux
SKIPPED kernel-vm: needs the Rime binary cache (TODO(handover) R2): it boots the Rime kernel and its signed modules
SKIPPED kernel-boot-budget: needs the Rime binary cache (TODO(handover) R2): it measures the Rime kernel image and initrd
```

The workflow prints each skip as `SKIPPED <check>: <reason>` in the plan job, the summary job and the run summary, so
a skip never reads as a pass.

### Why those four are skipped

`nix build --dry-run` against an empty store (what a hosted runner starts with) shows `linux-rime-7.2.9.drv` in exactly
four checks: `vm-boot`, `kernel-gate`, `kernel-vm`, `kernel-boot-budget`. A full kernel build on a 4-vCPU hosted runner
does not fit the hour, and there is no Rime binary cache to substitute it from yet (TODO(handover) R2).
`kernel-config-fresh` only fetches the CachyOS source and config (no compile), so it runs.

Store each leg fetches from cache.nixos.org (dry-run, empty store) and the derivations it builds:

| check | builds | fetches (download / unpacked) |
|---|---|---|
| eval | 0 | 21 MiB / 79 MiB |
| kernel-config-fresh | 5 | 182 MiB / 596 MiB |
| rimed | 283 | 1.4 GiB / 4.7 GiB |
| security | 473 | 1.4 GiB / 4.8 GiB |
| shell | 281 | 2.5 GiB / 6.9 GiB |
| gaming | 344 | 2.9 GiB / 8.5 GiB |
| desktop | 430 | 3.5 GiB / 10.1 GiB |
| libexec | 434 | 3.5 GiB / 10.1 GiB |
| (skipped) vm-boot | 529 | 4.3 GiB / 13.0 GiB |

Each leg runs `./.github/actions/free-disk-space` with `required-gb: 40`.

### KVM and user namespaces

- `cachix/install-nix-action` with `enable_kvm: true` installs the udev rule that opens `/dev/kvm` to the runner user;
  `extra_nix_config` adds `kvm` and `nixos-test` to `system-features`. A "Prove KVM" step then checks `/dev/kvm` is
  readable and writable and that `nix config show system-features` contains `kvm` and `nixos-test`, so a VM test never
  falls back to emulation and a missing feature fails with a named error.
- rimed's tests run under bubblewrap inside the Nix build sandbox, which needs an unprivileged user namespace. Ubuntu
  24.04 restricts those through AppArmor, so the packages and check jobs run
  `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` (a throwaway hosted VM). The packages job then builds
  `checks.x86_64-linux.ci-userns` first: one bwrap call in the build sandbox, seconds, so a refused namespace fails by
  name instead of twenty minutes into rimed's checkPhase.

### Pinned actions

| action | pin |
|---|---|
| actions/checkout | `fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09` (v5.1.0) |
| cachix/install-nix-action | `75c75c31ebece974e48ebc36b4ad125bd97bd97b` (v31.11.3, Nix 2.35.4 as in the build container) |
| actions/upload-artifact | `ea165f8d65b6e75b540449e92b4886f43607fa02` (v4.6.2) |
| actions/download-artifact | `d3f86a106a0bac45b974a628896c90dbdf5c8093` (v4.3.0) |

### No /nix cache

Not added. The GitHub Actions cache is capped at 10 GB per repository and the legs above need 5-13 GB of store each, so
hit rates would be poor and evictions constant. The answer is the Rime binary cache (TODO(handover) R2).

### Suites with no CI home

`tests/check-suites-run-in-ci.sh` now accepts a flake check as a CI home (PLAN PR 7). A suite counts as run by a flake
check when it is named on a non-comment line of a `nix/tests/*.nix` file that defines a check the plan runs (not a
skipped one). It also refuses a skip entry with no reason or naming no check, and a `nix.yml` that does not run the plan.
Without `flake.nix` (the bootc main branch) its output is byte-identical to before. Local run:

```
suite coverage: 148 suites, 140 run by CI, 8 exempt, 0 unrun and undeclared
flake checks: 10 run by .github/workflows/nix.yml, 4 skipped with a reason; 0 suite(s) run through a flake check
```

### The checks of this machinery

`nix/tests/ci.nix` adds two flake checks, both run by CI:

- `ci-userns`: bubblewrap's user namespace inside the build sandbox.
- `ci`: on the real tree, the plan, check-suites, actionlint on `nix.yml`, `ci/runner/check-workflows.py`, and that
  every `runs-on` is `ubuntu-24.04`. Then mutants that must be refused: a skip entry naming no check, a skip with no
  reason, a deleted skip line (the check must move to WILL RUN), a `nix.yml` that does not run the plan, a suite
  mentioned only in a comment, a suite mentioned only in a skipped check's file, a failed and a missing check result in
  the summary, and the cargo report fixtures below.

### Validation done

The workflow has not run on GitHub (no push from this worktree). Its commands were run as written in the `rime-nix`
container (`/var/lab/rime-nix/scratch/ci/jobs.sh`, under `nix shell` for gawk/grep/sed, which the container lacks and a
runner has):

- `nix flake check path:. --no-build`: rc 0, "all checks passed!".
- `nix/ci/run-checks.sh plan`: rc 0, the plan above; `run.json` = the 10 checks that run.
- `RIME_FLAKE_CHECKS_FILE=... tests/check-suites-run-in-ci.sh`: rc 0 (output above).
- `nix build ...#checks.x86_64-linux.ci-userns`: rc 0. `rimed`, `rime-shell`, `rime-libexec` build (all three already in
  the store for this tree; `rimed`'s build ran the checkPhase and wrote the report below).
- `run-checks.sh build` for the `ci`, `ci-userns` and `eval` legs: all `pass`. `run-checks.sh summary` with only those
  three results: rc 1, the other seven planned checks reported as **NO RESULT**, the four kernel checks as SKIPPED.
- `actionlint .github/workflows/nix.yml` (nixpkgs pin's actionlint): rc 0. Also run inside `checks.x86_64-linux.ci`.
- Every pinned action SHA resolves to its tag (GitHub API, 2026-11-01). install-nix-action v31.11.3 installs Nix 2.35.4
  and its `enable_kvm` writes the udev rule `KERNEL=="kvm", GROUP="kvm", MODE="0666"` (read from the pinned
  `install-nix.sh`).

### Not proven (first hosted run)

- That the AppArmor sysctl and the KVM udev rule work on the hosted image as written. `ci-userns` and "Prove KVM"
  exist so a failure is named in seconds.
- Wall time against the 60-minute budget. katana's load average was 60-127 from parallel agents during this work, so
  local timings say nothing about a 4-vCPU runner. The heaviest legs are `desktop` and `libexec` (about 430 local
  derivations each) and `packages` (rimed's checkPhase). A hosted run gives the real numbers.
- Every check leg starts from an empty store. The `rimed`, `gaming` and `security` checks have rimed in their build closure
  (`nix-store -qR` on the check's drv), so those legs compile the rimed workspace before their VM test (11 min on 20
  threads in the container; expect several times that on 4 vCPU), as does the packages job with its checkPhase on top.
  `desktop`, `libexec`, `shell`, `eval` and `kernel-config-fresh` do not. This is the main risk to the 60-minute budget,
  and the binary cache (R2) is what removes it.
- The `shell` and `gaming` checks compile Hyprland: their VMs use `programs.hyprland` without `withUWSM = true`, and the
  NixOS module's override then gives `/nix/store/a6gqc16mwivb1pf7xpf6sgcnjvggprx4-hyprland-0.57.0`, which is 404 on
  cache.nixos.org. `desktop` and `libexec` (whose module sets `withUWSM = true`) get
  `dmxlm63flzqqmm1wxfgmq057g2df2pmx-hyprland-0.57.0` = `pkgs.hyprland`, 200 on the cache. TODO(handover): the owners of
  `nix/tests/shell.nix` and `nix/tests/gaming.nix` set `programs.hyprland.withUWSM = true` (as the desktop module does)
  so their Hyprland substitutes.

### Handover

- TODO(handover) R2: the Rime binary cache. With it, the four kernel checks substitute the kernel and can move off the
  skip list (delete their lines in `nix/ci/checks-not-in-ci.txt`; the plan picks them up).
- TODO(handover): the per-job ephemeral runners in `ci/runner/` (label `katana-nix`) become the primary for the heavy
  legs once installed on katana, with this hosted workflow as the fallback. Any job moved there needs the fork guard
  `ci/runner/check-workflows.py` enforces.
- At the merge to main, PLAN puts this job inside `pr-validation.yml`, because the ruleset requires only "PR validation".
- Someone with push rights pushes the branch and reads the first run's summary.

## cargo total

`rimed`'s checkPhase runs `cargo test --workspace --no-fail-fast -- --show-output` under bubblewrap in the Nix build
sandbox. `postCheck` runs `nix/pkgs/rimed-test-report.py` over the log and `cargo metadata`, sums the test targets into
crates, and installs the report as `share/doc/rimed/cargo-tests.{txt,json}`. It refuses the build when the total cannot
be trusted:

- a test target with no `test result:` line, or a result with no target;
- any failed test;
- the number of tests `filtered out` differs from the number of `--skip` names (a stale or ambiguous skip entry);
- a test that cargo reports as passed but that printed a SKIP line and returned, unless `selfSkipOk` names it;
- a `selfSkipOk` name that no longer skips itself.

### The total

From `share/doc/rimed/cargo-tests.txt` of `/nix/store/8m0cym0j3255gnyi381f8bxwkc85svay-rimed-0.1.0`, the store path
`nix eval --raw path:.#rimed.outPath` names for this branch (built in the container, checkPhase and postCheck passed):

```
crate                targets  passed selfskip  failed  ignored  skipped
rime                       5     679        8       0        1        7
rime-agent-core           11     809        3       0        0        0
rime-agentd               16     272       29       0        0       27
rime-aid                   1      22        0       0        0        0
rime-backup-core           2     167        0       0        0        0
rime-paths                 4       5        0       0        0        0
rime-perm-core             2      22        0       0        0        0
rime-remote-core           3     116        0       0        2        0
rime-remoted               5      89       17       0        0        0
rime-secret-core           2     151        0       0        0        1
rime-secretd               3     390        0       0        0        0
rimed                      1      53        0       0        0        0
rimed-core                15     901        0       0        0        0
TOTAL                     70    3676       57       0        3       35
```

- **passed**: ran and passed.
- **selfskip**: cargo reports ok, but the test printed SKIP and returned early, so it proved nothing. Before this report
  existed, these 57 read as passes.
- **skipped**: excluded by `--skip` with a reason (below).
- **ignored**: `#[ignore]` in the source.

### Skipped by name (35), from `nix/pkgs/rimed.nix`

- **needsLoginSessionCgroup (26)**, rime-agentd: the daemon decides who a client is from `/proc/<pid>/cgroup` and
  refuses one in neither a login session nor a user service. Every process in a build sandbox is in "/", so the session
  is refused before the behaviour under test runs.
  an_unconfined_session_is_refused_rather_than_given_a_field_that_does_nothing,
  a_pem_carrying_a_private_key_starts_no_session_at_all, a_budget_of_zero_is_refused_before_anything_is_started,
  a_budget_refusal_is_a_policy_answer_and_not_a_bad_request, a_budget_with_no_user_manager_refuses_and_names_the_path,
  an_unparseable_config_file_does_not_pretend_there_is_no_budget, an_unbudgeted_session_runs_in_the_daemons_own_cgroup,
  a_mistyped_budget_key_refuses_rather_than_starting_the_session_unbudgeted,
  a_wedged_application_cannot_hold_the_connection, the_verb_is_reachable_and_the_selection_comes_back_verbatim,
  a_clipboard_holding_a_picture_is_refused_and_its_bytes_are_never_read,
  a_machine_with_no_compositor_says_so_rather_than_reporting_an_empty_clipboard,
  a_clipboard_past_the_cap_is_refused_with_the_limit_named, an_empty_clipboard_is_an_answer_and_never_an_error,
  a_hook_inside_a_session_reaches_the_daemon_that_started_it,
  the_daemon_hands_the_agent_a_curated_configuration_and_the_flag_that_makes_it_exclusive,
  a_session_with_no_connectors_is_handed_an_empty_set_rather_than_no_file, a_name_the_validator_refuses_starts_nothing,
  the_three_ways_a_write_can_fail_come_back_as_three_different_kinds,
  renewing_a_grant_that_does_not_exist_is_refused_without_asking_anybody,
  a_ttl_is_refused_before_anything_is_created_or_anybody_is_asked,
  a_ttl_on_a_session_that_asked_for_no_grant_is_refused_rather_than_ignored,
  a_project_with_no_binding_starts_the_agent_it_was_asked_for,
  a_session_that_names_no_agent_gets_the_bound_one_and_not_the_users_default,
  a_session_naming_an_agent_this_project_did_not_bind_is_refused,
  a_session_with_a_parent_death_signal_survives_its_client_hanging_up.
- **needsFhsHost (7)**, rime (`tests/mcp_launch_live.rs`, `tests/mcp_sidecar_live.rs`): they start real confined MCP
  servers that are host programs (`/usr/bin/python3`, probe scripts on `/usr/bin:/bin`, a mount point created under a
  read-only `/`). The confinement itself is covered by rime-agent-core's sandbox tests, which run.
  a_plugins_server_launched_through_the_curated_config_cannot_reach_the_network,
  a_plugins_server_launched_through_the_curated_config_cannot_read_the_users_home,
  a_confined_server_cannot_read_the_home_the_agent_can,
  a_confined_server_with_no_network_cannot_reach_a_listener_that_is_there,
  a_variable_the_parent_has_does_not_reach_a_confined_server,
  the_broker_dimension_opens_the_runtime_socket_and_only_when_asked,
  the_shipped_command_runs_a_stdio_mcp_server_and_its_handshake_still_completes.
- **needsPrivsUnset (1)**, rime-agentd: `pty::tests::no_new_privs_reaches_the_session_and_the_negative_control_proves_it`.
  Its negative control starts a child without no_new_privs; the build sandbox and bwrap set it on everything.
- **needsForeignOwnedFile (1)**, rime-secret-core: `project::tests::a_file_the_owner_does_not_own_is_refused` needs a
  file owned by another user at `/etc/hostname`; the sandbox has none and cannot make one.

### Self-skipped (57), named in `selfSkipOk`

- **noLoginSessionCgroup (50)**: the same cause as needsLoginSessionCgroup. These catch the refusal, print SKIP and
  return. 8 in rime (its tests start rime-agentd), 25 in rime-agentd, 17 in rime-remoted (pairing asks rime-agentd who
  the client is).
- **liveBudgetOptIn (2)**, rime-agentd: `a_budgeted_session_runs_in_its_own_scope_with_the_limits_really_written`,
  `a_wall_clock_budget_really_ends_the_session`. Opt-in by design (`RIME_BUDGET_LIVE=1`); they need a systemd user
  manager to start a scope.
- **noFirefoxPolicy (2)**, rime-agentd: `the_capsule_sees_the_merged_policy_and_can_open_the_ca_it_names`,
  `a_session_that_asked_for_no_ca_sees_the_machines_own_file`. They need `/etc/firefox/policies/policies.json`.
- **noHostTools (3)**, rime-agent-core: `sandbox::tests::a_confined_session_can_still_resolve_names` (needs
  `/usr/bin/getent`), `sandbox::tests::the_masked_run_really_does_block_the_system_bus` (needs busctl and a system bus),
  `lock::tests::the_loginctl_contract_this_parser_relies_on_is_still_true` (needs loginctl on PATH and a running
  logind; it prints `skipped: loginctl is not on PATH`, which the first version of the report did not recognise, so it
  was counted as a pass).

The full list with each test's SKIP line is in the build's `share/doc/rimed/cargo-tests.txt`.
TODO(handover): the 76 tests blocked by the login-session cgroup (26 skipped + 50 self-skipped) can run in a NixOS VM
check where `cargo test` runs inside a logged-in user session.

### Ignored in the source (3)

- rime: `a_removed_plugins_hook_is_observed_not_to_run`: drives the real `claude`, which CI does not install.
- rime-remote-core: `vectors::write_the_android_noise_vectors`, `vectors::write_the_android_push_vectors`: they write
  test-vector files; run by hand after a protocol or envelope change.

### Made to run instead of skipped

- **rime-secretd `tests::a_stale_socket_from_a_dead_daemon_is_replaced`** was skipped as flaky. The cause was in the
  test: it bound the socket in-process and dropped the listener, but a concurrent test's fork could still hold the fd
  until exec, so the "dead" socket sometimes still accepted a connection. The dead daemon is now a child process (the
  test binary re-run with `RIME_SECRETD_TEST_BIND_AND_EXIT`) that binds and exits, so no fd outlives it. Measured in the
  container (`cargo test -p rime-secretd`, debug, `/var/lab/rime-nix/scratch/ci/secretd.sh`): the whole crate in
  parallel 20/20 runs green, the test alone 50/50. Mutant (what a concurrent fork does): after the child exits, bind the
  path again in-process and hold a `try_clone()` of the listener: 5/5 runs fail with "connecting to a socket whose
  daemon has exited SUCCEEDED". (The loop skipped one other test, `peer::tests::a_handle_for_a_dead_process_...`, only
  because it execs `/bin/true`, which the container lacks; it runs and passes in the Nix build.)
- **rime-remoted `the_record_names_this_machine_this_port_and_nothing_that_identifies_it` and
  `the_record_goes_away_when_the_daemon_is_killed_outright`** were skipped as "needs avahi". They publish through a
  stub; the stub's `#!/usr/bin/env bash` needed `/usr/bin/env`, which NixOS has and the bwrap test root now provides.
  Both pass.

### Checks on the report

`checks.x86_64-linux.ci` runs the report on fixture logs: a clean log (exact per-crate rows), a failed test, a skip name
that filtered nothing, a self-skip not in `selfSkipOk` (refused), the same self-skip named in `selfSkipOk` (accepted and
counted as selfskip, not passed), the lower-case `skipped: <why>` spelling (refused, then accepted when named), a
`selfSkipOk` name that did not skip itself (refused), and a target with no header.
Mutants: counting a self-skip as passed (dropping `c["passed"] -= 1`) fails the check on the two row assertions; the
first version's pattern (`^SKIP|^skipping`, which missed `skipped:`) fails it on the lower-case fixture.
