# Live updates

`sudo rime update` is still the only way an image-owned component changes. The
command now also asks a second question after the release is staged: which
parts of it can run **now**, without a restart, and is it safe to switch to
them at this moment? Whatever passes is switched over and checked. Everything
else waits for the restart it actually needs, and the command says which
restart that is.

Nothing here writes to `/usr`, adds a second updater, or layers packages. The
staged bootc deployment is always the source of truth. Live activation is an
optimisation on top of a reboot that already works, so an unknown answer is
always "wait".

## What `rime update` does

1. **Gate.** The existing signature and provenance gate runs against the image
   reference before anything is pulled (`docs/trust-enforcement.md`).
2. **Stage download-only.** `bootc upgrade --download-only`. A download-only
   deployment is not applied at the next boot and can be garbage-collected, so
   a release that fails the next step can never boot.
3. **Verify what was staged.** The staged deployment's digest goes through the
   same verifier. If it fails, the deployment stays download-only and the
   command stops. If the deployment was already queued by something outside
   `rime update` (a bare `bootc upgrade`), the command says so.
4. **Queue.** `bootc upgrade --from-downloaded`. From this point the release
   applies at the next restart, whatever happens later.
5. **Measure and plan.** The booted and staged trees are diffed (`ostree diff`,
   or `composefs-info dump` as a fallback). Every changed path is classified
   into a component (table below). The ELF dependencies of every changed
   first-party binary are read, and the plan is built against this machine's
   state: sessions and their lock state, Gaming Mode and running games,
   running units.
6. **Activate what is safe.**
   - The staged files of every live component go into a systemd-sysext
     *directory* extension at `/run/extensions/rime-live`, with removed files
     written as overlay whiteouts and the booted OS's `ID`/`VERSION_ID` in its
     extension-release. `systemd-sysext refresh` runs under rime-pkg's lock.
   - Every file is then read back through `/usr` and compared byte for byte
     with the staged tree.
   - Activators run in order: `daemon-reload`, restarts of the restart-safe
     units, the user managers' `daemon-reload`, the Rime Shell in each
     graphical session, then the Hyprland modules.
7. **Verify, or undo.** Each activator checks that the new code is what runs:
   - a restarted unit must be active, and its main PID's executable must not
     be a deleted file;
   - the shell must answer `shell revision` with the staged commit;
   - Hyprland must report no config errors.

   Any failure puts the previous layer back (or removes it), re-runs the same
   activators, and reports `failed-rolled-back`.

`/run` is a tmpfs, so the layer never survives a restart. After a restart the
queued deployment carries the same files.

Flags:

| | |
|---|---|
| `rime update --plan` | stage download-only, verify, print the plan; nothing queued, nothing activated |
| `rime update --live-only` | activate what can be activated; leave the deployment download-only (not applied at the next boot) |
| `rime update --no-live` | the old behaviour: verify, queue, nothing live |

`--plan` needs root because it downloads the release to measure it.

## `rime live`

| | |
|---|---|
| `rime live status [--json]` | the last transaction, per component (anyone may read it) |
| `rime live explain [COMPONENT]` | why each component activated, waited or needs which restart |
| `rime live logs [--json] [-n N]` | the audit log (`/var/lib/rime/live/history.jsonl`) |
| `rime live doctor [--json]` | every capability detector, with what it measured |
| `sudo rime live apply [--only C…]` | activate what the last update deferred (a session was locked, a game was running) |
| `sudo rime live rollback` | remove the live layer and restart what it changed: the booted versions run again |
| `sudo rime live soft-reboot --yes` | restart userspace into the staged deployment (see below) |

`apply` and `soft-reboot` act only on a deployment that `rime update`
verified in **this boot**, with the same digest. A record from an earlier
boot, a superseded record, or one where `--allow-unverified` let an image
through is refused.

## Components

