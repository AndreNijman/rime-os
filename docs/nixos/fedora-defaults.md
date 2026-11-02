# Fedora defaults Rime relied on, against the NixOS configuration

PLAN.md §4.12, last row: "diff a bootc VM and a NixOS VM (`sysctl -a`, enabled units, PAM stack, udev rules) and decide
each difference". This file is that diff, done on 2026-11-01 against:

- **Fedora/bootc side:** katana (Rime release 2026.11.01, `fedora-bootc:45`), read-only: `systemctl list-unit-files
  --state=enabled,masked` (system and `--global`), `/usr/lib/systemd/system-preset/*`, `sysctl -a`, `/usr/lib/udev/rules.d`
  + `/etc/udev/rules.d`, `/etc/pam.d/*`, `/usr/lib/NetworkManager/conf.d`, `fc-match`, `findmnt /tmp`, `swapon`,
  `/usr/lib/systemd/zram-generator.conf`, `/etc/default/useradd`, `rpm -qa`. Nothing on katana was changed.
- **NixOS side:** `nixosConfigurations.rime-vm` at feat/nixos 8b492a5e8, evaluated option by option in the `rime-nix`
  container (`nix eval path:.#nixosConfigurations.rime-vm.config…`), plus the built toplevel's `etc/` and
  `etc/udev/rules.d`. rime-vm imports `qemu-vm.nix`, which turns some things off (timesyncd); those were read from a
  hardware-shaped evaluation of the same modules without `qemu-vm.nix`.

Verdicts: **same** (both machines end up with the same behaviour), **different-intended** (differs, and the reason is
written down), **MISSING → added** (now set in `nix/modules/fedora-parity.nix`, proved by
`checks.x86_64-linux.fedora-parity`), **MISSING → recorded** (not added; the reason and the owner are given).

`nix/modules/fedora-parity.nix` sets everything with `mkDefault` behind `rime.fedoraParity.enable` (default true), so a
host or a later area can choose differently without a priority fight.

## MISSING → added

