# The P2 boot matrix

What this proves, how to run it again, and the decisions it records. Gate rows: `docs/nixos/p2-gates.md` B1-B10,
X2, K1-K3. Evidence of the run recorded below: `/var/lab/rime-nix/artifacts/bootmatrix/` (not committed); the
Secure Boot follow-ups (microcode split, ESP budget, lockdown, K2 mutant, the real MOK): `/var/lab/rime-nix/artifacts/secureboot/`.

## The cells

Every cell boots the **shipped profile** (`nix/hosts/profile.nix`: desktop, services, gaming, security, catalogue,
`rime.boot.enable`) on the **real Rime kernel** (`linux-image-7.2.9`), signed the way `nix-kernel-sign.yml` signs it
(`nix/pkgs/kernel/sign-detached.sh`) but with a **throwaway TEST MOK** made in the lab, through **Fedora 45's signed
shim and GRUB** (the files bootupd installs) in OVMF with Secure Boot on (MOK enrolled) or off.

| cell | disk | menu |
|---|---|---|
| `bls-plain` | ESP + ext4 root (fresh install) | BLS entries in the ESP's `loader/entries` |
| `bls-luks` | ESP + LUKS2 (ext4 inside) | BLS |
| `custom-plain` | ESP + one btrfs (topology E): `/boot` a bootc-shaped top-level directory (ostree's `loader.1`, a Windows entry in `custom.cfg`), bound at `/boot`, NixOS in `@nixos` | `custom.cfg` (migrated machine) |
| `custom-luks` | the bootc installer's LUKS layout: ESP + bootc-shaped ext4 `/boot` (`rime-boot`; ostree's `loader.1`, a Windows entry in `custom.cfg`) + LUKS2 with btrfs `@nixos` inside | `custom.cfg` |

Each cell runs with `sb-mok` and `sb-off`: 8 runs. Each run is three boots of one disk:

1. GRUB's default (generation 1). LUKS: the initrd asks, the driver screenshots the prompt and types the passphrase
   on the virtual keyboard (QMP `send-key`, one key at a time, then Return). The machine reaches the greeter
   (greetd + `rime-greet`, screenshot), then installs **generation 2**, a deliberately bad configuration (root
   `LABEL=rime-lab-missing`), with `switch-to-configuration boot`. The menu's `set default` must move to it.
2. Generation 2 is the default and fails: initrd emergency mode, its own marker; nothing reaches stage 2.
3. **Recovery** (B9 plain, B10 LUKS): the driver moves the GRUB menu selection to generation 1 with arrow keys
   (screenshots before and after the move), presses Return; (LUKS: passphrase again) generation 1 reaches the
   greeter again.

Each good boot is checked for: the stage-2 marker of the exact system path, `SecureBoot enabled|disabled` as the
firmware says, the TEST MOK in the kernel's keyring (SB on), `BOOT_IMAGE` = the hook's content-addressed copy of the
**signed** image, a `-rime` kernel, the greeter seat (`loginctl` VTNr 1, class greeter), and for LUKS
`rd.luks.uuid=<uuid>` on the entry's command line and `/` on `/dev/mapper/luks-<uuid>`, a LUKS2 header. For the
`custom` cells: ostree's `loader -> loader.1` symlink and its entry byte for byte, and the Windows entry in
`custom.cfg` exactly once.

## Running it

```
nix/probe/ovmf/boothook/run.sh matrix            # everything (≈ 1 h on katana, 4 disks of 12-14 GB apparent size)
nix/probe/ovmf/boothook/run.sh matrix-run bls-luks-sb-mok   # one run again
```

Steps: `matrix-prep` (OVMF, Fedora's shim/GRUB from the OVMF probe, the TEST MOK, varstores), `matrix-sign` (every
cell's unsigned image signed into `$LAB/sig`), `matrix-gens` (generation 1 and 2 of every cell, built with those
signatures; `require = true`), `matrix-disks`, `matrix-run` (two VMs at a time). The systems are
`nix/probe/ovmf/boothook/matrix.nix`; the disks `mkdisk.sh`; the driver `drive.py matrix CELL FW`. Results:
`$ART/results.tsv`, one row per boot, and per run `boot{1,2,3}.serial.txt`, `-menu.png`, `-passphrase.png`,
`-picked.png`, `-greeter.png`.

**The LUKS volumes are never opened on the host.** `mkdisk.sh` leaves an empty partition and a payload image (the
filesystem the root goes into); a throwaway VM (`matrix.nix` `luksWriter`: nixpkgs' kernel + a busybox initrd with
`cryptsetup`) runs `luksFormat --type luks2 --uuid <fixed> --pbkdf argon2id`, opens it and copies the payload in. The
passphrase reaches that VM on `fw_cfg`, not on the command line. Log: `$ART/luks-writer-<cell>.log`.

## Decisions recorded here

### Menu (R22)

