# Modes, the workload manager and the Performance Lab

Roadmap §11 (modes), §12 (gaming mode and Performance Lab) and §13 (the
workload-aware performance manager), as shipped.

## `rime mode`: named modes, composed from what already exists

§11 asks for coherent operating modes and says how: *"avoid duplicating the
whole OS for every mode"*, *"use `rimed` as a narrow policy/control plane"*. A
mode therefore adds no hardware lever. It names a combination of three things
the CLI could already do:

| Lever | D-Bus call |
|---|---|
| power tier | `org.rimeos.Rimed1.Power.SetTier` |
| AC/battery auto-switch | `org.rimeos.Rimed1.Power.SetAutoSwitch` |
| game mode (cpuset, IRQ steering, GPU clock locks, sched-ext) | `org.rimeos.Rimed1.GameMode.SetActive` |

Modes changed no frozen member's signature. Since 2026-10-10 the chosen mode
is kept across restarts: `rime mode set` first calls the additive
`org.rimeos.Rimed1.Mode.Hold`, rimed writes `/var/lib/rimed/mode`, and puts that
mode back at every start (Daily = no file). While Gaming is held,
`rime game stop` is refused until another mode is chosen (see
[rimed-dbus.md](rimed-dbus.md)).

```
rime mode list             # the eight modes and the policy each applies
rime mode show gaming      # what it changes, what it only reports, and why
rime mode status           # which mode the machine is in
rime mode set gaming       # apply
rime mode set gaming --dry-run
rime mode set --auto       # apply what `rime workload` measured
```

### The catalogue

| Mode | Tier | Game mode | Intent (§13) |
|---|---|---|---|
| `daily` | auto (profile defaults) | off | none |
| `gaming` | performance (pinned) | **on** | latency |
| `development` | performance (pinned) | off | throughput |
| `creator` | performance (pinned) | off | sustained |
| `ai` | **balanced** (pinned) | off | preserve VRAM |
| `battery` | power-saver (pinned) | off | efficiency |
| `couch` | balanced (pinned) | off | low-power |
| `server` | performance (pinned) | off | throughput |

`ai` pins *balanced* rather than *performance* on purpose. Local inference is
bound by GPU and memory bandwidth, so pinning every core to the performance
governor spends package power the GPU wants and adds no tokens per second.
No kernel interface can reserve VRAM, so Rime cannot either, and the mode
reports VRAM headroom instead of claiming to manage it.

`gaming` is the only mode that turns game mode on. Game mode confines work to
the P-cores, which suits a game and hurts a parallel build.

### There is no mode state file

Rime **derives** the active mode from what rimed reports (tier, auto-switch,
game mode) and stores nothing. That has two consequences, and both are
intended:

* `rime mode set` needs no root. Persisting the mode would have meant a
  root-owned file under `/var/lib`, and `rime`'s root gating already documents
  why a blanket root requirement is wrong for the verbs the desktop's power
  controls drive as the session user.
* The answer cannot go stale. Change the tier by hand and `rime mode status`
  reports it at once, naming the closest mode and the exact difference.

`rime mode status` can report **several** modes at once:

```
mode          : development, creator, server
```

That output is correct. Those three modes pin the same tier with the same
game-mode setting; they differ only in declared intent and in the service sets
they report, and a running machine exposes neither. Collapsing them to one name
would invent a certainty the machine does not have.

### Ordering is load-bearing

`rime mode set` applies its steps in a fixed order. Two of the rules exist
because getting them wrong makes a mode fail to stick, with no error:

1. **Leave game mode first.** `rime game stop` restores the tier that was active
   before the session. If the new mode's tier went first, that restore would
   overwrite it: you ask for Battery Saver and land wherever you were an hour
   ago.
2. **Turn auto-switch off before pinning a tier.** While it is on, rimed
   re-derives the tier from the profile's AC/battery defaults, and enabling it
   reconciles at once.

Mutation-verified tests pin both rules.

### Service sets and system extensions are reported, not applied

