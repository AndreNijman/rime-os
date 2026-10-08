# How a Windows program installs Rime, and why it is not the obvious way

`bootc install` is Linux software. It opens block devices, runs `mkfs`, writes
an ostree or composefs store through a Linux kernel's filesystem drivers, and
relabels files with the target's SELinux policy. None of that has a Windows
implementation and none of it is going to get one. So "install Rime from a
Windows app" cannot mean "run the normal installer", and the first job of this
directory is to say what it means instead.

Three routes were on the table. This page says which one was taken and gives
the measured facts that killed the other two, because a stranger who wants to
disagree with this later needs the facts, not the preference.

---

> **As built, 2026-10-08.** Route 2 below is what shipped, with one change
> forced by a measurement: the live environment is staged into **Rime's own
> new ESP**, not into the target partition. dracut keeps the live medium
> mounted read-only for the whole live session (dmsquash-live-root unmounts
> only the squashfs), so a medium inside the partition the installer formats
> could not be formatted. The ESP is the one partition the installer never
> formats, it had to exist anyway (docs/rime-owns-its-esp.md), and sized at
> ~2.25 GiB it keeps ~0.5 GiB free after Rime's boot files for the
> systemd-boot layout later. The sections below that describe writing into
> the chosen partition, or into Windows' ESP, are the record of the plan;
> "What the Windows side writes" has the as-built list.

## The decision

**Route 2: stage a payload, and complete the installation on the first boot.**

The Windows program does four things and then stops:

1. it verifies and takes exclusive ownership of one empty partition the user
   chose,
2. it writes an **Rime bootstrap environment** into that partition: a kernel,
   an initramfs and an answers file, not an operating system,
3. it adds a small, self-contained boot directory to the shared Windows ESP,
   and
4. it appends one firmware boot entry to the **end** of `BootOrder`, leaving
   Windows the default.

The first time you select that entry, Rime's own live environment comes up and
runs `installer/rime-install` in `mode=partition`, against the same partition,
on Linux, as a normal Rime installation. The real installer formats the target,
runs `bootc install to-filesystem`, creates the account, relabels it with the
target policy and queues the MOK enrolment. The Windows program never imitates
any of that.

The division of labour is the design: **Windows moves bytes; Linux installs the
operating system.** Every step Windows performs is a file copy or a firmware
variable, both of which Windows can do, and both of which are exactly
reversible.

---

## Why not Route 1: "write a prepared filesystem image to the partition"

This is the tempting one. Build the root filesystem in CI, ship it as an image,
have Windows write it to the chosen partition, add a boot entry, done. Three
measured facts stop it.

### 1. A prepared root image does not carry the half that has to boot

Rime is pivoting to systemd-boot with bootc's composefs backend
(`docs/boot-v2.md`, "The pivot to systemd-boot", Andre's decision of
2026-09-20). On that backend `/boot` **is** the FAT ESP: the kernel, the
initramfs and the loader entry all live there, not on the root filesystem:

> a loose `vmlinuz` and `initrd` under `/EFI/Linux/bootc_composefs-<verity>/`,
> a `.conf` in `/loader/entries/`, and the kernel command line on that file's
> `options` line (`docs/boot-v2.md`, "Two phases, and which one ships first")

So on the path Rime is moving to, "write the root image" installs nothing that
can start. The bootable half is a separate transaction into a partition shared
with Windows, and the ESP-size section below explains why that transaction does
not fit.

### 2. The per-machine configuration is not optional, and it is Linux-only

`installer/rime-install` does not finish when `bootc install` returns. It then
runs, inside the new deployment:

- `useradd --root "$deploy" -m -G wheel -s "$ushell"` (`installer/rime-install:1608`),
- `chroot "$deploy" /usr/sbin/setfiles -F "$spec" …` with the **target's** policy
  (`:1627`), because the live environment runs `selinux=0` and every file it
  creates is otherwise unlabelled (`:947`),
- `mokutil --import` to queue Secure Boot enrolment (`:3357`).

The relabel is not cosmetic. The installer treats its absence as fatal and says
why:

> cannot SELinux-relabel the new account files … Without the relabel, login
> would be denied. The OS is installed on $DISK but the account is not usable.
> (`installer/rime-install:1620`)

A Windows program cannot run `setfiles`, cannot run `useradd --root`, and
cannot write a MOK request into the deployment. So **a first boot on Linux is
mandatory whichever route is chosen.** Once that is true, Route 1's only
remaining advantage over Route 2 ("no second stage") is gone.

### 3. A generic image cannot be its own boot reference

