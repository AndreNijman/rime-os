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
format `openssl pkeyutl -verify -pubin -keyform DER -rawin` accepts, which `payload-sign.sh` runs after signing (on the
release side only). On the bootc side the bridge is the ONLY verifier: there is no outer OpenSSL check. The readiness
release's `rime migrate` (`rimed/rime/src/nixos_bridge.rs`, draft PR rime-os #137 at f26b97d17) execs
`/usr/libexec/rime-migrate` with no gate of its own (an earlier draft checked it in Rust too, and that check was
dropped there so the two could never disagree); the bootc image has no openssl step for the payload at all. `verify_payload` is pure Python RFC 8032, because
the bridge runs with Fedora's python and nothing from /nix. Refusals, each by name: `payload-key-missing` (the image
has no key file: no payload is installed), `payload-key-bad`, `payload-unsigned`, `payload-key-untrusted` (signed by a
key of another name), `payload-signature-bad`, `payload-corrupt`.

The producer is `nix/pkgs/rime-bridge/payload-sign.sh` (installed as `share/rime-bridge/payload-sign.sh`): it takes a
Nix-format secret key (`nix key generate-secret` output, seed and public half), signs with OpenSSL, reads the
signature back and verifies it with OpenSSL's `pkeyutl -verify -rawin` (a check of the release's own output, not
something the bootc side repeats), and prints the public key line the image ships. `bridge-unit` runs it end to end: OpenSSL's verifier and the bridge's both accept it, a byte
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

**Decision (integrate-wave3, 2026-11-03): the start record is the one rule both sides read.** `start` writes
`/var/lib/rime/nixos-bridge/state.json` at its first step. Until wave 3 the rollback (`try`) began only at the
snapshot step, so a failure in the backup, while masking the old side's units or at the first fsck gate left a record
with neither `failed` nor `rolled back`, the units it had masked still masked, and the readiness release's `rime`
refusing its bootc verbs for good (readiness review of #137). Now every step after the record exists is inside the
rollback, `mask_old_side` records each unit as it masks it (a failure half way unmasks exactly those), and `check` and
`start` both read the record with the same rule as `rimed/rime/src/nixos_bridge.rs`: no record, or a start that failed
and rolled back, allows; a finished move refuses `already-moved`, any other record `start-unfinished` (naming the last
step), an unreadable one `start-record-unreadable`. `check --json` carries the record as `start_record`. Proof:
`bridge-unit`'s `StartRecord` tests (a failed backup, a failure half way through the masks, a failed first fsck gate
each end in `rolled back` with the masks undone; check and start refuse over each kind of record and start touches
nothing; a rolled-back record allows); mutants `start-early-steps-outside-rollback` and `check-ignores-start-record`.

### `[boot] microcode` comes from the moved machine's CPU (bridge3)

**Decision.** `rime-hostlayer carry` writes `[boot] microcode = "intel"` or `"amd"` into host.toml from the
`vendor_id` of `/proc/cpuinfo` (`--cpuinfo` names another file), the rule the installer uses. A vendor that is neither
GenuineIntel nor AuthenticAMD, or a file it cannot read, writes nothing, so rime-boot's default (`both`) applies. The
carry report names the answer under `CPU vendor`.

**Why.** Before this a bridged machine loaded both vendors' early microcode where an installed one loads its own
(`docs/nixos/boot-matrix.md` "Early microcode"): the other vendor's cpio copied into `/boot` and loaded by GRUB at every boot
for nothing.
The carry runs inside `start`'s chroot of the new root with `/proc` mounted, so `/proc/cpuinfo` is the machine being
moved, and `rime-bridge.py` did not change for it. Proof: `rime-hostlayer`'s
`test_the_cpu_vendor_picks_the_boot_microcode` (Intel, AMD, Hygon, no vendor) and
`test_microcode_alone_makes_a_boot_table_and_an_unreadable_cpuinfo_none`; mutants `hostlayer-microcode-dropped` and
`hostlayer-microcode-swapped` in `bridge-unit`; the OVMF runs below (`*-nixos-checks.txt`: host.toml, `MENU` and `BOOTFILE` lines): host.toml says `intel`
(the lab CPU is `-cpu host` on katana) and the boot menu and `/boot` hold only `intel-ucode`.

### Hyprland's "updated" window is off (bridge3)

