# The bridge (`rime migrate`): decisions and what is proven

`nix/pkgs/rime-bridge` (`rime-migrate`) moves a bootc Rime OS machine to Rime OS on NixOS in place (PLAN.md D1-D3,
docs/nixos/p2-gates.md D1, D2, X1, X3, H8, H9). This page records the calls made while finishing it (branch
`nix/bridge2`) and why, so nobody has to re-derive them. The OVMF lab is `nix/probe/ovmf/bridge/` (README there); the
flake checks are `bridge-unit`, `bridge-refusals` and `rootfs-unit` (`nix/tests/bridge.nix`).

## What the bridge accepts

Topologies A (installer default) and E (a failed boot-v2 trial, katana) on any base the check can verify. Since
`nix/bridge2`, A includes the **installer's encrypted layout** (the default: `installer/rime-installer-gui` has
encryption on by default):

| GPT | Filesystem | Mounted at |
|---|---|---|
| 1 | ESP, FAT | `/boot/efi` on bootc (shim + GRUB + bootupd's static `grub.cfg` in `\EFI\fedora\`) |
| 2 | ext4, label `rime-boot` | `/boot` (GRUB's `grub2/grub.cfg`, ostree's `loader` and kernels) |
| 3 | LUKS2, label `RIMEROOT` -> btrfs `rime-root` | `/sysroot` (the ostree repo, deployments, `/var`) |

## Decisions

### /boot on its own partition: recorded in host.toml, mounted by `rime.rootfs`

**Decision.** `check` finds the boot partition beside the root (an ext4 with `grub2/grub.cfg` and ostree's `loader`)
and verifies the chain to it; `start` writes `[boot] device = "UUID=<ext4 uuid>"` and `fstype = "ext4"` into
`/var/lib/rime/host.toml` (merged into the one `[boot]` table carry wrote, then `rime-hostlayer host validate` in the
new root). On NixOS, `rime.rootfs` (`boot = "bootc"`) installs the generator `rime-rootfs-boot`
(`nix/modules/rootfs/boot-generator.py`), which writes `boot.mount` from that table at every boot: the partition (with
its fsck unit), or a bind of the btrfs top level's `/boot` when there is no device (the plain layout).

**Why.** Where `/boot` lives is a per-machine fact, and the toplevel is one generic build (D9): it cannot be in
`fileSystems`. The generator is the only owner of `/boot`; an existing `boot.mount` (an fstab line someone added) is
left alone, and an invalid device writes nothing and says why, so a bad table never stops a boot. Only ext4 is
accepted (`BOOT_PART_FSTYPES`), because that is all the installer writes; xfs would add a module and an fsck tool to
every closure for no machine. Anything else is refused by name: `boot-partition-fstype`, `boot-unknown`.

### One option set: `rime.rootfs`

`rime.installTarget` (the installer's) and `rime.rootfs` (the bridge's) declared the same three subvolumes of the same
label. Now `rime.rootfs` owns them with `boot = "bootc" | "esp"`; `nix/installer/target.nix` is a shim
(`installTarget.enable` sets `rootfs.enable` + `boot = "esp"`, `rootLabel`/`espLabel` are aliases). The installer's
`nixosConfigurations.rime` evaluates the same `fileSystems`, supported filesystems, generators, boot layout and LUKS
setting before and after (only the order of two tmpfiles rules changed:
`/var/lab/rime-nix/artifacts/bridge2/cfg-{head,wt-bridge2}.json`); `installer-target-eval` and `rootfs-unit` build.

### The payload is signed (ed25519, the rime-cache-1 key)

**Decision.** `payload/manifest.sig` is `<key name>:<base64 signature>` over `rime-bridge-payload-v1\n` followed by the
exact bytes of `manifest.json`; `manifest.json` names every other file (store, registration, preview) by sha256. The
bridge trusts `TRUSTED_PAYLOAD_KEYS`, which is the `rime-cache-1` key `nix/modules/update.nix` ships as
`rime.update.cacheKeys` (`bridge-unit` fails if the two differ). Unsigned, untrusted-key and tampered payloads are
refused by name (`payload-unsigned`, `payload-key-untrusted`, `payload-signature-bad`, `payload-corrupt`).

**Why.** It is the same key and primitive the update path already trusts, so the release pipeline signs one more
small file with the key it holds, and no new trust root reaches users. The domain prefix keeps a manifest signature
from ever being valid as a narinfo signature or the other way round (mutant `signature-no-domain`). The verifier is
pure Python (RFC 8032, standard library only) because it runs on the bootc machine with Fedora's python and nothing
from /nix; `bridge-unit` checks it against RFC 8032's vectors and against a signature made by OpenSSL. There is no
environment variable or flag that skips the check: the OVMF lab builds a bridge that trusts a TEST key
(`overrideAttrs`), so a lab build can never be mistaken for the product.

### The left-behind report before `start`

`check --payload P --explain` runs the payload's own `rime-hostlayer carry --plan` (from `preview.tar`, which the
signature covers) against an empty root, with the machine's real `/` and `/proc/cmdline`: the user sees what will not
come across before anything is written. `start` prints the same report first and refuses (`carry-refused`) if the plan
fails.

### What `check` reads of the EFI grub.cfg (and why katana may read `boot-chain-unverified`)

`check --explain` prints one `boot chain:` line: how the booted loader was found (NVRAM BootCurrent, or the only Rime
ESP), the `\EFI\<dir>\grub.cfg` it read, its shape, the filesystem that shape hands over to, and `/boot`'s filesystem.
Known shapes: `bootupd-static` (sources `bootuuid.cfg`, searches `${BOOT_UUID}`, `configfile .../grub.cfg`; every A
install) and `grub2-stub` (a literal `search --fs-uuid <UUID>` + `configfile`). Any other file is `unknown`, and the
refusal names its line count, sha256 prefix, the commands it uses, what each known shape needs and which directives
are missing, and its first lines (`bridge-refusals` case U; unit test `GrubShape`).

**P3/HARDWARE.** katana boots `\EFI\APEX\SHIMX64.EFI` on the Windows ESP; its `\EFI\APEX\grub.cfg` has never been
read (PLAN open question 7, and the katana snapshot does not include it). If it is bootupd's static file, katana
classifies as E with a verified chain; if it is anything else, `check` says `boot-chain-unverified` with the detail
above and nothing is written. Running `rime migrate check --explain` on katana at the P3 GO answers it.

### repair-e archives the trial, never deletes it

**Decision.** `repair-e --confirm` moves the trial's top-level `/composefs` and `/state` (with its ESP files and NVRAM
entry, as before) into `/var/lib/rime/boot-migrate/archive-<time>/` on the same btrfs, by `rename`. Across
filesystems it copies (`cp -a --reflink=auto`), compares every file's sha256, then removes the original; a destination
that exists already is refused. Its manifest records each move, and `start` refuses (`e-repair-pending`) while either
tree is still in the top level.

