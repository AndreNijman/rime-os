# rimed D-Bus contract (FROZEN at M3, extended additively at M6)

`rimed` exposes a single service on the **system bus**. This document is the
frozen interface contract: the rime-shell `PowerProfileService`, the `rime`
CLI, and any Grafana/metrics consumers depend on it. Changes after M3 are
additive only (new members, new interfaces); existing signatures and the tier
IDs never change.

- **Bus name:** `org.rimeos.Rimed1`
- **Object path:** `/org/rimeos/Rimed1`
- All seven interfaces live on that one object path.

## Tier IDs (frozen)

Exactly these five strings, ordered most→least aggressive. They are the value
of `.Power.Tier`, the argument to `SetTier`, and the members of `.Power.Tiers`:

```
ultra-max   ultra   performance   balanced   power-saver
```

They match the rime-shell picker IDs verbatim (`PowerProfileService.qml`).

## `org.rimeos.Rimed1.Power`

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Tier` | property (r) | `s` | Current tier ID |
| `Tiers` | property (r) | `as` | All tier IDs, high→low |
| `OnAcPower` | property (r) | `b` | AC line online |
| `AutoSwitch` | property (r) | `b` | AC/battery auto-switching enabled |
| `SetTier` | method | `s → ()` | Switch tier; `InvalidArgs` on unknown ID; polkit `manage-power` |
| `SetAutoSwitch` | method | `b → ()` | Toggle auto-switch; enabling reconciles immediately; polkit `manage-power` |
| `TierChanged` | signal | `s` | Emitted whenever the active tier changes |

## `org.rimeos.Rimed1.Battery`

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `ChargeStart` | property (r) | `y` | Charge start threshold (%) |
| `ChargeEnd` | property (r) | `y` | Charge stop threshold (%) |
| `TravelMode` | property (r) | `b` | Travel/storage window active |
| `Capacity` | property (r) | `y` | Battery charge (%) |
| `Status` | property (r) | `s` | e.g. `Charging`, `Discharging`, `Full` |
| `SetChargeThresholds` | method | `yy → ()` | start, end; `InvalidArgs` if start>end or end>100; polkit `manage-battery` |
| `SetTravelMode` | method | `b → ()` | On = 55/60 storage window; off = restore profile defaults; polkit `manage-battery` |
| `Calibrate` | method | `() → ()` | Opens a 0/100 calibration window; polkit `manage-battery` |

## `org.rimeos.Rimed1.Profile` (read-only)

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Active` | property (r) | `s` | Effective profile ID (device ?? class ?? generic) |
| `Class` | property (r) | `s` | CPU-class profile ID, or `""` |
| `Device` | property (r) | `s` | Exact-device profile ID, or `""` |

## `org.rimeos.Rimed1.Metrics`

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Snapshot` | property (r) | `a{sv}` | Best-effort telemetry: `tier`(s), `on_ac`(b), `ppt_watts`(d), `battery_uwh`(t), `temp_<zone>`(d) |

Read it from the CLI with `rime metrics`:

```
rime metrics                        # aligned table, one sample
rime metrics --json                 # one JSON object
rime metrics --stream               # resample every 2s until interrupted
rime metrics --stream 0.5           # ... every 500ms
rime metrics --json --stream 1      # JSON Lines, flushed per sample
```

It only reads, so it needs no root. Keys the machine cannot report (no battery,
no hwmon power source) are left out instead of rendered empty. A one-shot read
exits non-zero if the daemon is unreachable, while `--stream` keeps retrying, so
a daemon restart does not end a long-running collector.

## `org.rimeos.Rimed1.Fan` (real since M6)

`Mode` and `SetMode` keep their M3 signatures; everything else is additive.

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Mode` | property (r) | `s` | `auto` \| `max` \| `manual` \| `curve`. Stays `auto` on a machine with no controllable fan (see `Supported`) |
| `Supported` | property (r) | `b` | True when rimed discovered a fan knob (hwmon `pwm*`, or msi-ec `fan_mode`/`cooler_boost`) |
| `Modes` | property (r) | `as` | The mode keywords this hardware accepts; empty when unsupported |
| `Pwm` | property (r) | `y` | Duty cycle rimed last commanded (0 outside manual/curve) |
| `Fans` | property (r) | `aa{sv}` | Per fan: `id`(s), `chip`(s), `rpm`(u, hwmon only), `percent`(y, msi-ec only), `pwm`(y), `controllable`(b) |
| `SetMode` | method | `s → ()` | Accepts `auto`, `max`/`full`, `manual`, `manual:<0-255>`, `curve`; `InvalidArgs` otherwise, `Failed` when unsupported; polkit `manage-power` |
| `SetPwm` | method | `y → ()` | Manual mode at a duty cycle, floored by the profile's `min_pwm`; polkit `manage-power` |
| `RestoreFirmware` | method | `() → ()` | Hand the fans back to firmware control now; polkit `manage-power` |

