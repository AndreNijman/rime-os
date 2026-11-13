# The move to Rime OS on NixOS: what this image does about it

This release prepares a bootc machine for the move. It does not move anything by
itself and it changes nothing until the owner runs `sudo rime migrate start`.

## `rime migrate`

`/usr/libexec/rime-migrate` is the bridge program (Python, standard library only,
run with `python3 -I`). `rime migrate ARGS…` execs it with every argument passed
through unchanged, so its own `--help` is the reference:

| command | what it does |
|---|---|
| `sudo rime migrate check [--explain] [--payload DIR]` | read-only: can this machine move, and if not, what blocks it; with `--payload`, also what will not come across |
| `sudo rime migrate repair-e --confirm` | topology E only: moves an earlier boot trial's leftovers that block a start aside (archived, never deleted) |
| `sudo rime migrate key-export --to DIR` | copies the LUKS header backup and the backup tool's keys to DIR |
| `sudo rime migrate start --payload DIR [--backup DIR]` | takes a read-only snapshot of the old system as its restore point, installs the NixOS closure beside bootc, adds the GRUB entry; on any failure it removes what it made and records `rolled back`. `--backup DIR` (optional) also copies home off the disk first |
| `sudo rime migrate undo [--yes]` | on the old system, after a start that finished but whose NixOS was never used: takes the move back (the NixOS GRUB entry, `@nixos`, `@nix`, `@home`, `@pre-nixos`, the masks), so `rime update` works again. Without `--yes` it only prints what it would do |

The copy here is taken byte for byte from the NixOS side's `rime-bridge.py`
(first line changed to `#!/usr/bin/python3 -I`), at the NixOS branch's bridge
branch feat/migrate-without-drive (sha256 of the file without its first line:
`a766cb9c34a7b08f32ae4e5b8d6bd6ee7756e8adbbd04970f8a221c5760c1f46`). Its tests
(`test_bridge.py`, `test_bridge_d2.py`) are in `tests/rime-migrate/` and run
from `tests/test-rime-migrate.sh`. The fixture uses a synthetic root UUID
(`0b0b0b0b-…`), never a real machine's. Compare again, by that hash, before
this release ships. Since the previous copy it changed one thing: the move
needs no drive (below).

On a system older than this release `rime migrate` does not exist; on one where
the program is missing, `rime migrate` says so and points at `sudo rime update`.

## The move needs no drive

`start` needs no space beyond the disk being moved, and no external drive.
Its restore point is the read-only `@pre-nixos` snapshot it takes of the old
system with home: it costs no space when made, and `start` restore-tests it
(a sample of home's files must read back equal from the snapshot; a file
written after the snapshot, because the machine is in use, counts as changed,
not as a failure). The old system also stays bootable from the GRUB menu until
`rime migrate finish`.

What it does not cover: the disk itself failing, and the time after `finish`,
which deletes `@pre-nixos` and the old system (its dry run then says `@home` is
the only copy of home). `--backup DIR` on another disk adds a copy off the disk
(home, restore-tested, and the keys); `sudo rime migrate key-export --to DIR`
copies just the LUKS header and `rime backup`'s keys (a few MB) at any time.

When home is a btrfs subvolume of its own, a snapshot of the top level does not
hold it, so `start` without `--backup` is refused as
`home-subvolume-needs-backup`. A Rime install never makes one. A btrfs
subvolume inside home (containers' btrfs storage, for one) is refused as
`home-nested-subvolume`, with or without `--backup`, naming each: neither the
copy of home nor a backup enters it, so its files would be left behind.

## How much free space the move needs

`start` makes reflink copies of `/var/home` and of `/var/lib/flatpak` (the
system's Flatpak apps, which keep working on NixOS: their launchers' `Exec=/usr/bin/flatpak`
is linked there) and a read-only snapshot of the old system. They share their data blocks with what is already on the disk, so they
cost no data space when made; the copies do duplicate the files' btrfs
metadata. `check` therefore asks two things, each refused by its own name:

| refusal | needed | counted against |
|---|---|---|
| `free-space` (data) | 19 GB (the NixOS system plus room for one full rebuild) + the size of the old ostree store | unused space in data chunks + unallocated space |
| `metadata-space` | home's and the Flatpak apps' metadata x 1.25 x the metadata profile's copies (2 on DUP) | (unused metadata space - the kernel's global reserve) x copies + the unallocated space data does not take |

Their metadata is the larger of its share of all metadata used (by their
size against all data used, at most all of it) and 1 KiB per file or directory
plus the bytes of files under 4 KiB. A btrfs root whose usage cannot be read is
refused `space-unmeasured`. Earlier copies charged home's full size as data,
which refused a 1.3 TB home on a disk with 377 GB free.

