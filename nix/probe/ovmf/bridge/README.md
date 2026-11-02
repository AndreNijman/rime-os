# Bridge OVMF lab: topologies A (plain and encrypted) and E end to end (p2-gates D1, D2, X1)

This rig proves `rime migrate` (nix/pkgs/rime-bridge) on disks made from the **published** Rime OS bootc image, in
KVM guests only. It never touches the host's disks, ESP or NVRAM: every disk is a file, the NVRAM is a copy of an OVMF
varstore, every key press goes to the guest.

| File | What it does |
|---|---|
| `mkbootc.sh KIND` | Runs inside the published image (`podman run --privileged`, an empty tmpfs over `/sys/firmware`). Makes `disks/KIND/disk.raw` (and for E `win.raw`); KIND A, L or E |
| `system.nix` | The lab NixOS system the bridge installs: the product modules + `rime.rootfs` + `rime.boot` + the **Rime kernel** (NVIDIA off), signed in two passes (`imageJson`, then `toplevel` with `signatureDir`/`certificate`, `require = true`) by the lab's TEST MOK. Lab-only knobs: serial console, Plymouth off (the passphrase prompt on the console), pixman/software Qt for the VGA greeter, an initrd unit that says `RIME-LAB: PASSPHRASE-ASKED` on ttyS0 when systemd-cryptsetup asks, Flathub setup off (no network) |
| `lab-bridge.nix` | The shipped `rime-bridge` with ONE line changed (`TRUSTED_PAYLOAD_KEYS` = the lab's TEST payload key), because the published image has no `nixos-bridge-payload.pub` yet |
| `payload.sh TOPLEVEL BRIDGE SECRET` | Inside the rime-nix container: the payload disk (`payload/store.tar.zst`, `registration`, `preview.tar`, `manifest.json` + `manifest.json.sig` signed with the product's `payload-sign.sh` and the TEST key, the bridge as it runs on bootc, `guest-prep.sh`) |
| `guest-prep.sh KIND` | Inside the guest (refuses outside KVM): the firmware entries a real machine of KIND has (E: katana's `APEX-OS Primary` first, `Fedora`, the trial's `Rime OS` systemd-boot entry not in BootOrder; katana's is Boot0006, here the first free number) |
| `refusals-mutant.nix` | `bridge-refusals` with a rime-migrate whose btrfs mounts are read-write: it MUST fail (proves the before/after sha256 can catch a write) |
| `drive-bridge.py KIND` | Inside `localhost/rime-ovmf-probe`: the three phases below, logs in `$ART/KIND` |
| `run.sh KIND [--fresh] [--phases …]` | Starts `drive-bridge.py` in the probe container |

## How the disks are made, and how they differ from a real install

`bootc install to-disk` was not used: it runs bootupd, and a `--privileged` container that can see the host's
efivarfs is one flag away from writing katana's NVRAM. Instead `mkbootc.sh` does the steps `bootc install` does, from
the same image, with `/sys/firmware` replaced by an empty tmpfs first (checked before anything else runs):

- `ostree admin init-fs` + `ostree container image deploy` of an OCI export of the published image
  `ghcr.io/andrenijman/rime-os@sha256:0ace1f6dad51597d79a7d0d84135dd2896fc89d748ec74b9ecfc7c98c92df9bf`
  (release 2026.11.01.2) with bootc's kernel arguments;
- the ESP gets the image's own shim, GRUB, MokManager and bootupd's static `grub.cfg` + `bootuuid.cfg` in
  `\EFI\fedora\` and `\EFI\BOOT\`; `/boot/grub2/grub.cfg` is bootupd's static config (pre + `configs.d`, which sources
  `$prefix/custom.cfg` from `41_custom.cfg`);
- layout A = installer/rime-install's plain disk: GPT [BIOS boot][ESP 512M `EFI-SYSTEM`][btrfs `rime-root`], `/boot` a
  directory of the btrfs top level;
- layout **L** = installer/rime-install's **default, encrypted** layout: GPT 1 ESP 1G `RIMEESP`, 2 ext4 1G `rime-boot`
  (= `/boot`, GRUB's `grub2/` and ostree's `loader` + kernels), 4 BIOS boot 1M, 3 LUKS2 `RIMEROOT` (argon2id, weakened
  exactly as the installer's own test builds weaken it: 32 MiB, 4 iterations) holding btrfs `rime-root`; kernel
  arguments `rd.luks.uuid=` + `rd.luks.name=` (the installer's `LUKS_KARGS`), `/etc/crypttab`, the `/boot` line in
  `/etc/fstab`; bootuuid.cfg names the ext4. ostree's own `boot -> .` link in the ext4 lets bootprefix paths resolve
  (mkbootc checks it is there). The passphrase is a TEST one (`RIME_LAB_LUKS`); `cryptsetup luksFormat/open` run on a
  loop device over the disk FILE inside the image's container (the host needs `dm_crypt` loaded);
- lab instrumentation (not on a real machine): the account `alex` with a TEST password and a seeded home with a
  checksum list, and `serial-getty@ttyS0` unmasked (the image masks it; the lab drives the guest over ttyS0).

**E** is the A disk labelled `apex-root` plus the failed boot-v2 trial's leftovers as rime-boot-migrate leaves them
(`\EFI\systemd\`, `\EFI\Linux\`, `\loader\` on Rime's ESP, `/composefs` and `/state` in the top level,
`/var/lib/rime/boot-migrate/` with `phase=failed`, `/var/lib/apex -> rime`) and a second "Windows" disk whose ESP holds
`\EFI\Microsoft\` and the legacy `\EFI\APEX\` shim + GRUB that boots the machine (katana's BootCurrent 0000).

## Phases (`drive-bridge.py`)

- **bootc**: log in on ttyS0 with the old password, `guest-prep.sh`, power off and boot again through the machine's own
  entry (A: `Fedora`; E: `APEX-OS Primary`), [E: `check` (topology E), `repair-e`, `repair-e --confirm`], format the
  backup disk, `check --explain --backup`, `start --backup --payload --yes` (3 min 13 s in the evidence run; 16-17 min when
  it shared the host with a second guest and a NixOS VM test: backup, both fsck gates with a scrub, the 4 GB closure,
  `nix-store --verify --check-contents`), record the subvolumes, `custom.cfg` and the state.
- **nixos**: GRUB's default is now the NixOS entry the boot hook wrote: [L: the NixOS initrd asks, the passphrase is
  typed] wait for the serial getty, screenshot the greeter (VGA), log in on ttyS0 with the OLD password, then greetd
  active, the mounts (L: `/` in the opened LUKS volume, `/boot` the ext4), the carried `phase` (E: `failed`), the
  machine-id, Secure Boot state and the home checksums. Then the **graphical login**: the old password is typed on the
  emulated keyboard into rime-greet (the user is prefilled from the carried `/var/lib/rime-greet/last-user`); the
  driver requires a `Hyprland` process for alex and a greetd `wayland` session, and screenshots the desktop
  (`KIND-nixos-desktop.png`).
- **back**: hold Up in GRUB's menu to pick the first entry (the ostree deployment): [L: passphrase] bootc boots, the
  same login works, home is intact.

Before `start`, phase bootc runs `check --explain --backup --payload`: the left-behind report (the payload's own
`carry --plan`) is printed before anything is written, and the run fails without it. L disks: every boot waits for
the passphrase prompt (dracut's on the console, or the NixOS initrd's marker), screenshots it and types the TEST
passphrase through QMP `send-key`.

`--phases bootc,nixos,back,nixos` (what the evidence run used) ends with NixOS again: after the ostree round trip the
default is still the NixOS entry.

`--rollback-first` (phase bootc, before the real start) runs `start` once with a copy of rime-migrate that raises
right after the boot entry is written (`step("fsck gate after")` replaced by a fault), requires rc 10 and
"rolled back", then computes a verdict in the guest: no @nixos/@nix/@home/@pre-nixos, the old label, none of the
bridge's masks, no `/boot/rime` or `grub2/rime-nixos.cfg`, `custom.cfg` as before. Only `VERDICT=clean` passes.

## Secure Boot

`--sb`: Fedora's Secure Boot OVMF (`code-secboot.raw`) with `fw/vars-sb-bridge.fd`: Fedora's enrolled varstore
(`vars-secboot.raw`, Microsoft + Red Hat keys, Secure Boot ON) plus a pre-seeded `MokList` (`virt-fw-vars --add-mok`,
the state MokManager leaves after an enrolment) with two certificates:

- the **real Rime MOK certificate** (`CN=APEX-OS Secure Boot key (andre)`, sha256 `2F:DD:2F:55:…:BC:BB`, public; the
  published image's kernel verifies against it: `sbverify --cert` OK), so the bootc side boots exactly as a real
  enrolled machine does;
- a **TEST MOK** made under the scratch dir (`CN=Rime bridge lab TEST MOK (throwaway, DO NOT TRUST)`), which signs the
  lab's Rime kernel (`nix/pkgs/kernel/sign-detached.sh`, as the installer lab does). A release kernel carries the
  Rime MOK's signature instead; the chain (shim → GRUB → MOK-signed kernel) is the same.

Every boot records `mokutil --sb-state`, the `SecureBoot` efivar and the kernel's `Loaded X.509 cert` lines; with
`--sb` the driver fails unless the guest itself reports Secure Boot on. Without `--sb` the key-less varstore
(Secure Boot off) is used.

## Running it

```
# once: an OCI export of the image into $LAB/oci; keys in $LAB/keys (all TEST, never committed):
#   test-mok.{key,crt,der,guid}  openssl req -x509 -newkey rsa:2048 -subj "/CN=Rime bridge lab TEST MOK (throwaway, DO NOT TRUST)/"
#   rime-mok.pem                 the Rime MOK certificate (public) out of the published image
#   payload-TEST.{sec,pub}       nix key generate-secret --key-name rime-bridge-lab-TEST-1 (in rime-nix)
# fw/vars-sb-bridge.fd: virt-fw-vars --input vars-secboot.raw --add-mok GUID rime-mok.der --add-mok GUID test-mok.der
sudo podman run --rm --privileged -e RIME_LAB_PASS=rime-lab-OLD-1 -e RIME_LAB_LUKS=rimelabluks1 -e DM_DISABLE_UDEV=1 \
  -v $LAB:/lab -v $RIG:/rig:ro --entrypoint bash ghcr.io/andrenijman/rime-os@sha256:0ace1f6d… /rig/mkbootc.sh L
# kernel: eval system.nix's imageJson, build the image, sign-detached.sh with the TEST MOK into $LAB/sig, then
nix build --impure --expr '(import ./nix/probe/ovmf/bridge/system.nix { flake = builtins.getFlake "path:/work/wt-bridge2";
  signatureDir = /work/scratch/bridge2/sig; certificate = /work/scratch/bridge2/keys/test-mok.crt; }).toplevel'
nix build --impure --expr '(import ./nix/probe/ovmf/bridge/lab-bridge.nix { flake = …; publicKey = "<payload-TEST.pub>"; })'
nix shell nixpkgs#{zstd,e2fsprogs,python3,gnutar,coreutils,gnused,bash,openssl,findutils,diffutils} \
  -c bash nix/probe/ovmf/bridge/payload.sh TOPLEVEL BRIDGE /work/scratch/bridge2/keys/payload-TEST.sec
nix/probe/ovmf/bridge/run.sh L --fresh --sb --phases bootc,nixos,back,nixos
nix/probe/ovmf/bridge/run.sh E --fresh --sb --phases bootc,nixos,back,nixos
# after rebuilding rime-bridge only: REFRESH_ONLY=1 ... payload.sh x BRIDGE x swaps the bridge on the payload disk
```

Evidence: `/var/lab/rime-nix/artifacts/bridge2/` (Secure Boot ON, L and E: `run-KIND-bootc.log` (phase bootc) and
`run-KIND.log` (nixos, back, nixos; re-run on the same disks after a driver fix) + `KIND/` per command and
screenshot, `mkbootc-KIND.log`, `payload.log`, `sign.txt`, `keys-and-varstore.txt`). Earlier, Secure Boot off, A and E:
`/var/lab/rime-nix/artifacts/bridge/` (`run1/` = the first passing run, on the bridge before the H8 refactor). Verifier re-run with `--rollback-first`:
`/var/lab/rime-nix/artifacts/verify-bridge/` (`run-KIND.log` + `ovmf/KIND/`).
