# Rime owns its own ESP

**Decision (Andre, 2026-09-21):** *"windows side install should be like
everything else with the systemd-boot. maybe it should build a new esp for
rime or something."*

This settles the first of the two product questions the Windows installer unit
was told to leave alone. The decision reaches beyond Windows: it removes a
special case rather than adding one.

## What was decided

1. **There is no ostree/GRUB variant for Windows machines.** Every Rime machine
   boots systemd-boot, from a UKI, through the same `bootc install
   --composefs-backend --bootloader systemd` path. That gives one boot story,
   one set of tests and one failure mode to understand.
2. **Rime gets its own EFI System Partition. It does not write into Windows'.**
   The rule has no "prefer not to" in it: Rime reads the Windows ESP for facts
   and never writes to it.

The second question, whether the tool may ever retype a basic-data partition
itself, is answered further down, under "The second decision".

## Why owning the partition is the right shape

The 512 MiB problem came from *borrowing* a partition.

A stock Windows ESP measured **68.3 MiB free**, and composefs needs about
**1.1 GiB**. Every route out of that (XBOOTLDR, shrinking the initramfs far
enough to fit, sharing the loader directory) squeezed Rime into a partition
Microsoft sized for Microsoft, and so competed with Windows for it forever: a
Windows feature update that grows `\EFI\Microsoft` reclaims the slack, and the
next `rime update` fails on a machine that worked yesterday.

Owning the partition ends that class of failure, and it is *also* the safer
option. The strongest guarantee Rime can offer a dual-boot user is that
Windows' own boot path survives byte-identical, and the cheapest way to keep
that guarantee is to never open the file for writing.

## The mechanism already mostly exists

> **CORRECTION 2026-09-21: two of the three bullets below are wrong, and they
> were labelled "checked rather than assumed".** They were checked against
> `--help` text; checked against bootc's source they do not hold for the
> composefs + systemd-boot path, which is the only path Rime uses.
> `crates/lib/src/bootc_composefs/boot.rs` reaches the ESP at four call sites
> and every one is `find_first_colocated_esp()`; `boot_mount_spec()` appears
> there only to build a `systemd.mount-extra=` karg and does **not** steer the
> loader write. Both `BootSetupType::Upgrade` arms re-discover, so every later
> `bootc upgrade` re-walks the GPT. Read at bootc 1.16.10, the installed
> version, and re-checked at 1.16.11. **The decision stands**; only the claim
> that the mechanism is nearly free falls. ESP preference can be expressed only
> as GPT partition ORDER, which makes it a GPT change and so must-measure #1.
> Full evidence: `ROADMAP/evidence/migrate-preconditions-20260921.md` §1.

Three things were true on `roadmap/v2.2` (since merged into `main`) when this
section was written, checked rather than assumed:

- **`bootc install to-filesystem` uses the ESP the CALLER mounted.** Its own
  help says partitions "are prepared and mounted by an external tool or
  script". bootc does not go hunting; it writes where the caller points it. So
  choosing the ESP is Rime's decision to make.
- **`rime-boot-migrate` already accepts `RIME_MIGRATE_ESP`**: "an already-
  mounted ESP, instead of finding one". The override needed to target a chosen
  partition is in the shipped tool today.
- **The tool already knows a machine can have two ESPs, and which is which.**
  `find_esp()` answers "which ESP belongs to this machine's root disk";
  `booted_esp_partuuid()` answers "which ESP did the firmware load the loader
  from". Its comment records that on katana those are different partitions on
  different disks.

ESP selection existed already, so the work is to add ESP *creation* and make
the chooser prefer Rime's own.

## What this fixes that is already broken

**katana is booting Rime off the Windows disk right now.** `Boot0000* Rime OS
Primary` points at PARTUUID `2ba9a2ea…`, the 200 MiB ESP Windows created, while
the Rime disk carries its own **unused 512 MiB `EFI-SYSTEM`** at PARTUUID
`99af3362…`. That machine therefore depends on another operating system's disk
to boot, and its NVMe device names reorder across ordinary reboots. Under this
decision katana needs no new partition at all: it needs to start using the one
it already has.

That is the first migration to run, because it is the cheapest proof: no
partitioning, no shrink, and a measured end to the machine's dependence on
Windows.

## Where the space comes from, in order of preference