On the composefs backend the image's verity digest names the ESP directory
(`/EFI/Linux/bootc_composefs-<verity>/`). That name is a property of the image
bootc produced, computed at install time. If a program that cannot compute it
copies in an image prepared elsewhere, that image's boot entry has to be
guessed.

---

## The ESP is the constraint that shapes everything

The rule for this tool is that the ESP is shared with Windows, is never
reformatted, and is only ever added to. That makes the ESP's **free space** a
hard budget, and the two Rime boot paths want very different amounts of it.

| path | what lands on the ESP | size |
| --- | --- | --- |
| ostree + GRUB (what `rime-install` installs today) | `EFI/BOOT/` + `EFI/fedora/` (shim, grub, mm, CSV, `grub.cfg`, `bootuuid.cfg`); kernels and BLS entries stay on the root filesystem | **≈ 7.47 MiB**, measured in `docs/m0-results.md`, "ESP contents + space delta" |
| systemd-boot + composefs (where Rime is going) | the whole of `/boot`: `vmlinuz` + `initrd` per deployment, sd-boot, loader entries | **374 MiB per deployment**, ≈ **1.1 GiB** for booted + rollback + a staging third (`docs/boot-v2.md`, "How reversible this is for an existing machine") |

A stock Windows ESP is **100 MB**. Windows Setup creates exactly that, and
`lab/autounattend.xml` asks for it on purpose, so the lab measures the real
constraint and not a comfortable one.

### The number, measured rather than quoted

Taken from inside a real Windows Server 2022 guest in `windows-installer/lab/`,
by mounting the ESP and asking Windows how much of it is left:

```
esp-total-bytes: 100663296     96.0 MiB
esp-used-bytes :  29087744     27.7 MiB   EFI/Microsoft/Boot + EFI/Boot
esp-free-bytes :  71575552     68.3 MiB
```

Almost all of the 27.7 MiB Windows uses is `EFI/Microsoft/Boot`: `bootmgfw.efi`
plus 33 language directories of `.mui` files and 16 boot fonts. That is the
floor on a clean install with nothing else on the machine; a real laptop with a
vendor diagnostic partition entry or a second Linux will have less.

So, against the two paths:

| path | needs | fits in 68.3 MiB? |
| --- | --- | --- |
| ostree + GRUB | 7.47 MiB | **yes**, with 60 MiB to spare |
| systemd-boot + composefs | ~1.1 GiB | **no**, short by a factor of 16 |

One deployment alone on the composefs path is 374 MiB, still five times the
whole free space. No version of this fits.

`docs/boot-v2.md` also recorded that even an **Rime** machine's ESP was too
small for the composefs path: 600 MiB on the L16 against the ~1.1 GiB needed
("What boots through systemd-boot today, measured"). A 100 MB Windows ESP is
not close. Since this page was written, the slim initramfs cut a deployment to
100.9 MiB and the three-deployment requirement to 350 MiB (`docs/boot-v2.md`,
"When it refuses", the `esp-too-small` row). That retires the refusal on an
Rime machine with a larger ESP; one deployment still does not fit in 68.3 MiB.

### The consequence

**A Windows-side Rime install cannot use the systemd-boot + composefs path into
a shared Windows ESP.** The boot files do not fit, by an order of magnitude,
and nobody can grow an ESP in place without moving the partition after it
(`docs/boot-v2.md`, "How reversible this is for an existing machine").

So this tool targets the **ostree + GRUB** backend, whose ESP cost is 7.47 MiB
and which fits inside what Windows leaves over. `installer/rime-install` passes
neither `--bootloader` nor `--composefs-backend` today, so that is also what it
already produces: the Windows path and the Linux path install the same thing.

This is a real tension with the systemd-boot decision and it is not this unit's
to resolve. It is written down here so the boot units see it: **either dual-boot
installs started from Windows stay on the ostree/GRUB backend, or a
Windows-side installer has to create a second, larger ESP, which is a GPT
modification, and everything else in this design exists to avoid one.** The
lab measures the actual free bytes in a real Windows ESP so that conversation
starts from a number.

---

## Why not Route 3: "run the installer under WSL / a shipped hypervisor"

Rejected without much argument, for the record:

- WSL2 has no raw block-device access to the physical disk the user chose, and
  installing WSL or Hyper-V is a system-wide change to the user's Windows
  machine that an installer has no business making. `bootc install` also wants
  a privileged container runtime; that is a second install.
- Shipping a hypervisor turns a "portable exe" into a driver install.
- Neither removes the mandatory Linux first boot from §2 above. They add a
  second Linux environment and keep the one that was already needed.

