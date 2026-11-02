# Bridge OVMF lab: topologies A and E end to end (p2-gates D1, D2, X1)

This rig proves `rime migrate` (nix/pkgs/rime-bridge) on disks made from the **published** Rime OS bootc image, in
KVM guests only. It never touches the host's disks, ESP or NVRAM: every disk is a file, the NVRAM is a copy of an OVMF
varstore, every key press goes to the guest.

| File | What it does |
|---|---|
| `mkbootc.sh KIND` | Runs inside the published image (`podman run --privileged`, an empty tmpfs over `/sys/firmware`). Makes `disks/KIND/disk.raw` (and for E `win.raw`) |
| `system.nix` | The lab NixOS system the bridge installs: the product modules + `rime.rootfs` + `rime.boot`, nixpkgs' kernel. Its `rime-bridge-lab` unit was meant to print markers on ttyS0; serial-getty hangs that tty up under it and the unit fails (lab-only, harmless), so the driver checks the same things after a serial login. That failed unit is why the lab system reads `degraded`; it is not a product unit |
| `payload.sh TOPLEVEL BRIDGE` | Inside the rime-nix container: the payload disk (`payload/store.tar.zst`, `registration`, `manifest.json`, the bridge as it runs on bootc, `guest-prep.sh`) |
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
- layout = installer/rime-install's plain disk: GPT [BIOS boot][ESP 512M `EFI-SYSTEM`][btrfs `rime-root`], `/boot` a
  directory of the btrfs top level;
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
- **nixos**: GRUB's default is now the NixOS entry the boot hook wrote: wait for the serial getty, screenshot the
  greeter (VGA), log in on ttyS0 with the OLD password, then greetd active, the mounts, the carried `phase` (E:
  `failed`), the machine-id and the home checksums.
- **back**: hold Up in GRUB's menu to pick the first entry (the ostree deployment): bootc boots, the same login works,
  home is intact.

`--phases bootc,nixos,back,nixos` (what the evidence run used) ends with NixOS again: after the ostree round trip the
default is still the NixOS entry.

`--rollback-first` (phase bootc, before the real start) runs `start` once with a copy of rime-migrate that raises
right after the boot entry is written (`step("fsck gate after")` replaced by a fault), requires rc 10 and
"rolled back", then computes a verdict in the guest: no @nixos/@nix/@home/@pre-nixos, the old label, none of the
bridge's masks, no `/boot/rime` or `grub2/rime-nixos.cfg`, `custom.cfg` as before. Only `VERDICT=clean` passes.

## Secure Boot

OFF (`code-secboot.raw` + `vars-sb-off.fd`). The published kernel is signed by the real Rime MOK and the lab NixOS
kernel by nothing; an SB-on run needs both certificates in `MokList` (TODO(handover): enrol the Rime MOK certificate
from the image + a TEST MOK for the lab kernel, or the kernsign pipeline's signed kernel).

## Running it

```
# once: an OCI export of the image into $LAB/oci, then
sudo podman run --rm --privileged -e RIME_LAB_PASS=rime-lab-OLD-1 -v $LAB:/lab -v $RIG:/rig:ro \
  --entrypoint bash ghcr.io/andrenijman/rime-os@sha256:0ace1f6d… /rig/mkbootc.sh A
nix build --impure --expr '(import ./nix/probe/ovmf/bridge/system.nix { flake = builtins.getFlake "path:/work/wt-bridge"; })'
nix shell nixpkgs#{zstd,e2fsprogs,python3,gnutar,coreutils,gnused,bash} -c bash nix/probe/ovmf/bridge/payload.sh TOPLEVEL BRIDGE
nix/probe/ovmf/bridge/run.sh A --fresh --rollback-first --phases bootc,nixos,back,nixos
# after rebuilding rime-bridge only: REFRESH_ONLY=1 ... payload.sh x BRIDGE swaps the bridge on the payload disk
```

Evidence: `/var/lab/rime-nix/artifacts/bridge/` (`run-KIND.log` + `KIND/` per command; `run1/` = the first
passing run, on the bridge before the H8 refactor). Verifier re-run with `--rollback-first`:
`/var/lab/rime-nix/artifacts/verify-bridge/` (`run-KIND.log` + `ovmf/KIND/`).
