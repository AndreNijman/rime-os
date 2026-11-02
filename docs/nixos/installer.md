# The NixOS installer (P2, D9)

The installer swap of the NixOS pivot: the same Fedora 45 live ISO, the same GUI and the same engine
(`installer/rime-install`), which on an ISO built with a NixOS system installs that prebuilt system instead of running
bootc. Gates I1-I3 in `p2-gates.md`.

## D9 as taken: the Fedora live ISO stays

The live environment keeps Fedora's Microsoft-signed shim and GRUB. That is the only boot chain a fresh machine with
Secure Boot on and Microsoft's keys alone (no MOK yet) will run, so it is the only way I1 can pass. A NixOS-kernel live
ISO would need a MOK-signed kernel, and shim refuses one when no MOK is enrolled. What changes is the payload and the
engine's install half; the GUI, the answers file, the refusals before erasing and the MOK enrolment flow are the bootc
installer's own.

## Two kinds of NixOS ISO

`NIXOS_SYSTEM=/nix/store/...-nixos-system-...` switches `installer/build-live-iso.sh` to NixOS mode. The bootc build is
unchanged when it is unset.

* **Production (the default): a network install.** No `NIXOS_CACHE_DIR`: the ISO carries no packages, only the stamps
  below, and downloads the system while it installs (`NETINSTALL=1`, the bootc netinstall's shape). The caches and keys
  come from `installer/nixos-production.env`: `https://cache.rimeos.com` with
  `rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=` for Rime's own paths, `https://cache.nixos.org` with
  `cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=` for the paths the release says come from upstream,
  and the release repository `ghcr.io/andrenijman/rime-os`. These are the values an installed system's `rime update`
  uses (`nix/modules/update.nix`, `rime.update.*`); `checks.installer-production-defaults` fails when the two disagree
  (mutant: the env file naming `cache.example.com` fails it, `artifacts/installer2/mutants/production-defaults-url.log`).
* **Lab (offline).** `NIXOS_CACHE_DIR` embeds a signed binary cache holding the whole closure as `nix-cache/` on the
  ISO (stamp `nixos-cache = iso`). This is how the OVMF lab runs without a public cache. It is never a release build.

Either kind is built from a release: `NIXOS_RELEASE_DIR` (or `NIXOS_RELEASE_REF`, fetched with skopeo) is the signed
release record, and the build refuses a system that is not that release's `NIXOS_VARIANT` toplevel (the same check the
engine repeats on the target machine).

| input | stamp in `/usr/lib/rime-installer/` | used for |
|---|---|---|
| `NIXOS_SYSTEM` | `nixos-system` | the prebuilt toplevel to install (nothing is evaluated on the target) |
| `NIXOS_VARIANT` | `nixos-variant` | which of the release's variants this ISO installs |
| `NIXOS_RELEASE_DIR` / `NIXOS_RELEASE_REF` | `nixos-release/` | the release manifest (skopeo `dir:` layout), its cosign `.sig` object, the pinned Fulcio root, the allowed signers and OIDC issuer |
| `NIXOS_CACHE_URL` or `NIXOS_CACHE_DIR` | `nixos-cache` (a URL, or `iso`) | Rime's binary cache |
| `NIXOS_CACHE_PUB` | `nixos-cache.pub` | the only key trusted for that cache |
| `NIXOS_UPSTREAM_URL`, `NIXOS_UPSTREAM_PUB` | `nixos-upstream-cache`, `nixos-upstream-cache.pub` | the upstream cache and its key (absent on a lab ISO whose cache holds everything) |
| `NIXOS_MOK_CERT` | `nixos-mok.der` | the certificate the kernel is signed with, offered for enrolment |
| `NIXOS_KERNEL_SIGNED` | `kernel-signed` | whether to offer enrolment at all (`signed`/`unsigned`) |

The NixOS live env adds `nix-core` (Fedora's Nix), `bootupd` (for its static GRUB config), `openssl` (the release
verifier) and `findutils` (`xargs`). Those are installed only when `Containerfile.installer` is built with
`--build-arg NIXOS=1`; with `NIXOS=0` (every bootc ISO) the build asserts their absence: "OK: bootc ISO: no nix-core,
no bootupd" (`artifacts/installer2/item3-bootc-iso-no-nix.txt`: the bootc installer image is 26.4 MB smaller,
3,395,661,496 against 3,422,076,087 bytes).

