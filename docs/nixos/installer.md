# The NixOS installer (P2, D9)

The installer swap of the NixOS pivot: the same Fedora 45 live ISO, the same GUI and the same engine
(`installer/rime-install`), which on an ISO built with a NixOS system installs that prebuilt system instead of running
bootc. Gates I1-I3 in `p2-gates.md`.

## D9 as taken: the Fedora live ISO stays

The live environment keeps Fedora's Microsoft-signed shim and GRUB. That is the only boot chain a fresh machine with
Secure Boot on and Microsoft's keys alone (no MOK yet) will run, so it is the only way I1 can pass. A NixOS-kernel live
ISO would need a MOK-signed kernel, and shim refuses one when no MOK is enrolled. What changes is the payload and the
engine's install half; the GUI, the answers file, the refusals before erasing and the MOK enrolment flow are the bootc
installer's own.

## What the ISO carries (build-live-iso.sh, NixOS mode)

`NIXOS_SYSTEM=/nix/store/...-nixos-system-...` switches `installer/build-live-iso.sh` to NixOS mode. The bootc build is
unchanged when it is unset. In NixOS mode no OCI image and no bootc inputs are needed, and the build:

| input | stamp in `/usr/lib/rime-installer/` | used for |
|---|---|---|
| `NIXOS_SYSTEM` | `nixos-system` | the prebuilt toplevel to install (nothing is evaluated on the target) |
| `NIXOS_CACHE_DIR` (or `NIXOS_CACHE_URL`) | `nixos-cache` = `iso` (or the URL) | a binary cache holding its closure; `iso` = `nix-cache/` on the ISO |
| `NIXOS_CACHE_PUB` | `nixos-cache.pub` | the only key the install trusts (`require-sigs`, no other substituter) |
| `NIXOS_MOK_CERT` | `nixos-mok.der` | the certificate the kernel is signed with, offered for enrolment |
| `NIXOS_KERNEL_SIGNED` | `kernel-signed` | whether to offer enrolment at all (`signed`/`unsigned`) |

