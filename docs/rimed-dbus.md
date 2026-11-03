# rimed D-Bus contract (FROZEN at M3, extended additively at M6)

`rimed` exposes a single service on the **system bus**. This document is the
frozen interface contract: the rime-shell `PowerProfileService`, the `rime`
CLI, and any Grafana/metrics consumers depend on it. Changes after M3 are
additive only (new members, new interfaces); existing signatures and the tier
IDs never change.

- **Bus name:** `org.rimeos.Rimed1`
- **Object path:** `/org/rimeos/Rimed1`
- All eight interfaces live on that one object path.
- rimed also serves power-profiles-daemon's API on PPD's own names and paths
  (see [The power-profiles API](#the-power-profiles-api-rimed-is-the-one-writer)).

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

`scx_fallback`, `scx_fallback_detail` and `scx_effective` were added
2026-10-31, with the fallback they report (`docs/gaming-and-sessions.md` §5h):

* **`scx_fallback`** is `none`, `verified` (a scheduler failed to load, or was
  ejected later in the session, and after rimed stopped it `sched_ext/state`
  read `disabled`), `unverified` (the stop did not get the kernel back to its
  own scheduler; rimed reports it and does not retry), or `held-back` (this
  scheduler already failed since rimed started, so this session never tried
  it). At most one fallback per session; `held-back` lasts until rimed
  restarts or the machine reboots (profiles are read when rimed starts, so a
  changed profile arrives the same way). While a scheduler that has not
  attached yet is still inside its 20 s grace, `scx_fallback` stays `none` and
  `scx_detail` says it is still waiting.
* **`scx_fallback_detail`** says when it failed (`at entry`, `mid-session
  (ejected)`, `mid-session (ejected and reloaded)`, `mid-session (never came
  up)`), the trigger, the kernel's own words from `/dev/kmsg`
  (`sched_ext: BPF scheduler "…" disabled (runnable task stall)` and the lines
  after it), and the result of the stop. Empty when there was none.
* **`scx_effective`** is who schedules now by the latest kernel reading: the
  requested name, another attached scheduler's struct_ops name, `kernel`, or
  `unknown`. It never names the requested scheduler without having read it in
  `root/ops`.

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

## `org.rimeos.Rimed1.Ui` (2026-11-03)

A short, input-triggered **UI boost**: smooth animations on battery without
moving the tier. On the L16 the battery default stays `balanced` (governor
`powersave`, EPP `balance_power`); for the length of one animation rimed raises
every policy's `energy_performance_preference` to `balance_performance`, then
writes the tier's own EPP back.

| Member | Kind | Signature | Notes |
|---|---|---|---|
| `Boost(ms)` | method | `u → u` | Ask for `ms` of boost. Answers the milliseconds the boost now lasts; `0` = nothing boosted |
| `Active` | property (r) | `b` | A boost is on the hardware now. Not signalled (`emits_changed_signal = false`) |
| `Lever` | property (r) | `s` | What a boost writes in the current tier, e.g. `energy_performance_preference = balance_performance (all policies)`; `""` when this tier is not boosted |
| `MaxMs` | property (r) | `u` | The most one call buys (600) |

**Who may call it.** Unprivileged, no polkit: the caller's uid must be the user
of seat0's **active, local** session (logind over D-Bus: `Seat.ActiveSession`,
then `Session.User/Remote/Active`). Any other caller, root included, gets
`org.freedesktop.DBus.Error.AccessDenied`. An unreachable logind refuses.

**Limits** (rimed enforces them; a client never has to):

- **Cap:** one call buys at most 600 ms.
- **Coalesce, never stack:** a call while boosted moves the end of the one
  window to `max(current end, now + ms)`. Ten callers at once still end 600 ms
  after the last call.
- **Ceiling + cooldown:** one continuous window lasts at most 1.5 s; a window
  that reached it is followed by 1 s in which `Boost` answers `0`.
- **Rate:** per bus connection, 8 calls back to back, then 20 a second;
  beyond that `org.freedesktop.DBus.Error.LimitsExceeded`.
- **Release:** `Boost(0)` gives this caller's share back. A caller that leaves
  the bus gives it back too. When no caller holds the window, the tier's EPP
  is written back at once.
- **Tiers:** only the tiers in the profile's `[ui_boost] tiers` (default
  `["balanced"]`) whose EPP is below the boost's. `performance` and
  `power-saver` answer `0` and write nothing. Any tier change ends a boost;
  the tier's own writes land last.

**How the Shell should call it.** On the input event itself (key press,
pointer press, gesture begin), *before* the first animated frame, and without
waiting for the reply: send the method call with the `NO_REPLY_EXPECTED` flag.
Ask for the animation's own length (a 300 ms transition asks 300); repeating
the call during a long gesture is fine, coalescing makes it free.

