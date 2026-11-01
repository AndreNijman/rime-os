# Boot v2: signed UKIs, boot counting, measured boot

Roadmap §22. This is the reference for the systemd-boot + Unified Kernel Image
path: what exists, what it was measured to do, how a machine gets onto it, and
how to get back.

**Andre decided on 2026-09-20 that Rime moves to systemd-boot on every
machine.** The next section is that decision and the design it forces. The rest
of the document (the ESP layout, the measurements, the enrolment procedure,
recovery) is the ground the decision is built on.

## The pivot to systemd-boot — the decision, and what it costs

**Decision, Andre, 2026-09-20: Rime moves to systemd-boot on every machine.**
He took it with the risks in front of him: it changes how every machine boots,
on an ESP shared with Windows on at least one of them, and there is no rollback
for an ESP something overwrote. This section does not argue the decision. It
is the design that has to satisfy the constraints the old default was
protecting.

Everything below that says *measured* is a command that ran in the VM lab, and
the transcript is in `ROADMAP/evidence/sdboot-image-20260920-lab.md` or
`ROADMAP/evidence/sdboot-image-20260920-decision.md`.

### The pivot is a storage-backend change, not a bootloader flag

This finding carries the rest of the design, and it is the reason GRUB stayed:

```
bootc install to-disk --via-loopback … --bootloader systemd   # ostree backend
error: Installing to disk: bootupd is required for ostree-based installs
```

`bootupd` **is** in the image, but it ships only a grub2+shim payload, so on the
ostree backend `--bootloader systemd` has nothing to install and bootc refuses
rather than producing an unbootable disk. Two structural reasons sit behind
that refusal, both visible on the L16. ostree's BLS entries and kernels live on
**btrfs** under `/boot`, which systemd-boot cannot read at all: it reads only
what UEFI's simple-file-system protocol reads, which is FAT. And the ostree
cmdline carries a per-deployment `ostree=` and a per-machine `root=UUID=`,
neither of which can be inside a UKI signed in CI.

`bootc install … --bootloader systemd --composefs-backend` installs, boots and
upgrades. So every Rime machine that boots systemd-boot is a machine whose root
storage is composefs-backed.

**That does not make it a reinstall. The sentence that used to stand here
said it did, and it was wrong.** The conclusion came from `bootc --help`, which
lists no migration verb. But `bootc install to-existing-root` takes
`--composefs-backend` and `--bootloader systemd`, and run against a live
ostree + GRUB machine it converts it in place. Measured 2026-09-21, on a guest
booted from an ordinary GRUB install:

```
Installing image: docker://localhost/sdmig:v2
Bootloader: systemd
Installing bootloader via systemd-boot
Installation complete!
```

The next section covers everything Rime had to add to make that safe.

## Migrating a machine that already exists

Andre, 2026-09-20, on being told the answer was a reinstall: *"it cannot be
that. active machines should automitcally migrate with sudo rime install, not
this dumbass reinstall shit. and also a failed or cancelled or power shut down
or something sudo rime install in this change should not fully break the boot
and actual system or whatever, everything should safely migrate."*

`files/system/libexec/rime-boot-migrate` is that. Until 2026-11-01 `rime
update` ran it with no flag and nothing for the user to choose.

**Restated 2026-11-01: the migration runs only when asked.** The automatic run
moved a real machine whose trial boot did not come up (`phase=failed`, its
`/var` join reversed by hand), and Rime's next platform does not build on this
chain. `rime update` now leaves the boot path alone. The move happens when the
user asks for it — `sudo rime boot migrate`, or `sudo rime update
--migrate-boot` in place of that run's image update — and only when the
machine is healthy right now: §26's verdict (no failed unit, no regression in
`rime recover status`), read before the engine runs. A machine whose last
attempt failed is told so by name on every update, and is re-armed only by
`rime-boot-migrate retry`. The other half of the rule stands unchanged: a
failed, cancelled or power-cut migration must not break the boot, and nothing
below about how the engine commits has changed.

### What the bare bootc command does to a machine, measured

Run with nothing around it, `bootc install to-existing-root --composefs-backend
--bootloader systemd`:

| | before | after |
| --- | --- | --- |
| `/EFI/fedora/shimx64.efi`, **what Boot0000 points at** | present | **deleted** |
| `/EFI/fedora/{grubx64.efi,grub.cfg,BOOTX64.CSV,…}` | present | **deleted** |
| `/EFI/BOOT/BOOTX64.EFI` | shim | **systemd-boot** |
| root filesystem `/boot`: grub2/, BLS entries, ostree kernels | present | **wiped** |

The guest that did that still booted, which is the dangerous part. It booted
only because the firmware fell through to the removable-media fallback, which
bootc had replaced moments earlier with an unsigned systemd-boot. On a machine
with Secure Boot on, the firmware refuses that fallback, and the machine's own
boot entry points at a file that is gone.

### The three phases, and where the commit is

**STAGE: nothing the firmware reads changes.** The install runs with a
throwaway FAT filesystem bound over `/target/boot` inside the container, so
bootc's wipe of "the bootloader state" lands on scratch. Measured with that
bind in place: `/EFI/fedora` is **byte-identical** before and after, the root
filesystem's `/boot` is untouched, and the only overwrite anywhere on the ESP
is `/EFI/BOOT/BOOTX64.EFI`, which the migration saves first and puts back,
verified by sha256. Everything else bootc writes is additive: `/EFI/systemd/`,
`/EFI/Linux/`, `/loader/`. GRUB reads none of those; it reads `/boot/loader` on
the root filesystem.

Two details are easy to get wrong, and both were measured going wrong first:

* the staging must be a real filesystem, because bootc reads a UUID off
  `/target/boot` and fails with `No UUID found for /boot` against a plain bind;
* and it must carry **the real ESP's volume id**, because bootc bakes that UUID
  into the kernel command line as `boot=UUID=…`. With a fresh one the migrated
  machine booted, could not find a filesystem with that UUID (the migration
  deletes the staging image minutes earlier), and dropped to emergency mode on
  `boot.mount`. An assertion now covers that case.

**COMMIT: one EFI variable write.** The migration creates the `Boot####` entry
with `--create-only`, so it is not in `BootOrder` and changes nothing. Then it
makes one `SetVariable` of **`BootNext`**. That is the commit point, and it
heals itself: the firmware consumes `BootNext` before it launches anything, so
a machine that fails to come up on the new path boots the old one next, with
nobody doing anything.

**CONFIRM: one more write, after proof.** `rime-boot-migrate-confirm.service`
runs after `boot-complete.target` on both paths while a migration is in flight.
On systemd-boot it writes `BootOrder` with the new entry first and GRUB behind
it. On GRUB it records the migration as **failed**, which stops `rime
update` spending a reboot on the same broken loader every time; `rime-boot-
migrate retry` re-arms it deliberately.

### The machine's data comes with it

A migration that loses `/var/home` is a reinstall under another name, and bootc
gives the composefs deployment a **fresh** stateroot. Measured: the first
migrated guest came up with an empty `/var` and no user.

* **`/var` is moved** into the composefs stateroot, with a symlink left where
  ostree looks, so both boot paths see one `/var` and the GRUB fallback is a
  working machine rather than an empty one. The direction matters. The obvious
  arrangement (leave `/var` where it is, symlink the stateroot at it) was
  tried first, and the machine came up with **`/var` mounted read-only**. The
  rename happens inside a private mount namespace, because the ostree
  stateroot's var is a mountpoint on a running machine and a plain rename is
  `EBUSY`.
* **`/etc` is copied** from the old deployment to the new one. The copy is exact
  rather than approximate only because the migration deploys **the digest the
  machine is already running**: same image, so the image's `/etc` is the same
  and the copy carries the local modifications and nothing else. New content
  arrives on the next ordinary `rime update`, through the new path.

### What it costs

Andre tracks this per machine, so here it is in full:

* the ostree repo and its deployment **stay**, because they are the recovery
  path, and on Rime that is most of 15 GB;
* `/composefs` is a second copy of the image content on the same disk;
* the install has to run as a container of the booted image, so the migration
  copies the image out of bootc's storage into podman's: a **third** copy. The
  migration deletes that one as soon as the install succeeds, and
  `bootc image copy-to-storage` makes it locally, with no download.

So a migrated machine carries roughly two copies of its image until someone
deletes the ostree deployment, and nothing in this unit deletes it. On katana's
tight `/var` that matters, so the precheck measures it: it refuses with
`root-too-small` unless the root filesystem has room for about twice the ostree
repo plus 512 MiB, and reports the figure as `root-space` when it passes.

### When it refuses

A machine that cannot migrate safely stays on GRUB, working, and says why.
Every refusal is named, and `rime update` carries on and takes its ordinary
image update.

| refusal | what it means |
| --- | --- |
| `secure-boot-unsigned-loader` | Secure Boot is on and this image's systemd-boot is unsigned. The firmware would refuse the new loader. **This is the L16 today.** |
| `esp-too-small` | the ESP cannot hold two deployments plus a staged third. It used to be the L16's second refusal too: 600 MiB against ~1.1 GiB. **The slim initramfs retired it:** at 100.9 MiB per deployment the requirement is 350 MiB, and on katana, booted on that image, `precheck` now answers `OK esp-space: 503 MiB free, 350 MiB needed` and `This machine can migrate` (2026-09-22, `ROADMAP/evidence/initramfs-slim2-20260922.md`). |
| `update-staged` | an ostree update is already staged; one shutdown must not have two finalize paths |
| `already-migrated` | the booted deployment is not on the ostree backend |
| `bootc-too-old` | no `--composefs-backend` on `install to-existing-root` |
| `no-esp`, `not-uefi`, `no-dosfstools`, `no-rsync`, `no-podman` | the machine or the image is missing something the migration needs |
| `last-attempt-failed` | the previous commit's trial boot did not come up. Not retried on its own. |

The engine has more refusals than the table shows, each named the same way:
`root-too-small` and `no-repo-size` (the root filesystem check above),
`esp-is-windows` (the ESP it would write carries Windows' `bootmgfw.efi`; see
`docs/rime-owns-its-esp.md`), `secure-boot-unknown`, `partial-install`,
`no-kernel` and `not-root`. `rime-boot-migrate precheck --explain` prints every
check rather than stopping at the first refusal.


### Which ESP, on a machine with more than one

Two of the three machines in this program have two ESPs, and the answer is not
the intuitive one.

**The migration writes the ESP on the disk the ROOT FILESYSTEM is on, not the
one the firmware is booting from.** `rime-boot-migrate`'s `find_esp` walks up
from `findmnt --target /sysroot` to a disk and scans that disk for the ESP type
GUID. bootc does the same thing independently
(`find_first_colocated_esp()` searches `find_all_roots()`, the disks backing the
root device, `crates/blockdev/src/blockdev.rs:214`), so the firmware entry and
the files it points at can never end up on different partitions.

On **katana** that is a real improvement. It boots Rime from the 200 MiB ESP on
the **Windows** disk (`Boot0000* APEX-OS Primary`
→ `\EFI\APEX\SHIMX64.EFI`, `BootCurrent: 0000`) while its own disk carries an
unused 512 MiB `EFI-SYSTEM`. Migrating moves Rime's boot onto its own disk, and
Rime stops depending on another operating system's disk. That dependency is why
every brief this year has warned that one mistake on that ESP takes out two
operating systems.

Because it is an improvement, it is **a note and not a refusal**. The precheck
reads the GPT PARTUUID out of `BootCurrent`'s device path, compares it with the
ESP it is about to write, and when they differ says so: which partition it boots
from, which it will write, and that it writes nothing to the first, so another
operating system installed there keeps its own boot entry. A refusal would keep
katana on the Windows disk forever. The trial boot already covers the failure a
refusal might guard against (firmware that does not enumerate the new disk): it
comes back to GRUB, `confirm` records `phase=failed`, and nothing re-arms.

What this does **not** decide yet: what `confirm` should do with the old entry
on the other disk. Today `confirm` demotes it within `BootOrder` and leaves it
in place, which is the right answer while that entry is also how Windows is
reached.

### What boots through systemd-boot today, measured

| | state |
| --- | --- |
| `bootc install --bootloader systemd --composefs-backend` | installs, and the guest reaches a login prompt under OVMF |
| `bootc upgrade` on that machine | writes a correct new entry; dedupes the ESP directory when kernel+initramfs are unchanged, writes a second one when the initramfs moves |
| rollback by boot counter | **proven**: at `+0-3` sd-boot selected the previous deployment by itself, with `systemd-bless-boot` masked throughout |
| `systemd-bless-boot` on the Rime image | **was failing on an enforcing SELinux denial; repaired and proven in a boot** (see below) |
| Secure Boot on this path | not measured yet; every lab boot was non-SB OVMF |
| a sealed UKI on this path | not built yet; `bootc container ukify` exists and has not been run |
| the ESP an existing Rime machine has | **big enough since the slim initramfs**: the requirement fell from ~1.1 GiB to 350 MiB (100.9 MiB per deployment, image `44c9a5cb`), and katana's `precheck` answers `OK esp-space: 503 MiB free, 350 MiB needed`. It was too small before: 600 MiB on the L16 against ~1.1 GiB |

