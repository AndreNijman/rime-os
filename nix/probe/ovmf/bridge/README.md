# Bridge OVMF lab: topologies A (plain and encrypted) and E end to end (p2-gates D1, D2, X1)

This rig proves `rime migrate` (nix/pkgs/rime-bridge) on disks made from the **published** Rime OS bootc image, in
KVM guests only. It never touches the host's disks, ESP or NVRAM: every disk is a file, the NVRAM is a copy of an OVMF
varstore, every key press goes to the guest.

| File | What it does |
|---|---|
| `prepare.sh STEP…` | Makes every input below on the host, through containers only (steps `keys vars kernel toplevel payload image oci disk-A disk-L disk-E`; its header says what each does) |
| `oci-layer.py OCI ROOT NOTE` | Appends one root-owned layer (the files under ROOT) to an OCI layout in place: how `prepare.sh oci` makes the **readiness image** (below) |
| `mkbootc.sh KIND` | Runs inside the published image (`podman run --privileged`, an empty tmpfs over `/sys/firmware`). Makes `disks/KIND/disk.raw` (and for E `win.raw`) from the OCI layout in `$LAB/oci`; KIND A, L or E |
| `system.nix` | The lab NixOS system the bridge installs: the product modules + `rime.rootfs` + `rime.boot` + the **Rime kernel** (NVIDIA off), signed in two passes (`imageJson`, then `toplevel` with `signatureDir`/`certificate`, `require = true`) by the lab's TEST MOK. `releaseKernel = true` (`RIME_LAB_KERNEL=release`, topology-e): NVIDIA on, so the kernel image hashes the same as `nixosConfigurations.rime`'s and its committed release signature (`nix/pkgs/kernel/signing/signatures/`) is used, no TEST MOK anywhere. Lab-only knobs: serial console, Plymouth off (the passphrase prompt on the console), pixman/software Qt for the VGA greeter, an initrd unit that says `RIME-LAB: PASSPHRASE-ASKED` on ttyS0 when systemd-cryptsetup asks, Flathub setup off (no network) |
| `lab-bridge.nix` | The shipped `rime-bridge` with ONE line changed (`TRUSTED_PAYLOAD_KEYS` = the lab's TEST payload key), for a run WITHOUT `--readiness` on a disk of the published image alone (no `nixos-bridge-payload.pub` there) |
| `payload.sh TOPLEVEL BRIDGE SECRET` | Inside the rime-nix container: the payload disk (`payload/store.tar.zst`, `registration`, `preview.tar`, `manifest.json` + `manifest.json.sig` signed with the product's `payload-sign.sh` and the TEST key, the bridge as it runs on bootc, `guest-prep.sh`) |
| `guest-prep.sh KIND` | Inside the guest (refuses outside KVM): the firmware entries a real machine of KIND has (E: katana's `APEX-OS Primary` first, `Fedora`, the trial's `Rime OS` systemd-boot entry not in BootOrder; katana's is Boot0006, here the first free number) |
| `verb-probe.sh LABEL allowed\|refused` | Inside the guest (refuses outside KVM; shipped on the payload disk): every bootc verb the readiness `rime` holds (11 forms) and `rime-update-check`, run against the guest's real start record with bootc, ostree, rpm-ostree, skopeo, cosign, flatpak, fwupdmgr, dnf, systemd-sysext, rime-boot-migrate and rime-pkg replaced by logging stubs in a private mount namespace (so an allowed verb changes nothing). Prints one line per verb and `PROBE-VERDICT=ok\|bad` |
| `refusals-mutant.nix` | `bridge-refusals` with a rime-migrate whose btrfs mounts are read-write: it MUST fail (proves the before/after sha256 can catch a write) |
| `drive-bridge.py KIND` | Inside `localhost/rime-ovmf-probe`: the three phases below, logs in `$ART/KIND`. `--readiness`: the disk is the readiness image, so the guest's own `rime migrate` and `/usr/libexec/rime-migrate` run (not the payload disk's copy) and the verb probes (`verb-probe.sh`) run; `--kill-probe`, `--interrupt-probe`: the unfinished and interrupted starts (below) |
| `run.sh KIND [--fresh] [--phases …]` | Starts `drive-bridge.py` in the probe container |

## The readiness image (bridge3; topology-e)

The readiness release (draft rime-os #137, branch `feat/nixos-readiness`) is not built or published yet, so the lab
makes its bootc side itself: the published image (`BASE`: bridge3 used 2026.11.01.2; topology-e #137's merge base
2026.11.03.5, `ghcr.io/andrenijman/rime-os@sha256:27a9b190…`) plus ONE layer with exactly what #137
changes in the image: `/usr/bin/rime` built from that branch's `rimed/` inside the published image (as
Containerfile.base's rime-builder stage does: rust + cargo, `cargo build --release --locked --bin rime`, in a
container that is thrown away), its `files/system/libexec/rime-migrate` as `/usr/libexec/rime-migrate`, its
`rime-update-check` (since topology-e: bridge3's layer left the base image's older one in place), and the TEST
payload key's public line as `/usr/share/rime-os/trust/nixos-bridge-payload.pub` (where the release will put
`rime-bridge-payload-1`'s). The branch is read with `git archive` only. `$ART/readiness-image.txt` records the branch
commit, the sha256 of rime-migrate minus its first line next to `rime-bridge.py`'s (they must match), every file's
sha256 and the new manifest digest; the guest prints the same facts back (`KIND-readiness.txt`). `prepare.sh image`
refuses when the two differ (`RIME_LAB_ALLOW_BRIDGE_DRIFT=1` overrides) and runs Containerfile.base's rime-migrate
checks in the base image.

Why an OCI layer and not `podman build FROM` the published image: on katana `podman build`'s COPY walked the image's
whole 129-layer rootfs (the ostree repo included) through fuse-overlayfs for more than 20 minutes while holding
podman's storage lock, which stalled every other podman user on the host. `podman save --format oci-dir` and
`oci-layer.py` need no lock beyond the save.

With `--readiness`, phase bootc runs `verb-probe.sh` (`KIND-probe-LABEL.txt`; every probe must say
`PROBE-VERDICT=ok`): `no-record` allowed before anything; with `--rollback-first` `rolled-back` allowed right after the
start that undid itself, then power off, boot ostree again and `rolled-back-rebooted` allowed; after the real start
`done` refused; phase back `done-back-on-ostree` refused. `--kill-probe` (on qcow2 overlays, the main disk's qemu off):
a copy of rime-migrate SIGKILLs itself at `fsck gate before` (after it masked the old side, so nothing rolls back),
`killed-start` refused, reboot, `killed-start-rebooted` refused, and a second `start` exits 3 with
`REFUSED [start-unfinished]` and the subvolume list unchanged. `--interrupt-probe` (overlays): Ctrl-C on the console
15 s into `install closure`: once with the image's rime-migrate, once with the payload disk's (this branch's), then
`interrupted-start*` allowed and what each bridge's `check` says about the record (`KIND-int-*.txt`).

## How the disks are made, and how they differ from a real install

`bootc install to-disk` was not used: it runs bootupd, and a `--privileged` container that can see the host's
efivarfs is one flag away from writing katana's NVRAM. Instead `mkbootc.sh` does the steps `bootc install` does, from
the same image, with `/sys/firmware` replaced by an empty tmpfs first (checked before anything else runs):

- `ostree admin init-fs` + `ostree container image deploy` of an OCI export of the published image
  `ghcr.io/andrenijman/rime-os@sha256:0ace1f6dad51597d79a7d0d84135dd2896fc89d748ec74b9ecfc7c98c92df9bf`
  (release 2026.11.01.2; since bridge3 with the readiness layer above) with bootc's kernel arguments;
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
cd nix/probe/ovmf/bridge            # LAB (scratch) and ART (evidence) default to .../scratch/bridge3 and .../artifacts/bridge3
./prepare.sh keys vars kernel toplevel payload image oci
sudo modprobe dm_crypt              # on the host, only while disk-L runs (cryptsetup on a loop device over the FILE)
./prepare.sh disk-A disk-L
sudo modprobe -r dm_crypt
./run.sh A --fresh --sb --readiness --rollback-first --phases bootc,nixos,back,nixos
./run.sh L --fresh --sb --readiness --rollback-first --phases bootc,nixos,back,nixos
# E (topology-e: LAB=.../scratch/topo-e ART=.../artifacts/topo-e RIME_LAB_KERNEL=release BASE=<#137's base>):
# ./prepare.sh keys vars toplevel image payload oci disk-E disk-A
# ./run.sh E --fresh --sb --readiness --rollback-first --greeter-first --phases bootc,nixos,back,nixos
# ./run.sh A --fresh --sb --readiness --kill-probe --interrupt-probe --phases bootc
# the first NixOS login straight through the greeter (no serial login first), then a negative control on the
# same guest (the update-window knob cut from the user's copy, firstrun masked: the window must show):
# ./run.sh A --fresh --sb --readiness --greeter-first --negative-control --phases bootc,nixos
# (--negative-control edits the guest user's config and masks firstrun: only on a --fresh throwaway disk)
# without --readiness (the published image alone): build lab-bridge.nix and pass it to payload.sh as BRIDGE
```

Evidence: `/var/lab/rime-nix/artifacts/topo-e/` (Secure Boot ON, release-signed NixOS kernel, readiness image at
#137 1a4070580: E end to end with greeter-first and every verb probe, A with the kill and interrupt probes;
`run-KIND.log`, `KIND/`, `kernel-release.txt`, `readiness-image.txt`, `prepare-*.log`).
`/var/lab/rime-nix/artifacts/bridge3/` (Secure Boot ON, readiness image, A plain and L: `run-KIND.log` +
`KIND/` per command and screenshot, `mkbootc-KIND.log`, `prepare-*.log`, `readiness-image.txt`,
`readiness-rime-build.log`, `payload.log`). Earlier, published image + lab bridge, Secure Boot ON, L and E:
`/var/lab/rime-nix/artifacts/bridge2/`. Secure Boot off, A and E: `/var/lab/rime-nix/artifacts/bridge/` (`run1/` = the
first passing run, on the bridge before the H8 refactor). Verifier re-run with `--rollback-first`:
`/var/lab/rime-nix/artifacts/verify-bridge/` (`run-KIND.log` + `ovmf/KIND/`).
