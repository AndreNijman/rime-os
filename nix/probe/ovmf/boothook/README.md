# Boot-hook lab: rime-boot in OVMF with Fedora 45's shim and GRUB

The end-to-end proof for `nix/pkgs/rime-boot` (PLAN.md D3 v1 chain, D2, D9, A8): a NixOS disk installed with
`nixos-install` and Rime's `boot.loader.external` hook, booted by Fedora 45's Microsoft-signed shim and
Fedora-signed GRUB with bootupd's static config, kernels signed by a **throwaway TEST MOK**. Nothing runs on the
host: Nix in the `rime-nix` container, keys, firmware and QEMU in `rime-ovmf-probe` (`../Containerfile`).

```
nix/probe/ovmf/boothook/run.sh all          # prep, gens, disks, scenarios (each scenario 2.5-5 min of boots)
nix/probe/ovmf/boothook/run.sh scenarios bls-sb-mok
nix/probe/ovmf/boothook/run.sh sbrecovery   # after prep, gens, disks bls (about 80 s of boots)
```

| file | what |
|---|---|
| `systems.nix` | the lab's generations, per layout: gen1 good, gen2 BAD (root label that does not exist), gen3 good. Each `<toplevel>/kernel` is the TEST-MOK-signed bzImage (detached signature + `sbattach`, the way `nix/modules/kernel.nix` links CI's signature); bootspec still names the unsigned one. Good generations run a stage-2 service that prints `RIME-BOOTHOOK:` markers on ttyS0 and does what QEMU's fw_cfg `opt/rime/action` says: `install:<toplevel>` (`nix-env --set` + `switch-to-configuration boot`) or `rollback:<N>` (`nix-env --switch-generation N` + the profile's `switch-to-configuration boot`), then powers off |
| `gens.sh` | builds the six toplevels, writes `gens.json` |
| `mkdisk.sh` | one base disk per layout: gen1 through `nixos-install` (the hook runs in its chroot), gen2/gen3 copied into the disk's store only; reads the store back from a fresh mount before it is done |
| `drive.py` | one scenario: five QEMU boots of a qcow2 overlay of that disk, the GRUB menu driven with QMP `sendkey`, screens via QMP `screendump`, a verdict per boot from positive markers only (a timeout is never a pass) |
| `run.sh` | the whole lab from the host; evidence in `/var/lab/rime-nix/artifacts/boothook/` |

## Secure Boot recovery (`run.sh sbrecovery`, drive.py `SbRecovery`)

The generations carry the TEST MOK's certificate (`rime.boot.mokCertificate`), so the hook places
`EFI/rime/rime-mok.der` on the bls disk's ESP and writes the `rime-secureboot-key` block during `nixos-install`. Four
boots of one overlay, varstore `vars-secboot.raw` (Secure Boot on, the TEST MOK NOT enrolled):

0. the menu lists "Enrol the Rime OS Secure Boot key"; the default generation is refused (`bad shim signature`).
1. End + Return picks the entry; its text and countdown show; GRUB chainloads MokManager; Enroll key from disk ->
   the ESP -> EFI -> rime -> rime-mok.der -> Continue -> Yes -> Reboot, each choice found by name in the list
   MokManager mirrors to the serial port (`mokmanager-steps.txt`); MokList read back from the varstore.
2. the same varstore: gen1 boots with Secure Boot on and the kernel loads the TEST MOK.
3. `vars-sb-off.fd`, the same disk: the entry is not in the menu; gen1 boots.

## Layouts

- **bls** (fresh install, D9): GPT [FAT ESP = `/boot`, 512 MiB][ext4 root]. shim/GRUB in `\EFI\fedora` and
  `\EFI\BOOT`, bootupd's `grub2/grub.cfg` on the ESP; the hook writes `loader/entries/rime-generation-N.conf`,
  `grub2/rime-nixos.cfg` and the marked block in `grub2/custom.cfg`.
- **custom** (migrated bootc machine, D2 topology E shape): GPT [FAT ESP: shim, GRUB, bootupd's stub][btrfs: `/boot`
  a top-level directory with ostree's `loader -> loader.1` entry, `/boot/ostree`, bootupd's `grub2`, a `custom.cfg`
  that already holds the bootc installer's Windows entry; NixOS in subvolume `@nixos`, `/boot` bound from the top
  level]. `host.toml` says `layout = "custom-cfg"`. Every good boot also checks that ostree's entry hash and loader
  symlink are unchanged and that the Windows entry is still there exactly once.

## Each scenario (layout x {sb-mok: Secure Boot on, test MOK in MokList; sb-off: blank varstore})

1. gen1 boots by default; installs BAD gen2 (`switch-to-configuration boot` in the VM).
2. gen2 is the default and fails (initrd emergency mode; marker printed by the bad generation itself).
3. the menu is driven to gen1 (arrow keys counted from the menu order on the console, then Return); gen1 boots and
   installs gen3.
