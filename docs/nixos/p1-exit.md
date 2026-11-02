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

Local plan, `nix/ci` branch before the wave-1 merges (the plan after them is in "Wave-1 integration" below):

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

### Why those five are skipped

`nix build --dry-run` against an empty store (what a hosted runner starts with) shows `linux-rime-7.2.9.drv` in exactly
four checks: `vm-boot`, `kernel-gate`, `kernel-vm`, `kernel-boot-budget`. The wave-1 merge of `nix/kernsign` added a
fifth, `kernel-sign` (it signs and relinks the real Rime kernel image; `nix-store -qR` on each check's derivation names
`linux-rime` in exactly these five). A full kernel build on a 4-vCPU hosted runner
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
container (`/var/lab/rime-nix/artifacts/ci/jobs.sh`, under `nix shell` for gawk/grep/sed, which the container lacks and a
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
- Hyprland in the `shell` check (fixed on `nix/flakes`): its VM used `programs.hyprland` without `withUWSM = true`, and
  the NixOS module's override then gave `/nix/store/a6gqc16mwivb1pf7xpf6sgcnjvggprx4-hyprland-0.57.0`, which is not on
  cache.nixos.org. `nix/tests/shell.nix` now sets `withUWSM = true` as the desktop module does, so `shell`, `desktop` and
  `libexec` all get `dmxlm63flzqqmm1wxfgmq057g2df2pmx-hyprland-0.57.0` = `pkgs.hyprland`, which `nix path-info --store
  https://cache.nixos.org` lists (`/var/lab/rime-nix/artifacts/flakes/hyprland-cache.txt`). `gaming` needs no change:
  its node has `programs.hyprland.enable = false`, and `nix-store -qR` of its derivation names no Hyprland at all.

### Handover

- TODO(handover) R2: the Rime binary cache. With it, the five kernel checks substitute the kernel and can move off the
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

From `share/doc/rimed/cargo-tests.txt` of `/nix/store/3raai4vl8hdy1kgx4wgq3g3ipimf7742-rimed-0.1.0`, the store path
`nix eval --raw path:.#rimed.outPath` names for this branch (built in the container, checkPhase and postCheck passed):

```
crate                targets  passed selfskip  failed  ignored  skipped
rime                       5     704        8       0        1        7
rime-agent-core           11     811        3       0        0        0
rime-agentd               16     272       29       0        0       27
rime-aid                   1      24        0       0        0        0
rime-backup-core           2     167        0       0        0        0
rime-paths                 4       5        0       0        0        0
rime-perm-core             2      22        0       0        0        0
rime-remote-core           3     116        0       0        2        0
rime-remoted               5      89       17       0        0        0
rime-secret-core           2     151        0       0        0        1
rime-secretd               3     390        0       0        0        0
rimed                      1      55        0       0        0        0
rimed-core                15     935        0       0        0        0
TOTAL                     70    3741       57       0        3       35
```

- **passed**: ran and passed.
- **selfskip**: cargo reports ok, but the test printed SKIP and returned early, so it proved nothing. Before this report
  existed, these 57 read as passes.
- **skipped**: excluded by `--skip` with a reason (below).
- **ignored**: `#[ignore]` in the source.
- **In a logged-in session** (`checks.rimed-session`, below): the 76 of these that need a login-session cgroup (26 of
  the 35 skipped, 50 of the 57 self-skipped) all pass in a NixOS VM, none by skipping. With them, 3741 + 76 = 3817 of
  the 3836 tests have run and passed; 9 skipped, 7 self-skipped and 3 ignored remain.

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
The 76 tests blocked by the login-session cgroup (26 skipped + 50 self-skipped) run in `checks.rimed-session`
(below).

### In a logged-in session: `checks.rimed-session`

`nix/tests/rimed-session.nix` builds the workspace's release test binaries the way rimed's checkPhase does (same
source, same `RIME_PATH_*`, compiled only), boots a VM where getty logs alice in on tty1 (`session-N.scope` under
`user-1000.slice`), and her login shell runs every test binary with the 76 names (`rimed.passthru.sessionTests` =
`needsLoginSessionCgroup` + `noLoginSessionCgroup`) as filters. Each name must have run, passed and printed no SKIP line.
Negative control: the same runner from the driver's root shell (`runuser`, so the process sits in `backdoor.service`,
a system service and not a session) must see them refused.