### `systemd-bless-boot` cannot rename an entry on a FAT ESP. Measured.

Without blessing there is no pivot: the counter would run every deployment down
to `+0-N` and roll it back, which is the failure the old default was avoiding.
On the Rime image, installed composefs + systemd-boot, with the entry renamed
to carry a counter, the blessing fails:

```
systemd-bless-boot[1609]: Failed to rename
    '/loader/entries/bootc_fedora-43-1+2-1.conf'
 to '/loader/entries/bootc_fedora-43-1.conf': Permission denied
audit: avc: denied { rename } for comm="systemd-bless-b"
    name="bootc_fedora-43-1+2-1.conf" dev="vda2"
    scontext=system_u:system_r:init_t:s0
    tcontext=system_u:object_r:dosfs_t:s0 tclass=file permissive=0
```

A mount option or FAT itself would have failed for any process, and they did
not: in the same boot, `mv` on the same file in the same directory succeeded,
and re-running the same binary from a shell succeeded (`Marked boot as 'good'`,
rc=0). The difference is the SELinux domain, and the cause lies in Fedora's
policy rather than in a Rime mistake:

* `/usr/lib/systemd/systemd-bless-boot` is labelled `init_exec_t`, so when PID 1
  runs it the process stays in `init_t`. Fedora 43's policy has
  `systemd_bless_boot_generator_t` for the *generator* and **no domain at all
  for the worker**.
* Queried out of the live policy with `setools`, `init_t` has no `rename`,
  `create`, `unlink` or `open` on `dosfs_t:file`, and no `add_name`/`write` on
  `dosfs_t:dir`. It has none of them on `boot_t` either, so relabelling the ESP
  with a `context=` mount option would not have helped.
* On a GRUB machine this never comes up, because `bootupd` writes the ESP, and
  its domain `bootupd_t` **does** have full management of `dosfs_t`. Fedora
  built that domain for this job.

Rime's fix sends the blessing into that domain rather than widening `init_t`:
a drop-in sets `SELinuxContext=-system_u:system_r:bootupd_t:s0` on
`systemd-bless-boot.service`, and a one-rule policy module grants the only
permission the transition is missing: `bootupd_t` may use `init_exec_t` as an
entrypoint. The `daemon` attribute already allows
`init_t → bootupd_t:process transition` and `bootupd_t`'s journal and fd
access; that was checked in the policy, not assumed.

**It is proven in a boot as well as in the policy.** Same image lineage, same
backend, same enforcing SELinux as the run that failed, installed through
`tests/lab/bootc-install-lab` and booted twice under OVMF:

```
systemd-bless-boot[1210]: Marked boot as 'good'. (Boot attempt counter is at 1.)
systemd[1]: Finished systemd-bless-boot.service
LAB-enforce:       Enforcing
LAB-bless-result:  active / success
LAB-entries-after: bootc_fedora-43-1.conf      <- the +3-0 suffix is gone
LAB-avc:           <empty>
```

One caveat changes what that proves: **`bootupd_t` is a permissive domain
in Fedora 43**, one of 41. The kernel logs denials inside it and allows them.
The zero AVCs still mean something, because a permissive domain logs what it
would have denied and nothing was logged; but the drop-in alone would probably
work today even without the module. The module makes this correct rather than
tolerated, and keeps it working on the day that domain becomes enforcing.

**The symmetric change to `rime-boot-count` is wrong.** It was tried in the
same guest and reverted. `/usr/libexec` is `bin_t`, and the policy has
`type_transition init_t bin_t:process unconfined_service_t` while `init_exec_t`
has no transition at all. So the helper was already unconfined and could rename
a `dosfs_t` file unaided; forcing `bootupd_t` confined it into a domain whose
`proc_t:file` access is `map` only, and the guest logged
`avc: denied { read } comm="cat" path="/proc/cmdline" scontext=bootupd_t` while
still appearing to pass, because the domain is permissive. `/proc/cmdline` is
where the counter reads the booted composefs digest, so on an enforcing
`bootupd_t` it would silently stop writing counters. `Containerfile.base` and
the test suite now assert that unit carries **no** `SELinuxContext=`.

The `-` prefix covers one family of failure, and only one.
`systemd.exec(5)`: with the dash, *"failing to set the SELinux security context
will be ignored, but it's still possible that the subsequent execve() may fail
when the security policy doesn't allow the transition."* So a machine with
SELinux disabled or permissive, or a policy with no `bootupd_t`, blesses
unconfined and is fine. A machine that is **enforcing with the module missing**
is not: `setexeccon` succeeds, the kernel denies the `execve`, the unit fails,
and the deployment rolls itself back on the fourth boot. The guard against that
is `Containerfile.base`, which asserts across the tier boundary that
`semodule -l` still lists `rime_sdboot`. The policy module and the drop-in are
a pair, and removing either one alone is the dangerous edit.

### Per-machine configuration under a signed UKI

Other units build against this decision. Under a sealed UKI the
initramfs is inside the signed PE, so **anything that must differ per machine
can no longer be regenerated locally**. `Containerfile.release` already
asserts the initramfs is baked at build time, and a signature makes that
irreversible rather than merely preferred. L-002's keymap is the concrete case:
a user who cannot type their passphrase on a `us` layout is stranded at the
LUKS prompt, and "regenerate the initramfs at install time" is what a
signed UKI forbids.

**Rime's mechanism is systemd credentials placed on the ESP.** The kernel
command line stays image-static inside the UKI; nothing per-machine goes into
it.

Why, read out of the shipped systemd (258.10) rather than from memory:

* **A per-machine cmdline is impossible.**
  `systemd-stub(7)`: *"If UEFI SecureBoot is enabled and the `.cmdline` section
  is present in the executed image, any attempts to override the kernel command
  line by passing one as invocation parameters to the EFI binary are ignored."*
  So on a Secure Boot machine the `options` line of a loader entry is dead, and
  a machine-specific cmdline would need a machine-specific signature.
* **The cmdline does not need to be per-machine on this path.** Measured from
  the lab guest's own `/proc/cmdline` on a composefs install:
  `rw <kargs> composefs=<verity>`, with no `root=` and no per-machine UUID. The
  composefs digest is a property of the image being booted, so it belongs
  inside the signed PE.
* **UKI addons are ruled out for per-machine use.** `systemd-stub(7)`: addons
  *"will be validated using keys in UEFI DB, Shim's DB or Shim's MOK, and only
  loaded if the check passes"*. Rime's signing key is a CI secret that must
  never reach a user's machine (`AGENTS.md`, and boot-path rule 4), so a
  machine cannot produce an addon its own firmware will accept. Addons remain
  useful for *image-wide* configuration signed in CI, which buys nothing over
  putting it in the UKI.
* **Credentials are not signature-gated, and have consumers already.** The stub
  collects `/loader/credentials/*.cred` and `<uki>.efi.extra.d/*.cred` into
  `/.extra/global_credentials/` and `/.extra/credentials/` in the initrd, and
  measures them into **PCR 12**. `systemd.system-credentials(7)` in this same
  systemd lists `vconsole.keymap`, `vconsole.font`, `cryptsetup.passphrase`,
  `system.hostname`, `fstab.extra`, `systemd.unit-dropin.*`, `udev.rules.*` and
  more as well-known credentials, so the keymap fix is a file the installer
  writes, not a dracut change.

Choosing credentials brings rules, and none of them is optional:

1. **A plaintext `.cred` on the ESP is unauthenticated.** Anyone who can write
   the ESP can change it. It is measured into PCR 12, so a TPM policy bound to
   PCR 12 notices, but nothing refuses to boot on its own.
2. So: **non-secret machine settings** (keymap, console font, hostname) may be
   plaintext. **Anything that unlocks the disk** must be
   `systemd-creds encrypt --with-key=tpm2`, which only this machine's TPM can
   decrypt, and which therefore cannot be lifted off the ESP onto another box.
3. **LUKS is discovered, not named.** With the `options` line dead under Secure
   Boot, `rd.luks.uuid=` and `root=UUID=` are both unavailable. The replacement
   is GPT Discoverable Partitions: `systemd-gpt-auto-generator(8)` in this
   systemd states that in the initrd an encrypted `/` is set up as
   `/dev/mapper/root` with a generated `sysroot.mount`, from the partition
   **type GUID** alone, and measures the volume key into PCR 15 when the kernel
   was booted through `systemd-stub`. gpt-auto stands down if `root=` is on the
   cmdline, which on this path it is not.

**The contract `luks-installer` builds against**, therefore:

* the root partition must carry the x86-64 root type GUID
  `4f68bce3-e8cd-4db1-96e7-fbcaf984b709`, so gpt-auto finds it with no kargs;
* the installer must not add `root=`, `rd.luks.uuid=` or
  `rd.vconsole.keymap=` to the command line: a Secure Boot machine ignores
  them, and on a non-Secure-Boot one they are a trap, because they work there
  and stop working when Secure Boot is turned on;
* the keymap is written as `vconsole.keymap` in
  `<ESP>/loader/credentials/vconsole.keymap.cred`, plaintext, at install time;
* the TPM2 unlock path stays `rime-luks-enroll`'s; a passphrase handed to the
  initrd as a credential must be TPM2-encrypted.

### How reversible this is for an existing machine: it is not, and here is why

> **Superseded on its headline claim, kept for its numbers.** "A migration is
> impossible, back up and reinstall" was the conclusion when this was written,
> and it is wrong: `bootc install to-existing-root --composefs-backend
> --bootloader systemd` converts a running machine in place, and Rime drives it
> from `rime boot migrate` (from `rime update` until 2026-11-01). Read "Migrating a machine that already exists" above for
> what happens. What survives here is the **ESP arithmetic**, which was then
> why the migration refused on the L16 (the slim initramfs retired it; see
> "When it refuses").

"Additive before destructive" is achievable as a **one-shot test** and is not
achievable as a migration.

*What is possible today* is the enrolment procedure further down this document:
build a UKI from the machine's current deployment, `bootctl install` onto the
existing ESP, add a new `BootXXXX`, and use `BootNext` so the first reboot is a
single test with GRUB untouched and still first in `BootOrder`. That is a real
additive test, and it is the right way to see systemd-boot boot this hardware.

That procedure is not a migration, for three measured reasons:

* **`bootc upgrade` will not maintain it.** On the ostree backend, updates keep
  writing BLS entries under `/boot` on btrfs, which sd-boot cannot read. The
  hand-built UKI is a snapshot of one deployment and goes stale at the next
  update.
* **The ESP does not fit two deployments.** The L16's kernel + initramfs is
  16.9 MB + 375.6 MB = **374 MiB per deployment**; bootc keeps booted and
  rollback, plus a third transiently while an update stages: about **1.1 GiB**.
  The L16's ESP is **600 MiB**, and an ESP cannot be grown in place without
  moving the partition after it. bootc's own composefs default is 1 GiB, which
  is itself under the transient peak, so Rime must ask for more than bootc's
  default. The two unused 2 GiB XBOOTLDR partitions on the L16 cannot absorb
  this, and the reason is bootc rather than systemd-boot (see the next
  section).
* **ostree → composefs has no converter.** So switching an existing machine is
  a reinstall with repartitioning, and its reverse is another reinstall.

What this section told a user then: **migrating an existing Rime machine to
systemd-boot means backing up and reinstalling.** New installs get it from the
installer, which creates the partition table anyway and can size the ESP
correctly the first time.

### SUPERSEDED IN PART, 2026-09-21: Rime builds its own ESP

Everything below about XBOOTLDR and about squeezing into a 512 MiB ESP still
holds, and the measurements stand. The question changed.

Andre decided that Windows-side installs boot systemd-boot like every other
machine, and that **Rime gets its own EFI System Partition rather than writing
into Windows'**. The 68.3 MiB-free figure stops being a constraint to defeat
and becomes a reason not to borrow the partition at all. See
`docs/rime-owns-its-esp.md` for the decision, what already exists to implement
it, and the four things that must be measured before it is claimed to work.

The 512 MiB ceiling still binds on machines whose ESP is already Rime's own
(the L16 and every existing install), so the initramfs work is not retired.

**Done, 2026-09-22.** The image built at `44c9a5cb` costs 100.9 MiB per
deployment against 375.2 MiB, so `need` falls from 1173 MiB to 350 MiB.
katana, booted on it, passes every `precheck` check with 153 MiB of ESP to
spare. A 512 MiB ESP is no longer a ceiling anything runs into.

### XBOOTLDR: the Boot Loader Specification allows it, bootc does not implement it

