# Open items after wave 3 (integration, 2026-11-03)

Every item the wave-2 workers, verifiers and integrator handed on (`/var/lab/rime-nix/artifacts/wave3-inputs.md`, 153
items in 19 sections, each numbered as in the ledger), with its state on `feat/nixos` after the wave-3 integration. Then the
items wave 3 itself opened.

States:

- **FIXED**: done, with the commit and the check or log that shows it.
- **DECIDED-NOT-TO**: deliberately not done, or nothing to act on; where the decision is written.
- **P3/HARDWARE**: needs katana, real firmware, a real account, the release environment or Andre; what is needed.
- **STILL OPEN**: agent work not done yet, and why.

An item that repeats one already answered says "see" and the first answer, so the two cannot drift. "The integration
check" means the wave-3 proof on the merged head: every `checks.x86_64-linux.*` outPath valid in the store
(`/var/lab/rime-nix/artifacts/integrate-wave3/outs-final.txt`; how it was built: `docs/nixos/p2-gates.md`, "P2 status").

## build:tpmkeymap

1. *No gate open in scope; B10 is bootmatrix's.* DECIDED-NOT-TO: nothing to act on (B10 met, p2-gates.md).
2. *katana's real PCR 7 under the real MOK and db/dbx not measured; MokManager / `mokutil --import` not exercised.*
   Split. MokManager enrolment: FIXED in the lab by nix/secureboot (796e830eb, `nix/probe/ovmf/mok`: the real Rime
   MOK enrolled screen by screen on a Microsoft-keys-only varstore). Real PCR 7 on katana: P3/HARDWARE (katana with
   the real MOK and firmware; `docs/nixos/p3-katana-gates.md`).
3. *`rime-luks-enroll` now refuses a volume with a recovery key unless `--replace-recovery`; bootc docs describe the old
   behaviour.* FIXED: `docs/disk-encryption.md` (lines 87, 277) and `docs/boot-v2.md` (37, 1543) describe the refusal
   and `--replace-recovery` (nix/secureboot, 0aa306d1a).
4. *luks-tpm.nix consumes no `rime.luks.*` option.* DECIDED-NOT-TO: volumes are named by `rd.luks.*` from host.toml
   (the carry and the installer write them; the encrypted-A bridge run L unlocks that way, p2-gates D2);
   `boot.initrd.luks.forceLuksSupportInInitrd` (nix/modules/luks.nix) gives the generator without per-volume options.
5. *Nothing starts `rime-luks-tpm-reenrol@`; nothing tells the owner after a Secure Boot or dbx change.* FIXED:
   `rime luks status` / `rime luks rebind` and a boot check of each TPM slot (b5d4bca5f; `docs/disk-encryption.md`
   269, `docs/boot-v2.md` 1548). The rebind succeeding under Secure Boot on real firmware is P3 (see "Opened by wave 3").
6. *keymap-luks typed while vconsole-setup was activating.* FIXED: the test types only once plymouth holds the question
   and a password agent reads the keyboard (9d13a72fe, 61a187318; `docs/nixos/ci.md` "keymap-luks under load", proven
   on 2 pinned CPUs at load ~12).
7. *tabrmd D-Bus warning in the TPM lab.* FIXED: `TPM2TOOLS_TCTI=device:/dev/tpmrm0` (f747d4fb9,
   `nix/probe/ovmf/tpm/systems.nix`).
8. *Lab disks deleted; remake with `run.sh disks`.* DECIDED-NOT-TO: a recipe, not a defect (`nix/probe/ovmf/tpm/README.md`).

## build:hostgates

1. *H9 free-space gate not in scope.* FIXED: H9 met (p2-gates.md H9, bridge refusals).
2. *Re-running the katana gate needs the root-0700 runtime share.* DECIDED-NOT-TO: by design (katana state is never
   committed). The share and recipe moved to `/var/lab/rime-nix/artifacts/hostgates/` (`kat-run.sh`); the wave-2
   integration re-ran it (STATUS.md, wave 2).
3. *H3: no OpenVPN profile brought up.* FIXED: hostgates brings up a WireGuard and an OpenVPN profile (de8535cd2;
   `docs/nixos/ci.md` hostgates rows, "H3 (WireGuard, OpenVPN)").
4. *H2: MAC-pinned 5 GHz profile only accepted.* FIXED: the pinned profile refuses a radio with another address, then
   comes up on a radio whose permanent address it is (de8535cd2, `nix/tests/hostgates.nix` header, lib/hwsim-radio.py).
5. *rpcbind forced off, NFSv3 locking untested.* FIXED: rpcbind on demand like Fedora, tested with a share (de8535cd2;
   `nix/modules/hostlayer.nix` 95, `docs/nixos/fedora-defaults.md` 90).
6. *DiskLab makes ext4 only.* FIXED: btrfs and xfs extra disks too (de8535cd2); the carried btrfs disk is scrubbed
   (13b39689d).
7. *No test that a machine without host.toml is called `rime`.* FIXED: `checks.hygiene` asserts `/etc/hostname`,
   the kernel hostname and `hostnamectl --static` are `rime`, with a mutant (45c2c4ac1, `nix/tests/hygiene.nix` 81-83, 126).
8. *Real hardware unproven (radios, disks, btrfs fsck).* P3/HARDWARE: katana after the move (p3-katana-gates.md).
9. *hostgates.nix headers; the "work in progress" commit dd166b8de could be squashed.* DECIDED-NOT-TO: history on
   `feat/nixos` is not rewritten; the headers are current.

## rebrand-merge

1. *No p2-gates rows in scope for the merge.* DECIDED-NOT-TO: nothing to act on.
2. *rime-shell `run-config-agents-keys-test.sh` fails on feat/nixos.* FIXED: rime-shell nix/shell-keys 5e8635e6 (the
   keys suite waits for state) merged as rime-shell `feat/nixos` 90f4d49a, locked in rime-os 2adce61a0; the shell
   PR's CI ran it green (shell worker).