`rime.boot.timeout = 5`, `timeout_style=menu`: v1 recovery from a bad generation is manual, through the GRUB menu,
so the menu is always drawn. bootupd's static config shows it for 1 s, too short to read the list and press a key;
5 s is long enough to read and press an arrow key (any key stops the countdown) and short enough not to slow every
boot. The matrix drives exactly that: recovery boots press the arrow key within the 5 s, the countdown stops.

### ESP rules (R20)

- The hook writes only to the boot filesystem it is given (a fresh install's own ESP at `/boot`, or bootc's
  `/boot`). If that filesystem holds Windows Boot Manager (`EFI/Microsoft/Boot/bootmgfw.efi`) it refuses **by name**
  ("this is a Windows EFI system partition") and writes nothing (`boot-hook-unit`, `test_refuses_a_windows_esp`).
- Space: before the first write the hook adds up the kernel, microcode and initrd files it must copy. When they do
  not fit above `rime.boot.reserveMiB` (32 MiB), it leaves the **oldest** generations that are not current, booted or
  pinned out of the menu until they do, logging each ("generation N left out of the menu: ... it stays in the
  profile"), and it **refuses**, naming the filesystem, the sizes and the floor and writing nothing, only when the
  current, booted and pinned generations alone do not fit (`test_prune_*`, `test_space_floor_refuses_and_writes_nothing`).
  When the new files fit only once the files of generations no longer listed are removed, it first writes a menu of
  the kept generations already on disk (bootable as it stands), removes what that menu does not name, then copies:
  a power cut in between boots one of those generations (`test_prune_interrupted_after_making_room_still_boots_*`,
  both layouts). It never removes a file a kept generation still names
  (`test_prune_keeps_a_file_a_kept_generation_shares_with_a_dropped_one`). A full disk part way through (ENOSPC)
  leaves the old menu in place (`boot-hook-unit`). Budget: "ESP budget" below.
- Nothing on the activation path names a loader installer or an NVRAM writer (X2, `boot-loader-scan`).

### Early microcode

One generic toplevel boots every machine (D9), so it names **both** vendors' early-microcode cpios (nixpkgs'
`microcode-intel` and `microcode-amd` files, unchanged) in its bootspec, `org.rimeos.microcode.v1`
(`nix/modules/boot.nix`), and does **not** prepend them to the initrd (`hardware.cpu.{intel,amd}.updateMicrocode`
default false under `rime.boot`). The hook loads them as their own `initrd` files before the generation's initrd:
one `initrd` line per file in a BLS entry, one `initrd a b c` line in `custom.cfg`'s menu. GRUB concatenates them
and the kernel's early loader reads the uncompressed cpios at the start of the image. `/var/lib/rime/host.toml`
`[boot] microcode = "intel" | "amd" | "both" | "none"` picks the vendors per machine; the default is `both`, and the
installer writes the CPU's own vendor (`/proc/cpuinfo` `vendor_id`, anything unknown keeps `both`). The files are
content-addressed like the rest, so **one copy** serves every generation that names the same package.

Proven:

- `nix/probe/ovmf/mok` `run.sh split`, through Fedora 45's shim and GRUB with Secure Boot on and the real Rime MOK
  enrolled: `retain_initrd` exposes the image GRUB handed the kernel (`/sys/firmware/initrd`), and its pieces'
  sha256s equal the Intel cpio (15,237,120 B), the AMD cpio (307,200 B) and the initrd, in that order, through
  blscfg with three `initrd` lines (U1-bls) and through a menuentry with one line naming three files (U2-menu); the
  control with one file has exactly one piece (U3-control). `/var/lab/rime-nix/artifacts/secureboot/mok/split/`.