The obvious escape from the ESP arithmetic above is the one the Boot Loader
Specification was written for: put `vmlinuz` and the initramfs on an **XBOOTLDR**
partition (GPT type `bc13c2ff-59e6-4262-a352-b275fd6f7172`) and leave the ESP
holding the loader. On the L16 that would cost nothing at all: `p2` is a 2 GiB
`EA00` partition, unmounted and referenced by nothing, sitting immediately after
the 600 MiB ESP.

**systemd-boot would read that. bootc will not write it.** The measurement
clears the loader: `strings` on the same
`systemd-bootx64.efi` that `bootc install --bootloader systemd` wrote onto a
lab guest contains `config_load_xbootldr`, and `bootctl` carries
`--boot-path=` and `--print-boot-path` for exactly this. The actor is bootc's
composefs backend, not sd-boot, and here are the call sites so nobody has to
re-derive them:

* `crates/lib/src/spec.rs:288`: `--bootloader systemd` maps to
  `BootloaderKind::BLSCompatible`.
* `crates/lib/src/bootc_composefs/boot.rs:751` and `:775`, in
  `setup_composefs_bls_boot`. The two arms are **not** symmetric. `GRUBClassic`
  asks `root.is_mountpoint("boot")` and roots the entry paths accordingly.
  `BLSCompatible` never asks: it calls `mount_esp_writable(&esp_device)`
  unconditionally, writes the kernel and initrd to `<ESP>/EFI/Linux/`, the
  `.conf` to `<ESP>/loader/entries/`, and sets `abs_entries_path` to
  `/EFI/Linux`, absolute to the ESP.
* `crates/lib/src/bootc_composefs/boot.rs:1422`: *"TODO: support XBOOTLDR …
  bootc does not yet detect or use XBOOTLDR in the composefs install path, so
  unconditionally mount the ESP at /boot for now."*
* `crates/lib/src/bootloader.rs:287`: *"If we supported XBOOTLDR in the
  future, that'd go here with `--boot-path`."*
* `crates/lib/src/install/config.rs:74`: `BasicFilesystems` carries both
  `xbootldr` and `esp` as commented-out TODOs, and the struct is
  `deny_unknown_fields`, so bootc **rejects** an install config that names
  either, rather than ignoring it. There is no way to ask for an XBOOTLDR and no
  way to ask for a bigger ESP.
* `crates/lib/src/store/mod.rs:396`: *"NOTE: Handle XBOOTLDR partitions here
  if and when we use it"*, in the **runtime** store. This one kills
  the workaround of letting bootc write to the ESP and moving the files
  afterwards: `boot_dir` is the ESP for every later `bootc upgrade`, `status`,
  `rollback` and `image delete`, so a relocation breaks the first steady-state
  update and would have to be re-applied forever.

Still open on upstream `main` (`65321ae`, 2026-09-21), and identical in **1.16.4**
(the version inside `rime-os:daily` when this was read, and so the one that ran
the migration then) as well as 1.16.10 and 1.16.13. Upstream states the
design in words in merged PR **#2440** (2026-09-16): *"GrubCC and SystemdBoot
both do not store anything inside of `/sysroot/boot` and will (should) always
have the ESP mounted at `/boot`."*

Three corollaries follow:

1. **Rime is already on Type #1.** `BootType::Bls` is the default, and an image
   shipping a plain kernel under `/usr/lib/modules` gets it: that is phase 1,
   below. "Move to Type #1 entries" is not an available change; the files are
   already loose `vmlinuz` + `initrd` with a `.conf`. The only variable is which
   partition they sit on, and bootc fixes that to the ESP.
2. **Secure Boot is unaffected either way.** ESP and XBOOTLDR hold the same
   bytes; the firmware validates the kernel PE through `LoadImage` wherever it
   sits, and the initrd is outside the signature in phase 1 regardless. The
   `secure-boot-unsigned-loader` refusal is about `systemd-bootx64.efi` itself
   and is orthogonal.
3. **An upstream fix is a six-site change**: `boot.rs:775`, `boot.rs:1422`,
   `bootloader.rs:287`, `store/mod.rs:396`, `status.rs:408`, and
   `finalize.rs:149` / `delete.rs:159`, all keyed off the same `boot_dir`
   decision. Rime must not carry that as a fork.

`ROADMAP/evidence/sdboot-xbootldr-20260921.md` has the full transcript, the
measured ESP of a Rime composefs guest, and what this means for a Windows
dual-boot machine.

### Legacy BIOS: what "all machines" costs

systemd-boot is a UEFI application. There is no BIOS systemd-boot, so a machine
that boots legacy BIOS cannot be on this path at all. What Rime supports today,
checked in the tree:

* `installer/rime-install` has two modes. **Disk mode** runs `bootc install
  to-disk --wipe`, and bootc lays out the table itself, including a **1 MiB
  BIOS boot partition on every install, UEFI or not, on both backends**.
  An encrypted disk-mode install builds its own table instead
  (`docs/disk-encryption.md`), and gives it a 1 MiB BIOS boot partition too.
  **Partition mode** runs `bootc install to-filesystem` into a root the
  installer made and an ESP that already exists, so it creates no table and no
  BIOS boot partition either.
* `bootupd` ships a BIOS payload (`/usr/lib/bootupd/updates/BIOS.json`).
* **The L16 has no BIOS boot partition at all.** `lsblk -o PARTTYPENAME` shows
  an EFI System partition, two `Linux extended boot` partitions and two
  filesystems, and nothing else. It was installed in partition mode. Its
  `/boot/bootupd-state.json` records only the `EFI` component, so even where
  the partition does get created, nothing is written into it.
* The **live ISO boots legacy BIOS**: `installer/build-live-iso.sh`
  builds an El Torito core image and an isohybrid MBR with a VESA `vga=791`
  handoff specifically for BIOS.

So the cost is smaller than it looks, and it still needs saying: **Rime's
installer media boots legacy BIOS; no Rime installation is known to have ever
booted legacy BIOS from disk**, because on whole-disk installs the bootloader
was never written there and on partition-mode installs the partition does not
exist. The pivot therefore drops a configuration that was created but never
functional, and on this laptop was not even created. If BIOS-from-disk is ever
wanted, it is GRUB on the ostree backend. The two paths cannot be the same
image's default, and choosing between them is a product decision, not a build
flag.

### Two phases, and which one ships first

Everything measured so far is **phase 1**, and the two phases are not the same
machine. Knowing which is which decides whether `luks-installer` writes a karg
that works or one that is ignored.