Result on `nix/flakes` (2026-11-02, `/nix/store/np2rw28pqicy7587wli07yxax5jzlivj-vm-test-run-rime-rimed-session`, log
`/var/lab/rime-nix/artifacts/flakes/rimed-session.log`): the runner's cgroup was
`0::/user.slice/user-1000.slice/session-1.scope`, and

    rimed-session: 76 names; in a login session 76 passed without SKIP (76 test runs ok), 0 did not; outside one (backdoor.service) 76 refused

so all 76 tests that `cargo test` in the sandbox cannot run (26 skipped + 50 self-skipped) pass on NixOS in a real
login session, and none of them passes by skipping. Getting there needed test fixtures only, no Rime code change: the
tests were written against a bootc machine's `/bin` (a stand-in agent run as `/bin/cat`, stand-in claude scripts with
`#!/bin/bash` that call `cat` and `mv`, a copy of `/bin/sleep` run under another name, which must not be nixpkgs'
multi-call coreutils), and rime-remoted's `end_to_end` stale-build guard compares source mtimes with the binaries', so
the VM's copy of the source tree gets the epoch. Earlier runs before those fixtures: 60 of 76, then 73 of 76.

### Ignored in the source (3)

- rime: `a_removed_plugins_hook_is_observed_not_to_run`: drives the real `claude`, which CI does not install.
- rime-remote-core: `vectors::write_the_android_noise_vectors`, `vectors::write_the_android_push_vectors`: they write
  test-vector files; run by hand after a protocol or envelope change.

### Made to run instead of skipped

- **rime-secretd `tests::a_stale_socket_from_a_dead_daemon_is_replaced`** was skipped as flaky. The cause was in the
  test: it bound the socket in-process and dropped the listener, but a concurrent test's fork could still hold the fd
  until exec, so the "dead" socket sometimes still accepted a connection. The dead daemon is now a child process (the
  test binary re-run with `RIME_SECRETD_TEST_BIND_AND_EXIT`) that binds and exits, so no fd outlives it. Measured in the
  container (`cargo test -p rime-secretd`, debug, `/var/lab/rime-nix/artifacts/ci/secretd.sh`): the whole crate in
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

## Wave-1 integration (2026-11-02)

The eleven wave-1 branches (`nix/ci`, `size`, `ledger`, `kernsign`, `boothook`, `catalogue`, `hostlayer`, `polish`,
`update-e2e` with `release` and `client` inside it) merged into `feat/nixos`, and rime-shell's `nix/polish` into
rime-shell `feat/nixos` (local; the flake.lock still pins the older rime-shell until that is pushed and relocked, so every
command below ran with `--override-input rime-shell path:/work/shell`). Evidence files are in
`/var/lab/rime-nix/artifacts/integrate-wave1/`.

### What the merge broke, and the fixes

- `checks.release-standin` failed: `rime trust --gate refused the published manifest`, `rime: /etc/rime/update.toml: No
  such file or directory`. The client's NixOS dispatch took over a `RIME_TRUST_ROOT` fixture run, which must stay on the
  bootc verifier. `rimed/rime/src/trust.rs` now asks `nixos_gate()` (no fixture root, and the machine is NixOS), with a
  unit test; the failing run on the tree before the fix is its mutant.
- `checks.hostlayer` timed out at 900 s in "rime-user makes a disposable guest on NixOS": polish gave tty2 to kmscon,
  which draws on DRM, so `wait_until_tty_matches("2", "login: ")` (it reads `/dev/vcs2`) never matched. The hostlayer
  test node turns kmscon off and logs in through getty.
- `checks.size` failed on the budget: linux-firmware (fedora-parity) joined the closure. Budget raised once to 10.5 GiB;
  `closure.md` §9 says what grew and re-measures `plain`.
