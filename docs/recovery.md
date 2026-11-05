# Recovery, repair and disposable execution

Roadmap §19. This is the reference for `rime recover`, `rime disposable` and
`rime doctor --json`: what exists, what each verb will and will not do, the
exact data boundary of the factory reset, and the two things §19 asks for that
Rime deliberately does not ship.

`docs/rollback.md` is the drill for the deployment layer underneath this
(`rime rollback`, `ostree admin pin`, rebuild-from-git). This document covers
the surface on top of it.

## The recovery surface

`rime recover status` reports the eight components §19 names, each with a
state and the command that addresses it.

```
COMPONENT               STATE        DETAIL
Current deployment      verified     ostree 8f14e45fceea — Rime OS 43 gaming
Previous deployment     available    2 deployments present, so there is one to go back to
Secure Boot             attention    firmware reports Secure Boot disabled
Filesystem              verified     /usr is read-only on an overlay root, ostree-booted
GPU driver              verified     1 — amd via amdgpu
Rime Shell              verified     vendored in the image, and this account is provisioned
Network                 available    a default route exists. Nothing was contacted.
Package extensions      verified     no user packages on this machine
```

There are four states, and the distinction between the last two matters:

| state | means |
| --- | --- |
| `verified` | present and checked against something |
| `available` | present and usable, but nothing verified it: a rollback target exists; nobody asserted it boots |
| `attention` | present and wrong in a way a named action fixes |
| `unavailable` | could not be determined, or does not exist on this hardware. **Never** a synonym for "fine" |

`--json` emits the same rows with stable ids (`current-deployment`,
`previous-deployment`, `secure-boot`, `filesystem`, `gpu-driver`, `rime-shell`,
`network`, `package-extensions`), the four action buttons, the recovery routes
and the reset scopes. Those ids are a compatibility surface: Rime Settings keys
on them, so a rename breaks a settings page. The same rule covers the members
of `org.rimeos.Rimed1`.

**It spawns no subprocess and contacts nothing.** Every fact is a file read:
`/proc/cmdline`, `/proc/mounts`, `/proc/modules`, `/proc/net/route`,
`/run/ostree-booted`, the efivars, the `/ostree/deploy` directory listing,
`/var/lib/rime/pkg/state.json`. Three things follow, and the suite asserts each
of them:

* it cannot raise an authentication prompt, because nothing it does needs
  authorising;
* it cannot hang, so Rime Settings can poll it;
* the states a healthy machine does not have (no rollback target, `/usr`
  mounted read-write, an extension built for the previous release, no GPU
  driver) are reachable as fixture trees instead of by reasoning.

The deliberate cost: the deployment row reports the ostree **checksum**, not
the image reference. The reference lives in `bootc status`, and parsing another
tool's JSON schema to duplicate what `rime changelog` already prints would buy
a second thing to keep in sync. The row names `rime changelog` instead.

### Rollback is `rime rollback`, and there is no second name for it

§19 lists `[Boot previous deployment]` as a button and asks to "make rollback
visible from Settings, not only the CLI". The `previous-deployment` row
satisfies both: it reports whether there *is* anything to go back to and names
`sudo rime rollback` as the command a button runs. Adding an
`rime recover previous` verb would have been a second name for an operation
that already exists.

The row also carries the advice `docs/rollback.md` gives: bootc keeps only
booted+previous, so two bad updates in a row can evict the last good image, and
`sudo rime pin` before anything risky prevents that.

## Automatic repair

`rime recover repair` is a dry run unless given `--commit`.

**A step is eligible for automatic repair only if it is idempotent and removes
no data.** That invariant makes a single button defensible: pressing it twice
does nothing the second time, and pressing it by accident costs nothing.
`rimed-core`'s test asserts it over the whole table: no step's argv may contain
a destructive argument, and none may contain `sudo`, `pkexec`, `su`, `run0` or
`systemd-run`.