**Phase 1: unsealed Type #1 entries.** What `bootc install --bootloader
systemd --composefs-backend` produces today: a loose `vmlinuz` and `initrd`
under `/EFI/Linux/bootc_composefs-<verity>/`, a `.conf` in
`/loader/entries/`, and the kernel command line on that file's `options` line.
This is what installs, boots, upgrades and rolls back in the lab. `bootc status`
calls it `bootType: Bls`. The initramfs is not signed and not measured
differently from what GRUB does today, so **a karg still works here**, and
that is the trap, because it stops working in phase 2.

`rime-boot-count` is a phase-1 program: it parses `options` lines and renames
`.conf` files.

**Phase 2: sealed UKI.** Kernel, initramfs and command line inside one signed
PE in `/EFI/Linux/`, with no `.conf` and no `options` line. Upstream's term for
it is *sealed*: the composefs digest is inside the signature. `bootc container
ukify` and `bootc container split-kernel-and-rootfs` exist in bootc
1.16.10 (the version the image shipped when this was written) and neither has
been run. In phase 2 the credential mechanism stops being a preference and
becomes the only way to configure a machine.

**Rime ships phase 1 first and targets phase 2.** Phase 1 is a measured
install/boot/upgrade/rollback path that needs no firmware enrolment; phase 2
needs a signing decision nobody has taken yet (below). Two consequences follow:

* Anything built on "a karg will carry this" is building on phase 1 only. Do
  not do it for per-machine values. Use a credential, which works identically
  in both phases.
* **Boot counting has to be redone for phase 2.** A UKI is counted by renaming
  the `.efi` itself (`rime-<id>+3-0.efi`), not a `.conf`, and how `bootc`
  names and stages UKIs is unmeasured. `rime-boot-count` will need a second
  branch, and its test suite a second fixture shape.

### Secure Boot: a decision, not a measurement

Gate 1 below is usually written as "measure Secure Boot on this path". It is
a design decision that nobody has taken, and you can see it in the ESP
`bootc` produces:

```
/EFI/BOOT/BOOTX64.EFI              <- systemd-boot itself, as the fallback
/EFI/systemd/systemd-bootx64.efi   <- systemd-boot
```

There is **no shim and no `/EFI/fedora`**. On a machine whose `db` holds the
stock Microsoft CA (every machine out of the box), neither of those binaries
validates, because `systemd-boot-unsigned` is what its name says. The two ways
out are different products:

1. **Rime-signed sd-boot, enrolled by the user.** Sign
   `systemd-bootx64.efi` and the UKI with the Rime MOK in CI, and every user
   enrols the Rime certificate in their firmware once. That is boot-path rule
   4's documented, user-initiated procedure, and it is a real cost: a
   per-machine manual step with a firmware UI in it.
2. **shim → sd-boot.** Keep Fedora's Microsoft-signed shim as the first stage
   and have it load a RIME-MOK-signed systemd-boot. Fedora's shim is built
   with `grubx64.efi` as its second stage, so this means either a shim built
   with a different second stage, or shipping sd-boot under that name. And
   `bootc --bootloader systemd` lays down neither shim nor that filename, so
   Rime would have to author the ESP rather than bootc.

Option 2 is the only one that keeps "install and it boots" true for a user who
never opens firmware setup. Option 1 is the only one that needs no new
bootloader plumbing. **This is the decision to take before phase 2**, and it is
a product decision, not a build flag.

### What this needs from the kernel build

`kernel-build` owns the kernel now, which makes UKIs easier: the
kernel, the initramfs and the signing are all Rime's. This path needs the
kernel build to expose three things, listed now so they are designed in rather
than retrofitted:

1. **`vmlinuz` and `initramfs.img` stay at `/usr/lib/modules/<kver>/`.**
   `bootc container ukify --kernel-dir` expects exactly that shape
   (`/parent/$kernel_version`), and `Containerfile.release` already asserts the
   initramfs is there. Do not move them to `/boot`.
2. **The MOK signing step must be reusable for an arbitrary PE binary, not only
   `vmlinuz`.** Phase 2 signs a UKI, and the Secure Boot decision above may
   also need `systemd-bootx64.efi` signed. A signer that only knows how to sign
   a kernel image has to be rewritten at the worst possible moment. It should
   also be able to attach a `.sbat` section, which makes a signed artifact
   revocable by SBAT policy instead of by DBX.
3. **The signature AND its gate move to the UKI the moment a UKI exists.**
   Getting this one wrong produces an unbootable machine with every assertion
   still green, so it is written as a rule rather than a preference:

   > Once a UKI is the object the firmware loads, signing the inner `vmlinuz`
   > signs something the firmware never validates. The kernel is signed, the
   > check that looks at the kernel passes, and the thing that boots
   > is unsigned. Nothing goes red until a real machine refuses to boot.

   The kernel unit (merge `74b777ac`) measured the pieces this needs:
   `vmlinuz` is a real `pei-x86-64` PE with
   `CONFIG_EFI_STUB=y` asserted, `sbsign` runs on it with rc=0 and `sbverify`
   confirms, and `/usr/share/rime-os/kernel/{build.txt,kernel.pin}` carry
   consumable provenance. So the signing path works end to end; what must not
   happen is the signature staying on the kernel once the UKI becomes the boot
   object. `AGENTS.md`'s Secure Boot invariant (read the signature out of the
   built artifact) then means `sbverify` against the **UKI**.

   Two facts from that unit that this design must get right:

   * **The kernel image is `FROM scratch`, RPMs only, 186.9 MiB, and no user
     ever downloads it.** The fleet update cost is unchanged by it. Nothing in
     `docs/update-cost.md`'s accounting for this pivot moves because of the
     kernel.
   * **There is no shell in that image.** `build-local.sh` read a verdict
     through `/bin/sh` on it, always got an empty string, and would have
     aborted a successful 74-minute build. Anything here that inspects the
     kernel image must read files out of it, not shell into it.

### Before this is pointed at katana or the L16

In order, and each one is required:

1. **The Secure Boot chain decided, then measured.** bootc's ESP has no shim
   and no `/EFI/fedora`, and `systemd-boot-unsigned` is unsigned, so on a stock
   `db` nothing on that ESP validates. Pick between a Rime-signed sd-boot the
   user enrols and a shim → sd-boot chain Rime authors itself (see above), then
   measure it in the VM lab under OVMF. Nothing above was measured with Secure
   Boot on at all.
2. A sealed UKI built by `bootc container ukify` from the Rime image, booting
   in that guest, with the credential mechanism above changing the keymap at a
   LUKS prompt. **The same change must move the MOK signature
   and the `sbverify` gate from the kernel to the UKI**, in one step: a build
   that emits a UKI while still signing and verifying only the inner kernel is
   green and unbootable.
3. ~~The blessing fix proven on the Rime image.~~ **Done, 2026-09-20**: entry
   counted, booted, suffix stripped, zero AVCs, `LAB-bless-result: active /
   success`. What remains from this gate is the cheap half: the *reverted*
   `rime-boot-count` renaming a staged entry in a guest. Both things it rests
   on are measured; the shipped combination has not been through a boot.
4. A Windows-entry assertion: an `efibootmgr -v` capture before and after an
   install into a lab disk that carries a `Windows Boot Manager` entry,
   compared with `cmp`, failing if anything moved. Katana's Rime `Boot0000`
   lives on the **Windows** ESP, so this assertion protects two operating
   systems rather than one.
5. An ESP sizing decision. When this list was written, 1 GiB was under the
   peak Rime needs; since the slim initramfs of 2026-09-22 the peak is 350 MiB
   (see "When it refuses").

Until all five are done, **no real machine is migrated**, and the L16 is not a
candidate at all: it was unbootable on the morning of 2026-09-20 because a
loopback install saw the host's EFI variables (`BOOT-BREAKAGE-2026-09-20.md`).

### What the image does today

The old default is still what ships. Nothing in the image runs
`bootctl install`, `bootctl update`, `bootupctl`, `grub2-install` or
`efibootmgr -c`, and `tests/test-boot-v2.sh` scans every shipped unit and
helper for those commands on **executable** lines and fails the build if one
appears. What the pivot has added to the image so far is the machinery that has
to exist *before* a default can move: `systemd-boot-unsigned` and
`systemd-ukify` in `Containerfile.core`, the blessing fix, the boot-counting
unit below, and `rime-boot-migrate`, which `rime boot migrate` runs, when asked,
to move a healthy machine that passes its precheck (see "Migrating a machine
that already exists").
Enrolment is still the human procedure further down.

The two units that implement boot counting carry
`ConditionPathExists=/sys/firmware/efi/efivars/LoaderBootCountPath-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f`.
That variable is set only when systemd-boot booted the machine **with a boot
counter in effect**, so on a GRUB machine neither unit starts, and a failed
condition is a skip rather than a failure.

The condition names `LoaderBootCountPath` specifically, and the rest of the
design depends on that choice. An earlier note here said the laptop had no
`Loader*` variables at all and only the katana did; re-checked 2026-09-20,
**that is wrong, and wrong in the direction that matters**. The L16 carries `LoaderInfo`,
`LoaderDevicePartUUID` and `LoaderSystemToken` too, and its `LoaderInfo` reads:

```
$ dd if=/sys/firmware/efi/efivars/LoaderInfo-4a67b082-… bs=1 skip=4 status=none | tr -d '\0'
GRUB 2.12
```

GRUB 2.12 sets `LoaderInfo` itself. A condition written against it (the
obvious "is systemd-boot involved" test) would fire these units on **every**
Rime machine, all of which boot GRUB. `LoaderBootCountPath` is set only by
systemd-boot and only when a counter is in effect. Any further conditioned unit
must use the same variable, or `entries.srel` where it cannot (see
`rime-boot-count` below).

### Boot counting has to be written by Rime, because bootc writes none

Four installs and two upgrades produced entry filenames of the form
`bootc_<osid>-<ver>-<N>.conf` with **no `+N-M` counter**, and a `loader.conf`
whose `timeout` and `console-mode` are both commented out. On a machine
installed exactly as bootc leaves it, `LoaderBootCountPath` never appears,
`rime-boot-health.service` and `rime-boot-notice.service` are both skipped by
their condition, `systemd-bless-boot` never runs, and there is no automatic
rollback. The health gate is inert until something writes the counter.

Renaming the live entry does not survive an upgrade: `bootc upgrade` writes a
fresh, uncounted set into `/boot/loader/entries.staged/` and
`bootc-finalize-staged.service` replaces the whole `entries/` directory at
shutdown, discarding anything Rime put in a filename. Renaming inside
`entries.staged/` **does** survive: measured across five boots, ending with
sd-boot selecting the previous deployment by itself at `+0-3`.

So Rime ships `rime-boot-count.service`: it renames the newest staged entry to
carry `+3-0`, and it runs at shutdown ordered so that its work happens **before**
`bootc-finalize-staged.service` swaps the directory in. It is a rename in a
directory bootc is about to install, not a write to a live boot path, and it is
conditioned on `/boot/loader/entries.staged` existing so it is inert on a GRUB
machine and on a machine with nothing staged.

## What exists

| piece | file | what it does |
| --- | --- | --- |
| UKI builder | `files/scripts/boot-v2/rime-mkuki` | kernel + initramfs + signed cmdline + microcode + `.rimeinf` metadata in one PE image |
| stage a Rime root | `files/scripts/boot-v2/rime-stage-root` | copies kernel/initramfs/os-release out of a booted deployment or image |
| ESP authoring | `files/scripts/boot-v2/rime-mkesp` | systemd-boot at `/EFI/APEX/`, UKIs at `/EFI/Linux/rime-<id>+N-M.efi` |
| ephemeral keys | `files/scripts/boot-v2/rime-sb-keys` | Secure Boot, PCR-policy and deliberately-untrusted keypairs |
| SB firmware vars | `files/scripts/boot-v2/rime-sb-vars` | an OVMF varstore with the Rime certificate as the only `db` entry |
| LUKS2 + TPM, shipped | `files/system/libexec/rime-luks-enroll` | the enrolment path a machine uses: a recovery key always, and a TPM slot bound to whichever policy this machine can enforce |
| LUKS2 + TPM, lab | `files/scripts/boot-v2/rime-luks-enroll` | signed PCR 11 policy plus a recovery key, against a software TPM. A different program from the row above; see "TPM-bound unlock" |
| VM harness | `files/scripts/boot-v2/run-scenarios` | 21 scenarios (`--list`): 12 in the default run and 9 requested by name. The guests boot under Secure Boot enforcing; the four `enroll-*` scenarios need only swtpm and cryptsetup and boot no guest |
| health gate | `files/system/libexec/rime-boot-health` | the `boot-complete.target` gate, and the rollback notice |
| boot counter | `files/system/libexec/rime-boot-count` | renames the STAGED entry to `+3-0` so bootc's uncounted set gets a counter; picks the entry by composefs digest, refuses on ambiguity |
| blessing repair | `files/system/selinux/rime_sdboot.te` + `files/system/units/10-rime-bless-boot-esp.conf` | lets `systemd-bless-boot` rename an entry on a FAT ESP, which `init_t` cannot |
| reporting | `rime boot status` | read-only; what verified this boot and what the counter believes |
| migration, asked for | `rime boot migrate` (root), or `rime update --migrate-boot` | the gate in front of `rime-boot-migrate auto`: runs it only when asked and the machine is healthy; names a failed earlier attempt (`rimed/rime/src/ops.rs`, `migration_gate`) |
| CI | `.github/workflows/boot-v2.yml` | builds the lab, boots the scenarios, publishes nothing |

Build-time tooling (`ukify`, `qemu`, `sbsign`, `swtpm`) lives in the
`rime-bootlab` container built from `bootlab/Containerfile`, never as packages
on a host. Installing it onto a Rime box is the machine drift `AGENTS.md`
prohibits, and the build box is a real Rime machine.

## The ESP layout, and the one directory Rime does not own

```
/EFI/APEX/systemd-bootx64.efi        the loader, at a Rime-owned path
/EFI/BOOT/BOOTX64.EFI                the removable-media fallback
/EFI/Linux/rime-<deployment>+N-M.efi the UKIs
/loader/loader.conf                  timeout 0, editor no
```

§22 asks for "Rime-owned EFI paths, not Fedora-named paths". `\EFI\Linux` is
the systemd-boot interface (a spec path, unlike the vendor name `\EFI\fedora`
that §22 is reacting to), and Rime owns the **filenames**, which are what the
menu, `bootctl list` and `rime boot status` display.

A fully `/EFI/APEX`-named UKI path is also possible, and a measurement made the
choice. Measured with systemd-boot 258.10-1.fc43:

| entry | boot counter applied? |
| --- | --- |
| type #2, `/EFI/Linux/rime-t2+3-0.efi` | **yes** → `+2-1` |
| type #1 `.conf` with `efi /EFI/APEX/uki/rime-t1.efi` | **no** (the entry booted fine) |
| type #1 `.conf` with `linux /EFI/APEX/uki/rime-t3.efi` | **yes** → `+2-1` |

So the entry type does not decide the tally: systemd-boot skips it for entries
named with the `efi` key. Type #2 is the default anyway, because `bootctl`
reports the tally, the `.osrel`-derived title and the embedded kernel version
only for entries it recognises as UKIs, and `rime boot status` reads
`bootctl list --json`. A type #1 layout would need Rime to author an entry file
per deployment *in addition to* the UKI, which is the two-artifacts-must-agree
drift a UKI exists to remove. The suite asserts all three rows, so a future
systemd that starts counting `efi` entries fails it instead of changing the
trade-off unnoticed.

## Boot counting and automatic rollback

Rime writes no boot counter, and cannot. The boots that need counting are the
ones that never reach userspace: a kernel that panics, an initramfs that
cannot find its root, a driver that hangs before the display comes up. Nothing
in userspace can increment a counter for a boot that never got there.

systemd-boot decrements the count **before the kernel starts**, by renaming the
entry file in the ESP. Rime contributes the definition of a healthy boot,
through the upstream extension point:

```
rime-boot-health.service   Before=boot-complete.target, RequiredBy= it
        ↓ non-zero exit
boot-complete.target       not reached
        ↓
systemd-bless-boot.service Requires=boot-complete.target — does not run
        ↓
the +N-M suffix survives   the next boot spends another try
        ↓ at +0-3