1. **An existing unused ESP on the Rime disk**, as on katana. Nothing to create.
2. **Free/unallocated space** on the target disk. No data moves.
3. **Shrinking the Windows NTFS volume.** The installer already surveys
   partitions and produces a plan; this becomes a planned, consented step, and
   per the standing constraint the tool tells the user the commands rather than
   silently repartitioning.

Size it for the job: room for two deployments plus slack, instead of 68 MiB of
someone else's leftovers.

## This does NOT retire the initramfs work

The ESP decision left `initramfs-slim` necessary, at the same priority. The
machines below have different problems:

| machine | situation | answer |
|---|---|---|
| L16, and every existing Rime install | 512 MiB ESP that is **already Rime's own** | shrink the initramfs; there is nothing to create |
| Windows dual-boot | Windows' ESP has 68.3 MiB free | build Rime its own ESP |
| katana | has an unused 512 MiB ESP on its own disk | use it; stop booting off the Windows disk |

Andre's *"it has to work in 512. i dont care how it just has to work"* was
about the L16, which has no Windows on it. A new ESP does not help there. A
smaller initramfs helps everywhere, including making case 2 fit on machines
with little free space to give.

That work has since landed. The image built at `44c9a5cb` costs 100.9 MiB per
deployment, so migration needs 350 MiB of ESP instead of 1173 MiB
(`docs/boot-v2.md`, `ROADMAP/evidence/initramfs-slim2-20260922.md`).

## What must be MEASURED before any of this is claimed to work

None of the following is safe to assume, and the first three are the whole risk
of the design:

1. **Two ESPs on one GPT disk.** Does the firmware boot the intended one from
   an explicit NVRAM entry? Rime already writes its own `Boot0000`, so the
   mechanism is there, but "the spec permits it" is not evidence, and firmware
   has bitten this project before.
2. **Does Windows tolerate a second ESP?** In particular across a feature
   update, a repair install and `bcdboot`. A design that survives installation
   and dies at the next Patch Tuesday is worse than no design.
3. **Does `bootupd` stay on the mounted ESP** for every later `bootc upgrade`,
   or does it re-discover one? XBOOTLDR was ruled out partly because bootupd
   "mounts the ESP unconditionally" and records paths in a runtime store every
   later upgrade reads. The same store must not point at Windows' ESP.
4. **Windows' own boot path byte-identical** either side, GPT included, proven
   by comparison against a pristine fixture rather than by Windows still
   booting once.
5. **Whether BitLocker's PCR profile on this machine binds PCR 5.** TCG assigns
   PCR 5 to the GPT partition table. BitLocker's default UEFI + Secure Boot
   profile binds PCR 7 and 11 and leaves it alone, but the legacy profile
   includes it and the profile is group-policy configurable on any machine.
   Where PCR 5 is bound, **any** GPT change forces a recovery prompt on the
   next Windows boot, and creating Rime's ESP counts as much as retyping does.
   Read the profile, never assume it: `manage-bde -protectors -get C:` prints
   it. The lab's `bitlocker-discover` job runs `manage-bde -status` and
   `-protectors -disable` and **does not read the profile at all**; adding
   that is the first thing it needs. This project has measured PCR 0, 7 and 11
   in detail and has never looked at 5.

### Measured 2026-10-08 (the Windows installer, resumed)

Answers to the five items above, from `windows-installer/lab/install-e2e`
(Windows Server 2022, OVMF with Secure Boot on and Microsoft's keys) and
`windows-installer/tests/staged-boot.sh` (Windows-shaped disk images).
Transcripts in `windows-installer/VALIDATION.md`.

1. **Two ESPs on one GPT disk: yes.** The firmware started Rime's setup
   loader from Rime's ESP (partition 4, after Windows' at 1) through an
   explicit `Boot####` + `BootNext`, under Secure Boot, and later the
   installed "Rime OS" entry from the same ESP.
2. **Windows tolerates it:** Windows started again from its own boot manager
   after Rime was installed and ran its job; `chkdsk C: /scan` clean. Windows
   gives the second ESP a volume but no drive letter. (Feature updates,
   repair installs and `bcdboot` are still unmeasured.)
3. **bootupd does NOT stay on the mounted ESP**, for the firmware entry, and
   re-discovers on every update. Read at bootupd 0.3.2 /
   bootc-internal-blockdev 1.16.10: files go to whatever vfat is mounted at
   `<root>/boot/efi`, but `update_firmware()` passes
   `get_esp_partition_number()` (the first ESP-typed child of the disk) to
   `efibootmgr --create`, and `run_update()` uses `find_colocated_esps()`
   (the first per disk) unless an ESP is already mounted at /boot/efi.
   `installer/rime-install` therefore removes other ESPs from the kernel's
   partition list (BLKPG, not the disk) for the bootloader step, and writes
   an fstab line mounting Rime's ESP at /boot/efi plus a
   bootloader-update.service drop-in that refuses to run without it.