## Release check: the installed system is a signed Rime OS release

Before any disk is touched, `installer/rime-install-nixos-release.py check` (run by `nixos_release_check`):

1. verifies the release manifest on the ISO the way `rime update` verifies one (`nix/release/rime_nix_release.py`, the
   port of rimed's `verify.rs`): the cosign signature over the manifest digest, the leaf certificate chained to the
   pinned Fulcio root, its SAN one of the allowed signers, its OIDC issuer; then the release layer read by that digest
   and validated. The verifier is the ISO's own copy;
2. requires the ISO's system to be the release's toplevel for the stamped variant, and every cache key the ISO trusts
   to be one the release names;
3. looks up every path of the closure in the cache its `origin` names, in parallel (32 threads) under a 300 s deadline:
   present, with the release's NarHash, NarSize and references, signed by the trusted key's name.

Then the engine runs `nix store verify` for each cache's own list of paths (`xargs`, no `--recursive`, a 600 s
timeout) with that cache's key as the only trusted key, so a key with the same name and other bytes is refused
cryptographically. A refusal names its cause and leaves every disk untouched. After the install the record lands
where `rime update` keeps its own: `/var/lib/rime/nix/releases/<release>/{manifest.json,digest}` and
`/var/lib/rime/nix/applied.json`.

**Cost, measured (`artifacts/installer2/item1-verify-timing.txt`, katana in Perth to `https://cache.nixos.org`):**

| closure | lookup (parallel narinfo) | `nix store verify` |
|---|---|---|
| 802 paths (eight desktop apps) | 2.16 s at 16 threads, 0.92 s at 64 | 2.18 s recursive, 7.55 s as an explicit list (cold) |
| the lab system, 2,127 paths (1,874 upstream) | 3.52 s at 32 threads (8.14 s on the first cold run), 2.20 s at 64 | 4.3 s and 5.44 s as the engine runs it; the old `--recursive` form 5.96 s and 5.85 s |
| the lab install inside the VM, from the ISO's `file://` cache | 8.69 s, 1.67 s and 8.18 s (`narinfo_seconds`, lab runs 1-3) | the whole check, signature included, 10 s, 2 s and 10 s (run 3: 14:30:00 to 14:30:10 in `lab/rime-install-gb.log`, `nix store verify` 0.92 s) |

The bounds are 300 s for the lookups and 600 s for the verify. Exit 2 from the checker ("a cache did not answer") is
reported as a network problem, not as a forged release.

## What the engine refuses (installer/rime-install-nixos.sh), before anything is erased

