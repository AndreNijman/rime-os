# Isolated CI runners on katana (runner isolation, step 1)

PLAN.md D8 and §6.2 row 6. Heavy Nix work (toplevel, packages, `runNixOSTest` VMs)
runs on katana in throwaway containers that cannot see Andre's home, keys or LAN.
Signing never happens here. Nothing in this directory is applied to katana until
Andre runs `sudo ci/runner/install.sh`.

## Shape

```
GitHub ── JIT config (one job) ──> rime-runner@N.service (root, host)
                                      │ podman run --rm, stdin = JIT config
                                      v
                     ┌──────────── job container ────────────┐
                     │ runner 2.338.2 as uid 1000             │  no caps, no_new_privs,
                     │ /nix/store            (read-only)      │  SELinux container_t,
                     │ /nix/var/nix/profiles (read-only)      │  own uid block,
                     │ daemon socket ────────────────┐        │  network rime-runner
                     └───────────────────────────────│────────┘
                                                     v
                     ┌──────────── builder container ────────┐
                     │ nix-daemon, real Nix sandbox, /dev/kvm │  only writer of the
                     │ /var/lab/rime-runner/nix  ->  /nix     │  shared store
                     └────────────────────────────────────────┘
```

- **One job per container.** `rime-runner-job` mints a just-in-time runner config
  (`POST /repos/{repo}/actions/runners/generate-jitconfig`) for a unique name, pipes
  it to the container on stdin and waits. A JIT runner takes exactly one job; GitHub
  removes it afterwards and the wrapper deletes it if that did not happen. `--rm`
  destroys the container and its writable layer. `Restart=always` starts the next one.
  Unlike today's `apex-runner-register`, nothing is written to disk on the host:
  no `config.sh` run, no `.runner`/`.credentials` files, and the token reaches
  curl through a pipe, never argv.
- **The shared Nix store is the builder's.** Jobs are untrusted nix-daemon clients
  (`Trusted: 0`). They can read the store and ask for builds, but they cannot add
  unsigned paths, change substituters or turn the sandbox off. A build output that a
  later job reuses was built by the daemon inside Nix's sandbox, not by job code, so
  one job cannot plant a fake output for the next one without escaping the sandbox.
  That is the "read-mostly" cache: one writer, read-only for everybody else.
- **Caches live on /var/lab** (`/var/lab/rime-runner`): the store, the podman storage
  root (images and every job's writable layer), the build directories. `/var` is ~87 % full.
  The runners use a podman storage root, lock and tmp dir of their own, so they never
  show up in, or lock against, Andre's own containers (`rime-nix` and others).
- **Fork pull requests never reach katana.** `check-workflows.py` (below).
- **Keys stay on GitHub-hosted runners.** Same checker, rule R3.

## Isolation, item by item

| Requirement | How | Proven by |
|---|---|---|
| No `/var/home/andre`, `~/.ssh`, `~/.gnupg` | never mounted; job has its own mount namespace | probes `absent:` + `unreadable:` (test-isolation.sh) |
| …even if someone mounts it by mistake | container root is host uid 2000065536+, so a uid-1000 0600 file stays unreadable; SELinux `container_t` cannot read `user_home_t`/`var_t` | mutant step: a canary mounted at `/var/home/andre/.ssh` is caught, and stays unreadable |
| No host credentials | `/etc/rime-runner/gh-token` and `/etc/apex-runner` exist only on the host side | probes |
| No privilege | `--cap-drop all`, `no-new-privileges`, no sudo in the image, no engine socket, no block/GPU/TPM/kvm devices | probes |
| Own uid range per role | builder 2000000000, slot N 2000000000+(N+1)·65536; outside login.defs `SUB_UID_MAX` (600100000), andre (524288) and ghrunner (1000000) | probe `user namespace maps…` |
| Off the LAN | podman network `rime-runner` 10.89.77.0/24 + nft table `inet rime_runner`: RFC1918, CGNAT (Tailscale), link-local rejected in `forward`; to katana itself only DNS on the gateway | `nft -c` parses it (not loaded by the test) |
| Caches on /var/lab | storage root + store under `/var/lab/rime-runner` | test layout |
| Builds sandboxed | builder runs nix-daemon with `sandbox = true`, `sandbox-fallback = false` | probe builds a derivation through the socket: pid 1 in its own namespace, nixbld uid |

Today's runner is filtered with `meta skuid ghrunner`. That match cannot see rootful
container traffic (it is forwarded from the container's namespace and has no local
socket owner), which is why these rules match the subnet instead.

### Why the builder holds the privilege, not the job (measured 2026-11-01)

Nix's sandbox inside a user-namespaced podman container on katana (podman 6.1.3,
SELinux enforcing) only works with all three of `--cap-add SYS_ADMIN` (inside the
user namespace), `--security-opt unmask=/proc/*` and `--security-opt label=disable`.
With any one missing Nix stops with "this system does not support the kernel
namespaces that are required for sandboxing" (with `sandbox-fallback = false`;
the default silently builds UNSANDBOXED, which is how the first probe looked
like a success). `label=nested` does not help. Dropping to a minimal cap set also
needs `SYS_CHROOT`.