**Why.** `/state` is the trial's `/var` and may hold writes made while it ran (a user who logged in to the trial).
Deleting it is the one step that cannot be undone, and a rename on one filesystem costs no space and no time. The
archive is inside `/var/lib/rime/boot-migrate/`, which `start` copies into `@nixos` (reflinks; that is how
`phase=failed` carries over), so it stays reachable on NixOS; removing it is the owner's call.

### Keys: the LUKS header and `rime backup`'s keys leave the disk; the recovery key does not

**Decision.** `start`'s backup now contains `keys/`: `luks-header.img` (`cryptsetup luksHeaderBackup`, read back with
`luksDump`, which must name the volume's UUID), `luks-dump.txt`, `rime backup`'s `/var/lib/rime-backup/{keys,recipients}`,
`keys.json` and a README. `rime-migrate key-export --to DIR` does the same on its own (refusing a directory on the disk
being migrated).

**DECIDED-NOT-TO: export the LUKS recovery key.** The installer shows it once and writes it to the boot USB when it
can (`installer/rime-install`); bootc never stores it, so there is nothing to export, and a header backup plus the
passphrase is what restores access. A TPM2 binding cannot leave the machine by design. Both are said in the README the
export writes.

### Nix on the bootc side: the payload's own nix, not a static nix

**Decision (a deviation from PLAN.md 4.2 and the readiness list, which call for a pinned static `nix` in the readiness
release).** `start` unpacks the closure into `@nix`, then runs that closure's own `nix-store --load-db` and
`--verify --check-contents` in a chroot of the new root (`manifest.json` `nix`).

**Why.** The nix that registers the store is then exactly the version the closure was built with (no schema skew),
it is covered by the signed manifest and verified by narHash like every other path, and the bootc image ships no extra
binary that would need its own updates. The cost: the bridge needs `chroot` and a kernel that runs the closure's
binaries, which every bootc machine has. A static nix stays the answer for the ISO (no target system to chroot into).

## Handover (outside this branch's scope)

- **Readiness release (bootc PR to rime-os main):** install `share/rime-bridge/rime-migrate` as
  `/usr/libexec/rime-migrate` and wire `rime migrate`; make `rime update` refuse while
  `/var/lib/rime/nixos-bridge/state.json` exists (the old side must not stage over a bridged disk); a Rust
  `rime backup key export` verb can call `rime-migrate key-export`.
- **Release pipeline:** build `preview.tar` (the payload's `rime-hostlayer`, the system's `etc/` and `sw/bin` names,
  greetd's config; `nix/probe/ovmf/bridge/payload.sh` shows the layout) and sign `manifest.json` with the
  rime-cache-1 key as above.
- **katana (P3):** read `\EFI\APEX\grub.cfg` with `rime migrate check --explain`.