3. *Suites write `files/system/libexec/__pycache__`; `path:.` copies it; rime-libexec fails.* FIXED: rime-libexec
   ignores `__pycache__` (45c2c4ac1).
4. *nix/rebrand branches and worktrees left for the orchestrator.* FIXED: deleted at the wave-2 integration (no
   `nix/rebrand*` branch or worktree exists).
5. *nix/rebrand-merge not pushed; STATUS.md line.* FIXED: merged at wave 2 (STATUS.md, wave 2) and `feat/nixos` was
   pushed afterwards (hosted runs 41400977973 and 41450212626 ran on it, `docs/nixos/ci.md`).

## verify:rebrand-merge

1. *The whole `files/` tree (11 MiB) reaches the closure through 50-rime-keymap.* FIXED: 50-rime-keymap names its
   script only (f732a192b).
2. *niri's default config binds Mod+T to alacritty.* FIXED: Mod+T opens the default terminal (45c2c4ac1).
3. *config-agents-keys still fails on its parent.* See rebrand-merge 2.
4. *`__pycache__` fragility.* See rebrand-merge 3.

## verify:hostgates

1. *katana's real machine-id and disk UUIDs in committed files.* FIXED: made-up values (b40513bab, wave 2); `git grep`
   at the wave-3 head finds them only in `ROADMAP/evidence/**`, which is never edited (CLAUDE.md).
2. *Carried `rd.luks.*` cannot unlock: rime-vm has no LUKS in its initrd.* FIXED: `boot.initrd.luks.forceLuksSupportInInitrd`
   (nix/modules/luks.nix) and the bridge's encrypted-A run L: the NixOS initrd asks for the passphrase and opens
   `/dev/mapper/luks-<uuid>` from host.toml's `rd.luks.*` (7e4317651, c277c3c6d; p2-gates D2).
3. *H2's "wpa_supplicant off fails" log was overwritten.* FIXED: H2 evidence re-recorded at the final content with
   dated mutant runs (743dc2390; p2-gates H2).
4. *Remaining hostgates handovers.* See build:hostgates 3-8.
5. *`nix flake check --no-build` killed (137).* DECIDED-NOT-TO: superseded; every check is built by the integration check.

## build:flakes

1. *No gate row in scope; STATUS.md "G2 message" row stale.* FIXED: STATUS.md says "Done" for it (wave 2 integration).
2. *Full `security` check (5 VMs) not run with its new asserts.* FIXED: built by the wave-2 and wave-3 integration
   checks.
3. *STATUS.md G2 row.* See 1.
4. *Every VM check rehashes (rime-qemu wrapper).* FIXED: the integration checks rebuild all of them.
5. *nix.yml time limits and the nix-release dry-run unproven until a hosted run.* FIXED: limits set from two measured
   hosted runs (93ce0afbd; `docs/nixos/ci.md` "Timeouts"); the dry-run met `eval-each.sh` and was fixed (8bc447255),
   passing in hosted run 41450212626.
6. *`vm-harness-vm.stock` is built by hand.* DECIDED-NOT-TO: by design (it must hang; `docs/nixos/p1-exit.md`).
7. *`listening()` scans /proc/net/unix with a bash loop, 1-4 s per VM start.* FIXED: one read per poll (22c7fbd5f).
8. *unit-docs reads only rime-vm; boot-v2 units not installed on NixOS.* DECIDED-NOT-TO: the bootc-doc mutant makes
   the check fail if one is added without its link.
9. *rimed-session gives the VM a bootc-shaped /bin.* DECIDED-NOT-TO: test fixtures for rimed's tests; remove when they
   stop assuming /bin paths.

## verify:flakes

1. *Only GitHub proves the time limits and the dry-run.* See build:flakes 5. The hosted runner's free disk for a full
   release closure is not measured: see "Opened by wave 3".
2. *`vm-harness-vm.stock` log is from an earlier tree.* DECIDED-NOT-TO: passthru illustration only.
3. *The 6x concurrent stress not re-run.* DECIDED-NOT-TO: same driver hash as the stress run; the VM limit allows 2.
4. *STATUS.md G2 row.* See build:flakes 1.
5. *Every VM check rehashes.* See build:flakes 4.
6. *nix-release's kernel-sign job runs on non-main refs when the variable is set.* FIXED: the release workflow calls
   the kernel signer on main only (e04971196).
7. *No p2-gates rows changed.* DECIDED-NOT-TO: nothing to act on.

## build:g2

1. *G3 "How proven" (client VM timing) not done; deltas not wired into `rime update`.* FIXED: nix/deltas wired deltas
   into `rime update` and the release workflow (5820fd615, 0514ad2f8, c5e478817) and timed them in two VMs
   (`checks.update-delta`, g3-vm runs; p2-gates G3 met).
2. *P4: wire deltas (previous release only, fallback, streamed import, parallel, window cap) and time it.* FIXED: as
   1 (`docs/nixos/delta.md` §5.2, §7).
3. *Release pipeline: deltas at `zstd -19 --long=27`, skip a delta over ~80 %.* FIXED: c5e478817, 3df752039 (stage at
   level 19).
4. *Experiment: kernel with `CONFIG_MODULE_COMPRESS_NONE`.* DECIDED-NOT-TO (now): measured (7d14f7dfa,
   `nix/release/delta/lab/modules-uncompressed.sh`); `docs/nixos/delta.md` 306 decides to ship it at the next
   `kernel.pin` bump (`stripExclude = [ "*.ko" ]`), not here.
5. *Measure `compression-level=19` without `--long` before raising the cache level.* FIXED: measured and adopted
   (`delta.md` 23, §8.1; 7d14f7dfa, 3df752039).
6. *A3: keep batching non-security mass rebuilds.* DECIDED-NOT-TO (yet): `delta.md` 361 "A3 (decided, not closed)":
   at most once every 4 weeks; revisit with real 30-day numbers (P4).
7. *STATUS.md not edited; `checks.delta` is new.* FIXED: STATUS.md lists it (wave-3 section).

## verify:g2

