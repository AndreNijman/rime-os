# Disk encryption in the Rime OS installer

This is the installer's half of L-002, "Enable LUKS2 by default". The other
half (what is enrolled into the LUKS2 header, and when a TPM keyslot is safe)
is `/usr/libexec/rime-luks-enroll` and `docs/boot-v2.md`.

Rime OS is moving from bootc to NixOS. The layout, the keys and the recovery
story below are the same on both; what differs on NixOS (where the unlock
arguments come from, the TPM re-enrolment, `rime-luks-enroll`'s refusal, and
what `rime migrate` does with an encrypted machine) is in
[On NixOS](#on-nixos-rime-and-moving-an-encrypted-machine-to-it).

The acceptance line for the item is *"encryption default does not strand users
across qualified hardware and recovery cases"*, so this document is organised
around the ways a person can be locked out, and what stops each one.

---

## What happens when encryption is on

`installer/rime-install` is handed `encrypt=yes` and a `lukspass=` in its
answers file, and builds the disk itself instead of letting `bootc install
to-disk` do it:

| part | size | type | filesystem | mounted at |
|------|------|------|-----------|------------|
| p1 | 1 GiB | `ef00` EFI System | FAT32 | `/boot/efi` |
| p2 | 1 GiB | `8300` Linux filesystem | ext4, **not encrypted** | `/boot` |
| p4 | 1 MiB | `ef02` BIOS boot | none | nothing |
| p3 | rest | `8309` Linux LUKS | LUKS2 → btrfs | `/` |

p4 is out of numerical order on purpose: sgdisk allocates in the order the
options are given, so the 1 MiB partition sits physically between `/boot` and
the encrypted volume while the numbers 1, 2 and 3 keep meaning what they have
always meant to the installer's own discovery, its tests, and the recovery
instructions below.

**Why a BIOS boot partition on a UEFI machine.** A UEFI machine leaves it empty
and unused. It exists because a LOOPBACK install passes bootc `--generic-image`
(the flag that stops the building machine's UEFI boot entries being rewritten),
and that flag also makes bootupd install the i386-pc GRUB component. With
nowhere to embed `core.img`, `grub2-install` refuses ("will not proceed with
blocklists") and the install fails *after* the volume is already encrypted.
Measured on 2026-09-20. One megabyte buys a layout that behaves the same in the
lab and on real hardware, which is worth more than the megabyte.

Then `bootc install to-filesystem` installs into the opened volume, with the
`/boot` partition already mounted so bootc picks up its UUID.

**Why `/boot` is outside the encryption.** Rime boots GRUB through bootupd.
GRUB can be talked into reading some LUKS2 headers, but not all, and getting
that wrong produces a machine that does not boot at all instead of one that
asks for a passphrase. A separate plain `/boot` is what Fedora ships for
encrypted installs and what this repository's own runbook specified
(`docs/m4-install-runbook.md` §5b, `docs/m0-results.md` §9). The kernel and
initramfs living in the clear leak nothing: they are the same public bytes as
the published image, and their integrity is Secure Boot's job.

**Why the partition type is `8309` and not the discoverable root GUID.** The
initramfs carries `systemd-gpt-auto-generator`, which acts on the discoverable
*root* type. Typing the LUKS partition as generic "Linux LUKS" keeps exactly
one thing responsible for opening it: the `rd.luks.*` kernel arguments below.

**Why not `bootc install to-disk --block-setup tpm2-luks`.** It exists, and it
would have been three words instead of a branch. It binds the unlock to
*"presence of the default tpm2 device"*, with no passphrase slot and no
recovery slot, and on systemd 258 `--tpm2-device=auto` binds to **no PCRs at
all** (measured on real hardware; L-002 round 31). That is an encrypted disk
with one key, held by a chip that a firmware update, a TPM clear or a
motherboard swap takes away: the stranding case, spelled as a flag. bootc's
own help points at `to-filesystem` for LUKS.

---

## The keys, and the order they are created in

1. `cryptsetup luksFormat --type luks2 --pbkdf argon2id`, passphrase on stdin.
   Never on a command line: argv is world-readable in `/proc`.
2. `/usr/libexec/rime-luks-enroll --device <dev> --recovery-out <file>`, run out
   of the **image being installed**, not the live environment, because
   enrolment policy belongs to the system it applies to. The installer offers
   the existing passphrase both as `$PASSWORD` (systemd-cryptenroll's own
   variable) and on stdin.
   The volume was formatted a moment earlier, so it has no recovery key yet:
   the helper's full path now refuses a volume that already carries one
   (re-binding the TPM is `--tpm-only`, rotating the key on purpose is
   `--replace-recovery`; `docs/boot-v2.md`), where the bootc version wiped it
   and handed out a new one.
3. The installer then **proves both keys open the volume** with `cryptsetup
   open --test-passphrase`, before the ten-minute `bootc install` starts. A
   recovery key that was printed but does not work is worse than none, and
   finding that out afterwards means finding it out on the user's machine.
4. The installer **observes in the header** whether a TPM keyslot exists
   (`cryptsetup luksDump` → `systemd-tpm2` token), and never takes it on trust.
   Only then does it add `rd.luks.options=<uuid>=tpm2-device=auto`.

**One path runs step 2 later than you would expect, and this section says so
as well as the code.** On a *network* install of a machine with nowhere to
stage the download (no second drive, no spare partition, so Rime OS is
downloaded onto the target disk as it installs), the enrolment helper lives in
an image that is not on the machine yet. Steps 2 to 4 therefore happen after
that download instead of before it: the order is luksFormat, open, mkfs, mount,
download, then enrol, prove and show. Nothing of the user's is at risk during
the gap: the volume holds only blobs that can be fetched again, the passphrase
they typed has already opened it, and the key still reaches them before
`bootc` writes the first byte of the system. Running the helper any earlier
pulled ~15 GB into the live ISO's RAM overlay, on the one machine shape that
path exists for.

If the helper is missing from the image, fails, produces no recovery key, or
produces one that does not open the volume, the install **refuses**, with the
disk carrying no data, so nothing is locked away. An encrypted disk whose only
key is one passphrase is one forgotten password away from being a brick.

---

## The recovery key: where it goes and how to use it

It reaches the user by two routes, because each fails differently.

1. **On screen.** The engine emits `RIME-INSTALL-RECOVERY-KEY: <key>` on
   stdout. The graphical installer captures that line, keeps it out of the
   scrolling install log, and shows it on the final page in large monospace,
   with a checkbox that must be ticked before **Reboot now** is enabled. A user
   who reboots past it never sees it again: the installer's RAM disk goes with
   the reboot.
2. **On the USB stick they booted from**, as a 0600
   `rime-recovery-key-<hostname>.txt` on the live ISO's own ESP. The file says,
   in as many words, that a stick in the same bag as the laptop protects
   nothing, and to move the key elsewhere and delete it.

It is **never** written to the target's ESP or `/boot`. Those live on the
machine that is encrypted, and an attacker holding the laptop has them.
It is **never** passed through `note()` or `log()`: the install log is copied
onto the stick in the clear on every failure.

**Why the recovery key survives a wrong keyboard.** systemd writes its recovery
key in a modhex alphabet whose characters sit in the same place on QWERTY,
QWERTZ and AZERTY. So it is also the answer to "the console keymap is wrong":
the owner can type it on a machine whose layout is not theirs.

---

## The keyboard, which is the other way to be locked out

The passphrase prompt is a **kernel console prompt**. It has one keymap, no
input method, no compose key and no way back. Two separate defects made that
prompt lie about which keyboard it was using.

**1. `/etc/vconsole.conf` was given an XKB layout name.** `loadkeys` does not
speak XKB. Measured against the keymap tree the image ships: of the 99 layouts
`xkeyboard-config` offers, **36 are not loadable console keymap names**.
`loadkeys` then fails and the console silently keeps the kernel's built-in map,
which is `us`. The installer now converts through systemd's own
`/usr/share/systemd/kbd-model-map`, preferring a keymap file of that exact name
when one exists (which is the order systemd itself uses), and validating the
table's answer against the keymap tree before writing it, because the table
names a `ge` keymap that Fedora 43's kbd does not ship. After conversion,
**0 of 99** resolve to something `loadkeys` cannot load.

**2. Nothing put the choice where the initramfs could read it.** The image
builds the initramfs **at build time, `--no-hostonly`**, so it cannot contain
this machine's `/etc/vconsole.conf` and is identical on every machine. On a
GRUB machine the one per-machine channel is the kernel command line:
`systemd-vconsole-setup`, which runs inside the initrd, parses
`vconsole.keymap=` from `/proc/cmdline` (verified by reading the strings of the
binary the image ships, and by the `i18n` dracut module delegating to it rather
than installing its own cmdline hook). So an encrypted install now gets

```
vconsole.keymap=<console keymap>   rd.vconsole.keymap=<console keymap>
```

added, and **only when the resolved console keymap is not exactly `us`**. The
guard is on the console keymap, not on the XKB layout: `us` + `dvorak` resolves
to `us-dvorak` and does get the argument, because that owner's fingers produce
different characters. Only a machine that lands on plain `us`, the kernel's
built-in, gains no argument that could later override `localectl set-keymap`.

On an encrypted machine the kernel argument is therefore the source of truth
for the console keymap, and `/etc/vconsole.conf` says so in a comment. That is
deliberate: changing only the file would leave the *unlock* prompt on the old
layout, which is the lockout this exists to prevent.

### The seam for systemd-boot and UKIs

A UKI carries its command line inside the signed PE, and `systemd-stub` ignores
a command line handed to it by the boot loader when Secure Boot is on. So the
`--karg` route above stops being per-machine the moment Rime pivots to
sd-boot + UKIs.

The replacement that needs no re-signing is a **system credential**:
`systemd-vconsole-setup.service` carries `ImportCredential=vconsole.*`, and both
`systemd-boot` (`\loader\credentials\*.cred`) and `systemd-stub`
(`\EFI\Linux\<uki>.efi.extra.d\*.cred`) pass credentials from the ESP into the
initrd.

**This is settled and built, and one measurement showed that the obvious
version of it does not work.**

`systemd-vconsole-setup(8)`, about the `vconsole.keymap` credential: *"The
matching options in vconsole.conf and on the kernel command line take
precedence over these credentials."* Rime's image ships `/etc/vconsole.conf`
with `KEYMAP=us`, and dracut's i18n module copies that file **into the
initramfs**. So the credential arrives, loses to a baked-in `us`, and a `.cred`
on the ESP on its own is **inert**. That comes from a boot, not from the
manual: a guest was booted with the credential and nothing else, the
credential is visible in its `/run/credentials/@system`, its
`/etc/vconsole.conf` still says `KEYMAP=us`, and the unlock was refused.

Three pieces make it work, and `installer/test-installer-keymap-boot.sh`
measures each:

| piece | file | what it does |
| --- | --- | --- |
| the credential on the ESP | `installer/rime-install` | writes `loader/credentials/vconsole.keymap.cred` on every encrypted install, alongside the karg. GRUB ignores it; a machine installed today needs no migration when the pivot happens |
| the shim that makes it count | `files/dracut/rime-unlock-hint/rime-vconsole-credential` | runs `Before=systemd-vconsole-setup.service`, pulled in by `sysinit.target.wants`, and writes the credential's value into the **initramfs's own** `/etc/vconsole.conf`: a tmpfs, gone at switch-root. Nothing on disk is touched |
| the precedence guard | the same script's first loop | a kernel command line that already sets `vconsole.keymap=` wins, and the shim says so and exits. Every machine installed before the pivot keeps working exactly as it does now |

The `.wants` directory is load-bearing, and the image build asserts it by name:
`files/scripts/check-initramfs-budget.sh`, which `Containerfile.rime` runs
against the built initramfs, fails if `sysinit.target.wants` does not pull in
the shim. `systemd-vconsole-setup` is ordered `Before=sysinit.target`, so a
unit pulled in by `initrd.target.wants` (where this module's other unit lives)
would run long after the keymap it is trying to set had been loaded. That
failure is silent; it shows up as an owner who cannot type their passphrase.

A **UKI addon** contributing a `.cmdline` section remains the other candidate,
and it is a better one in one respect: an addon is signed, and a `.cred` on the
ESP of a machine without Secure Boot is writable by anyone who can reach the
disk. The shim validates the credential as a keymap name for that reason and
drops anything else. If `sdboot-image` settles on addons instead, the
installer's side is one function, and the shim becomes dead weight but not a
fault.

---

## What is refused, and why

Every one of these fires **before anything is written**, and says so.

| case | what happens |
|------|--------------|
| `encrypt=` missing from the answers file | refused. It does not default to "no", because a dropped key would silently produce an unencrypted machine, and it does not default to "yes", which would encrypt CI disks. |
| `encrypt=yes` with `mode=partition` | refused: one partition has nowhere to put an unencrypted `/boot`. The user is told to choose whole-disk or turn encryption off. |
| a passphrase with a character outside printable ASCII | refused: the boot prompt has no accents, no emoji and no input method. The **login** password is not restricted. |
| a passphrase shorter than 8 characters | refused. |
| `encrypt=yes` with a non-btrfs root | refused: an untested combination is not something to discover on somebody's only disk. |
| `cryptsetup`, `sgdisk`, `mkfs.*` or dm-crypt missing | refused, naming what is missing and offering the unencrypted route. |
| the image has no `/usr/libexec/rime-luks-enroll` | refused, naming the path. |
| the target already carries a `crypto_LUKS` header (partition mode) | refused, exactly as before. That guard is unchanged. |
| unattended (`rime.unattended`, test builds only) with `rime.encrypt=yes` | refused: that path has nobody to show a recovery key to. |

---

## Recovering a machine you cannot get into

1. **The passphrase is refused and you are sure it is right.** The keyboard is
   probably not the one you think. Type the **recovery key** instead, at the
   same prompt: its letters are in the same place on nearly every layout.
2. **You have lost the passphrase.** Use the recovery key. It is on the USB
   stick you installed from, unless you deleted it, and it was on the last
   screen of the installer.
3. **Automatic (TPM) unlocking stopped working** after a firmware update, a TPM
   clear or a motherboard swap. The passphrase still works; if it does not, the
   recovery key does. `docs/boot-v2.md` has the measured behaviour of each of
   those cases.
4. **You have neither key.** There is no fourth option, by design. The
   acknowledgement checkbox on the last installer screen exists for this case.

Once inside, `cryptsetup luksChangeKey` changes the passphrase. A new recovery
key is `rime-luks-enroll --device /dev/<partition> --recovery-out <file>
--replace-recovery`: on a volume that already has one, `rime-luks-enroll`
**refuses** without `--replace-recovery`, because its full path wipes the
recovery slot and the key the owner filed stops working (the TPM-only re-bind,
which keeps it, is `--tpm-only`; see [On NixOS](#on-nixos-rime-and-moving-an-encrypted-machine-to-it)).
`systemd-cryptenroll --recovery-key` on its own adds a second recovery key and
leaves the first one working.

---

## On NixOS Rime, and moving an encrypted machine to it

**The same disk.** The NixOS installer (`docs/nixos/installer.md`) writes the
layout above unchanged: ESP, ext4 `rime-boot` as `/boot`, the 1 MiB BIOS boot
partition 4, LUKS2 `RIMEROOT` holding btrfs `rime-root` (now with the `@nixos`,
`@nix` and `@home` subvolumes). The recovery key is made and shown the same way,
by the same `rime-luks-enroll`, which NixOS ships in `rime-libexec`
(`nix/pkgs/rime-libexec.nix`) rather than at `/usr/libexec`.

**Unlocking.** The toplevel is one generic build, so it names no volume: the
machine's `rd.luks.uuid=`, `rd.luks.name=` (and the keymap arguments) live in
`/var/lib/rime/host.toml` `[boot] kernel_params`, and the boot hook writes them
into every menu entry. `rime.luks` (`nix/modules/luks.nix`) puts dm-crypt,
`systemd-cryptsetup` and its generator into the systemd initrd, which opens the
volume as `/dev/mapper/luks-<uuid>`; `rime.rootfs` mounts `/boot` from
host.toml's `[boot] device` (`docs/nixos/bridge.md`). The prompt, the keymap
rules and the recovery key at that prompt are the ones described above.

**TPM.** The initrd unseals a `systemd-tpm2` token by itself
(`nix/modules/luks-tpm.nix`) and asks only when the TPM refuses. PCR 7 is the
same on NixOS as on bootc Rime while shim, GRUB and the MOK stay the same
(measured in `nix/probe/ovmf/tpm`), so moving to NixOS needs no re-enrolment.
When PCR 7 does change, `sudo systemctl start
rime-luks-tpm-reenrol@<luks-uuid>.service` re-binds the TPM slot through
`rime-luks-enroll --tpm-only`, which never touches the passphrase or recovery
slots and fails if they changed. **`rime-luks-enroll` without `--tpm-only` on a
volume that already has a recovery key refuses unless `--replace-recovery` is
given**, and that flag means what it says: a new key is written and the old one
stops opening the volume. (On bootc every run rotated the key silently; that is
what the refusal stops. p2-gates T3.)

**Moving an encrypted bootc machine (`rime migrate`).** The volume, its
passphrase, its recovery key and its TPM slot are kept: the bridge writes the
new system inside the opened volume, carries the `rd.luks.*` arguments from the
running kernel's command line into host.toml, and records the `rime-boot`
partition as `[boot] device`. Before anything is written, `start`'s backup takes
the machine's keys off the disk: `keys/luks-header.img` (`cryptsetup
luksHeaderBackup`, read back with `luksDump`) and `rime backup`'s keys. The
recovery key is **not** exported, because bootc never stored it (the installer
showed it once); the header backup plus the passphrase is what restores access.
`check` refuses a LUKS root it cannot see open (`root-locked`) and a non-`us`
unlock keymap until that is proven (`luks-keymap`). Proven in the bridge's OVMF
lab with Secure Boot on: the installer's encrypted layout, the passphrase typed
in the bootc initrd and then in the NixOS initrd, back to bootc and to NixOS
again (`nix/probe/ovmf/bridge`, kind `L`; evidence
`/var/lab/rime-nix/artifacts/bridge2/L/`).

---

## What this does not do yet

* **The installer's own tests exercise no TPM enrolment.** The installer calls
  the helper and checks what came back; whether a TPM keyslot is safe is
  `rime-luks-enroll`'s decision and `docs/boot-v2.md`'s evidence.
* **A disk this installer produced has booted in the lab.** A boot on real
  hardware is a separate claim, and this document does not make it.

  `installer/test-installer-keymap-boot.sh` boots the shipped initramfs against
  a real LUKS2 volume and types the passphrase on an emulated keyboard, so the
  keymap property is measured as an owner experiences it: the same key
  positions unlock the volume on `de` and are refused on `us`. That guest uses
  a bare volume, reached by a direct kernel boot with no firmware and no
  bootloader, and not a disk this installer partitioned.

  `installer/test-installer-luks-boot.sh` covers firmware → shim → GRUB →
  kernel → unlock → the pivot into the real root; the login after that is not
  its subject. It builds an encrypted disk with the real engine, boots it under OVMF in the
  `rime-bootlab` container, types the passphrase through QMP, and reads
  dracut's own "Switching root" line from the serial log. The round 38 run, on
  2026-09-21, reported `switched-root=yes`, the first time anything had booted
  such a disk end to end (`ROADMAP/evidence/L-002-esp-fallback-20260921.md`).
  It needs root, a loop device and `/dev/kvm`, so it is listed in
  `tests/suites-not-in-ci.txt` and CI does not run it.
* **In-place encryption of an existing install is not offered**, and should not
  be: `cryptsetup reencrypt` on a live root is the most dangerous thing this
  installer could learn to do.