| Component | Paths (examples) | Live? | How it activates | Verified by | Otherwise needs |
|---|---|---|---|---|---|
| Rime Shell | `/usr/share/rime-shell`, `/usr/lib64/rime-shell` | yes | per session, `rime-live-session shell` | new pid answers `shell revision` = staged commit | log out |
| Hyprland configuration | `/usr/share/rime/hypr` | yes | `rime-shell-firstrun` re-renders the modules, then `hyprctl reload` | `hyprctl configerrors` empty | log out |
| Rime tools | `/usr/bin/rime`, `/usr/libexec/rime-*` | yes, if every library it needs is unchanged or proven | next invocation | files compared through `/usr` | soft reboot |
| rimed | `/usr/bin/rimed`, `rimed.service` | yes | `systemctl restart` | active, `/proc/PID/exe` not deleted | soft reboot |
| Rime services | `rime-remoted` (restart-safe); `rime-agentd` | remoted yes; agentd next start | restart / next start | as rimed | — |
| Unit files | Rime units | yes | `daemon-reload` (system and user) | — | soft reboot |
| systemd | PID 1, `libsystemd-shared` | no | — | — | soft reboot |
| Libraries | `libc`, `ld.so`, any `.so` | no | — | — | soft reboot |
| Compositor | Hyprland, niri, labwc, Xwayland | no | no handover exists | — | soft reboot |
| Shell runtime | Quickshell, Qt | no; it also holds back the Shell | — | — | soft reboot |
| NVIDIA | modules, userspace, GSP firmware | no (detector only) | — | — | restart |
| Kernel | `/usr/lib/modules/*/vmlinuz`, modules, initramfs | no | — | — | kernel transition |
| Firmware files | `/usr/lib/firmware` | no | — | — | restart |
| Bootloader | shim, GRUB, bootupd payloads | no | applied by bootupd at its own time | — | restart |
| `/etc` defaults | `/usr/etc` | no | merged at deployment | — | soft reboot |
| Other system | anything else | no | — | — | soft reboot |

Dependencies:

- The Shell needs the Rime tools and the shell runtime to be unchanged.
- The Hyprland configuration needs the Shell and the compositor to be
  unchanged.
- The Rime tools need rimed.

When one component is deferred, everything that depends on it is deferred too.

## Safety rules

- **The lock screen.** The Rime Shell draws the lock screen, so it is never
  replaced while a session is locked or unlocking, or when its lock state
  cannot be read.
  - The engine reads logind's `LockedHint` when it plans.
  - `rime-live-session` reads `LockedHint` again and asks the running shell
    (`qs ipc call shell state`, `"locked":false` only). A shell that cannot
    answer counts as locked, so the first live replacement needs a shell from
    a release that has the `shell` IPC target.
  - It checks a third time, under a `sleep:idle:handle-lid-switch` inhibitor,
    immediately before it stops the old shell.
  - A deferred shell's files are taken out of the layer.
  - Nothing here unlocks anything, talks to PAM, or adds an unlock IPC.
- **No shell left behind.** If putting the old shell back finds no shell
  running, one is started from the restored files. A session with no shell
  has no lock screen.
- **Gaming.** Nothing activates while a Gaming Mode session (gamescope) or a
  game runtime is running (the `rime-game` cgroup, gamescope, wineserver,
  proton). Those components report `deferred`, and
  `sudo rime live apply` picks them up later.
- **Fixed commands only.** The Settings page and the notice run fixed
  commands in the user's terminal (`sudo rime update`,
  `sudo rime update --plan`, `rime live explain`). Nothing in the status file
  is ever executed. The engine accepts no paths, scripts or binaries from
  outside the verified staged tree, and refuses layer paths outside `/usr`.
- **Restarts are allow-listed.** Only `rimed.service` and
  `rime-remoted.service` are ever restarted. Applications are never killed to
  make an update possible.
- **Soft reboot is explicit.** `rime update` never soft-reboots.
  `rime live soft-reboot` refuses unless:
  - it is given `--yes`;
  - bootc reports the staged deployment as soft-reboot capable;
  - no game is running;
  - the deployment was verified in this boot.

  It removes the live layer first, then runs
  `bootc upgrade [--from-downloaded] --apply --soft-reboot=required`.
- **Module signing is untouched.** No module is built or signed on a user
  machine. The doctor reports `module.sig_enforce`, lockdown and Secure Boot;
  it does not change them.

## The transaction record

`/var/lib/rime/live/txn.json` is root-only (0600). It is written with tmp,
fsync and rename **before** each step it describes.

States:

```
discovered → verified → staged → planned → activating → verifying → active
                                    ↘ deferred      ↘ rolling-back → rolled-back | failed
any non-final, active or deferred → superseded
```

Moves outside this graph are refused and change nothing. On the next run:

| Record found | What happens |
|---|---|
| Another boot id | marked superseded |
| Another booted digest (a soft reboot keeps the boot id) | superseded, and any stale layer removed |
| Interrupted while the layer was changing | the previous layer is put back and the activators re-run, then rolled-back |
| Interrupted before activation | closed as failed; the deployment is either download-only or queued, both safe |

Every step is appended to `/var/lib/rime/live/history.jsonl`. One lock
(`/var/lib/rime/live/lock`) serialises `update`, `apply`, `rollback` and
`soft-reboot`.

## Status document (`/run/rime-live/status.json`, 0644, schema 1)

```json
{
  "schema": 1,
  "updated": 1791604800,
  "txn": "…",
  "state": "active",
  "booted": {"digest": "sha256:…", "release": "2026.10.10"},
  "target": {"digest": "sha256:…", "release": "2026.10.11"},
  "staged_for_boot": true,
  "verified": true,
  "components": [
    {"component": "shell", "label": "Rime Shell", "state": "active",
     "requirement": "service-restart", "detail": "active now: …", "versions": "…"}
  ],
  "remaining": "kernel-transition",
  "recommendation": "restart when convenient",
  "summary": "1 component(s) updated live; the rest needs: …"
}
```

