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

**Found by the encrypted OVMF run, fixed (`write_boot_table`).** `start` used to open host.toml for writing *before*
reading it to merge, so the open truncated what carry had just written: every migrated machine (A and E alike) came
up with a host.toml holding only `[boot] layout/device/fstype`, and lost its carried timezone, locale, keyboard,
`[[mount]]`, `[[luks]]` and `[boot] kernel_params`. On a plain disk nothing visibly broke (the generic toplevel's
defaults hid it); on an encrypted one the menu entry had no `rd.luks.*`, nothing asked for the passphrase and the
initrd timed out on `/dev/disk/by-label/rime-root`. Now the text is read and merged first and written through a
temporary file and `rename`; unit test `BootTable.test_written_over_carrys_file_keeps_it`, mutant
`boot-table-clobbers-carry`. The earlier A and E lab runs (`artifacts/bridge/`) had this defect; the `bridge2` runs
check the carried host.toml after the first NixOS boot.

### One option set: `rime.rootfs`

`rime.installTarget` (the installer's) and `rime.rootfs` (the bridge's) declared the same three subvolumes of the same
label. Now `rime.rootfs` owns them with `boot = "bootc" | "esp"`; `nix/installer/target.nix` is a shim
(`installTarget.enable` sets `rootfs.enable` + `boot = "esp"`, `rootLabel`/`espLabel` are aliases). The installer's
`nixosConfigurations.rime` evaluates the same `fileSystems`, supported filesystems, generators, boot layout and LUKS
setting before and after (only the order of two tmpfiles rules changed:
`/var/lab/rime-nix/artifacts/bridge2/cfg-{head,wt-bridge2}.json`); `installer-target-eval` and `rootfs-unit` build.

### The payload is signed (ed25519, the bootc image's `nixos-bridge-payload.pub`)

**Decision.** The payload carries `manifest.json.sig`: one line `<key name>:<base64 of the 64-byte ed25519 signature>`
over the **exact bytes** of `manifest.json`; `manifest.json` names every other file (store, registration, preview) by
sha256. The key is the one the bootc image ships at `/usr/share/rime-os/trust/nixos-bridge-payload.pub`, one line
`<key name>:<base64 of the 32-byte public key>`, and the names in the two files must agree. This is byte for byte the
format `openssl pkeyutl -verify -pubin -keyform DER -rawin` accepts (what `payload-sign.sh` runs after signing). The
bridge is the ONLY verifier: the readiness release's `rime migrate` (`rimed/rime/src/nixos_bridge.rs`, draft PR rime-os
#137 at 37a5f9a89) execs `/usr/libexec/rime-migrate` with no gate of its own (an earlier draft checked it in Rust too,
and that check was dropped there so the two could never disagree). `verify_payload` is pure Python RFC 8032, because
the bridge runs with Fedora's python and nothing from /nix. Refusals, each by name: `payload-key-missing` (the image
has no key file: no payload is installed), `payload-key-bad`, `payload-unsigned`, `payload-key-untrusted` (signed by a
key of another name), `payload-signature-bad`, `payload-corrupt`.

The producer is `nix/pkgs/rime-bridge/payload-sign.sh` (installed as `share/rime-bridge/payload-sign.sh`): it takes a
Nix-format secret key (`nix key generate-secret` output, seed and public half), signs with OpenSSL, reads the
signature back and verifies it with the same `pkeyutl -verify -rawin` command as the bootc side, and prints the public
key line the image ships. `bridge-unit` runs it end to end: OpenSSL's verifier and the bridge's both accept it, a byte
appended after signing is refused by both, no key file is `payload-key-missing`, a secret whose halves do not match
is refused; 5 mutants on the format (`signature-unchecked`, `signature-prefixed`, `signature-file-renamed`,
`key-compiled-in`, `key-file-first-line`).