Running job code with `label=disable` would trade away SELinux for every shell step,
every marketplace action and the GitHub token. So only the builder carries those
flags and runs nothing but nix-daemon and the derivation builders Nix itself confines;
the job keeps `container_t`, no capabilities and a read-only view of the store.
**Rejected:** the sandbox inside each job container (flags above, on job code), and
`sandbox = false` in the job (impure builds, and job code writing straight to a
shared store).

## Scheduling: gaming and benchmarks

- `rime-runner.slice` has **weights, no quota**: `CPUWeight=20`, `IOWeight=20`
  (Andre's session is 100). An idle katana gives a job all 20 threads; when he
  plays or works he wins 5:1. `MemoryHigh=44G` throttles before the desktop
  swaps, `MemoryMax=52G`, systemd-oomd kills inside the slice under pressure.
  Today's `apex-github-runner.service` runs with `CPUWeight=10000` (a
  `set-property` drop-in): that one competes with gaming; these do not.
- `sudo rime-runner-ctl gaming on|off`: `CPUWeight=idle IOWeight=1` (runtime only).
- **Benchmarks** (P3: "the runner is paused during every benchmark run"):
  `sudo rime-runner-ctl pause --freeze`, run the benchmark, `sudo rime-runner-ctl resume`.
  `pause` alone takes no new job and lets the running one finish; `--freeze` also
  stops the running job and the builder in place with `podman pause` (the cgroup
  freezer per container: no CPU, IO or GPU use) and resume continues them.
  `systemctl freeze` is not used: katana's systemd 262 reports `CanFreeze=no` for slices. A frozen job misses GitHub's heartbeat, so keep a
  freeze to minutes; a job GitHub declares lost has to be re-run.
