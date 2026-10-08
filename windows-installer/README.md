# Rime OS installer for Windows

Puts Rime OS on a Windows computer beside Windows, without a USB stick.

**Status (2026-10-08): installs.** Proven end to end in the lab on a real
Windows Server 2022 guest under OVMF with Secure Boot on (`lab/install-e2e`),
and on Windows-shaped disk images (`tests/staged-boot.sh`). It has **not**
been run on physical hardware yet; the first such run is a decision for the
owner of the machine, not for a script.

## What a person does

1. Shrink C: in Disk Management (right-click Start > Disk Management >
   right-click C: > Shrink Volume) by 30 GB or more, and leave the space
   **Unallocated**. The installer never touches NTFS, so the shrink is done
   by Windows' own tool, which knows how to move files out of the way.
2. Run `rime-windows-installer.exe` (it asks for administrator rights).
3. Choose the free space. Read the confirmation, which names the disk by
   model, serial and size (never by "Disk 1": disk numbers change between
   restarts). Install.
4. Restart. Rime's installer starts once, by itself: keyboard, network,
   account, confirm. It downloads Rime OS and installs it into the space.

Afterwards the computer starts Rime OS. Windows is started from Rime's boot
menu, or, where Windows uses BitLocker (including Windows 11's "Device
encryption"), from the firmware's boot menu (F12/F11/F9/Esc), because starting
it through GRUB would change the TPM's PCR 7 and trip BitLocker's recovery
prompt every time.

## What it does to the machine, completely

Read `src/winwrite.rs`: every Windows call that changes anything is in that
one file, and `tests/test-windows-installer.sh` section 0 fails the build if a
write API, a write IOCTL or a firmware variable name appears anywhere else.

| what | where | how it is undone |
|---|---|---|
| downloads the installer ISO | `%ProgramData%\Rime\Installer\` | delete the folder |
| writes Rime's own **ESP** (FAT32, ~2.25 GiB: shim, GRUB, the live kernel, initramfs and squashfs) | the start of the free space | `undo` |
| zeroes 3 MiB of the new root partition (old signatures) | the rest of the free space | nothing to undo |
| adds **two GPT entries**: Rime's ESP and an empty Linux root | the partition table, edited in place, primary header last | `undo` (and a byte copy of both tables is saved first) |
| creates one firmware entry, **"Rime OS Setup"**, and sets **BootNext** to it | UEFI NVRAM; BootOrder untouched | the firmware consumes BootNext; Rime's installer removes the entry after a successful install; `undo` removes it otherwise |
| suspends BitLocker for **one** restart, only if its TPM profile binds PCR 5 (the partition table) | C: | resumes by itself |

It never writes Windows' ESP, Windows' BCD, C:, or any other partition, and
never reorders boot entries.

The ISO is **pinned**: its URL, size and SHA-256 are compiled into the .exe
(`src/pin.rs`) by the workflow that publishes that ISO
(`.github/workflows/build-installer-iso.yml`). A download, or a file the user
already has, is used only if it hashes to the pin.

## How it works

`ARCHITECTURE.md` explains why a Windows program cannot simply run the Linux
installer, and what it does instead (stage Rime's own live environment, let
Linux install Linux). In order, `winstall::install`:

1. obtains the pinned ISO and checks its SHA-256;
2. reads the firmware's boot entries and picks a free number;
3. re-surveys the chosen disk (identity must match what was confirmed; Windows'
   partition list and the raw GPT must agree);
4. plans every byte: two random partition GUIDs, the FAT32 layout with every
   live file in contiguous clusters, the boot menu, the hand-off file;
5. saves both GPT copies and a journal under `%ProgramData%`;
6. writes the payload into space no partition describes yet, then reads all
   of it back and compares hashes;
7. re-reads the table, re-enumerates Windows' volumes, and commits the two GPT
   entries; asks Windows to re-read the table and checks it sees them;
8. writes "Rime OS Setup" and BootNext, reading both back.

The Linux side (`installer/rime-install`, `installer/rime-installer-gui`) reads
`\rimeinst\handoff.cfg` on the ESP it booted from: partition GUIDs, the disk
GUID, the boot entry number, whether Windows uses BitLocker. No secrets: the
account is created in Rime's installer, never on the Windows side. The GUI
checks every identifier against the real disk, skips its disk pages, and the
engine installs in partition mode onto Rime's ESP.

## Build and test

```sh
cd windows-installer
cargo test --offline --locked                 # 44 unit tests, Linux
./build-windows.sh dist                       # the .exe, in a container
../tests/test-windows-installer.sh            # gate + build + tests + wine
tests/staged-boot.sh ISO WORK [KARGS]         # stage a Windows-shaped image, boot it
lab/winlab image|media|golden|fixtures        # the Windows VM (Secure Boot on)
lab/install-e2e ISO EXE WORK                  # the whole thing, four boots
```

Set `RIME_ISO_URL`, `RIME_ISO_BYTES`, `RIME_ISO_SHA256` before
`build-windows.sh` to pin an image; without them the .exe refuses to install.

Commands (`rime-windows-installer --help`): no arguments opens the window;
`candidates`, `install ID [--iso FILE]`, `undo`, `download`, plus the
read-only `survey`, `inspect` and `lab`, and the developer `stage-image`.

## Known limits, stated

- Lab-proven only: OVMF + Windows Server 2022, and disk images. No physical
  machine has run it.
- The .exe is not Authenticode-signed, so SmartScreen warns on first run.
  Signing needs a certificate; that is a purchasing decision.
- 512-byte-sector GPT disks only (4Kn disks are refused, as before).
- UEFI only; a Windows started in legacy BIOS mode is refused with the reason.
- A release's .exe works only with that release's ISO, by design, and ISOs
  older than this change ignore the hand-off: the first installer release
  after this merge is the first one the .exe can use.
- `rime-boot-migrate` (the move to systemd-boot) still picks the first ESP on
  a disk; on a dual-boot disk it must refuse until it learns to use Rime's.

History: `PAUSED.md` (why this stopped on 2026-09-21 and was resumed on
2026-10-08), `VALIDATION.md` (what was measured, with transcripts).