**The key (decided 2026-11-02).** A dedicated key named `rime-bridge-payload-1`, not the update path's
`rime-cache-1`. It is generated by Andre's release environment (`nix key generate-secret --key-name
rime-bridge-payload-1`, the tool that environment already uses for the cache key), never on a workstation or in an
agent session; its public half (`nix key convert-secret-to-public`) is what the readiness release adds as
`nixos-bridge-payload.pub` (that release's Containerfile asserts the file is absent until then, so shipping it is a
deliberate edit). The secret is a release secret like `APEX_SB_*`. The OVMF lab signs with a TEST key it generates
under its scratch (`rime-bridge-lab-TEST-1`) and builds a lab-only bridge whose `TRUSTED_PAYLOAD_KEYS` line names it
(`nix/probe/ovmf/bridge/lab-bridge.nix`, `overrideAttrs`); the shipped program has `TRUSTED_PAYLOAD_KEYS = None` and
reads only the image's file, and no flag or environment variable changes that.

**Why a separate key, and why no domain prefix.** The first version of this branch signed `rime-bridge-payload-v1\n`
+ the manifest with `rime-cache-1`; the readiness side signs the bare manifest bytes, and two verifiers that disagree
would refuse every real payload, so the format follows the side that ships first. Without a prefix, the cache key would
still be safe (a narinfo fingerprint is `1;/nix/store/...;sha256:...;<size>;<refs>` and a JSON object can never be
that string), but a separate key keeps the two authorities apart: rotating or revoking the cache key (which every
NixOS machine trusts for every update) does not touch the one-time bridge, and the bootc image trusts nothing that can
sign a NixOS store path.

### The left-behind report before `start`

`check --payload P --explain` runs the payload's own `rime-hostlayer carry --plan` (from `preview.tar`, which the
signature covers) against an empty root, with the machine's real `/` and `/proc/cmdline`: the user sees what will not
come across before anything is written. `start` prints the same report first and refuses (`carry-refused`) if the plan
fails. A payload without `preview.tar` is refused by `start` too (`payload-no-preview`, before
anything is mounted): the report is never optional. (Until the verifier pass, `start` skipped the report silently when
the payload had no preview.)

The commands, not only their helpers, are tested (`test_bridge_d2.Commands`): `start` refuses an unsigned, a
foreign-key and a tampered payload, and one with no preview, before it mounts or writes anything; `check --explain`
refuses (`carry-refused`) when the plan fails and still prints it; `key-export` refuses the disk whose keys it exports.
Mutants `start-unverified`, `start-without-preview`, `check-carry-refusal-dropped`, `key-export-same-disk`,
`manifest-paths-unchecked`, `ed25519-y-only` (a negated S: same y, other x) are killed by `bridge-unit`.

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

## What the OVMF lab proved (2026-11-02, Secure Boot on)

`nix/probe/ovmf/bridge` (README there), evidence `/var/lab/rime-nix/artifacts/bridge2/`. Both runs use disks made
from the published image (2026.11.01.2), Fedora's Secure Boot OVMF with the real Rime MOK certificate and a lab TEST
MOK in MokList, the NixOS Rime 7.2.9 kernel signed by the TEST MOK, and a payload signed in the readiness format by a
TEST key that a lab-only bridge trusts. Every boot reports `SecureBoot efivar=1`.

- **L, the installer's encrypted default** (`run-L-bootc.log`, `run-L.log`, `L/`): passphrase at dracut's prompt,
  `check --explain` (A, /boot partition, chain verified, left-behind report), `start` (LUKS header backup read back),
  then the NixOS initrd asks for the same passphrase, `/` is the opened volume, `/boot` the ext4, host.toml carries
  `rd.luks.*` and `[[luks]]`; rime-greet logs alex in with the old password into Hyprland and the Shell; the ostree
  entry still boots (passphrase, home intact); NixOS again. p2-gates D2.
- **E, katana's topology** (`run-E-bootc.log`, `run-E.log`, `E/`): repair-e archives the trial (ESP dirs, NVRAM
  entry, `/composefs`, `/state`), then the same sequence without LUKS. p2-gates D1, X1.

The first encrypted run found the host.toml truncation above; the first SB run found that `check --payload` crashed on
unit masks in `preview.tar` (`preview_extract` now makes masks links to `/dev/null` and leaves other special files
out, counted; tests `Preview.test_masks_survive_and_specials_never_crash`, `test_mask_never_written_through_a_link`,
mutants `preview-specials-crash`, `preview-mask-through-link`).

Not proven here: katana's own `\EFI\APEX\grub.cfg` (above, P3) and a real-key signed payload (the release key does not
exist yet).

## Handover (outside this branch's scope)

- **Readiness release (bootc PR to rime-os main):** install `share/rime-bridge/rime-migrate` as
  `/usr/libexec/rime-migrate` and wire `rime migrate`. **Re-copy it from this branch before that PR merges:** the copy
  on draft #137 (`files/system/libexec/rime-migrate` at 37a5f9a89) predates `write_boot_table` (it still truncates
  carry's host.toml, so an encrypted machine loses `rd.luks.*` and does not unlock), `preview_extract` (check crashes
  on a payload's unit masks) and `start`'s mandatory preview; its copies of `test_bridge*.py` need the same refresh; make `rime update` refuse while
  `/var/lib/rime/nixos-bridge/state.json` exists (the old side must not stage over a bridged disk); a Rust
  `rime backup key export` verb can call `rime-migrate key-export`.
- **Release pipeline:** generate `rime-bridge-payload-1` once (above), build `preview.tar` (the payload's
  `rime-hostlayer`, the system's `etc/` and `sw/bin` names, greetd's config; `nix/probe/ovmf/bridge/payload.sh` shows
  the layout) and run `payload-sign.sh` on every payload; the readiness release ships the public line.
- **katana (P3):** read `\EFI\APEX\grub.cfg` with `rime migrate check --explain`.
- **Readiness release, again:** after `start`, the bootc side's `rime update --check` still runs normally in the lab
  (`*-update-after-start.txt`): the refusal belongs to `rimed/rime/src/nixos_bridge.rs` (draft PR rime-os #137), not
  to this branch.
- **Release pipeline, again:** `payload-sign.sh` needs `cmp` (diffutils) next to openssl.
- **Desktop:** the first NixOS login after the move shows Hyprland's "updated to 0.57.0" notice
  (`artifacts/bridge2/L/L-nixos-desktop.png`). Whether to silence it is the desktop module's call.
- **Lab host (not Rime):** on katana, `/proc/sys/kernel/modprobe` names a `/nix/store/...-kmod-31/bin/modprobe` that
  does not exist on the host (written from inside the privileged `rime-nix` container by something that runs NixOS
  activation or sysctl there), so the host kernel cannot autoload modules: `cryptsetup` inside the lab container needed
  `modprobe dm_crypt` by hand. A reboot clears it; this branch did not change it.
