# Gaming Mode, Safe Graphics and the niri session

This document records what the 2026-09-19 katana hardware qualification found
in these three sessions, what Rime changed in response, and the commands a
machine has to run to close the rows that a laptop with one GPU cannot answer.
§5c to §5e and §6.8 follow the sched-ext work on to 2026-09-26.

It is written against `ROADMAP/evidence/katana-qualification-20260919.md`.
Section numbers below are that file's.

---

## 1. Gaming Mode opens the card the monitor is on (§6.1)

### What it did

`rime-gaming-session` passed gamescope no device preference. gamescope took its
default, the first DRM node, which on a hybrid laptop is the integrated GPU.
On the MSI Katana that is `card1`, an Intel Iris Xe whose only connector is the
laptop panel. The RTX 3070 driving the user's only external monitor is `card2`,
and gamescope never looked at it.

### The rule, and why it is this one

**The output decides the card.** A rule of "prefer the discrete GPU" would fail
worse than the defect it replaces: a discrete GPU with no connector attached
gives a session with no screen.

1. Every connector under `/sys/class/drm/card*-*` whose `status` is
   `connected`.
2. External beats internal. `eDP-*`, `LVDS-*` and `DSI-*` are the panel built
   into the chassis; everything else is a cable somebody plugged in. This is
   also what makes the docked-with-the-lid-shut case work, because `eDP-1` is
   still `connected` there.
3. Among equals, by connector name, so two runs on one machine cannot
   disagree. Sysfs cannot tell you which of two monitors is "the" gaming
   monitor.
4. The chosen connector's card supplies `--prefer-vk-device <vendor>:<device>`,
   read from `device/vendor` and `device/device`.

That rule needs no special case for a single-GPU machine or an all-AMD one: on
those, every connector belongs to the only card, so it emits that card's id
(the one gamescope would have picked anyway) plus a `--prefer-output` that
still moves the session onto the monitor rather than the panel. The code
hardcodes nothing about `10de:249d`.

The flag is `--prefer-vk-device` because gamescope's DRM backend is its own,
not wlroots', and it ignored `WLR_DRM_DEVICES` when the qualification tried it
(§6.1, second attempt). Measured: `--prefer-vk-device` moves the DRM node as
well as the Vulkan device.

### Where it lives

| | |
|---|---|
| the rule | `rimed/rimed-core/src/gpu.rs`, `choose_display` |
| the tests | `rimed/rimed-core/tests/gpu_parity.rs` |
| the caller | `rime gaming --gamescope-device-args` |
| the consumer | `files/system/libexec/rime-gaming-session` |

### Failing loudly

A silent fallback to the iGPU is the defect this fixes, so no path is silent.
Every case that cannot produce a complete answer (no connectors, nothing
connected, a card with no PCI id, a connector whose `status` could not be read)
sets a problem string that the session prints and `rime gaming` raises as a
warning.

It is **not fatal**, by design. Refusing to start would regress every
single-GPU machine over a probe that hiccupped, and gamescope's own default is
correct on those.

The session uses a **partial** answer rather than discarding it. `rime gaming
--gamescope-device-args` exits non-zero when it could not produce a complete
answer (a screen *and* the card to drive it with). The commonest cause is a DRM
node with no PCI device behind it, where `--prefer-output` is good and only
`--prefer-vk-device` is missing. Throwing the good half away would add a second
silent regression to the one being fixed. "Fail loudly" requires the session to
state the gap. It does not require the session to get less than it could have.

`RIME_GAMING_NO_DEVICE_SELECT=1` restores the old behaviour, and says what it
is giving up. The session appends `RIME_GAMESCOPE_ARGS` after the computed
flags, so a hand-set preference wins.

---

## 2. `--rt` is only claimed when it can be granted (§6.2)

gamescope gates its realtime path on **CAP_SYS_NICE**, not on
`RLIMIT_RTPRIO`. With `/etc/security/limits.d/30-rime-gaming-rtprio.conf`
installed and working (the soft limit read 20), the session passed `--rt`, and
gamescope answered

```
No CAP_SYS_NICE, falling back to regular-priority compute and threads.
```

on every attempt. The session now checks the capability (its own `CapEff`, or a
file capability on the gamescope binary) and passes `--rt` only when one holds.
`rime gaming` gained a `realtime capability` row beside the existing `realtime
limit` one. They are separate warnings because a machine can have the limit and
not the capability, and every Rime machine is in that state today.

### Why nothing grants it

* **`setcap` cannot work.** Rime's `/usr` is a read-only composefs, and
  gamescope arrives through a `systemd-sysext` overlay whose lower layers are
  read-only too. There is no writable inode to hang the `security.capability`
  xattr on.
* **`pam_cap` would work, and would break Steam.** It is the direct analogue of
  the limits.d drop-in, and it would put capabilities into the **permitted**
  set of every process in the session. That includes Steam's own `bwrap`,
  which refuses to start with any of them: *"Unexpected capabilities but not
  setuid, old file caps config?"*. That is the message §6.3 recorded and could
  not attribute. Granting CAP_SYS_NICE at login would trade a frame-pacing
  regression for a Steam that does not start.

If a future gamescope RPM ships `cap_sys_nice=ep` on the binary, the session's
`getcap` probe finds it and `--rt` comes back with no change to any of this.
The Steam interaction above does not arise then, because a file capability
applies to gamescope's own exec and not to the session that started it.

---

## 3. The capability sets are logged, so §6.3 can be attributed next time

`rime-gaming-session` logs `CapEff`, `CapPrm` and `CapAmb` at every start, and
says so when the permitted set is non-empty, naming what that breaks. The
qualification saw §6.3's bwrap failure without finding its cause. It did
**not** log in through greetd: it used `systemd-run --property=PAMName=login` on
VT 2 (§4), which left open whether the non-empty permitted set came from the
harness or the image.

The next run answered it from the session log: the harness. A greetd login
gives the session `CapPrm=0`, and Steam's requirements check passes (katana,
2026-09-19, §0.6 and §0.7 of `ROADMAP/evidence/katana-image-qual-20260919.md`).

---

## 3a. Adaptive sync is asked about the right screen, and the silence is named (§7.2)

`vrr_capable` does not exist anywhere on katana. The NVIDIA connector publishes
seven sysfs attributes and that is not one of them; the Intel connector
publishes fifteen and also lacks it. The old probe globbed every connector on
the machine, matched nothing, passed nothing and **said nothing**. On a 240 Hz
monitor, "this machine has no VRR" and "this driver does not publish the
property" therefore produced identical silence, although only the first is
about the hardware.

The probe now lives in `choose_display` beside the screen it asks about, and it
asks only about the connector this session will use. The glob had two faults,
and the second is subtler:

* It would have enabled adaptive sync on the strength of the laptop panel while
  the session ran on the monitor.
