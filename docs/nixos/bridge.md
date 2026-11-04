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

### H9 counts what `start` consumes: data and metadata apart (bridge-space)

**The defect.** The first H9 charged the whole home as data: `floor + home + old store` against statfs free, the worst
case "every file in home is rewritten after the move". The real L16 (1.6 TB btrfs, Secure Boot on; everything else
passed) read `376.8 GB free, 1334.1 GB needed (floor 19.0 + home 1294.2 + old store 20.9)`: REFUSED. That rule locks
out every machine whose home fills more than about half its disk, which is most real users.

**What `start` really consumes.** `cp -a --reflink=always --one-file-system` makes `@home` share every data extent with
the old `var/home`, and `@pre-nixos` is a read-only snapshot that shares everything: neither costs data space when it
is made. Their immediate cost is btrfs METADATA: the copy writes a new inode, directory entries and file extent items
for every name in home, one more back-reference per shared extent, and the data of every file under one sector, which
btrfs keeps inline in the metadata tree. A full metadata space with free data is btrfs's real ENOSPC.

**Decision.** `space_plan` (`rime-bridge.py`) checks two figures from `btrfs filesystem usage -b` of the root (read-only
ioctls; `--disk` mode reads its read-only mount the same way), and refuses each by its own name:

| | Needed | Available |
|---|---|---|
| `free-space` (data) | `FLOOR_BYTES` (closure + one mass rebuild, 19 GB) + the old ostree store, counted again in case it is rewritten | unused space in data chunks + unallocated / data ratio (= statfs free on one profile) |
| `metadata-space` | home's metadata x `META_MARGIN` (1.25) x the metadata ratio (DUP: 2) | (unused in metadata chunks - the global reserve) x ratio + the unallocated space data did not take |

Home's metadata is estimated two ways and the larger is kept: its **share** (metadata used x min(1, home bytes / data
used); capped because du counts uncompressed bytes and Used on-disk ones, and home's metadata cannot exceed all of it)
and **per name** (names x `ENTRY_META_BYTES` (1 KiB) + bytes of files under 4 KiB; the share alone misses a home of many
small files on a disk of large ones). Data takes its unallocated first; metadata gets what is left (unallocated is
counted once). Mixed block groups (tiny filesystems only) are one pool, per-name estimate only. A btrfs root whose usage
cannot be read is refused `space-unmeasured`, never guessed.

**The L16, again** (`test_bridge.SpaceH9.test_l16_passes`, its real `usage -b` verbatim): data 376.8 GB free, 39.9 GB
needed (floor 19.0 + old store 20.9); metadata 70.26 GB on the device, 39.46 GB needed (share capped at all 15.79 GB
used, x 1.25 x 2; available (2.47 unused - 0.54 reserve) x 2 + 66.40 unallocated). It passes, with 30 GB of metadata
headroom. Its home's names were not counted; the per-name estimate would pass up to ~22 M names.

**Divergence is the user's, after the move, and gradual.** Every file changed or added on NixOS takes new space while the
old side keeps its old blocks, so the free figure shrinks as home diverges; it comes back when the old side
(`@pre-nixos`, `ostree/`, the old `var/home`) is removed. That is PLAN.md D2's reclaim step, `rime migrate finish`
(planned, not built); `check --explain` says so in a `divergence:` line and names no verb that exists today. Until then
the ostree entries still boot the old system with home as it was at the move ("Booting back to bootc").

**The full-home du stays.** The gate needs home's bytes only for the capped share and its names for the per-name
estimate (both from ONE walk, `du_tree`), but `start`'s first step copies all of home off the disk and
`backup-too-small` compares the backup's free space with home's size. On the L16 that walk took ~5 min over 1.3 TB.

**Proof.** `bridge-unit`: the L16 fixture passes; a data floor fixture refuses `free-space`; a tight metadata fixture
refuses `metadata-space` (and passes at the exact boundary); a home of 6 M small files on a disk of games refuses
`metadata-space`; unallocated shared refuses; mixed block groups; the explain text. Mutants killed:
`metadata-gate-dropped`, `metadata-ratio-ignored`, `home-as-data` (the L16 refused), `global-reserve-dropped`,
`share-uncapped`, `per-name-dropped`, `unallocated-twice`, plus `free-space-flipped` and `floor-dropped` rewritten for the
new code. `bridge-refusals` (VM, real btrfs): `REFUSED [free-space] data: ... floor 0.3 + old store 0.3 (home is a reflink copy: no data at start)` on the H9 disk and on a 1 GiB A under the real 19 GB floor; case M (1 GiB, filled with 8 MiB files then half freed, 60,603 home names) `REFUSED [metadata-space]`, 0.13 GB on the device vs 0.38 GB needed, disk sha256 unchanged, and the refused `cp --reflink` run anyway on that disk fails (rc 1): the refusal was real. Case R measures the estimate against a REAL start (snapshot + `cp -a --reflink=always`) on a 4 GB disk with a 1.5 GB store and 40,403 home names: snapshot + subvolume 96 KiB metadata, the copy +58.5 MB metadata (1,449 B per name) and +0 B data; the estimate (101.2 MB before the margin) is 1.73x what the copy took, 2.16x with it. Log: `/var/lab/rime-nix/artifacts/bridge-space/bridge-refusals.log`; unit + mutants: `.../bridge-space/bridge-unit.log`.

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
it (`A-nixos-update-news.txt`: `ExecMainExitTimestampMonotonic`, `HYPR-START-US`; the likely mechanism, pam_systemd
waiting for the user manager whose `default.target` orders after the oneshot, is not confirmed), `lastVersion` 0.56.2 -> 0.57.0, `getoption` true,
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