* a root filesystem other than btrfs;
* a disk below **19 GB** in whole-disk mode, or a root partition below **17 GB** in partition mode (closure.md A11: a
  fresh system, one update staged beside it and the rollback generation, as `rime update`'s space gate counts them);
* no timezone (the answer, else the live environment's `/etc/localtime`), an invalid locale or kernel parameter;
* a missing system, cache key, release record, release checker or loader file;
* a release record that fails the check above: no record, a manifest changed after it was signed, a release signed by
  another identity, an ISO whose system is not the release's, an ISO trusting a key the release does not name, a
  cache whose copy of the system is not the release's, a cache signed by another key OF THE SAME NAME, a release that
  takes paths upstream on an ISO that names no upstream cache;
* no network route, or no DNS for the cache host (named as such, the bootc netinstall's `net_diagnose`);
* another disk already carrying `RIME-ESP` or `rime-root` (the installed system could boot from the wrong one);
* in partition mode: Windows' own EFI System Partition (it holds Windows Boot Manager), or an ESP without room for
  the release's kernel and initrd (`boot_bytes` from the release, plus 48 MiB); and encryption (the generic
  `rime-install` guard: an encrypted root needs a whole disk).

## What the engine writes

1. **GPT** (whole disk). Partition 1 is a 2 GiB ESP (`RIME-ESP`, FAT32). Partition 2 is a Linux x86-64 root (8304):
   plain, or LUKS2 (argon2id) with a recovery key, the same as the bootc path. In partition mode only the chosen
   partition is wiped; Rime's own ESP is relabelled `RIME-ESP` last.
2. **btrfs `rime-root`** with `@nixos` (/), `@nix` (/nix) and `@home` (/var/home), mounted `compress-force=zstd:3`
   (D2). `nix/installer/target.nix` is the matching NixOS side.
3. **ESP.** Fedora's shim, GRUB and MokManager in `EFI/fedora` and `EFI/BOOT`; bootupd's static config in
   `EFI/*/grub.cfg` and `grub2/grub.cfg`, with `bootuuid.cfg` naming the ESP.
4. **host.toml** (`/var/lib/rime/host.toml`): hostname, timezone, `LANG`, console and XKB keymap, and `[boot]` with
   `layout = "bls"` plus kernel params (`rd.luks.uuid=` when encrypted; `rd.vconsole.keymap=` when the unlock keymap
   differs from the console's; lab arguments through `RIME_EXTRA_KARGS`).
5. **The system.** `nix copy` from each cache its own list of paths, then the system's OWN `nixos-install --system`
   (private mount namespace, the target's /nix). Its activation runs `rime-hostlayer apply` (so `/etc/hostname` is the
   answered name from the first boot) and `switch-to-configuration boot`, whose hook (rime-boot) writes the BLS
   entries GRUB reads.
6. **The release record** (see above), the account (`useradd`/`chpasswd` through `nixos-enter`, wheel and the desktop
   groups) and the network connections.
7. **Secure Boot.** The MOK certificate at `/var/lib/rime/secureboot/rime-mok.der`; `mokutil --import` unless already
   enrolled; an NVRAM entry for shim (in partition mode on Rime's ESP, by partition number). In partition mode a
   "Windows Boot Manager" entry is added to the GRUB menu, and the Windows app's one-shot setup entry is removed.

## Partition mode and the Windows app

The NixOS path installs beside Windows. The Windows app (`windows-installer/`, `rime-windows-installer.exe`) shrinks
nothing itself: it takes free space the user made, writes Rime's own ESP (`RIME-EFI`, holding the six live files and
`rimeinst/handoff.cfg`) plus an empty Linux partition, and a one-shot boot entry into the staged live environment.
From there the engine runs in partition mode with `ESP_IS_LIVE=1`: the live medium IS the ESP it installs onto, so it
is bind-mounted instead of mounted twice. At the first boot `rime-staged-cleanup.service` (`nix/modules/boot.nix`)
removes `rimeinst/` and `EFI/rimeinst/`, as the bootc system does.

**Decision on the .exe:** the app is ISO-agnostic. It pins one ISO by URL, size and SHA-256
(`build-installer-iso.yml` sets `RIME_ISO_*`) and copies `EFI/BOOT/{BOOTX64.EFI,grubx64.efi,mmx64.efi}`,
`images/pxeboot/{vmlinuz,initrd.img}` and `LiveOS/squashfs.img`; the lab NixOS ISO carries all six (checked with
`xorriso -find`), and the NixOS stamps live inside the squashfs. So the same workflow pins a NixOS netinstall ISO with no
change to the app, with two consequences:

* **only a production (network) ISO can be installed from Windows.** The app does not stage `nix-cache/`, so a lab
  ISO's `iso` cache does not exist on a staged ESP;
* **the app's smallest root was 16 GB, below the engine's 17 GB partition floor.** A space the app accepted could be
  refused by the engine after the GPT was rewritten. `MIN_ROOT_BYTES` is now 17 GB (the bootc engine accepts it too),
  with a test at 16.5 GB (`cargo test` 45 passed, `artifacts/installer2/windows-app-cargo-test.log`; with the floor
  back at 16 GB the new test fails, `artifacts/installer2/mutants/windows-app-floor16.log`). The published v3.2.0
  `.exe` and the website still carry 16 GB until the next installer release.

ESP room on a machine installed from Windows: the app sizes Rime's ESP as payload + 512 MiB + 16 MiB rounded up to
256 MiB. For the lab ISO's six files (2,057,417,888 bytes) that is 2,684,354,560 bytes, leaving about 598 MiB beside
the live files; the engine needs the release's `boot_bytes` (81,416,224 for the lab release) plus 48 MiB, about
126 MiB. After the first boot the live files are removed, and the ESP holds only Rime's boot files.

## LUKS and the keyboard

The non-US unlock refusal is gone. tpmkeymap's initrd keymaps (`nix/modules/luks.nix`) carry every console keymap, and
rime-boot appends `rd.vconsole.keymap=` from host.toml's `[keyboard] console`; the engine adds its own only when the
unlock keymap differs from the console keymap (gate M1 is the module-level proof). The installer2 lab proves it end to
end: a `gb` install with the passphrase `rime"lab42` (`"` is Shift+2 on `gb`, Shift+' on `us`) is unlocked by typing
those keys on a `gb` layout, and the same key presses typed as a `us` layout do not open it (the control).

After the unlock the greeter must type the same layout. It did not: greetd's sway took the configuration's XKB
environment (`us`), so the lab's `gb` install showed "English (US)" at the login screen. The greeter command now
starts through a wrapper that reads the keymap generator's output (host.toml's `[keyboard]`) and replaces the
`XKB_DEFAULT_*` variables before sway starts; the run-3 screenshot reads "English (UK)".

## Recovery key

`rime-install` writes the recovery key to the stick it booted from. A stick written from the ISO is ISO 9660 and so
read-only, as is a staged Windows install's live medium, so on those machines the screen was the only copy
(`recovery_unsaved`). The GUI's last page now offers to save it to ANOTHER removable stick:
`installer/rime-install-save-key` refuses (writing nothing) a non-removable device, the disk Rime was just installed on
(any of `RIME-ESP`, `rime-root`, `RIMEROOT`, `RIMEEFI`, `RIME-EFI`), the live medium, a LUKS volume, a device without a
writable filesystem (it never formats) and a mounted one; it writes `rime-recovery-key-<host>.txt`, remounts read-only
and reads the key back. The key is never logged. `checks.installer-nixos` saves the LUKS install's key to a second
virtio disk (`RIME_RECOVERY_ALLOW_FIXED=1`, the test seam for a VM disk that is not removable) and reads it back.

## Polish found at first boot

| seen | cause | now |
|---|---|---|
| `splash` twice on every entry | `desktop.nix` named it and nixpkgs' plymouth module adds it | `desktop.nix` names `quiet` only; `checks.installer-polish-eval` counts it (mutant: both again fails, `artifacts/installer2/mutants/polish-splash-twice.log`) |
| `hostnamectl --static` empty | two causes. The earlier runs had no `/etc/hostname` until the first `rime-hostlayer apply`. This run had the file (PID 1 used it), but NixOS points systemd-hostnamed at `/etc/static/hostname` (`SYSTEMD_ETC_HOSTNAME`, set for when `networking.hostName` makes that link), which does not exist when the closure names no machine | the install's own activation writes `/etc/hostname`, and `hostlayer.nix` points hostnamed at it. A minimal NixOS VM shows `Static hostname: (unset)` before and `rime-lab-x` after (`artifacts/installer2/hostnamed-static-{before,after}.log`, probe `hostnamed-static-probe.flake.nix`). `installer-polish-eval` checks the unit; with the setting dropped it fails (`mutants/polish-hostnamed-static.log`). The lab's firstboot compares `/etc/hostname` and `hostnamectl --static` with the answer |
| zsh-newuser-install on the first console login | NixOS ships no `/etc/skel/.zshrc` | `fedora-parity.nix` ships Fedora's stock one (a real file, 0644), which rime-shell-firstrun replaces at the first desktop login as on Fedora |
| systemd-journald restarts once | see "journald" below | explained, not changed |
| the greeter offers English (US) on a `gb` install | greetd's sway took the configuration's XKB variables, not host.toml's | the greeter reads host.toml's keymap at start (`desktop.nix`); `checks.hostlayer` asserts it, mutant `mutants/hostlayer-greeter-us.log` |

**journald.** The first boot of the `gb` LUKS install (`artifacts/installer2/lab/run1-oldiso/firstboot-gb.json`,
`journald` facts) shows `NRestarts=1` and, in order: the initrd's journald (PID 115) starts at 16.73 s;
`systemd[1]: Switching root.` at 25.00 s; the initrd journal stops; the real root's journald (PID 421) starts at
27.90 s; `systemd-journald[115]: Received SIGTERM from PID 1 (systemd).` and
`systemd-journald.service: Scheduled restart job, restart counter is at 1.` at 27.91 s; then the runtime journal is
flushed to `/var/log/journal`. That is the systemd initrd handing the journal over at switch-root: PID 1 stops the
initrd's journald with SIGTERM and the unit's `Restart=` starts the real one, which the counter records as one restart.
There is no error, core dump or watchdog line; run 3's `lab/firstboot-gb.json` shows the same order (switch-root at
48.72 s, SIGTERM to the initrd's journald and restart counter 1 at 53.54 s). Not changed. Also seen there: journald warns that `/var/log/journal`
is copy-on-write on btrfs (systemd's `journal-nocow.conf` tmpfiles rule would set `+C`); left for the base module's
owner.

## Proof

* `checks.installer-target-eval`: `nixosConfigurations.rime` evaluates with the installer's layout.
* `checks.installer-production-defaults`: the ISO's production defaults are `rime update`'s.
* `checks.installer-polish-eval`: one `splash`, a stock `/etc/skel/.zshrc`, the staged-cleanup unit.
* `checks.installer-nixos`: the engine, `rime-install --headless`, in a NixOS VM standing in for the live env, against a
  TEST release minted by `nix/installer/mint-test-release.py` (a throwaway Fulcio root and signer; `--bad-narhash` for
  the poisoned-cache case). Subtests: every refusal above, each with the disk shown untouched; a whole-disk install
  checked partition by partition, subvolume by subvolume and file by file; the release record; an install over http
  from two caches; LUKS with a German keymap, its `rd.vconsole.keymap`, and the recovery key saved to a second disk; a
  Windows-shaped disk installed in partition mode from the staged live medium, Windows untouched, the setup entry gone.
  Pass at installer2's head: `artifacts/installer2/check-installer-nixos.log`. Mutant: the release signature left
  unchecked (`signer = signers[0]`) installs a release signed by another identity, and the check fails exactly in
  "a release signed by another identity" (`artifacts/installer2/mutants/release-sig-skipped.log`).
* `checks.hostlayer`: among its subtests, the greeter's sway types host.toml's `gb` + `caps:escape`, not the
  configuration's `us` (`artifacts/installer2/check-hostlayer.log`; mutant `mutants/hostlayer-greeter-us.log`).
* The OVMF lab, `nix/installer/lab/run.sh` (results below).

### Results (2026-11-02, katana, nix/installer2)

Run 3 is the current one: the lab ISO built from installer2's head (greeter fix included), sha256 f169cea2775f…,
system `c4is44z0…-nixos-system-unnamed`, a TEST release `2026.11.02` (digest sha256:e8faff499009…, 2,128 paths)
signed by a throwaway TEST signer, cache key `rime-installer-lab-TEST-1`. All evidence under
`/var/lab/rime-nix/artifacts/installer2/lab/`.

| step | result | evidence |
|---|---|---|
| `qualify` (I1) | pass: `uefi-sb` from USB with Microsoft keys only, `secure_boot` true, Fedora kernel 7.2.14-300.fc45, the GUI's first page by OCR | `i1-iso-qualify.log`, `i1-uefi-sb-usb.{json,png,serial.log}` |
| `install gb` (LUKS, XKB `gb`, passphrase `rime"lab42`) | pass. Release check 10 s (narinfo 8.18 s, `nix store verify` 0.92 s); install 14:29:59 to 14:35:54; host.toml `[keyboard] console = "gb"`; `recovery_unsaved` (the ISO stick is read-only, as designed) | `install-gb.{log,serial.log}`, `install-gb-gui.png`, `rime-install-gb.log`, `result-gb.txt` |
| `firstboot gb` (I3, SB on, TEST MOK) | pass: the passphrase typed on a `gb` layout opens the volume (`luks_result` "Switching root"); `rd.vconsole.keymap=gb` on the command line; SecureBoot 1 "enabled (user)"; `/etc/hostname` and `hostnamectl --static` both `rime-lab-gb`; greeter on VT 1 (`Class=greeter`) showing **English (UK)**; no zsh-newuser-install; one `splash`; journald `NRestarts=1` (explained below) | `firstboot-gb.{log,json,png,serial.log}` |
| `firstboot-us-control` (run 2, same engine and passphrase) | FAILS as it must: the same key presses typed on a `us` layout give "Failed to activate with specified passphrase" | `run2-greeter-us/firstboot-gb-us-control.{json,log,serial.log}` |
| `install part` (Windows-shaped disk: Windows ESP, MSR, NTFS `Windows`, the app's `RIME-EFI` + empty 8304) | pass, 14:40:32 to 14:45:03: "grub: Windows entry added (ESP /dev/vda1 …)", "nvram: Rime OS -> /dev/vda part 4 \EFI\fedora\shimx64.efi", "esp: /dev/vda4 is now RIME-ESP" | `install-part.log`, `rime-install-part.log`, `result-part.txt`, `part-disk-before.txt` |
| `firstboot part` (I2, SB on) | pass: firmware `BdsDxe: starting Boot0004` (Rime OS on partition 4, shim), staged files absent, one Windows entry in GRUB. Run 3 kept no copy of the disk after it; the after-install inspection (Windows' ESP still holds its `EFI/Microsoft/Boot/bootmgfw.efi` fixture, NTFS `Windows` intact, `RIME-ESP` without `rimeinst/`, `rime-root` btrfs) is run 2's, from the same install scripts, and `checks.installer-nixos` compares sha256 of Windows' three partitions before and after | `firstboot-part.{log,json,png,serial.log}`; `run2-greeter-us/part-disk-after.txt` (`lab/part-disk-after.txt` is a copy of it) |

The probe's `nvram` fact is empty in `firstboot-part.json` (`efibootmgr` through the probe's `sudo` prints nothing in
that VM); the firmware's own `BdsDxe` line is the evidence that the entry the engine wrote is the one that booted.

History of this lab:

* **Run 1** (`lab/run1-oldiso/`, an ISO from before ab01f061f): `firstboot gb` failed only on the static hostname
  (hostnamed reading `/etc/static/hostname`, fixed in `hostlayer.nix`); `install part` failed "Could not read the EFI
  System Partition /dev/vda4" because the lab's Windows fixture wrote nothing (`put` ignored its offset; `run.sh` now
  checks it and `blkid` verifies the fixture). Its first gb release check: 10 s, narinfo 8.69 s.
* **Run 2** (`lab/run2-greeter-us/`, ISO sha256 d8b63b74…): every step passed (release check 2 s, narinfo 1.67 s),
  but the gb install's greeter screenshot read **English (US)**: greetd's sway took the configuration's `us` keymap,
  not host.toml's, so a `gb` user typed a `us` layout at the greeter. Fixed in 1ffc80fcd (`desktop.nix`'s greeter
  reads the keymap generator's output at start); `checks.hostlayer` asserts `XKB_DEFAULT_LAYOUT=gb` and
  `XKB_DEFAULT_OPTIONS=caps:escape` in the greeter's sway (mutant: the old `us` environment fails,
  `artifacts/installer2/mutants/hostlayer-greeter-us.log`), and `checks.desktop` still passes
  (`artifacts/installer2/check-desktop.log`, the configuration's `us` default where no host.toml names one).
* **Run 3**: the table above; the greeter now reads English (UK).

### Earlier runs

The first lab run (`/var/lab/rime-nix/artifacts/installer/`) and its independent re-run
(`/var/lab/rime-nix/artifacts/installer-verify/`) proved I1-I3 for plain and LUKS installs with the US keymap: the ISO
booting with Secure Boot on and Microsoft's keys only, the plain disk booting (`verdict=booted`), the LUKS disk
unlocking (`verdict=switched-root`), and the plain first boot reaching greetd's greeter. They found five bugs, all fixed
then (kernel parameters refused by a regex that did not survive `grep -E`; `nixos-install` losing its arguments; the
bootc presence check on the NixOS path; the probe's EFI GUID; the probe reading the 0077 ESP as a user) plus the
account's first `sudo` lecture racing the probe. Since the NixOS initrd's journald does not forward PID 1 to the
console, the lab adds `systemd.log_target=kmsg` (lab only, through `RIME_EXTRA_KARGS`) so that "Switching root."
reaches serial.

## Rerun

```
cd /var/lab/rime-nix/<worktree>
export LAB=/var/lab/rime-nix/scratch/<key>/lab ART=/var/lab/rime-nix/artifacts/<key>/lab
for s in prep sign system cache release iso qualify; do bash nix/installer/lab/run.sh $s || break; done
bash nix/installer/lab/run.sh install gb && bash nix/installer/lab/run.sh firstboot gb
bash nix/installer/lab/run.sh firstboot-us-control      # must FAIL: the gb passphrase typed on a us layout
bash nix/installer/lab/run.sh install part && bash nix/installer/lab/run.sh firstboot part
bash nix/installer/lab/run.sh install plain && bash nix/installer/lab/run.sh boot plain   # and luks
bash nix/installer/lab/run.sh clean
sudo -n podman exec -w /work/<worktree> rime-nix nix build path:.#checks.x86_64-linux.installer-nixos -L
```

`LAB` and `ART` default to the first run's directories; pass both.

## Decided not to

* **`root:rime` on VT 2 of the live env.** The live ISO has no sshd; VT 2 is a physical-console operator shell, like
  Fedora's passwordless liveuser. Anyone at the keyboard can already boot anything from USB. The lab drives the engine
  from there. Kept.
* **`build-live-iso.sh` under host `sudo`.** It builds a release ISO on the release builder, as the bootc ISO always
  has (podman build, squashfs, xorriso). Moving it into the Nix container is a build-system change, not an installer
  fix.
* **`installer/test-installer.sh` on katana.** Its engine half runs the real `rime-install` as root on the host with
  real loop devices and names `/dev/sda`/`/dev/sdb`; on katana that is the machine the hard lines protect. CI's runner
  runs it (`pr-validation.yml`); `checks.installer-nixos` runs the same guards in a VM. Its GUI half (every page
  drawn headless in root podman, stubbed engine and disks, nothing on the host's disks) can run on katana: the first
  attempt failed only because the render images could not be built then; run alone at 0f210d2bf it passed 24/24
  (12 pages at 1024x600 and 1366x768), `/var/lab/rime-nix/artifacts/verify-installer2/gui-half/`.

## Handover

* **Release (P3):** the release kernel signed by the real Rime MOK. Only the TEST MOK signature is proven. A person
  clicking through MokManager is not proven either: the boot drivers use a varstore with the TEST MOK already
  enrolled; the install queues the import (`mokutil --import`). Owner: the secureboot worker.
* **Release (P3):** the first production NixOS netinstall ISO, built against a release published by
  `nix-release.yml` and `cache.rimeos.com` serving it; then `build-installer-iso.yml` pins that ISO in a new `.exe`
  release (whose 17 GB floor also updates rimeos.com/download#windows). A real `stage-image` run of the app against a
  NixOS ISO (the Windows lab, `windows-installer/lab`) has not been done; the app's Linux half is proven by
  `installer-nixos` (`ESP_IS_LIVE=1`) and the OVMF partition run (`ESP_IS_LIVE=0`).
* **Kernel owner:** `Lockdown: swapper/0: hibernation is restricted; see man kernel_lockdown.7` on the live
  environment's serial console (Fedora's kernel, every earlier I1 and install log) is that kernel's Secure Boot
  lockdown, not an installer fault. Whether the Rime kernel should allow hibernation under Secure Boot is the kernel
  owner's call.
* **Bridge:** `nix/installer/target.nix` (`rime.installTarget`) and the bridge's `rime.rootfs` declare the same D2
  mounts; they should become one option (bridge2's piece; not edited here).
* **Obsolete:** the merge order with bootmatrix (its luks.nix and tpmkeymap's keymap work are on main now).
