# rime-boot: the boot hook of Rime on NixOS

PLAN.md D3 (v1 chain), D9 (fresh installs), D2 (migrated machines), decision A8. Program: `rime-boot.py`. Module:
`nix/modules/boot.nix` (`rime.boot.*`). Tests: `nix/tests/boot.nix` (`boot-hook-unit`, `boot-hook-vm`). End-to-end
proof in OVMF with Fedora's signed shim and GRUB: `nix/probe/ovmf/boothook/`.

## The chain

Fedora's Microsoft-signed shim -> Fedora-signed GRUB with bootupd's static config -> the MOK-signed NixOS kernel and
its NixOS initrd. shim and GRUB are put on the disk by the installer (fresh machines) or are bootc's own (migrated
machines). NixOS's GRUB and systemd-boot modules are not used: both install loader binaries (PLAN 4.1).
`boot.loader.external.installHook` is `rime-boot install`, which **runs no program at all** (the package's install
check fails if it names `subprocess`, `exec`, `spawn`, `popen` or `system`): no `bootctl`, `grub-install`,
`grub2-*`, `efibootmgr` or `mokutil`, no NVRAM write, no loader binary written. (It does write one data file on the
ESP, Rime's MOK certificate: see "The way back when Secure Boot refuses the kernel".)

## Layouts

The layout is chosen at run time, because the toplevels are generic (D9) and the layout is a per-machine fact (D2):
`/var/lib/rime/host.toml` `[boot] layout` wins, then `rime.boot.layout`, then `auto` (ostree's `/boot/loader`
symlink or `/boot/ostree` present = `custom-cfg`, else `bls`). `bls` is refused when `/boot/loader` is a symlink:
NixOS entries never go into ostree's `loader.N` (it is swapped on every bootc deploy).

| | `bls` (fresh install; the FAT ESP is `/boot`) | `custom-cfg` (migrated bootc machine; `/boot` is bootc's) |
|---|---|---|
| menu entries | `/boot/loader/entries/rime-generation-<N>.conf`, read by Fedora GRUB's `blscfg` | `menuentry … --id rime-generation-<N>` in `/boot/grub2/rime-nixos.cfg` |
| default, timeout | `/boot/grub2/rime-nixos.cfg` | the same file |
| sourced by | a marked block in `/boot/grub2/custom.cfg` (bootupd's `41_custom.cfg` sources it) | the same |
| kernel, initrd | `/boot/rime/nixos/<sha256[:32]>-<store name>.{efi,img}` | the same |
| never touched | anything not named `rime-generation-*.conf` | `/boot/loader`, `loader.N`, `/boot/ostree`, every byte of custom.cfg outside the block (the bootc installer's Windows entry) |

The path GRUB sees (`/rime/nixos/…` on an ESP, `/boot/rime/nixos/…` on bootc's btrfs) is read from
`/proc/self/mountinfo` (the mount's root within its filesystem, which for btrfs is relative to subvolid 5, as GRUB
reads it), or set with `rime.boot.grubPath`.

## What one run does

1. Generations: every `/nix/var/nix/profiles/system-<N>-link`. Kept: the newest `configurationLimit` (10), the
   profile's **current** generation (the default; `rime rollback` = `nix-env --switch-generation` + the hook), the
   booted one, and every pinned one (`/nix/var/nix/gcroots/rime/pin-<N>`, D6 11.2).
2. Kernel = `realpath(<toplevel>/kernel)`, not bootspec's `kernel`. bootspec names `${kernelPackages.kernel}/bzImage`,
   which is unsigned; `nix/modules/kernel.nix` relinks `<toplevel>/kernel` to the MOK-signed image once the hosted
   signing job's detached signature is set. The hook does not sign and does not require a signature (shim decides);
   it logs `(signed)` / `(no signature)` per generation from the PE certificate table.
3. Floor: the bytes of every kernel/initrd not already on `/boot`, plus `rime.boot.reserveMiB` (32), must be free, or
   the hook exits 3 with `REFUSED: not enough space on /boot: … Nothing was written` before its first write.
4. Writes, in an order a power cut cannot break (D6 8.7): each new file to a temporary name, fsync, rename, fsync the
   directory; then the BLS entries; then `rime-nixos.cfg`; then `custom.cfg` (each tmp + fsync + rename, and only
   if its content changed, so a second run changes nothing); only then removes stale `rime-generation-*.conf` and
   unreferenced files in `/boot/rime/nixos/`.

Kernel parameters with characters GRUB would expand or split (`$`, quotes, `\`, `;`, braces, `|`, `&`, `<`, `>`,
whitespace, the wildcards `*` and `?`, a `#`) are refused by name, not escaped. `host.toml` `kernel_params` must be
a list of strings.

`rime-boot check` (read-only, D6 8.2 step 5) prints JSON: `default_generation`, `default_toplevel`,
`profile_generation`, `missing`, `up_to_date`. Exit 0 = the menu boots the profile's current generation and every file
it names is there; 1 = not (an interrupted update); 2 = error; 3 = refused.

## Recovery from a bad generation (v1): manual, through the menu

The menu is always shown (`timeout_style=menu`, `rime.boot.timeout` = 5 s; bootupd's static config shows it for 1 s)
and lists every kept generation as `Rime (generation N, <date>)`: in `custom-cfg` the default first, then newest
first; in `bls` in blscfg's order (by the entry's `version`, which is the generation number: newest first), with
the default chosen by its id (`set default=rime-generation-N`), so after a rollback it is not the first line. A user
whose new generation does
not boot restarts and picks the previous one; `rime rollback` (or `nix-env --switch-generation` + `rime-boot install`)
then makes it the default. Proven in OVMF for both layouts, Secure Boot on and off (`nix/probe/ovmf/boothook/`).

There is no automatic fallback (boot counting) in v1, as D3 and D9 say. bootupd's static config ships none of
Fedora's `grub.d` counting scripts, and GRUB cannot write its environment block on btrfs, which is where a migrated
machine's `/boot/grub2` lives, so a `grubenv` counter could only work on the BLS layout. D3's later step (MOK-signed
systemd-boot + UKI, `+N` boot counting written by this hook) is where automatic fallback belongs.

## The way back when Secure Boot refuses the kernel

shim refuses a kernel signed by Rime's MOK until that key is enrolled ("bad shim signature"). The installer imports
the key (`mokutil --import` + `mokutil --timeout -1`), but a user who misses shim's prompt on the first boot would be
stuck. So every run of the hook also writes the bootc images' recovery entry, the same bytes as rime-os's
`rime-secureboot-key` helper (between `### BEGIN rime-secureboot-key ###` and `### END rime-secureboot-key ###` in
`custom.cfg`, after this hook's own include block):

- `rime.boot.mokCertificate` (default: `rime.kernel.signing.certificate` when the kernel is signed) is converted to DER
  at build time (openssl in a derivation) and placed at `<ESP>/EFI/rime/rime-mok.der` (written atomically, skipped when
  the same bytes are there).
- The ESP is `/boot` in `bls`; in `custom-cfg` only a mounted FAT `/boot/efi` that is not a Windows ESP (the bootc
  helper's rule). The ESP must hold `EFI/fedora/mmx64.efi` (MokManager), or no entry is written and the log says why.
- The entry `Enrol the Rime OS Secure Boot key (if Rime OS does not start)` (`--id rime-enrol-key`) shows only when
  GRUB's `$shim_lock` is `y` (Secure Boot on), explains the MokManager steps, and after 20 s (Esc goes back)
  chainloads `mmx64.efi`, where "Enroll key from disk" -> the ESP -> EFI -> rime -> rime-mok.der enrols the key.
- An existing block is replaced as a whole (never duplicated), an unterminated one is repaired, every other byte of
  `custom.cfg` is kept (the Windows entry, this hook's include block). If the certificate or MokManager is missing
  the existing block is left as it is.
- No machine-specific value (UUID, hostname) is in the block: GRUB finds the ESP with `search --file`.
- `systemctl start rime-secureboot-key.service` runs the hook again (the block's own advice when the file is gone).

In `bls`, `custom.cfg` is the ESP's `grub2/custom.cfg`, and bootupd's static `grub2/grub.cfg` sources it through
`41_custom.cfg` (`source $prefix/custom.cfg`, `$prefix` = the ESP's `/grub2`). Proven in OVMF (Secure Boot on, key
not enrolled: entry -> MokManager -> enrol -> generation boots; Secure Boot off: no entry):
`nix/probe/ovmf/boothook/run.sh sbrecovery`.

## LUKS / TPM / keymap (wave 2)

Nothing here assumes a plain root. The initrd is NixOS's systemd initrd copied as is (systemd-cryptsetup works in it);
per-machine arguments come from `host.toml` `[boot] kernel_params = ["rd.luks.uuid=…", "rd.luks.options=…",
"rd.vconsole.keymap=…"]` (D2) and are appended to every entry. `boot.loader.external` sets
`supportsInitrdSecrets = false`, so `boot.initrd.secrets` (key files baked into the initrd) is not available; TPM2 and
passphrase unlock do not need it.

The console keymap of the passphrase prompt (PLAN.md D2 keymap gate, R19): when `host.toml` `[keyboard] console`
names a keymap other than `us`, the hook appends `rd.vconsole.keymap=<it>` to every entry (unless `[boot]
kernel_params` already names one). The shared initrd keeps `KEYMAP=us`; its systemd-vconsole-setup loads the named
map from the initrd's keymap tree, which is the generation's `/etc/kbd/keymaps` (nix/modules/keymap.nix adds the XKB
names bootc's installer writes, e.g. `gb`, `ch`). The hook looks the name up in `<default toplevel>/etc/kbd/keymaps`:
missing on a machine that unlocks LUKS at boot (`[[luks]]` in host.toml or an `rd.luks.*`/`luks.*` parameter) is a
refusal by name, since the passphrase would be read with the us layout; missing without LUKS is a warning and no
parameter. checks.keymap-luks is the VM proof (gb and ch passphrases typed as key codes at the initrd prompt).

## Not done here (handover)

- Updating Fedora's signed `shimx64.efi`/`grubx64.efi` on the ESP (SBAT revocations; PLAN 4.1 "shim and GRUB
  updates", D9 "Firmware updates"). This hook writes menus only.
- Kernel signing (worker kernsign): the hook boots whatever `<toplevel>/kernel` is.
- Specialisations are not listed (Rime defines none).
- Which `grub.cfg` katana's `\EFI\APEX\grubx64.efi` loads, and whether it sources `custom.cfg`, is still unverified
  (needs root on katana, D3).
- `host.toml` itself (format and writer) belongs to the host-layer PR; this hook reads only `[boot] layout`,
  `[boot] kernel_params`, `[keyboard] console` and whether `[[luks]]` is present. TODO(handover, host layer):
  rime-hostlayer's `validate_host` does not know `[boot]` yet, so a host.toml carrying it is refused there.
