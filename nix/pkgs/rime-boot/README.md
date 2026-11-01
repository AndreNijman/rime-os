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
`grub2-*`, `efibootmgr` or `mokutil`, no NVRAM write, no loader binary written.

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
whitespace) are refused by name, not escaped.

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

## LUKS / TPM / keymap (wave 2)

Nothing here assumes a plain root. The initrd is NixOS's systemd initrd copied as is (systemd-cryptsetup works in it);
per-machine arguments come from `host.toml` `[boot] kernel_params = ["rd.luks.uuid=…", "rd.luks.options=…",
"rd.vconsole.keymap=…"]` (D2) and are appended to every entry. `boot.loader.external` sets
`supportsInitrdSecrets = false`, so `boot.initrd.secrets` (key files baked into the initrd) is not available; TPM2 and
passphrase unlock do not need it.

## Not done here (handover)

- Updating Fedora's signed `shimx64.efi`/`grubx64.efi` on the ESP (SBAT revocations; PLAN 4.1 "shim and GRUB
  updates", D9 "Firmware updates"). This hook writes menus only.
- Kernel signing (worker kernsign): the hook boots whatever `<toplevel>/kernel` is.
- Specialisations are not listed (Rime defines none).
- Which `grub.cfg` katana's `\EFI\APEX\grubx64.efi` loads, and whether it sources `custom.cfg`, is still unverified
  (needs root on katana, D3).
- `host.toml` itself (format and writer) belongs to the host-layer PR; this hook reads only `[boot] layout` and
  `[boot] kernel_params`.