**Decision.** Rime's `appearance.lua` sets `ecosystem = { no_update_news = true }`. Hyprland 0.57 opens its own
"Hyprland updated to 0.57.0" window (hyprland-update-screen) at the first login after `lastVersion` changes. Rime's own
release notes are the "what is new" for every update, and the window points at Hyprland's changelog, not at what
changed for the user. The donation nag (`no_donation_nag`) stays as Hyprland ships it. Who meets the window: the
published bootc image 2026.11.01.2 already ships `hyprland-0.57.0-2.fc45` (`rpm -q hyprland` in it, verify-bridge3), so
a user who logged in there has `lastVersion` 0.57.0 and NixOS's 0.57.0 takes no "updated" path; the window comes for a
user whose last bootc session ran 0.56 or older, and at every later Hyprland bump a NixOS release brings. The lab seeds
`lastVersion` 0.56.2 on the bootc side to force the path. (An earlier text cited
`artifacts/bridge2/L/L-nixos-desktop.png` as showing the window: that file is the SECOND NixOS boot's screenshot, the
bridge2 driver overwrote the first boot's, and it shows a bare desktop. The window on an OVMF guest is shown by the
negative control below.)

**How it reaches a bridged user.** The file is in `/share/rime/hypr/rime/`, which rime-shell-firstrun (a oneshot
user unit in `default.target`) refreshes into `~/.config/hypr/rime/` when the user manager starts; the carried bootc
copy has no knob (`BOOTC-KNOB=0`). **Greeter-first login measured (verify-bridge3):** `run.sh A --fresh --sb
--readiness --greeter-first --negative-control --phases bootc,nixos` makes the user's FIRST NixOS session the
greeter's (no serial login before it): firstrun exited at 38.39 s of the boot and Hyprland started at 38.42 s, after
it (`A-nixos-update-news.txt`: `ExecMainExitTimestampMonotonic`, `HYPR-START-US`; pam_systemd waits for the user
manager, whose `default.target` waits for the oneshot), `lastVersion` 0.56.2 -> 0.57.0, `getoption` true,
`UPDATE-SCREEN=0`, journal clean, bare desktop (`A-nixos-desktop.png`). Evidence
`/var/lab/rime-nix/artifacts/verify-bridge3/greeter-first/` (`run-A.log` PASS, `A/`). The order is observed on one
run, not a systemd guarantee Rime sets: the same line in the readiness release's `appearance.lua` (handover below)
makes it irrelevant. **Negative control on the same guest:** with the table cut from the user's copy (firstrun masked)
and `lastVersion` set back to 0.56.2, the next greeter login opens the window (`A-nixos-negative-desktop.png`:
"Hyprland updated to 0.57.0!", `getoption` false, `UPDATE-SCREEN=1`, `A-nixos-negative-control.txt`), so the lab's
checks can fail there. Proof in Nix:
`desktop` subtest "no Hyprland 'updated' window after a version change" (seeds `lastVersion` 0.56.2, logs in,
`lastVersion` becomes 0.57.*, `hyprctl getoption ecosystem:no_update_news` is set and true, no
hyprland-update-screen process and no journal line naming `hyprland-update`, screenshot
`hyprland-no-update-news.png`); its mutant `checks.x86_64-linux.desktop.mutants.update-news` (the same system with the
table cut out of `appearance.lua`) fails with `bool: false`. The verifier found the process check alone could not fail
in that VM: with the table cut AND the getoption assertion skipped the check still PASSED, because
hyprland-update-screen starts (`--new-version 0.57.0`) and then SEGFAULTS in hyprtoolkit at its first configure (no GPU
buffers in the test VM), so it is gone before `ps` looks (`artifacts/verify-bridge3/desktop-mutant-window-only.log`). The
subtest now also asserts the journal names no `hyprland-update` (systemd-coredump records the crash); the same
getoption-skipped mutant then FAILS on that line (`desktop-mutant-window-only-fixed.log`). On the OVMF guest (`-device
VGA`) it is the other way round: the window renders and stays (`ps` sees it, the journal has no line), so the lab
driver asserts both.

## What the OVMF lab proved on the readiness image (bridge3, 2026-11-03, Secure Boot on)