Rollback and factory reset are **not** repairs and this verb will never perform
either. §19 lists them as separate actions because they carry three different
levels of consequence.

Two steps, each offered only when the surface diagnoses it:

| step | domain | when |
| --- | --- | --- |
| `reprovision-desktop` | user | the `rime-shell` row reports attention |
| `rebuild-package-extension` | system | the `package-extensions` row reports attention |

Like `rime apply`, repair converges the privilege domain it is already running
in and *reports* the other. Nothing here calls `sudo` itself, so it cannot
raise an authentication prompt; `sudo rime recover repair --commit` runs the
system half.

`ostree admin pin 0` is **not** a repair step, by design. It would pass every
invariant (it is idempotent and deletes nothing), but Rime cannot tell whether
it is *needed* without running `ostree admin status`, and `rime recover status`
reads files rather than spawning subprocesses. A repair with no diagnosis
behind it gets proposed on every healthy machine, and people learn to ignore a
button that always has something to say. It is advice on the
previous-deployment row instead.

## The factory reset, and exactly what it deletes

`rime recover reset` is the most destructive verb in the product, so it is
built to be refused.

**Dry run is the default.** Performing it needs `--commit` **and**
`--confirm <token>`, where the token is derived from the scope *and the exact
set of paths the plan found*:

```
$ rime recover reset --scope user
Factory reset — DRY RUN. Nothing has been changed.
…
To perform it, run exactly:
  rime recover reset --scope user --commit --confirm user:9:3f2a1c9b
```

A caller cannot construct that token from the scope alone: it has to run the
plan, which is the step that prints the loss list. If the machine changes
between plan and commit, the token differs and the commit is refused with
nothing touched.

### Two scopes, and no third

| | `--scope desktop` | `--scope user` |
| --- | --- | --- |
| Rime Shell settings, keybinds, caches | removed | removed |
| the generated Hyprland input/monitor overrides | **emptied**, not removed | **emptied**, not removed |
| your blueprint (`~/.config/rime/blueprint.toml`) | preserved | **removed** |
| per-game profiles, trusted devices, local-model settings | preserved | **removed** |
| `~/.local/state/rime` (applied-blueprint record, probe cache, recorded agent sessions) | preserved | **removed** |

Preserved by **both**, and printed in full by the dry run:

* every document, project, checkout and credential in your home directory;
* `~/.ssh`, `~/.gnupg`, `~/.aws` and every browser profile;
* your Hyprland, niri and labwc configuration, including the lock/idle config;
* Rime Shell plugins in `~/.config/rime-shell/plugins`;
* your capsules and their records;
* installed packages, Flatpaks and downloaded models;
* the booted deployment and its rollback target.

### Why those boundaries, and not wider

* **Nothing under `/etc`.** It holds `passwd`, `shadow`, `fstab`, `crypttab`
  and the NetworkManager connections. ostree three-way-merges it against the
  deployment and there is no runtime verb that restores it to image state
  without deploying. A reset that emptied it would produce a machine that does
  not boot or cannot be logged into, and the thing that would undo it is what
  it broke.
* **Nothing under `/var/lib/rime`.** The package extension, the model store and
  the boot-health records each have a program that owns them (`rime-pkg`,
  `rime ai`, `rime-boot-health`). Deleting `pkg/state.json` under a *merged*
  system extension leaves `/usr` carrying packages Rime can no longer name,
  which is worse than the state being left. The machine-level operations are
  the verbs that already exist, and `rime recover status` names them.
* **Not the capsule records under `~/.local/share/rime/env`.** Each one names a
  real rootless container. Deleting the record orphans the container:
  `rime env list` would show nothing while `podman ps -a` still shows them, and
  Rime would have lost the name it needs to remove them. `rime env rm` is the
  verb for that.