systemd-boot selects the previous, blessed entry (no suffix at all)
```

`RequiredBy`, not `WantedBy`: with `WantedBy` the target would be reached with
the check failing, which blesses a broken deployment, the outcome §22 forbids
with *"do not mark an update successful merely because the kernel started."*

**Health is an explicit short list.** It is neither `systemctl is-system-running`
nor upstream's `systemd-boot-check-no-failures`, both of which fail on *any*
failed unit. `AGENTS.md` requires optional hardware and services to fail without
degrading the boot transaction, so a Rime machine can legitimately be
`degraded`: an absent fan controller, a Bluetooth adapter that did not appear.
Rolling the OS back over one of those would be worse than the fault. The list:

* the default target (`systemctl get-default`) is active
* `rimed.service` is active
* `systemd-logind.service` is active
* the system bus (`dbus-broker.service`, or `dbus.service` if that is what the
  image has) is active
* `greetd.service` is active, **only** when the default target is
  `graphical.target`, because requiring a greeter on a deliberately headless
  machine would roll it back for working as configured

`rime-boot-notice.service` runs *after* `boot-complete.target`, so the notice
it writes always reads "you were rolled back and the machine is now working"
rather than appearing on a machine that is still failing. There is deliberately
no `rime boot ack` verb: the notice is cleared automatically on a boot where no
entry is exhausted, so it tracks reality instead of tracking whether somebody
dismissed it.

## Measured boot: the PCR policy, and the one that was rejected

The hard requirement is a policy that survives a legitimate kernel update. On
an image-based OS the kernel changes on every update, so a policy that breaks
when the kernel changes breaks on every update, and the user meets it at the
one moment they cannot get a shell.

**Chosen: a signed PCR 11 policy.** `ukify` computes the PCR 11 values the UKI
will produce in each boot phase, signs them with an RSA key, and embeds the
signatures as `.pcrsig` with the public half as `.pcrpkey`.
`systemd-cryptenroll --tpm2-public-key=` then binds the LUKS2 keyslot to the
**public key**, through a TPM2 `PolicyAuthorize`, rather than to a measurement.
Any UKI signed by that key satisfies the policy, so a kernel update needs no
re-enrollment and no user interaction. The trust anchor becomes a key Rime
already owns and already protects, the same shape as the Secure Boot chain.

**Rejected: `systemd-pcrlock`.** It predicts the firmware PCRs (0, 2, 4, 7)
from the TPM event log and writes the resulting policy into a TPM NV index.
It has three problems, all about the update path rather than the security model:

* `systemd-pcrlock make-policy` must be re-run, with TPM access, after every
  kernel install. A missed run is an unbootable machine.
* The policy lives in an NV index, so a firmware reset or a TPM clear loses it,
  and firmware resets are exactly the "hardware edge case" §22's step 6 wants
  proven before encryption goes on by default.
* It binds firmware measurements, so a UEFI update also invalidates it.

**The limitation**: signed PCR 11 attests the UKI and the boot *phase*.
It does not attest the firmware or the Secure Boot state. What stops an attacker
substituting their own UKI is Secure Boot enforcing, the layer that makes only
Rime-signed images loadable. The two are complementary and neither alone is
enough.

## What was measured

**Two machines and two dates, because one header covered both for four rounds
and had stopped being true.** The UKI, Secure Boot, reproducibility and
boot-counting figures are from the katana on 2026-09-03: a real Rime machine,
`VARIANT_ID=gaming`, kernel `7.1.5-cachyos1.fc43.x86_64`. The LUKS and TPM edge
cases below them, from 2026-09-14 onward, ran on the L16 against a root staged
from kernel `7.2.3-cachyos2.fc43.x86_64`, which is what each run's `.rimeinf`
line records. Every guest ran under `OVMF_CODE_4M.secboot` with only the
ephemeral Rime certificate enrolled as PK/KEK/db. Nothing here is a
prediction.

**A UKI from the real Rime image boots.** Kernel 16,758,856 bytes, the real
Rime initramfs 386,072,073 bytes, the signed UKI ~390 MB. sd-stub printed
`Booting initrd of Rime OS dracut-107-8.fc43 (Initramfs)`, the real Rime
initramfs ran to dracut's `pre-mount` hook, reported `NAME="Rime OS"` from
`/etc/initrd-release` and the command line the image was signed with, and
powered off cleanly. `.rimeinf` recorded `microcode=embedded-in-initrd`, which
is how §22's "microcode in the UKI" is satisfied on the real image: dracut
prepends an uncompressed `kernel/x86/microcode` cpio (`AuthenticAMD.bin` 304,866
bytes, `GenuineIntel.bin` 16,778,240 bytes) and `rime-mkuki` detects it rather
than demanding a duplicate `--ucode`.

**Secure Boot refuses everything else.** Unsigned, foreign-signed, and
one-byte-tampered-in-`.cmdline` UKIs each failed to reach userspace. Every
mutant is proven to exist and to differ from the original before the boot, and
the foreign one is proven to be *validly* signed by a key that is not in `db`;
"unsigned is refused" would be a much weaker claim.

**Reproducibility of the payload, not of the signature.** Two *unsigned*
builds with the same `SOURCE_DATE_EPOCH` are byte-identical, and a different
epoch changes the bytes, so the property is controlled rather than accidentally
true.

The **signed** UKI is not byte-reproducible and cannot be: `sbsign` records a
signing time in the PKCS#7 structure, and there is no flag to omit it. Measured
directly: one unmodified `linuxx64.efi.stub` signed twice, two seconds apart,
with one key, gave `c266048303a17056…` and `66d64b06a66db973…`.

This was found the hard way. The scenario originally asserted that two *signed*
builds were byte-identical. It passed repeatedly, then failed on a CI run whose
only changes were to documentation, because two signings match exactly when
they land in the same second. So the assertion now covers the unsigned payload,
the signed artifact is asserted to *verify* rather than to be identical, and the
harness reports whether the signed bytes happened to match without asserting it
either way: a lucky run must not read as evidence of a property that does not
hold.

**Boot counting.** Four boots walked `rime-new+3-0.efi` →`+2-1` →`+1-2`
→`+0-3`, the fifth selected the unsuffixed `rime-good.efi`, and a sixth stayed
there. Exact filename pairs are asserted at every step, in both directions: the
counted entry decrements and the blessed entry never grows a suffix.

**Measured boot and TPM-bound LUKS2**, with `swtpm`:

* the enrolled state unlocks, with PCR 11 at a real measurement (e.g.
  `4CBAF0A3342F8628AB22E15C8EEB4FF4A60DE4F2B8D9DAC52473943A24C4BCC3`), and the
  signature and public key delivered to
  `/run/systemd/tpm2-pcr-{signature.json,public-key.pem}`
* PCR 11 **changes** between two UKIs signed by the same PCR key, and the same
  keyslot still opens with no re-enrollment: the kernel-update requirement,
  measured rather than reasoned
* a UKI whose `.pcrsig` was made by a **different** key is refused
  (`Failed to unseal secret using TPM2`)
* the **recovery key unlocks in the same boot that was refused**, so "it
  refuses" and "it is recoverable" are two checks that met in one boot
* a 512-byte marker written through the mapper device in the first boot is read
  back in the second, so the volume decrypted rather than a device merely
  appearing

**TPM clear, firmware change, suspend/resume and a missing TPM**: L-001's
edge cases and L-002's, all against the same software TPM. Counts come from the
container logs with `State.FinishedAt` checked on each, so `Exited (0)` is not
taken at face value.

* **TPM clear: 22 passed, 0 failed.** Four boots, one UKI throughout, so a
  refusal cannot be the PCR policy in disguise. A real `TPM2_Clear` runs between
  boots 1 and 2, confirmed by the owner primary key's *name* changing rather
  than by `tpm2_clear` exiting 0. Boot 2 refuses the TPM unlock, the recovery
  key opens the volume in that same boot, and the plaintext marker written
  before the clear comes back, so the user has their disk and not an opened
  keyslot. It was 20 passed for two rounds: the guest emitted that marker line
  and this scenario read past it without asserting on it.
* **Firmware change: 18 passed, 0 failed, 1 could-not-run.** Fedora's real
  `DBXUpdate` blob moves PCR 7 (76 → 21340 bytes) and the signed PCR 11 policy
  still unlocks, which is why boot-v2 chose this policy. The
  could-not-run is PCR 0: moving it needs a second OVMF build differing **only**
  in code, and the lab image's two 4 MB builds differ in Secure Boot enforcement
  as well, so swapping them would conflate "the firmware changed" with "Secure
  Boot was turned off".
* **Firmware code change: 22 passed, 0 failed, 1 could-not-run.** The
  could-not-run above is now answered. Two Fedora builds of OVMF that differ in
  edk2 revision and in nothing else (`edk2-d46aa46c8361` (20250812) and
  `edk2-2970e5699ba6` (20260812), both Secure Boot, both 4 MB, both booted from
  a fresh copy of one varstore template) move PCR 0 from
  `0FA5AE84ABAB76C8D…` to `0E33803144670784C…`. PCR 11 does not move
  (`22C006FB6217982C0…` throughout), the signed PCR 11 policy still unlocks, and
  a control volume bound **by value** to PCR 0 refuses in that same boot. The
  512-byte marker written under the old firmware is read back under the new one.
  PCR 7 is **identical** across the two revisions (`933DE452FF745C293…`), which
  is worth knowing on its own: an edk2 version change moves the code register
  and leaves the Secure Boot policy register alone.

  The harness does not take the supplied firmware on trust. It reads the
  revision out of the binary rather than off its filename, and the firmware has
  to refuse an unsigned UKI before anything rests on it. Without that, a build
  with Secure Boot compiled out would move PCR 0 for the obvious reason and the
  run would report that a firmware update had not broken the policy, about a
  firmware that had stopped checking signatures. This unit's scratch directory
  contains that trap: a non-Secure-Boot build saved as `OVMF_CODE_4M.secboot.fd`.

  The could-not-run that remains is a real limit: an OVMF binary swap is not
  a vendor UEFI capsule. No microcode, no ACPI tables, no option ROMs and no
  management-engine firmware moved with it.
* **No TPM at all: 16 passed, 0 failed, 1 could-not-run.** L-002's wording is
  that the default must not strand users, and the case with no scenario was the
  machine where nothing answers: a TPM switched off in firmware setup, a board
  replaced under warranty, a disk moved. One volume, booted twice, with the TPM
  device as the only difference. With it, the TPM unlocks and writes the marker.
  Without it the guest reports `tpm-device=absent`, systemd says *"No TPM2
  hardware discovered and EFI firmware does not see it either, falling back to
  traditional unlocking"*, the recovery key opens the volume in that boot, and
  the marker written through the TPM path comes back. The qemu exit status is
  an assertion rather than a note in the log: a guest hanging on a TPM that will
  never answer strands the user the same way as one that refuses with no
  fallback.

  That boot does not show two things. The guest ran with `headless=1`, which
  turns a passphrase prompt into a refusal; on a real machine the same
  situation shows the user a prompt, and whether that prompt is legible is a
  plymouth question this lab does not touch. And a machine that has **never**
  had a TPM is a different case: it could not carry a `systemd-tpm2` token at
  all, so it never reaches either boot.
* **Suspend/resume: 17 passed, 0 failed, on edk2-20250812.** The volume stays
  open across S3, and a mapper created *after* the resume reads the marker back,
  so the plaintext came off the disk and not out of the page cache of a device
  that had been open since before the sleep. Two non-colluding witnesses: the
  guest's own `/sys/power/suspend_stats`, and qemu asked separately over QMP.
  `s3-mode=deep`, so it was S3 and not suspend-to-idle.

**The shipped OVMF build cannot resume from S3, and it is an edk2 regression.**
On `edk2-ovmf-20260812-4.fc43` the guest suspends and qemu wakes it (the QMP
record has `suspended_seen`, `wakeup_sent` and `resumed_seen` all true, with the
suspend at 107.656 s and the wake at 108.161 s), and then the firmware stops in
its own resume path:

```
SEC: S3 resume
PeiInstallPeiMemory MemoryBegin 0x7FF70000, MemoryLength 0x90000
PopulateMemoryTypeInformation: No Memory Type Information HOB found, S4 resume is likely to fail
ASSERT_EFI_ERROR (Status = Not Found)
ASSERT MemoryServices.c(203): !(((RETURN_STATUS)(Status)) >= 0x8000000000000000ULL)
```

A DEBUG build deadloops on that assert, so the harness kills qemu at the timeout
and the guest never prints anything at all. Changing one variable at a time
places it:

| build | edk2 | Secure Boot + SMM | S3 resume |
| --- | --- | --- | --- |
| in-image `OVMF_CODE_4M.secboot` | 20260812 | on | asserts |
| in-image `OVMF_CODE_4M` (non-secboot) | 20260812 | off | asserts the same way |
| `OVMF_CODE_4M.secboot` from edk2-ovmf-20250812-18.fc43 | 20250812 | on | resumes, 17/0 |

Turning Secure Boot and SMM off changes nothing; going back one edk2 version
fixes it. So the S3 criterion is measurable, on the older firmware, and neither
Rime nor the lab is at fault. `RIME_BOOTLAB_FW` points the harness at a
pre-converted firmware cache, which is how the harness takes the older build.
Each build's version comes out of the binary itself (`strings … | grep edk2-`),
not from its filename.

**Know this firmware fact before debugging anything here.** Fedora's 2 MB
`/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd` does Secure Boot but has **no TCG2
protocol**: sd-stub sets no `StubPcr*` variables, PCR 11 reads as 64 zeros, and
every TPM unlock fails with *"No signature for current PCR policy in TPM2
signature JSON"*, which reads exactly like a broken PCR policy. Only
`OVMF_CODE_4M.secboot.qcow2` measures (367 `Tcg2` lines in the firmware debug
log). `lib.sh` converts the 4 MB pair to raw once and refuses to fall back to
the 2 MB build.

## L-001, criterion by criterion, and the part a VM cannot do

L-001's acceptance is one sentence: *"Real TPM, firmware update, TPM clear,
suspend/resume, recovery key paths tested"*. The lab covers four of those five.
It cannot cover the first from a VM, which is what keeps L-001 `partial` and
L-002 and L-003 blocked behind it.

| criterion | scenario | counts | firmware | what it shows |
| --- | --- | --- | --- | --- |
| **Real TPM** | none | n/a | n/a | **nothing. Every row below is `swtpm`.** The silicon runs are in the MSI Katana section below |
| firmware update, Secure Boot policy (PCR 7) | `luks-firmware-change` | 18 / 0 / 1 | shipped 20260812 | Fedora's real `DBXUpdate` moves PCR 7 (76 → 21340 bytes); the signed PCR 11 policy still unlocks; a by-value PCR 7 control refuses in the same boot |
| firmware update, firmware code (PCR 0) | `luks-firmware-code` | 22 / 0 / 1 | 20250812 → 20260812 | two edk2 revisions differing only in code move PCR 0; PCR 11 and PCR 7 do not; the policy still unlocks; a by-value PCR 0 control refuses in the same boot |
| TPM clear | `luks-tpm-clear` | 22 / 0 | shipped 20260812 | a real `TPM2_Clear` between boots, confirmed by the owner primary key's *name* changing; the TPM then refuses the next boot, **the recovery key opens the volume in it**, and the marker written before the clear reads back |
| suspend/resume | `luks-s3` | 17 / 0 | **20250812 only** | the open volume survives S3 and a mapper created *after* the resume reads the plaintext back; two non-colluding witnesses; `s3-mode=deep` |
| suspend/resume | `luks-s3` | could-not-run | shipped 20260812 | the firmware asserts in its own S3 resume path: an edk2 regression, isolated one variable at a time |
| recovery key paths | `luks-tpm-clear`, `luks-no-tpm` | n/a | shipped 20260812 | refusal and recovery **in one boot**, twice, and the second reads the marker back, so it is the same disk and not an opened keyslot alone |
| no TPM at all (L-002's wording) | `luks-no-tpm` | 16 / 0 / 1 | shipped 20260812 | the TPM device is gone; systemd refuses the unlock instead of hanging; the recovery key produces the same plaintext |

Read the table with its one caveat in front of you: **an emulated TPM is the
subject of every row.** `swtpm` is a faithful implementation of the TPM 2.0
command set, so everything above is a real statement about Rime's policy, the
LUKS2 header shape and systemd's behaviour. It says nothing about anybody's
silicon.

### What silicon changes that no VM can substitute

`swtpm` lacks each of these mechanisms outright:

* **`Shutdown(STATE)` / `Startup(STATE)` across a real S3.** The TPM has to
  save and restore its volatile state through a power transition it does not
  control. Vendor fTPM bugs in that pair are why suspend/resume is on
  L-001's list at all. `swtpm` keeps its state in a file on a host that never
  slept.
* **A TPM clear issued from firmware setup under physical presence.** The lab
  issues `TPM2_Clear` over an mssim TCP socket from the host. On a machine the firmware
  asserts physical presence, on most boards behind a menu item that also resets
  other platform state, and on some boards that menu is the only way to do it at
  all.
* **PCR 0 from a vendor UEFI capsule.** `luks-firmware-code` swaps one OVMF
  binary for another. A real update ships microcode, ACPI tables, option ROMs
  and often management-engine firmware in one capsule, applied by the firmware
  to itself across a reboot it schedules. Both move PCR 0; only one of them can
  fail halfway.
* **Dictionary-attack lockout.** Real TPMs count failed authorisations and lock
  out, with vendor-set thresholds and recovery times. Nothing above ever
  triggered it. This matters the moment Rime adds a PIN to the unlock path.
* **fTPM and PTT quirks.** AMD's fTPM has a documented stutter; a BIOS update
  can reset an fTPM and take every sealed object with it, which is a TPM clear
  the user did not ask for; Intel PTT is a different implementation again. A
  discrete TPM chip is a fourth case.
* **Machines that do not offer S3.** The guest selected `deep` by name out of
  `/sys/power/mem_sleep`. Many current laptops offer only `s2idle`, and the
  question "does the TPM come back" has a different answer when the platform
  never fully slept. Check the L16 for `deep` before assuming it has S3.

### The run somebody with hardware would have to do

Run these steps rather than reading them. They destroy things: clearing a TPM
loses every sealed object on the machine.

**Not the L16.** Andre's laptop is out of scope for this program: its TPM is not
to be cleared and its boot path is not to be touched.

**Before anything:**

1. **The machine must already boot through a signed UKI.** The enrolment
   procedure below this section is that work. The check is required:
   `cat /sys/class/tpm/tpm0/pcr-sha256/11` must not be 64 zeros. On a stock
   Rime install it **is** zeros (measured on the L16 on 2026-09-13), because
   the shipped image boots GRUB through bootupd and only `sd-stub` extends
   PCR 11. A signed PCR 11 policy on a GRUB machine binds to nothing.
2. **Write the recovery key down, store it somewhere that is not that disk, and
   use it once before going further.** Print it, reboot, decline the TPM unlock,
   type it in. A key nobody has typed is a key nobody has tested.
3. **Know what else on the machine is bound to that TPM.** BitLocker on a dual
   boot, another Linux's LUKS, `systemd-creds` secrets, a TPM-backed SSH agent.
   `systemd-cryptenroll --tpm2-device=list` and
   `tpm2_getcap handles-persistent`, recorded before, is how you know what you
   are about to break. A TPM clear takes all of it.
4. **A live USB you have booted at least once**, and the key sequence for this
   machine's firmware setup menu.

**Run 1: the baseline.** Reboot and confirm the TPM unlocks with no prompt.
Record: `cat /sys/class/tpm/tpm0/pcr-sha256/{0,7,11}`, and
`cryptsetup luksDump <dev>` showing exactly one `systemd-tpm2` token and one
`systemd-recovery` token.

**Run 2: TPM clear.** Clear the TPM through the **firmware**, not with
`tpm2_clear` over the resource manager: the point is the path a user takes. On a
board whose `/sys/class/tpm/tpm0/ppi/tcg_operations` answers `5  4: User not
required` that path is `echo 5 > …/ppi/request` plus a reboot, and it needs
nobody at the machine; otherwise it is the firmware setup menu. Reboot. Record
that the TPM unlock is **refused**, that the recovery key opens the volume in
that same boot, and the three PCR values afterwards. Then re-enrol in **two
separate invocations** (wipe the old slot, then enrol) and record that the
`tpm2-blob` in the header changed. The single-command form reports success and
changes nothing; see the Recovery table below.

**Verify the clear; do not infer it from the reboot.** A firmware that silently
declined the request looks identical from the outside, and `ppi/response` is no
help: on the MSI board it reads `5 0: Success` the moment the request is
written, before any reboot. Read TPM state instead: `lockoutAuthSet`,
`tpm2_getcap handles-persistent`, `tpm2_getcap handles-nv-index`,
`TPM2_PT_NV_COUNTERS` and `TPM2_PT_LOCKOUT_COUNTER` all move. `tpmGeneratedEPS`
does **not**: `TPM2_Clear` leaves the endorsement seed alone, so an unchanged
`tpmGeneratedEPS` is not evidence the clear failed.

**Run 3: a real firmware update.** `fwupdmgr get-updates`, then
`fwupdmgr update`. Record PCR 0 and PCR 7 before and after, and whether the
signed PCR 11 policy still unlocks with no prompt. **This is the run a VM
cannot stand in for**, and the expected result is that unlock still works: the
keyslot is bound to the PCR signing key, and PCR 11 measures the UKI rather than
the firmware. Anything else is a finding worth the whole exercise.

**Run 4: suspend and resume.** With the volume open:
`cat /sys/power/mem_sleep` (record whether `deep` is offered at all),
`cat /sys/power/suspend_stats/success`, `systemctl suspend`, resume, then the
same counter again. Record that the volume still reads. Then close the mapper, reopen it through
the TPM, and record whether that **fresh** unseal works. Those are two
questions, and vendor firmware gets the second one wrong.

**Run 5: the TPM goes away.** Disable the TPM (or fTPM/PTT) in firmware setup.
Record that the boot **prompts for the recovery key rather than hanging**, that
the data is intact, and how long the prompt took to appear. Re-enable it and
record that TPM unlock returns with no re-enrolment: the sealed object is still
bound to the same SRK, which a clear would have destroyed and a disable does
not.

Each run's output belongs in this document, under a heading naming the machine,
its firmware version and its TPM (`tpm2_getcap properties-fixed | grep -i
manufacturer`). Until runs 1, 2 and 5 exist for one machine, nothing
satisfies L-001's word "Real", and L-002 and L-003 stay blocked behind it.

### MSI Katana GF76 12UG · firmware E17L3IMS.110 (AMI, 2025-09-08) · Intel PTT (`INTC`/`ADL`, fw 0x2580012)

2026-09-19. The first entry under that instruction. Full record, every row with
its command and its output: `ROADMAP/evidence/L-001-katana-tpm-20260919.md`.

Runs done on a **loopback LUKS volume**, because this machine's system disk is
not encrypted and converting it was out of scope. Secure Boot is **OFF** here,
so every PCR 7 row is provisional.

| run | verdict |
|---|---|
| 1: baseline enrol and unlock | **PASS.** One `systemd-tpm2` token, one `systemd-recovery` token. TPM unseal **0.29 s** (×3); recovery-key unlock 0.032 s; the same plaintext back through both |
| 2: TPM clear | **PASS**, with Andre's explicit approval, 2026-09-19. PPI operation 5 + a reboot, unattended over SSH, 29 s of downtime. Verified five ways: `lockoutAuthSet` 1 → 0, five persistent handles → none, thirteen NV indices → five, four NV counters → zero, DA counter 2 → 0. A volume enrolled **before** the clear then gave `tpm-unlock=REFUSED` → `recovery-unlock=SUCCESS` → marker identical, **in one boot**. Cost, authorised in advance: Andre's Windows Hello PIN. No data: there is no BitLocker |
| 3: real firmware update | **COULD-NOT-RUN.** `fwupdmgr get-updates` offers nothing for System Firmware on this board |
| 4: suspend and resume | **PASS.** s2idle, 90 s, RTC wake. Open volume survived and still read; a **fresh** unseal after resume worked in 0.295 s; all 24 PCRs byte-identical across the cycle. `deep` is offered and untested |
| 5: the TPM goes away | **COULD-NOT-RUN.** PPI op 2 (Disable) is also *"User not required"*, but once firmware hides the TPM the `ppi` directory goes with it, so re-enabling needs somebody in firmware setup. Not symmetric; not attempted |

Runs 1, 2 and 4 are done on this machine. **Run 5 is the only one left that
L-001's word "Real" still needs**, and it needs a hand on the hardware, not
another agent.

Four more things the lab could not have told us, from the Run 2 session:

5. **`ppi/response` is not a discriminator.** It reads `5 0: Success` the
   instant the request is written, before any reboot. Only TPM state proves a
   clear happened.
6. **The part returns two different codes for "TPM unlock failed", and systemd
   prints one sentence for both.** `0x18b` (`TPM_RC_HANDLE`) means the SRK at
   `0x81000001` is absent and is recoverable: the SRK is deterministic from the
   storage seed, so recreating it restores unlock with no re-enrolment. `0x1df`
   (`TPM_RC_INTEGRITY`) means the seed itself was rolled, i.e. the TPM was
   cleared, and re-enrolment is the only way back. Both surface as
   `Failed to unseal secret using TPM2: State not recoverable`.
7. **`systemd-cryptenroll` provisions the SRK; `systemd-cryptsetup` never
   does.** And `--wipe-slot=tpm2 --tpm2-device=auto`'s *"executing no
   operation"* is a no-op on the LUKS header only: it creates and persists a
   key at `0x81000001`, a handle shared with every other OS on the machine.
8. **A TPM clear moves PCR 1 permanently on this board.** The firmware's own
   log grew by three events on PCR 1, and the new value was identical again on
   the next boot. Which three is unrecoverable: the earlier run kept the
   per-register event count and not the event list. PCR 0, 4 and 7 stayed
   byte-identical across all three boots. Never bind a keyslot to PCR 1.

And one that is not about TPMs at all: **`/dev/nvme0n1` is not a stable name on
this machine.** Three boots were logged; the first two enumerated the two NVMe
controllers one way and the third (an ordinary reboot with nothing special
about it) enumerated them the other way, so the disk that had been Windows's
took Rime's name. The kernel probes the controllers asynchronously and the index
falls out of the race. **Any procedure that protects a disk by device name
protects the wrong one sooner or later**: use the serial, the PCI function, the
PARTUUID or the filesystem label. The advice applies beyond katana; katana is
where it was caught.

Four things the lab could not have told us, all measured here:

1. **`systemd-cryptenroll --tpm2-device=auto` binds to NO PCRs on systemd
   258.10.** `tpm2-hash-pcrs` empty, `tpm2-policy-hash` 32 zero bytes. Not
   even PCR 7. Always pass `--tpm2-pcrs=` or `--tpm2-public-key-pcrs=`
   explicitly. Every enrolment call site in this tree does, and the line numbers
   that used to be quoted here are gone on purpose: a citation into a file that
   keeps changing goes stale silently, so the property is asserted instead.
   `Containerfile.base` refuses to build an image in which
   `/usr/libexec/rime-luks-enroll` names a TPM2 device on an executable line
   with no PCR selection beside it, `tests/test-boot-v2.sh` asserts the same
   thing on every pull request with both controls, and the shipped script reads
   the token back after enrolling and refuses one with no policy. Reproduced in
   the boot lab against swtpm on 2026-09-20, so the finding is no longer
   silicon-only: `tpm2-pcrs: []`, **no `tpm2-pcr-bank` field at all**,
   `tpm2-policy-hash` 64 zeros, `New TPM2 token enrolled as key slot 1`, exit 0.
2. **`systemd-pcrextend` is a silent no-op on a GRUB machine.** *"Kernel stub
   did not measure kernel image into PCR 11, skipping userspace measurement,
   too."*, with **exit status 0** and PCR 11 unchanged. So the four phase policies
   `systemd-measure sign --current` produces by default can never be reached on
   a machine that has not been through the enrolment procedure below. Enrolling
   a phase policy there yields a volume whose TPM unlock fails at boot, not at
   enrolment.
3. **A zero PCR 11 is a local denial-of-service.** `tpm2_pcrextend 11:…` works
   from plain root, and `tpm2_pcrreset 11` answers `bad locality`. Every
   PCR-11-bound keyslot then refuses until the next reboot. One more reason the
   UKI path matters: after `sd-stub` has extended PCR 11, an attacker can no
   longer choose the value.
4. **The signed PCR 11 policy works on Intel PTT**, which is the mechanism this
   document chose and which had only ever run against `swtpm`. A signature from
   the wrong key is refused by systemd on the fingerprint; a **forged signature
   under the right fingerprint is refused by the part itself**,
   `Esys_VerifySignature … 0x2db` (`TPM_RC_SIGNATURE`). Moving PCR 11 breaks
   unlock; re-signing for the new value restores it.

Two corrections to the bullets above this section, now that a real part has been
asked:

* **Dictionary-attack lockout is no longer untriggered.** Intel PTT here:
  `MAX_AUTH_FAIL` 32, `LOCKOUT_INTERVAL` 7200 s, `LOCKOUT_RECOVERY` 86400 s, and
  **a successful authorisation does not clear the counter**. Only the 2-hour
  decay or `TPM2_DictionaryAttackLockReset`, which needs `lockoutAuth`, does. If Rime
  ever ships `--tpm2-with-pin`, a user who mistypes 32 times over any span of
  time locks the TPM for up to a day and Rime cannot reset it.
* **`deep` being offered is not the same as `deep` working.** This machine lists
  `[s2idle] deep` and suspends as s2idle. The TPM came back from s2idle intact.
  S3 remains untested on silicon.

One thing to add to the prerequisites, learned the hard way: **check
`/sys/class/tpm/tpm0/ppi/tcg_operations` before trusting that a TPM clear needs
physical presence.** Operation 5 reading `4: User not required` means any root
process can schedule a clear that the firmware performs unattended at the next
boot: a data-loss primitive reachable from a shell on a machine with a
TPM-bound volume.

## Enrolling a machine: the human procedure

**Read this whole section before running any of it.** There is no rollback for
an ESP you overwrote or an EFI variable you replaced, because the thing that
would perform the rollback is what you broke. Nothing in Rime automates these
steps, and that is deliberate.

Irreversible, in order of how bad it is to get wrong:

1. **Enrolling a Secure Boot key writes your firmware.** It is done from the
   firmware's own setup UI or with `mokutil`, by you, on a machine you can put
   into Setup Mode. Rime ships no script that touches `db`, `KEK` or `PK`, and
   CI and VMs only ever get ephemeral keys.
2. **`bootctl install` writes the ESP** and creates an EFI boot entry. If the
   machine shares its ESP with another OS, this is where that OS's loader gets
   displaced.
3. **`systemd-cryptenroll` changes a LUKS2 header.** Take the recovery key it
   prints and store it somewhere that is not the encrypted disk before you
   reboot, not after.

Before starting: know how to reach your firmware's boot menu on this machine,
and have a live USB you have booted at least once.

```bash
# ── 0. what is the machine doing now? ──
rime boot status                    # expect: Bootloader grub, boot counting not in effect
sudo bootc status                   # note the booted and rollback deployments
sudo ostree admin pin 0             # pin the current deployment before anything risky