### The start record is read one way everywhere (topology-e)

Three programs read `/var/lib/rime/nixos-bridge/state.json`: the bridge's own `check`/`start` (`record_refusal`), the
readiness `rime` before every bootc verb (`nixos_bridge.rs` `classify`) and `rime-update-check`. They now apply ONE
rule: `done` present and not null is a finished move (refused); otherwise `failed` PRESENT (whatever its text) with
the last step `"rolled back"` is a start that undid itself (allowed); anything else, including `steps` that is not a
list, is an unfinished start (refused); an unreadable record refuses. The bridge used to read `failed` as a truth value
and look at `done` second.

Why it mattered (found by the topology-e interrupt probe on the readiness image): a Ctrl-C during `start` raises
`KeyboardInterrupt`, whose text is `""`. The rollback ran, the record said `failed: ""` and `"rolled back"`; `rime` and
`rime-update-check` allowed every bootc verb (correct: the old side is restored), but the bridge's own `check` and
`start` refused `[start-unfinished]` with the self-contradicting "reached step 'rolled back' and neither finished nor
rolled itself back", for ever, with no verb to clear it. Fixed on both ends: `start` records `str(e) or
type(e).__name__` (`"KeyboardInterrupt"`, which even #137's current reader accepts), and `record_refusal` uses the
rule above. Rust and `rime-update-check` already used it, so they do not change. Tests
`StartRecord.test_a_start_interrupted_by_ctrl_c_rolls_back_and_allows_another`,
`test_the_record_rule_is_the_readiness_release_rule` (both FAIL on the old bridge); mutants `record-failed-truthy`,
`failed-text-empty-on-interrupt`, `record-done-not-first`, `record-done-truthy`, `record-steps-any-type` (each
killed, `bridge-unit`: 56 mutants killed).

## What the OVMF lab proved on E with the release kernel (topology-e, 2026-11-04, Secure Boot on)

Evidence `/var/lab/rime-nix/artifacts/topo-e/` (`run-E.log`, `run-A.log`, `E/`, `A/`, `kernel-release.txt`,
`readiness-image.txt`, `prepare-*.log`). Commands: `run.sh E --fresh --sb --readiness --rollback-first --greeter-first
--phases bootc,nixos,back,nixos` and `run.sh A --fresh --sb --readiness --kill-probe --interrupt-probe --phases bootc`;
both `PASS`.

- **The bootc side is #137 at 1a4070580** (merged unchanged as d97a26af19, release 2026.11.04) on #137's merge base: the published 2026.11.03.5
  (`ghcr.io/andrenijman/rime-os@sha256:27a9b1907a06…`, revision bdc5b8845) plus one layer with #137's `rime`
  (built in that image), `/usr/libexec/rime-migrate` (minus line 1 = this branch's base `rime-bridge.py`, both
  `a6fcdc2a…`), `/usr/libexec/rime-update-check` (bridge3's layer had left the base image's older one) and the TEST
  payload key. `prepare.sh image` now refuses when rime-migrate and `rime-bridge.py` differ.
- **The NixOS kernel is the release-signed one**, not a TEST MOK one: the lab system with NVIDIA on hashes its image
  `5r2srpcsqwk47dlmfc9pch2wy3rw9kfn`, the same as `nixosConfigurations.rime`, so the committed release signature
  applies; `sbverify --cert rime-mok.pem` OK, the TEST MOK does not verify it (`kernel-release.txt`). MokList holds the
  real Rime MOK certificate and nothing else (`prepare-vars.log`), and every boot reports `SecureBoot efivar=1`, so
  shim and GRUB accepted the release signature on NixOS as on bootc.
- **E end to end:** boot through `APEX-OS Primary`, `repair-e` (the trial archived), `check` (E), the left-behind
  report, a start with an injected fault after the boot entry (rc 10, `VERDICT=clean`), a reboot into ostree, then the
  real `start` (about 3 min 35 s): NixOS from GRUB's default, **the first login through rime-greet before any serial
  login** (alex prefilled from the carried last-user, `E-nixos-greeter-before-login.png`; Hyprland + the Shell for alex
  in a greetd wayland session, no update window, `E-nixos-desktop.png`; on that boot Hyprland started 0.13 s before
  firstrun exited and the window still stayed off), then `@nixos`/`@nix`/`@home`, the carried `phase=failed`, and the
  home checksums `Documents/note.txt`, `Pictures/blob.bin`, `.config/rime-lab/settings.toml` all `OK`
  (`E-nixos-checks.txt`). Back on the ostree entry: home intact; NixOS again: PASS.
- **Every bootc verb, read on the real record** (`verb-probe.sh`: `rime update`, `update --check`,
  `update --firmware-only`, `update --migrate-boot`, `rollback`, `pin`, `channel set`, `boot migrate`, `live apply`,
  `live rollback`, `live soft-reboot`, each as the image's `/usr/bin/rime` with bootc/ostree/… stubbed, plus
  `rime-update-check` with a stub bootc that reports an update; `KIND-probe-LABEL.txt` holds each verb's output):

  | Probe | Record | `rime` (11 forms) | `rime-update-check` |
  |---|---|---|---|
  | `no-record` (E, A) | none | 11 allowed | `state=available` |
  | `rolled-back` (E) | `failed` = the fault's text, last `"rolled back"` | 11 allowed | `state=available` |
  | `rolled-back-rebooted` (E) | the same, after booting the ostree deployment again | 11 allowed; the masked units enabled again | `state=available` |
  | `done` (E, A) | `done` set | 11 refused: exit 3, `rime: '<verb>' REFUSED [nixos-migration-started]`, no stub reached | refused, `state=none` |
  | `done-back-on-ostree` (E) | the same, booted back from NixOS | 11 refused | refused |
  | `killed-start` (A) | last `"old side"`, no `failed` (SIGKILL at the first fsck gate) | 11 refused | refused |
  | `killed-start-rebooted` (A) | the same after a reboot | 11 refused; a second `start` exits 3 `REFUSED [start-unfinished]`, subvolumes unchanged | refused |
  | `interrupted-start` (A, #137's rime-migrate) | `failed: ""`, last `"rolled back"` (Ctrl-C in `install closure`) | 11 allowed | `state=available` |
  | `interrupted-start-branch` (A, this branch's) | `failed: "KeyboardInterrupt"`, last `"rolled back"` | 11 allowed | `state=available` |

  "Allowed" means the guard let the verb through: what it then did is its own (the stubs show `update` and `rollback`
  reaching bootc; `boot migrate` stopped at rime-boot-migrate's own state, E `[last-attempt-failed]`, A
  `[unhealthy]`). So `rime` and `rime-update-check` agree in every state, and allow again only after a start whose
  record says failed + rolled back. The one disagreement was the bridge itself: on `interrupted-start` #137's
  `rime migrate check` refused `[start-unfinished]` (`A-int-check-readiness.txt`), while this branch's bridge does not
  (`A-int-check-branch.txt`). The decision above has the fix.

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
- **Readiness release, topology-e (a follow-up bootc PR to main; nothing pushed there):** #137 merged (d97a26af19,
  release 2026.11.04) byte-identical to the 1a4070580 tested here, so the shipped `rime-migrate` has the old reader.
  Not reachable on a real machine yet (`start` refuses `[payload-key-missing]` until the release key ships), so it only
  has to land before that key does. Re-copy this branch's `rime-bridge.py` as `files/system/libexec/rime-migrate` (keep
  line 1 `#!/usr/bin/python3 -I`) and `tests/test_bridge_d2.py` as `tests/rime-migrate/test_bridge_d2.py` (main's
  copies are byte-identical to feat/nixos 06e715def's). The change in rime-migrate (main = 1a4070580): `record_refusal` (lines 1063-1068) reads `steps` only when it is a list, returns
  `already-moved` first when `rec.get("done") is not None`, and allows when `"failed" in rec and last == "rolled
  back"`; `cmd_start`'s handler (line 1763) records `state["failed"] = str(e) or type(e).__name__`. Nothing changes in
  `rimed/rime/src/nixos_bridge.rs` or `files/system/libexec/rime-update-check`: they already read the record that way.
- **Release pipeline, again:** `payload-sign.sh` needs `cmp` (diffutils) next to openssl.
- **Desktop:** DONE (bridge3): the "updated" window is off (decision above).
- **Lab host (not Rime):** RESOLVED by 2026-11-03: katana's `/proc/sys/kernel/modprobe` reads `/usr/sbin/modprobe`
  again (it had named a `/nix/store/...-kmod-31/bin/modprobe` written from inside the privileged `rime-nix`
  container). bridge3 loaded `dm_crypt` on the host only while `prepare.sh disk-L` ran and removed it after.
