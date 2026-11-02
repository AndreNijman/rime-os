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
| T1 | PCR 7 is recorded before the move | OVMF lab log (`tpm2_pcrread sha256:7`, swtpm) | `nix/probe/ovmf/tpm` (README.md), Secure Boot on, swtpm: boot P2 (Fedora shim -> GRUB -> Fedora-signed kernel) and P3/P4 (same chain, MOK-signed kernel: the bootc-Rime shape). PCR 7 read from sysfs `pcr-sha256/7` in a busybox initrd and equal to the `tpm2_eventlog` replay of the saved event log (`tpm2_pcrread` itself runs on the NixOS boots). `/var/lab/rime-nix/artifacts/tpmkeymap/tpm/main/{results.tsv,pcr7.txt,P*.eventlog.yaml}`. TEST MOK and OVMF's db: katana's real value is TODO(handover). Independent rerun with fresh TEST keys (getty and ask-watcher races in the lab driver fixed): `/var/lab/rime-nix/artifacts/verify-tpmkeymap/tpm/main/` 10/10 PASS, P2 reproduced exactly | **met** (2026-11-02) |
| T2 | PCR 7 is recorded after the move, and the difference is explained | OVMF lab log | boot N1: NixOS Rime through the Rime boot hook's BLS entry on the same shim/GRUB/MOK: `tpm2_pcrread sha256:7` = sysfs = event-log replay = **P3's value** (`345554e0...`). P2 differs from P3 by one `EV_EFI_VARIABLE_AUTHORITY` (MokListRT) event carrying the MOK certificate: PCR 7 records the key that signed the kernel, not the kernel (README.md, "Why P2 and P3 differ"). N2: the bootc-era TPM slot unseals on NixOS with 0 asks. `/var/lab/rime-nix/artifacts/tpmkeymap/tpm/main/`. Independent rerun: N1 = P3 (`a1c21dd1...` with that run's TEST MOK), N1's PCR 7 event list = P3's, P2 vs P3 = the one MokListRT event: `/var/lab/rime-nix/artifacts/verify-tpmkeymap/tpm/main/` | **met** (2026-11-02) |
| T3 | The TPM-only re-enrolment keeps the pre-existing recovery key working: after re-enrolment, the volume unlocks **with the recovery key** | OVMF lab log of the recovery-key unlock | wired: `nix/modules/luks-tpm.nix` `rime-luks-tpm-reenrol@<uuid>.service` -> `rime-luks-enroll --tpm-only` (never touches the recovery or passphrase slots, and fails if they changed). Lab N3: stale slot refused, re-enrolment `result=success`, slots 0 and 1 byte-identical; N4: the new slot unseals, 0 asks; N5 (TPM cleared) and N6 (no TPM): the recovery key made with the disk unlocks at the initrd prompt. Mutant `run.sh run mutant-bootc-reenrol` (bootc's recovery-replacing re-enrolment): M4 FAILS (the old key is refused). `/var/lab/rime-nix/artifacts/tpmkeymap/tpm/{main,mutant-bootc-reenrol}/`. Independent rerun: `/var/lab/rime-nix/artifacts/verify-tpmkeymap/tpm/{main,mutant-bootc-reenrol}/` (main 10/10, mutant M4 only FAILS); script-level mutant (a `--tpm-only` that rotates the recovery key in the same slot) is caught by the kept-slot fingerprint: `/var/lab/rime-nix/artifacts/verify-tpmkeymap/enroll-tpm-only-{real,mut}.log`; a `--tpm-only` decline (no TPM slot left) now exits 3 instead of 0, so `rime-luks-tpm-reenrol@` cannot report success without a slot: `enroll-tpm-only-fixed.log` | **met** (2026-11-02) |

## Keymap gate (D2)

| # | Gate | How proven | Evidence so far | Status |
|---|---|---|---|---|
| M1 | A LUKS volume with the **gb** keymap unlocks with a passphrase holding a layout-dependent character | Nix VM test (systemd initrd, the character typed as keycodes; PLAN named `console.keyMap = "uk"`, the product carries host.toml's kbd name `gb` as `rd.vconsole.keymap=gb` on a shared `us` initrd) | `checks.x86_64-linux.keymap-luks`: host.toml `[keyboard] console = "gb"` -> rime-boot writes `rd.vconsole.keymap=gb` -> systemd initrd + plymouth prompt; passphrase `a@b"c#d` typed as key codes unlocks at the first ask; a same-run us control refuses the same key codes. The keymap is `gb` (the kbd name bootc's installer writes, shipped by `rime-kbd-xkb`), not nixpkgs' `uk`. Mutant `initrd-us` (the initrd's keymap tree is nixpkgs' kbd alone, so the us map stays): FAILS (`nix build --impure --expr` over `nix/tests/keymap.nix { pkgs; self; mutant = "initrd-us"; }`). `/var/lab/rime-nix/artifacts/tpmkeymap/{keymap-luks,checks-keymap-libexec-2}.log`, `/var/lab/rime-nix/artifacts/tpmkeymap/mutant-keymap-initrd-us{,-2}.log`. Independent: fresh rerun of the unchanged check `/var/lab/rime-nix/artifacts/verify-tpmkeymap/keymap-luks-rerun-2.log`; hardened check (the ask report waits for systemd-vconsole-setup to finish and rescans at switch-root) `/var/lab/rime-nix/artifacts/verify-tpmkeymap/keymap-luks-final.log`; mutant initrd-us on it FAILS `/var/lab/rime-nix/artifacts/verify-tpmkeymap/mutant-initrd-us-final.log`; mutant rime.keymap.enable=false FAILS `/var/lab/rime-nix/artifacts/verify-tpmkeymap/mutant-keymap-disabled.log` | **met** (2026-11-02) |
| M2 | The same with the **ch** keymap | Nix VM test | `checks.x86_64-linux.keymap-luks`, same run: `rd.vconsole.keymap=ch` from host.toml, passphrase `zy-'+*` typed as y, z, slash, minus, shift+1, shift+3, unlocks at the first ask. Independent: same runs as M1 (`/var/lab/rime-nix/artifacts/verify-tpmkeymap/keymap-luks-final.log`: ch1 unlocks after 1 ask, vconsole-setup active/success) | **met** (2026-11-02) |

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
| X2 | The `test-boot-v2.sh` scan finds no `bootctl`, `grub-install` or `efibootmgr` in the activation script or the boot hook | D3 | check attr (boothook) | not met yet |
| X3 | A machine that never took the release carrying `RENAMED_SIGNER` is refused by name | D1 | `rime migrate check` VM test | not met yet |
| X4 | An end-to-end update in a VM refuses a tampered narHash, an unsigned manifest and a wrong identity | D6 | `update-e2e` (release + client) | not met yet |
| X5 | Rollback works after a refused update | D6 | `update-e2e` | not met yet |
