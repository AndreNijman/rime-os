# Rime on NixOS: integration status (feat/nixos, 2026-11-02)

## Wave 2 (2026-11-02): merged, all 61 checks pass, P2 45 of 50 met

Merged into `feat/nixos` with `--no-ff` from 0545be610, in this order: `nix/rebrand` (b9d083e91; rime-shell's
`nix/rebrand` first, as rime-shell `feat/nixos` 97a42d92, pushed), `nix/installer` (cf3993e05), `nix/bootmatrix`
(af79360c6), `nix/tpmkeymap` (2dee86dd3), `nix/hostgates` (9dbb56894), `nix/g2` (12c1f530a), `nix/bridge` (ef53e8936),
`nix/d5` (28e32bbbb), `nix/flakes` (b1263e396, the Rust-touching ones last). `flake.lock` pins rime-shell 97a42d92 on
GitHub, so no command needs `--override-input` any more. Evidence: `/var/lab/rime-nix/artifacts/integrate-wave2/`.

What the merge needed (each its own commit):

- b40513bab: the host-layer fixtures carried katana's real machine-id and disk UUIDs; replaced with made-up values.
- a34782975: `rime update`'s catalogue pass failed every update when the release built no copy of a requested app
  (a catalogue app named in the profile but missing from the release's outs). It now names the app and leaves the
  catalogue profile as it is; `rime install` still refuses such an app. Test
  `an_app_the_release_names_no_build_of_is_named_not_failed`; mutant (`apps_without_a_build` returns nothing) fails it.
- 43cfec25a: `kernel-vm-signed` (new in bootmatrix) boots the signed Rime kernel, so it joins the other kernel checks
  in `nix/ci/checks-not-in-ci.txt` (re-measured with `nix-store -qR`: six checks reach linux-rime).
- c3bc50699: bootmatrix turned the Rime boot hook on in the shipped profile; `activation`'s QEMU node has no /boot and
  the hook rightly refused it, so A1's switch failed. The test turns the hook off (A1 is about greetd; the hook has its
  own checks).
- fb49ebd59: hostgates removed `rime-vm`'s hostName (host.toml names the machine), which renamed the toplevel
  `nixos-system-unnamed-…` and failed `release-unit`. `system.name = "rime-vm"` restores the name.

Proof on HEAD fb49ebd59:

- `nix flake check path:. -L --keep-going` (no override): run 1 failed only `activation` and `release-unit` (above);
  run 2 **all checks passed!**, EXIT 0 (`flakecheck2.log`). No virtiofsd hang in either run. All **61** checks'
  `outPath`s evaluated and valid in the store (`proof-final.txt`).
- `hostlayer-katana` on katana's real snapshot and runtime share (`artifacts/hostgates/kat-run.sh`): 13 subtests,
  EXIT 0 (`hostlayer-katana.log`); the snapshot's file hashes are the same before and after (f0ad6e4d…).
- cargo (rimed): 3750 passed, 57 self-skipped, 0 failed, 3 ignored, 35 skipped by name.
- Closure (rime-vm toplevel `n4w06x94…-nixos-system-rime-vm-26.11.20261030.1c89083`): **2108 paths, 10,575,430,816 B
  NAR (9.8 GiB)**; wave 1 was 2079 paths, 10,556,067,672 B (+29 paths, +19.4 MB). `checks.size` passes.
- P2 checklist: `docs/nixos/p2-gates.md` "P2 status": 45 met, 5 partial (D2, G3, P2, X4, X5), 0 not met.

Not merged: `nix/ci-hosted-fix` (the hosted-eval `import (path + …)` fix and the scxctl stub flake); it was held until
wave 2 landed and has no verdict yet. Hosted CI on the merged head has not run.

Handover:

- Bridge: the old (bootc) side does not refuse `rime update` once `rime migrate start` has run; the bridge payload is
  not signed; D2's encrypted layout is refused (`boot-partition`), not migrated; the runs are SB off only; katana's real
  `\EFI\APEX\grub.cfg` is unknown (PLAN open question 7).
- Installer: a LUKS install with a non-US keymap is refused; lift it once rime-boot's `rd.vconsole.keymap` is proven
  on the installed `rime` configuration.