- **Wave 4: the early loader itself, read from the guest** (same lab, `run.sh split` U1-U5 **5/5 PASS** 2026-11-03,
  `/var/lab/rime-nix/artifacts/boot2/mok/split/`). U1/U2 now also assert the microcode file names in the bytes GRUB
  handed the kernel (`AuthenticAMD.bin` and `GenuineIntel.bin`; U3: none), and the image size allows GRUB's 4-byte
  padding of each file (4,973,393 B is 4,973,396 in the image). U4 boots U2's menu (three files on one `initrd`
  line) with `-cpu host,-hypervisor` and `nokaslr`, starts QEMU stopped, and walks the loader with hardware
  breakpoints over the gdbstub (addresses from the kernel's own `System.map`): `get_microcode_blob()` is called from
  `load_ucode_intel_bsp()`, calls `find_microcode_in_initrd()`, and that returns `data=0xffff88804f075094
  size=15,228,928`: exactly the size of `kernel/x86/microcode/GenuineIntel.bin` inside nixpkgs' `intel-ucode.img`, and
  the 48 bytes at that address (read in QEMU's physical-memory mode) are byte for byte the file's first patch header
  (rev 0x45, sig 0x650, date 0x05251999). U5, the same with the initrd alone, returns `data=0 size=0`. So the early
  loader finds the microcode in the separate cpio GRUB loads before the initrd, the layout `rime-boot` writes.
- **Why the wave-3 probe read "none"** (wave-4 input, secureboot problem 4): it looked for
  `/kernel/x86/microcode/*.bin` in rootfs. nixpkgs' cpios hold that one file and **no directory entries**, and the
  kernel's initramfs unpacker (`init/initramfs.c`) creates a file only inside a directory that already exists, so the
  names never reach rootfs from these cpios, with or without a working loader. The early loader does not read rootfs:
  it scans the raw image with `find_cpio_data()`. The probe now reads the raw image (`/sys/firmware/initrd`, kept by
  `retain_initrd`).
- **Why a VM never shows a loaded revision.** With the CPUID hypervisor bit set (any KVM guest by default),
  `microcode_loader_disabled()` turns the loader off (`x86_hypervisor_present && !CONFIG_MICROCODE_DBG`,
  `arch/x86/kernel/cpu/microcode/core.c`): R1 and U1-U3 report `hypervisor-flag=1` and no `Current revision` line.
  Without the bit (U4/U5) the loader runs, but `get_microcode_blob()` still returns NULL (`mc=0`, recorded, not
  asserted): `scan_microcode()` takes only a newer revision than the CPU's (`cur_rev >= mc_header->rev` skips), and
  the guest reports the host's 0x43b, which is also the newest patch for this CPU (signature 0x906a3) in the blob
  (verified by parsing every patch header of `GenuineIntel.bin` in the pinned microcode-intel-20260925: the only
  patches for 0x906a3 are rev 0x43b, `/var/lab/rime-nix/artifacts/vboot2/ucode-906a3.txt`).
  KVM also ignores a guest's write to MSR 0x79, so no VM can show a revision being **applied**.
  P3/HARDWARE: on katana, `dmesg | grep microcode` after the first NixOS boot (`Current revision` and, with a newer
  blob than the firmware's, `updated early from ... to ...`). Today katana's firmware already runs 0x43b, the
  newest in the pinned blob, so even katana will report no early update until either moves.
- `boot-microcode`: the real hook on the shipped toplevel, both layouts, every `microcode` value, reads every file the
  entry names: the vendor cpios are wholly uncompressed cpio holding only their vendor's `.bin`, byte for byte the
  bootspec's; the last file is byte for byte `<toplevel>/initrd` and starts with **no** microcode; the boot
  filesystem holds kernel + initrd + one file per vendor, nothing else. A toplevel without the extension gets one
  `initrd` line (the negative control) and the mutant, NixOS's default prepend, **fails** the check.
- `boot-hook-unit`: dedupe across generations, vendor selection, refusing any other value by name before a write,
  `rime-boot check` naming a missing microcode file that is the 2nd path on a `custom.cfg` line. Mutants (each
  fails at least one test, `/var/lab/rime-nix/artifacts/secureboot/esp/boot-hook-unit-mutants.txt`): microcode lines
  dropped, `check` reading only the first path, prune disabled, the making-room GC removing a shared file, the
  interim menu skipped, `microcode` ignored.

Before (wave 1): both cpios were prepended into every initrd, **+15.5 MB per generation** on the ESP (36.0 → 51.6
MB). Now: 15.5 MB once for `both`, 15.2 MB for an Intel machine, 0.3 MB for an AMD one.

### ESP budget

What one generation puts on the boot filesystem (measured 2026-11-02): the signed Rime 7.2.9 image 17.7 MB
(`linux-image-7.2.9-signed/bzImage`, 17,733,752 B) and the initrd about 36-38 MB (36.0 MB for `rime` in wave 1,
37.8 MB for `rime-vm` today), so about 55 MB, under `kernel-boot-budget`'s ceiling of 130 MiB. The microcode is
15.5 MB once. Fedora's shim, GRUB and MokManager in `EFI/fedora` and `EFI/BOOT` are 2 × 6.1 MB.

- **Fresh install: a 2048 MiB ESP** (`installer/rime-install-nixos.sh` `NIXOS_ESP_MIB`). `rime.boot.configurationLimit
  = 10`; an update's peak is 11 generations (the new one is copied before the oldest is removed): 11 × 130 MiB at the
  ceiling + 15 MiB microcode + 20 MiB loaders + the 32 MiB floor = 1,497 MiB, fits. `boot-esp-budget` asserts that
  arithmetic from the module's own values and the installer's own number, and its mutant (`configurationLimit = 20`,
  2,797 MiB) fails. At today's sizes 11 generations are about 620 MB.
- **Migrated machine (`custom-cfg`, bootc's `/boot`)**: plain bootc Rime keeps `/boot` on the btrfs root, so the
  space is the root's; an encrypted bootc install has its own small ext4 `/boot` (that topology is refused by
  `rime migrate check` today, p2-gates D2). Either way the hook does not assume a size: prune-to-fit keeps the newest
  generations that fit and refuses only when the current, booted and pinned generations (130 MiB each at the
  ceiling, plus the microcode once) do not. Prune-to-fit is proven twice: in `boot-hook-unit` with the free space
  simulated (`free_bytes` replaced by a fixed-size model), and in `boot-esp-prune` on a real full ESP (below).
- **`boot-esp-prune`** (wave 4, `nix/tests/boot.nix`): a 256 MiB FAT32 disk is the ESP at `/boot` (`mkfs.vfat -F
  32`, so `statvfs` and ENOSPC are the kernel's), five generations whose initrds each carry a 28 MiB incompressible
  pad (sizes measured in the VM: kernel 13.2 MiB, microcode 15.2 MiB, initrd 28.6 MiB + pad). Generation 1 is the
  booted one and the oldest. Generations 1-3 install with nothing left out (free after 3: 56,798,720 B); generation
  4 does not fit and the hook logs `generation 2 left out of the menu: /boot cannot hold its files as well (free 54.2
  MiB, floor 32.0 MiB)`, removes 2's initrd and writes 4; generation 5 leaves out 3. Generation 1 (booted) is never
  left out and its files stay. After every install every file every entry names is on the ESP with its source's
  size, and `rime-boot check` agrees. PASS at `e94b69c6a` (`vm-test-run-rime-boot-esp-prune`). Mutants (each FAILS,
  logs in `/var/lab/rime-nix/artifacts/boot2/logs/`): the hook never leaves a generation out (`if True:` for the
  fits test) → generation 4's `switch-to-configuration boot` exits 1 with `[Errno 28] No space left on device`; the
  booted generation treated like any other → "generation 1 left out".
  Independent verification (wave 4, vboot2): a third mutant, the oldest-first order reversed (the newest optional
  generation goes first), FAILS ("generation 3 left out" at generation 4;
  `/var/lab/rime-nix/artifacts/vboot2/mutant-newest-first.log`).
- **The pruned ESP boots** (added by the wave-4 verification, vboot2). The sizes and `rime-boot check` above cannot
  see a file whose bytes are wrong, and the test VM starts with QEMU's `-kernel`, so a plain reboot never reads the
  ESP. The last subtest therefore boots off the full, pruned ESP: it reads generation 5's entry, concatenates the
  initrd files it names in its order (each padded to 4 bytes, as GRUB hands them over), `kexec -l`s the kernel file
  it names with its `options` line, `systemctl kexec`, and asserts the VM came up as generation 5
  (`/run/booted-system`, `/etc/rime-boot-test-generation`, `/proc/cmdline` = the entry's options). PASS
  `/nix/store/d2vvhjkmci74ncdkimcg20kkkzq2frca-vm-test-run-rime-boot-esp-prune`
  (`/var/lab/rime-nix/artifacts/vboot2/boot-esp-prune-kexec.log`). Mutant: the hook writes every main initrd with
  the right size and zero bytes: generations 1-5 pass every size check and `rime-boot check`, and the boot FAILS
  (`Kernel panic - not syncing: VFS: Unable to mount root fs`;
  `/var/lab/rime-nix/artifacts/vboot2/boot-esp-prune-mutant-zero-initrd.log`). GRUB reading the menu itself is not
  in this check: the boot matrix (`nix/probe/ovmf/boothook`) boots the same layout and entry format through GRUB.
- **No separate Secure Boot boot of a pruned ESP (DECIDED-NOT-TO).** A pruned ESP is the layout the boot matrix
  already boots through Fedora's shim and GRUB with Secure Boot on (B1-B10), with fewer entries; what pruning could
  break is a file an entry still names, and `boot-esp-prune` checks every one of them by size, with `rime-boot
  check` after each install, and by booting the default entry's files off the pruned ESP (kexec, above). A boot
  through shim would add the firmware, not a new path through the hook.
- `rime gc` removes generations from the profile; the hook then removes their files. A pinned generation always
  stays listed.

### LUKS

`nix/modules/luks.nix`: `rime.luks.enable`, `rime.luks.uuid`, `rime.luks.keymap`. The root is unlocked by the
systemd initrd from `rd.luks.uuid=` on the kernel command line, never from a label and never from a crypttab entry
in the initrd: the UUID comes from the machine (`/var/lib/rime/host.toml` `[boot] kernel_params`, written by the
installer or the host layer), so one generation boots on any machine. `rime.luks.uuid` exists for a machine that
builds its own configuration. The keymap reaches the initrd's `vconsole.conf` so the passphrase is typed with the
user's layout. `luks-boot` proves this in the NixOS test VM (a fixed UUID, the passphrase typed at the console
prompt); the matrix proves it through shim and GRUB.

### rime luks rebind

`rime luks status` says, per LUKS volume with a TPM slot, whether the slot still matches this boot's PCR 7 (`ok`, rc
0) or was sealed to other values (`stale`, rc 1: a new Secure Boot key, a db/dbx update, a firmware change). `rime
luks rebind` starts `rime-luks-tpm-reenrol@<uuid>.service` for each stale volume (the unit asks for the passphrase
or recovery key through systemd's password agent, `rime-luks-enroll` re-seals the TPM slot and keeps the recovery
key and passphrase slots byte for byte), then measures again. Its exit code is `failed == 0 && still.is_empty()`
(`rimed/rime/src/luks.rs`): 0 only when every unit succeeded **and** every volume measures `ok` afterwards.

- **`luks-tpm-rebind`** (the wave-3 check in `nix/tests/luks-tpm.nix`, extended in wave 4): three arms replace the unit's `ExecStart` with a runtime
  drop-in so that exactly one half fails: the unit succeeds but the slot stays stale → rc 1; the slot is re-sealed
  but the unit fails → rc 1; the unit re-seals and succeeds → rc 0. PASS at `af5208599`. Mutants of the exit-code
  line, each built in an isolated copy of the tree (`/var/lab/rime-nix/artifacts/boot2/logs/luks-mutants.txt`):
  `failed == 0 || still.is_empty()` FAILS arm 1 ("rebind reported success while the slot is still stale: rc=0");
  `still.is_empty()` alone FAILS arm 2 ("rebind reported success although its unit failed: rc=0");
  `failed == 0` alone FAILS arm 1. Each build exits 1.
- **Under Secure Boot after a real policy change** (OVMF TPM lab, `nix/probe/ovmf/tpm/run.sh run rebind-sb`; wave 4):
  OVMF with Secure Boot on, Fedora 45's shim and GRUB, the NixOS kernel signed by a TEST MOK, swtpm, one disk and
  one varstore carried from boot to boot. S1 seals a TPM slot to the boot's PCR 7; S2 unseals it with no prompt; a
  throwaway TEST certificate is then appended to **db** in the varstore (`virt-fw-vars --add-db`, what a firmware
  update or an owner's key does); S3: PCR 7 moved, the slot is refused and the passphrase is typed, `rime luks status`
  says `stale` (rc 1), `rime luks rebind` (its own unit; the driver answers the one ask) re-seals: `re-bound`, kept
  slots `unchanged`, rc 0, status `ok` afterwards; S4 unseals with no prompt under the new db, PCR 7 equal to S3's.
  **4/4 PASS** (`/var/lab/rime-nix/artifacts/boot2/tpm/rebind-sb/`: PCR 7 `eaa882d1…` before, `87f86b52…` after).
  Mutant `run mutant-rebind-skipped` (S3 only measures, nobody re-seals): S3 FAILS and S4 FAILS ("initrd asked 1
  times, expected 0"), as it must.
- **P3/HARDWARE:** katana's real PCR 7 under the real Rime MOK, its real db/dbx and its firmware; the real rebind on
  real firmware after a real dbx update.

### Kernel lockdown under Secure Boot

Decision (2026-11-02, decision A12 "keep it off at launch, as today"): **the NixOS Rime kernel boots with lockdown
`[none]` under Secure Boot, like bootc Rime today, and does not copy Fedora's lock-on-Secure-Boot.**

Why the two kernels differ, measured rather than recalled:

- Fedora's kernel (`kernel-core-7.2.13-300.fc45`, its `/lib/modules/*/config`): `CONFIG_LOCK_DOWN_IN_EFI_SECURE_BOOT=y`
  and `CONFIG_SECURITY_LOCKDOWN_LSM_EARLY=y`. That option comes from Fedora's downstream patch ("efi: Lock down the
  kernel if booted in secure boot mode"): it raises lockdown to `integrity` when the firmware reports Secure Boot,
  and the kernel prints `Kernel is locked down from EFI Secure Boot mode; see man kernel_lockdown.7` (MOK lab boot I1).
- Rime's kernel (`linux-cachyos-7.2.9-2`, the bootc kernel tier's source): the option does **not exist** in its
  `security/lockdown/Kconfig` (only `SECURITY_LOCKDOWN_LSM`, `_EARLY`, `LOCK_DOWN_KERNEL_FORCE_{NONE,INTEGRITY,
  CONFIDENTIALITY}`), and `nix/pkgs/kernel/config` has `SECURITY_LOCKDOWN_LSM=y`, `_EARLY` unset, `FORCE_NONE=y`,
  `CONFIG_LSM="lockdown,..."`. Nothing raises lockdown, so it reads `[none]` with Secure Boot on.
- bootc Rime today, measured: the published bootc kernel `7.2.9-cachyos1.apex1.fc45`, signed with the real Rime MOK
  and booted through Fedora's shim with Secure Boot on (MOK lab boot R1): `/sys/kernel/security/lockdown` =
  `[none] integrity confidentiality`, no locked-down message. katana's running system reads the same
  (`nix/modules/security.nix` G12 note).

Why not turn it on now: lockdown `integrity` refuses unsigned modules, `/dev/mem`, `/dev/port`, MSR writes from user
space, ACPI table overrides, unsigned kexec, hibernation, `bpf_probe_write_user` and parts of debugfs. Rime's own
modules would load (every module verifies against the kmodigest certificates in the image, `kernel-vm`), but the
change takes those interfaces away from the machine's owner and is a product decision, A12's to make, not a side
effect of the move. What the NixOS kernel keeps so the decision stays one line:

- the lockdown LSM is built in, listed in `CONFIG_LSM` and first in the shipped `lsm=` (`security.nix`, G12), so
  `lockdown=integrity` on the command line, or `echo integrity > /sys/kernel/security/lockdown` at run time, turns
  it on with no rebuild;
- `kernel-gate` (`nix/pkgs/kernel/gate.nix`, item 7) fails the build if the posture drifts: `SECURITY_LOCKDOWN_LSM`,
  `LOCK_DOWN_KERNEL_FORCE_NONE` and `MODULE_SIG` must be `=y`, `FORCE_INTEGRITY`, `FORCE_CONFIDENTIALITY` and
  `MODULE_SIG_FORCE` must be unset, and `CONFIG_LSM` must name `lockdown`. Mutants (each FATAL, gate script run on a
  mutated copy of the config): `artifacts/secureboot/lockdown/gate-mutants.txt`;
- `kernel-vm` / `kernel-vm-signed` boot the kernel with the shipped profile's `lsm=` and assert the LSM is loaded
  and reads `[none] integrity confidentiality`.

To turn it on later (A12 changed): add `lockdown=integrity` to the shipped kernel parameters (or set
`FORCE_INTEGRITY` and change gate item 7), change the `kernel-vm` assertion, and run the MOK lab and the boot matrix
again.

## Signed kernel (K1-K3)

`kernel-vm-signed`: `kernel-vm` (16 schedulers attach, survive load, detach; `kill -9` falls back) on the image
signed by `sign-detached.sh` with a deterministic TEST key, `rime.kernel.signing.require = true`; the VM boots the
signed image (`sbverify` against the TEST certificate) and `/sys/kernel/btf/vmlinux` is byte for byte the BTF
`kernel-gate` checked (K1 carries over: signing appends a signature and changes no byte of the kernel's BTF).

### K2 under load

The scx self-test's attach window is 60 s in the checks (`nix/tests/kernel.nix`, `settle`); bootc's self-test uses
10 s. Under katana's load on 2026-11-01/02 (load average 60-120 from CI and other labs, several VMs at once)
`scx_eevdf` took 11.4 s and 16.0 s and `scx_flow`/`scx_lavd` ~11 s to attach in a 4-vCPU VM, and the 10 s window
failed. 60 s still fails a scheduler that never attaches.

The mutant, run 2026-11-02 (`nix/tests/bootmatrix.nix` takes `settle`; `kernel-vm-signed` with `settle = 10`, built
with `--impure --expr` from `/var/lab/rime-nix/artifacts/secureboot/k2/mutant.nix`), against host CPU load made on purpose (40, then 60
`sha256sum /dev/zero` burners on katana's 20 threads):

| run | window | host load average (1 min) | `scx_eevdf` attach | verdict |
|---|---|---|---|---|
| mutant, 40 burners | 10 s | 8 → 58 | 8.5 s | PASS 17/17 |
| control (`checks.kernel-vm-signed`), 40 burners | 60 s | 59 → 63 | 9.3 s | PASS 17/17 |
| mutant `--rebuild` pass 1, 60 burners | 10 s | 72 → 91 | 9.2 s | PASS 17/17 |
| mutant `--rebuild` pass 2, 60 burners | 10 s | 90 → 88 | 8.0 s | PASS 17/17 |
| mutant `--rebuild` pass 3, 60 burners | 10 s | 87 → 93 | 8.7 s | PASS 17/17 |
| mutant, `settle = 5` | 5 s | ~90 | not enabled within 5 s | **FAIL** |

What this shows, and the record (`artifacts/secureboot/k2/`):

- **The 60 s window is not what makes K2 pass under CPU load alone.** With the host's CPUs saturated (load
  58-93) the 10 s window passed four times out of four. CPU load does slow `scx_eevdf` (3.8 s at load 5 in the
  wave-2 passes), but it levels off at 8.0-9.3 s however many burners run, under 2 s inside bootc's window. The
  11.4 s and 16.0 s attaches came with several VMs and Nix builds running at once (memory and I/O pressure as well
  as CPU), which CPU burners do not reproduce; that kind of host is what the 60 s window is for.
- **The check can fail** on the attach time: `settle = 5` fails `scx_eevdf` and `scx_flow` ("not enabled within
  5s"; 15 of 17 pass, `k2/mutant5.log`), so the window is measured, not decorative.
- Decision: keep 60 s. It is headroom for a loaded CI host, not a correctness change; a scheduler that never
  attaches still fails. Why `scx_eevdf` alone needs 8-9 s on a busy 4-vCPU VM is recorded, not fixed here.

### The real Rime MOK

Every signing proof above uses a TEST key. `nix/probe/ovmf/mok` (README there) proves what can be proven without
the private key, with the **real** Rime MOK certificate (`nix/pkgs/kernel/signing/rime-mok.pem`, CN "APEX-OS Secure
Boot key (andre)", sha256 of the DER `2fdd2f55...`) and a varstore that starts with Microsoft's keys only, no MOK of
any kind. Run 2026-11-02, all PASS (`artifacts/secureboot/mok/run/results.tsv`):

- C1: the published bootc kernel (signed in CI with the real Rime key) is refused before enrolment ("bad shim
  signature").
- I1: a Fedora-signed kernel boots under Secure Boot and runs `mokutil --import rime-mok.der` (MokNew + MokAuth
  written by the guest, nothing pre-seeded).
- M1: shim starts MokManager; the driver presses keys over QMP and saves a screenshot of every screen: press any
  key, Enroll MOK, Continue, Yes, the password, Reboot (`M1-01` ... `M1-09-*.png`).
- R1: the same bootc kernel now boots: `SecureBoot enabled`, the Rime MOK in MokListRT and in the kernel's platform
  keyring, lockdown `[none]`.
- R2: a kernel signed by a key that is not enrolled is still refused.
- V: `virt-fw-vars` reads MokList: exactly one certificate, the Rime MOK.
- Mutant: MokManager gets the wrong password (`RIME_MOK_MUTANT=wrong-password`): M1 never reaches Reboot, R1 never
  boots, V finds an empty MokList, exit 1 (`artifacts/secureboot/mok/run-mutant/`).

**Not proven: the NixOS kernel carrying the release signature.** `nix-kernel-sign.yml` cannot be dispatched from
`feat/nixos`: `gh workflow run nix-kernel-sign.yml --ref feat/nixos` and the REST dispatch both answer HTTP 404
"workflow ... not found on the default branch". GitHub dispatches a workflow only once it exists on the default
branch (`main`) or has been registered by a run, and this one has never run: its only caller, `nix-release.yml`'s
`kernel-sign` job, runs only on a release with the repository variable `RIME_NIX_KERNEL_HOST` set, which is not set
(`artifacts/secureboot/release-mok/dispatch.txt`). Pushing to `main` or setting that variable is Andre's (both
publish), so the run waits for him. When the artefact exists, `run.sh realsig DIR` checks it names this commit's
`rime` image, attaches it exactly as `sign.nix` does (`sbattach` + `sbverify --cert rime-mok.pem`, `require = true`)
and boots that NixOS kernel on the enrolled, Rime-MOK-only varstore. katana's real PCR 7 under the real MOK and its
real db/dbx is P3.

## Results

Run on katana 2026-11-02 (00:10-01:40 UTC, 08:10-09:40 AWST), worktree `nix/bootmatrix`. **All 8 runs pass, 24 of 24 boots**
(`results.tsv`; times are wall clock per boot under katana's CI load):

| run | gate | boot 1: gen 1 default, greeter, installs bad gen 2 | boot 2: bad gen 2 default, fails | boot 3: menu → gen 1, greeter |
|---|---|---|---|---|
| `bls-plain-sb-mok` | B1, B9 | PASS 102 s | PASS 58 s | PASS 79 s |
| `custom-plain-sb-mok` | B2, B9 | PASS 85 s | PASS 43 s | PASS 81 s |
| `bls-plain-sb-off` | B3, B9 | PASS 67 s | PASS 29 s | PASS 53 s |
| `custom-plain-sb-off` | B4, B9 | PASS 65 s | PASS 29 s | PASS 52 s |
| `bls-luks-sb-mok` | B5, B10 | PASS 106 s | PASS 60 s | PASS 110 s |
| `custom-luks-sb-mok` | B6, B10 | PASS 95 s | PASS 48 s | PASS 109 s |
| `bls-luks-sb-off` | B7, B10 | PASS 89 s | PASS 32 s | PASS 87 s |
| `custom-luks-sb-off` | B8, B10 | PASS 89 s | PASS 37 s | PASS 95 s |

Every run's menu order was `[1]` on boot 1 and `[2, 1]` after `switch-to-configuration boot` (the bad generation
became the default by itself); the recovery keys were `down ret`. What one good LUKS boot reports
(`custom-luks-sb-mok` boot 3, serial, shortened):

```
RIME-LAB: PASSPHRASE-ASKED 1.60 4.16
RIME-LAB: cmdline: BOOT_IMAGE=/rime/nixos/233b3613626c118170332b2894910f8d-linux-image-7.2.9-signed-bzImage.efi init=/nix/store/…-nixos-system-rime-bootmatrix-…/init quiet splash … rd.luks.uuid=5b1f0c2e-…
RIME-LAB: uname: 7.2.9-cachyos1-rime1
RIME-LAB: sb-state: SecureBoot enabled
RIME-LAB: root: /dev/mapper/luks-5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6[/@nixos] btrfs …
RIME-LAB: luks: /dev/mapper/luks-5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6  type: LUKS2 cipher: aes-xts-plain64 device: /dev/vda3
RIME-LAB: ostree: loader -> loader.1; bfd20738…  /boot/loader.1/entries/ostree-1.conf
RIME-LAB: custom.cfg windows entry: 1
RIME-LAB: GREETER-UP after 19 s: session 1 VTNr=1 Type=wayland Class=greeter ; sway pid 1122; qs pid 1169
```

The SB-off runs report `SecureBoot disabled`; every boot 2 reports `BAD generation 2 reached initrd emergency mode
(root by-label/rime-lab-missing)`. Screenshots per run: `boot{1,3}-greeter.png` (rime-greet), `boot3-menu.png` and
`boot3-picked.png` (the selection moved from generation 2 to generation 1), and for LUKS `boot*-passphrase-late.png`
(Plymouth's "Please enter passphrase for disk root (luks-5b1f0c2e-…)"; the shot 4 s after the ask still shows shim's
text, Plymouth draws a few seconds later).

Reproduced independently on 2026-11-02 (01:56-02:06 UTC) from this branch's committed rig with a separate LAB/ART:
`matrix-gens` evaluated to the same 8 toplevels as the run above, fresh disks for `bls-plain` and `custom-luks`,
`bls-plain-sb-mok` and `custom-luks-sb-off` 6 of 6 boots PASS (`artifacts/verify-bootmatrix/matrix/`).

Checks (all built on this branch; logs in `artifacts/bootmatrix/logs/`):

- `boot-loader-scan` (X2): the shipped `rime-vm` and `rime-vm-luks` toplevels, 14 files each (the hook, activation
  scripts, `switch-to-configuration`, `INSTALL_BOOTLOADER` and what they reference to depth 4): **0 findings**.
  Mutants: a system with `rime.boot` off (NixOS's own GRUB) is flagged (`install-grub.pl`: `grub-install`), and an
  activation script that names `bootctl` is flagged (line 99).
- `boot-microcode` (wave 2, superseded by "Early microcode" above): both layouts, two leading cpios
  (`GenuineIntel.bin`, `AuthenticAMD.bin`) inside the initrd, 51,584,750 B; with microcode off 36,040,430 B. The
  split layout now keeps them out of the initrd and the prepended layout is the check's mutant.
- `luks-boot`: the systemd initrd carries the cryptsetup generator with no crypttab entry, and KEYMAP=de
  in `vconsole.conf`; the entry with `rd.luks.uuid=` boots after the passphrase is typed and `/` is
  `/dev/mapper/luks-<uuid>`. Mutant (run by the verifier, 2026-11-02): `nix/modules/luks.nix` without
  `boot.initrd.luks.forceLuksSupportInInitrd` fails the check ("no cryptsetup generator in the initrd");
  log `artifacts/verify-bootmatrix/luks-boot-mutant-forceLuks.log`.
- `kernel-vm-signed` (K1-K3): three consecutive `nix build --rebuild` passes, 17/17 each (16 schedulers + `kill -9`),
  VERDICT PASS, at rising host load (load average 5→16, 16→25, 25→34; `scx_eevdf` attached in 3.8 s, 6.2 s and
  11.4 s). The third would have failed the old 10 s window; the `settle = 10` mutant run under CPU load is in "K2
  under load" above.
  The booted kernel is the signed image (sbverify, not the unsigned file) and its `/sys/kernel/btf/vmlinux` is byte
  for byte `kernel-gate`'s `vmlinux.btf`.

### Lab notes

- Fedora's `qemu-system-x86-core` has no `virtio-gpu-pci`: the matrix uses `-vga std` (the bochs DRM device).
- The probe container runs with `--security-opt label=disable` and no `:z`: a recursive relabel of `/lab` fails on a
  disk tree still being built (its `var/empty` is immutable).
- The LUKS writer VM needs the kernel's `modules` output (not `out`), the cipher and AF_ALG modules `dm_crypt` loads
  (`encrypted_key: failed to alloc_cipher (-2)` otherwise), and copies every block (a sparse copy left decrypted noise
  where ext4 expects zeros, and e2fsck failed).
- The big disks are deleted after the run; `run.sh matrix` rebuilds them (about 15 min).