* **Nothing under `~/.config/hypr` is ever *deleted*.** The reset **truncates**
  the generated modules `rime/input.lua`, `rime/monitors.lua` and
  `rime/shell-keybinds.lua` to the "no overrides" state, and preserves
  everything else in that directory. A one-line edit to a live compositor
  config has already cost this project a desktop once; `AGENTS.md`'s "Editing a
  live machine's configuration" rules exist because of it.

  Truncating rather than deleting used to be forced: `hyprland.conf` `source=`d
  the generated files and a `source=` with no match is a **fatal** config error.
  P0-025 removed that (`hyprland.lua` checks `package.searchpath` before
  requiring, so an absent module is skipped), and the rule stays anyway,
  because "empty" is a state the compositor understands and one this code can
  produce without deciding which of the user's files it may remove.

  `rime/user-overrides.lua` is the exception in the other direction: it is where
  `rime-hypr-migrate` puts a user's own hand-written hyprlang after converting
  it, so it is a preserved landmark and not a reset target at all.

### The other protections

* **It refuses to run as root.** Root's home is not the user's, so
  `sudo rime recover reset` would reset root's desktop and leave the user's
  as it is, while reporting success.
* **`$HOME` is validated before anything is resolved under it**: absolute, an
  existing directory, at least two components deep, and never `/`, `/home`,
  `/var/home`, `/usr`, `/etc` or `/var`.
* **Every target is re-resolved before it is touched.** The final component
  must not be a symlink (`realpath` on a symlink returns its *target*, so a
  prefix check alone would resolve `~/.cache/rime-shell` → `$HOME` and conclude
  `$HOME` is inside itself), the parent is canonicalised, and the rebuilt path
  must be inside the home and must not be the home. A single refusal aborts the
  **whole** reset. It is all-or-nothing because a reset that removed four paths
  and then refused the fifth leaves a state nobody planned.
* **Everything removed is copied aside first**, to
  `~/rime-reset-backup-<timestamp>`, except caches. `AGENTS.md`'s first rule
  about touching a live config is to back it up, and that rule is what turned
  an earlier outage into a two-minute restore.
* **Preserved landmarks are re-checked afterwards.** Every landmark that
  existed before the commit must still exist after it. Grepping for what you
  deleted cannot detect what you deleted *as well*; this check can.
* **It refuses to start if the provisioner is missing.** Removing the files
  Rime Shell needs with no way to put them back is worse than the state being
  left. `--no-reprovision` takes the deletion alone, deliberately.

### What a full factory reset is, and why it is not this verb

A machine indistinguishable from a fresh install (user accounts gone, `/etc`
pristine, disks repartitioned) is **a reinstall**, and the installer is what
does it. Nothing on a running system can recreate `/etc` from the image without
deploying, and deleting user accounts from a running system is not a CLI verb.

`rime recover reset --scope user` is the largest reset Rime will perform, and
it is scoped to one account's Rime-owned state. Saying so is better than a
`[Factory reset]` button that stops somewhere the user cannot predict.

## Recovery routes

`rime recover status` reports which ways back into a working system exist on
*this* machine. They differ from machine to machine, and the report says which
apply.

| route | when it exists |
| --- | --- |
| previous deployment | two or more deployments under `/ostree/deploy` |
| `rescue.target` | the kernel command line is editable at the boot menu |
| boot counting | `LoaderBootCountPath` is set: the opt-in systemd-boot path |
| disposable environment | `/usr/libexec/rime-disposable` is installed |
| recovery boot entry | **never**; see below |
| installer media | reported as *unknown*, because a running system cannot tell |

The rescue route depends on the **UKI**, not on the bootloader's name. GRUB
lets you edit the command line at the menu; a Unified Kernel Image does not,
because its command line is inside the signed image, which is what makes the
signature worth having. On the opt-in systemd-boot+UKI path that route
therefore does not exist, on exactly the machines that are hardest to get
into, and reporting it on every machine would be a false claim there.

### Rime ships no recovery boot entry, and that is deliberate

§19 asks for "a recovery boot entry **or** environment". Rime ships the
environment and documents the entry as an operator procedure. The reason is
`AGENTS.md`'s boot-path rules, which are binding:

> There is no rollback for an ESP you overwrote or an EFI variable you
> replaced, because the thing that would perform the rollback is what you
> broke.

Creating a genuine recovery *entry* means one of `bootctl install`,
`efibootmgr -c`, a file under `/boot/loader/entries`, or `grub2-mkconfig`
against a live `grub.cfg`. Rule 1 forbids every one of those on a real Rime
host, and `tests/test-boot-v2.sh` scans every shipped unit and helper for those
commands on executable lines and **fails the build** if one appears. Scripting
it would have broken an existing CI gate as well as the contract.

`rime recover status` therefore reports this route as `available: false` with
the reason, rather than omitting it. The operator procedure, for someone who
wants one:

```bash
# On a machine you can put into firmware setup, with a live USB you have
# actually booted once. Read docs/boot-v2.md's enrolment section first.
#
# GRUB is already installed and already in the firmware's boot order. The
# simplest recovery "entry" on a GRUB machine costs nothing and needs no
# script: at the menu, press `e`, append systemd.unit=rescue.target to the
# linux line, and press Ctrl-X.
#
# For a persistent menu entry, /etc/grub.d/40_custom is the supported place,
# and running grub2-mkconfig is YOUR decision on YOUR machine:
sudo $EDITOR /etc/grub.d/40_custom      # add a menuentry with the rescue cmdline
sudo cp /boot/grub2/grub.cfg /boot/grub2/grub.cfg.pre-recovery-entry
sudo grub2-mkconfig -o /boot/grub2/grub.cfg
```

Nothing in this repository runs those commands for you, and a test checks
that.

## When the Rime Shell does not start

The compositor starts the Shell through `rime-shell-autostart`
(`/usr/libexec/` on bootc, `/run/current-system/sw/libexec/` on NixOS). It
writes every step to the journal under its own tag, and waits up to 90 s for
the Shell to answer on IPC:

- quickshell exits before the Shell answers: the launcher starts it again, up
  to three starts in all;
- three starts fail, or quickshell runs but never answers: an error in the
  journal and a notification on screen ("Rime Shell did not start: …").
  Hyprland draws that notification itself, so it shows with no Shell; other
  sessions get it through `notify-send`.

```sh
journalctl -b -t rime-shell-autostart      # what the launcher saw
qs -c /usr/share/rime-shell log            # quickshell's own log (the NixOS path: /run/current-system/sw/share/rime-shell)
/usr/libexec/rime-shell-autostart          # start it again from a terminal
```

An IPC client already talking to the Shell's config (`qs -c <dir> ipc …`,
the lock command, `rime shell`) is not taken for a running Shell. Until
2026-11-04 it was, and the launcher exited with no Shell and no message.

## Rime Safe Graphics

Roadmap P2-018. When the desktop does not paint (a GPU driver that stopped
working after an update, a compositor configuration that will not parse, Rime
Shell dying on startup), Safe Graphics is a minimal desktop that comes up
anyway, on the CPU, with the tools to find out what happened.

It is a labwc session started as:

```
labwc -C /usr/share/rime/safe-graphics
```

That flag names a config **directory**, not a config file, and it is the whole
of the mechanism. labwc reads `rc.xml`, `autostart`, `environment` and
`menu.xml` from there and never opens `~/.config/labwc`. That matters more on
this system than elsewhere, because Rime itself writes into the user's copy:
`rime-input-apply` splices a `<libinput>` element into `rc.xml` with
ElementTree, and `rime-labwc-keybinds` generates the keybind block. labwc falls
back to its built-in defaults **silently** on a malformed `rc.xml`, so a
recovery session that read the user's copy would inherit whatever broke the
last one and say nothing about it.

The session uses `-C` rather than pointing `XDG_CONFIG_HOME` somewhere else, on
purpose: the compositor has to be isolated from the user's configuration, and
the clients must not be. A file manager with no config directory cannot save a
bookmark, and somebody in recovery still has a home directory they came here to
reach.

