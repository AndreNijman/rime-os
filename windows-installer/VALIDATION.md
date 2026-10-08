# Validation

## 2026-10-08: release v3.2.0, the published files

- The draft's `rime-os-netinstall-x86_64.iso` and `rime-windows-installer.exe`
  matched their `.sha256` files and `gh attestation verify`; the .exe carries
  the ISO's SHA-256 and the v3.2.0 URL.
- Those exact two files ran `lab/install-e2e` (four boots, Windows Server 2022,
  Secure Boot on): every check PASS. Then the release was published.
- After publishing, the released .exe on the Windows guest with a network
  card (`RIME_WINLAB_NET=1 winlab run jobs/download`): `download` fetched the
  1,969,137,664-byte ISO from GitHub through WinHTTP and verified it, 26 s.
  That was the one path the earlier runs (which used `--iso`) never took.

## 2026-10-08: the write path, end to end

Everything below ran on the build at the head of `feat/windows-installer`,
with a test installer ISO built from the same branch
(`installer/build-live-iso.sh`, PRODUCTION=1 NETINSTALL=1, pinned image
`sha256:02a7c22e…`). Lab artefacts are in `/var/lab-scratch/` on the L16
(not in git: disk images).

### The real Windows guest: `lab/install-e2e` (four boots of one machine)

Windows Server 2022 Standard Evaluation, OVMF with Secure Boot on and
Microsoft's keys, AHCI system disk made by Windows Setup (64 GiB).

