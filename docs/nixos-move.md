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
commit f03485259 (sha256 of the file without its first line:
`7b24b07a708c1e97fff4861ae3f24e012a97e63be0dc00bc0af1497b582596b3`). Its tests
(`test_bridge.py`, `test_bridge_d2.py`) are in `tests/rime-migrate/` and run
from `tests/test-rime-migrate.sh`. The fixture uses a synthetic root UUID
(`0b0b0b0b-…`), never a real machine's. That commit is not on the NixOS
branch's tip yet: compare again, by that hash, before this release ships.

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
| any other readable record | refused: a start neither finished nor rolled itself back; the refusal names the last step the record reached and the record's path |
| unreadable as root, or not JSON | refused (fails closed) |
| permission denied as a normal user | left to the root check, which runs next |

**A start that stops before its snapshot step stays refused.** The bridge
writes the record at its first step (`backup`) but removes what it made and
records `rolled back` only for a failure from its snapshot step on. A start
that fails in the backup, while masking the old side's units, or at the first
fsck gate leaves a record with neither `failed` nor `rolled back`, the units it
masked still masked, and these verbs refused. That is the safe answer (nobody
can tell it apart from a start still running), so the refusal says exactly that
and names the record rather than promising a rollback that did not happen.
`rime migrate check` does not read the record. Both belong in the bridge (its
rollback should cover every step after the record is written, and `check`
should report the record): a handover to the NixOS side, not changed here,
because this copy must stay byte for byte the bridge it was taken from.

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

**The key is not shipped yet.** It is a dedicated `rime-bridge-payload-1` key
made in the release environment (never on a workstation), so this image has no
`nixos-bridge-payload.pub` and every `start` refuses `payload-key-missing`;
`check` and `repair-e` are not affected. Shipping it is one file,
`files/system/trust/nixos-bridge-payload.pub`, plus a `COPY` to that path in
`Containerfile.base`; `tests/test-rime-migrate.sh` then checks it is one valid
key line and that the image installs it.

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

## Disk encryption docs

`rime-luks-enroll --replace-recovery` is a NixOS-side change and does not ship
in this image, so `docs/disk-encryption.md` and `docs/boot-v2.md` still describe
this image's behaviour and are unchanged here.
