# Rime on NixOS: integration status (feat/nixos, 2026-11-01)

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
`nix path-info -Sh` gives **10.3 GiB**, in 1,995 store paths. That figure is uncompressed and includes the whole
closure, so it does not compare directly with bootc's compressed image sizes. CLAUDE.md records 5.06 GB after #83 and
4.20 GB for a fresh install after the #123 rechunk; neither was measured here.

These are the largest paths, which a size pass should look at first (not acted on here):

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
| Package catalogue | `nix/modules/packages-catalogue.nix` | The option exists and its config is empty. `rime install` has no Nix catalogue (D5). This includes mapping `rime install steam` to `rime.gaming.enable`. |
| Trust and update path (§4.4) | — | `rime update` and `rime trust` still speak bootc. The D6 client replaces rime-update-check/notice and the live engine. |
| Kernel MOK signing job | `nix/pkgs/kernel/README.md` | `rime.kernel.signing.imageSignature` has nothing to feed it yet. The CI job (sbsign with the `APEX_SB_*` secrets, names unchanged) does not exist. The boot chain (shim+MOK or Lanzaboote) is D3's decision. |
| kernel-boot-budget target | `nix/tests/kernel.nix:101` | It measures a stand-in laptop config. It must point at the shipped host configs, with the real target (ESP or btrfs /boot). |
| Plymouth accent | `nix/modules/desktop.nix:132` | Every machine shows chartreuse. `rime-plymouth-accent` writes a file NixOS generates. The boot chain has to hand the accent to the initrd. |
| kmscon console | desktop | Not ported (bootc #125 is not merged either). |
| Guest sessions | `nix/modules/base.nix:142` | `rime-user` enables `rime-guest-session@<uid>` with `systemctl enable`, which NixOS refuses. It needs a runtime add-wants or a declarative guest list. |
| rime-aid sandbox | `nix/pkgs/rimed.nix:110` | The backend sandbox binds only /usr and /etc, so a backend cannot start until it binds `/nix/store` and `/run/opengl-driver`. |
| Agent sessions and system PATH | rimed / security | The confined session masks `/run` (the security test saw `/run/current-system absent`, `/run/wrappers absent`), so a confined agent session has no system PATH. The sandbox needs NixOS-shaped binds. |
| `rime agent enable` | rimed | It symlinks a store path into the user's units. That path is garbage-collected after an upgrade; it should point at the profile. |
| G2 message | rimed sandbox.rs | MissingBwrap still says `/usr/bin/bwrap`, even when the compiled path is a store path. |
| RTL / qttranslations | shell | Qt's own translations and the RTL check are not wired. |
| D1 / D2 | libexec | The `apex` aliases (D1) and the hypr-migrate and wallpaper tables (D2 bridge) are not verified on NixOS. |
| Steam Remote Play ports | gaming / security | Not opened through `programs.steam.remotePlay.openFirewall`, because that option feeds NixOS's own firewall, which G8 keeps off. Whether rime.nft must allow them is the gaming owner's call. |
| Hotspot firewall dispatcher | security | `50-rime-hotspot-firewall` is installed but no test runs it. |
| NVIDIA suspend | kernel | `hardware.nvidia.powerManagement` and the suspend/resume path are not verified (no GPU in QEMU). |
| LSM / IMA parity | kernel | NixOS passes `lsm=landlock,yama,bpf`, so lockdown and IMA are not active at boot, unlike bootc (kernel README, "Known differences"). |
| Hardware-only items | kernel / gaming | msi-ec shift mode, Cooler Boost, charge window, real NVIDIA rendering, gamescope on hardware. All need katana-class hardware, and no VM can prove them. |
| CI builders | rimed | rimed's checkPhase needs user namespaces (bwrap tests) on the builder. |
| Flaky test skips | rimed | `a_stale_socket_from_a_dead_daemon_is_replaced` and one remoted record test are `--skip`ped in the Nix build. |
| Hyprland and UWSM | desktop | `programs.hyprland.withUWSM = true` keeps Hyprland a binary-cache hit. Rime Tiling execs start-hyprland directly. CI cost if the cache misses: a full Hyprland build. |
| flake.lock | flake | rime-shell is locked to `path:/work/shell`. Every command needs `--override-input`. Relock to GitHub after rime-shell `feat/nixos` is pushed. |
| rime-shell | rime-shell feat/nixos | The PR needs a `## Release note`. a88d0340 must also be cherry-picked to main. |
| Wallpaper first paint | desktop | The desktop screenshot shows a spinner and no wallpaper eight seconds in. Nothing checks for the painted wallpaper. |
| thermald | services | `thermald.service` exits 1 in QEMU (no thermal hardware). It is expected in a VM and not checked on hardware. |

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
- rime-plymouth-accent
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