`rpm` and `percent` are independently optional: hwmon reports RPM, the MSI
embedded controller reports a percentage, and rimed never synthesises one from
the other. Fan writes go through the same `SysWriter` as every other write, so
`RIMED_DRY_RUN=1` neutralises them.

## `org.rimeos.Rimed1.GameMode` (real since M6)

`Active` and `SetActive` keep their M3 signatures.

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Active` | property (r) | `b` | A session is running |
| `Supported` | property (r) | `b` | The active profile permits game mode |
| `Status` | property (r) | `a{sv}` | `active`(b), `supported`(b), `tier`(s), `cgroup`(s), `cpuset_policy`(s), `irq_policy`(s); while active also `cpus`(s), `core_source`(s), `prior_tier`(s), `irqs_steered`(u), `irqs_attempted`(u), `irqs_refused`(u), `gpus_locked`(au), `gpus_lock_attempted`(au), `scx_requested`(s), `scx_state`(s), `scx_detail`(s), `scx_btf`(s), `pids`(au), `notes`(as), `owner_pid`(u); while idle also `cpus`(s, empty), `core_source`(s), `pcores`(s), `ecores`(s), `nvidia_smi`(b), `scx_requested`(s), `scx_state`(s), `scx_detail`(s), `scx_btf`(s) |
| `SetActive` | method | `b → ()` | Enter/leave; idempotent both ways; polkit `manage-power` |
| `StartForPid` | method | `u → ()` | Enter and pin a PID (its children inherit the cgroup); polkit `manage-power` |
| `AttachPid` | method | `u → ()` | Attach another PID to a running session; `Failed` when inactive; polkit `manage-power` |
| `StartOwnedBy` | method | `u → ()` | Enter and record the process whose death ends the session; polkit `manage-power` |
| `ActiveChanged` | signal | `b` | Emitted on every entry and exit |

Entering also moves the tier (to the profile's `[gamemode] tier`) and disables
auto-switching for the duration. Exit restores both, and rimed emits
`Power.Tier` + `TierChanged` so `.Power` consumers stay in step.

`StartOwnedBy` exists because a Gaming Mode session **could not release
itself**. The session script's `EXIT` trap calls `SetActive(false)`, which is
`manage-power` and therefore `allow_active = yes`. The instant logind stops
calling that session active (a greetd restart, a VT switch, any logind-driven
teardown), polkit refuses the session's own release. Measured on katana
2026-09-19: the machine sat on a p-core cpuset with steered IRQs and the
`performance` tier for 75 minutes with nothing able to undo it.

> **That account named `scx_lavd` as a fourth thing left running, and it was
> wrong.** Corrected 2026-09-20, and the correction is recorded because it
> matters: no scheduler was running, then or on any boot before the
> correction. `rime game status` said one was, `scxctl` had refused every call,
> and the status surface was repeating the plan. The three things above were
> real. See the `scx_*` keys below.

The daemon takes the job instead. `StartOwnedBy(pid)` records the PID **and
its `/proc` start time**, a 2 s watch reads `/proc/<pid>/stat`, and when the
owner is gone rimed calls the same `game_exit()` and emits the same
`ActiveChanged` / `Status` / `Power.TierChanged` set a method call would. It is
the *same* polkit action: entering game mode is exactly as restricted as it
was, and no new caller gains a new operation. rimed **watches the owner and
does not pin it**; `StartForPid`/`AttachPid` remain the way to put a PID in the
cpuset. `owner_pid` in `Status` is `0` for a session nothing is watching. A
`/proc` that cannot be read is a third answer and never a release.
`docs/gaming-and-sessions.md` §5a has the full argument, including why the
polkit rule stayed as it is.

`irqs_steered` counts the affinity writes the **kernel accepted**, not the ones
the plan contained. It carried the plan's number until it was corrected, which
made it false on any machine that refuses affinity writes: kernel-managed MSI-X
queues answer `-EIO`, so a session could report "12 IRQs steered" having
steered none. The plan's number now has its own key, `irqs_attempted`, and
`irqs_refused` is the difference. A partial result is the normal case on real
hardware, and `notes` carries the kernel's reason when there is one. Consumers
that render `irqs_steered` need no change and now render a measurement.

`gpus_locked` had the same defect and the same fix: it was the list of GPUs the
plan MEANT to lock, so a card whose clock lock `nvidia-smi` rejected was still
reported as locked. It now holds the GPUs whose lock writes all landed, with
`gpus_lock_attempted` beside it.

`scx_state` is **`loaded`, `not loaded`, `unknown` or `not requested`**. The
three that are not `not requested` are three different facts, which this
surface used to collapse into one. Before 2026-09-20 there was no sched-ext key
at all: the only report was a `notes` line reading `sched-ext: scx_lavd
for the session`, copied out of the plan, printed on machines where the switch
had refused every call since the feature landed.

* **`loaded` requires a reading of `/sys/kernel/sched_ext`.** A `scxctl` that
  exits 0 is a fact about `scxctl`. The daemon waits, bounded, for the
  scheduler to attach and then reads the kernel; a command that succeeds and
  changes nothing reports `not loaded`.
* **`not loaded`** covers both "nothing attached" and "this kernel has no
  `CONFIG_SCHED_CLASS_EXT`", because both are definite.
* **`unknown`** is the third answer, and rimed never rounds it to either of the
  others: `state` unreadable, or mid-transition (`enabling`/`disabling`).
* `scx_detail` names both halves (what `scxctl` said and what the kernel says),
  so a disagreement shows up instead of being resolved in silence. It also
  flags a scheduler that attached but is not the one that was asked for. The
  kernel's `root/ops` publishes the **struct_ops** name, which drops the
  prefix: `scx_lavd` reads as `lavd`.

`scx_btf` is the **fourth** key, added 2026-09-20. It answers the question
`scx_state` cannot: whether a scheduler could EVER attach to this kernel. It is
a reading of `/sys/kernel/btf/vmlinux` taken by `rimed` itself.

* **`ok`**: the sched-ext kfunc prototypes are the shape a BPF scheduler
  expects.
* **`implicit-args`**: one or more `scx_bpf_*` kfuncs still carry the
  verifier's implicit `struct bpf_prog_aux *` argument in their public
  prototype, so `libbpf` rejects every scheduler with `func_proto incompatible
  with vmlinux`. **No sched-ext scheduler can load on such a kernel**, and no
  Rime setting changes that. Every Rime image gave this reading until Rime
  began building its own kernel (`Containerfile.kernel`, pahole 1.32), whose
  build gate refuses a kernel with the defect. `docs/gaming-and-sessions.md`
  §5d has the measurement, and §5e what followed once a scheduler could load.
* **`no-sched-ext`**: the BTF parsed and carries no `scx_bpf_*` kfunc at all.
* **`absent`**: `/sys/kernel/btf/vmlinux` is not there.
* **`unreadable`**: it is there and could not be read or parsed. Kept
  **separate** from `absent`: a probe that could not look has not looked and
  found nothing wrong.
* `not probed`: nothing asked for a scheduler, the same shape as
  `scx_requested` being empty.

`scx_requested` is the scheduler the profile resolves to on this CPU, not the
profile's text: the default `scx = "auto"` is `scx_lavd` on a CPU with one kind
of core and empty (the kernel's own scheduler) on an Intel P/E hybrid
(`GameModeConfig::scx_for`; `docs/gaming-and-sessions.md` §5f).

When `scx_btf` blocks loading **and** the kernel did not end up with a
scheduler attached, rimed appends its sentence to `scx_detail`. A session that
reports `loaded` is not argued with, and a kernel with nothing wrong with it
earns no clause, so the clause's presence is itself information.

The four `scx_*` keys are also present **while game mode is off**, reporting
the live reading. On katana `sched_ext/state` read `disabled` before, during
and after a session, so quoting it as a release discriminator proved nothing,
and the surface makes that visible instead of leaving a reader to infer it. It
matters more for `scx_btf`: you can learn that no Gaming Mode session can carry
a scheduler **without starting one**.

## `org.rimeos.Rimed1.Mode` (2026-10-10)

The mode the user chose, kept across restarts in `/var/lib/rimed/mode`
(`StateDirectory=rimed`). At every start rimed plans the held mode against its
own initial state (`rimed_core::mode::plan`) and applies it itself.

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Held` | property (r) | `s` | Held mode id, `""` when nothing is held (Daily) |
| `Hold(mode)` | method | `s` → `` | Hold a mode; `daily` or `""` releases. Moves no lever. InvalidArgs for an unknown id. polkit `manage-power` |

