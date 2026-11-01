# Subsystem owners: every Containerfile stage → its NixOS owner, PR and test

PLAN.md §4.12: "**P1 exits only when a table maps every Containerfile stage to its NixOS option or package, PR and
test.**" This is that table, written on 2026-11-01 against `feat/nixos` 8b492a5e8 plus this branch (`nix/ledger`).

How it was made: every `# ── … ──` stage header and `FROM` line of the five Containerfiles was listed
(`grep -n '^# ──\|^FROM' Containerfile.*`), and each owner was found by grepping `nix/` for the stage's files, units and
packages (and, where a grep was not enough, by evaluating `nixosConfigurations.rime-vm`). A `file:line` in the owner
column is where the grep hit. Nothing here is from memory.

Column meanings:

- **Owner**: the NixOS module, package or option that carries the stage, or `dropped: <reason>` (bootc-only by design),
  or `unowned: <next step> (<key>)`. Keys are the wave-1 workers (`ci`, `size`, `ledger`, `kernsign`, `boothook`,
  `release`, `client`, `catalogue`, `hostlayer`, `polish`) or `wave 2` (LUKS/TPM/keymap gates, the D9 installer swap,
  the D1 bridge).
- **PR**: no feat/nixos work has a GitHub PR number yet (feat/nixos is pushed, nothing is opened against main before
  the P2 gate). The column names the branch that landed it: `foundation` = the seven ports merged into feat/nixos
  8b492a5e8 (`nix/{rimed,shell,desktop,libexec,kernel,gaming,security}`), `ledger` = this branch.