`rime-luks-enroll` (the recovery-key helper) is installed from `files/system/libexec/`. The live env adds `nix-core`
(Fedora's Nix 2.35) and `bootupd` (for its static GRUB config in `/usr/lib/bootupd/grub2-static`).

## What the engine writes (installer/rime-install-nixos.sh)

Refused by name before anything is erased:

* partition mode ("whole disk only");
* a root filesystem other than btrfs;
* a disk below **19 GB**. That is closure.md A11's floor: a fresh system, one update staged beside it and the rollback
  generation, as `rime update`'s space gate counts them. The message names the disk and its size;
* no timezone. The answer is used, or else the live environment's `/etc/localtime`;
* an invalid locale or kernel parameter;
* a missing system, cache key or loader file;
* a cache that does not hold the system, or whose closure is not signed by the stamped key itself (`nix store verify
  --recursive` with that key as the only trusted one; a key of the same name is not enough);
* an encrypted install whose unlock prompt would need a non-US console keymap (D9: "refused by name until the keymap
  VM gate passes"). The generic toplevel's initrd carries the US map only. This lifts when tpmkeymap's initrd keymaps
  and the boot hook's `rd.vconsole.keymap=` from host.toml land.

Then, on the whole disk:

1. **GPT.** Partition 1 is a 2 GiB ESP (`RIME-ESP`, FAT32). Partition 2 is a Linux x86-64 root (8304): plain, or LUKS2
   (argon2id) with a recovery key, the same as the bootc path.
2. **btrfs `rime-root`.** It holds `@nixos` (/), `@nix` (/nix) and `@home` (/var/home, with /home a link to it), all
   mounted `compress-force=zstd:3` (D2). `nix/installer/target.nix` is the matching NixOS side.
3. **ESP.** Fedora's shim, GRUB and MokManager go into `EFI/fedora` and the removable path `EFI/BOOT`. The bootupd
   static config goes into `EFI/*/grub.cfg` and `grub2/grub.cfg`, with `bootuuid.cfg` naming the ESP.
4. **host.toml** (`/var/lib/rime/host.toml`): hostname, timezone, `LANG`, the console and XKB keymap, and `[boot]` with
   `layout = "bls"`, plus kernel params (`rd.luks.uuid=` when encrypted, and lab arguments through `RIME_EXTRA_KARGS`).
5. **The system.** `nix copy` from the cache into the target store, then the system's OWN `nixos-install --system`
   (run in a private mount namespace with the target's /nix). It sets the profile and runs
   `switch-to-configuration boot`, whose boot hook (rime-boot, `boot.loader.external`) writes the BLS entries GRUB
   reads.
6. **The account.** `useradd`/`chpasswd` run through `nixos-enter` (`users.mutableUsers`), in wheel and the desktop
   groups. The network connections are copied.
7. **Secure Boot.** The MOK certificate goes to `/var/lib/rime/secureboot/rime-mok.der`. If it is already enrolled,
   nothing is queued. Otherwise `mokutil --import` with the answered password. An NVRAM entry for shim is created,
   except on a loop device.

## Proof

* `checks.installer-target-eval`: `nixosConfigurations.rime` (what a release ISO installs) evaluates with that layout.
* `checks.installer-nixos`: the engine, `rime-install --headless`, in a NixOS VM standing in for the live env,
  installs onto a blank 20 GiB disk. It is checked partition by partition, subvolume by subvolume, and file by file on
  the ESP, host.toml, the profile and the account. It refuses the 19 GB floor, no timezone, partition mode, a
  wrongly signed cache (another key, and another key of the same name) and LUKS with a non-US unlock keymap, and the
  disk is shown untouched each time. The EFI binaries there are fixtures; the real chain
  is the lab's job.
* `nix/installer/lab/run.sh` is the OVMF lab and holds the gate evidence. It builds:
  * the lab system, with its Rime kernel signed by a THROWAWAY TEST MOK (`sign-detached.sh`);
  * a cache signed by a TEST key;
  * a PRODUCTION ISO.

  It then boots the ISO with Secure Boot on and Microsoft's keys only (I1, `iso-qualify.py live`). It installs from
  that ISO through the engine, driven from VT 2 the way an operator could, onto blank 20 GiB disks, plain and LUKS.
  Each installed disk is booted with Secure Boot on and the TEST MOK pre-enrolled in the varstore (I2
  `plain-boot-drive.py`, I3 `luks-boot-drive.py`), and the first boot is checked through to the greeter
  (`iso-qualify.py firstboot --nixos`). The evidence is in `/var/lab/rime-nix/artifacts/installer/`.

### Results (2026-11-02, katana, nix/installer branch)

All evidence is in `/var/lab/rime-nix/artifacts/installer/`. The lab system is
`/nix/store/m987c7n31r5jjphqw777k0rj55gb8iy3-nixos-system-unnamed-26.11.20261030.1c89083` (2,100 paths, 10.88 GB NAR,
`system.txt`). Its kernel is signed by the TEST MOK (`kernel-sbverify.txt`: "Signature verification OK"). The cache is
4.0 GB, signed `rime-installer-lab-TEST-1` (`cache.txt`), and the ISO's sha256 is 53238287b658... (`rime-os-installer.iso.sha256`).

| step | result | evidence |
|---|---|---|
| I1: `iso-qualify.py live --firmware uefi-sb --media usb` on that ISO | pass: `secure_boot` true, the GUI's first page read by OCR | `i1-iso-qualify.log`, `i1-uefi-sb-usb.{json,png,serial.log}` |
| install, plain, from the ISO (SB on, Microsoft keys) | pass: `RIME-INSTALL-OK`, engine 68 s; NVRAM entry "Rime OS" for `\EFI\fedora\shimx64.efi`; MOK enrolment queued | `install-plain.log`, `rime-install-plain.log`, `result-plain.txt` |
| install, LUKS2 | pass: engine 79 s; the recovery key was enrolled (not saved to the read-only boot USB, see Handover) | `install-luks.log`, `rime-install-luks.log`, `result-luks.txt` |
| I2: `plain-boot-drive.py` (SB on, TEST MOK in the varstore) | `verdict=booted`, `switched-root=yes`, login prompt | `boot-plain.log`, `serial-boot-plain.log` ("Secure boot enabled", "systemd[1]: Switching root.") |
| I3: `luks-boot-drive.py` (SB on, TEST MOK) | `verdict=switched-root`: the prompt was seen, the passphrase typed, the pivot and "Welcome" followed | `boot-luks.log`, `serial-boot-luks.log` |
| a later boot, plain (`iso-qualify.py firstboot --nixos`; greetd had already started on the first boot of both disks, per the serial logs) | pass: the installed system is the ISO's, hostname/timezone/LANG from the answers, SecureBoot=1 and bootctl "enabled (user)", @nixos/@nix/@home with compress-force=zstd:3, the BLS entry, and greetd's greeter session on VT 1 | `firstboot-plain.{log,json,png,serial.log}` |
| `checks.installer-nixos` | pass | `check-installer-nixos.log` |

Mutants, each one failing as it should:

* `NIXOS_MIN_GB=17`: `installer-nixos` fails in "a disk below the 19 GB floor", because the 16 GiB disk is installed onto
  (`check-installer-nixos-mutant-floor17.log`).
* host.toml without its `timezone` line: `installer-nixos` fails in the host.toml subtest
  (`check-installer-nixos-mutant-no-timezone.log`).
* firstboot run with `--expect-tz Europe/Berlin --greeter-user nobody`: verdict fail, both named
  (`firstboot-plain-mutant.log`).

### Independent re-run (2026-11-02, verification of nix/installer at 14a69aada)

Evidence: `/var/lab/rime-nix/artifacts/installer-verify/`. A fresh THROWAWAY TEST MOK and TEST cache key
(`prep.txt`), the kernel re-signed with it (`sign.txt`; `kernel-sbverify.txt` "Signature verification OK", and the same
image fails against the first run's TEST MOK), the lab system
`/nix/store/irnss0hfhric61afccx06kys09zsbz81-nixos-system-unnamed-26.11.20261030.1c89083` (`system.txt`), a new
PRODUCTION ISO carrying the fixed engine (sha256 412183c3d885..., `rime-os-installer.iso.sha256`):

* I1 `iso-qualify.py live --firmware uefi-sb --media usb`: pass (`i1-iso-qualify.log`, `i1-uefi-sb-usb.*`);
* install plain and LUKS from that ISO, SB on with Microsoft keys only: pass, NVRAM entry, MOK import queued
  (`install-*.log`, `rime-install-*.log`);
* I2 `plain-boot-drive.py`: `verdict=booted`, `switched-root=yes`; I3 `luks-boot-drive.py`: `verdict=switched-root`,
  prompt seen and typed (`boot-*.log`, `serial-boot-*.log`: "Secure boot enabled" on the Rime 7.2.9 kernel);
* `firstboot --nixos`: the FIRST run on a fresh install failed, because the account's first `sudo` printed its lecture
  into the LANG section (the first run most likely passed on an account that had already used sudo during the probe's
  development). The re-run's engine is the fixed one: each install log has 2,100 `checking` lines from
  `nix store verify --recursive`. Kept as
  `lecture-race-firstboot-plain.*`. The probe now runs that sudo before its first marker, silenced; the plain disk
  was reinstalled from the same ISO and booted again (`install-plain.log`, `boot-plain.log`) and the probe passed
  (`firstboot-plain.{log,json,png,serial.log}`: greeter on VT 1, screenshot of the ridge greeter);
* `checks.installer-nixos` passed (`check-installer-nixos.log`; the final head evaluates to the same out path
  `4s8ds57w…-vm-test-run-rime-installer-nixos`, `check-installer-nixos-final.log`), with two new mutants, each failing
  where it should: the old name-only signature check (`check-installer-nixos-mutant-sig-name-only.log`: the same-name
  key passes the pre-flight, the disk is wiped, `nix copy` fails) and the LUKS keymap refusal removed
  (`check-installer-nixos-mutant-no-keymap-refusal.log`).

The lab found five bugs, all fixed. The first three were in the engine and the last two in the firstboot probe:

1. The engine refused every kernel parameter, because rime-boot's Python character class did not survive in `grep -E`.
2. The target's `nixos-install` lost its arguments (`shift 2`).
3. The bootc presence check ran on the NixOS path.
4. The firstboot probe had the wrong EFI GUID.
5. The firstboot probe read the 0077 ESP as a user.

The drivers were not edited. Since the NixOS initrd's journald does not forward PID 1 to the console, the lab adds
`systemd.log_target=kmsg` (lab only, through `RIME_EXTRA_KARGS`) so that "Switching root." reaches serial.

## Handover

* **TODO(handover) Andre / release:**
  * The production cache URL and its key. The ISO takes `NIXOS_CACHE_URL` or an embedded cache; there is no Rime cache
    or key yet.
  * The real Rime MOK signature of the release kernel. Only the TEST MOK signature has been proven.
  * Cosign/manifest verification of the toplevel before install. Today the cache signature (`require-sigs`, one key)
    is the only trust.
* **Not proven:** MokManager enrolment by a person. The boot drivers use a varstore with the TEST MOK already enrolled
  (`virt-fw-vars --add-mok`). The install queues the import (`mokutil --import`), and nobody has clicked through the
  blue screen in the lab.
* **Not supported on the NixOS path yet:**
  * partition mode is refused by name, so the Windows `.exe` flow does not work: it installs beside Windows;
  * NETINSTALL is refused by `build-live-iso.sh`.
* **bootmatrix overlap:**
  * `nix/modules/luks.nix` here is exactly bootmatrix 0add5aa48's, and its cherry-picks f020b7d91 and 859cfd35c are on
    this branch. Merge bootmatrix first, or drop those commits.
  * tpmkeymap's luks.nix change 49053ae23 (TPM enrolment, keymap in the initrd) is NOT here, so the engine refuses
    LUKS with a non-US unlock keymap by name (D9). Once tpmkeymap lands (its rime-boot appends
    `rd.vconsole.keymap=` from host.toml `[keyboard] console`), drop that refusal in `nixos_preflight`.
* **Bridge:** `nix/installer/target.nix` (`rime.installTarget`) declares the D2 mounts for a fresh install. The bridge's
  `rime.rootfs` does the same for a migrated machine, and the two should become one option.
* **Recovery key (pre-existing bootc behaviour, unchanged):** the key is written to the boot USB. That fails when the USB
  mounts read-only ("source write-protected", as in the lab), so the result says `recovery_unsaved` and the GUI shows
  the key instead.
* **Seen on the installed system (not the installer's doing):**
  * The BLS options start with the toplevel's `quiet splash root=fstab splash loglevel=3`, so `splash` appears twice.
  * systemd-journald restarts once after the pivot (`scheduled-restart-jobs=1` in the plain driver).
  * No static hostname is written (`hostnamectl --static` is empty), but the running hostname is the answered one.
* **Not probed:** the LUKS install's greeter session. `firstboot --nixos` does not type the passphrase; the LUKS serial log shows greetd started.
  * The account's zsh opens zsh-newuser-install on its first login, because there is no `~/.zshrc`.
* `installer/test-installer.sh`, run in the lab container: every engine guard passes at the branch head. Its GUI half
  draws nothing there, and the base commit fails the same way, so it needs CI's runner.