1. *G3 open as worded.* See build:g2 1.
2. *Re-decode guard was never tested.* FIXED: b2fa4b6f8 (wave 2).
3. *Fixtures have one candidate per path and empty References: "smallest of several candidates" and references-first
   ordering are untested.* STILL OPEN: low value, fail-safe (a wrong order makes `nix copy --no-recursive` refuse);
   owner: delta/release side, add a two-candidate fixture to `checks.delta`.
4. *scratch/g2 left untouched; STATUS.md.* FIXED: STATUS.md lists `checks.delta`; scratch removed at the wave-2/3
   cleanups (no `scratch/g2`).

## build:bootmatrix

1. *R20/R22 have no row; recorded as decisions.* DECIDED-NOT-TO: `docs/nixos/boot-matrix.md` (timeout 5 with
   `timeout_style=menu`; Windows ESP refused by name; `reserveMiB` 32).
2. *p2-gates header still says "every line is not met yet".* FIXED: header and "P2 status" rewritten at this
   integration (p2-gates.md).
3. *(a) `rime.boot.enable` default for shipped profiles; vm-boot passes.* DECIDED-NOT-TO: information.
4. *(b) Early microcode +15.5 MB per generation; a host that knows its vendor should turn the other off.* FIXED:
   microcode loads as separate initrd files and the ESP stays in budget (aa6d0e637; `checks.boot-microcode`,
   `docs/nixos/boot-matrix.md`). Per-vendor choice: see "Opened by wave 3" (bridged machines keep both).
5. *(c) R22.* DECIDED-NOT-TO: as 1.
6. *(d) R20.* DECIDED-NOT-TO: as 1.
7. *(e) K2 attach window 60 s.* FIXED: the window is a parameter of the check (b23d79080) and the wording says what was
   measured (1c3bbf84f; p2-gates K2).
8. *tpmkeymap: `rime.luks.*` options.* DECIDED-NOT-TO: information; see build:tpmkeymap 4.
9. *Probe image has no virtio-gpu-pci; probe runs `label=disable`.* DECIDED-NOT-TO: lab-only
   (`docs/nixos/boot-matrix.md`).
10. *Real hardware: Plymouth prompt on real GPUs, real MOK enrolment; APEX_SB_* untouched.* P3/HARDWARE: katana
    (p3-katana-gates.md); MokManager in the lab: see build:tpmkeymap 2.

## verify:d5

1. *Latent: the release emits `catalogue = [ ]`, so after the move every `rime update` fails on a class-2 app.*
   FIXED: `rime update` names an app the release builds no copy of instead of failing (a34782975, wave 2) and releases
   now name their catalogue builds and feature variants (5468b6bf5; `checks.release-unit`, `.#rime-release-catalogue`
   in the release dry-run).
2. *No end-to-end update -> catalogue-outs -> install check.* FIXED: a real catalogue build in the release worker
   (4ac69da4b) and `checks.catalogue` / `vendor-apps` (79f9fe911) install from release-named builds.
3. *Translator and judge share a blind spot (deb/ dir, `deb_depends`, `.omitted`).* FIXED: the translator reports
   cached .debs, `deb_depends` and saved /etc files (21e3c33a7); the judge sees computed state.json keys (fc1084a9e);
   P1 names the fifth judge mutant (7fb9936a8).
4. *P3 proves the native binary, not `rime install claude-code` (npm route).* FIXED: the npm route runs offline
   (9b2fb6a1c).
5. *P4 relies on a seeded Equicord asar; Equibop downloads at run time.* FIXED: Equibop takes Equicord from nixpkgs
   and never downloads (02d4d075e; `checks.equibop`).
6. *`rime.virtualization.enable` / gaming cannot reach a machine: only `plain` is published.* FIXED: releases publish
   feature variants (5468b6bf5, `nix/release/variants.nix`). The installer installing them: see "Opened by wave 3".
7. *P2 stays partly met.* FIXED: met by decision (nix/apps, ce99ac05a, 17bc4fded: compositing mutant); Steam's own
   launch, sign-in, Proton and the real GPU are P3-S1..S4.
8. *d5's cargo mutants never logged.* DECIDED-NOT-TO: rimed's in-build cargo report is the evidence (3770 passed, 0 failed
   at the integration build, `/var/lab/rime-nix/artifacts/integrate-wave3/rimed-build.log`).
9. *"Not problems" (attribution greps, host reads).* DECIDED-NOT-TO: nothing to act on.

## verify:tpmkeymap

1. *Two timing races in the lab and the check (serial-getty hang-up, 0.2 s ask polling).* FIXED (wave 2, by the
   verifier); keymap-luks: see build:tpmkeymap 6.
2. *M1's "How proven" said `console.keyMap = "uk"`; T1 reads PCR 7 from sysfs plus an event-log replay.* FIXED (wave
   2): the column says `rd.vconsole.keymap=gb` from host.toml; T1 discloses its method (p2-gates T1).
3. */var/lab hit 99 % (other workers' scratch).* STILL OPEN as a host condition: /var/lab is at 97 % (about 19 GB
   free) at this integration; wave-2/3 scratch is cleaned, the large remaining directories belong to other sessions
   (`verify-readiness`, `relayfix`, `rb-website`) and are not deleted without their owner.
4. *`rime.luksTpm.enable` mostly restates nixpkgs defaults.* DECIDED-NOT-TO: not a defect (the module says so).
5. *Carried forward:* real PCR 7 (P3, see build:tpmkeymap 2); MokManager (FIXED, same); luks-tpm options (see
   build:tpmkeymap 4); the rebind hint (FIXED, build:tpmkeymap 5); the bootc docs (FIXED, build:tpmkeymap 3); the
   `rime-luks-enroll` refusal changing bootc behaviour when it reaches main (DECIDED: documented in
   `docs/disk-encryption.md`, a release note when it ships); `keymap_params` checks only the default generation's
   keymap tree (STILL OPEN, no regression: an older generation without `rime-kbd-xkb` boots on `us`; owner rime-boot);
   the lab's "bootc-era slot" is sealed by NixOS `systemd-cryptenroll` (P3/HARDWARE: katana's real bootc enrolment).