4. gen3 is the default and boots; the profile is rolled back to gen1.
5. gen1 is the default again, picked by its GRUB id (in the bls layout blscfg lists it last) and boots.

A good boot must show: the stage-2 marker naming the expected generation and toplevel, `mokutil --sb-state` as
expected, with Secure Boot on the kernel's `Loaded X.509 cert 'Rime boot-hook lab TEST MOK`, `BOOT_IMAGE` = the
hook's content-addressed signed copy, the driver's last line.

## Results

See `/var/lab/rime-nix/artifacts/boothook/results.tsv` (20 rows) and per-scenario `bootN.serial.txt` /
`bootN-menu.png` / `boot3-picked.png`. Observed on 2026-11-01 (katana, KVM, edk2-ovmf 20260812-9.fc45, shim and
grub2-efi from Fedora 45 as `../lab.sh rpms` extracted them, GRUB 2.12):

| verdict | scenario | boot | what | time | menu order on the console |
|---|---|---|---|---|---|
| PASS | bls-sb-mok | boot1 | gen1 boots by default, switch-to-configuration boot makes BAD gen2 the default | 55s | menu [1] |
| PASS | bls-sb-mok | boot2 | BAD gen2 is the default and fails at boot | 58s | menu [2, 1] |
| PASS | bls-sb-mok | boot3 | menu driven to gen1 (keys: down ret), boots, installs gen3 | 57s | menu [2, 1] |
| PASS | bls-sb-mok | boot4 | GOOD gen3 is the default and boots; rollback to gen1 rewrites the default | 52s | menu [3, 2, 1] |
| PASS | bls-sb-mok | boot5 | after the rollback gen1 is the default (by id) and boots | 58s | menu [3, 2, 1] (gen1 at position 2) |
| PASS | bls-sb-off | boot1 | gen1 boots by default, switch-to-configuration boot makes BAD gen2 the default | 30s | menu [1] |
| PASS | bls-sb-off | boot2 | BAD gen2 is the default and fails at boot | 29s | menu [2, 1] |
| PASS | bls-sb-off | boot3 | menu driven to gen1 (keys: down ret), boots, installs gen3 | 28s | menu [2, 1] |
| PASS | bls-sb-off | boot4 | GOOD gen3 is the default and boots; rollback to gen1 rewrites the default | 42s | menu [3, 2, 1] |
| PASS | bls-sb-off | boot5 | after the rollback gen1 is the default (by id) and boots | 27s | menu [3, 2, 1] (gen1 at position 2) |
| PASS | custom-sb-mok | boot1 | gen1 boots by default, switch-to-configuration boot makes BAD gen2 the default | 52s | menu [1] |
| PASS | custom-sb-mok | boot2 | BAD gen2 is the default and fails at boot | 52s | menu [2, 1] |
| PASS | custom-sb-mok | boot3 | menu driven to gen1 (keys: down ret), boots, installs gen3 | 49s | menu [2, 1] |
| PASS | custom-sb-mok | boot4 | GOOD gen3 is the default and boots; rollback to gen1 rewrites the default | 66s | menu [3, 2, 1] |
| PASS | custom-sb-mok | boot5 | after the rollback gen1 is the default (by id) and boots | 57s | menu [1, 3, 2] (gen1 at position 0) |
| PASS | custom-sb-off | boot1 | gen1 boots by default, switch-to-configuration boot makes BAD gen2 the default | 36s | menu [1] |
| PASS | custom-sb-off | boot2 | BAD gen2 is the default and fails at boot | 29s | menu [2, 1] |
| PASS | custom-sb-off | boot3 | menu driven to gen1 (keys: down ret), boots, installs gen3 | 26s | menu [2, 1] |
| PASS | custom-sb-off | boot4 | GOOD gen3 is the default and boots; rollback to gen1 rewrites the default | 28s | menu [3, 2, 1] |
| PASS | custom-sb-off | boot5 | after the rollback gen1 is the default (by id) and boots | 28s | menu [1, 3, 2] (gen1 at position 0) |

## Findings

- `set default=rime-generation-N` selects a blscfg entry by its id (the file name without `.conf`): boot 5 of the
  bls scenarios boots generation 1 from position 3 of the menu.
- blscfg lists Rime's entries newest generation first (it sorts on `version`, which the hook sets to the
  generation number).
- The kernel and initrd paths: on the ESP GRUB reads `/rime/nixos/...` (BOOT_IMAGE shows `(hd0,gpt1)/rime/nixos/...`);
  on bootc's btrfs `/boot/rime/nixos/...`, both derived by the hook from mountinfo.
- Lab trap, not a product bug: the first custom disk was assembled from a btrfs image that had not been cleanly
  unmounted (the old cleanup fell back to a lazy unmount; the image then needed log replay): gen2 and gen3, copied
  last, were missing in the VM and all 10 custom boots FAILED on their markers. `mkdisk.sh` now unmounts strictly
  and reads every generation back from a fresh mount before it is done.