- Wiring Gaming Mode (rimed's hold) to `gaming on` automatically is step 2/3.

## Fork-PR and key guard

```
python3 ci/runner/check-workflows.py .github/workflows                     # rules
python3 ci/runner/check-workflows.py --simulate-fork-pr .github/workflows  # dry run
bash ci/runner/tests/test-check-workflows.sh                               # fixtures
```

Every job whose `runs-on` names `self-hosted` or a `katana*` label (or an expression
that could resolve to one) must:

- R1: live in a workflow without `pull_request_target` or `workflow_run` triggers;
- R2: carry the fork guard that kernel-build.yml and katana-probe.yml already use

  ```yaml
  if: >-
    github.event_name != 'pull_request' ||
    github.event.pull_request.head.repo.full_name == github.repository
  runs-on: [self-hosted, katana-nix]
  ```
- R3: use no secret but `GITHUB_TOKEN`, no `id-token: write` (keyless signing),
  no cosign, sbsign, `APEX_SB_*`, `APEX_KEY*`, narinfo signing.

`--simulate-fork-pr` evaluates every self-hosted job's `if:` in a fork
`pull_request` context and fails for each job that would be scheduled. The `if:`
is the belt: a fork PR runs its own copy of the workflow file. The control is the
repository setting "Require approval for all external contributors", which stays on.

Signing stays where it is: cosign, `sbsign` with `APEX_SB_*`, narinfo signing and
the R2 upload run on GitHub-hosted runners, after a katana job hands its outputs
over (step 2 designs that hand-over).

## Install, uninstall

```
sudo ci/runner/install.sh --plan            # print every step, change nothing
sudo ci/runner/install.sh                   # coexist with today's runner (default)
sudo ci/runner/install.sh --replace-legacy  # also stop + disable apex-github-runner.service
sudo /usr/local/libexec/rime-runner/uninstall.sh [--purge]
```

**Default is coexist.** The new runners answer the new label `katana-nix`, so no
existing job moves. `apex-github-runner.service` (label `katana`) keeps serving
`kernel-build.yml` and `katana-probe.yml`, which run rootless podman inside the
runner; a job container here has no container engine. PLAN.md: "Today's
apex-github-runner.service stays until step 3 replaces it".

install.sh records `apex-github-runner.service`'s state (enabled/active) in
`/etc/rime-runner/legacy-state` before it does anything; uninstall.sh puts it back
exactly (with no record it enables and starts it: katana's state on 2026-11-01). It
never touches `/etc/apex-runner`, the ghrunner user, `/var/lab/runner`, the boot
chain, ESP, GRUB, MOK, NVRAM or subvolumes.

### Token

The job wrapper needs a credential that can mint JIT configs. Create a **fine-grained
PAT for AndreNijman/rime-os only**, repository permission **Administration: Read and
write** (nothing else), then

```
sudo install -m 0600 /dev/stdin /etc/rime-runner/gh-token    # paste, Ctrl-D
sudo systemctl enable --now rime-runner@0.service
```

Do not copy `/etc/apex-runner/gh-token`: that file holds Andre's broad `gh` OAuth
token (its own README asks for it to be replaced). install.sh starts no job slot
until the token exists.

## Test it now (no registration)

```
sudo ci/runner/tests/test-isolation.sh          # ~3 min first run (image build), ~20 s after
sudo ci/runner/tests/test-isolation.sh --clean  # remove /var/lab/rime-runner-dryrun
```

It builds the images into `/var/lab/rime-runner-dryrun`, starts the builder and one
job container with the **same `builder_args`/`job_args` production uses**
(`lib/common.sh`), network `none`, `RIME_RUNNER_DRY_RUN=1`: the entrypoint runs
`images/job/probe.sh` instead of the runner. Then the mutant, `nft -c`, and a check
that no container is left and Andre's own podman storage has none of ours. It starts
no unit and touches no host config.

## Files

| Path | What |
|---|---|
| `runner.conf` | every tunable (repo, labels, slots, paths, uid base, subnet, weights) |
| `lib/common.sh` | `builder_args`, `job_args`, store seeding and SELinux labels: the one definition of what a container may see |
| `bin/rime-runner-job` | one job: pause check, JIT mint, `podman run`, deregister |
| `bin/rime-runner-builder` | nix-daemon container, foreground |
| `bin/rime-runner-ctl` | `status`, `pause [--freeze]`, `resume`, `gaming on|off`, `build-images`, `seed` |
| `builder/nix.conf` | daemon settings: sandbox on, no fallback, only root trusted, GC thresholds |
| `images/job/` | Containerfile (Fedora 45 by digest + runner tarball by sha256), entrypoint, probes |
| `systemd/` | slice, builder, `rime-runner@.service`, firewall |
| `nftables.conf.in` | `inet rime_runner` table |
| `check-workflows.py` | fork-PR + key guard, `--simulate-fork-pr` |
| `tests/` | `test-isolation.sh` (root), `test-check-workflows.sh` + fixtures |

## Upkeep

- **Runner version.** With self-update off, GitHub stops sending jobs to a runner more
  than 30 days behind its latest release. Bump `RUNNER_VERSION`/`RUNNER_SHA256` in
  `images/job/Containerfile` and the tag in `runner.conf`, then
  `sudo rime-runner-ctl build-images`, within 30 days of each actions/runner release.
- **Store size.** nix-daemon collects garbage below 50 GB free on /var/lab, up to
  100 GB free. A job's `result` link is not a GC root in the daemon's view, so copy
  what must outlive the job (`nix copy`, upload) inside the job.

## Not in step 1

- The hand-over from katana to the hosted signer, and the hosted fallback dispatcher
  (D8; whether `GITHUB_TOKEN` may list runners is unverified).
- Gaming Mode wired to `gaming on`.
- A container engine inside jobs (bootc kernel builds keep using today's runner).
- Per-job VMs (KVM). Containers were chosen for step 1; the builder already has
  `/dev/kvm` for `runNixOSTest`.
- P3: the runner rebuilt as NixOS configuration on katana.
- Not run on katana: the systemd units, the JIT mint (`generate-jitconfig`, including
  whether GitHub adds `self-hosted`/`Linux`/`X64` to the custom labels itself), the
  network + nft table under a real job, `install.sh` outside `--plan`.