Field values:

- **Transaction `state`:** discovered, verified, staged, planned, activating,
  verifying, active, deferred, rolling-back, rolled-back, failed, superseded.
- **Component `state`:** unchanged, active, active-at-next-use, deferred,
  pending, failed-rolled-back, failed, not-applicable.
- **`remaining`:** the largest restart still owed:
  - nothing, next-use, live-reload, service-restart, app-restart
  - driver-reload, compositor-handover, session-restart
  - soft-reboot, kernel-transition, reboot

The Settings page (Updates) and `rime-update-notice` read this file as
untrusted display data. The notice announces each finished transaction once:
applied, undone or failed.

## Capabilities, measured

`rime live doctor` measures these on every run. The table records what was
measured while this engine was built (Rime 2026.10.10 on the L16, Fedora 45,
kernel 7.2.9-cachyos1.apex1).

| Subsystem | Status | Evidence |
|---|---|---|
| Shell, rimed, remoted, unit files, tools, Hyprland modules | **implemented** | end-to-end suite `tests/test-rime-live.sh` (stubbed bootc/sysext/logind, real layer build, whiteouts, rollback, recovery) and `tests/test-rime-live-session.sh`; VM run: see below |
| Library classification | **implemented** | DT_NEEDED closure of every changed first-party ELF; a changed or unreadable library defers it and its dependents |
| Compositor handover | **unavailable** | none of Hyprland 0.56, niri or labwc can hand a live session to a new binary; reported as such, never attempted |
| NVIDIA driver reload | **detector only, opt-in** | eligible only with no process holding `/dev/nvidia*`, no display on the GPU (`nvidia_drm` modeset/holders, refcount ≤ 1) and `[live] nvidia_reload = true` in `/etc/rime/live.toml`. No reload is performed by this release; the L16 has no NVIDIA GPU, and a reload on katana needs explicit permission |
| Kernel livepatch | **unavailable** | the kernel has `HAVE_LIVEPATCH=y` but `CONFIG_LIVEPATCH` is not set; `module.sig_enforce=Y`; lockdown `[none]`; Secure Boot on. Pipeline below; unproven until a `CONFIG_LIVEPATCH` kernel is built |
| Kexec handover (KHO) / Live Update Orchestrator (LUO) | **compiled, not enabled** | `KEXEC_HANDOVER=y`, `LIVEUPDATE=y`; no `kho=on` on the command line and no `/dev/liveupdate`. Nothing in Rime uses it; never automatic |
| Soft reboot | **explicit command** | bootc 1.16 reports `softRebootCapable` per deployment; `rime live soft-reboot --yes` only |
| Bootloader | **classified** | `bootupctl status --json`: an available update is reported; never applied live |
| Firmware (fwupd) | **classified** | pending firmware is `UpdateState` 1 (pending) or 4 (needs reboot); the `needs-reboot` flag alone is a capability, not a pending update |
| Storage backend | ostree: supported; composefs: **unsupported for live** | katana uses the composefs backend, where the deploy tree is not a plain checkout; everything defers there, honestly, until it is measured |

### Livepatch pipeline (design; not shipped)

1. Kernel build: `scripts/config -e LIVEPATCH` in `kernel/kernel-cachyos.spec`.
   This changes the kernel, so it ships with the next kernel rebuild, which
   needs a go-ahead.
2. Patch build in CI, never on a user machine: `klp-build` or `kpatch-build`
   against the exact pinned kernel's `-devel`, producing
   `livepatch-<kver>-<n>.ko`.
3. Signing with the same MOK key in CI as every other module. The module goes
   into the image at `/usr/lib/modules/<kver>/livepatch/`, so it reaches
   machines only inside a verified release.
4. `rime update` classifies it as a new `Kernel` (live) artefact. It is
   loaded only when:
   - the running kernel is exactly `<kver>`;
   - `/sys/kernel/livepatch` exists;
   - `module.sig_enforce=Y`, so the kernel itself rejects an unsigned module.

   It is verified through `/sys/kernel/livepatch/<name>/enabled` and
   `transition`, and rolled back by writing 0 to `enabled`.

## Known limits

- **The first live shell replacement** needs the running shell to have the
  `shell` IPC target, which ships with this release. Until a machine has
  booted it once, a shell change waits for the next login or restart.
- **Lazy QML loading.** Between the layer merge and the swap (seconds) the
  running shell reads new files if it loads a component lazily. If the
  session locks in that window, the late lock check withdraws the files.
- **`rime live apply`** works only in the boot in which `rime update`
  verified the deployment.
- **composefs-backend machines** get no live activation yet.