6. *`tests/test-boot-v2.sh` 1 failure (the `tail -c +5` inverse control).* DECIDED-NOT-TO: it reads the host's real
   efivarfs and fails the same on main on this machine (seen again at this integration: 154 passed, 1 failed, on #137).

## build:d5

1. *P2 partly met.* See verify:d5 7.
2. *Equibop runtime self-update channel.* See verify:d5 5.
3. *ChatGPT / Claude Desktop pins and catalogue packaging TODO.* FIXED: pinned, vendor-key checked, in the catalogue
   (7c4444c5e, 0592c75bf, 2f48d135a). Their licences for redistribution through the cache: see "Opened by wave 3" (R14).
4. *P2 on real hardware, `-applaunch`, login, Proton.* P3/HARDWARE: P3-S1..S4 (Andre's Steam account, katana).
5. *Floors should count on-disk bytes (14/16 GB).* FIXED on the release side: floors count on-disk bytes (5468b6bf5,
   3ad89d90c; `nix/release/floors.nix`: plain disk 16 GB / partition 14 GB, plain-gaming 18 / 16). The installer
   reads the same figures since nix/installer3 (`nix/installer/choices.nix`; "Opened by wave 3" row 1).
6. *Refuse `rime install steam` below the gaming floor.* FIXED for `rime install` (`variant.rs`
   `steam_is_refused_by_name_below_the_gaming_floor`, 5468b6bf5). The installer refuses gaming below its floor by name
   since nix/installer3 ("Opened by wave 3" row 1).
7. *`appimage.enable` stays on.* DECIDED-NOT-TO: decided (d5; the translator's AppImage note assumes it).
8. *virtualization check: pin `swtpm.enable`.* FIXED: the no-swtpm mutant now fails it (27ddb6fb9). An explicit
   assertion line was not added: see "Opened by wave 3".
9. *gaming-launch black frames / held frames.* FIXED: the cube is told apart by colour and shots retry for 90 s
   (17bc4fded).

## build:bridge

1. *D1/D2 proven with Secure Boot off only.* FIXED: SB on with the Rime MOK chain (c277c3c6d; p2-gates D1, D2).
2. *rime-os main needs the readiness PR.* FIXED (draft, by design not merged): rime-os #137 ships the bridge as
   `/usr/libexec/rime-migrate` and `rime migrate`; refreshed at this integration to the merged bridge (f26b97d17).
3. *Payload manifest unsigned.* FIXED: signed with a dedicated ed25519 key, `manifest.json.sig` checked against the
   image's `nixos-bridge-payload.pub` (7e4317651, f03485259; `docs/nixos/bridge.md`). The release key itself: P3
   (see "Opened by wave 3").
4. *start uses the closure's own nix and chroot, no static nix.* DECIDED-NOT-TO: `docs/nixos/bridge.md` (bridge2
   decisions, 9594161f9).
5. *Old side only masks units; `rime update` still runs after start.* FIXED: #137's `rime` refuses `update`,
   `rollback`, `pin`, `channel set`, `boot migrate`, `live …` by name once the start record exists
   (`tests/test-rime-migrate.sh --with-binary` 98/0 on f26b97d17).
6. *Bugs found by the OVMF runs and fixed.* FIXED (wave 2): each with a test and a mutant.
7. *Untested: SB on, LUKS root, separate /boot, rollback log.* FIXED: run L (encrypted A, /boot ext4, SB on) and the
   verifier's `--rollback-first` runs (c277c3c6d; p2-gates D2, H8).
8. *repair-e leaves `/composefs` and `/state`.* DECIDED-NOT-TO: archived, decided in bridge2 (9594161f9,
   `docs/nixos/bridge.md`).
9. *Lab unit `rime-bridge-lab` reads degraded.* DECIDED-NOT-TO: lab-only.
10. *The L16 was not used.* DECIDED-NOT-TO: information.
11. *Reproduce via README.* DECIDED-NOT-TO: information (`nix/probe/ovmf/bridge/README.md`).

## verify:bootmatrix

1. *bootmatrix invalidated 8 checks; 9 more were unbuilt against the new rime-vm.* FIXED: the integration checks build
   every check (wave 2 and this one; p2-gates "P2 status").
2. *K2's 60 s window is 6x bootc's 10 s; the settle=10 mutant was never run.* FIXED: run on the signed derivation at
   loads 58-93, it passes 17/17 four times (`docs/nixos/boot-matrix.md` 198-213; 36e88ec48, 1c3bbf84f), so the
   wider window is margin for heavier load, not a hidden failure.
3. *K2/K3 "How proven" said `kernel-vm`.* FIXED: both say `kernel-vm-signed` (p2-gates K2, K3).
4. *Early microcode adds 15.5 MB per generation; check the ESP and generation limit.* FIXED: see build:bootmatrix 4.
5. *Probe runs `label=disable`; the in-VM driver uses `pgrep -f`.* DECIDED-NOT-TO: lab-only, inside a throwaway guest.

## verify:bridge

1. *Nothing refuses `rime update` / `rime boot migrate` after start.* See build:bridge 5.
2. *D2 was only partial (encrypted A refused).* FIXED: encrypted A migrates (bridge2, run L; p2-gates D2 met).
3. *D1 rests on the lab ESP chain; katana's `\EFI\APEX\grub.cfg` is unknown.* P3/HARDWARE: run `rime migrate check
   --explain` on katana (`docs/nixos/bridge.md` handover; p3-katana-gates.md).
4. *Secure Boot off in every run.* See build:bridge 1.
5. *Payload unsigned.* See build:bridge 3.
6. *No static nix.* See build:bridge 4.
7. *Left-behind report shown only after start.* FIXED: start always shows it first (627cb4f31).
8. *Login proven over serial only.* FIXED: run L logs in through rime-greet into Hyprland (`artifacts/bridge2/L/L-nixos-desktop.png`).
9. *repair-e leftovers.* See build:bridge 8.
10. *`rime backup key export` not done.* DECIDED-NOT-TO for the recovery key (`docs/nixos/bridge.md` 145); the LUKS
    header is backed up (`keys/luks-header.img`, run L) and `rime migrate key-export` exists.
11. *H8 rollback evidence overwritten.* FIXED: the verifier's `--rollback-first` runs (p2-gates H8).

## build:installer

1. *Release MOK signature unproven; MokManager not clicked through.* Split. MokManager: FIXED in the lab (see
   build:tpmkeymap 2). The release kernel signature: P3/HARDWARE, blocked on Andre: `nix-kernel-sign.yml` is not on
   main and `RIME_NIX_KERNEL_HOST` is unset (`docs/nixos/installer.md` handover; `nix/probe/ovmf/mok/run.sh realsig`
   is ready).
2. *Merge order with bootmatrix.* DECIDED-NOT-TO: obsolete (installer.md).
3. *Production cache URL and key; real MOK signature; verification of the toplevel before install.* Split. Verifying
   a signed release before install: FIXED (65f65b289: the NixOS path installs a verified signed release from the
   production caches). The production cache and the real keys: P3/HARDWARE (release environment).
4. *Partition mode refused (no Windows .exe flow); NETINSTALL refused.* FIXED: partition mode on the NixOS path
   (65f65b289) and the .exe offers no root smaller than the installer accepts (945f6f694); the NixOS ISO is the
   netinstall shape (c0e894866 tests the path with the netinstall marker on). A production netinstall ISO and a new
   .exe: P3 (see "Opened by wave 3").
5. *`rime.installTarget` and `rime.rootfs` overlap.* FIXED: one option set (ea835825d; `nix/installer/target.nix` maps
   onto `rime.rootfs`).
6. *Recovery key written to the boot USB fails read-only.* DECIDED-NOT-TO: pre-existing bootc behaviour; the GUI
   handles `recovery_unsaved` (installer.md).
7. *splash twice, journald restart, no static /etc/hostname, zsh-newuser-install.* FIXED: one `splash`, a stock
   `/etc/skel/.zshrc`, hostnamed reads `/etc/hostname` (da24223c4, ab01f061f; `checks.installer-polish-eval`,
   `checks.hygiene`; installer.md 177-199). The journald restart is explained in installer.md; the journal is No_COW
   since nix/installer3 ("Opened by wave 3").
8. *LUKS greeter session not probed.* FIXED: the greeter types in the machine keymap from host.toml (1ffc80fcd) and
   the I3 firstboot run types a `gb` passphrase (installer.md 224).
9. *Rerun recipe.* DECIDED-NOT-TO: information.
10. *Write-up location.* DECIDED-NOT-TO: information (`docs/nixos/installer.md`).

## verify:installer

1. *`Containerfile.installer` adds nix-core and bootupd to every live ISO.* FIXED: only on a NixOS ISO
   (`--build-arg NIXOS=1`, `installer/Containerfile.installer` 161-192).
2. *Pre-wipe `nix store verify --recursive` cost against an https cache unmeasured.* FIXED: per-cache `nix store
   verify` without `--recursive` under a deadline, timings measured (installer.md 62-77).
3. *`installer/test-installer.sh` not re-run.* DECIDED-NOT-TO on katana: its engine half writes real loop devices as
   root (installer.md "Decided not to"); CI runs it; its GUI half passed 24/24 (`artifacts/verify-installer2/gui-half/`).
4. *Encrypted install with a non-US unlock keymap refused; lift once rime-boot passes `rd.vconsole.keymap`.* FIXED:
   the refusal is gone (installer.md 151); I3's `gb` firstboot proves it.
5. *Rime kernel under SB does not print "Kernel is locked down".* FIXED: the kernel's lockdown posture is gated
   (b23d79080, `docs/nixos/boot-matrix.md`). Hibernation under lockdown: see "Opened by wave 3".
6. *Production live ISO has `root:rime` on VT 2.* DECIDED-NOT-TO: installer.md "Decided not to".
7. *`build-live-iso.sh` runs on the host under sudo.* DECIDED-NOT-TO: installer.md "Decided not to".
8. *Handover list.* See build:installer 1, 3, 4, 5, 8.

## integrate (wave 2)

1. *D2 partial.* See verify:bridge 2.
2. *G3 partial.* See build:g2 1.
3. *P2 partial.* See verify:d5 7.
4. *X4 partial: tampered narHash in a NAR and an unsigned manifest under a signed channel not tested.* FIXED: both are
   `update-e2e` subtests with mutants (f6cf12503, 50d72dddb; p2-gates X4).
5. *X5 partial: no rollback after a refusal.* FIXED: `update-e2e` rolls back after four refusals, with a mutant
   (f6cf12503; p2-gates X5).
6. *feat/nixos not pushed; hosted CI never ran on the merged head.* FIXED: pushed after wave 2; hosted runs
   41400977973 and 41450212626 (`docs/nixos/ci.md`). The wave-3 head is not pushed yet (this integration's task).
7. *nix/ci-hosted-fix not merged.* FIXED: merged (694ed2cb6).
8. *Bridge gaps.* See build:bridge 1, 3, 5; verify:bridge 2, 3.
9. *LUKS install with a non-US keymap refused.* See verify:installer 4.
10. *Release must build every catalogue app.* FIXED: see verify:d5 1.
11. *X4/X5 subtests owed; G3 wiring.* See 4, 5 and build:g2 1.
12. *Every signing row used TEST keys.* P3/HARDWARE: the release MOK, cache key, Fulcio identity and payload key are
    proven only by a real signed release (release environment; p2-gates "P2 status").
13. *Hosted timings of the new VM checks unmeasured.* FIXED: measured in runs 41400977973 / 41450212626
    (`docs/nixos/ci.md`); the kernel checks stay off hosted CI until the Rime cache exists (`nix/ci/checks-not-in-ci.txt`).
14. *proof-final.txt taken one commit before HEAD.* DECIDED-NOT-TO: docs-only difference, outPaths re-checked equal.
15. *os-release `ID=nixos` + `VARIANT_ID=rime` not done.* FIXED: adb063a79.
16. *Kept on purpose: rb-website, relock, shellro, default.png.* DECIDED-NOT-TO: still kept (the website rebrand is
    held; not wave-3 keys).

## Opened by wave 3

Items the wave-3 pieces and this integration found and did not close.

| Item | State | Owner / what is needed |
|---|---|---|
| The installer still hardcodes `NIXOS_MIN_GB=19` / `NIXOS_PART_MIN_GB=17` (`installer/rime-install-nixos.sh` 93-98) while the release publishes per-variant floors (`nix/release/floors.nix`: plain 16 / 14, plain-gaming 18 / 16); it has no gaming-floor refusal, does not install the published `plain` / `plain-gaming` toplevel by choice, and does not write `/var/lib/rime/switch/rime-feature@gaming.target.on` | FIXED (nix/installer3) | The ISO stamps `nix/installer/choices.nix` (floors.nix's figures) as `nixos-choices.json`; the engine reads its floors only from it, refuses gaming below 18 / 16 GB by name, installs the release's `plain` or `plain-gaming` toplevel (`--install-variant`) and writes the switch with `steam`; the GUI offers the choice. `checks.installer-choices` (5 mutants killed) and `checks.installer-nixos` (both installs pass the installed system's own `rime trust --gate`); OVMF: `artifacts/installer3/lab/` (installer.md "Plain or gaming"). Verified independently (`artifacts/verify-installer3/`): a mutant recording the wrong system in `applied.json` fails the gaming install's own `rime trust --gate`; the gaming refusal is now also checked for a partition (14.5 GB: refused for gaming, plain dry run passes). |
| Virtualization floors are estimates; the compressed 0.55 branch of the floor maths is unit-tested only | FIXED (wave 4 updates2): all four variants installed and measured in a VM, `nix/release/floors.nix` all `estimated = false`, virtualization floors 16 / 18 GB (closure.md §12, `checks.update-floors` + 4 mutants); the 0.55 branch refused and passed end to end on a btrfs `compress-force=zstd:3` store (`checks.update-variants`, mutants `ratio-one`, `compress-not-forced`) | — |
| The hosted runner's free disk for a full release closure is not measured | FIXED in part (ci2, measured; verification 11-03: the fetched set and the free disk are measured, the outputs of the 762 derivations the release builds are not sized, and the release job's own `df` is not yet seen): same image ubuntu-24.04 20261025.353.1: the release dry run 41534111012 fetches 2422 paths, 17.1 GiB unpacked; 86 GB available on / before cleanup (run 41450212626). Fits with >60 GB to spare (`docs/nixos/updates-and-release.md` §18 "Hosted runner disk"); a full build's own `df` after the stage is printed by its first run | — |
| virtualization check: no explicit `swtpm.enable` assertion (the mutant fails it through behaviour) | FIXED (wave 4, twice: updates2 80893d52f and ci2 a2845933e each added the assertion; the integration keeps ci2's line): `checks.virtualization` asserts `virtualisation.libvirtd.qemu.swtpm.enable` before any behaviour; mutant `no-swtpm` fails with "virtualisation.libvirtd.qemu.swtpm.enable is off" | — |
| `release-standin` not re-run at the deltas head (3 VMs) | FIXED: built at the merged head (`outs-final.txt`) | — |
| Uncompressed kernel modules | DECIDED-NOT-TO now: at the next `kernel.pin` bump (`docs/nixos/delta.md` 306) | kernel owner |
| The mass-rebuild pair is timed VM-free only | DECIDED-NOT-TO: `delta.md` §5.2 says why (the weekly pair is VM-timed) | — |
| A real internet link for the delta/update path | P3/HARDWARE | katana on the real cache |
| Release MOK signature of the NixOS kernel | P3/HARDWARE, blocked on Andre | `nix-kernel-sign.yml` on main or `RIME_NIX_KERNEL_HOST` set; then `nix/probe/ovmf/mok/run.sh realsig` |
| Real PCR 7; `rime luks rebind` succeeding under Secure Boot (T3 path on real firmware) | Split. Under Secure Boot after a real db change: FIXED in the OVMF TPM lab (wave 4, boot2: `run.sh run rebind-sb` 4/4, mutant fails; see "build:boot2"). Real firmware and katana's real PCR 7: P3/HARDWARE | katana |
| Early-microcode loading not measured directly on hardware; ESP prune unit-tested only | Split. ESP prune on a real full ESP: FIXED (`boot-esp-prune`, e94b69c6a, 2 mutants). The early loader finding the separate Intel cpio: FIXED in a VM (mok `split` U4/U5, gdbstub). A revision being APPLIED: P3/HARDWARE (KVM ignores MSR 0x79) | katana: `dmesg` after the first NixOS boot (`docs/nixos/boot-matrix.md` "Early microcode") |
| Bridged machines get no per-vendor `[boot]` microcode choice (both vendors by default) | FIXED (bridge3): `rime-hostlayer carry` writes `[boot] microcode` from `/proc/cpuinfo` `vendor_id`; unit tests + mutants `hostlayer-microcode-dropped`/`-swapped` in `bridge-unit`; OVMF A and L: host.toml `intel`, menu and `/boot` intel only | `docs/nixos/bridge.md`, `artifacts/bridge3/` |
| P3-S1..S4 (Steam), P3-V1 (ChatGPT / Claude Desktop sign-in), P3-E1 (Equibop presence) | P3/HARDWARE | Andre's accounts, katana (`docs/nixos/p3-katana-gates.md`) |
| R14: `nix/redistributable.nix` does not exist; the OpenAI / Anthropic binaries' redistribution licences are unchecked | FIXED (ci2, R14 commit): decided the public cache never carries them; `nix/redistributable.nix` lists chatgpt + claude-desktop as device-built (catalogue `"build": "device"`, `out` = the `.drv`), every other app is published only if its licence allows it; `manifest`/`publish` refuse a closure holding a vendor path. `checks.release-plan` 181/0 (mutants `publish-vendor-apps`, `unlisted` FAIL), `checks.release-unit` (2 tool mutants killed). `docs/nixos/updates-and-release.md` §6.3 step 3. Verification (11-03): the recipe closures carry no vendor bytes (`nix-store -qR` of each device `.drv`: 4658 and 4374 paths, none of the plan's 6 `non_redistributable` paths, no non-`.drv` path named chatgpt/claude/codex); removing the refusal at the top of `cmd_publish` (the one that runs BEFORE the fill) survived every test, since build_manifest refuses only after the upload: new unit test `test_publish_refuses_a_vendor_path_before_anything_is_uploaded` and release-unit mutant "publish uploads before its R14 refusal" (killed, `/var/lab/rime-nix/artifacts/ci2-verify/release-unit-b0e084013.log`) | — |
| R14 follow-ups: a device-built app installed on a VM from the recipe (the vendor fetch needs the internet: not VM-proven); a licence walk of the variants' whole system closures (NVIDIA and steam-unwrapped are `unfreeRedistributable` at the pin, read by hand); the recipe closure is ~4.5k small paths per app on the GitHub cache backend. Verification (11-03): the client reads a device entry's `out` (the `.drv`) as if it were the app, so `rime recover status` (recover.rs, `sys.exists(&outs[app])`) reports a device-built app installed at the release's build once its `.drv` is fetched, even when its build on the machine failed; the outs record should carry the derivation's output path (read from the `.drv`) for that check | STILL OPEN / P3/HARDWARE (vendor fetch) | release owner: first release with a device entry, on katana or a networked VM |
| Hosted run 41549507599 (nix/ci2 at ee8a24c59): `checks.luks-tpm-rebind` failed building rimed, rime-agentd test `a_phone_whose_connection_died_gives_the_desktop_its_size_back` ("the dead connection was not dropped"). Cause: tests in the same binary fork (pty's held spawns park a child pre-execve for >= 300 ms) and the child holds a copy of every fd, so dropping the peer left the socket open and the write succeeded | FIXED (ci2 verification, f3fa4ca0b): the test shuts the peer down before dropping it. Proof: with a fork injected right before the drop, the test FAILS the same way without the shutdown (`/var/lab/rime-nix/artifacts/ci2-verify/agentd-exp-fork-noshut.log`) and passes with it (`agentd-exp-fork-shut.log`); mechanism shown in `agentd-forkrace.txt`. Not changed: a held child still holds the daemon's other fds until it is released (production releases it right after the session is recorded) | — |
| Production NixOS netinstall ISO, a new `.exe` (17 GB floor), rimeos.com's 16 → 17 GB text, a real Windows `stage-image` run | P3/HARDWARE | release environment + Windows lab (installer.md handover) |
| Hibernation under lockdown | DECIDED-NOT-TO here | kernel owner's call (installer.md handover) |
| journald files copy-on-write on btrfs (no nocow rule) | FIXED (nix/installer3) | the engine creates `/var/log/journal`, empty and `chattr +C`, before anything is written; `checks.installer-nixos` reads `lsattr` `C` (mutant: directory not created fails; `chattr` alone dropped survives because nixos-enter's tmpfiles `h ... +C` then sets it); the OVMF lab first boots show `C` and no copy-on-write line (installer2's had two) (installer.md "Polish found at first boot", Results installer3) |
| `desktop` stalled once on the hosted runner (41450212626), not reproduced | FIXED as a mitigation (ci2): diagnosed as plymouthd not answering plymouth-read-write (the console log reproduced line for line by freezing plymouthd); the module now bounds the splash's units and stops plymouthd; `checks.desktop-plymouth-stuck` + mutants `no-bound` (fails as the runner did) and `no-kill` (`docs/nixos/ci.md`, "desktop: a splash that stops answering"). Why plymouthd stopped on the runner: unknown. Verification (11-03): this is a MITIGATION (a bound plus SIGKILL), not a state wait: the consequence is bounded and proven, the cause is not found (follow-up row below); mutant `no-kill` fails only because PID 1 loses the test backdoor after both units time out, not on an assertion | — |
| Why plymouthd stopped answering plymouth-read-write on hosted run 41450212626 (in its log plymouthd started in the initrd at 4.9 s, `plymouth switch root` finished at 5.3 s, read-write started at 10.6 s and never finished; no drm or plymouth error in between) | STILL OPEN | desktop owner: if `rime: plymouthd ... did not answer` ever appears in a hosted or machine journal, keep that boot's full journal and read plymouthd's state then; until then `checks.desktop-plymouth-stuck` proves the boot goes on |
| rime-hostlayer `profile_files` should refuse a profile already managed by configuration | FIXED (ci2, 1eb8fc842): a file a carried profile names that the new system manages is not carried (reported), and a symlinked certificate is followed inside the old root; host tests 30/0, 2 mutants. Verification (11-03, 34aa3b646): a named LINK whose target the new system manages (ca= /etc/openvpn/client/ca.crt -> /etc/ssl/certs/ca-certificates.crt) was reported "the profile reads the new system's copy" while nothing was written at the named path, so the profile's file was missing on the new system; the link is now carried, absolute, to the managed target (test `test_a_profile_naming_a_link_to_a_managed_path_gets_the_link`; without the fix it errors: no link at the named path). Host tests 31/0; `checks.hostlayer` rebuilt at 34aa3b646 rc=0 (`/var/lab/rime-nix/artifacts/ci2-verify/hostlayer-34aa3b646.log`) | — |
| `rime update --download-only` leaves `pending.json`; the `ops.rs` comment wording | FIXED (ci2, beccf4688) wording: `ops.rs` and `rime update --help` now say the download is kept safe from `rime gc` until the next `rime update` installs it. Leaving `pending.json` is the intended behaviour (DECIDED-NOT-TO change it) | — |
| The delta stream test's 60 s deadline | DECIDED-NOT-TO: a hang fails instead of blocking (`delta.md` 355) | — |
| Payload key `rime-bridge-payload-1` | P3/HARDWARE | release environment generates it; a bootc release ships `nixos-bridge-payload.pub` (`docs/nixos/bridge.md`) |
| Plain topology A not re-run on the fixed bridge; the bridge changed again at this integration (223a0758d: every start step after the record rolls back, `check`/`start` read the record) | FIXED (bridge3): OVMF A (plain, `\EFI\fedora\`) and L re-run on the readiness image (#137 f26b97d17, its rime-migrate = this bridge byte for byte minus line 1), SB on, `--rollback-first`, NixOS, ostree, NixOS: both PASS; the bootc verbs refuse after `start` | `artifacts/bridge3/run-{A,L}.log`, `docs/nixos/bridge.md` |
| LUKS recovery key export | DECIDED-NOT-TO | `docs/nixos/bridge.md` 145 |
| katana `\EFI\APEX\grub.cfg` shape | P3/HARDWARE | `rime migrate check --explain` on katana |
| rime-shell PR #49 open (not draft), CI green | DECIDED-NOT-TO here | Andre merges or not |
| Hyprland "updated to 0.57.0" notice at the first NixOS login | FIXED (bridge3): `ecosystem.no_update_news = true` in Rime's appearance.lua; `desktop` subtest + mutant `desktop.mutants.update-news` (fails `bool: false`); the subtest's no-window check also reads the journal (verify-bridge3: in the test VM the window segfaults before `ps` sees it, so the `ps` check alone could not fail; a getoption-skipped mutant now fails on the journal line); OVMF A and L: lastVersion 0.56.2 -> 0.57.0 with no window (after a serial login); verify-bridge3: a first login straight through the greeter (A, SB on, readiness image) also shows no window (firstrun exits before Hyprland starts), and a negative control on that guest (table cut, firstrun masked) shows it, `artifacts/verify-bridge3/greeter-first/` | `docs/nixos/bridge.md` decision |

## build:boot2 (wave 4)

The wave-4 inputs' secureboot problems 3-6 and the open row above. Evidence under `/var/lab/rime-nix/artifacts/boot2/`
(not committed).

1. *The MokManager lab proves enrolment with the bootc kernel and a busybox `mokutil`; it does not prove the NixOS
   installer's own mokutil path.* FIXED: `nix/probe/ovmf/mok/run.sh installer ILAB`. The NixOS installer ISO (the
   installer lab's own build, TEST MOK) installed `plain` on a Microsoft-keys-only varstore with `mokpw` set
   (`secureboot: MOK enrolment queued`); MI1 MokManager driven screen by screen on that disk and varstore; V MokList
   = exactly the TEST MOK; MI2 the installed NixOS, kernel 7.2.9 signed by that TEST MOK, boots to the greeter with
   Secure Boot on. Mutant `RIME_MOK_MUTANT=wrong-password`: FAILS, nothing enrolled. The release signature (real
   MOK): P3/BLOCKED on Andre (`nix-kernel-sign.yml` not on main, no `RIME_NIX_KERNEL_HOST`).
2. *Early microcode: `split` never asserted want_ucode; the early-cpio probe read none and nobody said why; nothing
   shows the loader reading the separate cpio.* FIXED: U1/U2 assert both names in the image GRUB handed the kernel
   and U3 none; the "none" was a rootfs probe, and nixpkgs' cpios carry no directory entries, so the names never
   reach rootfs (`boot-matrix.md` "Early microcode"); U4 walks `find_microcode_in_initrd()` over the gdbstub and it
   returns GenuineIntel.bin (15,228,928 B, first patch header = the file's), U5 (initrd alone) returns nothing.
   5/5 PASS. Applied revision: P3/HARDWARE (KVM ignores the write; katana already runs 0x43b, the blob's newest).
3. *ESP prune-to-fit only unit-tested (mocked free space).* FIXED: `checks.boot-esp-prune` (e94b69c6a), a real
   256 MiB FAT32 ESP filled to the floor; mutants (no prune → ENOSPC; booted generation not protected; newest
   first) FAIL. The piece's version never booted afterwards (the task asked for it); the wave-4 verification added
   the boot: generation 5's entry is kexec'd off the pruned ESP and comes up as generation 5, and a same-size,
   wrong-bytes initrd mutant that passes every size check FAILS it (kernel panic). A separate Secure Boot boot of
   the pruned ESP: DECIDED-NOT-TO (`boot-matrix.md` "ESP budget").
4. *The rebind exit-code branch has no mutant; a successful re-seal under Secure Boot only at T3/P3.* FIXED:
   `checks.luks-tpm-rebind` (af5208599) tests each half of `failed == 0 && still.is_empty()`; the `||`, no-`failed`
   and no-`still` mutants each FAIL it. Re-seal under Secure Boot after a db change: TPM lab `rebind-sb` 4/4 PASS,
   `mutant-rebind-skipped` FAILS S3 and S4. katana's real PCR 7: P3/HARDWARE.
5. *Checks nobody re-ran at the wave-3 head.* FIXED: at the boot2 head every listed check (desktop, fedora-parity,
   hostlayer-katana, libexec, release-unit, security, update-client, update-e2e, gaming, polish, catalogue, ci,
   shell, keymap-luks, luks-boot, boot-loader-scan) has a valid outPath. 14 already were; `release-unit` ("plan ok")
   and `ci` ("ci: all passed") were built here. The doc-only commits do not move any check's drvPath.
   Correction (wave-4 verification): the piece's `release-unit` and `ci` outPaths (`ingip0fd…`, `9v7airhh…`) were
   built from a tree with untracked `__pycache__` directories in `nix/release/` and `nix/probe/ovmf/mok/`
   (gitignored, but `path:.` copies them), so they are not the clean HEAD's. With those removed, `release-unit`
   is `p3qm9pwn…` (valid) and `ci` is `x3w87n5a…`, built by the verification ("ci: all passed"); every other
   listed check has the same outPath with or without them
   (`/var/lab/rime-nix/artifacts/vboot2/{headcheck-clean.txt,build-ci-clean.log}`).