### Getting into it

**From the greeter.** "Rime Safe Graphics" is in the session list.

**The machine offers it, after three failed starts.** You do not have to know
about this route. `/usr/libexec/rime-session-watchdog` counts: the greeter
writes down what it launched, and the next greeter to start asks how long ago
that was. A session that did not last 45 seconds did not get anywhere. After
three of those in a row for the same session, the picker comes up already on
Rime Safe Graphics, with a line above the password box saying *Your desktop did
not start — Rime Safe Graphics selected*.

That line is a suggestion, and the machine does not force it:

- It **preselects**. Cycle the picker to your normal desktop and the machine
  stops arguing for that login: touching the picker settles the question for
  the greeter you are looking at. If the desktop bounces again, the next
  greeter will preselect again, which is the right behaviour, because by then
  it has failed a fourth time.
- The recovery session is **never remembered** as your default. Your previous
  choice survives the visit, so once the machine is fixed you land back on the
  desktop you were using rather than on the rescue one.
- **Success clears the count; recovery does not.** Any ordinary session that
  survives 45 seconds wipes it. A working trip through Rime Safe Graphics
  clears nothing, because recovery working says nothing about whether the normal
  desktop was fixed. After you fix the machine, pick your desktop once and the
  notice is gone.
- It **survives a reboot**, on purpose. A machine that bounced three times and
  was then power-cycled has not been fixed by the power cycle.

To clear the count by hand (the `greetd` user owns the state, so this needs
root):

```sh
sudo /usr/libexec/rime-session-watchdog reset
```

and to see what it believes now, which needs nothing:

```sh
/usr/libexec/rime-session-watchdog status
```

**What it does not catch.** It detects a session that *dies* within 45
seconds. A compositor that comes up and stays up while the shell inside it
crashes in a loop is not a bounce, and the watchdog will not notice it; nor
will it notice a desktop that paints nothing but never exits. The virtual
console below is for those cases. The counter covers the narrow, certain half of the problem on purpose,
because a heuristic could put a working machine into the rescue session.

**From a virtual console.** `Ctrl+Alt+F2`, log in, and run:

```sh
rime-safe-graphics
```

The virtual console is the route that works when nothing paints. The greeter is
itself a wlroots compositor on VT 1, so a machine with no working GL may never
paint the session picker at all, and a recovery desktop reachable only through
a screen that does not come up is no recovery desktop. Nothing else in Rime
advertises the virtual consoles; this document does.

There is no route through a recovery boot entry, for the reason the section
above gives at length: a shipped helper may not write the ESP or an EFI
variable, and `tests/test-boot-v2.sh` fails the build if one tries.

### What it does differently

| | normal session | safe graphics |
| --- | --- | --- |
| configuration | `~/.config/labwc`, seeded and then yours | `/usr/share/rime/safe-graphics`, read-only |
| renderer | GPU | `WLR_RENDERER=pixman`, `LIBGL_ALWAYS_SOFTWARE=1`, llvmpipe |
| driver selection | whatever the environment says | `__GLX_VENDOR_LIBRARY_NAME`, `GBM_BACKEND`, `MESA_LOADER_DRIVER_OVERRIDE` **unset** |
| clients | Rime Shell, your autostart | one terminal |
| menu | Rime Shell's, plain right-click | the recovery menu, plain right-click |

`WLR_BACKENDS` is the one variable the session does **not** set. Unset, wlroots
picks DRM for a person at a keyboard, and a test can ask for `headless` and get
it, which is the only way to exercise this session without taking somebody's
display away.

The autostart does not start Rime Shell. The shell is one of the
things that can be broken, and a recovery session that started it would fail in
the same way as the session the user left.

### What you can do from in there

Right-click anywhere:

* **What is wrong**: `rime recover status`, the eight-row report above.
* **Full health report**: `rime doctor`.
* **Collect diagnostics**: `rime-safe-graphics diagnose`, which writes
  `rime-diagnostics-<date>.tar.gz` into your home directory and prints the
  path. It carries the boot journal at warning and above, the user journal,
  `rime recover status --json`, `rime doctor --json`, `lspci -k` and
  `/proc/modules`. **No file in it is ever empty**: a step that could not run
  writes `could-not-run: …` into its own file, because an empty file and a
  refused command read the same and only one of them means there was nothing to
  report.
* **Roll back to the previous deployment**: `sudo rime rollback`. This is also
  the driver rollback: a graphics driver on Rime is image content, so the
  previous deployment *is* the previous driver.
* **Files** and **Network**: Thunar and `nmtui`.
* **Log out**, back to the greeter.

`Super+Return` opens a terminal, `Super+q` closes a window, `Alt+Tab` switches.
There is nothing else, on purpose: a recovery desktop that grew a desktop's
features would be a second desktop to keep working.

### Checking it without starting it

```sh
rime-safe-graphics check
```

This prints the config directory, the compositor, the terminal it would open
and the renderer, or fails naming what is missing. A session that cannot start
exits non-zero rather than showing a black screen, and greetd then re-displays
the greeter: the same fail-safe `rime-gaming-session` takes.

## Disposable environments

`rime disposable` is a **mode of Rime Capsules**, not a second environment
mechanism. Every disposable environment is an ordinary capsule created through
`/usr/libexec/rime-env`, so `rime env list` sees it and `podman ps` sees it.
It adds three things a capsule does not have: a home that is not yours, an
explicit copy boundary, and a teardown.

```bash
rime disposable plan --copy-in ~/src/thing --copy-out ~/results
rime disposable run  --copy-in ~/src/thing --copy-out ~/results -- make test
rime disposable run --git https://github.com/someone/unknown-project
rime disposable list       # normally empty: a run deletes its own
rime disposable purge      # removes ones a lost-power run left behind
```

### It is not a security boundary, and `plan` says so

distrobox mounts the host's root filesystem at `/run/host` inside **every**
capsule. That is how `distrobox-export` reaches back out to write a `.desktop`
file into the host's home, which `rime-env` documents and depends on. No
distrobox flag removes it. A program in a disposable capsule can read and write
your files through `/run/host`, as your own uid.

"Disposable" here means the **environment** is disposable (its packages, its
home, its state) and nothing more. §19's "run untrusted GitHub projects in
disposable capsules" is satisfied on the axis of throwing the environment away;
it is **not** satisfied on the axis of containing hostile code.

For confinement (`$HOME` masked, `~/.ssh` unreachable, the system bus
unreachable, the environment cleared and rebuilt from an allowlist), the
mechanism that exists and fails closed is the agent sandbox (`rime agent`,
`rimed/rime-agent-core/src/sandbox.rs`). The two compose: an agent session
confined by that sandbox can be started from inside a disposable capsule, and
then the environment is thrown away as well.

`rime disposable plan` prints all of this before anything is created, and the
image build asserts that `--help` still contains "not a security boundary".

### The copy boundary

Default-deny at both ends.

* **In**: nothing, unless `--copy-in` names it. It is a **copy**, not a bind:
  whatever happens inside cannot change the host original, which is stronger
  than a read-only mount and far easier to prove. Copies land at `~/in/<name>`.
* **Out**: nothing, unless `--copy-out` names a destination. Only `~/out`
  leaves. A destination inside the disposable root is refused, because teardown
  would delete it. An existing file at the destination is never overwritten:
  a collision refuses the **whole** copy-out, and `--force` is the explicit
  opt-in.
* **Devices**: none, ever. A capsule holding a device open can stop the machine
  suspending, and nothing disposable needs one.
* `--git` clones **inside** the environment, so your git configuration and
  credential helpers are never used for it. `https://` only: an ssh remote would
  need your key, and a disposable environment that can reach your ssh agent can
  push with your identity.

