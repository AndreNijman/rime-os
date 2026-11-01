# Contributing to Rime OS

Thanks for helping. This file covers how a change gets into Rime OS. For
the rules a change has to meet, read [AGENTS.md](AGENTS.md). It is written as
the contract for coding agents, and it binds people just as much: the tiered
image build, what may live in which tier, Secure Boot, SELinux (on the bootc
images; the NixOS side replaces it with the security gates in
`nix/tests/security.nix`), and what counts as a test.

## Where a change belongs

| Change | Repository |
|---|---|
| The image, `rimed` and the `rime` CLI, the agent runtime, the installer, Rime Remote and its relay | this one |
| The desktop: bar, notch, Dashboard, lock screen, settings, notifications | [rime-shell](https://github.com/AndreNijman/rime-shell) |
| rimeos.com | [rime-website](https://github.com/AndreNijman/rime-website) |

A published image (built from rime-os `main`) vendors rime-shell's `main`,
so a desktop fix reaches machines with the next Rime OS image after it merges
there. A build of another rime-os branch vendors the rime-shell branch of the
same name if there is one.

## Issues

Use the issue forms. A bug report needs the output of `bootc status`, because
it names the exact image digest you are running. Security problems do not go
in issues: see [SECURITY.md](SECURITY.md).

## Pull requests

- Branch from `main`, one topic per branch, and open a pull request against
  `main`. Nothing reaches `main` any other way.
- The `PR validation` check must pass, and the branch must be up to date with
  `main` when it merges. Pull requests merge with a merge commit; squash and
  rebase merges are turned off for `main`.
- Files listed in [.github/CODEOWNERS](.github/CODEOWNERS) (the agent
  contracts and the validation workflow) need the maintainer's review.
- CI on a pull request from a fork waits until a maintainer approves the run.
  Fork pull requests never reach the self-hosted kernel runner.
- If your change needs a matching Rime Shell change, give the rime-shell
  branch the same name as yours. The desktop-session check clones a
  same-named rime-shell branch when one exists.

### The release note

Every pull request fills in the `## Release note` section of the template:
one or two sentences about what someone using Rime will notice, or `none`.
That section is the only part copied onto the release page on rimeos.com, and
the pull request title becomes the page title, so write both for users. Leave
out people's names, machine names and quotes.

### Commits

- [Conventional Commits](https://www.conventionalcommits.org/): `feat:`,
  `fix:`, `docs:`, `refactor:`, `perf:`, `test:`, `build:`, `ci:`, `chore:`.
- One logical change per commit. Say why in the body, not only what.
- No AI attribution in commits, pull request text, release notes or source
  files.

## Testing

Run every test in [.github/workflows/pr-validation.yml](.github/workflows/pr-validation.yml)
that touches what you changed; most are plain `tests/*.sh` scripts you can run
from a checkout. In particular:

- Shell scripts: `shellcheck -S warning -x` on anything you touched.
- `rimed/`: `cargo clippy --all-targets --locked -- -D warnings` and
  `cargo test --locked` from `rimed/`.
- A fixed bug comes with a test or an executable assertion that fails without
  the fix. Assertions check the shipped artifact or externally visible
  behaviour, not a copy of the implementation.

`./build-local.sh` builds the images locally with a signed kernel; the README
section "Building the ISOs yourself" covers the installers.

## What a merge does

A merge to `main` that touches `Containerfile*`, `files/**`, `rimed/**`,
`config/**`, `kernel/**` or `.github/**` builds, signs and publishes a new
image, and every Rime machine picks it up on its next `rime update`. Treat
those merges as releases. The one exception is a merge whose commit subject
contains `[skip ci]`, which the maintainer uses for documentation-only changes
under `.github/` (issue forms, templates) so they do not publish an image. A
change to `kernel/kernel.pin` also rebuilds the
kernel, and a change to `Containerfile.core` makes the next update a
multi-gigabyte download for everyone.

## Licence

Rime OS is released under the [MIT licence](LICENSE), and contributions are
accepted under the same licence. A few files come from other projects and keep
their own licences:

- `kernel/kernel-cachyos.spec` is derived from the spec in CachyOS's
  [copr-linux-cachyos](https://github.com/CachyOS/copr-linux-cachyos), a
  GPL-3.0 repository. (Its `License:` tag, GPL-2.0-only, is the licence of the
  kernel it packages, not of the spec file.)
- `kernel/research/Makefile.btf.cachyos` is a copy of the Linux kernel's
  `scripts/Makefile.btf` (GPL-2.0).
- `android/gradlew` and `android/gradlew.bat` are the Gradle wrapper
  (Apache-2.0).

Taking part here means following the [Code of Conduct](CODE_OF_CONDUCT.md).
