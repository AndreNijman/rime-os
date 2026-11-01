# OVMF Secure Boot probe: Fedora shim + GRUB boot a MOK-signed NixOS kernel

PLAN.md PR 5, the P0 gate of D3 ("a NixOS kernel and initrd boot in OVMF with Secure Boot on and the MOK enrolled,
through Fedora's shim and GRUB, from a BLS entry and from a `custom.cfg` entry").

**Result (2026-11-01): the v1 chain works. 10 of 10 cases PASS** (7 original cases, reproduced unchanged by an
adversarial re-run, plus 3 added by that review: a tampered signed kernel, and a no-shim disk with Secure Boot on and off). Fedora 45's Microsoft-signed shim and Fedora-signed
GRUB boot a NixOS kernel signed with a test MOK, plus its NixOS (systemd) initrd, all the way to the NixOS stage-2
system, with Secure Boot on, both from a BLS entry on the ESP and from a `custom.cfg` entry on a bootc-shaped btrfs
`/boot`. An unsigned kernel is refused with Secure Boot on. A MOK-signed kernel is refused when the MOK is not
enrolled, so it is the MOK that admits it, not firmware `db`. A MOK-signed kernel with one byte flipped after signing
is refused (shim checks the image hash, not just that a signature is present). The firmware itself refuses the
MOK-signed kernel when it is put straight in `EFI/BOOT/BOOTX64.EFI` (no shim), and starts it when Secure Boot is off,
so OVMF really enforces `db` and the chain cannot be entered by skipping shim. With Secure Boot off an unsigned kernel
boots.

Nothing here touched a host's ESP, NVRAM, GRUB or MOK: every disk and varstore is a file, the key is a throwaway
(generated on the first run into `$ART/keys/`, reused while that directory exists), and QEMU runs in a container.

## What was tested

| Piece | Exactly |
|---|---|
| Firmware | Fedora 45 `edk2-ovmf`: `OVMF_CODE_4M.secboot` + `OVMF_VARS_4M.secboot` (Red Hat PK; Microsoft KEK 2011 + 2023; db = Windows PCA 2011, Windows UEFI CA 2023, Microsoft UEFI CA 2011, Microsoft UEFI CA 2023, Option ROM UEFI CA 2023). SB-off runs use the blank `OVMF_VARS_4M` (Setup Mode) |
| shim | `shim-x64-16.1-7.x86_64.rpm` (sha256 `04b7132d…f028`, `rpm -K`: digests signatures OK). `shimx64.efi` sha256 `351e131d…1ad3`, signed by Microsoft UEFI CA 2011 **and** Microsoft UEFI CA 2023 |
| GRUB | `grub2-efi-x64-2.12-83.fc45.x86_64.rpm` (sha256 `190badba…a198`, OK). `grubx64.efi` sha256 `49ae82fc…912f`, signed by `fedora-ca-20250530` and `fedoraca` (20200709) |
| GRUB config | bootupd's static config from `quay.io/fedora/fedora-bootc:45` (bootupd-0.3.2-1.fc45): `grub-static-efi.cfg` + `bootuuid.cfg` on the ESP, `grub-static-pre.cfg` + `configs.d/*` as `grub2/grub.cfg`. Unmodified |
| Kernel | nixpkgs `1c89083e451e891152365b799595cf7790a43325` (A1 pin), `linuxPackages_latest` = Linux 7.3.0, `/nix/store/gzdqqppva27kwqkk9dfrcda8a6jcyzl8-linux-7.3/bzImage`, sbsigned with a test MOK (RSA-2048, CN "Rime OVMF probe TEST MOK (throwaway, DO NOT TRUST)") |
| initrd | NixOS systemd initrd, `/nix/store/wvpdhcxfi262j179v2ycjfj2a247b1kw-initrd-linux-7.3/initrd` (29 MB), unsigned (GRUB does not verify initrds) |
| System | `system.nix`: minimal NixOS, no NixOS loader module, root ext4 by label; a oneshot service prints `RIME-OVMF-PROBE:` lines (SB state, MokListRT, kernel keyring) to ttyS0 and powers off |
| MOK enrolment | **pre-seeded** `MokList` (NV+BS) with `virt-fw-vars --add-mok`: the state MokManager leaves after a user enrols. The `mokutil --import` → MokManager screen path is **not** tested here |

## Layouts

**(a) fresh install, BLS** (`disk-bls-*.img`, one 256 MiB FAT ESP = `/boot`, D9):
```
EFI/BOOT/BOOTX64.EFI (shim) grubx64.efi mmx64.efi grub.cfg (= grub-static-efi.cfg) bootuuid.cfg
EFI/fedora/shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV grub.cfg bootuuid.cfg
grub2/grub.cfg (= pre + configs.d, bootupd's) grub2/bootuuid.cfg      BOOT_UUID = the ESP (1234-ABCD)
loader/entries/rime-nixos-generation-1.conf
rime/nixos/<hash>-linux-7.3-bzImage.efi  rime/nixos/<hash>-initrd-linux-7.3-initrd.efi
```
```
title Rime (NixOS 26.11pre-git) generation 1, kernel signed
version 1
linux /rime/nixos/gzdqqppva27kwqkk9dfrcda8a6jcyzl8-linux-7.3-bzImage.efi
initrd /rime/nixos/wvpdhcxfi262j179v2ycjfj2a247b1kw-initrd-linux-7.3-initrd.efi
options init=/nix/store/…-nixos-system-rime-ovmf-probe-26.11pre-git/init console=ttyS0,115200 root=fstab loglevel=4 lsm=landlock,yama,bpf
```

**(b) migrated bootc machine, custom.cfg** (`disk-custom-*.img`: 64 MiB ESP + 512 MiB btrfs with `/boot` as a
directory, katana's topology E shape). The ESP's stub finds `/boot` by `BOOT_UUID` and takes the
`($prefix)/boot/grub2` branch. ostree's `/boot/loader -> loader.1` holds a bootc BLS entry ("Fedora Linux 45
(ostree:0)", listed first in the menu; its kernel is deliberately absent). Rime adds only:
```
/boot/grub2/custom.cfg        if [ -f ${prefix}/rime-nixos.cfg ]; then source ${prefix}/rime-nixos.cfg; fi
/boot/grub2/rime-nixos.cfg    menuentry 'Rime (NixOS …) generation 1, kernel signed' --id rime-nixos-generation-1 {
                                linux /boot/rime/nixos/<hash>-linux-7.3-bzImage.efi init=…/init console=ttyS0,115200 root=fstab …
                                initrd /boot/rime/nixos/<hash>-initrd-linux-7.3-initrd.efi }
                              set timeout_style=menu; set timeout=3; set default=rime-nixos-generation-1
/boot/rime/nixos/…            kernel + initrd
```
The NixOS root filesystem (ext4, label `nixos-probe`) is a second virtio disk (a qcow2 overlay per run) in every case;
the boot chain does not depend on where the root is.

## Results

`/var/lab/rime-nix/artifacts/ovmf/logs/results.tsv` (review run of 2026-11-01 10:28-10:39 UTC, 10/10 PASS, rc 0, at a
host load average of ~88 from other workflows' builds, so boots took 29-146 s; an idle run took 18-30 s per boot. The
original 7-case run, 09:50-09:57 UTC, is kept in `logs-claim-0951Z/`, and its first full re-run, 10:05-10:11 UTC, in
`logs-rerun-1011Z/`, both 7/7 PASS):

| Case | Disk | Firmware | Expect | Got | Kernel saw |
|---|---|---|---|---|---|
| a-bls-sbon-mok-signed | (a) BLS, MOK-signed kernel | SB on + test MOK | boots | **boots** to stage 2 | `SecureBoot enabled`, test MOK in MokListRT |
| b-custom-sbon-mok-signed | (b) custom.cfg, MOK-signed | SB on + test MOK | boots | **boots** to stage 2 | `SecureBoot enabled`, test MOK in MokListRT |
| a-bls-sbon-mok-unsigned | (a) BLS, unsigned kernel | SB on + test MOK | refused | **refused** (`bad shim signature`) | never ran |
| b-custom-sbon-mok-unsigned | (b) custom.cfg, unsigned | SB on + test MOK | refused | **refused** (`bad shim signature`) | never ran |
| a-bls-sbon-nomok-signed | (a) BLS, MOK-signed kernel | SB on, no MOK | refused | **refused** (`Binary is not authorized`) | never ran |
| a-bls-sboff-unsigned | (a) BLS, unsigned kernel | SB off (Setup Mode) | boots | **boots** to stage 2 | `SecureBoot disabled` |
| b-custom-sboff-unsigned | (b) custom.cfg, unsigned | SB off (Setup Mode) | boots | **boots** to stage 2 | `SecureBoot disabled` |
| a-bls-sbon-mok-tampered | (a) BLS, MOK-signed kernel with byte size/2 flipped after signing (`sbverify`: "Image fails hash check") | SB on + test MOK | refused | **refused** (`Binary is not authorized`, `bad shim signature`) | never ran |
| c-fw-sbon-mok-mokkernel | (c) ESP holding only `EFI/BOOT/BOOTX64.EFI` = the MOK-signed kernel (no shim, no GRUB) | SB on + test MOK | fw-refused | **fw-refused** (`Access Denied -- rejected probably by Secure Boot`) | never ran |
| c-fw-sboff-mokkernel | (c), the control | SB off (Setup Mode) | fw-starts | **fw-starts** (`BdsDxe: starting Boot0002`) | (bare kernel, no initrd: not followed) |

"boots" = the NixOS stage-2 service printed `RIME-OVMF-PROBE: DONE`, its `uname -r` and `/run/current-system` equal
`entry.json`'s kernel version and toplevel, and `BOOT_IMAGE` names this build's `<hash>-linux-7.3-bzImage.efi`
(and, with SB on, the kernel reported Secure Boot enabled and loaded the test MOK from MokListRT). "refused" = GRUB's
shim_lock verifier refused the kernel and no kernel output appears. "fw-refused" = BdsDxe `failed to load … Access
Denied` on the console plus `DxeImageVerificationLib: Image is signed but signature is not allowed by DB` and
`The image doesn't pass verification: …\EFI\BOOT\BOOTX64.EFI` in the OVMF debug log, and no shim line. "fw-starts" =
BdsDxe `starting Boot…` with no image-verification failure. A timeout alone is never a PASS.

The kernel under test is the pinned build: a clean re-run rebuilt the same store paths
(`gzdqqppva27kwqkk9dfrcda8a6jcyzl8-linux-7.3`, `wvpdhcxfi262j179v2ycjfj2a247b1kw-initrd-linux-7.3`,
`lbbka71ys79l2r6vrmqsk279k9hsgwls-nixos-system-rime-ovmf-probe-26.11pre-git`) and the same unsigned `bzImage`
(sha256 `2272d497…4238`, equal to the store file). `bzImage.signed` differs per signing run (the signature carries a
timestamp).

### Log excerpts

Full serial logs: `/var/lab/rime-nix/artifacts/ovmf/logs/<case>.serial.log` (raw) and `.serial.txt` (ANSI stripped);
firmware debug logs `<case>.ovmf.log`; the per-case lines below are in `logs/excerpts.txt`. Shim runs verbose
(`SHIM_VERBOSE`), so its verification steps are on the serial console.

(a) SB on, MOK-signed kernel: shim checks GRUB's `linux` load against db (no match), then MokListRT (match), and
accepts a kernel with no `.sbat` section:
```
shim.c:152:load_image() attempting to load \EFI\BOOT\grubx64.efi
…  (GRUB menu: *Rime (NixOS 26.11pre-git) generation 1, kernel signed)
verify.c:150:check_db_cert_in_ram() trying to verify cert 1 (db)   … cert 3, 5, 7, 9 (db)
verify.c:150:check_db_cert_in_ram() trying to verify cert 1 (MokListRT)
verify.c:150:check_db_cert_in_ram() trying to verify cert 3 (MokListRT)
verify.c:160:check_db_cert_in_ram() AuthenticodeVerify() succeeded: 1
pe.c:489:verify_sbat_section() No .sbat section data
Booting initrd of NixOS 26.11 (Zokor) (Initrd).
RIME-OVMF-PROBE: stage-2 reached: 7.3.0 system=/nix/store/lbbka71ys79l2r6vrmqsk279k9hsgwls-nixos-system-rime-ovmf-probe-26.11pre-git
RIME-OVMF-PROBE: cmdline: BOOT_IMAGE=(hd0,gpt1)/rime/nixos/gzdqqppva27kwqkk9dfrcda8a6jcyzl8-linux-7.3-bzImage.efi init=/nix/store/…/init console=ttyS0,115200 root=fstab …
RIME-OVMF-PROBE: mokutil --sb-state: SecureBoot enabled
RIME-OVMF-PROBE:           Subject: CN=Rime OVMF probe TEST MOK (throwaway, DO NOT TRUST)
RIME-OVMF-PROBE:   [    0.019109] Secure boot enabled
RIME-OVMF-PROBE:   [    0.579586] integrity: Loaded X.509 cert 'Rime OVMF probe TEST MOK (throwaway, DO NOT TRUST): a7e15c6802be3d2efc89fc74cf2a34fc5ae84f0e'
RIME-OVMF-PROBE: DONE, powering off
```
(b) SB on, custom.cfg: the menu lists the ostree entry first, `set default=rime-nixos-generation-1` from
`rime-nixos.cfg` wins, and the kernel comes off btrfs:
```
RIME-OVMF-PROBE: cmdline: BOOT_IMAGE=/boot/rime/nixos/gzdqqppva27kwqkk9dfrcda8a6jcyzl8-linux-7.3-bzImage.efi init=/nix/store/…/init …
RIME-OVMF-PROBE: mokutil --sb-state: SecureBoot enabled
RIME-OVMF-PROBE:   [    0.804360] integrity: Loaded X.509 cert 'Rime OVMF probe TEST MOK (throwaway, DO NOT TRUST): a7e15c6802be3d2efc89fc74cf2a34fc5ae84f0e'
RIME-OVMF-PROBE: DONE, powering off
```
Unsigned kernel, SB on (a and b identical):
```
verify.c:590:verify_buffer_authenticode() No signatures found
error: ../../grub-core/kern/efi/sb.c:shim_lock_verifier_write:192:bad shim signature.
error: ../../grub-core/loader/i386/efi/linux.c:grub_cmd_initrd:260:you need to load the kernel first.
Press any key to continue...
  Failed to boot both default and fallback entries.
```
MOK-signed kernel, SB on, MOK not enrolled:
```
verify.c:660:verify_buffer_authenticode() Binary is not authorized
SSL Error: verify.c:664 verify_buffer_authenticode(): Security Policy Violation
error: ../../grub-core/kern/efi/sb.c:shim_lock_verifier_write:192:bad shim signature.
```
SB off: `RIME-OVMF-PROBE: mokutil --sb-state: SecureBoot disabled Platform is in Setup Mode`, `Secure boot disabled`,
`DONE`.

Tampered MOK-signed kernel, SB on + MOK (the signature is intact, the hash is not):
```
verify.c:660:verify_buffer_authenticode() Binary is not authorized
SSL Error: verify.c:664 verify_buffer_authenticode(): Security Policy Violation
error: ../../grub-core/kern/efi/sb.c:shim_lock_verifier_write:192:bad shim signature.
```
No shim, MOK-signed kernel as `BOOTX64.EFI`, SB on + MOK (console, then `c-fw-sbon-mok-mokkernel.ovmf.log`):
```
BdsDxe: failed to load Boot0002 "UEFI Misc Device" from PciRoot(0x0)/Pci(0x2,0x0): Access Denied -- rejected probably by Secure Boot
DxeImageVerificationLib: Image is signed but signature is not allowed by DB and SHA256 hash of image is not found in DB/DBX.
The image doesn't pass verification: PciRoot(0x0)/Pci(0x2,0x0)/HD(1,GPT,…)/\EFI\BOOT\BOOTX64.EFI
```
The same disk with SB off: `BdsDxe: starting Boot0002 "UEFI Misc Device"`.

## Findings for the plan

1. **D3 v1 chain: proven in OVMF** for plain disks, both entry styles. The P0 OVMF gate is met.
2. **Fedora's GRUB `blscfg` reads a NixOS BLS entry as is** (literal `options`, no `$kernelopts`, no grubenv), with
   kernel and initrd paths relative to the ESP. The hook can write plain BLS files.
3. **bootupd's static config sources `$prefix/custom.cfg` (41_custom.cfg) and lets it set `default`** over ostree's
   BLS entries. A migrated machine needs no change to bootupd's files. katana's own `grub.cfg` is still unverified
   (needs root, D3).
4. **Shim 16.1 accepts a kernel with no `.sbat` section** through the shim_lock protocol (`No .sbat section data`,
   boot continues). The NixOS kernel needs no SBAT work for v1.
5. **The NixOS kernel logged no lockdown under Secure Boot** (no `lockdown` line in dmesg; the default
   `lsm=landlock,yama,bpf` does not list the lockdown LSM, and nixpkgs carries no "lock down when SB is on" patch like
   Fedora's). Consistent with D3's `LOCK_DOWN_FORCE_NONE`; worth a line in the release note's security section.
6. **Fedora's signed `grubx64.efi` has no `serial` terminal** (`terminal 'serial' isn't found`). GRUB output reaches
   a serial console only through the firmware console. P2 VM rigs should not rely on `console.cfg`.
7. **Only the kernel is verified.** shim_lock covers the file GRUB loads with `linux`; the initrd, the BLS entry /
   `rime-nixos.cfg` (so `init=` and every kernel parameter) and the root filesystem are not signed or measured here,
   and GRUB's menu editor (`e`) is open. Same posture as Fedora's own chain, so not a probe defect, but it means
   Secure Boot alone does not protect the NixOS system from someone who can write the ESP or `/boot`. Closing it is a
   later design question (UKI, signed initrd, GRUB password, TPM-sealed LUKS), not v1.
8. **Fedora 45 moved the EFI binaries** to `/usr/lib/efi/{shim/16.1-7,grub2/1:2.12-83.fc45}/EFI/fedora/` in the RPMs
   (no `/boot/efi` payload). The hook that updates shim and GRUB (D3 risk: SBAT revocations) must read them there.

## Not tested here (P2 gates)

LUKS; TPM and PCR 7; MokManager enrolment from `mokutil --import`; recovery through the GRUB menu from a bad
generation; the real `boot.loader.external` hook (the rig writes the entries itself, from bootspec); a root on btrfs;
the CachyOS kernel; real hardware. The ESP uses the removable path (`EFI/BOOT/BOOTX64.EFI` = shim, with `grubx64.efi`
beside it) instead of an NVRAM entry for `\EFI\fedora\shimx64.efi`; `fbx64.efi` is left out so shim does not run
fallback.

## Reproduce

From a rime-os checkout on this branch, on a host with rootful podman, `/dev/kvm` and the `rime-nix` container
(STATE.md, Infrastructure):
```sh
nix/probe/ovmf/run-all.sh                  # build NixOS bits + all 10 cases; exit 0 = every case PASS
SKIP_NIX=1 nix/probe/ovmf/run-all.sh       # reuse $ART/in
SKIP_NIX=1 nix/probe/ovmf/run-all.sh case a-bls-sbon-mok-signed   # one case (after a full run)
BOOT_TIMEOUT=300 REFUSE_TIMEOUT=120 …      # a loaded host: a timeout is reported FAIL (got=unknown), never PASS
```
(`run-all.sh` passes both into the lab container. Defaults: 300 s per boot, 60 s per refusal. At load average ~88
one SB-off boot took 146 s, so the old 150 s boot limit was too tight.)
`run-all.sh` refuses an `ART` that is not an absolute
directory three levels deep or that lies under a home directory, since it ends in a recursive `chown`.
- `build-nixos.sh`: `nix-build system.nix --arg nixpkgs 'builtins.fetchTarball "…/1c89083e….tar.gz"' -A kernel -A
  initrd -A toplevel -A rootfs -A entry` in `rime-nix`, then copies `bzImage`, `initrd`, `rootfs.img`, `entry.json`
  (the bootspec fields the hook needs) to `$ART/in`.
- `Containerfile`: `rime-ovmf-probe`, Fedora 45 with qemu, edk2-ovmf, virt-firmware, sbsigntools, mtools,
  btrfs-progs.
- `lab.sh` (inside `rime-ovmf-probe`): `rpms` (dnf download + `rpm -K` + extract) → `keys` (throwaway MOK) → `vars`
  (three varstores) → `sign` (`sbsign`) → `disks` (GPT images, no loop devices) → `cases`.

`ART` defaults to `/var/lab/rime-nix/artifacts/ovmf`. Keys stay there (`keys/`, never in the repo); a fresh key is
generated when `keys/` is empty. Run log of the recorded run: `/var/lab/rime-nix/artifacts/ovmf/run-all.log`.
