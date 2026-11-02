# The move to Rime OS on NixOS: what this image does about it

This release prepares a bootc machine for the move. It does not move anything by
itself and it changes nothing until the owner runs `sudo rime migrate start`.

## `rime migrate`

`/usr/libexec/rime-migrate` is the bridge program (Python, standard library only,
run with `python3 -I`). `rime migrate ARGS…` execs it with every argument passed
through unchanged, so its own `--help` is the reference:

| command | what it does |
|---|---|
| `sudo rime migrate check [--explain]` | read-only: can this machine move, and if not, what blocks it |
| `sudo rime migrate repair-e --confirm` | topology E only: removes an earlier boot trial's leftovers that block a start |
| `sudo rime migrate start --backup DIR --payload DIR` | backs up, installs the NixOS closure beside bootc, adds the GRUB entry; on any failure it removes what it made and records `rolled back` |

The copy here is taken byte for byte from the NixOS side's `rime-bridge.py`
(first line changed to `#!/usr/bin/python3 -I`). Its tests are in
`tests/rime-migrate/` and run from `tests/test-rime-migrate.sh`. The fixture
uses a synthetic root UUID (`0b0b0b0b-…`), never a real machine's.

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

`rime migrate start --payload DIR` verifies `DIR/manifest.json.sig` before it
runs the bridge:

- key: `/usr/share/rime-os/trust/nixos-bridge-payload.pub`, Nix format
  `name:base64(32-byte ed25519 public key)`;
- signature: `name:base64(64-byte ed25519 signature)` over the exact bytes of
  `DIR/manifest.json`, with the same `name`;
- checked with `/usr/bin/openssl pkeyutl -verify -rawin`.

Refusals, each by name: `payload-key-missing`, `payload-key-bad`,
`payload-unsigned`, `payload-signature-bad`, `payload-unverifiable`.

The directory checked must be the directory the bridge installs. The bridge
reads its arguments with Python's argparse defaults, which accept any prefix of
a long option (`--payl DIR`) and keep the last of a repeated one, so `rime
migrate start` also refuses, before anything is checked or run:
`payload-arg-abbreviated` (spell it `--payload`), `payload-arg-repeated`
(`--payload` given twice) and `payload-missing` (no `--payload`; `--help` is
let through, since argparse prints help and exits before `start` runs).
Decided 2026-11-02 after review proved both spellings reached the bridge
unchecked.

Not covered here: the window between this check and the bridge reading
`manifest.json` and the payload files again. Run `start` on a payload
directory only root can write; closing that window (the bridge verifying the
signature itself, over what it then installs) belongs to the bridge.

**The key is not shipped yet.** Until it is, every `start` refuses with
`payload-key-missing`; `check` and `repair-e` are not affected. The image build
asserts the file is absent so that adding it is a deliberate change.

## Disk encryption docs

`rime-luks-enroll --replace-recovery` is a NixOS-side change and does not ship
in this image, so `docs/disk-encryption.md` and `docs/boot-v2.md` still describe
this image's behaviour and are unchanged here.