### Teardown

The environment is deleted when it closes: on a clean exit, on a failure
inside, on a failed creation, and on Ctrl-C. Teardown is a recursive removal
driven by a name, which makes it the most dangerous line in the file, so it is
fenced four ways and **any** failure is a hard exit with nothing removed:

1. the name must match `disp-[a-z0-9]{1,24}`;
2. the final component must not be a symlink: `realpath` on a symlink returns
   its *target*, so a prefix check alone would resolve `<root>/disp-x` → `$HOME`
   and conclude `$HOME` is inside itself;
3. the path is resolved with `realpath -e`;
4. the resolved path must equal **exactly** `<resolved root>/<name>`, by string
   equality, because a prefix test passes for `<root>-elsewhere/x`.

`purge` re-validates every name on the way out and leaves a directory that is
not a disposable environment alone, saying so rather than skipping it without a
word.

Both guards are mutation-tested. Removing the symlink check fails one assertion
and the canary still survives, because the equality check catches it on its
own; removing both fails six, including "THE CANARY IS GONE".

The disposable root is `$XDG_STATE_HOME/rime/disposable`, **not** `/tmp`. This
repository has already had `/tmp` wiped mid-session
(`docs/p1-progress.md`), and a sweep between `run` and its teardown would take
the one directory the user asked to keep.

## `rime doctor --json`

This is §19's "expose `rime doctor` results graphically", from the OS side:
the checks the text form prints, in the shape a UI renders.

```json
{ "checks": [ {"ok": true, "check": "rimed running (owns org.rimeos.Rimed1)"} ],
  "passed": 14, "warned": 3, "total": 17 }
```

It renders one list twice and adds no second set of checks: two diagnostic
implementations would disagree, and the one the user reads would be the one
wired to nothing. The suite asserts that the JSON and the text report the same
count.

**No severity field.** `rime doctor`'s own comment says a WARN is information
rather than a fault: a laptop with no ACPI `platform_profile` is not broken.
Adding a severity would mean inventing a judgement the checks do not make, and
a UI painting an invented judgement red is worse than one showing two states.

## What §19 asks for and this does not do

* **A recovery boot entry: not shipped, deliberately.** See above. The
  constraint is `AGENTS.md` boot-path rule 1 and the existing
  `tests/test-boot-v2.sh` gate, and the honest deliverable is §19's own
  alternative, a recovery *environment*, plus the entry as a documented
  operator procedure.
* **A full factory reset: not shipped, deliberately.** Accounts, `/etc` and
  partitions belong to the installer. The largest reset here is scoped to one
  account's Rime-owned state, and `rime recover reset --scope user` prints
  exactly where it stops.
* **"Open normally / Open isolated / Open disposable" is two modes, not
  three.** "Open normally" is the host's default handler and "Open disposable"
  is `rime disposable run`. There is no generic *confined-open* verb: the
  bubblewrap sandbox in `rime-agent-core` confines **agent sessions**, and
  exposing it as a general "run this one program confined" command is a real
  feature that does not exist yet. Offering a three-item menu where one item
  does nothing different would be worse than reporting the gap.
* **No GUI in THIS repository.** §19's action list is a UI, and the OS side of
  it is `rime recover status --json` plus `rime doctor --json`. Nothing here
  writes QML.

  That sentence used to end "Rime Shell renders them", which was a plan rather
  than a fact: the consumer this `--help` text advertises as "safe for Rime
  Settings to poll" did not exist on any rime-shell branch. It does now.
  `src/services/RecoveryService.qml` polls those two verbs and nothing else,
  and `src/services/config_tab/pages/RecoveryPage.qml` is the Recovery page
  under System in the Nexus (Rime Settings, `src/nexus/PageRegistry.qml`), which
  opens from the Dashboard's Settings tab. The row ids above are what it keys
  on, which is what makes them the compatibility surface this document already
  said they were.