| boot | what happened | result |
|---|---|---|
| 1 Windows | `Resize-Partition` shrank C: by 34 GB; `candidates` offered 36.5 GB of unallocated space (and listed the lab's other disks with reasons); `install --iso` staged Rime; Windows' `bcdedit /enum firmware` showed "Rime OS Setup", `\EFI\rimeinst\shimx64.efi`, as `bootsequence` (= BootNext); BootOrder unchanged (Windows Boot Manager first) | PASS |
| 2 setup | the firmware ran BootNext: shim -> GRUB from Rime's ESP -> live kernel with `root=live:PARTUUID=<Rime's ESP>`; the live GUI's `read_handoff()` accepted the disk; the engine installed Rime with the GUI's own answers (5.4 GB from the registry) | 15/15 PASS |
| 3 Windows | BootNext -> Windows Boot Manager; Windows started and ran its job; `chkdsk C: /scan` clean | PASS |
| 4 Rime | firmware BootOrder (Rime first), Rime's MOK cert added to db; Rime booted from its own ESP: `/boot/efi` = Rime's ESP (not the disk's first), `bootloader-update.service` Result=success, `bootupctl update` by hand rc 0, "Rime OS" entry -> Rime's ESP | 6/6 PASS |
| host | Windows' ESP, listed from outside the guest before, after the install and after Rime's own bootloader update: no Rime file, same file list | PASS |

Found by the first run and fixed: Rime's ESP was left with the FAT dirty
flag (Windows showed the volume as "Warning"). The live medium is remounted
read-write for the bootloader, and deleting the running squashfs then kept it
from going back to read-only. The engine now returns it to read-only as soon
as it is done writing and leaves the staged files for the installed system to
delete on its first boot (`rime-staged-cleanup.service`). Re-run on the final
build (`/var/lab-scratch/winlab/e2e-3`): every check above PASS, plus "FAT
clean, read-only" in boot 2 and "first boot removed the staged installer" in
boot 4; Windows lists RIME-EFI as Healthy.

Also measured: the window itself, clicked through a complete install on a
real desktop (`lab/jobs/gui-install`), from Install to its done page in 25 s
(hash 2 GB, write it, read it back), firmware showing "Rime OS Setup" as the
one-shot.

### Windows-shaped disk images: `tests/staged-boot.sh` + `tests/staged-install.py`

Windows Setup's layout (first usable LBA 34, ESP, MSR, NTFS C:, recovery at
the end) on a 64 GiB image, staged with `stage-image` (the .exe's own code),
the firmware entry the .exe writes injected with virt-fw-vars, booted under
OVMF Secure Boot:

- every Windows partition byte-identical after staging and after the whole
  install (sha256 per partition);
- sgdisk: no problems; BootNext consumed; live system up;
- the same 15 checks as boot 2 above, PASS;
- with `--bitlocker` (hand-off says Windows uses BitLocker): no GRUB Windows
  entry, and the installer's last page sends the user to the firmware boot
  menu instead (PASS).

### The window

`lab/jobs/gui` (autologon, at-logon task, BM_CLICK): the window opens on a
real Windows desktop, lists the shrunk space preselected and the lab's
other disks with their reasons, and reaches the confirmation page (disk by
model, serial and GPT GUID; Install enabled only with the box ticked).
Bugs found this way and fixed: a panic on reentry (buttons send
DM_GETDEFID while the window is being built) that made the window vanish
without a word under `panic = abort`; Enter did nothing in a non-dialog
window; a click on the hidden checkbox could enable Next.

### Unit tests and the gate

`cargo test`: 44 tests (GPT writer against sfdisk/sgdisk images, FAT32 writer
against fsck.fat/7z/blkid, ISO reader against the real ISO, boot entry
byte-identical to one real firmware booted, staging plan with a guard that
refuses any write outside the plan's ranges). `tests/test-windows-installer.sh`
section 0 mutation-tested: a `PK` write, `WriteFile` outside winwrite.rs,
IOCTL 0x0007C054, and a renamed allowlisted IOCTL each fail it.

---

# Validation record, 2026-09-21

Branch `task/windows-installer-2`, worktree from `origin/roadmap/v2.2` at
`602a8376`. `git fetch origin roadmap/v2.2` succeeded this time; the base is
the current remote tip. No push to `roadmap/v2.2`, no PR.

The previous record (2026-09-20) ended with: *"No Windows disk enumeration,
volume ownership/lock validation, GUI, destructive confirmation, payload
deployment, additive bootloader transaction or undo is implemented."*

Of those seven: **disk enumeration and ownership validation now exist and are
measured on a real Windows.** The **destructive confirmation text** exists,
has unit tests and is printed, but nothing prompts for it, because nothing
would act on the answer. **Locking turned out not to apply**, for the reason
given below. **The GUI, payload deployment, the bootloader transaction and undo
do not exist at all.** This file says how each claim it makes was checked.

---

## The Windows machine

There is one. `windows-installer/lab/winlab` builds it end to end:

| artefact | what it is |
| --- | --- |
| `ws2022-eval.iso` | Windows Server 2022 Evaluation, **5044094976 bytes**, from Microsoft's own `https://go.microsoft.com/fwlink/p/?LinkID=2195280`. Size checked against that constant on every fetch. |
| `apex-winsetup.iso` | the same media re-authored: `autounattend.xml` and `apexlab-agent.ps1` in the root, EFI El Torito image replaced with `efisys_noprompt.bin`, `install.wim` split into `install.swm` + `install2.swm`. | <!-- rime-rename: keep: the lab artifact names -->
| `golden.raw` + `golden-VARS.fd` | Windows Server 2022 Standard (Core), installed **headlessly in 3 qemu phases, about 150 seconds**. |
| `fixture-a.raw`, `fixture-b.raw` | the target disks. |

Its partition table, read by `sfdisk` from outside the guest:

```
1 : start=2048,   size=204800,   type=C12A7328-…  name="EFI system partition"
2 : start=206848, size=32768,    type=E3C9E316-…  name="Microsoft reserved partition"
3 : start=239616, size=83644416, type=EBD0A0A2-…  name="Basic data partition"
```

Its firmware variables after Setup, read by `virt-fw-vars` from outside the
guest, are the before-baseline for every claim about the Windows entry:

```
Boot0005  title="Windows Boot Manager" devpath=Partition(nr=1)/FilePath(\EFI\Microsoft\Boot\bootmgfw.efi)
BootOrder 0005, 0003, 0000, 0001, 0004
```

Fixtures, on two different buses so the enumeration order can be changed:

```
fixture-a  AHCI, model RIME-FIXTURE-A, serial FIXA00000001
   1  17 GiB  Linux filesystem type, every byte zero      "RIME-TARGET-A"
   2   1 GiB  basic data, real NTFS, label WINDATA        "Windows data"
   3  17 GiB  basic data, every byte zero                 "Blank basic"
fixture-b  NVMe, serial FIXB00000002
   1  17 GiB  Linux filesystem type, every byte zero      "RIME-TARGET-B"
```

17 GiB and not something convenient, because the tool refuses anything under
16 decimal GB, the number `installer/rime-install` refuses too. Fixtures small
enough to be quick would have exercised every rule except the one that fires in
real life. The images are sparse.

---

## The suite, run end to end

`RIME_WINLAB_GUEST=1 ./tests/test-windows-installer.sh`, one uninterrupted run,
two Windows guest boots:

```
    PASS  no disk, file or firmware WRITE API appears in the source by name
    PASS  every declared IOCTL code (5) is on the read-only allowlist
    PASS  no IOCTL code is passed as a bare numeric literal
    PASS  the Windows cross-build completed
    PASS  a binary was produced
    PASS  it is a PE32+ x86-64 Windows binary
    PASS  the partition-eligibility and confirmation rules pass (14 unit tests)
    PASS  the GPT image laboratory passes (12 cases, against the compiled binary)
    PASS  the 'never identify a disk by index' rule is among the tests that ran
    PASS  running it with no arguments exits 1
    PASS  the usage line the program itself prints reached the user
    PASS  it still declares that installation and firmware changes are disabled
    PASS  the Windows survey path runs to a stated conclusion under wine
    PASS  the Windows guest ran the survey job
    PASS  the Windows guest ran it again with the disks on swapped ports
    PASS  guest: the survey reached its own conclusion
    PASS  guest: the on-disk GPT and Windows' table agreed on every disk
    PASS  guest: no disk had disagreeing partition-table readings
    PASS  guest: the Microsoft reserved partition was refused as a protected type
    PASS  guest: the running Windows system partition was refused, and C: named
    PASS  guest: the NTFS fixture partition was refused, and its letter named
    PASS  guest: an all-zero basic-data partition was refused, Windows having lettered it
    PASS  guest: an eligible partition was read to the last byte and found zero
    PASS  guest: the exclusivity check was re-asked immediately before reading
    PASS  guest: the confirmation named the disk by its serial number
    PASS  guest: the firmware variables were unchanged by the run
    PASS  guest: Windows numbered RIME-FIXTURE-A as disk 2 and then as disk 1
    PASS  guest: the confirmation text is identical across both enumeration orders
    PASS  guest: the confirmation text contains no device index
    rime-windows-installer: 29 passed, 0 failed, 0 could-not-run
```

---

## Executed, on Linux

- `cargo build --offline --locked` and `cargo test --offline --locked`:
  **14 unit tests pass**, up from 3. The new ones are in `src/plan.rs` and
  cover the eligibility rules and the confirmation text, including:
  - a partition Windows is using is refused **before** type and size are
    considered, and the refusal names the mount point;
  - every Windows-owned type is refused even when large and entirely zero;
  - the basic-data refusal carries a `diskpart set id=` remedy and states that
    the program will not retype a partition itself;
  - the confirmation text contains model, serial and partition GUID and
    **contains no device index**, the load-bearing negative;
  - everything the program prints is ASCII.
- `windows-installer/build-windows.sh`: produces
  `PE32+ executable for MS Windows 5.02 (console), x86-64`.
- `tests/test-windows-installer.sh` proves "this build writes nothing" in two
  ways, because one is not enough. A **denylist of names** (`GENERIC_WRITE`,
  `WriteFile`, `SetFirmwareEnvironmentVariable`, `SetEndOfFile`, `DeleteFile`,
  `MoveFile`, `CreateDirectory`, …) catches the obvious. But `DeviceIoControl`
  takes an arbitrary `u32` and the control codes are hand-written hex:
  `IOCTL_DISK_SET_DRIVE_LAYOUT_EX` is `0x0007C054`, a number no name-based grep
  will ever see. So there is also an **allowlist**: every `IOCTL_`/`FSCTL_`
  constant declared in the source must be one of the five read-side codes, and
  none may be passed as a bare numeric literal. Both halves were checked to
  fail: declaring `0x0007_C054` produces *"an IOCTL/FSCTL code outside the
  read-only allowlist is declared"*, and replacing a named constant with its
  literal produces *"an IOCTL code is passed as a bare numeric literal"*.

### A difference from the Linux installer worth knowing

The 16 decimal GB minimum is borrowed from `installer/rime-install`, but not
applied to the same thing. That installer checks **`$DISK`** in both of its
modes (`installer/rime-install:849-855`), so in partition mode it never sizes
the target partition at all: a 40 GB disk with a 2 GB free partition passes its
check. This program applies the number to the **partition**, and is therefore
**stricter than the Linux installer**, not equal to it.

## Executed, in the Windows guest

`winlab run jobs/survey`, as `NT AUTHORITY\SYSTEM` on
`Microsoft Windows Server 2022 Standard Evaluation`, against four GPT disks and
eight partitions. Verbatim from the guest's own output:

- **Both readings of every partition table agreed.** Windows'
  `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` and this crate's own GPT parse of the same
  handle: `AGREE` on all four disks, `DISAGREE` on none.
- **The ESP was refused**: `in use by Windows`, `no drive letter or mount
  point -- FAT32, label "SYSTEM"`.
- **The Microsoft reserved partition was refused**: `protected partition
  type`.
- **C: was refused**: `in use by Windows … mounted at C:\ -- NTFS, label
  "Windows"`.
- **The NTFS fixture partition was refused** and the letter named: `mounted at
  E:\ -- NTFS, label "WINDATA"`.
- **The zeroed basic-data fixture was refused**, and for a stronger reason
  than its type: `mounted at F:\ -- unrecognised filesystem`. Windows had
  already given a RAW basic-data partition a drive letter. That is the design's
  claim about basic-data partitions, demonstrated rather than argued.
- **The eligible partition was read to the last byte**:
  `exclusivity no volume object covers this partition, re-checked against a
  fresh volume enumeration`, then
  `ALL-ZERO CONTENT: 18253611008/18253611008 bytes read.` That is 17 GiB,
  read through a `\\.\PhysicalDriveN` handle, not an image file.
- **The confirmation named the disk by `FIXA00000001`** and contains no
  `PhysicalDrive` and no disk number.
- **The firmware variables were byte-identical before and after**, compared by
  `virt-fw-vars` outside the guest: `IDENTICAL — no boot entry and no boot
  order changed`.

### The enumeration-order claim, made properly

The whole reason the confirmation text names a serial number and not a disk
number is that disk numbers move. So the job runs twice, and the second run
puts fixture A on a different AHCI port:

```
guest: Windows numbered RIME-FIXTURE-A as disk 2 and then as disk 1
guest: the confirmation text is identical across both enumeration orders
```

Both halves are asserted, and in that order. The first attempt at this reversed
the order of the `-device` arguments instead of the ports, which changed
nothing: `ich9-ahci` is a fixed q35 device, `nvme` takes the next free PCI slot
either way, and Windows numbers storahci before stornvme. Both runs produced
byte-identical `Get-Disk` tables, so comparing their output would have proved
nothing while looking like proof. The suite now fails if the disk number did
**not** move, before it compares anything.

### Three defects the guest found that nothing else could have

1. **The protective-MBR check refused every Windows-formatted disk.** Windows
   writes `SizeInLBA` = `0xFFFFFFFF` unconditionally; measured on a 40 GB disk
   where the correct `0x04FFFFFF` fits easily. UEFI 2.10 permits both.
2. **The GPT geometry check refused every `sfdisk`-made disk**, because it
   required first-usable-LBA to be exactly 34. `sfdisk` aligns to 1 MiB and
   writes 2048. UEFI 2.10 §5.3.2 constrains these to a range.
3. **`PARTITION_INFORMATION_EX.Name` was read at offset 104 instead of 72.** It
   did not crash: it returned `"tion"` for `"EFI system partition"`.

And one the guest could not have reported any other way: a fresh Windows Server
console is not UTF-8, and every em dash in the program's output arrived as
`???`. All printed strings are now ASCII, with a unit test to keep them so.

---

## What is still NOT implemented, and must not be assumed

- **No installation.** No payload is written anywhere. `inspect` ends with
  `INSTALLATION IS NOT IMPLEMENTED IN THIS BUILD.`
- **No bootloader transaction, no firmware write, no undo.** The design for all
  three is in `ARCHITECTURE.md`; none of it is code.
- **No GUI.** The front end is a console program. The confirmation text exists
  and is unit-tested; the screen that shows it does not.
- **No destructive confirmation prompt.** `inspect` prints the text and stops.
  Nothing asks for consent because nothing would act on it.
- **There is no volume lock, by construction rather than by omission.**
  Windows creates no volume object for a Linux-filesystem-type partition
  (measured on both eligible fixtures), and a partition that does have one has
  already been refused, because an overlapping volume is what "in use by
  Windows" means. An earlier draft carried an `FSCTL_LOCK_VOLUME` call that
  could not be reached on any input; it was removed rather than left in place,
  because safety code that never executes reads as coverage. What runs instead
  is a fresh volume enumeration immediately before the content is read, and a
  refusal if that enumeration could not be completed. The exclusivity
  mechanism the *write* path will need is described in `ARCHITECTURE.md`; it
  does not exist yet.
- **The build is the GNU target, not MSVC, and is unsigned.** No Authenticode,
  no static-CRT MSVC build, no supply-chain review.
- **No 4Kn disk, no BitLocker, no Storage Spaces, no dynamic disk, no hot
  unplug, no removable media** has been tested. The enumerator refuses
  `BytesPerSector` it does not expect only in the sense that the GPT reader is
  512-byte-sector-only; a 4Kn disk has not been put in front of it.
- **No physical disk, anywhere, ever.** Every disk in this record is a file
  under `/var/lab-scratch/winlab/`.

## Before this is pointed at a real machine

In addition to the review gates in `README.md`:

1. A human must read `ARCHITECTURE.md` and agree with the route, because the
   whole design follows from it.
2. The ESP tension has to be resolved as a product decision, not by this unit:
   the composefs path needs ~1.1 GiB of ESP and a stock Windows ESP has
   **68.3 MiB free, measured**.
3. The basic-data refusal has to be resolved as a product decision: it refuses
   exactly the partition a user produces by shrinking C:.
4. Everything in the "still NOT implemented" list above has to exist and be
   tested in the guest, at every write boundary, with fault injection.