# ── 1. build a UKI from THIS machine's image, in the boot lab ──
git clone https://github.com/AndreNijman/apex-os.git ~/build/rime-os
cd ~/build/rime-os
podman build -t rime-bootlab -f bootlab/Containerfile .
mkdir -p ~/bootlab-work
sudo files/scripts/boot-v2/rime-stage-root --output ~/bootlab-work/rime-root
#   ^ reads /usr/lib/modules/<kver>/{vmlinuz,initramfs.img} and os-release.
#     Root because initramfs.img is mode 0600. It writes only the output dir.

# The command line must be the one THIS machine boots with, because a UKI's
# cmdline is inside the signed image and cannot be edited at the boot menu.
# Take it from the running system, not from this document:
cat /proc/cmdline

# Every host-side launch of the boot lab goes through nvram-guard. It reads
# `efibootmgr -v` and the efivarfs Boot* digests before and after and fails the
# run if this machine's boot entries moved. It cannot prevent a write; it turns
# "found at the next power-on" into "found when the command returns", which is
# the difference between a five-minute fix and an unbootable laptop
# (BOOT-BREAKAGE-2026-09-20.md). AGENTS.md boot-path rule 6. The check belongs
# HERE and not inside run-scenarios, because inside the container there are no
# host EFI variables to read.
./tests/lab/nvram-guard -- \
podman run --rm -v ~/bootlab-work:/work:z rime-bootlab -c '
  /work/rime-os/files/scripts/boot-v2/rime-mkuki \
      --output /work/rime-<deployment>.efi \
      --from-root /work/rime-root \
      --cmdline "<the cmdline from /proc/cmdline>" \
      --deployment "<a short id: the ostree deployment checksum works>" \
      --variant "$(. /etc/os-release; echo "$VARIANT_ID")" \
      --sb-key /work/keys/sb/key.pem --sb-cert /work/keys/sb/cert.pem'
