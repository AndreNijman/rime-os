# Rime OS M6 notes (rimed: real fan control + game orchestration)

M6 turns the two M3 stubs into real implementations: `org.rimeos.Rimed1.Fan`
now enumerates and commands fans, and `org.rimeos.Rimed1.GameMode` runs a real
session (top tier, NVIDIA clock locks, P-core cpuset pinning and IRQ steering)
and undoes it exactly on exit. The frozen M3 member signatures are unchanged;
everything new is additive.

## What landed

```
rimed-core/src/topology.rs   P-core / E-core detection (ladder, records its source)
rimed-core/src/fan.rs        hwmon + msi-wmi-platform + msi-ec discovery, modes, curve, restore
rimed-core/src/gpu.rs        nvidia-smi query + clock-lock planning
rimed-core/src/irq.rs        /proc/irq enumeration, steering plan, irqbalance detection
rimed-core/src/game.rs       the symmetric enter/exit planner
rimed-core/src/profile.rs    [fan] and [gamemode] schema (both optional)
rimed-core/src/tier.rs       12 new Actions, every one carrying an absolute path
rimed-core/src/syswriter.rs  RealWriter support for them + the fan-restore ladder
rimed/src/fan.rs             FanController: snapshot, modes, curve loop, restore
rimed/src/game.rs            GameSession: prior-state capture, enter/exit, status
rimed/src/dbus.rs            real .Fan and .GameMode interfaces
rimed/rimed/rimed.service    ProtectControlGroups=no + the ExecStopPost fan safety net
rime/src/main.rs             `rime fan …` and `rime game …`
config/sysprofiles/*.toml    Katana M6 values; L16 + intel-hybrid degradation
```

No new crates. `Cargo.lock` is untouched, so `cargo build --release --locked`
needs no network beyond the existing dependency set.

### Build / test / lint

Run in a `rust:1-slim-bookworm` container (this workstation is bootc; `/usr` has
no toolchain):

```
cargo build --release --locked   Finished `release` profile [optimized] target(s)
cargo test                       77 passed; 0 failed   (21 fan + 15 gamemode +
                                 10 profile_m6 + 9 topology + 11 tier_plan +
                                 6 selection + 5 rimed in-crate)
cargo clippy --all-targets -- -D warnings   clean
```

The 17 M3 tests still pass unmodified: M6 actions never enter a tier plan
(`profile_m6::every_shipped_profile_still_loads_and_plans_tiers` asserts it).

## Interfaces used

| Concern | Interface | Notes |
|---|---|---|
| Fan RPM | `/sys/class/hwmon/hwmon*/fan*_input` | RPM, read-only |
| Fan control | `/sys/class/hwmon/hwmon*/pwm*`, `pwm*_enable` | hwmon ABI: `0` = no control (**full speed**), `1` = manual, `2` = firmware automatic |
| MSI fan RPM | hwmon chip `msi_wmi_platform` | `msi-wmi-platform` registers **4 read-only `fanN_input` channels and no PWM** |
| MSI fan control | `/sys/devices/platform/msi-ec/fan_mode`, `cooler_boost` | `auto`/`silent`/`basic`/`advanced` and `on`/`off`; **no PWM** |
| MSI fan speed | `/sys/devices/platform/msi-ec/{cpu,gpu}/realtime_fan_speed` | a **percentage**, not RPM; never reported as `rpm` |
| Curve sensor | `hwmon*/temp*_input` (prefers `coretemp`/`k10temp`/`zenpower`), else `class/thermal/thermal_zone*/temp` | |
| P/E split | `/sys/devices/cpu_core/cpus` + `/sys/devices/cpu_atom/cpus` | rung 1; then `cpu/types/*/cpulist|cpumap`, `cpu_capacity`, `acpi_cppc/highest_perf`, `cpuinfo_max_freq` |
| cpuset | cgroup v2: `<cgroup>/cpuset.cpus`, `cpuset.mems`, `cgroup.procs`, parent `cgroup.subtree_control` | default cgroup `/sys/fs/cgroup/rime-game` |
| Prior cgroup of a PID | `/proc/<pid>/cgroup` (`0::` line) | recorded so exit can put the PID back where it was |
| IRQ affinity | `/proc/irq/<n>/smp_affinity_list`, handler names from `/proc/irq/<n>/<name>/` | |
| irqbalance | scan of `/proc/*/comm` | detection only |
| NVIDIA | `nvidia-smi -i N {-pm,-lgc,-lmc,-rgc,-rmc}`, `--query-gpu=index,name,clocks.max.graphics,clocks.max.memory,persistence_mode` | requested clocks are clamped to the queried maxima |