4. **Windows' partitions byte-identical:** every Windows partition (ESP, MSR,
   C:, recovery) hashed identical before staging, after staging and after the
   whole Rime install, on the disk images; on the Windows guest, Windows' ESP
   holds no Rime file afterwards (its BCD changes on every Windows boot, as
   recorded above, so its bytes are not the test there).
5. **PCR 5:** read, never assumed: `Win32_EncryptableVolume`'s
   `GetKeyProtectorPlatformValidationProfile` for each TPM protector of C:.
   Where it binds PCR 5 (or cannot be read), protection is suspended for one
   restart before the table changes. Separately and more often relevant:
   starting Windows through shim + GRUB changes **PCR 7**, which BitLocker's
   default profile binds, so on BitLocker machines Rime's menu does not
   offer Windows at all.

The tension with the systemd-boot decision is unchanged and stated: the
installer installs what `rime-install` installs today (GRUB via bootupd). The
new ESP is sized with ~0.5 GiB to spare, which the 350 MiB systemd-boot
layout fits; `rime-boot-migrate` still uses the first ESP on a disk and has
to learn Rime's own before it may run on a dual-boot disk.

## Bounds that do not change

- The Windows ESP is **read-only, always**. There is no flag that makes it
  writable.
- Never `bootc install` without `--generic-image` outside a real target, and
  always through `tests/lab/bootc-install-lab`. `--generic-image` is the
  prevention; `tests/lab/nvram-guard` is only detection. bootc runs `--pid=host`
  and `nsenter`s into the host mount namespace, so a tmpfs over efivars inside
  the container is inert. That mistake broke the L16 twice in one evening.
- An `efibootmgr -v` diff either side of anything that could touch firmware
  variables.
- The single commit point stays one `SetVariable` of `BootNext`. Creating a
  partition must be complete and verified before the boot entry moves, and a
  failed trial boot must land back where it started.

## The second decision: the tool edits GPT entries itself

**Decided 2026-09-21. Andre delegated it ("you decide"), so the reasoning is
written out here, and it is a judgement rather than a proof.**

**The tool changes the partition's type GUID and attributes itself. It does not
print `set id=` and `gpt attributes=0x0` for the user to run in diskpart.**

### Why

The ESP decision asks for two things: *"build a new esp for rime"* and
*"like everything else"*. The tool could print diskpart commands for creating
that ESP too, but a wizard whose answer is "now go and type seven things into
another program" is the *"suckky system where half the time peoples things wont
work"* that Andre rejected by name. Once the tool creates a partition, refusing
to change 24 bytes of an existing entry is an inconsistency rather than a
safety position: the edit is smaller than the creation and easier to reverse.

Handing a user raw diskpart is also the **more** dangerous option. diskpart has
no undo, and it makes the *user* do the targeting: `select disk N`, `select
partition M`. The tool has already read the raw GPT and knows which disk and
which entry. The accident lives in transcription: one wrong `select disk` and
the user has retyped the volume Windows boots from. A programmatic edit with
verification beats manual transcription. That is a judgement about where the
risk sits, and not a theorem.

### The boundary, as a principle, for the next write somebody asks about

| operation | who |
|---|---|
| **GPT entry edits**: retype; create from unallocated space | **the tool**, under the invariants below |
| **Filesystem operations**: the NTFS shrink | **the user**, in Windows' own tooling, by design: the tool has no NTFS knowledge and must not grow any |
| **Whole-layout construction**: a table built from anything but a fresh read of the current one | **never** |
| **Firmware writes**: `SetFirmwareEnvironmentVariable` | **the tool**, additively only, under the `BootNext` discipline (see "The third decision" at the end of this file) |

### Invariants, not mechanism

The mechanism is **not** decided here, on purpose: neither candidate is
measured yet, and the choice is empirical:

- a raw sector read-modify-write through `\\.\PhysicalDriveN` is narrow in
  blast radius, but nobody has verified that Windows permits a write to LBA
  2-33 on a **live system disk** at all (the payload-write proof wrote to a
  partition extent, a different protection regime), and after one, Windows'
  cached partition view is stale until `IOCTL_DISK_UPDATE_PROPERTIES`
  (0x70140), which is not on the allowlist either;