```

(The repository's pre-rebrand name: GitHub redirects it after the rename.) <!-- rime-rename: keep -->

`--sb-key`/`--sb-cert` are **your** key, from outside the repository. A private
key never enters the tree, and `.gitignore` blocks the patterns.

```bash
# ── 2. enroll the certificate in firmware. THIS TOUCHES FIRMWARE. ──
#   Reboot into firmware setup, put Secure Boot into Setup Mode, and enroll
#   the DER certificate from a USB stick, or use mokutil and answer the
#   MokManager prompt on the next boot:
sudo mokutil --import /path/to/your-cert.der
#   Verify before going further:
mokutil --list-enrolled | grep -i "<your CN>"

# ── 3. check the ESP has room, BEFORE writing anything to it ──
#   A UKI is the kernel plus the initramfs: 374 MiB on the L16 today. Know the
#   number for this machine and the free space you have, and stop here if it
#   does not fit. This is a ONE-deployment test, not a migration — two Rime
#   deployments plus a staged third need about 1.1 GiB of ESP.
du -b "/usr/lib/modules/$(uname -r)"/{vmlinuz,initramfs.img}
df -h /boot/efi

# ── 4. install systemd-boot and place the UKI ──
#   `bootctl install` writes the ESP and adds an NVRAM entry. Nothing in this
#   repository runs it for you.
#   Capture the boot entries first: this is what you compare against, and it is
#   what proves the Windows entry on a dual-boot machine was not touched.
sudo efibootmgr -v | tee ~/efibootmgr.before
sudo bootctl install
sudo mkdir -p /boot/efi/EFI/Linux
#   +3-0 is the boot counter: three tries, none used.
sudo cp ~/bootlab-work/rime-<deployment>.efi \
        /boot/efi/EFI/Linux/rime-<deployment>+3-0.efi
sudo bootctl list                   # the entry must appear, with 3 tries left

#   Per-machine settings go beside the UKI as credentials, because a signed
#   UKI's command line cannot carry them. A keymap, for instance — the setting
#   that decides whether you can type your own LUKS passphrase:
#   The directory does not exist on a GRUB machine's ESP; create it first.
sudo mkdir -p /boot/efi/loader/credentials
printf 'de' | sudo tee /boot/efi/loader/credentials/vconsole.keymap.cred
#   Plaintext is right for a keymap and WRONG for anything that unlocks the
#   disk: a .cred on the ESP is unauthenticated and anyone who can write the
#   ESP can change it. It is measured into PCR 12, so a TPM policy notices, but
#   nothing refuses to boot on its own. Secrets go through
#   `systemd-creds encrypt --with-key=tpm2`.

# ── 5. compare the boot entries. Additive means ADDITIVE. ──
sudo efibootmgr -v | tee ~/efibootmgr.after
#   The existing entries must be byte-identical; only a new one may appear.
diff <(grep -v '^BootOrder:' ~/efibootmgr.before) \
     <(grep -v '^BootOrder:' ~/efibootmgr.after)
#   On a machine that also boots Windows, this is the line that matters:
cmp <(grep -i 'Windows Boot Manager' ~/efibootmgr.before) \
    <(grep -i 'Windows Boot Manager' ~/efibootmgr.after) \
  && echo "Windows entry unchanged"
#   If the Windows entry moved, STOP and restore it from ~/efibootmgr.before
#   before rebooting.

# ── 6. make the first reboot a TEST, not a commitment ──
#   BootNext applies to exactly one boot and the firmware clears it. GRUB stays
#   first in BootOrder, so if systemd-boot does not come up, the boot after
#   that is the machine you had this morning.
sudo efibootmgr | grep -i 'Linux Boot Manager'     # note its BootXXXX number
sudo efibootmgr --bootnext XXXX
sudo systemctl reboot
#   after it comes up:
rime boot status
#   expect: Bootloader systemd-boot · Signed UKI yes · Boot counting in effect
#           and the entry listed as "good" — systemd-bless-boot stripped the
#           +N-M suffix because rime-boot-health.service passed.
```

If it does **not** come up, the boot counter is doing its job: after three
failed attempts systemd-boot selects the previous entry by itself. GRUB is still
installed and still first in the firmware's boot order, and `BootNext` is
already spent, so the next power-on is the machine you had before.

**This is a test and not a migration, and the difference matters.** The
UKI you placed in step 4 is a snapshot of one deployment. `bootc upgrade` on this
machine keeps writing ostree BLS entries under `/boot`, which systemd-boot
cannot read, so the UKI goes stale at the next update and nothing refreshes it.
Getting a machine onto systemd-boot *permanently* means moving it onto the
composefs backend. `rime update` does that in place, through
`/usr/libexec/rime-boot-migrate auto`, on a machine that passes its precheck:
see "Migrating a machine that already exists".

### TPM-bound unlock

There are **two scripts called `rime-luks-enroll`** and they are different
programs. This section exists to stop you confusing them.

| | `files/scripts/boot-v2/rime-luks-enroll` | `/usr/libexec/rime-luks-enroll` |
| --- | --- | --- |
| who runs it | the boot lab | the installer, or you |
| what it targets | an image file it creates itself | a LUKS2 volume that already exists |
| what TPM | **only** a software TPM at a named swtpm state directory; `auto` and any `/dev` path are refused outright | the real one |
| the binding | always a signed PCR 11 policy | whichever binding this machine can enforce; it asks |
| in the image | no | yes, shipped by `Containerfile.base` |

On a real machine:

```bash
sudo /usr/libexec/rime-luks-enroll \
     --device /dev/<your-luks-partition> \
     --recovery-out /root/rime-recovery-key.txt