* **DRM connector names are unique per card, not per machine.** A hybrid laptop
  has `card1-HDMI-A-1` (the iGPU's own port, usually wired to nothing) *and*
  `card2-HDMI-A-1`. Anything that resolves a connector by name alone answers
  about whichever sorts first: the disconnected one on the wrong GPU. That is
  §6.1's mistake again, and it is why the fixtures now carry the namesake
  connector.

The selector logs and reports four distinct outcomes: the output says `1`, the
output says `0`, the output publishes nothing while other connectors here do,
and nothing on this machine publishes the property at all.

`--adaptive-sync` therefore arrives inside the same `--gamescope-device-args`
output as the device flags, from the same selector, and the session script
holds no DRM logic of its own.

```sh
for p in /sys/class/drm/*/vrr_capable; do [ -e "$p" ] && echo "$p = $(cat "$p")"; done
rime gaming | grep 'adaptive sync'
```

Empty on katana, and on the L16 too: amdgpu does not publish it for `eDP-1`
there either, so `rime gaming` says `not published` rather than `no`. If VRR
matters for Gaming Mode on NVIDIA, the property has to come from somewhere
other than this sysfs attribute. That is separate work, and this row stays
COULD NOT RUN.

---

## 4. Safe Graphics and a screen on the second GPU (§5.5)

The pixman software renderer cannot import DMA-BUFs, and wlroots' multi-GPU
path needs that import to feed a secondary card's connector. On katana the
recovery session painted the laptop panel and left the external monitor dark,
with 60 `Swapchain for output 'HDMI-A-1' failed test` errors against zero for
`eDP-1`.

`rime-safe-graphics` now:

* names the primary GPU, the outputs it can light and the outputs it cannot,
  in `check` and in its own log, before the compositor starts;
* when every connected screen is on one **non-primary** card, points wlroots at
  that card with `WLR_DRM_DEVICES`. One device means no import, so software
  rendering drives it. labwc *is* wlroots and honours the variable, unlike
  gamescope (§6.1);
* keeps the primary in the mixed case (a live panel and a monitor on the other
  card), because no single device choice can light both, and says which screen
  it is about to leave dark plus the override that recovers on the other one.

`RIME_SAFE_GRAPHICS_DRM_DEVICE=/dev/dri/cardN` forces a device.
`RIME_SAFE_GRAPHICS_RENDERER=auto` lets wlroots pick a hardware renderer. It is
an opt-out and warns what it undoes, because "the GPU does not work" is the
case this session exists for.

---

## 5. The niri session and niri's own default config (§5.4)

niri's upstream `default-config.kdl` carries `spawn-at-startup "waybar"`, and
that file is what lands in `~/.config/niri/config.kdl`, whether niri writes it
on first run or `rime-shell-firstrun` copies it. firstrun then appended the
Rime autostarts, which start quickshell, so every login got two bars.

`rime-shell-firstrun` now disables that one line, matched in full at column 0
and only in upstream's own spelling, guarded by a marker, a backup, `niri
validate` before and after, and a whole-file hash comparison with that line
removed.

### What else the upstream default turns on (recorded, not changed)

Rime left these **alone**. They are user-visible defaults, and changing them is
Andre's decision rather than a side effect of fixing a bar.

| line | what it does | why it is worth a decision |
|---|---|---|
| `hotkey-overlay { // skip-at-startup }` | the "Important Hotkeys" pop-up on every niri start | Rime Shell has its own keybind UI; Hyprland and labwc sessions show nothing equivalent |
| `Mod+T { spawn "alacritty"; }` | terminal | Rime's own terminal choice is foot-first (see `rime-safe-graphics`) |
| `Mod+D { spawn "fuzzel"; }` | launcher | Rime Shell has a launcher |
| `Super+Alt+L { spawn "swaylock"; }` | lock | Rime's lock is compositor-enforced (§5.3); swaylock may not be installed, so this is a bind that does nothing |
| `Super+Alt+S { spawn-sh "pkill orca \|\| exec orca"; }` | screen reader | Rime ships `rime-screen-reader` |

**The binds matter more than they look.** `RimeShellKeybinds.kdl` is created
*empty* by firstrun and stays empty until Rime Settings writes it, so the
`include` that is supposed to override these overrides nothing on a fresh
machine: the stock binds are the only binds there are.

---

## 5a. Leaving Gaming Mode is rimed's job, not the session's

### What it did

`rime-gaming-session` released game mode from an `EXIT` trap that ran
`rime game stop`. On katana 2026-09-19 that worked on a clean gamescope exit
and did nothing at all when someone restarted greetd underneath it: `rime game
status` still read `active: true` seventy-five minutes later, with a p-core
cpuset, steered IRQs and the `performance` tier still in force, and **not one
line in the session's own log** said so.

> **This account first listed `scx_lavd` as a fourth thing left running. That
> was false, and the correction stays here instead of a silent deletion**
> (2026-09-20). No sched-ext scheduler was running then, or on any boot between
> the feature landing and that date. `rime game status` said one was because the
> only sched-ext thing it reported was a sentence copied out of the plan, while
> `scxctl` had refused every call. See §5c.

The trap has no bug. `rime game stop` goes through polkit action
`org.rimeos.rimed.manage-power`, whose defaults are

```
allow_any      auth_admin
allow_inactive auth_admin
allow_active   yes
```

The instant logind stops calling the session *active* (a greetd restart, a VT
switch away, any logind-driven teardown), polkit refuses the session's own
call. Measured from a session with no seat:

```
$ rime game stop
rime: leaving game mode failed: org.freedesktop.DBus.Error.AccessDenied:
      not authorized for org.rimeos.rimed.manage-power
$ sudo rime game stop
rime: game mode OFF
```

The process that is *supposed* to clean up loses the privilege to do it at the
moment it needs it.

### Why the polkit rule was not loosened

Giving the release path an `allow_inactive=yes` of its own is one line and it
is the wrong line. `allow_inactive` is every local session, active or not, so
any unprivileged user on the machine could switch Gaming Mode off while someone
else is playing. It still would not close the case it exists for: a session
that is `SIGKILL`ed runs no trap at all, so there is no call to authorise.

**The process that has to ask may already be dead**, and changing who may ask
does nothing about that.

### The rule, and where it lives

`rimed` is root, holds the session's exit plan in memory, and asks polkit
nothing about itself. It is the only party that still exists after the session
dies, so it takes the job:

* `GameMode.StartOwnedBy(owner_pid)` enters game mode **and** records the
  process whose death ends the session. It uses the same polkit action as
  before, so *entering* Gaming Mode is as restricted as it was.
* A 2 s watch in `rimed/src/main.rs` reads `/proc/<pid>/stat`; when the owner is
  gone, rimed calls the same idempotent `game_exit()` a D-Bus request would,
  and emits the same signals, so `rime game status` and rime-shell do not keep
  showing a session that is over.
* `rime-gaming-session` passes `--owner-pid $$`.

Three of the details are decisions rather than mechanics:

* **A PID is not enough; the start time is recorded with it.** PIDs are reused.
  A bare-PID watch would keep game mode engaged for as long as some unrelated
  process held the number: the same failure, quieter. `starttime` (field 22 of
  `/proc/<pid>/stat`, and read by splitting after the **last** `)`, because
  `comm` may contain spaces and parentheses) is the kernel's own tiebreaker.
* **An unreadable `/proc` is a third answer, and never a release.** Turning an
  I/O error into a hardware change is not a fail-safe. rimed logs it once and
  leaves the session alone.
* **The owner is watched, not pinned.** Putting the session script in the game
  cpuset would put every Steam process on the p-cores, which is a behaviour
  change nothing has measured. `StartForPid`/`AttachPid` remain the way to pin.

The `EXIT` trap is still there, and is now the *second* of two paths: it is
instant on the clean exit, where the watch would take up to a tick longer, and
both call the same idempotent release, so a race between them is harmless. It
can no longer fail in silence: it prints polkit's own message and names what
will release the machine instead.

## 5b. MangoHud and `--expose-wayland` cannot both be on

### What it did

`mangoapp` crash-looped at about 2 Hz for the whole of **every** Gaming Mode
session on katana: 15 376 core dumps in one boot, 14 403 respawns and 57 612
GLFW lines in a single two-hour session (144 187 journal lines for one login),
and 4.0 GB of stored core dumps on a `/var` that was already 94 % full. The
overlay never drew a pixel.

### The cause, from the dumps and then from an A/B

The backtrace off the kept core dump is two frames long:

```
#0  XInternAtom (libX11.so.6 + 0x17c50)
#1  main (mangoapp + 0x1f79f)
```

That is `XInternAtom(NULL, …)`. The session log lines before it name the
reason: `libdecor` plugin failures, which only GLFW's **Wayland** backend loads,
and four `Glfw Error 65550: X11: Platform not initialized` lines, which is
`glfwGetX11Display()` refusing on a non-X11 platform and returning NULL.

A controlled A/B on the same machine, headless, same binaries, one variable:

| gamescope flags | `mangoapp` environment | result in 22 s |
|---|---|---|
| `--mangoapp` | `DISPLAY=:0`, `GAMESCOPE_WAYLAND_DISPLAY=gamescope-0` | same pid alive throughout, **0 restarts** |
| `--expose-wayland --mangoapp` | the same **plus `WAYLAND_DISPLAY=gamescope-0`** | **171 restarts** |

`--expose-wayland` puts `WAYLAND_DISPLAY` into the environment of gamescope's
children. GLFW auto-selects its Wayland backend whenever that variable is set,
and mangoapp then hands the NULL X11 display it gets back straight to Xlib.

### The choice

`--expose-wayland` stays and `--mangoapp` goes. Native Wayland (xdg-shell)
games are worth more than an overlay that has never rendered on this system,
§6.1 and §6.3 were qualified on hardware **with** `--expose-wayland`, and the
overlay was costing a crash loop for nothing.

The session gates `--mangoapp` on the flag rather than deleting it, so
`RIME_GAMING_EXPOSE_WAYLAND=0` brings the overlay back by itself. That is also
what makes the gate testable in both directions in
`tests/test-rime-gaming-session.sh`.

Neither half is Rime's to fix: mangoapp should ask GLFW for the X11 platform
(or refuse to dereference a NULL `Display`), and `gamescopereaper --respawn`
has no backoff. Rime records both rather than working around them. Rime *does*
own the risk that a 2 Hz crasher takes `/var` with it, and
`files/system/coredump/50-rime-coredump-limits.conf` bounds that separately.

## 5c. Gaming Mode had never loaded a sched-ext scheduler, and status said it had

### What it did

Three shipped images logged this on every boot, directly above the line the
status surface quoted:

```
rimed: scxctl switch -s scx_lavd failed (exit status: 1):
       error: no scx scheduler running, use 'start' instead of 'switch'
rimed: game: sched-ext: scx_lavd for the session, kernel scheduler restored on exit
```

The second line was `rime game status`'s only sched-ext output. It asserted as
fact the thing the line above it had reported as failed, because it was a
sentence lifted out of the *plan*, printed whether or not the plan had done
anything.

Found on katana 2026-09-20 while qualifying §6.6. The same lines are in
`journalctl -b -1` and `-b -2`, so it was not a regression: it had never
worked. `scxctl` and `scx_lavd` were installed (`scx-scheds-1.1.3-3.fc43`),
`scx_loader.service` was active, and `/sys/kernel/sched_ext/nr_rejected` and
`enable_seq` were both 0. The kernel never rejected a scheduler, because
nothing ever offered one.

### The cause: two verbs that are not interchangeable

`scxctl` has both, and each refuses in the other's state, in as many words:

| state | `start` | `switch` |
|---|---|---|
| nothing attached | attaches it | `error: no scx scheduler running, use 'start' instead of 'switch'` |
| one attached | `error: scx scheduler already running, use 'switch' instead of 'start'` | replaces it |

Rime loads no scheduler at boot, so the first entry into Gaming Mode always
finds none, and the engine hardcoded `switch`.

### The rule, and why it is not "use `start` instead"

Swapping one hardcoded verb for the other would work on the machines Rime ships
today and fail on any machine that already runs a scheduler. The engine
therefore **reads the verb off the kernel**: `/sys/kernel/sched_ext/state`
decides, and a single retry on whichever verb `scx_loader`'s own error names
covers both the race and the case where the state could not be read. One
retry, not a loop.

The half that cost three images: **`scxctl` exiting 0 is a fact about
`scxctl`.** Whether a BPF scheduler is attached is a fact about the kernel.
After a successful call the daemon waits, bounded at 2 s, for
`sched_ext/state` to reach `enabled`, then reports **what it read**:

* `scx_state : loaded` means the kernel says a scheduler is attached.
  `scx_detail` quotes `root/ops`.
* `scx_state : not loaded` means nothing is attached, or this kernel has no
  `CONFIG_SCHED_CLASS_EXT`. Both are definite answers. **A `scxctl` that
  exited 0 over a kernel that still reads `disabled` lands here**, which is
  the general form of the defect.
* `scx_state : unknown` means `state` was unreadable, or mid-transition. The
  daemon never rounds it to either of the others: a failed read is not an
  absent feature.

`scx_detail` names both halves, so a disagreement between the command and the
kernel stays visible instead of being resolved in silence. `rime game status`
reports the keys while game mode is **off** as well, so "disabled before,
disabled during" reads as the non-answer it is rather than as a passing row.

> **`not loaded` was the answer on every Rime image before the Rime kernel
> tier, and it was not this fix failing.** The COPR kernel's BTF could not
> accept a sched-ext scheduler at all. A fourth key, `scx_btf`, says which kind
> of `not loaded` it is: **read §5d before reading anything into `scx_state` on
> a real machine.**

> **`root/ops` is the struct_ops name without the `scx_` prefix, followed by
> the scx build ID.** On katana (2026-09-22) `scx_lavd` attached as
> `lavd_1.1.3_x86_64_unknown_linux_gnu` and `scx_rusty` as
> `rusty_1.1.3_x86_64_unknown_linux_gnu`. The first version of the comparison
> expected the bare name (`lavd`) and so reported every correct load as the
> wrong scheduler (this defect inverted). `scx_ops_matches()` now strips `scx_`
> and accepts the bare name, or the name followed by `_` and a version. A
> genuine mismatch is *reported* rather than treated as "not loaded".

### The same shape, found next door

The defect is "a command whose result is assumed rather than read", so the
sibling writers got the same check. One more had it: **`gpus_locked` in
`rime game status` was the list of GPUs the plan MEANT to lock**, while
`run_nvidia_smi` had been returning a valid refusal that nothing read. It now
reports the GPUs whose locks `nvidia-smi` accepted, with `gpus_lock_attempted`
beside it. And the **exit** path discarded every outcome
but a hard error, so a refused restore was silent underneath a line asserting
the machine had been put back.

### Where it lives

* `rimed/rimed-core/src/syswriter.rs`: `scx_load`, `scx_stop`,
  `read_scx_state`, and `Outcome::Unknown`, the third answer the writer had
  nowhere to put before.
* `rimed/rimed/src/game.rs`: `ScxReport`, `GpuLockReport`, and the `scx_*`
  keys in `Status`.
* Verified without hardware: 17 tests drive a fixture sysfs and a fake
  `scxctl` that can be honest, refuse either way, or exit 0 and change nothing;
  10 more pin what `rime game status` says in each state. Nothing in the suite
  can reach a real scheduler. The fixture constructor is `#[cfg(test)]`, and
  the host-command guard exists because a live writer in a test once reached
  the developer's own.

## 5d. No sched-ext scheduler could load on the COPR kernel at all

> **Fixed since (2026-09-22).** Rime now builds its own kernel,
> `7.2.6-cachyos1.rime1` (`Containerfile.kernel`, `kernel-build.yml`), and
> `Containerfile.core` refuses any kernel whose manifest does not say
> `btf_scx=usable`, with no fallback to COPR. On that kernel `scx_btf` reads
> `ok` and schedulers attach: `scx_rustland` by hand
> (`ROADMAP/evidence/katana-schedext-fixed-20260922.md`) and `scx_lavd` through
> rimed (§6.8 Row A, `ROADMAP/evidence/katana-final-qual-20260922.md`). The
> account below is the 2026-09-20 one, when every image shipped COPR's
> `kernel-cachyos`.

§5c fixed the verb and made `rime game status` stop claiming a scheduler the
kernel says is not there. On hardware, the honest answer it then gave was
`not loaded`, **on every Rime image up to that date**, for a reason that is
neither the verb nor the settle budget.

Measured on katana 2026-09-20, `7.2.6-cachyos1.fc43.x86_64`, from
`journalctl -u scx_loader`:

```text
libbpf: extern (func ksym) 'scx_bpf_create_dsq': func_proto [1864]
        incompatible with vmlinux [60823]
libbpf: failed to load BPF skeleton 'bpf_bpf': -EINVAL
Error: the running kernel's BTF has malformed scx kfunc prototype(s):
  scx_bpf_cidperf_cap, … scx_bpf_create_dsq, … scx_bpf_kick_cpu, …
  (22 names)
```

Type `60823` in that kernel's own BTF reads

```text
s32 scx_bpf_create_dsq(u64 dsq_id, s32 node, const struct bpf_prog_aux *aux)
```

The third parameter is the **verifier's implicit argument**. A kfunc marked
`KF_IMPLICIT_ARGS` is supposed to have it stripped from the public prototype by
`resolve_btfids`, which finds the kfunc through a `BTF_KIND_DECL_TAG` valued
`bpf_kfunc` that `pahole` emits. On this kernel **22 of 68 `scx_bpf_*` kfuncs
carry no such tag**, so the strip never happened for them, and libbpf rejects
every scheduler that references one of the 22, which is all of them.

`nr_rejected` stays `0` throughout: the kernel never sees an attach to reject,
because the BPF program will not load. `SCX_SETTLE` is not the cause either:
`sched_ext/state` never read `enabling`.

**Rime could not fix this on 2026-09-20.** It did not build a kernel then:
`Containerfile.core` stage 1 installed the prebuilt `kernel-cachyos` RPM from
COPR `bieszczaders/kernel-cachyos`. (`kernel/**` in this repository was the M0
spike that *chose* that kernel, and built nothing that shipped.) The full
working, including why the "built with pahole < 1.26" explanation `scx_utils`
prints is wrong for this kernel and what would have to happen upstream, is at
`ROADMAP/evidence/kernel-btf-scx-20260920.md`.

### What the status surface does about it

A fourth key, **`scx_btf`**, reporting a reading of `/sys/kernel/btf/vmlinux`
taken by `rimed` itself:

* `ok`: the sched-ext kfunc prototypes are the shape a BPF scheduler expects.
* `implicit-args`: one or more still carry `struct bpf_prog_aux *`. **No
  scheduler can load on this kernel**, and `scx_detail` says so in words,
  naming a kfunc and the proportion.
* `no-sched-ext`: the BTF parsed and carries no `scx_bpf_*` kfunc at all.
* `absent`: `/sys/kernel/btf/vmlinux` is not there.
* `unreadable`: it is there and could not be read or parsed. **Not folded
  into `absent`**, and it blames nothing: a probe that cannot see has not seen
  a broken kernel.

rimed appends the clause to `scx_detail` only when the probe says loading is
blocked **and** the kernel did not end up with a scheduler attached. It does
not argue with a `loaded` session, and a kernel with nothing wrong gets no
sentence. The keys are reported while game mode is **off** too, so you have the
answer before you start a session that cannot work.

> **With `scx_btf`, `not loaded` tells you what to do.** `not loaded` on a
> kernel that could take a scheduler is a bug report about Rime. `not loaded`
> with `scx_btf : implicit-args` is a kernel to replace, and no amount of
> retrying, reconfiguring or reinstalling will move it.

`rime game status`'s **daemon-not-running** branch prints `scx` and `scx_btf`
as well. It is a local view assembled by the CLI, not the daemon's `Status`
map, and it used to say nothing about sched-ext at all.

`rimed/rimed-core/src/kernelbtf.rs` is the reader: a bounded BTF parser with no
dependency, rooted at `sys_root` like `read_scx_state`, so every answer is
reachable from a temp directory. It was checked against fixtures and against
**three** real kernels:

| kernel | version | `scx_bpf_*` kfuncs affected |
|---|---|---|
| L16 (`kernel-cachyos`) | `7.2.3-cachyos2.fc43` | 20 of 68 |
| katana (`kernel-cachyos`) | `7.2.6-cachyos1.fc43` | **22 of 68** |
| Fedora stock (`kernel-core`) | `7.2.6-100.fc43` | 18 of 68 |

katana's 22 are **the same 22 `libbpf` named**, which validates the reader.
The other two rows make the problem general: three kernels, three different
subsets, all broken, all three including `scx_bpf_get_idle_cpumask`. Neither
"boot Fedora's kernel" nor "pin an older CachyOS kernel" is the workaround it
looks like.


## 5e. Once it could load, COPR's scx_lavd stalled the game, so Rime builds the fixed one (2026-09-26)

With the `rime1` kernel's BTF fixed (§5d), sched-ext attached, and the first
real game session on it froze. Terraria (tModLoader), katana,
`7.2.6-cachyos1.rime1`, `scx-scheds-1.1.3-3.fc43` from the CachyOS COPR:

| time (AWST) | kernel: "runnable task stall" | starved |
|---|---|---|
| 08:03:46 | `fossilize_repla` | 35.3 s |
| 08:10:58 | `.NET TP Worker` | 34.9 s |
| 08:12:33 | `rime` | 37.4 s |
| 08:15:11 | `dotnet` | 32.4 s |
| 08:19:19 | `.NET TP Worker` | 31.5 s; scx_loader gives up, "attempt 5/5" |

Plus Firefox (`IPC I/O Child`, 41.4 s) the night before. Every loader dump has
the same shape:

```
R dotnet[50542] *root -32375ms
    dsq_id=0x8
    cpus=00004 no_mig=1
    \_ cpdom_id: 0   scpu: 8
```

The task is migration-disabled on CPU 2 (`cpus=00004 no_mig=1`) but queued on
the per-CPU DSQ of CPU 8, its cached suggested CPU (`scpu: 8`). CPU 8 cannot run
it and CPU 2 never looks there, so it waits until the watchdog ejects the whole
scheduler. That is **sched-ext/scx#3791**, and upstream commit `6d31ddd89`
fixes it: lavd_enqueue's REENQ path now checks the cached CPU against
`cpus_ptr`. Arch shipped that as `scx-scheds 1.1.3-2`. COPR's `1.1.3-3` is the
v1.1.3 tag plus hotfixes for scx_cake and scx_pandemonium, and lacks this one.

**What Rime does:** `Containerfile.core`'s toolbuilder builds scx_lavd from
the same v1.1.3 tag with the vendored patch
(`files/system/src/scx/lavd-6d31ddd89-reenq-cpus-ptr.patch`), and the final
stage installs it over COPR's binary only while COPR is still at 1.1.3. Any
later scx release already contains the fix, so a newer COPR is kept and the
log says to drop the rebuild. `/usr/lib/rime-scx-versions` records which
binary shipped. The patched binary's embedded BPF line info carries the fix
(one more `bpf_cpumask_first(p->cpus_ptr)` than COPR's: 4 against 3).

Gaming Mode keeps sched_ext: rimed still asks for `scx_lavd`
(`rimed/rimed-core/src/profile.rs`), and the image now ships the fixed build.
(Since 2026-10-07 it asks only on CPUs with one kind of core: see §5f.)

`scx_loader` restarting lavd after every watchdog exit turned one bug into five
freezes, and rimed reports "sched-ext loaded" once at entry and never looks
again. Both are still true, and each needs its own fix.

The Hyprland crash the same day is a different defect. It was an i915 GPU hang
in the game's own context (`ecode 12:1:84dffffb`, rcs0), which reset Hyprland's
context too because both ran on the Intel iGPU. It reproduced in daily mode with
sched-ext off. The cause was Rime Shell's launcher ignoring Steam's
`PrefersNonDefaultGPU=true`. rime-shell PR #26 fixed it: `DesktopExec` now
starts such entries through `switcherooctl launch`
(`src/scripts/desktop-launch.sh`), so Steam and the games it starts run on the
discrete GPU.


## 5f. Gaming Mode was slower than the desktop: lavd's E-cores, a capped GPU, and an empty cpuset (2026-10-07)

Cyberpunk 2077 ran at a lower, choppier frame rate in Gaming Mode than on the
desktop on katana (2026.10.07, F45). Read live from the running session, with
nothing in it changed:

- **CPU-bound.** GPU busy 48 % (`clocks_event_reasons` = idle). The game's
  `GameThread` ran 4,895 ms of a 5 s window (98 %), from
  `/proc/PID/task/*/schedstat` deltas.
- **scx_lavd ran that thread on an E-core in 65 of 200 samples** (field 39 of
  `/proc/PID/task/TID/stat` every 25 ms) and moved it across all 20 CPUs in 5 s.
  The source says why (scx v1.1.3, `scheds/rust/scx_lavd/src/bpf/`). On a
  hybrid CPU, `is_perf_cri()` sends a thread to a big core only when its
  `perf_cri` is above a threshold that splits capacity between big and little.
  `perf_cri` is `log2(wait_freq × wake_freq) + log2(runtime × run_freq)`
  (`lat_cri.bpf.c`). It rewards threads that sleep and wake others often, and a
  game's bottleneck thread rarely sleeps. The CPU is chosen again at every
  wake-up. `--performance` only turns off core compaction
  (`update_thr_perf_cri` still splits), and 1.1.3 has no option that turns the
  split off.
- **The P-core cpuset never held the game.** `rime-gaming-session` runs
  `rime game start --owner-pid $$` with no `--pid`, and nothing calls
  `AttachPid`. `rime game status` showed cpus 0-11 with no pids, and the game
  sat in `session-N.scope` with Cpus_allowed 0-19. Even so, the plan moved 47
  interrupts onto CPUs 12-19, the E-cores the game's threads were also using.
- **The profile would cap the GPU.** katana's profile locked graphics to
  `[1200, 1620]` MHz, taking 1620 MHz (the part's rated boost) for its ceiling,
  while `clocks.max.graphics` is 2100 MHz. In this session the lock was not
  applied (`gpus_lock_attempted: 0`), so it did not cause this slowdown. Why it
  was not applied, the profile match or nvidia-smi's answer at enter, was not
  read.

The desktop runs the kernel's scheduler on all 20 CPUs, with no clock lock and
no interrupt steering. lavd had never been benchmarked against that on katana.
It was turned on because it is designed for gaming.

**What changed:**

- `scx` defaults to `auto`: scx_lavd on a CPU with one kind of core, where lavd
  treats every thread as performance-critical, and the kernel's own scheduler on
  a P/E hybrid. A profile that names a scheduler still gets it. katana's profile
  says `auto` and records the measurement.
- katana's `[gamemode.nvidia]` locks no clocks. The card boosts as it does on
  the desktop. On this chassis the CPU and GPU share one cooler, so holding the
  GPU's clocks up while a game waits on the CPU would cost the CPU its headroom.
- Interrupts move only when a game is in the cpuset: at enter for
  `rime game start --pid`, at the first attach otherwise
  (`game::steer_on_attach`). A Gaming Mode session leaves them where they are.

Gaming Mode does not confine the game to the P-cores. Cyberpunk starts 20
worker threads, and 12 logical CPUs instead of 20 is a different machine for
it. The sign of that change was never measured, so the knob stays and nothing
in Gaming Mode uses it.

Not Rime's to fix: Steam turned gamescope's HDR on (gamescope then composites
every frame, 14-16 % of the GPU), and its frame limiter at 240 = refresh forces
FIFO. Both are Steam settings, not Rime's.


## 5g. Discord activity in Gaming Mode (Equibop's Rich Presence, 2026-10-09)

Asked for: with Equibop installed and its Rich Presence on, what you play in
Gaming Mode shows on your Discord profile, without the rest of Equibop.

### What "only Rich Presence" can mean

Equibop's Rich Presence is arRPC (`resources/arrpc/arrpc`, arRPC-Bun 1.4.0 in
Equibop 3.3.1): the `discord-ipc-0` socket games talk to, plus a `/proc` scan
that recognises known games, Proton ones included. arRPC has no account. It
sends each activity over a local WebSocket to Equibop, and Equibop hands it to
the signed-in Discord page (`src/main/arrpc/index.ts`:
`mainWin.webContents.send(IpcEvents.ARRPC_ACTIVITY, …)`). Without that page
nothing reaches Discord, and anything else that posted a presence would have to
log in with the account's token, which Discord bans accounts for.

So Gaming Mode starts Equibop itself, with nothing of it on screen or audible.

### How it runs

`/usr/libexec/rime-gaming-discord run --owner <Steam's pid>`, started by
`rime-gamescope-steam` next to Steam:

- the launcher the user has: the `equibop*.desktop` entry's `Exec` (so a
  wrapper like katana's `~/.local/bin/equibop`, which sets `ARRPC_DATA_DIR`,
  is kept), else `~/.local/bin/equibop` or `equibop` on PATH, or the Flathub
  build `io.github.equicord.equibop` (its socket under
  `$XDG_RUNTIME_DIR/app/<id>/` is linked to `$XDG_RUNTIME_DIR/discord-ipc-0`
  when nothing is there);
- `--start-minimized --ozone-platform=x11 --disable-gpu`; `SteamOS` and
  `SteamGamepadUI` removed (with `XDG_CURRENT_DESKTOP=gamescope` they make
  Equibop go full screen over Steam, `src/main/utils/steamOS.ts`);
  `PULSE_SERVER` pointed nowhere (Flatpak: `--nosocket=pulseaudio`);
- nice 10 with RLIMIT_NICE at most 10 (only ever lowered: a hard limit of 0,
  the kernel default, cannot be raised). With only a relative nice, Chromium
  put its browser, GPU and arRPC processes back at nice −8 (measured in
  gamescope);
- stopped (process group TERM, then KILL) once Steam exits, polled every
  0.5 s (measured: 1.6 s from Steam exiting to the last Equibop process gone),
  and by `rime-gaming-session`'s cleanup, so the desktop's own Equibop starts
  normally afterwards. Its saved window size (`state.json` `windowBounds`,
  `maximized`, `minimized`) is put back: the hidden run would otherwise leave
  gamescope's screen size there.

It refuses, with the reason in the session log and in Settings → Gaming, when
Equibop is missing, arRPC is off in Equibop (`settings.json` `arRPC`), Equibop
was never opened (`state.json` has no `firstLaunch: false`: its first run opens
a welcome window, and in a headless gamescope that window took the focus), or
Equibop is already running (a live `SingletonLock`: a second launch only tells
the first to show its window).

The setting is `discord_presence` in `~/.config/rime/gaming.json`, off until
turned on: `rime-gaming-discord set on|off`, or the switch in Settings → Gaming.

### Measured (L16, Equibop 3.3.1 tarball, headless gamescope, signed-out profile)

- `GAMESCOPE_FOCUSABLE_WINDOWS` empty for the whole run with first launch done;
  arRPC up and `discord-ipc-0` created within ~10 s.
- A test client's `SET_ACTIVITY` reached arRPC, the bridge, and Equibop's main
  process (`[arRPC > debug] Received activity`).
- `--ozone-platform=headless` is not usable: Equibop segfaults at start-up
  (null call in the browser process), with or without `--disable-gpu`.
- Not measured: the activity on a real profile (needs a signed-in Equibop:
  katana), and memory/CPU while signed in. Equibop's updater window
  (electron-updater) can open on installs it can update (RPM/AppImage); the
  tarball install has no updater. Steam's base-layer focus should keep such a
  window behind Steam, not verified.

## 6. The rows that need katana, and the exact commands

A machine with one GPU and no external monitor cannot answer any of the rows
below. Nothing here was simulated; each row names the command that closes it.

**Where the rows stand.** §6.1 to §6.5 passed on katana on 2026-09-19
(§3 of `ROADMAP/evidence/katana-image-qual-20260919.md`), apart from Safe
Graphics' automatic branch in §6.4, which needs the panel dark. §6.8 ran on
2026-09-22 on the Rime kernel (`ROADMAP/evidence/katana-final-qual-20260922.md`).
The commands stay here as the re-check for a new image.

Run everything from a **greetd login**, not from `systemd-run`, so §6.3's
capability question is answered on the real path (§4 explains why the
qualification could not).

### 6.1 Gaming Mode on the right screen

```sh
# 1. What the selector decides, before rebooting into anything:
rime gaming
rime gaming --gamescope-device-args ; echo "rc=$?"
#    expect: --prefer-vk-device 10de:249d / --prefer-output HDMI-A-1, rc=0
#    and the report's "the screen Gaming Mode will use" block naming card2.

# 2. Pick "Rime Gaming Mode" at the greeter, then afterwards:
journalctl --user -b -o cat | grep -E 'rime-gaming-session|gamescope' | head -40
#    expect in the session log:
#      [rime-gaming-session] GPU/output: --prefer-vk-device 10de:249d --prefer-output HDMI-A-1
#      [gamescope] vulkan: selecting physical device 'NVIDIA GeForce RTX 3070 Laptop GPU'
#      [gamescope] drm: opening DRM node '/dev/dri/card2'
#      [gamescope] drm: selecting connector HDMI-A-1
#      [gamescope] drm: selecting mode 1920x1080@240Hz

# 3. The monitor is the screen that lit, not the panel:
wlr-randr 2>/dev/null || true
nvidia-smi --query-compute-apps=pid,name --format=csv
```

### 6.2 Realtime, in both directions

```sh
grep -E '^Cap(Eff|Prm|Amb):' /proc/self/status     # in the Gaming Mode session
getcap "$(command -v gamescope)"                    # expect: nothing, today
rime gaming | grep -E 'realtime (limit|capability)'
#    expect: realtime limit yes, realtime capability no

# Whether --rt was passed. Read the session's OWN line, and nothing else:
grep -m1 'starting: gamescope' <the session log>
#    expect: no --rt in it, and the rime-gaming-session line above it saying
#            "CAP_SYS_NICE: absent".
```

**Do not check this by grepping for `CAP_SYS_NICE`,** which is what an earlier
version of this section invited. Measured on katana 2026-09-19 (§6.2 in
`ROADMAP/evidence/katana-image-qual-20260919.md`): gamescope prints

```
No CAP_SYS_NICE, falling back to regular-priority compute and threads.
```

**whether or not `--rt` was passed**. It is gamescope's own start-up
capability probe, and the identical two lines appear on an older image where
the session passed `--rt` unconditionally. The message discriminates nothing:
grep for the string and you find both the session's honest
`CAP_SYS_NICE: absent` and gamescope's warning, and can conclude the opposite
of the truth. The `starting: gamescope …` line is the only witness to what the
session passed.

If a later gamescope RPM does carry the capability, the same commands should
show `cap_sys_nice=ep` and `--rt` back in the `starting: gamescope …` line,
with no code change.

### 6.3 Steam inside gamescope

This row passed on katana on 2026-09-19 (§3.3 of
`ROADMAP/evidence/katana-image-qual-20260919.md`): through a greetd login,
Steam Big Picture came up inside gamescope on the RTX 3070 and stayed up for
2 h 11 m. Two independent things had stopped it, and only one of them was
Rime's. Both are closed:

* **32-bit Vulkan.** `rime install steam` used to ship no `*_icd.i686.json` at
  all (§6.5), so Steam's 32-bit client had zero ICDs and failed with `BInit -
  Unable to initialize Vulkan!`. Unit **pkg-share** (merge `5de97037`) fixed
  it in `files/system/libexec/rime-pkg`: the 32-bit pass now keeps the i686
  manifests, 13 of them on katana, including `nvidia_icd.i686.json`.

  ```sh
  ls /usr/share/vulkan/icd.d/ | grep i686     # expect non-empty AFTER pkg-share
  ```

* **bwrap and capabilities.** The qualification harness caused this half (§3):
  a greetd login gives the session `CapPrm=0`. To re-check it on a new image,
  read the session log:

  ```sh
  grep -E 'capabilities:|permitted set' <the session log>
  grep -E 'bwrap|user namespaces' ~/.local/share/Steam/logs/console-linux.txt
  ```

  A zero `CapPrm` with the bwrap message still present means the cause is not
  the session's capabilities and the hunt moves to Steam's runtime. A non-zero
  `CapPrm` means it is, and the session says so.

  `Unable to open X11 display` is expected to disappear on its own: it was
  downstream of §6.1, where gamescope had already died on `card1`.

### 6.4 Safe Graphics on the dGPU output

```sh
/usr/libexec/rime-safe-graphics check
#    expect: primary gpu card1 / can light eDP-1 / cannot light HDMI-A-1(card2)

# The real test is the emergency shape. Disable the panel, or just force it:
RIME_SAFE_GRAPHICS_DRM_DEVICE=/dev/dri/card2 /usr/libexec/rime-safe-graphics
#    expect: the monitor lights, foot appears on it, and the log has no
#    "Renderer did not support importing DMA-BUFs" for HDMI-A-1.
```

The automatic branch (every connected screen on one non-primary card) needs the
panel dark. With the lid shut and the machine docked, or with the panel
disabled in firmware, start Safe Graphics from the greeter and expect
`WLR_DRM_DEVICES=/dev/dri/card2` in its log with no override set.

### 6.5 The niri bar

**The precondition is the user manager starting.** A niri login is not
needed. The transform runs from `rime-shell-firstrun.service`, which is a
**user** unit and starts with `user@<uid>.service`. On a machine with lingering
or an ssh login, that happens at boot, with no graphical session anywhere.
Measured on katana 2026-09-19: the line was rewritten at 18:29:35, five minutes
after a reboot and four hours before any niri session (§3.6 of the evidence).
The check below **confirms** the end state; the niri login does not cause it,
and on a machine whose user manager has already run once there is nothing left
for a login to do.

```sh
# On a machine that had the old config, after its user manager has started
# once — a niri login is sufficient but not necessary:
grep -n 'waybar' ~/.config/niri/config.kdl
#    expect exactly one line, commented, ending in the Rime marker.
pgrep -a -u "$USER" waybar          # expect: nothing
pgrep -a -u "$USER" quickshell      # expect: one
ls ~/.config/niri/config.kdl.pre-rime-bar.bak
niri validate --config ~/.config/niri/config.kdl
```

### 6.6 A Gaming Mode session destroyed *without cooperation*

This is the row §5a exists for, and **it needs an image that carries the
change**: the owner watch lives in `rimed`, so a machine running an older build
behaves as before. `rime game status` printing `owner_pid` is how you know the
image is new enough.

Arm a Gaming Mode session the way the qualification run did (the greetd
helpers and the dead-man restore timer are in
`ROADMAP/state/agents/katana-image-qual.md`), and **always arm the restore
timer first.** Then, from ssh while the session is up:

```sh
rime game status
#    expect: active : true, and owner_pid : <the rime-gaming-session pid>
#    owner_pid : 0 means this image predates the watch — stop here, the row
#    cannot pass and the session log says so too.
pgrep -f '^/usr/libexec/rime-gaming-session'   # must equal that owner_pid
```

Record what must come back, **while game mode is on**:

```sh
cat /sys/fs/cgroup/rime-game/cpuset.cpus    # the p-core list
rime game status | grep -E '^(tier|prior_tier|scx_)'
#    expect: scx_state : loaded, and scx_detail naming root/ops.
#    See §5c before reading anything into sched_ext/state by itself.
```

Now destroy the session in a way nothing can cooperate with. **It must be
`SIGKILL`, and to the session script's own PID.**

```sh
sudo kill -9 "$(pgrep -f '^/usr/libexec/rime-gaming-session')"
```

> **`sudo systemctl restart greetd` does NOT test this row, and this run-book
> told you to use it until 2026-09-20.** The qualification found out why: the
> restart takes the *seat* away, but the session script itself survives long
> enough to run its own `EXIT` trap, so the ordinary cooperative path releases
> game mode (`[rime-gaming-session] rimed game mode released`, three times,
> idempotent). That is a good outcome for a different row. Only killing the
> owner outright leaves nothing that can cooperate: no trap, no signal handler,
> no `rime game stop`. Measured: 1.9 s to release, which is the 2 s watch.

Within a few seconds, with **nothing having asked**:

```sh
rime game status | head -3
#    expect: active : false
test -d /sys/fs/cgroup/rime-game && echo STILL THERE || echo removed
#    expect: removed
rime game status | grep '^scx_'
#    expect: scx_state : not loaded  (see §5c)
sudo journalctl -u rimed -b -o cat | grep -m1 'session owner is gone'
#    expect: rimed: game: the session owner is gone (/proc/<pid> is gone)
#            — releasing game mode.
```

**Two of the obvious readings are NOT discriminators on katana.** Quoting
either as evidence produces a row that passes without proving anything.

* **The CPU governor.** Katana's profile's AC default tier is already
  `performance`, so `prior_tier` and `tier` are both `performance` and
  `scaling_governor` reads `performance` before, during and after. On a machine
  whose default tier is `balanced`, `scaling_governor` is a real witness.
* **`/sys/kernel/sched_ext/state` on its own.** On the 2026-09-20 run it read
  `disabled` during the session as well as after, because Gaming Mode had never
  loaded a scheduler at all (§5c). On an image with that fix and the Rime
  kernel (§5d) it moves, and `rime game status`'s `scx_state` is the reading to
  record, because it distinguishes `not loaded` from `unknown` where the bare
  file cannot.

On that run the readings that moved were **the cgroup, `active`, and the rimed
journal line**. Current images carry both the §5c fix and the Rime kernel, so
record `scx_state` as a fourth, and say which image it came from.

Two more worth taking while you are there:

```sh
# The trap's own failure is now visible instead of silent.
sudo journalctl -b -t <session tag> -o cat | grep -A2 'could not release game mode'
#    expect polkit's own AccessDenied message, and a line naming rimed as what
#    releases it instead. BOTH lines, or the log is back to hiding the cause.

# And the belt-and-braces path still works: end a session by SIGTERMing
# gamescope instead, and the trap should release it immediately.
```

### 6.7 The overlay is gone and the dump store is bounded

```sh
grep -m1 'starting: gamescope' <the session log>
#    expect: --expose-wayland present, --mangoapp ABSENT.
grep -m1 'NOT passing --mangoapp' <the session log>
#    expect: the explanation, naming WAYLAND_DISPLAY.

# Nothing crashed, for the whole session — the number must not move.
coredumpctl list --no-pager | grep -c mangoapp      # before and after
journalctl -b -t <session tag> -o cat | grep -c 'Glfw Error'   # expect: 0
journalctl -b -t <session tag> -o cat | wc -l
#    for scale: the 2026-09-19 two-hour session took 144 187 lines.

# And the guard that holds whatever crashes next:
systemd-analyze cat-config systemd/coredump.conf | grep -E '^(MaxUse|KeepFree)='
#    expect: MaxUse=256M and KeepFree=2G. Neither line appears on an image
#    without the drop-in, because Fedora ships every value commented out.
du -sh /var/lib/systemd/coredump
```

To confirm the gate works the other way on real hardware rather than only in
the suite, arm one session with `RIME_GAMING_EXPOSE_WAYLAND=0` in the Exec
environment: `--mangoapp` should be back in the `starting:` line,
`--expose-wayland` gone, and the overlay should render, which no machine here
has ever seen it do.

### 6.8 sched-ext loads (§5c, §5d)

> **CORRECTED 2026-09-20, after the rows below were run on katana.** Rows A and
> C as originally written **could not pass on any Rime image built up to then**,
> and that was not a failure of §5c's fix. The COPR kernel's BTF gave 22
> sched-ext kfuncs a prototype `libbpf` refuses, so no `scx_*` scheduler loaded
> (§5d). The old text expected `scx_state : loaded`, and whoever ran it next
> would have read the result as a regression. What each row can prove on such a
> kernel is stated alongside what it was written to prove.

> **UPDATED 2026-09-22: the rows ran on the Rime kernel**
> (`7.2.6-cachyos1.rime1`, `ROADMAP/evidence/katana-final-qual-20260922.md`).
> Row 0 read `ok`, and Rows A and B passed: Gaming Mode loaded `scx_lavd`
> through rimed for the first time. Two comments in the block below predate
> that run. Row 0's `implicit-args` is what every image *before* the kernel
> tier gave, and Row A's `root/ops` read
> `lavd_1.1.3_x86_64_unknown_linux_gnu` rather than `lavd` (see the §5c note).
> Row C chose `switch` correctly, but `rime game status` then reported
> `not loaded` while `scx_lavd` was running. A `switch` tears down and
> re-attaches over about 1.4 s, the settle check accepts the scheduler being
> replaced, and the status read lands in the gap. That status defect is still
> open.

Everything in §5c is proven against fixtures. **Three rows need the machine**,
and none can be inferred from a green suite. Run them from an **image that
carries the fix**. On an older image `scx_state` is absent from
`rime game status`, which is how you tell.

**Start with Row 0.** It decides whether Rows A and C can prove anything at
all, and it takes one command.

```sh
# ── Row 0: can this kernel take a scheduler? (§5d) ──────────────────────────
rime game status | grep '^scx_btf'
#    `ok`            → Rows A and C are runnable as written.
#    `implicit-args` → they are NOT. Skip to Row A-alt. This is the reading
#                      every Rime image has given so far.
#    `absent` / `unreadable` / `no-sched-ext` → the probe could not answer;
#                      record which one and read §5d before going further.
#
# The kernel's own account of the same fact, worth capturing once per image:
sudo journalctl -u scx_loader -b -o cat | grep -m1 'func_proto'
#    On an affected kernel: "extern (func ksym) 'scx_bpf_create_dsq':
#    func_proto [N] incompatible with vmlinux [M]".
#    NOTE: scx_loader is bus-activating. Running this after `rime game start`
#    reads a journal that exists; running it on an idle machine may find no
#    unit at all, which is not the same as no error.

# ── Row A: a scheduler actually attaches.  (needs Row 0 = ok) ───────────────
# With NO session running first, so the starting state is the one that used to
# break:
cat /sys/kernel/sched_ext/state          # expect: disabled
rime game status | grep '^scx_'
#    expect: scx_requested : scx_lavd / scx_state : not loaded
#    (Since §5f katana's profile is `scx = "auto"`, which is the kernel's own
#    scheduler on its P/E CPU: scx_requested is empty and nothing loads. Set
#    `scx = "scx_lavd"` in a profile override to run this row.)

sudo rime game start
rime game status | grep '^scx_'
#    expect: scx_state : loaded
#            scx_detail : ... sched_ext/state is enabled, root/ops reads '<name>'
# RECORD THE root/ops STRING VERBATIM. It is expected to be `lavd`, and that
# expectation has never been checked on hardware — no machine here can load a
# scheduler to look at it. If it reads something else, scx_ops_matches() wants
# to know.
sudo journalctl -u rimed -b -o cat | grep -m1 'scxctl'
#    expect: NO 'no scx scheduler running' line. Its presence means the verb
#    selection did not see `disabled`, which is a real failure of this fix.

# ── Row A-alt: the row that IS runnable on an affected kernel. ──────────────
# It proves the two things §5c and §5d are actually responsible for: that the
# verb was chosen from the kernel, and that the status names the real reason.
sudo rime game start
sudo journalctl -u rimed -b -o cat | grep -c 'no scx scheduler running'
#    expect: 0. The verb was read off sched_ext/state, saw `disabled`, and
#    chose `start`. The old hardcoded `switch` produced that refusal on every
#    boot of three images.
rime game status | grep '^scx_'
#    expect: scx_state : not loaded
#            scx_btf   : implicit-args
#            scx_detail: ... — kernel BTF: N of M sched-ext kfuncs still carry
#                        the verifier's implicit 'struct bpf_prog_aux *'
#                        argument (e.g. scx_bpf_...) ... NO sched-ext
#                        scheduler can load on this kernel
# Cross-check the probe against the kernel's own complaint: the kfunc names in
# scx_detail must be drawn from the same set scx_loader printed above. They
# matched exactly on katana (22 of 68) and a disagreement is a defect in
# kernelbtf.rs, not in the kernel.
sudo rime game stop

# ── Row B: it goes away again. ──────────────────────────────────────────────
# Runnable either way: nothing attached is the state `stop` is for.
sudo rime game stop
cat /sys/kernel/sched_ext/state          # expect: disabled
rime game status | grep '^scx_state'     # expect: not loaded
sudo journalctl -u rimed -b -o cat | grep -m1 'sched-ext after exit'
#    expect: a line, and it must say disabled. Before this fix the exit path
#    discarded every outcome but a hard error, so a refused stop was silent.

# ── Row C: `switch` is reached when something IS already running. ────────────
# The one branch a fixture cannot honestly stand in for, because it needs
# scx_loader holding a real scheduler — which an affected kernel cannot give
# it. NEEDS ROW 0 = ok.
sudo scxctl start -s scx_rusty
cat /sys/kernel/sched_ext/state          # expect: enabled
#    On an affected kernel this reads `disabled` and `scxctl get` will
#    nevertheless claim a scheduler is running. That disagreement is
#    scx_loader's bookkeeping, not the kernel's, and it is the exact lie §5c
#    exists to stop Rime repeating. Do not proceed; the row cannot run.
sudo rime game start
sudo journalctl -u rimed -b -o cat | grep -m1 'scxctl'
#    expect: no refusal. The engine must have chosen `switch`, not `start`.
rime game status | grep '^scx_'
#    expect: scx_state : loaded, root/ops now naming lavd rather than rusty.
sudo rime game stop
#    EXPECT A NAMED LINE, not a silent restore:
#    "sched-ext was already running before this session (rusty) and exit
#     STOPPED it rather than putting it back"
#    That is a KNOWN LIMITATION, not a failure of the run: game mode stops the
#    scheduler it found rather than restoring it. `scxctl restore` exists and
#    would be the fix; it is not done here because no Rime image loads a
#    scheduler at boot, so nothing has ever reached it.
sudo scxctl stop
#    expect: it REFUSES — `rime game stop` already stopped it, and nothing is
#    running. That refusal is the row passing, not a loose end.
```

**Row C's retry branch was reached anyway on 2026-09-20**, through a condition
better than the scripted one: after a failed attempt on an affected kernel,
`scx_loader`'s own bookkeeping believed a scheduler was running while the
kernel said none was, so Rime chose `start`, was refused with
`already running, use 'switch'`, and took the single retry. Both verbs ran,
and the status still refused to claim a scheduler. Row C's *other* half, the
named "stopped it rather than restoring it" line on exit, stayed unreached
until Row C on the Rime kernel printed it on 2026-09-22.

**On the timing.** `scx_load` waits up to 2 s (`SCX_SETTLE`) for the scheduler
to attach before reporting. If Row A comes back `unknown` with a `state` of
`enabling`, the budget is too short for that hardware and the constant needs
raising. Record the number rather than re-running until it passes.

**Left out on purpose:** `scx_loader` has *modes* (`Gaming`, `LowLatency`,
`PowerSave`, `Server`) that `scxctl start -m` selects and that Rime does not
use. Whether `-m gaming` beats a bare `-s scx_lavd` is a tuning question for a
machine with a game on it. It does not belong in the row that proves the
scheduler loads at all.
