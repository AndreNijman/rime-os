# P2 exit checklist

Every item of PLAN.md §6.1's **P2 exit gate** cell, one line each, written on 2026-11-01 (P2 started the same day; the
K2 clock puts the P2 exit at 2027-01-24 plus one 2-week extension). The cell's compound items are split: the boot
matrix into its 2 × 2 × 2 cells, the keymap gate per layout, the host-layer gate into D2's VM checks, "Topologies E and
A … every other topology is refused by name" per topology, "G2 delta size measured" from D6, and "D5 gates" from D5.

Columns:

- **How proven**: the evidence that will close the line: a `checks.x86_64-linux.<attr>` (a Nix VM or build check), a
  log from a named rig (OVMF lab, iso-qualify), or a screenshot. A line closes only on that evidence, seen.
- **Evidence so far**: partial proof that exists today, with where it is. Partial is not met.
- **Status**: every line is **not met yet**. Change a line only with the evidence named in its row.

P4 gates (8 weekly updates, live shell-only update, 30-day bytes, wall-clock) are not P2 and are not listed.

## Kernel

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| K1 | The Rime kernel has BTF, with the bootc tier's gate (pahole, required `=y` options, no BTF-mismatch escape hatch, committed config, kfunc BTF read by two readers) | `kernel-gate` | `kernel-vm-signed` (nix/bootmatrix): the VM boots the image signed by `sign-detached.sh` (TEST key, `require = true`, sbverify) and `/sys/kernel/btf/vmlinux` is byte for byte `kernel-gate`'s `vmlinux.btf`; `kernel-gate` builds green. Log `/var/lab/rime-nix/artifacts/bootmatrix/logs/kernel-vm-signed.log` | **met** 2026-11-02 (nix/bootmatrix) |
| K2 | The scx gate: all 16 shipped sched-ext schedulers attach, survive load and detach on the shipped kernel | `kernel-vm` (bootc's own `tests/kernel/scx-selftest` + `run-scx-selftest.sh --parse-only`) | `kernel-vm-signed`: attach window 60 s (`nix/tests/kernel.nix`); three consecutive `--rebuild` passes, 17/17 VERDICT PASS each, host load average 5→16, 16→25, 25→34 (`scx_eevdf` attach 3.8 / 6.2 / 11.4 s: the third fails the old 10 s window). Logs `/var/lab/rime-nix/artifacts/bootmatrix/logs/k2-kernel-vm-signed.pass{1,2,3}.log` | **met** 2026-11-02 (nix/bootmatrix) |
| K3 | A `kill -9` of the running scheduler hands every task back to the kernel's class | `kernel-vm` | `kernel-vm-signed`, the same three passes: the `kill -9` case passes in each | **met** 2026-11-02 (nix/bootmatrix) |

## Boot chain: OVMF, shim → Fedora GRUB → MOK-signed kernel (D3)

The probe evidence below is `nix/probe/ovmf/` (feat/nixos, from nix/ovmf-sb-probe 40cc38b95; logs
`/var/lab/rime-nix/artifacts/ovmf/logs/`). It used nixpkgs' Linux 7.3, a throwaway TEST MOK, a hand-built disk with
bootupd's static GRUB config, pre-seeded `MokList`, no LUKS, and no Rime boot hook. A line closes only when the **Rime
kernel, signed by the real pipeline's key path** (kernsign) boots from an entry **written by Rime's boot hook**
(boothook) on a system built from the shipped modules.

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| B1 | plain, SB on, BLS entry (fresh install) | OVMF lab log: kernel reports `SecureBoot enabled`, reaches the greeter | OVMF matrix run `bls-plain-sb-mok` (`nix/probe/ovmf/boothook/run.sh matrix-run bls-plain-sb-mok`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot enabled, TEST MOK in the kernel keyring, greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/bls-plain-sb-mok/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B2 | plain, SB on, `custom.cfg` entry (migrated) | OVMF lab log | OVMF matrix run `custom-plain-sb-mok` (`nix/probe/ovmf/boothook/run.sh matrix-run custom-plain-sb-mok`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot enabled, TEST MOK in the kernel keyring, greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/custom-plain-sb-mok/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B3 | plain, SB off, BLS | OVMF lab log | OVMF matrix run `bls-plain-sb-off` (`nix/probe/ovmf/boothook/run.sh matrix-run bls-plain-sb-off`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot disabled, greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/bls-plain-sb-off/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B4 | plain, SB off, `custom.cfg` | OVMF lab log | OVMF matrix run `custom-plain-sb-off` (`nix/probe/ovmf/boothook/run.sh matrix-run custom-plain-sb-off`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot disabled, greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/custom-plain-sb-off/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B5 | LUKS, SB on, BLS | OVMF lab log: passphrase prompt, unlock, greeter | OVMF matrix run `bls-luks-sb-mok` (`nix/probe/ovmf/boothook/run.sh matrix-run bls-luks-sb-mok`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot enabled, TEST MOK in the kernel keyring, Plymouth prompt `boot1-passphrase-late.png`, passphrase typed over QMP, `/` on `/dev/mapper/luks-<uuid>` (LUKS2, `rd.luks.uuid=`), greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/bls-luks-sb-mok/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B6 | LUKS, SB on, `custom.cfg` | OVMF lab log | OVMF matrix run `custom-luks-sb-mok` (`nix/probe/ovmf/boothook/run.sh matrix-run custom-luks-sb-mok`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot enabled, TEST MOK in the kernel keyring, Plymouth prompt `boot1-passphrase-late.png`, passphrase typed over QMP, `/` on `/dev/mapper/luks-<uuid>` (LUKS2, `rd.luks.uuid=`), greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/custom-luks-sb-mok/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B7 | LUKS, SB off, BLS | OVMF lab log | OVMF matrix run `bls-luks-sb-off` (`nix/probe/ovmf/boothook/run.sh matrix-run bls-luks-sb-off`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot disabled, Plymouth prompt `boot1-passphrase-late.png`, passphrase typed over QMP, `/` on `/dev/mapper/luks-<uuid>` (LUKS2, `rd.luks.uuid=`), greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/bls-luks-sb-off/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B8 | LUKS, SB off, `custom.cfg` | OVMF lab log | OVMF matrix run `custom-luks-sb-off` (`nix/probe/ovmf/boothook/run.sh matrix-run custom-luks-sb-off`): shipped profile, signed Rime 7.2.9 through Fedora 45 shim+GRUB, SecureBoot disabled, Plymouth prompt `boot1-passphrase-late.png`, passphrase typed over QMP, `/` on `/dev/mapper/luks-<uuid>` (LUKS2, `rd.luks.uuid=`), greeter `boot1-greeter.png`. `/var/lab/rime-nix/artifacts/bootmatrix/custom-luks-sb-off/`, `results.tsv` | **met** 2026-11-02 (nix/bootmatrix) |
| B9 | A user recovers from a bad generation through the GRUB menu, plain | OVMF lab log + screenshot of the GRUB menu entry picked; the previous generation reaches the greeter | the 4 plain runs (B1-B4): bad generation 2 installed by `switch-to-configuration boot` becomes the default and fails (initrd emergency); the GRUB selection is moved to generation 1 (`boot3-menu.png` → `boot3-picked.png`), which reaches the greeter (`boot3-greeter.png`). `/var/lab/rime-nix/artifacts/bootmatrix/*-plain-*/` | **met** 2026-11-02 (nix/bootmatrix) |
| B10 | The same, LUKS (D3 lists plain and LUKS for P2) | OVMF lab log + screenshot | the same in the 4 LUKS runs (B5-B8), passphrase typed again on the recovery boot. `/var/lab/rime-nix/artifacts/bootmatrix/*-luks-*/` | **met** 2026-11-02 (nix/bootmatrix) |

## TPM gates in OVMF (LUKS + SB on, D3)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| T1 | PCR 7 is recorded before the move | OVMF lab log (`tpm2_pcrread sha256:7`, swtpm) | none | not met yet |
| T2 | PCR 7 is recorded after the move, and the difference is explained | OVMF lab log | none | not met yet |
| T3 | The TPM-only re-enrolment keeps the pre-existing recovery key working: after re-enrolment, the volume unlocks **with the recovery key** | OVMF lab log of the recovery-key unlock | none (`rime-luks-enroll` is shipped in rime-libexec, not wired: rime-libexec.nix:38) | not met yet |

## Keymap gate (D2)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| M1 | A LUKS volume with the **gb** keymap unlocks with a passphrase holding a layout-dependent character | Nix VM test (systemd initrd, `console.keyMap = "uk"`, the character typed as keycodes) | none | not met yet |
| M2 | The same with the **ch** keymap | Nix VM test | none | not met yet |

## Host-layer gate (D2) on a VM made from a copy of katana's state

Input: the sanitised snapshot `/var/lab/rime-nix/artifacts/katana-state/` (0700, never committed).

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| H1 | Log in with the password from before the migration | Nix VM test or OVMF lab log on the copied state | none (hostlayer in progress) | not met yet |
| H2 | A saved Wi-Fi profile comes up | VM test (NetworkManager keyfile carried; `nmcli` shows it activated on a simulated AP or its keyfile accepted) | none | not met yet |
| H3 | A saved VPN profile comes up | VM test | none | not met yet |
| H4 | The SSH host key fingerprint is unchanged | VM test: `ssh-keygen -lf` before = after | none | not met yet |
| H5 | `machine-id` is unchanged | VM test | none | not met yet |
| H6 | Both fstab mounts come up | VM test | none | not met yet |
| H7 | Boot still completes with one of those disks absent | VM test (second disk removed) | none | not met yet |
| H8 | fsck containment gate (D2) | VM test | none | not met yet |
| H9 | Free-space gate on the disk figure: closure + one mass rebuild (~10.5 GB) + what the old copies can pin (D2) | `rime migrate check` VM test: refuses below the figure, by name | none | not met yet |
| H10 | The `/etc` carry list, including katana's host-local files (`60-bfq-nvme.rules`, `99-dualsense-touchpad.rules`, `90-desktop-writeback.conf`, docs/nixos/fedora-defaults.md) | VM test: each file present or listed in the "left behind" report | none | not met yet |

## Activation

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| A1 | greetd is not restarted by `switch-to-configuration switch` | Nix VM test: greetd's `MainPID` and the user's session survive a switch to a changed generation | none | not met yet |

## Installer (D9)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| I1 | The installer passes `iso-qualify.py` in OVMF with SB on | iso-qualify log | the bootc installer's CI boots passed for v3.2.0 (build-installer-iso 37749860175); the NixOS swap does not exist yet (wave 2) | not met yet |
| I2 | The plain boot driver (`plain-boot-drive.py`) passes on the installed NixOS system, SB on | driver log | none | not met yet |
| I3 | The LUKS boot driver (`luks-boot-drive.py`) passes, SB on | driver log | none | not met yet |

## Bridge (D1): topologies

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| D1 | Topology **E** (a failed boot migration's leftovers; katana) migrates in a VM | OVMF lab log on a VM built to topology E | none (wave 2) | not met yet |
| D2 | Topology **A** (installer default) migrates in a VM | OVMF lab log | none | not met yet |
| D3 | Topology **B** is refused by name | `rime migrate check` VM test: names B, writes nothing | none | not met yet |
| D4 | Topology **C** is refused by name | same | none | not met yet |
| D5 | Topology **D** is refused by name | same | none | not met yet |
| D6 | Topology **F** is refused by name | same | none | not met yet |

## G2 delta transport (D6)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| G1 | The size of a `zstd --patch-from` delta against the previous release's paths is measured, for a non-mass-rebuild release pair | measurement script + raw data committed (`probe/tools/delta.py` over closure TSVs, or the real narinfo `fileSize` set) | none: PLAN §3.5 and §7.2 Q4 record it as not measured | not met yet (owner: unowned, `size` or `release`) |
| G2 | The same for a mass-rebuild pair (glibc/stdenv), the case A3 batches | same | none | not met yet |
| G3 | The client's import step (fetch delta, reconstruct the NAR, check narHash, import) has a measured cost | client VM test timing | none | not met yet (client) |

## D5 gates (packages)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| P1 | The translator reports every requested package on a copy of katana's state | translator test on the katana-state copy: no package unreported | none (`nix/modules/packages-catalogue.nix` is a skeleton) | not met yet |
| P2 | Steam launches a game in Gaming Mode in a VM with a GPU stub | Nix VM test + screenshot | `gaming` check: the Gaming Mode session, gamescope and steam resolve, the switch works; no game launch | not met yet |
| P3 | claude-code runs from `~/.local/bin` under nix-ld | Nix VM test | none (no nix-ld config yet) | not met yet |
| P4 | Equibop runs (needed for Discord presence) | Nix VM test | none | not met yet |

The P3 hardware half of P2 ("and later on katana") belongs to the P3 gate, not here.

## Also required by the decisions for P2 (not in the §6.1 cell)

Listed so they are not lost; they close the same way.

| # | Gate | Source | How proven | Status |
|---|---|---|---|---|
| X1 | The topology E repair runs | D3 P2 gates | OVMF lab log | not met yet |
| X2 | The `test-boot-v2.sh` scan finds no `bootctl`, `grub-install` or `efibootmgr` in the activation script or the boot hook | D3 | `boot-loader-scan`: `rime-vm` and `rime-vm-luks`, 14 files each (hook, activate, dry-activate, switch-to-configuration, INSTALL_BOOTLOADER, depth 4), 0 findings; negative controls flagged (NixOS GRUB `install-grub.pl` grub-install; activation `bootctl` line 99). Log `/var/lab/rime-nix/artifacts/bootmatrix/logs/checks-nonvm.log` | **met** 2026-11-02 (nix/bootmatrix) |
| X3 | A machine that never took the release carrying `RENAMED_SIGNER` is refused by name | D1 | `rime migrate check` VM test | not met yet |
| X4 | An end-to-end update in a VM refuses a tampered narHash, an unsigned manifest and a wrong identity | D6 | `update-e2e` (release + client) | not met yet |
| X5 | Rollback works after a refused update | D6 | `update-e2e` | not met yet |