The space the copy shares is not free forever: each file changed or added on
NixOS takes new space while the old system keeps its blocks. `check --explain`
says so in a `divergence:` line. The space comes back with `rime migrate
finish`, run on Rime OS on NixOS once it has started well three times: it removes
the old side (@pre-nixos, ostree/ and the old var/home) and the GRUB menu's
ostree entries. `sudo rime migrate finish --dry-run` (also what it does without
`--yes`) first lists what it would delete and what stops working, then measures
the space it should free. On a big home that measurement can take hours; Ctrl+C
stops it safely, and `--yes` does not measure. Until then those entries still
start the old system, with home as it was at the move.

## After a start, the bootc verbs refuse

The bridge writes `/var/lib/rime/nixos-bridge/state.json` (directory 0700) at its
first step and after every step. Once that record exists, `rime` refuses these
verbs by name, before it asks for root:

`update` (every form, `--check` too), `rollback`, `pin`, `channel set`,
`boot migrate`, `live apply`, `live rollback`, `live soft-reboot`.

The refusal is `REFUSED [nixos-migration-started]`, exit 3, and it names the
NixOS GRUB entry, "Rime (generation …)". `boot status`,
`channel status` and `migrate` itself are not held (the bridge keeps its own record).

| record | answer |
|---|---|
| absent | allowed |
| `failed` set and the last step is `rolled back` | allowed (the start undid itself) |
| `done` set | refused: the move finished, boot the NixOS entry |
| any other readable record | refused: a start neither finished nor rolled itself back; the refusal names the last step the record reached and the record's path |
| unreadable as root, or not JSON | refused (fails closed) |
| permission denied as a normal user | left to the root check, which runs next |

**A start that fails at any step undoes itself.** The bridge writes the record
at its first step (`backup` with `--backup`, `old side` without), and every step after that is inside its
rollback: a start that fails in the backup, while masking the old side's units
(each one is recorded as it is masked), at an fsck gate or later unmasks what it
masked, removes what it made and records `failed` with `rolled back`, which
allows these verbs again. (Until the NixOS side's wave 3 the rollback began only
at the snapshot step, so such a failure left the verbs refused for good.)
`rime migrate check` and `start` read the record with the same rule as the
table above and refuse by name: `already-moved`, `start-unfinished` (naming the
last step), `start-record-unreadable`. A record that is still refused after
this, for example a start killed by a power cut, names its path and last step.

`rime-update-check` (the timer that looks for updates) reads the same record
with the same rule: while it refuses, it never asks the registry and reports
`state=none`, so Settings → Updates does not offer a bootc update the machine
would then refuse. The start also masks that timer; this covers a machine where
something unmasked it.