`rime mode set` calls `Hold` before it moves any lever. While the held mode
keeps game mode on (Gaming), two existing behaviours change, on purpose:

- `GameMode.SetActive(false)` fails (`org.freedesktop.DBus.Error.Failed`, the
  message names `rime mode set daily`) while game mode is active. Without this,
  gamemode.ini's `end=` hook (every desktop game that quits) and Gaming Mode's
  EXIT trap ended the mode the user had turned on.
- `GameMode.StartOwnedBy` on the held session does not adopt the owner, so
  Gaming Mode ending does not release it.

## Authorization

The D-Bus system policy (`org.rimeos.Rimed1.conf`) lets only root own the name
and lets any local user *send* to the service. Reads are unrestricted;
**polkit gates the mutating methods inside the daemon**:

- `SetTier`, `SetAutoSwitch` → action `org.rimeos.rimed.manage-power`
- `SetChargeThresholds`, `SetTravelMode`, `Calibrate` → action `org.rimeos.rimed.manage-battery`
- M6: `Fan.SetMode`, `Fan.SetPwm`, `Fan.RestoreFirmware`, `GameMode.SetActive`,
  `GameMode.StartForPid`, `GameMode.AttachPid`, `GameMode.StartOwnedBy` → action
  `org.rimeos.rimed.manage-power` (reusing the shipped action instead of adding
  new ones to the polkit policy; see the IMAGE TODO in `docs/m6-notes.md` if
  finer granularity is wanted)

