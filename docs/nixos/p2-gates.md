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
| K1 | The Rime kernel has BTF, with the bootc tier's gate (pahole, required `=y` options, no BTF-mismatch escape hatch, committed config, kfunc BTF read by two readers) | `kernel-gate` | `kernel-gate` builds green on feat/nixos 8b492a5e8 (foundation, `nix flake check` 21 checks) | not met yet: the release kernel is the signed one (kernsign); re-run on that derivation |
| K2 | The scx gate: all 16 shipped sched-ext schedulers attach, survive load and detach on the shipped kernel | `kernel-vm` (bootc's own `tests/kernel/scx-selftest` + `run-scx-selftest.sh --parse-only`) | `kernel-vm` green on feat/nixos 8b492a5e8 (foundation). A `nix build --rebuild` of that same derivation on 2026-11-01 ~21:35, host load average ~120, failed `scx_eevdf` `not_enabled_within_10s` (16 of 17 passed): the 10 s attach window is not robust under load. The ledger branch's own `kernel-vm` derivation (fedora-parity on the node) failed the same way at that load, then passed at 23:02 with load average ~5 (all 17 cases) | not met yet: needs a stable pass (attach window or runner isolation) and the signed-kernel re-run |
| K3 | A `kill -9` of the running scheduler hands every task back to the kernel's class | `kernel-vm` | as K2 | not met yet |

## Boot chain: OVMF, shim → Fedora GRUB → MOK-signed kernel (D3)

The probe evidence below is `nix/probe/ovmf/` (feat/nixos, from nix/ovmf-sb-probe 40cc38b95; logs
`/var/lab/rime-nix/artifacts/ovmf/logs/`). It used nixpkgs' Linux 7.3, a throwaway TEST MOK, a hand-built disk with
bootupd's static GRUB config, pre-seeded `MokList`, no LUKS, and no Rime boot hook. A line closes only when the **Rime
kernel, signed by the real pipeline's key path** (kernsign) boots from an entry **written by Rime's boot hook**
(boothook) on a system built from the shipped modules.

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| B1 | plain, SB on, BLS entry (fresh install) | OVMF lab log: kernel reports `SecureBoot enabled`, reaches the greeter | probe case `a-bls-sbon-mok-signed`: boots to stage 2; unsigned (`a-bls-sbon-mok-unsigned`), tampered and no-MOK cases refused | not met yet |
| B2 | plain, SB on, `custom.cfg` entry (migrated) | OVMF lab log | probe case `b-custom-sbon-mok-signed`: boots; `b-custom-sbon-mok-unsigned` refused | not met yet |
| B3 | plain, SB off, BLS | OVMF lab log | probe case `a-bls-sboff-unsigned`: boots | not met yet |
| B4 | plain, SB off, `custom.cfg` | OVMF lab log | probe case `b-custom-sboff-unsigned`: boots | not met yet |
| B5 | LUKS, SB on, BLS | OVMF lab log: passphrase prompt, unlock, greeter | none (no LUKS case in the probe) | not met yet |
| B6 | LUKS, SB on, `custom.cfg` | OVMF lab log | none | not met yet |
| B7 | LUKS, SB off, BLS | OVMF lab log | none | not met yet |
| B8 | LUKS, SB off, `custom.cfg` | OVMF lab log | none | not met yet |
| B9 | A user recovers from a bad generation through the GRUB menu, plain | OVMF lab log + screenshot of the GRUB menu entry picked; the previous generation reaches the greeter | none (boothook in progress) | not met yet |
| B10 | The same, LUKS (D3 lists plain and LUKS for P2) | OVMF lab log + screenshot | none | not met yet |

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
| H8 | fsck containment gate (D2) | VM test | `rime migrate start` runs ostree fsck + `btrfs device stats -c` + a read-only scrub before and after, then compares ostree's /boot, custom.cfg outside the hook's block, the deployments and every ESP file: both clean and unchanged in the OVMF A and E runs (`/var/lab/rime-nix/artifacts/bridge/A/A-start.txt`, `E/E-start.txt`); a failure rolls back: verifier run with `--rollback-first` injects a fault right after the boot entry is written, `start` exits 10 "rolled back", and an in-guest verdict finds no new subvolume, the old label, no masks, no `/boot/rime` or `rime-nixos.cfg`, custom.cfg as before (`VERDICT=clean`, `/var/lab/rime-nix/artifacts/verify-bridge/ovmf/{A,E}/*-rollback-*.txt`); the rollback used to leave the hook's kernels and menu on /boot (fixed on this branch; mutant rollback-keeps-kernels + `UndoBoot` unit tests); `bridge-unit` tests the diff with 2 mutants | met (2026-11-02) |
| H9 | Free-space gate on the disk figure: closure + one mass rebuild (~10.5 GB) + what the old copies can pin (D2) | `rime migrate check` VM test: refuses below the figure, by name | check `bridge-refusals`: `REFUSED [free-space]` on the H9 disk and on a 1 GiB A under the real 19 GB floor, disk sha256 unchanged (`/var/lab/rime-nix/artifacts/bridge/bridge-refusals.log`); the OVMF A run reports "52.5 GB free, 27.8 GB needed (floor 19.0 + home 0.0 + old store 8.8)"; mutants free-space-flipped + floor-dropped | met (2026-11-02) |
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
| D1 | Topology **E** (a failed boot migration's leftovers; katana) migrates in a VM | OVMF lab log on a VM built to topology E | `/var/lab/rime-nix/artifacts/bridge/run-E.log` + `E/`: a disk from the published image (2026.11.01.2) + katana's leftovers, booted through `APEX-OS Primary`; check = E (on D), repair-e, start; NixOS boots by default, greeter shown (`E/E-nixos-greeter.png`), old password logs in, home sums OK, phase=failed carried; the ostree entry still boots; NixOS again after it. SB off; katana's snapshot also classifies as E (`katana-state-classify.log`). Reproduced by the verifier (`/var/lab/rime-nix/artifacts/verify-bridge/run-E.log`, PASS). Caveat: the lab's `\EFI\APEX\grub.cfg` is bootupd's static file placed by `mkbootc.sh`; katana's real one is not known (PLAN open question 7), so katana itself may read `boot-chain-unverified` until it is checked | met (2026-11-02, SB off, lab chain) |
| D2 | Topology **A** (installer default) migrates in a VM | OVMF lab log | `/var/lab/rime-nix/artifacts/bridge/run-A.log` + `A/`: a disk from the published image (2026.11.01.2), booted through `Fedora`; check = A, start; @nixos/@nix/@home mounted compress-force=zstd:3, greeter shown (`A/A-nixos-greeter.png`), old password logs in, home sums OK, machine-id kept; the ostree entry still boots; NixOS again after it. SB off. Reproduced by the verifier (`/var/lab/rime-nix/artifacts/verify-bridge/run-A.log`, PASS). Plain layout only. The installer encrypts by default (`installer/rime-installer-gui` p_encrypt, "on by default"), and that layout has /boot as its own ext4 partition: `check` refuses it by name `boot-partition` (mutant accept-boot-partition) because `rime.rootfs` binds /boot from the btrfs top level; before this branch's fix it was accepted. So the default install is refused, not migrated; a LUKS A VM run is still owed | partial (2026-11-02: plain A met, SB off; encrypted A refused) |
| D3 | Topology **B** is refused by name | `rime migrate check` VM test: names B, writes nothing | check `bridge-refusals`: `REFUSED [topology-B]`, disk sha256 unchanged (`/var/lab/rime-nix/artifacts/bridge/bridge-refusals.log`); its rw-mount mutant fails on the sha256 (`bridge-refusals-mutant-rw.log`); both reproduced by the verifier (`/var/lab/rime-nix/artifacts/verify-bridge/bridge-refusals-HEAD.log` rc=0, `bridge-refusals-mutant-rw.log` rc=1) | met (2026-11-02) |
| D4 | Topology **C** is refused by name | same | check `bridge-refusals`: `REFUSED [topology-C]`, disk sha256 unchanged | met (2026-11-02) |
| D5 | Topology **D** is refused by name | same | check `bridge-refusals`: `REFUSED [topology-D]`, disk sha256 unchanged | met (2026-11-02) |
| D6 | Topology **F** is refused by name | same | check `bridge-refusals`: `REFUSED [topology-F]`, disk sha256 unchanged | met (2026-11-02) |

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
| X1 | The topology E repair runs | D3 P2 gates | OVMF lab log: `/var/lab/rime-nix/artifacts/bridge/E/E-repair-e*.txt`, `E-after-repair.txt` (trial ESP dirs + the trial NVRAM entry archived under /var/lib/rime/boot-migrate/archive-*, entry deleted, BootOrder untouched, phase=failed kept and carried to NixOS). The trial's `/composefs` and `/state` in the btrfs top level are left in place by design (not on the boot path; `E/E-back-state.txt`) | met (2026-11-02) |
| X2 | The `test-boot-v2.sh` scan finds no `bootctl`, `grub-install` or `efibootmgr` in the activation script or the boot hook | D3 | check attr (boothook) | not met yet |
| X3 | A machine that never took the release carrying `RENAMED_SIGNER` is refused by name | D1 | `rime migrate check` VM test: `bridge-refusals` X3 disk `REFUSED [renamed-signer]`, sha256 unchanged (`/var/lab/rime-nix/artifacts/bridge/bridge-refusals.log`); mutant no-signer-check | met (2026-11-02) |
| X4 | An end-to-end update in a VM refuses a tampered narHash, an unsigned manifest and a wrong identity | D6 | `update-e2e` (release + client) | not met yet |
| X5 | Rollback works after a refused update | D6 | `update-e2e` | not met yet |