§11 lists them among the things a mode "may change". Rime models them so that
`rime mode show` can state the full intent, and `rime mode set` does **not**
move them. Merging a system extension on a mode switch is a heavyweight lever
with its own rebuild service, and `Containerfile.core` masks `irqbalance` on
every image, so a mode that toggled it would fight the image. A declared gap
beats an action that fails without telling you, and `tests/test-rime-modes.sh`
fails if `rime mode` ever spawns `systemctl`.

## `rime workload`: measured signals, and an explained verdict

§13 prescribes how this feature behaves as well as what it does:

> Make automatic choices visible and overrideable.
> Do not market random tuning as AI optimization.
> Use measured workload signals and hardware capabilities.

Three rules follow from that.

**Every signal carries its provenance.** A reading is either measured, with the
path it came from, or unavailable, with the reason. No third state turns a
missing reading into a default. A kernel without PSI says so and names
`CONFIG_PSI`. A machine whose `power_supply` class has no `Mains` object is a
*gap*, not "on battery", because a default in either direction is wrong on half
the fleet.

**A process name never decides anything on its own.** An editor holding a stale
`rustc` is not a build. The render and compile rules need corroboration from a
busy signal measured on its own (PSI `some avg10`, or load per CPU). When
neither can be read, the verdict is `unknown` and the report names the gap.

**The classification is a documented ladder**, most authoritative first:

1. rimed's own game cgroup has processes in it. That is first-party fact:
   rimed put those PIDs there. `steam` is *not* in the process table on
   purpose, because it runs whenever the client is open and would report a
   permanent game session.
2. a game runtime process (gamescope, wine), reported as the weaker signal it is
3. a local inference server, corroborated by VRAM where the driver reports it
4. rendering, 5. compiling: both need a busy signal
6. browsing: browsers present and measured *not* busy
7. idle

The battery row of §13 is a **constraint** layered on top, not a workload. On
battery, efficiency takes precedence, except over a live game session. The
policy leaves that session alone: unwinding something you started on purpose,
without telling you, is the invisible automatic choice §13 prohibits.

### Nothing is applied automatically

Rime ships no timer, no daemon loop and no background auto-apply. `rime
workload` reports; `rime mode set --auto` applies once, when you run it. The
project considered a shipped-but-disabled systemd unit and rejected it, because
the root `AGENTS.md` treats aspirational language presented as implemented as a
defect.

## `rime perf`: the Performance Lab

§12 asks for "frame time, CPU/GPU clocks, power, temperatures, VRAM and
scheduler state". `rime perf` reports all of it, `--json` included, read-only
and without root.

### Frame time is unavailable, and nothing stands in for it

No generic source exists. Frame pacing is a property of a client's swapchain.
The application sees it, an interposed layer such as MangoHud sees it, and a
compositor that exports it can pass it on; a bystander reading sysfs cannot.
Wayland's presentation feedback goes to the *client*.

The row says that, names MangoHud as the way to get a real measurement, and
refuses to substitute GPU busy percentage, a clock, or a frame rate derived
from anything else. A GPU can sit at 99% while a game stutters and at 40% while
it runs smoothly. A Performance Lab that shows a confident number it did not
measure is worse than one with an honest gap.

### Two GPUs are two rows, and the headline follows the discrete one

The lab used to have one set of GPU readings, filled from whichever DRM card
answered first. On a hybrid laptop that is the iGPU. On the MSI Katana `card1`
is Alder Lake-P Iris Xe and `card2` is an RTX 3070, so `rime perf` reported the
iGPU's clock, labelled it "GPU", and never mentioned the card the games run on.

The lab now lists every card with its vendor, its driver and its own readings,
and the headline rows follow the discrete card. Measured on that machine:

```
── GPU ──
 card1        : Intel i915, boot display — clock 0 MHz, busy unavailable — i915
                exposes engine busy through its PMU, not through sysfs
*card2        : NVIDIA nvidia — clock 210 MHz, busy 0%
clock         : 210 MHz
busy          : 0%
vram          : 0.0 / 8.0 GiB (0% used, 8.0 GiB free)
```