- `kernel-sign` builds the Rime kernel, so it joined the CI skip list (above).

### Results on `feat/nixos` 53210926a

- `nix flake check path:. --no-build`: "all checks passed!".
- Every one of the 30 flake checks built in the `rime-nix` container: `nix eval` of each check's `outPath`, then `nix
  path-info` of it, prints 30 valid paths (`proof-final.txt`). Retries, stated: `update-client` failed twice (hung
  in flake check 2, failed on the first lone build) and `update-e2e` hung twice (flake checks 1 and 2), all from the
  virtiofsd race below; each passed on its last lone build (`update-rerun.log`).
- `hostlayer-katana` with the real snapshot (`RIME_KATANA_STATE=/var/lab/rime-nix/artifacts/katana-state`, private
  TMPDIR, in the container): all six subtests pass, andre logs in through greetd, snapshot digest `fe7aab3c…` the same
  before and after (`hostlayer-katana.log`). In `nix flake check` it skips by design.
- `size` mutants, all nine still fail with the new budget (alsa32-dropped, docs-back, manual-back, mbrola-back,
  niri-doc-unlinked, registry-store-path, registry-unlocked, second-nerd-font, webkit-back).
- CI plan: `plan: 30 checks, 25 run by CI, 5 skipped`; suite coverage: `148 suites, 140 run by CI, 8 exempt, 0 unrun
  and undeclared`; `flake checks: 25 run by .github/workflows/nix.yml, 5 skipped with a reason`.
- cargo total (`share/doc/rimed/cargo-tests.txt` of `/nix/store/crzca63q0qsvk8jha0f6h6rp0njddwia-rimed-0.1.0`):

```
crate                targets  passed selfskip  failed  ignored  skipped
rime                       5     704        8       0        1        7
rime-agent-core           11     810        3       0        0        0
rime-agentd               16     272       29       0        0       27
rime-aid                   1      24        0       0        0        0
rime-backup-core           2     167        0       0        0        0
rime-paths                 4       5        0       0        0        0
rime-perm-core             2      22        0       0        0        0
rime-remote-core           3     116        0       0        2        0
rime-remoted               5      89       17       0        0        0
rime-secret-core           2     151        0       0        0        1
rime-secretd               3     390        0       0        0        0
rimed                      1      55        0       0        0        0
rimed-core                15     935        0       0        0        0
TOTAL                     70    3740       57       0        3       35
```

### The update-client / update-e2e hang (a nixpkgs race, not Rime code)

Seen four times in seven runs of the two checks under load (flake check 1: update-e2e; flake check 2: both; the first lone update-client retry). The generated `run-client-vm` (nixpkgs `1c89083e`, `qemu-vm.nix`) starts its
virtiofsd daemons with `&` and then `exec`s QEMU without waiting for their sockets. When QEMU wins, it exits 1:
`-chardev socket,id=shared,path=…/shared: Failed to connect to '…/shared': No such file or directory`. The test driver's
error path then calls `self.process.stdout.read()`, which never returns, because the two orphaned virtiofsd inherited
that pipe and wait for a connection forever. So a one-line failure becomes a hang until the 3600 s global timeout. To see
the real error, kill the client's two orphaned virtiofsd by PID; the driver then prints `QEMU process exited with code 1
before connecting to monitor socket` with QEMU's output. The client is the node with no nix-store virtiofsd (it boots a
disk image), so its two daemons have the least head start.

**Fixed for every Rime VM test on `nix/flakes`.** `flake.nix` hands `nix/tests` a `pkgs` whose
`testers.runNixOSTest` imports `nix/tests/lib/vm-harness.nix` into every test, which sets `qemu.package` (mkDefault) to
`nix/tests/lib/rime-qemu.nix`: nixpkgs' `qemu_test` with `qemu-system-x86_64` replaced by
`nix/tests/lib/qemu-wait-sockets.sh`. The wrapper reads every non-server `-chardev socket,path=` QEMU is given, waits
until each is LISTENING in `/proc/net/unix` (never by connecting: virtiofsd serves one connection), then `exec`s the real
QEMU; it names every socket that was late (`rime-qemu: 1 of 3 socket(s) not listening yet (shared)`). A socket that never
listens (300 s, `RIME_VM_SOCKET_WAIT`) is named, the run script's background children are killed so the driver's pipe
closes, and QEMU is not started: a failure, never a hang. `nix eval` of `config.qemu.package.name` of every VM check gives
`qemu-rime-test-11.1.2` for all 23 (`qemu-per-check.json`). Checks: `vm-harness` (no VM: late, bound-not-listening,
server and never-listening sockets against a fake QEMU; the wrapper with its wait removed fails "QEMU ran before the
socket listened") and `vm-harness-vm` (a real VM whose virtiofsd starts 5 s late every time: boots, both virtiofs mounts
present). Its stock half, `nix build path:.#checks.x86_64-linux.vm-harness-vm.stock` (nixpkgs' QEMU, 300 s timeout),
hangs at "starting vm" until the timeout (`vm-harness-vm-stock.log`).