| Item | Fedora / bootc (katana) | NixOS before | Now | Proof in `fedora-parity` |
|---|---|---|---|---|
| Firmware | `linux-firmware`, `amd-ucode-firmware`, `wireless-regdb` installed (Containerfile.core stage 1c adds more on top) | the toplevel had **no** `firmware` directory: `hardware.enableRedistributableFirmware` false. `nixos-generate-config`'s `not-detected.nix` normally sets it; Rime cannot count on that file | `hardware.enableRedistributableFirmware = true` (Intel/MediaTek/Realtek Wi-Fi, Bluetooth, AMD/Intel GPU firmware, regdb). **CPU microcode is not set:** it is per CPU vendor and goes with the host layer (D2) | subtest "firmware is installed": iwlwifi, `mediatek/mt7925/WIFI_RAM_CODE_MT7925*`, `regulatory.db` under `/run/current-system/firmware` |
| zram swap | `zram-generator-defaults`: `zram-size = min(ram, 8192)`, zstd; `swapon` shows `/dev/zram0` | `zramSwap.enable = false`, no swap at all | `zramSwap` zstd, `memoryPercent = 100`, `memoryMax = 8 GiB` | subtest "zram swap": unit, `swapon`, `zramctl` algorithm, size = min(RAM, 8 GiB) ± 64 MiB |
| `/tmp` on tmpfs | `tmp.mount` (systemd) → `tmpfs /tmp`, empty every boot | `/tmp` on the root filesystem, never cleaned (`boot.tmp.useTmpfs = false`, `cleanOnBoot = false`) | `boot.tmp.useTmpfs = true`; nix-daemon builds in `/var/tmp` (`TMPDIR`) so a big build does not fill RAM | subtest "/tmp is a tmpfs" |
| zsh as the default shell for new accounts | Containerfile.core:2569-2591: `/etc/default/useradd` `SHELL=/usr/bin/zsh`; `/etc/zshrc` sources `/etc/profile.d` | `/etc/default/useradd` `SHELL=…/bash`; `programs.zsh.enable = false`, so there was **no `/etc/zshrc`** and base.nix's `programs.zsh.*ShellInit` profile lines (starship, accent highlighting, tool broker) were dead | `programs.zsh.enable = true`; `/etc/default/useradd` `SHELL=/run/current-system/sw/bin/zsh`. Declared accounts keep their declared shell (bootc too: only accounts created afterwards) | subtest "new accounts get zsh": `useradd -m` → zsh, `su -` runs zsh, a declared user keeps bash |
| Rime's NetworkManager drop-ins | Containerfile.base:303-356: `20-rime-wifi-powersave.conf` (`wifi.powersave=2`: MT7925 resume, lid-closed VPN) and `21-rime-connectivity.conf` (captive portals) | not ported (only `rime-wifi-resume` was) | the same two files from `files/system/NetworkManager/`. TODO(handover): the connectivity URI is Fedora's hotspot endpoint; after the pivot that is a party a NixOS machine did not talk to before. Andre picks the endpoint | subtest "NetworkManager": `NetworkManager --print-config` + D-Bus `ConnectivityCheckEnabled = true` |
| NetworkManager's Fedora package defaults | `/usr/lib/NetworkManager/conf.d/22-wifi-mac-addr.conf` (`wifi.cloned-mac-address=stable-ssid`), `24-clat-auto.conf` (`ipv4.clat=1`) | `wifi.macAddress = "preserve"` (the hardware MAC on every network); no CLAT, although nixpkgs builds NetworkManager 1.58 with it | `networking.networkmanager.wifi.macAddress = "stable-ssid"`, `settings."connection-24-clat-auto"."ipv4.clat" = 1` | same subtest |
| fontconfig generic families | `fc-match`: sans-serif → Noto Sans, serif → Noto Serif, monospace → Noto Sans Mono, emoji → Noto Color Emoji | DejaVu for sans/serif/mono (the Noto packages were installed, not preferred) | `fonts.fontconfig.defaultFonts` = the four Noto families | subtest "fontconfig picks Noto" (`fc-match`) |
| switcheroo-control udev rule | `30-discrete-gpu.rules` from the package (tags the dGPU) | the nixpkgs module installs the daemon but not its udev rule, so "launch on the discrete GPU" never finds one | `services.udev.packages = [ switcheroo-control ]` | subtest "udev rules" |
| libinput udev rules | `80-libinput-device-groups.rules`, `90-libinput-fuzz-override.rules` | installed only with X11 (`services.libinput.enable` defaults to `xserver.enable`) | `services.libinput.enable = true` | subtest "udev rules" |
| hyprlock PAM service | the Fedora hyprlock package ships `/etc/pam.d/hyprlock` | hyprlock is on the system profile (desktop.nix) with no PAM service: run, it falls to `other` (deny) = a lock nobody can open. The Shell locks with its own lockscreen, so this is a trap, not a live bug | `security.pam.services.hyprlock = { }` | subtest "hyprlock has a PAM service" |
| ipp-usb | installed (`71-ipp-usb.rules`): driverless USB printers and scanners | absent | `services.ipp-usb.enable = true` | subtest "udev rules" |
| usbmuxd | installed (`39-usbmuxd.rules`, udev-activated): iPhone USB tethering and files | absent | `services.usbmuxd.enable = true`. NixOS's module writes its own Apple rule into `99-local.rules` and starts the daemon at boot instead of on plug-in (different-intended: one idle daemon) | subtest "udev rules": the `05ac` rule + the unit |
| mpris-proxy | bluez's user unit, globally enabled, active in katana's session: headset buttons reach the Shell's media controls | the unit exists but nothing wants it | `systemd.user.services.mpris-proxy.wantedBy = [ "default.target" ]` (when Bluetooth is on) | subtest "mpris-proxy" |
| xfs and btrfs tools for a carried disk | `xfsprogs` and `btrfs-progs` installed (Fedora's default filesystem and the root's): a disk in the old fstab with either is mounted and, with a passno, checked | `boot.supportedFilesystems` had nfs and cifs only; a VM's system had no `fsck.xfs`/`fsck.btrfs`, and the host-gate disk lab made ext4 only | `boot.supportedFilesystems` adds `btrfs` and `xfs` (hostlayer.nix, 11-02) | not in `fedora-parity`: `checks.hostgates-disks` mounts a carried btrfs disk (passno 2; its fsck unit succeeds, but fsck.btrfs reads nothing, so the check scrubs it read-only) and an xfs disk (passno 0) at boot and finds both fsck helpers; mutants `hostgates-disks.mutants.no-xfs` and `hostgates-disks.mutants.btrfs-corrupt` (a corrupted data block: the scrub reports it) FAIL |