```
# From a shell (waits for the answer, for testing):
rime ui boost 300
busctl call org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Ui Boost u 300
# Fire-and-forget, as an input handler should:
busctl call --expect-reply=no org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Ui Boost u 300
```

**Hyprland.** `rime ui hypr-boost` (started by `rime/session.lua`) follows
Hyprland's event socket and asks for 500 ms on `openwindow`, `closewindow`,
`workspace`/`workspacev2`, `activespecial`, `fullscreen` and `movewindow`, at
most once per 100 ms, fire-and-forget. Those events arrive as the animation
starts; the Shell's call on the input event is the one that lands before the
first frame.

**Profile knobs** (`config/sysprofiles/*.toml`):

```toml
[ui_boost]
enabled = true                 # default true
epp = "balance_performance"    # performance | balance_performance | balance_power | power
tiers = ["balanced"]           # default ["balanced"]
```

## The power-profiles API: rimed is the one writer

power-profiles-daemon (PPD) and rimed both wrote the governor, EPP, CPU boost,
the min-frequency floor and `platform_profile`, and the last writer won. Since
2026-11-03 **`power-profiles-daemon.service` is masked** in the image and rimed
owns PPD's names:

- `org.freedesktop.UPower.PowerProfiles` at `/org/freedesktop/UPower/PowerProfiles`
- `net.hadess.PowerProfiles` at `/net/hadess/PowerProfiles`

The PPD package stays, so `powerprofilesctl` and desktop applets keep working,
against rimed. A PPD started by hand anyway cannot own its name and exits
before its drivers write anything.

| Member | Behaviour |
|---|---|
| `ActiveProfile` (rw) | Reads rimed's tier (`performance`, `balanced`, `power-saver`). **Writing it is refused** (the error's message says AccessDenied and names `rime tier`; zbus wraps a property-setter error, so the error NAME on the wire is `org.freedesktop.zbus.Error`): a property write carries no caller rimed could check with polkit |
| `Profiles` | The three tiers, lowest first; `CpuDriver`/`Driver` = `rimed`, `PlatformDriver` when the firmware has `platform_profile` |
| `HoldProfile(s,s,s) → u` | `performance` or `power-saver`; polkit `manage-power`. `power-saver` wins over `performance`. Auto-switch is off while a hold stands. Refused during game mode |
| `ReleaseProfile(u)` | Releasing the last hold puts back the tier and the auto-switch setting from before the first hold. A holder leaving the bus releases its holds. `SetTier` drops every hold (the user's choice stands) |
| `ProfileReleased(u)` | Emitted per released cookie |
| `ActiveProfileHolds` | `Profile`, `ApplicationId`, `Reason` |
| `Actions`, `ActionsInfo` | Empty. `SetActionEnabled` is refused |
| `PerformanceDegraded`, `PerformanceInhibited` | `""` |
| `BatteryAware` | `true` (rimed's AC/battery auto-switch) |
| `Version` | `rimed <version>` |

What moved from PPD into rimed's tiers (amd-pstate): `cpu_boost` (per-policy
`boost`, else `cpufreq/boost`, else intel_pstate `no_turbo`) and `cpu_min_freq`
(`scaling_min_freq` from `amd_pstate_lowest_nonlinear_freq` in
performance/balanced, `cpuinfo_min_freq` in power-saver), the values PPD 0.30
wrote. Not ported: `trickle_charge` (peripheral batteries only) and the amdgpu
`panel_power`/`dpm` actions (off by default in PPD; nothing changes).

## Authorization

The D-Bus system policy (`org.rimeos.Rimed1.conf`) lets only root own the names
(rimed's own and PPD's two)
and lets any local user *send* to them. Reads are unrestricted;
**polkit gates the mutating methods inside the daemon**:

- `SetTier`, `SetAutoSwitch` → action `org.rimeos.rimed.manage-power`
- `SetChargeThresholds`, `SetTravelMode`, `Calibrate` → action `org.rimeos.rimed.manage-battery`
- M6: `Fan.SetMode`, `Fan.SetPwm`, `Fan.RestoreFirmware`, `GameMode.SetActive`,
  `GameMode.StartForPid`, `GameMode.AttachPid`, `GameMode.StartOwnedBy` → action
  `org.rimeos.rimed.manage-power` (reusing the shipped action instead of adding
  new ones to the polkit policy; see the IMAGE TODO in `docs/m6-notes.md` if
  finer granularity is wanted)

- `Ui.Boost` → **no polkit**: the caller must be the user of seat0's active
  local session (see `org.rimeos.Rimed1.Ui` above).
- The power-profiles API: `HoldProfile` → `org.rimeos.rimed.manage-power`;
  writing `ActiveProfile` is always refused.

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