- `GET_DRIVE_LAYOUT_EX` → change one entry → `SET_DRIVE_LAYOUT_EX` is wide in
  API but narrow in intent, and Windows maintains its own state and the backup
  GPT for you.

One guest boot answers which is safer.

> **MEASURED 2026-09-21: that boot has happened.** Evidence:
> `ROADMAP/evidence/windows-installer-3-20260921.md`; job
> `windows-installer/lab/jobs/gpt-write-mechanism`; 13 host checks, 0 failures.
> The mechanism is still the implementation's to choose, but it is no longer
> choosing blind.
>
> - **The stated unknown is answered: Windows PERMITS a write to LBA 2-33 of
>   the disk it booted from**, and permits `SET_DRIVE_LAYOUT_EX` on it too.
>   Both probed with no-ops, and the host confirms the system disk's primary
>   and backup GPT are byte-identical to pristine afterwards. "Windows will not
>   let you" is not a safety property available here: the second time in one
>   round that a platform backstop turned out to be missing.
> - **Both mechanisms work** and produce a GPT whose four CRCs are all correct,
>   and **neither writes a byte of partition content**.
> - The raw mechanism's stale-view prediction **holds**, and
>   `IOCTL_DISK_UPDATE_PROPERTIES` (`0x00070140`) fixes it (`ok=True err=0`).
> - **`SET_DRIVE_LAYOUT_EX` relocated the primary entry array from LBA 2 to
>   LBA 2016 and left the old array at LBA 2 untouched**: two partition tables
>   that disagree in the primary GPT area, the stale one at the LBA hardcoded
>   GPT readers use. The raw mechanism edits in place and leaves one table.
>   **The relocation target is a function of `FirstUsableLBA`**: the array is
>   parked to END at it (`2016 + 32 == 2048`, checked). The lab fixtures are
>   `sgdisk`-made with a 1 MiB reserve, `FirstUsableLBA = 2048`; `golden.raw`,
>   partitioned by **Windows Setup itself**, has `FirstUsableLBA = 34`, where
>   the same rule yields LBA 2 and **no relocation at all**. So this hazard
>   spares a disk Windows made and bites disks made by Linux tooling, **which
>   is what Rime itself creates**. The `FirstUsableLBA=34` row is a prediction
>   from one measurement, not a second measurement.
> - **Invariant 3 is exercised as well as specified**: both copies saved to a
>   33 792-byte file before the change, restored afterwards, byte-exact.
> - If `SET_DRIVE_LAYOUT_EX` ever wins, `0x0007C054` and `0x00070140` have to
>   join section 0's allowlist: a deliberate widening that must keep the gate
>   failing both ways.

Either way, the implementation must satisfy these:

1. The delta is **exactly one entry's type GUID and attributes**. Nothing else
   on the disk changes, proven byte-identical against a pristine fixture.
2. Both GPT copies, primary and backup, end consistent, with correct CRCs.
3. The tool writes a **backup of both copies to a file before the change**, and
   `--undo-gpt <file>` restores it. Undo restores the **table**, not partition
   contents; say so to the user in those words.
4. Windows' partition view is coherent afterwards, not stale.
5. The tool **re-enumerates volumes immediately before the write** and refuses
   it if anything now overlaps: ARCHITECTURE.md's Exclusivity rule, which
   already applies to the payload write.
6. Any layout handed to the kernel is derived from a **fresh read of the current
   one**. Never from a cached or reconstructed table. This holds forever,
   whichever mechanism wins.

### Refusals that are never overridable

- Anything with a **recognised filesystem signature**. NTFS present → refuse,
  and say "delete the volume in Disk Management first". This keeps the tool
  out of data destruction altogether, and the survey already reads the raw bytes
  to tell.
- The ESP, Microsoft Reserved, any recovery partition, and the partition
  Windows booted from.
- A BitLocker-protected volume (`-FVE-FS-`).
- **A disk whose BitLocker profile binds PCR 5** (see must-measure item 5),
  unless the user confirms they hold the recovery key, with the reason stated.

Consent names the disk, partition number, size, current type, filesystem and
label, and confirms *that partition*, rather than asking a yes/no question.

### What happens to the write-API gate