Evidence `/var/lab/rime-nix/artifacts/bridge3/` (`run-A.log`, `run-L.log`, `A/`, `L/`; the records of the second
NixOS boot in `KIND/nixos-again/`). The disks are the **readiness image**: the published 2026.11.01.2 plus one layer
with draft #137's `rime` and `/usr/libexec/rime-migrate` (f26b97d17) and the TEST payload key where the release puts
its own (`nix/probe/ovmf/bridge/README.md`, `readiness-image.txt`). So the bootc side is #137 itself, not a lab
bridge: the guest's `rime migrate check --explain` and `rime migrate start` run, and `KIND-readiness.txt` shows the
guest's rime-migrate minus line 1 hashing to the same `a6fcdc2a…` as this branch's `rime-bridge.py`. Same Secure
Boot rig as below (Fedora's SB OVMF, real Rime MOK + TEST MOK, `SecureBoot efivar=1` on every boot). Both runs:
`--rollback-first` (an injected fault after the boot entry: rc 10, `VERDICT=clean`), then `check --explain`, `start`,
NixOS, back to the ostree entry, NixOS again; `run.sh KIND --fresh --sb --readiness --rollback-first --phases
bootc,nixos,back,nixos`; both `PASS`.

- **Plain A through `\EFI\fedora\`** (re-run owed since the host.toml fix): firmware boots `Boot0005 Fedora` ->
  `\EFI\fedora\shimx64.efi`, `check` = A with a verified chain (`A-check.txt`), `start` 5 min 29 s; NixOS mounts
  `@nixos`/`@nix`/`@home` and `/boot` from the btrfs top level, host.toml keeps timezone, locale and keyboard, rime-greet
  logs alex in with the old password into Hyprland + the Shell (`A-nixos-desktop.png`); back on ostree the home sums
  hold (`A-back-checks.txt`).
- **L, encrypted** (re-run on the start-record bridge): the same with dracut's and the NixOS initrd's passphrase
  prompts, `/` in the opened LUKS volume, `/boot` the ext4, host.toml `kernel_params` rd.luks.* (`L-nixos-checks.txt`),
  `start` 5 min 51 s.
- **The bootc verbs refuse after `start`** (readiness problem 8, bridge2 open 3): `rime update`, `rime update --check`
  and `rime boot migrate` each exit 3 with `REFUSED [nixos-migration-started]` (`KIND-refusals-after-start.txt`,
  `REFUSED-COUNT=3`); before `start`, `rime update --check` is not refused (`KIND-refusals-before-start.txt`).
- **Microcode:** host.toml `[boot] microcode = "intel"` (the guest is `-cpu host`, GenuineIntel), the menu's `initrd`
  line and `/boot/rime/nixos` carry `intel-ucode` only; `check --explain` and `start` print `CPU vendor: vendor_id
  GenuineIntel: host.toml [boot] microcode = intel` in the left-behind report.
- **Hyprland's window:** alex's `lastVersion` is seeded 0.56.2 on the bootc side; at the first NixOS login it
  becomes 0.57.0, `hyprctl getoption ecosystem:no_update_news` is `bool: true set: true` and no hyprland-update-screen
  runs (`KIND-nixos-update-news.txt`); the screenshot is the bare desktop.

The first pass of both runs (`artifacts/bridge3/run1/`, also PASS) wrote the second NixOS boot's records over the
first's; the driver now writes a repeated phase into `PHASE-again/`.

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

- **Readiness release (bootc PR to rime-os main):** DONE at integrate-wave3. Draft rime-os #137 carries this
  branch's `rime-bridge.py` byte for byte as `/usr/libexec/rime-migrate` (first line `#!/usr/bin/python3 -I`) and its
  `test_bridge*.py`, refreshed after bridge2 merged (write_boot_table, preview_extract, the mandatory preview and the
  start record above), and `rime` refuses `update` and the other bootc verbs while the start record says so.
  Still open there: the release key file (`nixos-bridge-payload.pub`, made by the release environment) and a Rust
  `rime backup key export` verb that calls `rime-migrate key-export` (not needed: `rime migrate key-export` exists).
- **Release pipeline:** generate `rime-bridge-payload-1` once (above), build `preview.tar` (the payload's
  `rime-hostlayer`, the system's `etc/` and `sw/bin` names, greetd's config; `nix/probe/ovmf/bridge/payload.sh` shows
  the layout) and run `payload-sign.sh` on every payload; the readiness release ships the public line.
- **katana (P3):** read `\EFI\APEX\grub.cfg` with `rime migrate check --explain`.
- **Readiness release, again:** DONE (bridge3). On a booted bootc VM of the readiness image (#137 at f26b97d17 as a
  layer on the published image, lab README) `rime update`, `rime update --check` and `rime boot migrate` each print
  `REFUSED [nixos-migration-started]` and exit 3 after `start`, and `rime update --check` is not refused before it
  (`artifacts/bridge3/{A,L}/*-refusals-*.txt`). `rime-bridge.py` did not change in bridge3: #137's
  `/usr/libexec/rime-migrate` minus its first line and `rime-bridge.py` minus its first line are both sha256
  `a6fcdc2a182abdee7ec6c7e47ba28084d8017e8ca7e8db31b56f69332964d14b` (`artifacts/bridge3/readiness-image.txt`, and the
  guest's own `*-readiness.txt`). Optional for #137: the same `ecosystem = { no_update_news = true }` table in bootc's
  `files/desktop/hypr/rime/appearance.lua` (decision above), so the first NixOS login is quiet whatever starts first.
- **Release pipeline, again:** `payload-sign.sh` needs `cmp` (diffutils) next to openssl.
- **Desktop:** DONE (bridge3): the "updated" window is off (decision above).
- **Lab host (not Rime):** RESOLVED by 2026-11-03: katana's `/proc/sys/kernel/modprobe` reads `/usr/sbin/modprobe`
  again (it had named a `/nix/store/...-kmod-31/bin/modprobe` written from inside the privileged `rime-nix`
  container). bridge3 loaded `dm_crypt` on the host only while `prepare.sh disk-L` ran and removed it after.
