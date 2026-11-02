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
| P1 | The translator reports every requested package on a copy of katana's state | translator test on the katana-state copy: no package unreported | `rime pkg translate` (rimed/rime/src/nixos/translate.rs). `checks.pkg-translate`: a synthetic bootc tree with every kind of record, 41 records, each outcome pinned, PASS. `checks.pkg-translate.katana` run on 2026-11-02 against the katana-state copy plus the read-only package sidecar (runtime only, never in git or the store): 13 sources read, "records counted from the tree: 59; rows reported: 59; 27 carried, 19 dropped, 4 feature, 3 flatpak, 2 kept, 2 pending, 2 refused", PASS, both trees' hashes unchanged (log `/var/lab/rime-nix/artifacts/d5/katana-run/translate-katana.log`). Mutant: a translator that drops the requested `steam` row fails the judge with "UNREPORTED requested: steam (x1)" (`katana-run/judge-mutant-drop-steam.log`); verifier re-run 59/59 PASS and a second mutant (the `/var/usrlocal` claude-desktop row dropped: "UNREPORTED usrlocal: claude-desktop", rc 1), `/var/lab/rime-nix/artifacts/verify-d5/katana-run/` (root 0700). The judge counts records by the translator's own source list, so a source both omit is invisible to it: `/var/lib/rime/pkg/deb/` (rime-pkg's DEB_DIR, cached .deb files) is read by neither; its packages are reported through their `deb:` lines in the requested list, and katana has none | met (D5, 2026-11-02) |
| P2 | Steam launches a game in Gaming Mode in a VM with a GPU stub | Nix VM test + screenshot | `checks.gaming-launch` (nix/tests/gaming-launch.nix), offline, Mesa lavapipe as the GPU stub, gamescope `--backend headless`: greetd's initial_session runs the offered entry's Exec → rime-gaming-session → gamescope → rime-gamescope-steam → nixpkgs' `steam` → Valve's bootstrap → Steam's own client binary (`ubuntu12_32/steam -gamepadui -steamos3`), kept up by a local stall server on Steam's update host; the client carries `GAMESCOPE_WAYLAND_DISPLAY=gamescope-0`, `STEAM_ENABLE_VOLUME_HANDLER=1`, `__GL_SHADER_DISK_CACHE_SIZE=12000000000`. A stub game (vkcube) started with the client's own environment inside Steam's FHS runtime (`steam-run`), app id 480 set on its window and the base layer by the test as Steam would: `GAMESCOPE_FOCUSED_APP = 480`, and gamescope's own screenshot shows the cube (two different frames, `/var/lab/rime-nix/artifacts/d5/game-a.png`, `game-b.png`). PASS 2026-11-02. Mutant: the session without rime-gamescope-steam (`RIME_GAMESCOPE_STEAM=/nonexistent`) fails. **Not proven:** Steam logging in, updating or starting an installed game itself (`steam -applaunch`), Proton, a real GPU | partly met (D5, 2026-11-02): the session, Steam's client and a game composited in it are proven offline; Steam itself starting a game (`-applaunch`, needs the network and an account) is not, nor real hardware (P3 gate on katana) |
| P3 | claude-code runs from `~/.local/bin` under nix-ld | Nix VM test | `checks.catalogue` subtest "claude-code's release binary in ~/.local/bin runs under nix-ld (P3)": the unpatched release binary (interpreter `/lib64/ld-linux-x86-64.so.2`) copied to `~/.local/bin/claude`, `command -v claude` = that file, `claude --version` prints nixpkgs' claude-code version, PASS 2026-11-02 (`/var/lab/rime-nix/artifacts/d5/catalogue-build.log`). Mutant: the same check with `programs.nix-ld.enable = false` fails exactly there, `claude --version` exit 127 (`catalogue-mutant-no-nix-ld.log`). This is the native release binary (Anthropic's installer shape); the catalogue's own `rime install claude-code` route (`user-npm`, npm into `~/.local`) is checked for routing only, not run offline | met (D5, 2026-11-02) |
| P4 | Equibop runs (needed for Discord presence) | Nix VM test | `checks.equibop` (nix/tests/equibop.nix): the catalogue's `equibop` (class 2, nixpkgs 3.2.2) started by Rime's own rime-gaming-discord (hidden, `--ozone-platform=x11 --disable-gpu`), its Electron main process up, arRPC listening on `$XDG_RUNTIME_DIR/discord-ipc-0` within 8.25 s and answering a game's HANDSHAKE with READY; when the owner exits Equibop stops and nothing answers. PASS 2026-11-02 (`/var/lab/rime-nix/artifacts/d5/equibop-build.log`). Mutant (verifier, 2026-11-02): rime-gaming-discord with its owner check disabled fails at "the owner exits: Equibop stops" (`/var/lab/rime-nix/artifacts/verify-d5/equibop-mutant-ignores-owner.log`). Precondition, not a mutant: without the Equicord asar in `sessionData/equicord.asar` the offline Equibop waits on its GitHub download and never starts arRPC (`equibop-build-attempt3-noasar.log`); a fresh machine therefore needs the network at Equibop's first start, and that download is an app self-update channel (handover). Not proven: Rich Presence reaching Discord (needs an account), `rime install equibop` (the release names no class 2 builds yet: nix/release/default.nix `catalogue = [ ]`) | met (D5, 2026-11-02) |

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
