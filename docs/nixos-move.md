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
| `sudo rime migrate start --backup DIR --payload DIR` | backs up, installs the NixOS closure beside bootc, adds the GRUB entry; on any failure it removes what it made and records `rolled back` |

The copy here is taken byte for byte from the NixOS side's `rime-bridge.py`
(first line changed to `#!/usr/bin/python3 -I`), at the NixOS branch's bridge
commit 9594161f9 (source sha256 `d307ec75…2a62867`). Its tests
(`test_bridge.py`, `test_bridge_d2.py`) are in `tests/rime-migrate/` and run
from `tests/test-rime-migrate.sh`. The fixture uses a synthetic root UUID
(`0b0b0b0b-…`), never a real machine's.

On a system older than this release `rime migrate` does not exist; on one where
the program is missing, `rime migrate` says so and points at `sudo rime update`.

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
| any other readable record | refused: a start neither finished nor rolled itself back; the refusal names the last step the record reached |
| unreadable as root, or not JSON | refused (fails closed) |
| permission denied as a normal user | left to the root check, which runs next |

`rime-update-check` (the timer that looks for updates) reads the same record
with the same rule: while it refuses, it never asks the registry and reports
`state=none`, so Settings → Updates does not offer a bootc update the machine
would then refuse. The start also masks that timer; this covers a machine where
something unmasked it.

Not wrapped: `bootc upgrade` typed directly. The bridge masks the update timers
and `bootc-fetch-apply-updates` is already masked in the image, so nothing on
the old side runs it by itself.

## The payload signature

The bridge checks the payload itself, at the point it reads it
(`verify_payload`), before `check --payload` reports on it and before `start`
writes anything:

- `DIR/manifest.sig` is `<key name>:<base64 ed25519 signature>` over
  `rime-bridge-payload-v1\n` followed by the exact bytes of
  `DIR/manifest.json`; the manifest names every other payload file by sha256;
- the only trusted key is the one the program compiles in
  (`TRUSTED_PAYLOAD_KEYS`): `rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=`,
  the narinfo key the NixOS update path already trusts;
- refusals, each by name: `payload-missing`, `payload-unsigned`,
  `payload-key-untrusted`, `payload-signature-bad`, `payload-corrupt`. No
  option or environment variable skips the check.

`rime migrate` puts no check of its own in front: it passes every argument to
the program unchanged.

**Decided 2026-11-02: one verifier, in the bridge, and no key file in the
image.** An earlier draft of this release had `rime` verify a
`manifest.json.sig` against a key file it would ship, before running the
bridge. The bridge then gained its own check (above). Two verifiers of one
format can only disagree, and when they do the outer one blocks a move the
bridge would accept; the outer one also checked the directory before the
bridge read it again, which is why it needed refusals for argument spellings
the bridge's parser reads differently. Checking where the payload is read
closes both. The trust root is the constant in the program, pinned by
`tests/test-rime-migrate.sh` (a refresh of the bridge that changes the key or
the domain fails CI), and it is the same public key the NixOS side ships: no
new trust root reaches anyone.

**Decided 2026-11-02: no `NIX_SIGNER` in this image.** The NixOS release design
(`docs/nixos/updates-and-release.md` on the NixOS branch) once planned for this
release to carry the Nix release workflow's Sigstore identity as well. Nothing on
the bootc side verifies a Nix channel document or closure through `rime trust`;
the only Nix object it ever installs is the bridge payload, and that is checked
by the key above. The identity belongs to the NixOS system's own `rime`.

## Disk encryption docs

`rime-luks-enroll --replace-recovery` is a NixOS-side change and does not ship
in this image, so `docs/disk-encryption.md` and `docs/boot-v2.md` still describe
this image's behaviour and are unchanged here.