Before the fix (evidence in `/var/lab/rime-nix/artifacts/flakes/`): the four hangs above, the stock `vm-harness-vm`
above, and one seen live on 2026-11-02 07:50 in another agent's `nix flake check` (`race-in-the-wild-2026-11-02.txt`):
an update-client driver 3151 s old, the server's QEMU running, NO client QEMU, the client's `shared` and `xchg`
virtiofsd reparented to the builder's shell and still waiting.

Stress after the fix (`nix/tests/lib/stress.sh`, the update-client driver of commit 32d3ee3d4,
`/nix/store/2cb1hig425s5wdwvsckmjmd9s5ww2ad1-nixos-test-driver-rime-update-client`, run 6 times AT ONCE in the
container, 12 VMs, beside the other agents' VM tests; launched 2026-11-02 08:25 with 40 GB available and load average
29.5, which rose to 98 during the runs): **6 of 6 passed, 0 hangs, 0 failures**, 1496-1506 s each. In 21 of the 30 VM
starts (5 per run: server, client, and the client's reboots) a virtiofsd socket was not listening yet when the run
script exec'd QEMU (19 times `shared`, once `nix-store`, once `shared` and `xchg`): every one a race that stock nixpkgs
leaves to chance, and that the wrapper waited out (0-2 polls, at most 4 s). Logs: `stress6/run*.log`, `stress6/summary.txt`,
`stress6-late-sockets.txt`, `stress6-launch.txt`.

### P1 exit gate, line by line (PLAN §6.1)

| gate item | state |
|---|---|
| Toplevel builds from the rime-os flake on the katana runner and on the hosted fallback | **not met**: builds in the `rime-nix` container only. The hosted workflow has never run (the branch is not pushed); the `katana-nix` runners (`ci/runner/`) are not installed |
| `cargo test --workspace --no-fail-fast` passes, total recorded | **met**: 3740 passed, 57 self-skipped, 0 failed, 3 ignored, 35 skipped by name (above) |
| Security gates G1-G9 pass in VM tests | **met in the container**: `checks.security` (G1-G9 named in `nix/tests/security.nix`) passes on the merged tree; not yet on a hosted run |
| The real closure is measured; P5 figure and A11 floor set from it | **met, re-measured**: `closure.md` §9, P5 = 3.46 GB (bootc 4.20 GB), A11 = 19 GB as designed / 14 GB physical / 7 GB install-only. The gaming figures are pre-firmware and stale |
| The §4.12 table is complete with an owner per row | **not met**: `subsystem-owners.md` §6 still lists unowned rows. It was written before the other wave-1 branches merged; kmscon (polish) and guest sessions on NixOS (hostlayer's `rime-user` guest subtest) are done since, the rest is open |
| Runner isolation steps 1-3 done (D8 gates) | **not met**: step 1 exists as code (`ci/runner/`, its own tests); not installed on katana; steps 2 and 3 not started |