---

## What the Windows side writes, precisely

### As built (2026-10-08)

Into the free space the user chose, two new GPT entries, made in place in
both tables (gptwrite.rs; primary header written last, so it is the commit):

```
Rime's ESP (FAT32, label RIME-EFI, ~2.25 GiB), written whole before the entry exists:
  \EFI\rimeinst\shimx64.efi  grubx64.efi  mmx64.efi   the ISO's signed chain
  \EFI\rimeinst\grub.cfg                             generated: root=live:PARTUUID=<this ESP>
  \rimeinst\vmlinuz  initrd.img  LiveOS\squashfs.img  the ISO's live environment
  \rimeinst\handoff.cfg                               partition GUIDs, disk GUID, boot entry, BitLocker
Rime's root (Linux filesystem type, empty): first and last MiB and the MiB at
  64 MiB zeroed, so no stale signature makes it look like a filesystem.
```

The FAT32 filesystem is generated by payload.rs with every file in contiguous
clusters, so the 1.6 GB squashfs streams straight from the ISO onto the disk;
everything is read back and hashed before the table is committed. After a
successful install the Linux installer deletes `\rimeinst` and
`\EFI\rimeinst`; bootupd has by then written `\EFI\fedora` and `\EFI\BOOT`
beside them.

In firmware: one new `Boot####` "Rime OS Setup" pointing at
`\EFI\rimeinst\shimx64.efi` on Rime's ESP, and `BootNext` set to it.
`BootOrder` is not touched. Windows' own `bcdedit /enum firmware` lists it as
a firmware application with that path, and `bootsequence` as the one-shot.

### Planned (2026-09-21), superseded by the above



A FAT32 filesystem containing the Rime bootstrap:

```
/rime/vmlinuz              the Rime kernel
/rime/initramfs.img        the Rime live initramfs
/rime/answers              the install answers the GUI collected
/rime/stage.json           the transaction journal's guest-visible half
```

FAT32 rather than ext4 because the loader has to read it before Linux exists,
and because the Windows side can write it without shipping an ext4
implementation. It is a *staging* filesystem, not the future root: the first
boot's `rime-install` formats this partition as btrfs and installs into it, so
the loader reads everything above into memory and it is gone afterwards by
design.

The payload is a live environment, not an operating system image.
`rime-install`'s existing netinstall path fetches the OS itself, and that path
already carries its own guards: default route, DNS, and a ≥ 32 GB non-tmpfs
scratch check (`NEED_SCRATCH_GB`; it was 22 GB when this page was written). A 400 MB bootstrap that pulls a verified image beats a 5 GB
payload that has to survive being copied into RAM before its own partition is
reformatted.

### SUPERSEDED 2026-09-21: there is no shared Windows ESP

Andre decided that Rime builds its **own** ESP, and that Rime reads Windows'
ESP for facts and never writes it. The section below describes writing into a
shared one and stays only as the record of what was planned; do not build
toward it. An agent reading this file fresh would otherwise implement the thing
the decision forbids.

See `docs/rime-owns-its-esp.md`: both product decisions now live there,
together with the five things that must be measured before either is claimed to
work, and the invariants any GPT write has to satisfy.

### Into the shared Windows ESP

One new directory, created with create-new semantics, containing only files
that did not exist before:

```
/EFI/APEX-<transaction-id>/    the loader and its configuration
```

Nothing is written to `EFI/Microsoft`, `EFI/BOOT` or the Windows BCD. No
existing file is opened for writing, even if its hash matches what would have
been written. The transaction records every pre-existing path's hash before it
starts and re-checks it afterwards.

### Into the firmware

One new `Boot####` variable and one appended entry at the **end** of
`BootOrder`. No `BootNext`, no reordering, no replacing an earlier Rime entry.
Windows stays the default boot option, and the assertion for that is a
before/after dump of the firmware variables compared outside the guest, not an
intention written in a comment.

---

## Exclusivity: there is nothing to lock, and that changes the write path

The obvious design is to take `FSCTL_LOCK_VOLUME` on the target and write
through the locked volume handle, which confines the writes to the extent by
construction instead of by arithmetic. It does not work here, and a
measurement says why.

**Windows creates no volume object for a Linux-filesystem-type partition.**
Measured in the lab on both eligible fixtures: no volume, no drive letter, no
`\\?\Volume{…}` name, nothing to open and therefore nothing to lock. And this
program has already refused any partition that *does* have a volume, because an
overlapping volume is what "in use by Windows" means. So a lock call could only
ever run on a partition that was already refused.