Both actions ship `allow_active = yes` (the logged-in local user acts
**passwordless**) and `allow_inactive`/`allow_any = auth_admin`. The daemon
calls `org.freedesktop.PolicyKit1.Authority.CheckAuthorization` with the
caller's `system-bus-name` and **fails closed** if polkit is unreachable.

## Metrics HTTP endpoint

Prometheus text exposition on `http://127.0.0.1:9723/metrics` (any path):

```
rimed_tier{tier="ultra-max|ultra|performance|balanced|power-saver"}  0|1
rimed_ac_online                                                      0|1
rimed_dry_run                                                        0|1
rimed_machine_info{vendor,product,cpu_vendor,scaling_driver,profile,batteries}  1
rimed_ppt_watts                          <watts>        (if a hwmon source exists)
rimed_battery_uwh                        <microwatt-hours>  (if BAT*/energy_now exists)
rimed_temp_celsius{zone="<thermal-zone-type>"}  <celsius>  (per thermal zone)
```

Every metric source is best-effort and read-only; a missing source leaves its
line out instead of raising an error.

## Install paths (image)

| File | Installed to |
|---|---|
| `rimed/rimed/rimed.service` | `/usr/lib/systemd/system/rimed.service` |
| `files/system/dbus-1/system.d/org.rimeos.Rimed1.conf` | `/usr/share/dbus-1/system.d/` |
| `files/system/dbus-1/system-services/org.rimeos.Rimed1.service` | `/usr/share/dbus-1/system-services/` |
| `files/system/polkit-1/actions/org.rimeos.rimed.policy` | `/usr/share/polkit-1/actions/` |
| `config/sysprofiles/*.toml` | `/usr/share/rimeos/sysprofiles/` (overrides the embedded set) |
| `rimed`, `rime` binaries | `/usr/bin/` |