- **Test**: the `checks.x86_64-linux.<attr>` that exercises it. "bootc suite only" = a `tests/*.sh` suite exists for
  the script itself (run by the libexec package's self-tests or CI) but no NixOS VM check boots it.

## 1. The §4.12 rows

| Subsystem | Today (bootc) | NixOS owner | PR | Test | Status |
|---|---|---|---|---|---|
| Safe graphics session | `rime-safe-graphics` (`Containerfile.base:3476`) | desktop.nix:49-105 (session entry + data), `rime-libexec` ships `rime-safe-graphics` (+ `-terminal`) | foundation | `vm-boot` "every area is on…" (session list), `libexec` (script resolves, `--help`) | owned; no check boots INTO the session: unowned: a VM subtest that starts Safe Graphics and sees the terminal (polish) |
| Bounce watchdog | `rime-session-watchdog` (`Containerfile.base:3947`), counted by the greeter | rime-greet (desktop.nix:113, `/usr/libexec` rewritten to the profile by `fhsToNixos`, desktop.nix:37-46; the greeter calls `…/libexec/rime-session-watchdog record/check`, `files/desktop/rime-greet/GreetContext.qml:263,529`); state dir `/var/lib/rime-greet` (desktop.nix:356) | foundation | `libexec` (resolves), bootc suite `tests/test-rime-session-watchdog.sh` | owned; no NixOS check makes a session die 3 times: unowned: VM subtest (polish) |
| `rime-vconsole-guard` | repairs a hand-edited `/etc/vconsole.conf` | dropped: NixOS generates `vconsole.conf` from `console.*` (rime-libexec.nix:36,91) | foundation | n/a | dropped by design |
| Guest wipe | `rime-guest-{wipe,session}@` (`Containerfile.base:1593`) | base.nix:116-150 (units, `/etc/rime/guest-accounts`, sandbox) | foundation | `libexec` "the libexec units and settings are in place" | partly: `systemctl enable rime-guest-session@<uid>` cannot work on NixOS (base.nix:142-145 TODO(handover)): unowned: boot-time `add-wants --runtime` from `/etc/rime/guest-accounts` (hostlayer) |
| Fast user switching | `rime-switch-greeter@` (`Containerfile.base:1707`) | base.nix:154-170 + sudo rule base.nix:86-90 | foundation | `libexec` (sudo rules, units) | owned; never switched in a VM: unowned: two-user switch subtest (polish) |
| `rime user` | `Containerfile.base:1673` | `rime-libexec` `rime-user` (rime-libexec.nix:154,310); rimed `RIME_USER` (rimed.nix:74) | foundation | `libexec` (`--help`/self-test) | owned |
| Kiosk | `files/system/shared-machine/` kiosk recipe | no hit in `nix/` | — | — | unowned: port the kiosk recipe as an option, or drop it with a release note (polish) |
| Virtualization (`rime-vm`) | qemu, libvirt, OVMF, swtpm, virtiofsd (`Containerfile.base:2651`) | `rime-vm` shipped but not wired (rime-libexec.nix:39,212); `virtualisation.libvirtd.enable = false` (rime-vm eval) | foundation (script only) | `libexec` (script resolves) | unowned: D5 class-1 switch `virtualisation.libvirtd` + swtpm + OVMF behind `rime install vm` (catalogue) |
| Accessibility | Orca (`Containerfile.core:1816`), `rime-screen-reader`, speech-dispatcher | desktop.nix:529-532 (`services.orca`, `services.speechd`, at-spi2-core); `rime-screen-reader` in libexec (rime-libexec.nix:143) | foundation | `libexec` (resolves), `desktop` (greeter AT-SPI bus) | owned |
| Notices: storage health | `Containerfile.base:1511` | base.nix:203-228 (`rime-storage-notice` user unit + timer), rimed `SMARTCTL` (rimed.nix:29,58) | foundation | `libexec` (units in place) | owned |
| Notices: pre-Turing GPU | `Containerfile.base:1540` | base.nix:203-228 (`rime-gpu-notice`), kernel.nix:101 | foundation | `libexec` | owned |
| `rime-hypr-migrate` | 1,190 lines | `rime-libexec` (rime-libexec.nix:131); run by firstrun | foundation | `libexec`, `vm-boot` "rime-shell-firstrun seeded…" | owned |
| `rime-session-migrate` | 856 lines | dropped: APEX→Rime rename on a bootc `/var`; a NixOS machine arrives already renamed (rime-libexec.nix:35,90) | foundation | n/a | dropped by design; the D1 bridge must run `rime-migrate-from-apex` on bootc first (PLAN D2) |
| VPN: sing-box | `Containerfile.core:2073-2080`; polkit `49-rime-singbox.rules` | base.nix:185-200 (unit, never started at boot), security.nix polkit rule | foundation | none boots the VPN toggle | owned; unowned test: Shell VPN toggle starts/stops sing-box under polkit (polish) |
| Bazaar | `Containerfile.core:1515` | desktop.nix:653 (`bazaar` 0.9.1 on the system profile) | foundation | `vm-boot` "every area is on…" (profile) | owned |
| RPMFusion codecs + Mesa freeworld | `Containerfile.core:1525-1604` | nixpkgs `mesa` is built `-Dvideo-codecs=all` (rime-vm eval of `hardware.graphics.package.mesonFlags`); no `ffmpeg`/GStreamer plugin set on the system profile (eval: no hit) | — | — | partly: VA-API codecs same as freeworld; unowned: GStreamer good/bad/ugly/libav + `ffmpeg` on the profile, decided against the closure budget (size) |
| Printing + scanning | `Containerfile.core:1708` | desktop.nix:485-510 (`services.printing`, `hardware.sane` + airscan); ipp-usb in fedora-parity.nix | foundation, ledger | `fedora-parity` (ipp-usb rule) | owned |
| Input methods | `Containerfile.core:1786` | desktop.nix:451-469 (fcitx5 with addons) | foundation | none | owned; unowned test: fcitx5 starts in the session (polish) |
| devstack node/gcc/gh | `Containerfile.core:2033` | no hit: no nodejs/gcc/gh on the profile (eval); `git` only via rime-shell | — | — | unowned: D5 decides (catalogue: dev toolchain as class 2, claude-code via nix-ld + per-user npm) |
| zsh as default | `Containerfile.core:2569` | fedora-parity.nix (`programs.zsh`, `/etc/default/useradd`) | ledger | `fedora-parity` "new accounts get zsh" | owned |
| Stage 5b2 CLI set: starship, zsh plugins, fish, nushell, tmux, modern CLI tools, yazi, zellij | `Containerfile.core:2086-2118` (5b2: `zsh zsh-autosuggestions zsh-syntax-highlighting fish nushell tmux eza bat zoxide fzf ripgrep fd-find`), starship + zellij later in 5b2, yazi from the toolbuilder; `/etc/shells` registers fish, nu and tmux (`:2585-2591`) | profile snippets wired (base.nix:94-107), but NONE of `starship`, `zsh-autosuggestions`, `zsh-syntax-highlighting`, `fish`, `nushell`, `tmux`, `eza`, `bat`, `zoxide`, `fzf`, `ripgrep`, `fd`, `yazi`, `zellij` is on the rime-vm profile (eval of `environment.systemPackages` at ledger HEAD); `/etc/shells` lists only zsh, bash and sh. rime-libexec ships fish/nushell vendor files (rime-libexec.nix:280-300) for shells that are not installed | — | `libexec` "interactive bash and zsh start cleanly" (snippets guard on the binary) | unowned: add the packages (+ `programs.fish.enable`, `environment.shells` for nu and tmux), or drop them with a release note: Containerfile.core:2102-2106 ties them to the "unrestricted and excellent" terminal claim (polish; closure cost: size) |
| Locale | `Containerfile.core:2628` | base.nix:57-58 (`i18n.defaultLocale`) | foundation | none | owned |
| Timezone | `Containerfile.core:2645` (Australia/Perth) | base.nix:57 (`time.timeZone = null`, set at runtime) | foundation | none | different-intended (docs/nixos/fedora-defaults.md); unowned: the installer writes it (wave 2, D9) |
| Captive portal | `Containerfile.base:325` | fedora-parity.nix (`21-rime-connectivity.conf`) | ledger | `fedora-parity` "NetworkManager" | owned; endpoint is Andre's call |
| MT7925 resume | `Containerfile.base:303` | base.nix:113 (`rime-wifi-resume` system-sleep hook) + fedora-parity.nix (`20-rime-wifi-powersave.conf`) | foundation, ledger | `fedora-parity` (powersave=2); hook itself: bootc suite only | owned; real resume needs hardware (P3) |
| Coredump limits | `Containerfile.base:2974` | base.nix:110 (`50-rime-coredump-limits.conf`) | foundation | none | owned |
| Fedora defaults Rime relies on | zram, faillock, crypto-policies, sysctls, udev rules, enabled units | fedora-parity.nix; every item decided in docs/nixos/fedora-defaults.md | ledger | `fedora-parity` | owned |

## 2. Containerfile.core (54 stage headers)

| Line | Stage | Owner | PR | Test |
|---|---|---|---|---|
| 1-62 | header, core-vs-base rule, rebuild policy, layering, base tag `fedora-bootc:45` | dropped: image-layering policy; NixOS has one toplevel (A1 pin in `flake.lock`) | — | `eval` |
| 69, 118 | `toolbuilder` (`FROM fedora-bootc:45`): matugen, awww, yazi built | matugen + awww: nixpkgs on the profile (desktop.nix:630-631, rime-shell.nix:65,117); yazi: unowned (polish, see §1 starship row) | foundation | `shell` (matugen/awww resolve for the shell) |
| 205-225 | kernel tier consumed by digest (`FROM ${RIME_KERNEL_IMAGE}`) | nix/pkgs/kernel (pin.nix, sources.nix, config.nix), kernel.nix | foundation (`nix/kernel`) | `kernel-config-fresh`, `kernel-gate` |
| 227-297 | sched-ext schedulers built at one pin, `scx-builder` | nix/pkgs/kernel/scx.nix, kernel.nix:127-133 | foundation | `kernel-vm` (16 schedulers attach, kill -9) |
| 392-393 | final `FROM fedora-bootc:45` | dropped: NixOS toplevel | — | `eval` |
| 415 | dnf parallel downloads | dropped: build-speed lever for dnf | — | n/a |
| 424-441 | Stage 1: CachyOS kernel swap / Rime's own kernel | kernel.nix (`boot.kernelPackages` = `pkgs.rime.kernel`) | foundation | `kernel-vm`, `vm-boot` (boots the Rime kernel) |
| 540 | serial console opt-in | no hit (`console=ttyS` only in nix/probe/ovmf); unowned: `boot.kernelParams` opt-in, default off as on bootc (boothook) | — | — |
| 580 | Stage 1c: firmware Fedora's weak deps do not pull | fedora-parity.nix (`hardware.enableRedistributableFirmware`); the extra 1c blobs are in `linux-firmware` | ledger | `fedora-parity` "firmware is installed" |
| 638 | Stage 1a2: disk-unlock chain explicit (cryptsetup, systemd-cryptsetup, tpm2) | unowned: NixOS systemd initrd + `boot.initrd.luks` exist in nixpkgs, no Rime config yet (wave 2, LUKS/TPM gates) | — | none |
| 680 | unlock prompt explains itself (dracut hint) | no hit; unowned: unlock hint for the systemd initrd (wave 2, with the keymap gate) | — | none |
| 699 | Stage 1b: sign the kernel with the Rime MOK | kernel.nix:62-87 (module-signing inputs); the kernel image's MOK signature: unowned until kernsign lands (hosted sbsign job) | in progress: kernsign | none yet (kernsign: OVMF SB-on boot of the real kernel) |
| 735 | Stage 1d: out-of-tree modules built and signed | kernel.nix:28-33 (`extraModulePackages`: msi-ec, xone, xpadneo), nix/pkgs/kernel/packages.nix | foundation | `kernel-vm` (modules verify against kmodigest certs, no `E` taint) |
| 965 | Stage 1e: NVIDIA userspace, every image | kernel.nix:196 (`hardware.nvidia.package`), packages.nix:39-42 | foundation | `kernel-vm` (eager loading gated on hardware) |
| 1011 | Stage 1f: NVIDIA boot correctness + initramfs budget | nix/tests/kernel.nix:97-102 | foundation | `kernel-boot-budget` (130 MiB, no firmware-bearing KMS driver in initrd) |
| 1209 | Stage 1g: controller + fan/IRQ policy | rimed (nix/pkgs/rimed.nix, services.nix) + kernel.nix:101-117 (msi-ec/profile wiring) | foundation | `rimed` (rimed up, CLI) |
| 1266 | Stage 2: desktop + greeter stack | desktop.nix:79-130, 318-410 (greetd, rime-greet, sessions, portals). Absent from the profile (eval): `distrobox`, `cage` (bootc's last-resort greeter host), `iw`; `nodejs` is the devstack row | foundation | `desktop`, `vm-boot` |
| 1401 | hyprland-qtutils | desktop.nix:628 (`hyprland-guiutils`) | foundation | `vm-boot` (profile) |
| 1490 | Stage 3: JetBrainsMono Nerd Font | desktop.nix:470-484, 708-709 | foundation | `shell` (no QML load errors) |
| 1503 | Stage 4: scx userspace runtime libraries | nix/pkgs/kernel/scx.nix, kernel.nix:132-133 (scx_loader) | foundation | `kernel-vm` (scx_loader/scxctl over D-Bus) |
| 1515 | Stage 5: Bazaar | desktop.nix:653 | foundation | `vm-boot` (profile) |
| 1525-1538 | Stage 5a-codecs + the Mesa leg | see §1 "RPMFusion codecs"; GStreamer/ffmpeg set unowned (size) | — | none |
| 1604-1619 | Stage 5a-apps: baked userland; the share stack | desktop.nix:611-675 (session tools), 543-567 (Flatpak + Flathub). Partly: absent from the profile (eval): `neovim`, `cifs-utils`, `samba` (smbclient), `parted`, `gptfdisk`, `scrcpy`, `android-tools`, `unrar` (unfree); `smartctl` is only a rimed dependency, not a user command. Unowned: add them or move them to the catalogue with a release note (polish / catalogue) | foundation | `vm-boot` |
| 1708-1736 | Stage 5a-print: printing + scanning; avahi asserted | desktop.nix:485-510; fedora-parity.nix (ipp-usb). Absent: `system-config-printer`, `avahi-tools` (polish) | foundation, ledger | `fedora-parity` (ipp-usb rule) |
| 1786 | Stage 5a-im: input methods | desktop.nix:451-469 | foundation | none (see §1) |
| 1816 | Stage 5a-a11y: screen reader | desktop.nix:529-532 | foundation | `desktop` (AT-SPI bus) |
| 1884-1907 | Stage 5a-laptop + "four more gaps" (bolt, iio-sensor-proxy, switcheroo, …) | desktop.nix:512-527 (bolt, `hardware.sensor.iio`, switcheroo, PPD, thermald, upower, fwupd, geoclue); fedora-parity.nix (switcheroo udev rule, libinput rules). Absent: `NetworkManager-vpnc` (NM plugins are openvpn + openconnect only), `wireguard-tools`, `libinput-utils`, `usbutils`, `pciutils`; `fprintd` decided in docs/nixos/fedora-defaults.md (polish) | foundation, ledger | `fedora-parity` (30-discrete-gpu, libinput rules) |
| 2009 | Stage 5a-fonts: full font stack | desktop.nix:470-484; fedora-parity.nix (Noto as generic families) | foundation, ledger | `fedora-parity` (`fc-match`) |
| 2033 | Stage 5a-devstack | see §1: unowned (catalogue) | — | none |
| 2086 | Stage 5b2: zsh + starship as the interactive shell | fedora-parity.nix (zsh); base.nix:94-107 (snippets); none of the 5b2 packages is on the profile (fish, nushell, tmux, zsh plugins, eza, bat, zoxide, fzf, ripgrep, fd, starship, zellij): unowned, see §1 "Stage 5b2 CLI set" (polish) | ledger, foundation | `fedora-parity`, `libexec` |
| 2158 | Plymouth engine | desktop.nix:130-141, 534-542 (`boot.plymouth`) | foundation | none (the VM boots `quiet`): unowned: splash screenshot (polish) |
| 2184 | efibootmgr for the Windows-boot helper | `rime-libexec` dep (rime-libexec.nix:63,117) | foundation | `libexec` |
| 2196 | systemd-boot + ukify (the bootc pivot's dnf half) | dropped: v1 boots NixOS through Fedora shim + GRUB (D3, A8); systemd-boot is the later step | — | n/a |
| 2240 | boot blessing writes a FAT ESP (dosfstools, mtools) | `rime-libexec` deps (rime-libexec.nix:71-72) for `rime-vm`; ESP writes (`custom.cfg`, BLS) are the boot hook's: unowned until boothook lands | foundation | none yet (boothook) |
| 2278 | `/dev/liveupdate` SELinux type | dropped: no SELinux (A2) | — | n/a |
| 2296 | npm global prefix per-user | no hit (`npmrc` not in `nix/`); unowned: per-user npm prefix with nix-ld (catalogue, D5 claude-code route) | — | none |
| 2346 | third-party Rust tools from the builder | see 69/118 | foundation | `shell` |
| 2416 | system defaults + hardening (os-release, sysctls, …) | security.nix:303-320 (kernel params, sysctls); `system.nixos.variant_id = "rime"` (desktop.nix:364); os-release NAME/PRETTY_NAME are still NixOS's: unowned (release) | foundation | `security` (G5, G12) |
| 2569 | zsh default login shell for new accounts | fedora-parity.nix | ledger | `fedora-parity` |
| 2593 | no unattended OS updates or reboots | `system.autoUpgrade.enable = false` (NixOS default; no hit for `autoUpgrade` in `nix/`); unowned: assert it in the D6 client's VM test (client) | — | none |
| 2628 | default locale | base.nix:58 | foundation | none |
| 2645 | default timezone | base.nix:57 (null) | foundation | different-intended; installer (wave 2) |
| 2659 | build-time leftovers | dropped: no mutable build root in a Nix build | — | n/a |
| 2686 | container lint | dropped: bootc lint; `nix flake check` takes its place | — | `eval` |

## 3. Containerfile.base (61 stage headers)

| Line | Stage | Owner | PR | Test |
|---|---|---|---|---|
| 1-38 | header, the rule, CORE pinned by digest | dropped: layering policy | — | `eval` |
| 53 | `rime-builder` (`FROM ${CORE}`): Rust workspace | nix/pkgs/rimed.nix | foundation (`nix/rimed`) | `rimed` |
| 107 | `rime-i18n-catalogue` | nix/pkgs/rime-shell.nix:5-6,191 (Rime.I18n) | foundation | `shell` "Rime.I18n loaded" |
| 224 | final `FROM ${CORE}` | dropped: NixOS toplevel | — | `eval` |
| 229 | rimed + rime CLI + rime-agentd payload | services.nix:47-70, nix/pkgs/rimed.nix | foundation | `rimed`, `vm-boot` |
| 303 | MT7925 Wi-Fi survives suspend | base.nix:113; fedora-parity.nix (powersave) | foundation, ledger | `fedora-parity` |
| 325 | captive portals | fedora-parity.nix | ledger | `fedora-parity` |
| 357 | agent runtime (rime-agentd) | services.nix:3-20,92 | foundation | `security` G1/G2/G4/G5/G7 |
| 363 | Rime Remote (rime-remoted) | services.nix:92-94; desktop.nix:500 | foundation | `rimed` "user units: rime-remoted enabled…" |
| 375 | rime-secretd | services.nix:23,77; base.nix:29 | foundation | `rimed`, `security` G7 |
| 408 | rime-aid | services.nix:19,101 | foundation | `rimed` "the AI model store" |
| 570 | system config drop-ins (dark mode, terminal, fastfetch, Firefox) | desktop.nix:568-610 | foundation | `desktop` (session) |
| 589 | the terminal choice and its reader | desktop.nix:588-593 (`xdg.terminal-exec`, Alacritty) | foundation | none |
| 861 | Stage 5b2: Rime Shell vendored | nix/pkgs/rime-shell.nix, desktop.nix:676-710 | foundation (`nix/shell`) | `shell`, `vm-boot` |
| 938 | the shell's polkit action | security.nix:93-120 | foundation | `security` G6 |
| 964-1075 | Stage 5b2a/5b2b: Rime.I18n module + catalogue, merge order | nix/pkgs/rime-shell.nix | foundation | `shell` "Rime.I18n loaded" |
| 1114 | Stage 5b3: paths that assumed a writable shell tree | nix/pkgs/rime-shell.nix (store tree, `RIME_SHELL_DIR`) | foundation | `shell` "no QML load errors" |
| 1177 | power menu "Boot into Windows" | base.nix:80-84 (sudo rule), `rime-boot-windows` | foundation | `libexec` (sudo rules) |
| 1192 | §22 boot v2: health gate + recovery notice | `rime-boot-health` shipped, not wired (rime-libexec.nix:37,116); unowned: health gate on the GRUB chain (boothook, D3) | foundation (script only) | `libexec` (resolves) |
| 1359 | §22 in-place move to systemd-boot | dropped: `rime-boot-migrate` (rime-libexec.nix:28,83); D3 boots NixOS through GRUB | — | n/a |
| 1446 | L-003 disk-encryption key slots | `rime-luks-enroll` shipped, not wired (rime-libexec.nix:38); unowned: TPM re-enrolment keeps the recovery key (wave 2 TPM gates) | foundation (script only) | `libexec` (resolves) |
| 1511 | §48 disk-health warning | base.nix:203-228 | foundation | `libexec` |
| 1540 | unsupported NVIDIA GPU notice | base.nix:203-228 | foundation | `libexec` |
| 1561 | "a new Rime update is available" | dropped: `rime-update-check`/`-notice` (rime-libexec.nix:32-33); the D6 client's notice is unowned (client) | — | none |
| 1593 | shared-machine recipes: guest + kiosk | base.nix:116-150 (guest); kiosk unowned | foundation | see §1 |
| 1673 | `rime user` | rime-libexec | foundation | `libexec` |
| 1707 | fast user switching | base.nix:154-170 | foundation | `libexec` |
| 1899 | Stage 5c: per-user provisioner (firstrun) | base.nix:232-238 (`rime-shell-firstrun.service`) | foundation | `vm-boot` "rime-shell-firstrun seeded…" |
| 2136 | Stage 5d: user packages (sysext) | dropped: `rime-pkg` (rime-libexec.nix:27); nix/modules/packages-catalogue.nix is a skeleton (`rime.catalogue.enable`, empty config); D5 catalogue + translator unowned (catalogue) | — | none |
| 2160 | §27 image self-verification (skopeo/openssl) | rimed trust code (rimed.nix:90-95 inputs) for bootc images; NixOS manifest + narinfo verification (D6) unowned (release, client) | foundation (inputs) | none |
| 2272 | store compactor | dropped: `rime-store-compact` (rime-libexec.nix:31); NixOS generation retention (D6; `nix.gc.automatic = false` today) unowned (client) | — | none |
| 2292 | Rime Capsules (§8) | no hit for `capsule` in `nix/` modules; `virtualisation.podman.enable = false` (rime-vm eval); capsule runtime (podman/CDI) unowned (catalogue, D5 "capsule only with `--source capsule`") | — | none |
| 2356 | disposable environments (§19) | base.nix:128-138; `rime-env`, `rime-disposable` in libexec | foundation | `libexec` |
| 2365 | default incoming firewall | security.nix:195-259 | foundation | `security` G8, `vm-boot` |
| 2491 | lid-closed continuous operation | services.nix:62-67 (`rime-lid`) | foundation | `rimed` "rime-lid: enabled…" |
| 2571 | what is attached (`rime-devices`) | rime-libexec (rime-libexec.nix:121), kernel.nix:176-179, security.nix:214 | foundation | `libexec` |
| 2651 | virtualization engine (P2-008) | see §1 | — | unowned (catalogue) |
| 2717 | P2-012 browser automation capsule (`rime browser`) | `rime-browser` in libexec (deps python3) | foundation | `libexec` (resolves); no VM run: unowned (polish) |
| 2812 | plugin CLI (§16) | `rime-plugin` in libexec (jq, nodejs-slim) | foundation | `libexec` |
| 2879 | console setup must not report the machine broken | dropped: `rime-vconsole-guard` (rime-libexec.nix:36) | — | n/a |
| 2925 | text consoles are kmscon | not ported: base.nix:157-158 only orders against `kmsconvt@`; `services.kmscon` not set anywhere in `nix/`; kmscon-or-getty decision unowned (polish) | — | none |
| 2974 | crash loop must not fill `/var` | base.nix:110 | foundation | none |
| 3349 | Hyprland pins + Secret portal | desktop.nix:410-440 (portals), Hyprland from nixpkgs | foundation | `desktop` (portals run) |
| 3476 | Rime Safe Graphics | desktop.nix:49-105 | foundation | see §1 |
| 3583 | Stage 6: Flathub remote + greeter payload + config | desktop.nix:543-567, 113-129 | foundation | `desktop`, `vm-boot` |
| 3597 | Zen Browser, shipped not imposed | desktop.nix:179 (data only); no `zen` package on the profile (eval); package or catalogue entry unowned (catalogue) | foundation | none |
| 3638 | window switcher keybind side | desktop.nix:524,681 (rime-switcher, Hyprland binds via rime-os config) | foundation | `shell` |
| 3748-3859 | login unlocks the keyring; one Secret Service; keyring migration | desktop.nix:142-164, 391-409 | foundation | `desktop` "the login password opened the keyring", "rime-keyring-login moves…" |
| 3895 | the login screen's session bus | `rime-greet-session` in libexec (dbus, glib), greetd command (desktop.nix:318-332) | foundation | `desktop` "boots to rime-greet on VT 1" |
| 3947 | bounce watchdog | see §1 | foundation | see §1 |
| 4015 | greeter wallpaper is the user's own | base.nix:122, desktop.nix:356-362 (`/var/lib/rime-greet/wallpapers`), `rime-greet-wallpaper` sudo rule base.nix:86 | foundation | `desktop` "greeter state…" |
| 4131 | Stage 6y: first boot after APEX-OS | dropped: `rime-migrate-from-apex` (rime-libexec.nix:34); runs on bootc before the D1 bridge copies anything (PLAN D2); the bridge's refusal until it has finished is unowned (wave 2, D1) | — | none |
| 4185 | Stage 6z: rimed, rime, agentd, secretd, aid units | services.nix:71-110 | foundation | `rimed`, `security` |
| 4358 | Stage 7: bootc lint | dropped | — | `eval` |

## 4. Containerfile.rime (8 stage headers)

| Line | Stage | Owner | PR | Test |
|---|---|---|---|---|
| 1-57 | header, `FROM ${BASE}` | dropped: layering | — | `eval` |
| 59 | edition stamp | `system.nixos.variant_id = "rime"` (desktop.nix:364) | foundation | none; branding: unowned (release) |
| 73 | Gaming Mode session without the payload | gaming.nix (session, switch at :281) | foundation (`nix/gaming`) | `gaming` |
| 223 | the image carries its GPU drivers (assertion) | nix/pkgs/kernel/packages.nix:41 | foundation | `kernel-vm` |
| 243 | NTSYNC | kernel.nix:120-124,230-231 | foundation | `kernel-vm`, `gaming` "NTSYNC is loaded…" |
| 269 | Plymouth boot splash | desktop.nix:130-141 | foundation | none (see 2158) |
| 399 | the APEX-OS names kept working | `apex` shim in libexec (rime-libexec.nix:158,319), base.nix:72 | foundation | `libexec` | 
| 443 | the release this image is (rimeos.com §7.6) | no NixOS release identity yet; release id in the manifest + os-release unowned (release) | — | none |

## 5. Containerfile.kernel (9 stage headers) and Containerfile.release (1)

| Line | Stage | Owner | PR | Test |
|---|---|---|---|---|
| kernel 56-59 | the gate, built from this repo's reader | nix/pkgs/kernel/gate.nix | foundation | `kernel-gate` |
| kernel 70-71 | the kernel (`FROM fedora@sha256:…`) | nix/pkgs/kernel/default.nix | foundation | `kernel-gate`, `kernel-vm` |
| kernel 96 | pinned toolchain | nixpkgs pin (A1); pahole checked by the gate | foundation | `kernel-gate` (pahole 132) |
| kernel 118 | sources verified against the pin | nix/pkgs/kernel/sources.nix + pin.nix (fetchurl sha256) | foundation | `kernel-config-fresh` |
| kernel 136 | build, gate, collect | nix/pkgs/kernel/default.nix + gate.nix | foundation | `kernel-gate` |
| kernel 330-333 | what core consumes (`FROM scratch`) | dropped: no image tier; kernel.nix consumes the derivation | — | `eval` |
| release 4 | release image (`FROM ${PLATFORM}`) | D6 manifest job (design: docs/nixos/updates-and-release.md); unowned until the release worker lands (release) | in progress: release | none yet (`update-e2e`) |

## 6. What is left unowned (P1 exit blockers)

Counted from the tables above. Each needs an owner before §4.12 is "complete with an owner per row":

| Next step | Key |
|---|---|
| kiosk recipe (port or drop with a release note); kmscon or getty; the stage 5b2 CLI set (starship, zsh plugins, fish, nushell, tmux, eza, bat, zoxide, fzf, ripgrep, fd, yazi, zellij, and `/etc/shells` for fish/nu/tmux); the rest of the bootc package lists absent from the profile (neovim, cifs-utils, samba, parted, gptfdisk, scrcpy, android-tools, unrar, distrobox, iw, system-config-printer, avahi-tools, NetworkManager-vpnc, wireguard-tools, libinput-utils, usbutils, pciutils): add or drop with a release note, with `size` for the closure cost; serial console karg is the boothook's | polish |
| VM subtests for: Safe Graphics session, bounce watchdog (3 dead sessions → Safe Graphics preselected), fast user switch, sing-box toggle, fcitx5, Plymouth splash | polish |
| guest session enable on NixOS (runtime `add-wants`) | hostlayer |
| virtualization class-1 switch (libvirtd, swtpm, OVMF); devstack; npm prefix + nix-ld; capsule runtime (podman/CDI); Zen; user packages | catalogue |
| GStreamer/ffmpeg codec set against the closure budget | size |
| boot health gate on GRUB; ESP writes (`custom.cfg`, BLS); serial console karg | boothook |
| MOK-signed real kernel in OVMF | kernsign |
| os-release branding + release identity; manifest verification | release |
| update notice, generation retention (`nix.gc`), no-unattended-update assertion | client |
| LUKS initrd + unlock hint, TPM re-enrolment, installer timezone, APEX state rename before the bridge | wave 2 |