Mutant: the same test with `rime.fedoraParity.enable = false` on the node fails (first at "firmware is installed").
Two more mutants show the later subtests can fail too (2026-11-01, logs in the rime-nix scratch dir `verify-ledger/`):
`fonts.fontconfig.defaultFonts.sansSerif = lib.mkForce [ "DejaVu Sans" ]` on the node fails "fontconfig picks Noto"
(`fc-match sans-serif = 'DejaVu Sans'`), and `systemd.user.services.mpris-proxy.wantedBy = lib.mkForce [ ]` fails
"mpris-proxy starts in every user session", the last assertion, after every earlier subtest passed.

## Same

| Item | Fedora / bootc | NixOS |
|---|---|---|
| systemd-oomd | enabled; `oomctl` on katana lists no monitored cgroups | enabled (`systemd.oomd`), same empty policy |
| fstrim | `fstrim.timer` weekly | `services.fstrim` weekly |
| coredump | `core_pattern` = systemd-coredump; Rime's limits Containerfile.base:2974 | same `core_pattern`; Rime's limits in base.nix (`50-rime-coredump-limits.conf`) |
| `vm.max_map_count` | 1048576 (Fedora's `10-map-count.conf`) | 1048576 |
| `kernel.sysrq` | 16 | 16 |
| inotify watches | 558301 (kernel default scaled to RAM) | 524288 (NixOS default); same order, no Rime code depends on it |
| PAM faillock | not in katana's `system-auth` / `password-auth` stacks | not in NixOS's stacks. Neither locks an account after wrong passwords |
| ModemManager, Bluetooth, CUPS (socket), Avahi + `nss-mdns`, fwupd, power-profiles-daemon, thermald, udisks2, upower, rtkit, systemd-pstore | enabled by presets | enabled by NixOS modules (desktop.nix, NetworkManager's ModemManager default); `systemd-pstore` enabled (the `fedora-parity` VM prints it) |
| Firewall | firewalld not installed; Rime's nftables ruleset (`rime-firewall`) | `rime-firewall` only, `networking.firewall.enable = false` (G8) |
| xdg-user-dirs | user unit inactive on katana: its `graphical-session-pre.target` is never reached under Rime's sessions; no `~/.config/user-dirs.dirs` | installed (desktop.nix:646) but nothing runs it: no user unit or autostart entry in the rime-vm toplevel. Same visible behaviour (none). If folders are ever wanted, both sides need the Shell or firstrun to run it |
| irqbalance | masked | not enabled (rimed steers IRQs) |
| Unattended updates | `bootc-fetch-apply-updates.timer` masked (Containerfile.core:2593) | `system.autoUpgrade.enable = false` |
| Locale, keymap | `en_US.UTF-8`, `us` | base.nix `en_US.UTF-8`, `console.keyMap = "us"` |
| Ctrl+Alt+Del | `reboot.target` | `reboot.target` |
| D-Bus | dbus-broker | dbus-broker |
| Wireless regulatory database | `wireless-regdb` | NetworkManager's module enables `hardware.wirelessRegulatoryDatabase` |
| NetworkManager-wait-online | enabled | enabled |
| NetworkManager hotspot (shared mode) | dnsmasq | nixpkgs builds NetworkManager with dnsmasq's path (`-Ddnsmasq=`) |
| WireGuard | NetworkManager's built-in WireGuard | same; `wireguard-tools` (the `wg` CLI) is not on either side's critical path (catalogue if asked for) |

## Different, intended (or decided here)

| Item | Fedora / bootc | NixOS | Why it stays different |
|---|---|---|---|
| Time sync | chronyd | systemd-timesyncd, `nixos.pool.ntp.org` | either keeps the clock; nothing in Rime talks to chrony |
| systemd-resolved | enabled; NetworkManager `dns=systemd-resolved` | off; NetworkManager `dns=default` (`/etc/resolv.conf` written by NM) | `rime.nft` mentions LLMNR in a comment only; the agent sandbox handles both layouts (security.nix). Turning it on is a separate, testable change |
| SSSD | `sssd.service` enabled, inactive (no domain) | absent | no Rime machine joins a directory |
| accounts-daemon | installed | absent | no Rime code calls `org.freedesktop.Accounts` (grep of rimed, rime-shell, files/) |
| smartd | `smartmontools` installed | not enabled | Rime's disk-health warning runs `smartctl` itself (rimed `SMARTCTL` is wired in nix/pkgs/rimed.nix) |
| systemd-homed / userdbd | presets present, unused | absent | Rime accounts are classic `/etc/passwd` accounts |
| lm_sensors, mdmonitor, raid-check, lvm2-monitor | enabled by presets | absent | Rime ships no RAID/LVM layout; sensors are read through hwmon by rimed |
| PAM extras | `pam_pwquality`, `pam_faildelay`, `pam_keyinit` in `system-auth` | not in NixOS's stack | password quality is checked by the installer and `rime user`; the delay/keyring-init modules change nothing a user sees. Revisit with D4 G16 (docs say what changed) |
| crypto-policies | `DEFAULT` system-wide policy | no equivalent; each library's defaults | D4: recorded in the security notes; no Rime code reads the policy |
| `kernel.yama.ptrace_scope`, `kernel.kptr_restrict` | 0, 0 | 1, 1 | security.nix decision (G12: lockdown no worse than today). ptrace_scope 1 still lets a debugger attach to its own children |
| `net.ipv6.conf.*.use_tempaddr` | 0 (NetworkManager sets privacy per connection) | 2 | NixOS's default prefers temporary addresses; NetworkManager's per-connection `ip6-privacy` still decides on managed links |
| Timezone | the image sets `Australia/Perth` (Containerfile.core:2645) | `time.timeZone = null` (base.nix: set at runtime) | TODO(handover) D9: the installer writes the timezone the user picks; a migrated machine carries `/etc/localtime` (D2 host layer) |
| fprintd | installed, PAM not wired to it | not added | the NixOS module wires `fprintAuth` into PAM, which bootc avoided. Owner: polish, if fingerprint login is wanted |
| usb_modeswitch, colord, libgphoto2, libcamera, libwacom, tpm2-tss udev rules, kdump, nvmf / Azure / SCSI / md rules | shipped by Fedora packages | absent | niche hardware or not applicable (no cloud, no kdump). The catalogue (D5 class 1) adds any that a request needs |
| logrotate | `logrotate.timer` enabled | `logrotate.timer` not found in the `fedora-parity` VM: NixOS enables logrotate only when a module configures a log file | Rime ships no logrotate config (grep of files/, rimed/, the Containerfiles); its logs and the system's are in the journal on both |
| rpcbind (port 111) | not enabled; `nfs-utils` installed, so an NFSv3 mount with locking started `rpc-statd`, which pulled `rpcbind.socket` in on demand | **on demand, decided 11-02** (nix/modules/hostlayer.nix): the unit is installed and nothing starts it at boot (`systemd.services.rpcbind.wantedBy = [ ]`); nixpkgs' NFS support would start it at boot on every machine | the bootc behaviour, measured: `checks.hostgates-net` finds no rpcbind and no port 111 at boot, then mounts the peer's export with `vers=3`, takes a lock (NLM), and rpc-statd and rpcbind are running. Until 11-02 rpcbind was turned off outright, which broke exactly that case (mutant `hostgates-net.mutants.rpcbind-off`: the NFSv3 lock FAILS). NFSv4 needs neither |
| cups-browsed | not enabled | enabled by `services.printing` | NixOS default; finds network printers. Kept |
| I/O scheduler | `bfq` on katana's NVMe | kernel default (the `fedora-parity` VM's virtio disk: `mq-deadline`; NVMe not measured) | katana's NVMe `bfq` comes from a **host-local** `/etc/udev/rules.d/60-bfq-nvme.rules`, not the image (see below). systemd's own `60-block-scheduler.rules` (bfq for SATA/MMC, not NVMe) is on katana and not among the NixOS toplevel's rules: TODO(handover) polish, one `services.udev.packages` line if wanted |
| `nix.gc.automatic` | n/a | false | new on NixOS; owned by D6 (generation retention), not a Fedora default |
| bootc-only units | `rpm-ostree-countme`, `akmods`, `ostree-remount`, `bootloader-update`, `selinux-autorelabel`, `authselect`, `flatpak-add-fedora-repos` | absent | belong to the bootc image (A2 drops SELinux; D6 replaces countme and updates; kernel.nix builds modules) |

## Host-local on katana, not image defaults

These differ from the image, so they are katana's own `/etc` and belong to the host layer's `/etc` carry gate (D2,
wave-1 key `hostlayer`), not to the NixOS defaults:

- `/etc/udev/rules.d/60-bfq-nvme.rules` (NVMe → bfq)
- `/etc/udev/rules.d/99-dualsense-touchpad.rules`
- `/etc/sysctl.d/90-desktop-writeback.conf` (`vm.dirty_bytes` 1 GiB, `vm.dirty_background_bytes` 256 MiB)

`vm.swappiness = 100` and `vm.page-cluster = 0` on katana appear in no `sysctl.d` file there: they are the CachyOS
kernel tree's own defaults (`mm/vmscan.c`: `int vm_swappiness = 100;` under `#ifdef CONFIG_CACHY`, `mm/swap.c`
`swap_setup()`: `page_cluster = 0;`, read from `linux-cachyos-7.2.9-2.tar.gz`, the source `nix/pkgs/kernel` builds;
`nix/pkgs/kernel/config:35` has `CONFIG_CACHY=y`). A NixOS machine on the Rime kernel gets the same
values; the `fedora-parity` VM runs nixpkgs' stock kernel and prints `vm.swappiness = 60`, which is that kernel's default.

## Handover

- The firmware closure (`linux-firmware` ≈ several hundred MB compressed) is now in every toplevel: the closure
  measurement (wave-1 key `size`) must be taken with it.
- CPU microcode (`hardware.cpu.{intel,amd}.updateMicrocode`) belongs to the host layer (D2). If it is added, the boot
  hook must carry `boot.initrd.prepend` (the microcode cpio) into the BLS / `custom.cfg` entry.
- The connectivity check endpoint (`21-rime-connectivity.conf`) is Andre's call.
- The installer (D9) writes the timezone.
- `/etc/default/useradd` is set with `mkForce` (fedora-parity.nix) so `useradd` and the installer give new accounts
  zsh while declared accounts keep their declared shell. A host that sets `users.defaultUserShell` changes declared
  accounts only; `useradd` keeps reading this file. Change it here (or turn `rime.fedoraParity.enable` off) instead.