The gate **sharpens**. `tests/test-windows-installer.sh` section 0 is a real
gate: a denylist of names, *plus* an allowlist of IOCTL codes (which exists
because `IOCTL_DISK_SET_DRIVE_LAYOUT_EX` is `0x0007C054`, a number no
name-based grep will ever see), *plus* a refusal of bare numeric literals. It
fails both ways today and must still fail both ways afterwards.

Whether that is one binary or a default build plus a declared write build is the
implementation's call, and it proves whichever shape it picks. Deleting an
assertion to get a job green is not on the table.

### Standing bound

Every test of a GPT write targets a **fixture inside the guest**. The L16's
disks are never a target. A bug in this code pointed at the wrong
`PhysicalDriveN` is the same class that took this machine's boot path out twice
in one evening.

---

# The third decision: firmware writes

**Decided 2026-09-21, after Andre said "complete everything".** He had been
offered this one separately and did not reserve it, so the unit took it here
instead of blocking on it. It is the highest-blast-radius operation in the
project and he can overturn it.

**The Windows tool writes UEFI boot variables itself, under the discipline
`rime-boot-migrate` already uses on the Linux side, and no other.**

## Why not leave it to the user

An install that cannot create a boot entry has not installed anything. The
alternative is printing `bcdedit` incantations, which is the same "go type
seven things into another program" that both earlier decisions rejected, and
worse here, because a mistyped `bcdedit /set {fwbootmgr}` can reorder or drop
Windows' own entry.

## Why not Windows' `{fwbootmgr}` BCD store

It is the tempting option: Windows constructs the variable, handles the vendor
quirks, and uses this path for itself. It is rejected because it **couples
Rime's bootability to Windows' BCD**, which a Windows repair, a reset or a
feature update can rewrite, and the point of this work is to stop depending on
Windows. katana is the cautionary example: it boots Rime off the *Windows* disk
today, which is the defect the ESP decision exists to end. Trading an ESP
dependency for a BCD dependency is no progress.

## The discipline, already proven on Linux

`files/system/libexec/rime-boot-migrate` solved this on Linux. The Windows side
mirrors it rather than inventing a second design, so there is one boot story to
reason about:

1. **Save first.** The tool writes `BootOrder` and the full entry list to a
   file before anything else (`bootorder.before` on the Linux side). NVRAM has
   no backup GPT; the dump *is* the backup.
2. **Create-only.** Write `BootXXXX` for Rime and **do not touch `BootOrder`**.
   `efibootmgr --create-only` is the Linux equivalent. After this step the
   machine still boots exactly what it booted before.
3. **One commit point: a single write of `BootNext`.** Nothing else commits.
   The firmware consumes `BootNext` before it launches anything, so a machine
   that fails to boot Rime **comes back to Windows by itself, with nothing to
   undo**. That property is why this is safe enough to do at all.
4. **`BootOrder` is written only after a verified successful first boot** of
   the new path, with Rime first and **Windows Boot Manager still in it, behind**.
5. **Never delete, never reorder, never rewrite another operating system's
   entry.** Windows Boot Manager stays byte-identical. Additive only.

## The variable allowlist

The source may reference **`BootOrder`, `BootNext`, `BootCurrent` and
`Boot####`, and nothing else.** In particular, never `PK`, `KEK`, `db`, `dbx`,
`SetupMode`, `OsIndications`, or any vendor-namespaced variable.

This mirrors the IOCTL allowlist section 0 already uses, and for the same
reason: `SetFirmwareEnvironmentVariableW` takes an arbitrary name string, so a
denylist of one API name proves nothing about what it is pointed at. The gate
**sharpens**: `SetFirmwareEnvironmentVariable` comes off the name denylist and
is replaced by an allowlist of the variable names the source may contain, plus
a refusal of any name built at runtime rather than declared as a constant. It
must fail both ways: red on a new variable name, red on a name that stops
being declared.

## Why this is not the mistake that broke the L16 twice

Those incidents were `bootc` **deleting and recreating** `Boot0000` to point at
an ESP inside a disk image being built: a destructive rewrite of the live
entry, from a tool that had left its container without anyone realising. Every
clause above targets that failure: additive only, never delete, the commit is
one-shot and self-reverting, and the prior state is on disk first.

The standing requirement is unchanged and applies here: an `efibootmgr -v`
equivalent diff either side of every run, and in the lab that is a guest's
firmware variables, never this machine's.

## Nothing is left open

All three Windows product decisions are now settled: the ESP, GPT entry
writes, and firmware writes.