- P2: Steam starting an installed game (`-applaunch`) needs an account and the network; real GPU is the P3 gate.
- X4/X5: two `update-e2e` subtests owed (tampered narHash in a NAR, unsigned manifest under a signed channel; rollback
  after a refusal). G3: deltas are not wired into `rime update` (P4).
- Every signing row used TEST keys; the release MOK, cache key and Fulcio identity are proven only by a real release.
- CI timings of the new VM checks on hosted runners are not measured.


## Wave 1 (2026-11-02): merged, all 30 checks pass

The eleven wave-1 branches (`nix/ci`, `nix/size`, `nix/ledger`, `nix/kernsign`, `nix/boothook`, `nix/catalogue`,
`nix/hostlayer`, `nix/polish`, `nix/update-e2e` with `nix/release` and `nix/client` inside it) are merged into
`feat/nixos` with `--no-ff`; rime-shell's `nix/polish` is merged into rime-shell `feat/nixos` (89325393, local). The
full record, the fixes the merge needed and the P1 gate line by line are in `docs/nixos/p1-exit.md` ("Wave-1
integration"); evidence in `/var/lab/rime-nix/artifacts/integrate-wave1/`.

- **30 of 30 flake checks built** at 53210926a (`nix eval` of every `checks.x86_64-linux.<name>.outPath`, each valid in
  the store: `proof-final.txt`), with `--override-input rime-shell path:/work/shell` until rime-shell is pushed and
  flake.lock relocked. `update-client` and `update-e2e` each failed twice before passing alone (a nixpkgs virtiofsd race, p1-exit.md).
- `hostlayer-katana` passed against katana's real sanitised state (six subtests, snapshot digest unchanged).
- cargo: 3740 passed, 57 self-skipped, 0 failed, 3 ignored, 35 skipped by name.
- Closure (rime-vm toplevel): **2079 paths, 10,556,067,672 B NAR** (`checks.size`, budget 11,274,289,152 B). The 1.51 GB
  of cuts (`docs/nixos/closure.md` §3) are in; linux-firmware (+825 MiB) came with fedora-parity (§9). Fresh-install
  download (`plain`, zstd -19) **3.46 GB**, bootc 4.20 GB.
- Ported in wave 1, each with its own check: package catalogue (`catalogue`, `catalogue-data`), trust and update
  path, D6 client and release side (`update-client`, `update-e2e`, `release-unit`, `release-standin`,
  `release-github`), kernel MOK signing (`kernel-sign`, `kernel-sign-workflow`; secrets not created), the boot hook for
  Fedora shim and GRUB (`boot-hook-unit`, `boot-hook-vm`), the host layer and /etc carry (`hostlayer`,
  `hostlayer-katana`), Fedora defaults (`fedora-parity`), polish (`polish`), the CI workflow and plan (`ci`,
  `ci-userns`), the size guard (`size`).
- Not done: hosted CI has never run; the Rime binary cache, kernel-signing secrets and `katana-nix` runners do not
  exist; `docs/nixos/subsystem-owners.md` §6 still lists unowned rows.

The sections below are the wave-0 record (seven area branches, 2026-11-01). Rows in "Stubbed" that wave 1 closed say so.


The seven area branches (`nix/rimed`, `nix/libexec`, `nix/desktop`, `nix/shell`, `nix/kernel`, `nix/gaming`,
`nix/security`) are merged into `feat/nixos` with `--no-ff`, and the branches and their worktrees are deleted.
Nothing here is pushed and nothing reaches rime-os main (STATE.md). bootc stays the product until the pivot ships.

Every command runs in the `rime-nix` container (the host has no Nix, QEMU or OVMF):

```
O='--override-input rime-shell path:/work/shell'
sudo -n podman exec -w /work/os rime-nix nix flake check path:. -L --keep-going $O
```

## Closure

`nixosConfigurations.rime-vm.config.system.build.toplevel` (every area on, nixpkgs' kernel, as `nix/hosts/vm.nix`
has it) = `/nix/store/883q7lvj…-nixos-system-rime-vm-26.11.20261030.1c89083`.
`nix path-info -Sh` gave **10.3 GiB**, in 1,995 store paths (wave 0; see Wave 1 above for today's figure). That figure is uncompressed and includes the whole
closure, so it does not compare directly with bootc's compressed image sizes. CLAUDE.md records 5.06 GB after #83 and
4.20 GB for a fresh install after the #123 rechunk; neither was measured here.

These were the largest paths; `nix/size` cut the first four and the registry source (closure.md §3):

- **mbrola-voices, 644 MiB.** speech-dispatcher pulls it in through `/etc`.
- **Two llvm-21 libs (538 + 550 MiB) and two Mesa builds.** These are the 64-bit and 32-bit stacks
  (`enable32Bit`, for Steam).
- **Two JetBrains Mono Nerd Fonts, 3.5.0 and 3.5.1 (228 + 231 MiB).** Version 3.5.0 comes from nixpkgs through
  X11-fonts and 3.5.1 is Rime's own pin.
- **Two webkitgtk ABIs (4.1 and 6.0).**
- **The nixpkgs source, 207 MiB.** It comes in through `/etc/nix/registry.json` (the flake registry pin).

## Ported and proven

These are the runs, with logs in `/var/lab/rime-nix/integrate/`:

1. `flakecheck2` (`nix flake check -L --keep-going`, "running 21 flake checks...") built every check except
   `security`, whose G2 mutant did not reach the test's agent runtime.
2. After that fix, `security` was rebuilt on its own: rc 0, `k88wj5lc…`.
3. `flakecheck3` then printed "running 0 flake checks... all checks passed!" (rc 0). Every result was already
   built, so nothing ran again.

The proof is that each `checks.x86_64-linux.<name>.outPath`, evaluated from HEAD, exists in the store
(`nix path-info`):

| check | result | what it proves |
|---|---|---|
| `vm-boot` | PASS, test script 34.15 s, `/nix/store/78jzzsgj…` | See the next table. |
| `rimed` | PASS | rimed owns `org.rimeos.Rimed1` and starts through its activation file. The `rime` CLI works. rime-secretd keeps its state at 0700. rime-lid ends cleanly on a machine with no lid. The polkit actions, the rescue target, the AI model store and the user units (rime-remoted on, the opt-in units off) are in place. The workspace's cargo tests run in the build. |
| `libexec` | PASS | Every script's interpreter and PATH resolve on the profile, and `--help`/`--self-test` hit no missing command. The sudo rules match through the store path and through `/run/current-system/sw`, and are as narrow as on bootc. The units and the bash/zsh profile snippets work. The build also runs the static audit (`rime-libexec-audit.py`). |
| `desktop` | PASS | rime-greet runs on VT 1. greetd's PAM stack unlocks gnome-keyring, and rime-keyring-login moves a seeded default keyring. A keyboard login reaches Hyprland 0.57 on VT 1. PipeWire, WirePlumber and the portals run. |
| `shell` | PASS | quickshell runs at the contract path `/run/current-system/sw/share/rime-shell`. IPC answers (lockscreen target), Rime.I18n loads, the bar maps as a layer, the QML has no load errors, and `system-auth` PAM accepts the password. |
| `gaming` | PASS | The Gaming Mode entry runs store paths. gamescope, steam and the wrappers resolve, and rime-game-env prints the expected environment. gamemoded drives Rime's game mode. NTSYNC is loaded and open to every user. wheel gets the rtprio/nice limits, and CAP_SYS_NICE behaves at the Steam hand-off. The Shell finds the switch. steamos-session-select switches into Gaming Mode from inside Steam's sandbox. |
| `security` | PASS, `/nix/store/k88wj5lc…` | Gates G1, G2, G4, G5, G6, G7, G8, G9 and G12, each also checked against a mutant that must fail. G2's mutant was refused with "bubblewrap (/usr/bin/bwrap) is not installed". |
| `kernel-config-fresh` | PASS | `nix/pkgs/kernel/config` equals what `config.nix` generates from the pin. |
| `kernel-gate` | PASS | The CONFIG_PAHOLE_VERSION check passes and both BTF readers agree on the built vmlinux. |
| `kernel-vm` | PASS | The Rime kernel 7.2.9-cachyos1-rime1 boots. sched_ext is present. ntsync, msi_wmi_platform, xpadneo and xone are signed and verified (no E taint). msi-ec comes from updates/. The NVIDIA open module is 615.78.08. |
| `kernel-boot-budget` | PASS | The boot image plus initrd stay under 130 MiB, and the initrd holds no GPU firmware. This uses a stand-in host config; see Stubbed. |
| `eval` | PASS | `rime-vm` evaluates with every area on. |

### vm-boot (`nix/tests/vm-boot.nix`): the full system test

The node is `nix/hosts/vm.nix` (desktop, services, gaming, security and catalogue all on) plus
`rime.kernel.enable = true` with `nvidia.enable = false` (QEMU has no NVIDIA GPU).
The run that passed is `/var/lab/rime-nix/integrate/vmboot.log`.

| subtest | output seen |
|---|---|
| Every area is on and owns its files. | `uname -r` = `7.2.9-cachyos1-rime1`. `/sys/kernel/sched_ext` is present. On the profile, `rime-session-select` comes from `rime-gaming-switch`, `rime-firewall` from `rime-firewall-0-unstable` (security), and `session.lua` from `rime-desktop-data`. `rime-shell-autostart` and `rime-greetd` come from `rime-libexec`. The Gaming session is offered. |
| greetd one-shot session switch | The greetd unit has `EnvironmentFile=-/run/rime-greet/greetd.env`, `ExecStartPre=-…/libexec/rime-greetd` and `ExecStart=… $RIME_GREETD_ARGS`, and `/etc/greetd/config.toml` exists. |
| Boots to rime-greet on tty1 | sway and `qs -p …/rime-greet/shell.qml` run as user greetd. tty1 is active, and the greeter's logind session is VTNr=1, Class=greeter. Screenshot: greeter. |
| Test user logs in through greetd into Rime Tiling | The password is typed at the greeter (the account's real password). The Hyprland IPC socket appears. The `rime` session is VTNr=1, Class=user, Service=greetd. `hyprctl version` answers. |
| rime-shell-firstrun seeded the user's config | The unit finished with Result=success. `~/.config/hypr/hyprland.lua` exists, and grep finds no unrendered `@KB_` placeholders in `~/.config/hypr` ("none"). |
| Rime Shell runs the shipped config | `quickshell -c /run/current-system/sw/share/rime-shell` is running. Its environment has `RIME_SHELL_DIR` at the contract path and `QML_IMPORT_PATH=/run/current-system/sw/lib/qt-6/qml`. `libqwebp.so` (qtimageformats) is reachable through its `QT_PLUGIN_PATH`. hypridle runs `-c ~/.config/hypr/hypridle.conf`, whose `lock_cmd` calls this shell's lockscreen IPC. `qs ipc show` lists lockscreen. `hyprctl layers` shows the bar. Screenshot: desktop. |
| rimed on D-Bus | As the user, `busctl --system introspect org.rimeos.Rimed1 /org/rimeos/Rimed1` lists the interface. `rimed.service` is active. `command -v rime` gives `/run/current-system/sw/bin/rime`. |
| NetworkManager | `NetworkManager.service` is active, and `nmcli general status` = `running:connected`. |
| sshd refuses password auth | `sshd -T` shows PasswordAuthentication no and KbdInteractiveAuthentication no, and `ssh-keyscan` gets an ed25519 key. A password login with the account's correct password gave rc=255. |
| rime-firewall | `rime-firewall.service` is active and `nft list table inet rime` prints the table. `firewall.service` and `nftables.service` are not active. |

The screenshots are in `/var/lab/rime-nix/artifacts/`:

- `greeter.png` (sha256 28794ca2…) shows the clock, username, keyboard and password fields, and the "Rime Tiling" session picker.
- `desktop.png` (sha256 f85a8644…) shows Rime Shell's bar (workspaces, "Desktop" title, network, volume, clock and notifications).
  The wallpaper had not painted when the shot was taken, eight seconds after the layers mapped: the background
  is dark and a loading spinner shows in the centre. The test does not check this.

### Integration fixes made while merging (commits on feat/nixos)

- **One owner per file on the system profile.** `system-path` drops collisions silently. rime-libexec no
  longer ships the helpers, units and data that desktop, gaming and security own. base.nix no longer writes
  `xdg-terminals.list`; `xdg.terminal-exec` writes it.
- **Gaming session and sudo.** desktop's `sessionPackages` is `mkForce`, so the Gaming session now goes through
  `rime.desktop.extraSessions`. The session switcher and its sudo rule exist on every desktop. The rule names
  the store path and the `/run/current-system/sw` path, because sudo matches the literal command path.
- **greetd one-shot switch ported.** `rime-greetd` runs as ExecStartPre, `RIME_GREETD_ARGS` comes from
  `/run/rime-greet/greetd.env`, and the config is `/etc/greetd/config.toml`.
- **The Shell's Qt plugins.** `QT_PLUGIN_PATH` now includes the profile's Qt 6 plugins, and qtimageformats is
  installed, because the product sessions exec the raw libexec `rime-shell-autostart`.
- **Kernel features.** `linuxManualConfig` sets no `features`, so `hardware.graphics.enable32Bit` (Steam) failed
  evaluation with Rime's kernel. `ia32Emulation`, `efiBootStub` and `netfilterRPFilter` are now read from the
  kernel config. They are passthru only, so the kernel derivation is unchanged (`31l7mz9a…-linux-rime-7.2.9`).
- **rime-shell-firstrun failed on the first login.** Store files are mode 0444 and cp keeps that mode. Appending
  to the niri `config.kdl` it had just seeded was refused, so the unit failed. A cp whose arguments name the store
  or the profile now gets `--no-preserve=mode` (`nix/pkgs/rime-libexec.nix` step 4).
- **Security mutants.**
  - G2's mutant is now a rimed with `RIME_PATH_BWRAP=/nonexistent/bwrap`, set through
    `rime.services.rimed.package`, and the test's agent runtime runs that package. The old mutant was a
    `/usr/bin/bwrap` link.
  - The G4/G7 mutant skips its unit tests, because rimed's own `nothing_under_nix_may_be_writable` refuses the
    deliberate mutation.

## Stubbed: TODO(handover)

| item | where | what is missing |
|---|---|---|
| Package catalogue | `nix/modules/packages-catalogue.nix`, `nix/catalogue/` | **Ported in wave 1** (`catalogue`, `catalogue-data`). Open: the D5 translator, `recover.rs` still calls rime-pkg, vendor apps. |
| Trust and update path (§4.4) | `rimed/rime/src/nixos/`, `nix/modules/update.nix`, `nix/release/` | **Ported in wave 1** (`update-client`, `update-e2e`, `release-*`). Open: the cache bucket, Pages and DNS; real keys; a real-network run. |
| Kernel MOK signing job | `nix/pkgs/kernel/`, `.github/workflows/nix-kernel-sign.yml` | **Ported in wave 1** (`kernel-sign`, `kernel-sign-workflow`). Open: the signing secrets do not exist and the workflow has never run; no host sets `require = true`. |
| kernel-boot-budget target | `nix/tests/kernel.nix:101` | It measures a stand-in laptop config. It must point at the shipped host configs, with the real target (ESP or btrfs /boot). |
| Plymouth accent | `nix/modules/branding.nix` | Closed on `rb/surfaces`: the ridge splash (`rime-os`, plane 3 Rime Blue) and 24 `rime-os-accent-NN` themes; `plymouthd.conf` is a real file `rime-plymouth-accent` may rewrite; an initrd unit reads the ESP credential before plymouthd. Proved by `checks.branding-surfaces`. `checks.polish` checks the same mechanism on the full desktop (accent slot, an unset accent restoring `rime-os`). |
| kmscon console | desktop | **Done in wave 1** (polish: Ctrl+Alt+F2 is kmscon; its mutant fails). It owns tty2, so VM tests that read a tty through `/dev/vcs` must turn it off (hostlayer does). |
| Guest sessions | `nix/modules/hostlayer.nix` | **Done in wave 1**: `hostlayer`'s "rime-user makes a disposable guest on NixOS, and its logout wipes it" passes. |
| rime-aid sandbox | `nix/pkgs/rimed.nix:110` | The backend sandbox binds only /usr and /etc, so a backend cannot start until it binds `/nix/store` and `/run/opengl-driver`. |
| Agent sessions and system PATH | rimed / security | **Done in wave 1** (9f72789c3): `security`'s "PATH: a confined session runs a system binary by name" passes. |
| `rime agent enable` | rimed | **Done in wave 1** (27dcc332b): the enable link stays valid across updates. |
| G2 message | rimed sandbox.rs | **Done**: MissingBwrap names `rime_paths::BWRAP`, the path the build runs (a store path on NixOS); pinned by `the_missing_bwrap_message_names_the_path_this_build_runs`. |
| RTL / qttranslations | shell | **Done in wave 1** (72900bf2c): RTL is measured inside the Shell's own quickshell. |
| D1 / D2 | libexec | **Done in wave 1** (polish): the apex unit aliases (mutants fail without them) and the migrated wallpaper path and hypr-migrate are checked. |
| Steam Remote Play ports | gaming / security | Not opened through `programs.steam.remotePlay.openFirewall`, because that option feeds NixOS's own firewall, which G8 keeps off. Whether rime.nft must allow them is the gaming owner's call. |
| Hotspot firewall dispatcher | security | **Done in wave 1** (polish: the dispatcher opens and closes the set; its mutant fails). |
| NVIDIA suspend | kernel | `hardware.nvidia.powerManagement` and the suspend/resume path are not verified (no GPU in QEMU). |
| LSM / IMA parity | kernel | NixOS passes `lsm=landlock,yama,bpf`, so lockdown and IMA are not active at boot, unlike bootc (kernel README, "Known differences"). |
| Hardware-only items | kernel / gaming | msi-ec shift mode, Cooler Boost, charge window, real NVIDIA rendering, gamescope on hardware. All need katana-class hardware, and no VM can prove them. |
| CI builders | rimed | rimed's checkPhase needs user namespaces (bwrap tests) on the builder. |
| Flaky test skips | rimed | `a_stale_socket_from_a_dead_daemon_is_replaced` and one remoted record test are `--skip`ped in the Nix build. |
| Hyprland and UWSM | desktop | `programs.hyprland.withUWSM = true` keeps Hyprland a binary-cache hit. Rime Tiling execs start-hyprland directly. CI cost if the cache misses: a full Hyprland build. |
| flake.lock | flake | **Done in wave 2**: locked to GitHub rime-shell `feat/nixos` 97a42d92; no `--override-input`. |
| rime-shell | rime-shell feat/nixos | The PR needs a `## Release note`. a88d0340 must also be cherry-picked to main. |
| Wallpaper first paint | desktop | **Done in wave 1** (polish: "the first login paints the wallpaper"). |
| thermald | services | **Done in wave 1** (polish: kept out of a VM by a condition; its mutant fails). Not checked on hardware. |

## Dropped as bootc-only

These rime-libexec scripts are not shipped (`passthru.dropped`):

- **rime-pkg:** the RPM/sysext engine. D5 replaces it with Nix.
- **rime-boot-migrate and rime-boot-count:** bootupd, ostree and staged deployments.
- **rime-live-session:** the sysext live layer. NixOS uses switch-to-configuration instead.
- **rime-store-compact:** `/sysroot/ostree`.
- **rime-update-check and rime-update-notice:** D6 replaces them.
- **rime-migrate-from-apex and rime-session-migrate:** a bootc /var rename. A NixOS machine arrives from Rime.
- **rime-vconsole-guard:** NixOS generates vconsole.conf.

These are shipped but not wired, and their owners decide:

- rime-boot-health (D3)
- rime-luks-enroll (installer, D9)
- rime-vm (libvirtd and its RPM install hint)

vpnc is dropped from the NetworkManager plugins.

## Commits on feat/nixos (on top of the skeleton 57d368f3d)

```
33befc110 merge: nix/rimed into feat/nixos
8cfcfa6ce merge: nix/libexec into feat/nixos
a08c33f24 merge: nix/desktop into feat/nixos
34e5159a9 merge: nix/shell into feat/nixos
701a54e2c merge: nix/kernel into feat/nixos
70e5e47e6 merge: nix/gaming into feat/nixos
ca0a65077 merge: nix/security into feat/nixos
5dabd61a4 fix(nix): integrate the ports so every file has one owner
62b40c347 test(nix): boot the whole system and log in through the greeter
3d6e75813 test(nix): run the security mutants against the merged rimed
```