What the write path gets instead, and what a reviewer should hold it to:

- writes go through `\\.\PhysicalDriveN` at the verified byte offset, with
  every offset and length validated against the partition extent before the
  call, never after;
- the volume list is **re-enumerated immediately before each write**, and the
  write is refused if anything now overlaps the extent. The survey's answer
  from ten seconds ago is not evidence about now;
- a volume enumeration that could not be completed is a refusal, not an
  absence of claims;
- Windows is **a partial backstop only, and the measurement says where it
  stops.** This used to read "Windows itself is the backstop: it refuses
  writes through a `PhysicalDrive` handle to regions a mounted volume owns."
  Nobody measured that sentence, and it is **false as it was written.**

  Measured 2026-09-21 in the lab by `lab/jobs/payload-write` against a real
  Windows Server 2022 guest, writing through `\\.\PhysicalDriveN` at a
  verified partition offset, with the bytes checked again afterwards from the
  host (evidence: `ROADMAP/evidence/windows-installer-3-20260921.md`):

  | target | result |
  |---|---|
  | mounted **NTFS** volume (`E:`, "Windows data") | **refused by Windows**: `Access to the path is denied`, HResult `0x80070005`; target bytes unchanged, confirmed host-side against the pristine fixture |
  | lettered volume with **no recognised filesystem** (`F:`, a RAW "Blank basic" partition) | **not refused: the write succeeded** and the bytes changed on disk, confirmed host-side |

  So the platform protects a mounted volume whose filesystem it *recognises*.
  A drive letter and a live volume object alone buy nothing. FAT is
  unmeasured; assume nothing about it either.

  **The consequence is a safety property.** `assess()` refuses that RAW
  partition as "in use by Windows", and that refusal is **load-bearing**: it is
  the only thing between a user and an overwritten partition, in the case a
  user is most likely to create: shrink `C:`, leave the new volume unformatted,
  and Windows letters it as RAW. For any partition with a recognised
  filesystem the offset arithmetic has a second line behind it; for a RAW one
  it is the only line. Do not weaken the ownership refusal on the theory that
  the platform will catch it.

The `inspect` command already performs the re-enumeration and prints what it
found, so the check exists and runs before any write does.

The same run measured the offset arithmetic itself. A 4 MiB payload written at
the eligible partition's verified offset landed at that offset and nowhere
else: read back from the host through the qcow2 overlay, the payload hashes to
the value the guest generated, the primary and backup GPTs are byte-identical
to the pristine fixture, and **no cluster outside the three partitions was
written at all**. The overlay's own allocation map proves that last point, a
stronger statement than a byte comparison because an unwritten cluster cannot
differ.

---

## What is undone by "undo", and what is not

Reversible, because it was additive:

- the `Boot####` variable and its position in `BootOrder`, removed only when
  the current `BootOrder` still matches what the journal recorded,
- the `EFI/APEX-<transaction-id>/` directory, removed only when every file in
  it still hashes to what the journal recorded.

Not reversible, and the tool tells the user so before it happens:

- the contents of the chosen partition. The tool verified every byte of it
  empty first, which is why "empty" is checked and never assumed, but zeroes
  that get overwritten do not come back.

Firmware writes and FAT writes are not atomic with each other. That is a
release blocker with a name: it needs fault-injection tests at every write
boundary, in VM firmware, and an Undo button does not solve it. The
transaction journal exists so that you can *diagnose* an interrupted run; it
does not make the interruption safe.

---

## What this means for the code

The priority order the work follows, and why it is that order:

1. **Enumeration**: Windows' own view of the disks, cross-checked against the
   existing raw-bytes GPT parser. Two independent sources that must agree.
2. **Selection and confirmation**: by size, label, GPT name, disk model and
   serial. **Never by device index.** NVMe enumeration on Andre's own hardware
   has reordered across three ordinary reboots; a tool that says "Disk 1" is a
   tool that erases the wrong disk on the fourth.
3. **Ownership**: refuse anything Windows has mounted or is using, and name the
   drive letter in the refusal. There is no volume to lock; see "Exclusivity"
   above.
4. **Payload deployment**: through `\\.\PhysicalDriveN` at the verified offset,
   with every offset and length checked against the extent and a volume
   re-enumeration immediately before each write, as "Exclusivity" describes.
5. **The bootloader transaction**: additive, journalled, reversible.
6. **Undo.**

Steps 4 and 5 are the ones that change a machine. Neither may run against
anything but a virtual disk until the review gates in `README.md` are closed.
