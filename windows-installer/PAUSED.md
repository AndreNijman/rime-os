# RESUMED, 2026-10-08

**Andre:** *"continue the work on the RIME windows app, that installs rime on
an empty partition FROM windows, completely finish the app and make sure its
definitely fully working and secure and fast and no issues"*, and then:
*"theres a similar project online, you can probably take a lot of the things
from that project"*.

That is the ask the pause below required. The project online is ULLI
(codeberg.org/rltvty/ulli), GPL-3.0; Rime OS is MIT, so its mechanisms and
lessons were studied and none of its code was taken. What was taken: stage a
live environment onto the disk and boot it instead of a USB stick; generate the
boot menu rather than patch the ISO's; warn about BitLocker and Secure Boot up
front. What was not, and why: `bcdedit /set {fwbootmgr}` (it couples Rime's
boot to Windows' BCD and moves Windows out of first place), writing into the
first ESP it finds (Windows'), and shrinking NTFS from the tool.

State now: the app installs. See README.md for what it does and its limits,
VALIDATION.md for what was measured. The three product decisions of
2026-09-21 stand and are implemented: Rime builds its own ESP, the tool edits
GPT entries itself (in place, primary header last, both copies backed up
first), firmware writes follow the BootNext discipline (one new entry,
BootOrder untouched, one-shot start).

Two findings of the resumed work outlive this tool:

1. **bootupd does not use "the ESP the installer chose".** It copies files to
   whatever is mounted at /boot/efi but names the FIRST ESP on the disk in the
   firmware entry, and `bootupctl update` (bootloader-update.service, every
   boot) writes to the first ESP it finds. On a Windows disk that is
   Windows'. `installer/rime-install` now hides other ESPs from the kernel for
   the bootloader step and makes the installed system mount its own ESP at
   /boot/efi, with the update service refusing to run without it.
2. **Starting Windows through GRUB trips BitLocker.** shim and GRUB put
   different Secure Boot authorities into PCR 7 than the firmware starting
   Windows directly. Where Windows uses BitLocker, Rime's boot menu does not
   offer Windows; the firmware's boot menu does.

---

# PAUSED, 2026-09-21

**Andre:** *"stop all development of windows app, push to repo and everything,
but show its archived and work is paused for now cause i dont see the need."*

Development stopped here. Nothing is deleted, nothing is half-finished in the
tree, and everything that was measured has landed. This file exists so the next
person (including a future agent scanning for work) does not pick it up without
reading why it stopped.

**Do not resume this without Andre asking for it.** There is no blocker and no
bug waiting; the unit was paused on product judgement.

## State at the pause

The tree is coherent. `tests/test-windows-installer.sh` passes, the lab works,
and the `.exe` **still has no write path**: the section 0 gate that guarantees
that is unchanged and still fails both ways.

What exists: a Rust binary that cross-builds for Windows and has been run on a
real Windows Server 2022 guest. It opens `\\.\PhysicalDrive*`, reads raw GPT
bytes through IOCTLs, surveys disks and partitions, detects BitLocker by its
`-FVE-FS-` signature, decides whether a partition is eligible, and prints the
exact diskpart remedy when one is not. Plus a VM lab (`lab/winlab`, ~150 s to a
booted guest) and four jobs: `survey`, `diskpart-remedy`, `payload-write`,
`bitlocker-discover`.

What does not exist: any write from the `.exe`. Payload deployment was proven
from the PowerShell side of the lab only.

## Why the measurements outlived the feature

Three of these are facts about **Windows and about bootc**, not about this
installer, and they stay true whether or not anyone writes another line here.
Two of them contradict things that were believed. Full text in
`docs/rime-owns-its-esp.md`.

1. **"Windows will not let you" is not a safety property for the partition
   table.** Measured: Windows permits a raw write to LBA 2-33 of the disk it
   booted from, and permits `SET_DRIVE_LAYOUT_EX` on it. Anyone who assumed the
   platform was a backstop was wrong.
2. **`SET_DRIVE_LAYOUT_EX` relocates the primary GPT entry array** from LBA 2
   to LBA 2016 and leaves the stale array at LBA 2, so two disagreeing tables
   sit in the primary area and the stale one is where hardcoded GPT readers
   look. Scoped to disks with a 1 MiB reserve, which is Linux tooling's default
   and Rime's own.
3. **Windows' ESP content is not byte-identical across a boot**: 42,481 bytes
   changed, all of it Windows writing its own BCD, two files before `bcdboot`
   even ran. "Rime never writes Windows' ESP" is a rule about Rime's behaviour,
   never a claim that the partition sits still. Any future integrity check that
   asserts otherwise will fail for an innocent reason.
4. **PCR 5 is the GPT**, and where a BitLocker profile binds it, *any* GPT
   change (including merely creating Rime's own ESP) forces a 48-digit
   recovery prompt on the next Windows boot. The `bitlocker-discover` job reads
   the profile three ways, because no single way exists on every machine.

## Known limits, stated rather than buried

- The second-ESP result was measured with the new ESP **later in partition
  order**. Position-dependence (the case that would bite, since bootc takes
  `find_first_colocated_esp()`) is **untested**.
- `payload-write` is a synthetic payload. It is not a Rime install.

## The three product decisions stand

They are settled and recorded in `docs/rime-owns-its-esp.md`: Rime owns its own
ESP, the tool edits GPT entries itself under stated invariants, and firmware
writes follow the `BootNext` discipline. They were decided on their merits and
are not withdrawn by this pause: they also govern the **Linux** migration path,
which is not paused and is where that reasoning is now doing its work.

## CI keeps running, and that is deliberate

The suites stay wired into CI rather than being exempted. This repo's own
`check-suites-run-in-ci.sh` exists because "code nobody compiles rots, and rots
silently", and this directory's first round is the cited example: it shipped
655 lines that had never been built for Windows, and the unit tests all passed
because they were compiled for Linux.

So: paused means nobody adds features. It does not mean the tree stops being
checked.

**If a Windows suite goes red while paused, record it here and tell Andre; do
not resume development.** A red suite on paused work is information about
something else in the repo having moved, most likely a shared gate or the Rust
toolchain.
