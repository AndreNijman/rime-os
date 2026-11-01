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

RESULTS