## Fan safety argument

The requirement: no failure mode leaves a fan stopped. Five independent
mechanisms enforce it, in the order they apply:

1. **A floor, not a duty cycle.** `plan_mode` clamps every manual/curve duty
   cycle to the profile's `min_pwm` (default 77/255 ≈ 30%, 90 on the Katana).
   Asking for `0` yields the floor. No code path emits
   `Action::FanPwm { value: 0 }`.
2. **Snapshot before the first mutation.** `FanController::set_mode` captures
   `pwm*_enable` and `pwm*` (and the msi-ec `fan_mode`/`cooler_boost`) once, on
   the first command, and never overwrites that snapshot. Restore replays it.
3. **A restore primitive with a fallback ladder.** `Action::FanSafeRestore`
   tries the recorded prior `pwm_enable`, then `2` (firmware automatic), and if
   both writes fail it drives `pwm` to `255` **before** writing `0` (the hwmon
   ABI's "no control" = full speed). The last rung leaves the fan at full speed,
   never stopped.
   `fan::safe_restore_falls_back_to_full_speed_when_auto_is_refused` proves it,
   using an unwritable `pwm1_enable` to force every rung.
4. **Every graceful exit restores.** `main.rs` unwinds in reverse order: leave
   game mode → `fan.restore()` → drop to a non-ryzenadj tier. Mode changes stop
   the curve loop before applying anything new.
5. **The crash path.** A killed daemon cannot restore anything, so
   `rimed.service` carries
   `ExecStopPost=-/usr/bin/rime fan restore --local`. That verb re-discovers the
   fans and applies `plan_firmware_restore` **directly to sysfs**, with no bus,
   no daemon and no prior state (`fan::firmware_restore_needs_no_prior_state`
   proves a fan left in manual at duty cycle 0 comes back to `pwm_enable=2`).
   By design it skips the `daemon_running` gate the other mutating verbs use.

Two further properties: writes go through `SysWriter`, so `RIMED_DRY_RUN=1`
neutralises all of it, and a machine with no controllable fan reports
`Fan.Supported = false` and plans nothing at all.

**Writable is not the same as effective.** Verified on this workstation (the
L16): `thinkpad_acpi` publishes `pwm1` and `pwm1_enable` at mode 0644 and then
answers `-EPERM` to every write, because the module loaded without
`fan_control=1`. Discovery cannot tell the difference from the file mode, so
`FanController::set_mode` **reads the controls back** after applying and, if
nothing moved, **replays the snapshot before returning an error**. A plan can
land its `pwm_enable=1` write and then have its duty-cycle write refused, and
the safety model must not end in that half-applied state. `rime doctor` phrases
its check as "fan control channel present, write access unverified" for the same
reason.

## The MSI Katana reality

Concurrent hardware research on the target changed the picture, and the code
and profile now reflect it:

* **`msi-ec` binds since 2026-10-08** (IMAGE TODO item 2). The kernel's own
  module (7.2.9) is only the battery-charge part, and its 25-entry EC-firmware
  allowlist has no `17L3` entry (the Katana GF76 is board **MS-17L3**). The
  image now ships the out-of-tree BeardOverflow driver instead, whose
  CONF_G2_1 lists the Katana's 17L3EMS1.109. Measured on the Katana:
  * `fan_mode` and `cooler_boost` are present, so rimed reports
    `Fan.Supported = true`; `rime fan mode max` sets Cooler Boost on and
    `fan_mode` advanced (both fans at ~7,900 / 6,000 RPM), `auto` hands the
    fans back to the EC's own curve;
  * `/sys/class/power_supply/BAT1/charge_control_*_threshold` exist, so the
    profile's `[charge]` block takes effect. msi-ec keeps start = stop - 10
    in one EC byte, so the profile's 60/80 lands as **70/80**.
  * Before this, the in-tree module did not bind on this board: no fans, and
    the `[charge]` block was a silent no-op (the `SysWriter` skips absent
    attributes).
* **`msi-wmi-platform` is the working lead for readings.** It has a `force`
  module parameter (`module_param_unsafe`, so it taints the kernel) and
  registers an hwmon device named `msi_wmi_platform` with four **read-only**
  `fanN_input` channels: RPM you can watch, but no PWM and no mode. rimed's
  generic hwmon leg picks it up on its own; `backend = "msi-wmi"` selects it and
  nothing else.
* Discovery **probes, never assumes**: naming a backend in a profile does not
  imply it exists. `fan::a_named_backend_that_is_absent_degrades_to_unsupported`
  and `fan::an_empty_msi_ec_directory_is_not_a_backend` cover that path.

Probe order for `backend = "auto"`: real hwmon PWM → `msi-wmi-platform` (RPM
only) → `msi-ec` → unsupported.

## Game mode

Enter, in order: record prior tier **and disable auto-switch** → apply the
profile's game tier → apply the profile's fan mode → create the cpuset cgroup
and move the PIDs in → steer IRQs → lock GPU clocks. Exit runs the inverse,
from a plan **built at enter time from values read before rimed wrote
anything**.

Symmetry rules that fixture tests cannot express but that matter on real
hardware:

* rimed captures prior state only on the 0 → 1 transition; a second
  `SetActive(true)` only attaches PIDs, so it can never clobber the restore
  values.
* rimed disables auto-switch for the session. Without that, an AC/battery
  transition mid-game would re-apply the profile default, clobber the game tier
  and strand the recorded prior tier.
* IRQ restore writes back each interrupt's exact prior `smp_affinity_list`;
  rimed neither writes nor records interrupts that were already on the target.
* rimed returns PIDs to the cgroup `/proc/<pid>/cgroup` reported before the
  move, not to the root cgroup.

Degradation: a uniform CPU (the L16) pins to all CPUs and therefore steers no
IRQs; no `nvidia-smi` means no GPU actions at all; a kernel-managed interrupt
that rejects an affinity write is a logged skip, not a failed session.

## Verified by tests vs unverified on hardware

**Verified (72 tests, synthetic sysfs/procfs fixtures, no real `/sys` written):**

* P/E detection on an i7-12700H-shaped fixture (`cpu_core/cpus = 0-11`,
  `cpu_atom/cpus = 12-19`), plus each lower rung, the "2% bin spread is not
  hybrid" guard, a uniform 16-thread AMD machine, and a missing sysfs root.
* Fan enumeration with hwmon, with `msi_wmi_platform`, with msi-ec, with none of
  them, with a named-but-absent backend, and with an allow/deny list.
* Mode plans: `max` = enable 1 + pwm 255 (hwmon) / cooler boost (msi-ec);
  `manual` clamped to the floor; msi-ec boost threshold; `auto` never writes a
  duty cycle; curve interpolation and its floor/ceiling.
* Fan restore: exact snapshot round-trip on disk (including a manual prior
  state), the no-prior-state firmware restore, and the full-speed fallback.
* Game mode: enter → exit leaves the fixture tree **byte-identical** (a
  filesystem diff, not an action-list comparison), exit is idempotent, IRQ
  steering skips IRQ 0/2 and already-correct affinities, the NVIDIA IRQ is
  pinned *to* the game cores, GPU clock locks are clamped to the reported
  maxima, and unlock is symmetric.
* Profile schema: pre-M6 profiles parse unchanged and get safe defaults; partial
  tables keep the other defaults; the Katana values are what the file says;
  an unparseable IRQ policy fails safe to `off`.
* The daemon half of game-mode symmetry (`rimed/src/game.rs`, in-crate tests
  against a `MockWriter` and an empty sysfs root): enter holds the profile's
  game tier and disables auto-switch, exit restores both, a repeated enter
  cannot overwrite the recorded prior state, and an exit with no session (or a
  second exit) changes nothing.

**Not verified (no access to the target machines):**

1. **`msi-wmi-platform` on the Katana.** Whether it binds with `force=1` on
   MS-17L3, and what its four fan channels correspond to. (Whether a driver
   can command these fans at all is answered: msi-ec can, see "The MSI Katana
   reality".)
2. **msi-ec's `realtime_fan_speed` percentage.** `fan_mode` and `cooler_boost`
   were verified on the Katana on 2026-10-08; the speed percentage still comes
   from the upstream driver's README.
3. **`nvidia-smi` behaviour.** No NVIDIA GPU here: the exact `-lgc`/`-lmc`
   acceptance on an RTX 3070 Laptop, whether `-lmc` is supported on that part,
   and the real `clocks.max.*` values are unverified. The clamp means a wrong
   value cannot exceed what the GPU reports, but "wrong but in range" is
   possible. In particular, the `[1200, 1620]` graphics floor/ceiling in the
   Katana profile is a design choice, not a measurement.
4. **cgroup pinning under systemd.** Nobody has tested on a live system whether
   `+cpuset` can be enabled on the root subtree on the shipped image, or whether
   moving a Steam/Proton PID out of its `user@.slice` scope behaves (and
   survives systemd re-parenting).
5. **IRQ steering in practice.** Someone needs to measure which of the Katana's
   interrupts accept an affinity write, and whether pinning the NVIDIA IRQ onto
   the P-cores helps or hurts.
6. **The D-Bus surface at runtime.** As with M3, nobody started the daemon or
   performed a bus round-trip; the zbus code compiles and the client proxies
   match, but nothing has marshalled `Fan.Fans` / `GameMode.Status` live.
7. **The `ExecStopPost` hook firing.** Unit tests cover the code path it
   invokes, but not systemd running it after a crash.
8. **Curve mode.** No machine here has a writable `pwm*_enable`, so the curve
   loop has never driven a real fan.

## IMAGE TODO

Owned by the image agents: M6 left `Containerfile.*` and `files/**` untouched
on purpose. In rough priority order:

1. **`msi-wmi-platform` with `force=1`** (Katana, for fan RPM):
   * `files/system/modules-load.d/rime-msi.conf` → `msi-wmi-platform`
   * `files/system/modprobe.d/rime-msi.conf` → `options msi-wmi-platform force=1`
   * Note: the parameter is `module_param_unsafe`, so loading it **taints the
     kernel**. Accept that or drop fan readings on this machine.
2. ~~**A `msi-ec` that binds on MS-17L3**~~ **DONE (2026-10-08):**
   `Containerfile.core` builds BeardOverflow `msi-ec` (pinned commit + sha256)
   into `updates/`, signs it with the other out-of-tree modules, and autoloads
   it on MSI hardware through a DMI alias in `51-rime-msi.conf`. Its
   CONF_G2_1 lists 17L3EMS1.109. Verified on the Katana: `rime fan mode max`
   turns Cooler Boost on and `auto` hands it back. The charge window lands as
   70/80: msi-ec keeps start = stop - 10 in one EC byte.
3. **`irqbalance` must not fight game mode.** It re-scatters interrupt affinity
   on its own cadence and will undo the steering within seconds.
   **Recommendation: mask it in the gaming image** (`systemctl mask
   irqbalance.service`), because a static `IRQBALANCE_BANNED_CPULIST` cannot
   track a cpuset rimed computes at runtime. rimed detects a running irqbalance
   and reports it in `rime game status`, but does not try to stop it.
4. **`gamemoded` must not fight rimed for the governor.** The image ships no
   `/etc/gamemode.ini` today, so gamemoded's default `desiredgov=performance`
   writes `scaling_governor` behind rimed's back. Ship `/etc/gamemode.ini` with:
   ```ini
   [general]
   ; rimed owns the governor; do not let gamemoded touch it
   desiredgov=performance
   defaultgov=performance
   igpu_desiredgov=performance

   [custom]
   start=/usr/bin/rime game start
   end=/usr/bin/rime game stop
   ```
   The `[custom]` hooks are the intended integration: gamemoded triggers the
   session and rimed orchestrates it. They run as the requesting user, which
   polkit's `allow_active = yes` already permits without a password.
5. **`nvidia-smi` on PATH** in the gaming image (the NVIDIA driver package's
   `/usr/bin/nvidia-smi`), plus the `nvidia` kernel module loaded. Without it
   game mode silently skips all GPU work. `rime doctor` checks for it when an
   NVIDIA GPU is present.
6. **cgroup v2 unified hierarchy** (systemd default) with the `cpuset`
   controller available. rimed enables `+cpuset` on the parent's
   `cgroup.subtree_control` itself, best-effort.
7. **Optional: dedicated polkit actions.** At M6, fan and game-mode mutations
   reuse `org.rimeos.rimed.manage-power` because
   `files/system/polkit-1/actions/org.rimeos.rimed.policy` is out of scope. If
   you want finer granularity, add `org.rimeos.rimed.manage-fan` and
   `org.rimeos.rimed.manage-game` (same `allow_active = yes` shape) and tell me
   to switch the daemon over.
8. **`rimed.service` needs no Containerfile change.** The unit file itself now
   carries `ProtectControlGroups=no` (required: the default `yes` makes
   `/sys/fs/cgroup` read-only and silently disables all cpuset pinning) and the
   `ExecStopPost` fan restore. It installs from the same path as before.