**A finished start can be taken back while NixOS was never used.**
`sudo rime migrate undo` runs on the old system (booted from its ostree entry).
It refuses by name, before it writes anything, when the move cannot be undone
safely: `finished` (`rime migrate finish` ran), `nixos-used` (NixOS reached
multi-user.target at least once), `home-changed-since-move` (something wrote the
new home after the move; it names the file), `home-unreadable` (a name in the
new home could not be read, so "nothing wrote it" cannot be said),
`subvolume-in-use`, `mounts-unreadable`, `boot-unmounted` (/boot is not
mounted, so the NixOS entry could not be removed), `boot-hook-block-unclear`
(custom.cfg's `rime-boot` block is not exactly one BEGIN line through one END
line with no other block's marker between them), and `start-unfinished` or
`start-record-unreadable` as above. Otherwise it removes the NixOS GRUB entry
(only the `rime-boot` block in custom.cfg; other blocks, such as the Secure Boot
key entry, stay, and the old file is kept beside it as `custom.cfg.pre-undo`),
deletes `@nixos`, `@nix` and `@home` (with the subvolumes a NixOS boot made
inside them) and `@pre-nixos` last, puts the old filesystem label back (none,
when it had none), and unmasks the old side. The record then reads `failed` with
`rolled back` (`done` is kept as `done_before_undo`), so the verbs above are
allowed again and a new `start` may run; only then are the update timers
started, so one that fires at once never sees the move. An undo that stops half
way keeps the record as moved; running it again continues. A timer that does
not start is named, with the command to start it; the undo itself is complete.

A machine moved by a release from before `undo` existed has no `undo` in its
`rime migrate`, and cannot take an update while it is moved. Run a newer
release's bridge from a copy: `sudo podman run --rm --entrypoint cat
ghcr.io/andrenijman/rime-os:daily /usr/libexec/rime-migrate >
/var/tmp/rime-migrate`, then `sudo python3 -I /var/tmp/rime-migrate undo` (the
plan) and `… undo --yes`. The record format is the same; a record from before
2026-11-11 that does not say which timers ran gets the enabled ones started.
That is how the first moved machine was taken back (2026-11-12).

Not wrapped: `bootc upgrade` typed directly. The bridge masks the update timers
and `bootc-fetch-apply-updates` is already masked in the image, so nothing on
the old side runs it by itself.

## The payload signature

The bridge checks the payload itself, at the point it reads it
(`verify_payload`), before `check --payload` reports on it and before `start`
writes anything:

- `DIR/manifest.json.sig` is one line `<key name>:<base64 ed25519 signature>`
  over the exact bytes of `DIR/manifest.json`, the format the NixOS side's
  producer (`nix/pkgs/rime-bridge/payload-sign.sh`) writes; the manifest names
  every other payload file by sha256;
- the only trusted key is the one line in this image's
  `/usr/share/rime-os/trust/nixos-bridge-payload.pub`
  (`<key name>:<base64 of the 32-byte public key>`), and the names must agree.
  Nothing is compiled in (`TRUSTED_PAYLOAD_KEYS` is `None` in the product; only
  the NixOS side's OVMF lab build sets it);
- refusals, each by name: `payload-missing`, `payload-key-missing`,
  `payload-key-bad`, `payload-unsigned`, `payload-key-untrusted`,
  `payload-signature-bad`, `payload-corrupt`. No option or environment variable
  skips the check.

**The key** is a dedicated ed25519 key, `rime-bridge-payload-1` (not the
NixOS update path's `rime-cache-1`). Its public line is
`files/system/trust/nixos-bridge-payload.pub`, which `Containerfile.base`
installs and checks. The secret half was generated in a pipe straight into the
`RIME_BRIDGE_PAYLOAD_KEY` secret of the `nix-release` GitHub environment
(deployment branch `main`): it was never written to a disk or shown.
`tests/test-rime-migrate.sh` checks the file is one valid key line and that the
image installs it. A machine on an image released before this key shipped
refuses every `start` with `payload-key-missing`.

`rime migrate` puts no check of its own in front: it passes every argument to
the program unchanged.

**Decided 2026-11-02: one verifier, in the bridge.** An earlier draft of this
release had `rime` verify `manifest.json.sig` against the key file before
running the bridge. The bridge has its own check (above). Two verifiers of one
format can only disagree, and when they do the outer one blocks a move the
bridge would accept; the outer one also checked the directory before the
bridge read it again, which is why it needed refusals for argument spellings
the bridge's parser reads differently. Checking where the payload is read
closes both.

**Decided 2026-11-02 (later the same day): the format is the key file's, not a
compiled-in key.** For a few hours the two sides crossed: this release copied a
bridge that trusted the compiled-in `rime-cache-1` narinfo key over a
domain-prefixed `manifest.sig`, while the NixOS side, at the same minute, moved
its bridge and its producer to `manifest.json.sig` checked against the image's
key file. A payload from that producer was refused `payload-unsigned` by the
bridge this release then shipped (reproduced: the producer's output, signed
with a throwaway key, was refused by that copy and accepted by this one). This
release follows the NixOS side, which owns the program and the producer: a
dedicated key, kept apart from the narinfo key, and the image as the one place
the trust root lives. Neither side changes the format again without the other:
`tests/test-rime-migrate.sh` pins `PAYLOAD_KEY_FILE`, `PAYLOAD_SIG` and
`TRUSTED_PAYLOAD_KEYS = None`.

**Decided 2026-11-02: no `NIX_SIGNER` in this image.** The NixOS release design
(`docs/nixos/updates-and-release.md` on the NixOS branch) once planned for this
release to carry the Nix release workflow's Sigstore identity as well. Nothing on
the bootc side verifies a Nix channel document or closure through `rime trust`;
the only Nix object it ever installs is the bridge payload, and that is checked
by the key above. The identity belongs to the NixOS system's own `rime`.

## Getting the payload

Each Nix release publishes its payload as an OCI artifact:
`ghcr.io/andrenijman/rime-os-nixos-payload:<release ID>`, and `:edge` for the
newest. Each file of the payload is one layer named by its file name, so podman
writes the files back as they were signed:

```
sudo podman artifact pull ghcr.io/andrenijman/rime-os-nixos-payload:edge
sudo mkdir -p /var/tmp/rime-payload
sudo podman artifact extract ghcr.io/andrenijman/rime-os-nixos-payload:edge /var/tmp/rime-payload
sudo rime migrate check --explain --payload /var/tmp/rime-payload
```

The registry is not trusted: the bridge checks `manifest.json.sig` and every
file's sha256 in `/var/tmp/rime-payload` before it reports on or installs
anything (above). Podman keeps its own copy of the artifact until
`sudo podman artifact rm ghcr.io/andrenijman/rime-os-nixos-payload:edge`.
The payload is the release's `plain` system: a moved machine starts with no
feature switched on. The release workflow (`payload` job in
`.github/workflows/nix-release.yml` on the NixOS side) has not published one
yet.
## Disk encryption docs

`rime-luks-enroll --replace-recovery` is a NixOS-side change and does not ship
in this image, so `docs/disk-encryption.md` and `docs/boot-v2.md` still describe
this image's behaviour and are unchanged here.