```

It enrols a recovery key first, before anything else, on every path. A volume
that has a TPM binding and no recovery path turns a firmware update into a
data-loss event, so the script never opens that window. Move the key file off
the encrypted disk before you reboot, not after.

It adds a TPM key slot only where this machine can enforce one, and when it
declines it tells you why in words you can act on:

| what it finds | what you get | the machine-readable line |
| --- | --- | --- |
| Secure Boot on, TPM present, no sd-stub | a TPM slot bound to **PCR 7** | `tpm2: enrolled binding=pcr7 pcrs=7 bank=sha256 hash=… pin=no` |
| Secure Boot on, booted through sd-stub, PCR 11 extended by it | a TPM slot bound to the **signed PCR 11** policy, which survives kernel updates | `tpm2: enrolled binding=signed-pcr11 …` |
| Secure Boot **off** | recovery key only | `tpm2: declined reason=secure-boot-off` |
| Secure Boot reports on, but the firmware is in Setup Mode | recovery key only | `tpm2: declined reason=setup-mode` |
| no TPM device | recovery key only | `tpm2: declined reason=no-tpm2-device` |
| Secure Boot on but PCR 7 never extended | recovery key only, and the slot it made is removed again | `tpm2: declined reason=pcr-uninitialised` |

**It exits 0 in every one of those rows.** A declined TPM slot still leaves you
a volume with a recovery key on it, so an installer must not stop there. Only a
failure to enrol the recovery key exits non-zero.

**Why no TPM slot without Secure Boot, for either binding.** PCR 7 records the
Secure Boot policy, and with Secure Boot off it records the *disabled* policy
whichever kernel boots, so an attacker boots their own system and the TPM
unseals for them. The signed PCR 11 policy is no better off, and people miss
that half: the signature it needs ships in the UKI's `.pcrsig` section and is
public, PCR 11 starts at zero on every boot, `tpm2_pcrextend 11`
works from plain root, and every digest that reaches the signed value is a hash
of public material. An attacker who can boot any kernel replays the extends and
unseals without Rime's private key. Secure Boot stops them, by refusing to
load their kernel. If Secure Boot cannot be enabled on a machine, `--with-pin`
is the other route; read what it prints about the lockout counter first.

**Never `--tpm2-device=auto` with no PCR selection.** Measured on systemd
258.10: the bare form exits 0, prints "New TPM2 token enrolled", and writes a
token with `tpm2-pcrs: []`, no `tpm2-pcr-bank` field and a policy hash of 32
zero bytes. That token is sealed to the TPM storage key and nothing else, so any
OS on that hardware unseals it. To check a volume somebody else enrolled:

```bash
sudo /usr/libexec/rime-luks-enroll --check --device /dev/<partition>
```

which exits 2 and says what is wrong if the TPM slot carries no policy.

On the UKI path the UKI must be built with `--pcr-key` so it carries the
matching `.pcrsig`. The policy refuses a UKI without one, by design.

## Recovery

| situation | what to do |
| --- | --- |
| the new deployment will not boot | do nothing for three attempts; systemd-boot selects the previous blessed entry itself. `rime boot status` then shows the failed entry as `OUT OF TRIES` and announces the rollback. |
| the machine boots but the desktop does not | `rime-boot-health.service` fails, the entry is never blessed, and the same automatic rollback happens. `journalctl -u rime-boot-health` names the unit that was not active. |
| you want GRUB back | GRUB was never removed. Select it from the firmware boot menu, then `sudo efibootmgr` (as yourself, deliberately) to put it back at the front of `BootOrder`. Delete `/boot/efi/EFI/Linux/rime-*.efi` to stop offering the UKI path. |
| TPM unlock stops working after a firmware update | the recovery key. Then re-check: with a **signed** PCR 11 policy a firmware update should not break unlock, because the keyslot is bound to the signing key and PCR 11 measures the UKI, not the firmware. The lab has now measured both halves of a firmware change and neither breaks it: the Secure Boot policy register (`luks-firmware-change`, a real `DBXUpdate`) and the firmware code register (`luks-firmware-code`, two edk2 revisions). Nobody has tried it on silicon with a vendor capsule yet: katana was asked on 2026-09-19 and `fwupdmgr get-updates` offers nothing for its System Firmware, so there was no capsule to apply. What katana *did* show is the half that matters: re-signing for a moved PCR 11 restores unlock on real Intel PTT. If a vendor capsule ever breaks it, record that finding here. |
| TPM unlock stops working after a kernel update | this should not happen: it is the property the policy was chosen for, and the `luks-tpm` scenario measures it. Use the recovery key, then check that the new UKI carries a `.pcrsig` signed by the enrolled key: `python3 files/scripts/boot-v2/pe-section.py <uki> .pcrsig`. |
| you rotated the PCR signing key | every existing keyslot is bound to the old public key. Enroll the new one with `systemd-cryptenroll --tpm2-public-key=<new>` **before** removing the old, and keep the recovery key usable throughout. |
| the TPM was cleared, or the disk moved to another machine | the sealed object is gone: it was bound to that TPM's SRK. Only the recovery key opens the volume. Re-enroll afterwards **in two separate invocations**: wipe the old slot first, then enroll. Doing both in one command reports success and changes nothing; see below. |
| the TPM is switched off in firmware setup, or the board was replaced | the recovery key, and the volume opens with the data intact (measured as `luks-no-tpm`). The sealed object is still in the header, so re-enabling the same TPM restores unlock with no re-enrolment; a *cleared* TPM is the row above and does need one. In a lab guest the refusal is immediate; on a machine the boot shows a passphrase prompt instead, and nobody has checked how legible that prompt is. |
| TPM unlock refuses with `Esys_Load … 0x18b` and the TPM was **not** cleared | the persistent SRK at `0x81000001` is missing, not the key material. `systemd-cryptsetup` only *uses* that handle; `systemd-cryptenroll` is what creates it, and the SRK is derived deterministically from the storage primary seed, so recreating it restores unlock with **no re-enrolment** and no change to the header. Measured on katana 2026-09-19 by evicting the handle and putting it back. From the initrd there is no way to do this, so the recovery key is the only way into that boot. |
| TPM unlock refuses and nothing about the machine changed | check PCR 11 before blaming the firmware. It can be extended from plain root and **cannot be reset** (`tpm2_pcrreset 11` → `bad locality`), so anything with root can deny every PCR-11-bound unlock until the next reboot. Measured on katana 2026-09-19. A reboot restores it; the recovery key works throughout. |
| you lost the recovery key and the TPM state | the data is gone. This is why enrollment prints the key and this document says to store it off the encrypted disk. |

**The one-command re-enrolment after a TPM clear is a no-op that exits 0.**
Measured on systemd 258.10-1.fc43, and it is the command a user reaches for
after a firmware TPM clear:

```
$ systemd-cryptenroll --wipe-slot=tpm2 --tpm2-device=… --tpm2-public-key=… VOLUME
This PCR set is already enrolled, executing no operation.
$ echo $?
0
```

The header is byte-identical afterwards. systemd de-duplicates against the token
already in the header: it compares the public key and the PCR set, the TPM
plays no part, and that check runs **before** it acts on the wipe. So on a
cleared TPM the whole invocation does nothing while reporting success: the
header keeps a blob sealed to a seed that no longer exists, and the next boot
fails exactly as it did before the "recovery" (`Esys_Load rc 0x1df`, *"Key
enrolled in superblock most likely does not belong to this TPM"*). Wiping in its
own invocation first produces a new blob and a working unlock.

**Confirmed on silicon after a real firmware clear**, 2026-09-19, Intel PTT:
`This PCR set is already enrolled, executing no operation.`, `rc=0`, `tpm2-blob`
sha256 unchanged, and the next unlock still refused, with `0x1df` rather than
the `0x18b` the first post-clear attempt gave, because the invocation had
meanwhile persisted a fresh SRK at `0x81000001`. Two separate invocations then
changed the blob and unlock worked in 298 ms. So the paragraph above describes
what a user gets, on silicon as well as in the lab.

The `luks-tpm-clear` scenario missed this at first, and the reason is worth
keeping: every header assertion it already made (exactly one `systemd-tpm2`
token, the public-key policy, PCR 11) held true of the no-op character for
character. It now compares the sealed **blob** before and after, and an
unchanged blob is a named failure.

## What §22 asks for and this does not do

* **Step 6, encryption by default: on in the graphical installer, and the
  owner can decline it.** §22 gates it on "once recovery and hardware edge
  cases are proven". The graphical installer (`installer/rime-installer-gui`)
  now offers disk encryption with the box ticked, and declining it is a plain
  answer; `installer/rime-install` creates the LUKS2 volume itself
  (`cryptsetup luksFormat`) and refuses the install unless both the passphrase
  and a recovery key open it. `docs/disk-encryption.md` is the reference. The
  unattended (CI) path keeps encryption off and refuses `rime.encrypt=yes`,
  because nobody is there to be shown a recovery key.

  In the lab, a TPM clear, a PCR 7 change, a PCR 0 change, an S3 cycle and a
  missing TPM are each measured. **Two of the five show a recovery key opening
  a volume the same boot had refused: the TPM clear, and the missing TPM.** The
  two firmware scenarios show a refusal and no recovery, because their control
  volumes carry no recovery key on purpose: a fallback would hide the refusal
  the instrument exists to show. The S3 scenario shows neither; it never touches
  a recovery key, and it checks that the volume stays open across the suspend.

  An earlier version of this paragraph said the PCR 7-bound control volume was
  refused and then opened with the recovery key. **That was wrong and is
  withdrawn.** The control has no recovery key by construction, the guest probe
  has no recovery path for it, and no `recovery-unlock` line appears in any
  serial log from any firmware-change boot. Checked against the logs rather
  than against the source, because the source is what made the claim plausible.

  All five of those ran against `swtpm` in a VM. On silicon, katana's Intel
  PTT has been through an enrol-and-recover cycle (runs 1 and 2 of the MSI
  Katana section, 2026-09-19, on a loopback LUKS volume); run 5, the TPM going
  away, has not run on silicon yet.
* **No NVRAM management in the lab tools.** `rime-mkesp` writes
  `/EFI/BOOT/BOOTX64.EFI` so the VM boots by the removable-media path, because
  creating a real boot entry means `efibootmgr`, and no lab script runs that
  near a real machine. On a real machine the one shipped helper that writes
  boot variables is `rime-boot-migrate`, under the discipline in "Migrating a
  machine that already exists": an entry created with `--create-only`, one
  `BootNext`, and `BootOrder` only after a good boot. For the manual procedure
  above, `bootctl install` creates the entry and the operator runs it.
* **The image now carries `systemd-boot-unsigned` and `systemd-ukify`, and
  puts the loader on a machine only through `rime-boot-migrate`.** They went into `Containerfile.core` with
  the pivot, because `bootc install --bootloader systemd` copies the loader
  *out of the image being installed*: an image without it produces an ESP with
  no loader binary and an install that exits 0. That is a `core` rebuild and
  therefore a real fleet-update cost, recorded in `docs/update-cost.md`'s terms.
  `bootctl` in the shipped path is still read-only: `rime boot status` calls
  `bootctl list` and nothing else.
* **"composefs" means two different things, on two different kinds of
  machine.** §23's row names composefs, and Rime already boots on a
  composefs-backed **ostree** root; the katana does, today, through GRUB. That
  is not bootc's `--composefs-backend`, which is a different storage backend
  with its own deployment layout, its own `bootc status` shape (`bootType:
  Bls`, `softRebootCapable: true`) and the ESP mounted at `/boot`. The pivot is
  a move to the latter. Reading the katana's existing composefs root as "the
  katana is already on the new backend" would be a serious mistake.
* **The `rime boot status` entry list needs root**, because the ESP is mode
  0700. Without it the command reports `entries: unavailable` **with the
  reason**, never an empty list: an empty list is indistinguishable from "no
  deployment has failed", which is the answer that would hide a rollback.

## Running the harness yourself

Every launch below is wrapped in `tests/lab/nvram-guard`, and the wrapper has a
job. The check has to live on the HOST side: `run-scenarios` runs inside
the container, where `/sys/firmware/efi/efivars` does not exist, so a check in
there would inspect nothing. AGENTS.md boot-path rule 6. A `bootc install
--via-loopback` goes through `tests/lab/bootc-install-lab` instead, which
builds the podman argv itself and refuses to launch without `--generic-image`
(bootc: "Changes to the system firmware will be skipped"). That flag, not the
efivars tmpfs, is what prevents the 2026-09-20 breakage: bootc takes
`--pid=host` and re-enters the host's mount namespace for the bootloader step,
so a tmpfs inside the container never covered it. AGENTS.md boot-path rule 6.

Two practical notes, both learned the hard way:

* **The guard needs no `sudo`**: efivars are world-readable and `efibootmgr
  -v` works as an ordinary user (checked on the L16 as uid 1000). It does need
  `efibootmgr` to exist: with `/sys/firmware/efi` present and the binary
  missing it reports `could-not-snapshot` and **refuses to run the command at
  all**, which is correct and is why `boot-v2.yml` installs it on the runner.
* **`sudo` in front of `podman` changes which image store is used.** The
  commands below build `rime-bootlab` rootless and run it rootless. If you
  build as root, run as root: an image in the other store looks like an image
  that does not exist.

```bash
# on the katana, or any box with /dev/kvm and podman
podman build -t rime-bootlab -f bootlab/Containerfile .
mkdir -p ~/bootlab-work/out
./tests/lab/nvram-guard --label bootlab -- \
podman run --rm --device /dev/kvm -v ~/bootlab-work:/work:z \
    -v "$PWD:/work/repo:z" rime-bootlab -c \
    '/work/repo/files/scripts/boot-v2/run-scenarios --work /work/out'

# the two scenarios that need a real Rime image are requested by name, and
# hard-fail rather than skipping if the staged root is missing:
sudo files/scripts/boot-v2/rime-stage-root --output ~/bootlab-work/out/rime-root
./tests/lab/nvram-guard --label bootlab-rime -- \
podman run --rm --device /dev/kvm -v ~/bootlab-work:/work:z \
    -v "$PWD:/work/repo:z" rime-bootlab -c \
    '/work/repo/files/scripts/boot-v2/run-scenarios --work /work/out rime-image luks-tpm'
```

`run-scenarios --list` prints every scenario name, and
`tests/test-boot-v2.sh` fails if a scenario exists that `--list` does not name:
a default run skips an unregistered scenario and reports green without
it.

One scenario takes an input the lab image cannot produce. `luks-firmware-code`
needs a **second** Secure Boot OVMF build, a different edk2 revision from the
image's, because moving PCR 0 needs two firmwares differing only in code:

```bash
# the older Fedora build, extracted from its rpm — any different revision works
./tests/lab/nvram-guard --label bootlab-fw-alt -- \
podman run --rm --device /dev/kvm -v ~/bootlab-work:/work:z \
    -v "$PWD:/work/repo:z" -e RIME_BOOTLAB_FW_ALT=/work/OVMF_CODE_4M.secboot.qcow2 \
    rime-bootlab -c \
    '/work/repo/files/scripts/boot-v2/run-scenarios --work /work/out luks-firmware-code'
```

Without it the scenario records a could-not-run naming the package to fetch and
the sha256 of a known-good image, and asserts nothing. With a firmware that is
the same revision, or that does not enforce Secure Boot, it fails rather than
reporting a result: both are checked before anything rests on the input. The
toolchain-free assertions (unit conditions, the boot-path tripwire, the health
gate's exit codes, the writer/reader schema parity) are `./tests/test-boot-v2.sh`,
which needs no VM and runs on every pull request in `pr-validation.yml`'s
`static` job. `./tests/test-boot-v2.sh --with-binary` adds the `rime boot status`
fixture cases and runs in the `rust` job.