The three vendors answer different questions, and the lab says which:

| | utilisation | current clock |
|---|---|---|
| amdgpu | `gpu_busy_percent` | `pp_dpm_sclk`, the `*`-marked level |
| i915 | nothing in sysfs: the PMU, and the row says so | `gt_cur_freq_mhz` |
| nvidia | `nvidia-smi --query-gpu=utilization.gpu` | `nvidia-smi` |

`[gamemode.gpu]` is the AMD and Intel half of what `[gamemode.nvidia]` does for
NVIDIA. `amd_perf_level` writes `power_dpm_force_performance_level`, validated
against the eight values amdgpu accepts: the driver answers an invalid one with
`-EINVAL`, and a refused write looks the same as an applied one.
`intel_floor_percent` raises `gt_min_freq_mhz` to a percentage of the range the
card publishes between `gt_RPn_freq_mhz` and `gt_RP0_freq_mhz`. Both default to
leaving the card alone, both are clamped to limits the hardware reports, and
rimed restores both on exit to the value it read on the way in. It never
restores to a default: a control whose prior value could not be read is left
alone rather than set to a guess.

Two writes are absent on purpose: `pp_od_clk_voltage` and any power-limit
write. Either can hang a card, and there is no hardware here to prove
otherwise on.

### There is no single "package power" figure

There used to be, and it was wrong. The reader took the first hwmon publishing
`power1_*`; on the development ThinkPad that is `hwmon4`, owned by `BAT0`, so
`rime perf` printed **"package: 20.47 W" above a battery row showing the
identical figure from the identical sensor**. Both numbers were real and the
label was a fabrication.

The lab now reports every hwmon power sensor with its chip and label
(`amdgpu/PPT: 10.00 W`) and skips hwmon devices hanging off a `power_supply`,
because the battery row covers them. You can tell from the label whether a
given chip's figure is "the package", which is more than the code could decide
for you.

### Per-machine gaps are named, not hidden

VRAM comes from amdgpu's `mem_info_vram_*` in sysfs. i915/xe publish no total,
and neither does the NVIDIA driver, so the NVIDIA leg goes through an injected
`nvidia-smi` querier. GPU clocks come from `pp_dpm_sclk` (the
`*`-marked active level, not the ceiling), `gt_cur_freq_mhz`, or the querier.
If none applies, the row names the three interfaces it tried.

## Testing, and why this area gets extra care

This area has already caused harm. An earlier game-mode suite applied its plans
through a live writer, which shelled out to `scxctl`, a D-Bus client for
`scx_loader` whose polkit action is **not** passwordless. Running the tests
raised a burst of authentication prompts on the developer's own desktop and
then blocked for 177 seconds waiting on a password, which read as a slow suite
rather than as a test reaching the host. Once authenticated it would have
switched the scheduler of the machine running the tests.

Everything here is built so that cannot happen again:

* `rimed-core::mode`, `::workload` and `::perf` construct **no `SysWriter` of any
  kind**. `mode` performs no I/O at all; the other two are read-only and take
  explicit `/sys` and `/proc` roots so each case runs against a fixture.
* The NVIDIA legs go through the `NvidiaSmi` trait, so tests inject a mock and
  spawn no process.
* `RIME_MODE_NO_APPLY=1` makes `rime mode set` refuse *before* it connects to
  anything. The suite proves that ordering by pointing
  `DBUS_SYSTEM_BUS_ADDRESS` at nothing and checking which message comes out.
* `tests/test-rime-modes.sh` puts fake `scxctl`, `nvidia-smi`, `systemctl`,
  `busctl` and `pkexec` first on PATH and fails if any of them runs. It then
  **calls `scxctl` on purpose** to prove the tripwire was armed, because "no
  spawn detected" is worth nothing if the fakes were never on PATH.

`RealWriter::new()` still runs no host commands; only the daemon's own
constructor does. `pr-validation.yml` enforces that with a static check, and
nothing in this phase weakens it.
