# The P2 boot matrix

What this proves, how to run it again, and the decisions it records. Gate rows: `docs/nixos/p2-gates.md` B1-B10,
X2, K1-K3. Evidence of the run recorded below: `/var/lab/rime-nix/artifacts/bootmatrix/` (not committed).

## The cells

Every cell boots the **shipped profile** (`nix/hosts/profile.nix`: desktop, services, gaming, security, catalogue,
`rime.boot.enable`) on the **real Rime kernel** (`linux-image-7.2.9`), signed the way `nix-kernel-sign.yml` signs it
(`nix/pkgs/kernel/sign-detached.sh`) but with a **throwaway TEST MOK** made in the lab, through **Fedora 45's signed
shim and GRUB** (the files bootupd installs) in OVMF with Secure Boot on (MOK enrolled) or off.

| cell | disk | menu |
|---|---|---|
| `bls-plain` | ESP + ext4 root (fresh install) | BLS entries in the ESP's `loader/entries` |
| `bls-luks` | ESP + LUKS2 (ext4 inside) | BLS |
| `custom-plain` | ESP + bootc-shaped ext4 `/boot` (ostree's `loader.1`, a Windows entry in `custom.cfg`) + btrfs `@nixos` | `custom.cfg` (migrated machine) |
| `custom-luks` | the same with the btrfs inside LUKS2 | `custom.cfg` |

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
- Space: before the first write the hook adds up the kernel and initrd files it must copy and refuses when free
  space is below that plus `rime.boot.reserveMiB` (32 MiB), naming the filesystem, both sizes and the floor, and
  writes nothing (`test_space_floor_refuses_and_writes_nothing`). A full disk part way through (ENOSPC) leaves the old
  menu in place (`boot-hook-unit`).
- Nothing on the activation path names a loader installer or an NVRAM writer (X2, `boot-loader-scan`).

### Early microcode

`rime.boot.enable` turns on Intel's and AMD's early microcode (`hardware.cpu.{intel,amd}.updateMicrocode`,
`mkDefault`). `boot-microcode` runs the real hook on the shipped system in both layouts and reads the entry's
`initrd` file: two leading uncompressed cpio archives, `GenuineIntel.bin` and `AuthenticAMD.bin`, before the
compressed initrd; with microcode off, none. Cost: the shipped initrd goes from 36.0 MB to 51.6 MB, **+15.5 MB per
generation** on the ESP / `/boot` (with 10 generations listed, up to 155 MB more if every generation has its own
initrd). A host that knows its CPU vendor can turn the other one off (handover).

### LUKS

`nix/modules/luks.nix`: `rime.luks.enable`, `rime.luks.uuid`, `rime.luks.keymap`. The root is unlocked by the
systemd initrd from `rd.luks.uuid=` on the kernel command line, never from a label and never from a crypttab entry
in the initrd: the UUID comes from the machine (`/var/lib/rime/host.toml` `[boot] kernel_params`, written by the
installer or the host layer), so one generation boots on any machine. `rime.luks.uuid` exists for a machine that
builds its own configuration. The keymap reaches the initrd's `vconsole.conf` so the passphrase is typed with the
user's layout. `luks-boot` proves this in the NixOS test VM (a fixed UUID, the passphrase typed at the console
prompt); the matrix proves it through shim and GRUB.

## Signed kernel (K1-K3)

`kernel-vm-signed`: `kernel-vm` (16 schedulers attach, survive load, detach; `kill -9` falls back) on the image
signed by `sign-detached.sh` with a deterministic TEST key, `rime.kernel.signing.require = true`; the VM boots the
signed image (`sbverify` against the TEST certificate) and `/sys/kernel/btf/vmlinux` is byte for byte the BTF
`kernel-gate` checked (K1 carries over: signing appends a signature and changes no byte of the kernel's BTF).

The scx self-test's attach window is 60 s in the checks (`nix/tests/kernel.nix`, `settle`): under katana's load
(load average 60-120 from CI and other labs) `scx_eevdf` took 16.0 s and `scx_flow`/`scx_lavd` ~11 s to attach in
a 4-vCPU VM; with the old 10 s window `scx_eevdf` failed. 60 s still fails a scheduler that never attaches.

## Results

Run on katana 2026-11-02 (00:10-01:40 AWST), worktree `nix/bootmatrix`. **All 8 runs pass, 24 of 24 boots**
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

Checks (all built on this branch; logs in `artifacts/bootmatrix/logs/`):

- `boot-loader-scan` (X2): the shipped `rime-vm` and `rime-vm-luks` toplevels, 14 files each (the hook, activation
  scripts, `switch-to-configuration`, `INSTALL_BOOTLOADER` and what they reference to depth 4): **0 findings**.
  Mutants: a system with `rime.boot` off (NixOS's own GRUB) is flagged (`install-grub.pl`: `grub-install`), and an
  activation script that names `bootctl` is flagged (line 99).
- `boot-microcode`: both layouts, two leading cpios (`GenuineIntel.bin`, `AuthenticAMD.bin`), initrd 51,584,750 B.
  Mutant: microcode off gives no cpio and 36,040,430 B, the check's `none` case.
- `luks-boot`: the systemd initrd carries the cryptsetup generator with no crypttab entry, and KEYMAP=de
  in `vconsole.conf`; the entry with `rd.luks.uuid=` boots after the passphrase is typed and `/` is
  `/dev/mapper/luks-<uuid>`. Mutant (read by hand):
  `rime-vm`'s initrd without `rime.luks` has no cryptsetup generator and no `dm_crypt`.
- `kernel-vm-signed` (K1-K3): three consecutive `nix build --rebuild` passes, 17/17 each (16 schedulers + `kill -9`),
  VERDICT PASS, at rising host load (load average 5→16, 16→25, 25→34; `scx_eevdf` attached in 3.8 s, 6.2 s and
  11.4 s). The third would have failed the old 10 s window (the mutant: `settle 10` is what failed on 2026-11-01).
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
