# Why `rime update` used to download the whole OS, and what changed

## The measurement

On the author's L16, running the published `:daily` image, against the registry:

```
$ sudo rime update --check
rime: running: bootc upgrade --check
Update available for: docker://ghcr.io/andrenijman/rime-os:daily
  Version: daily
  Digest: sha256:2bc521664ed5b673392317d4ee01bcab54b16a6999d808bfadc683378a36776d
Total new layers: 153   Size: 5.3 GB
Removed layers:   152   Size: 5.4 GB
Added layers:     153   Size: 5.3 GB
```

**153 of 153 layers, 5.3 GB, on every update.** A one-line edit to a shell
script cost the same download as the first update after a big change.

## Why

`bootc` (via ostree-ext) fetches container layers whole, and skips a layer only
when it already holds a blob with that *exact digest*. Three things combined to
guarantee it never held one:

1. **Everything lived in one image.** `Containerfile.base` carried the CachyOS
   kernel, the firmware set, the whole desktop stack, codecs, the baked
   applications, the font stack, the dev toolchain, and also the branding
   files, the rimed binaries and the vendored shell.

2. **Its rebuild trigger was almost every commit.** The base job's path filter
   covered `files/**`, `rimed/**` and `config/**`, the directories that change
   most.

3. **A rebuild produces new digests even for identical content.** CI builds with
   no layer cache (an earlier attempt at a registry cache was removed as
   unreliable). Every `dnf` transaction rewrites the ~200 MB sqlite rpmdb into
   its layer, and rpm records install timestamps, so "install the same packages
   again" does not reproduce the same bytes.

Editing one line of QML therefore re-issued ~90 layers, and the fleet
re-downloaded the operating system.

## The fix: a third tier

The image is now built in four tiers. `core` is the one this fix added; the
kernel tier came later and has its own section below:

| Tier | File | Contents | Rebuilds when |
|------|------|----------|---------------|
| **kernel** | `Containerfile.kernel` | the kernel itself, compiled from pinned source with a pinned `dwarves` | `kernel/**` changes, i.e. `kernel/kernel.pin` moves |
| **core** | `Containerfile.core` | kernel *install* + MOK signing, firmware, desktop/greeter stack, scx, Bazaar, codecs, baked apps, printing, input methods, fonts, node/gcc for Rime's own tools, zsh/starship, awww/matugen/yazi, OS branding & locale | `Containerfile.core` or `kernel/**` differ from the revision the published `core` was built from · `force_core` · the weekly cron finds a **new** `fedora-bootc` digest |
| **base** | `Containerfile.base` | rimed + rime CLI, sysprofiles, D-Bus/polkit/units, every `files/**` COPY, the vendored Rime Shell (the rime-shell commit the run's "Pin rime-shell" step resolved: `main` on a `main` build) | every run: the path filter still computes a `base` output, but no job reads it |
| **image** | `Containerfile.rime` | edition stamp, gaming-session files, Plymouth theme, final initramfs | every run |

The GPU stack, the Mesa leg and `power-profiles-daemon` used to sit in the
flavor tier. They are in `core` now, and the move changed the download: measured
on the published `:daily` manifest, its flavor tier was
342 MiB over six layers, of which **67 MiB was two `dnf` transactions**
(`power-profiles-daemon`, and the Terra Mesa `distro-sync`) whose only real
content was a rewritten ~200 MB sqlite rpmdb. Every push shipped those to every
machine. In `core` they are inside a digest-pinned parent nobody re-downloads,
so collapsing three images into one, while *adding* the NVIDIA driver to every
machine, made the per-update download smaller.

The base is built `FROM ghcr.io/andrenijman/rime-os:core@sha256:…`. **A digest-pinned `FROM` reuses
the parent's layer descriptors verbatim**: the derived manifest lists the same
digests, so `bootc` recognises blobs it already has and downloads none of them.

That is the whole mechanism. It needs no build cache and no registry cache, and
it cannot silently stop working: if the core digest moves, the layers move; if
it does not, they do not.

### The one row that rebuilds every run got 275 MiB smaller

The `image` row above rebuilds on every run, and CI builds it `--layers=false`:
one squashed layer for the whole of `Containerfile.rime`. Its digest therefore
moves every build and **every machine re-downloads all of it on every update**.
Measured with `skopeo inspect --raw` across all 14 published `rime-<sha>` tags
(manifests only, nothing pulled):

| rime-tier layer, compressed | |
|---|---|
| the 13 builds up to 2026-09-21 | **358.1 – 359.5 MiB** |
| `rime-44c9a5cb`, first with the slim initramfs | **84.5 MiB** |

**~275 MiB off every `bootc upgrade`**, for free, as a side effect of
`initramfs-slim`. It is the cheapest recurring win on this page, because unlike
core, every machine pays this layer on every update.

Two things that measurement also settled, both in
`ROADMAP/evidence/initramfs-slim2-20260922.md`:

* **The initramfs itself is bit-reproducible:** two `podman build --no-cache`
  runs from the same parent produce the same 88,934,458 bytes, `cmp` clean. So
  bootc's `find_vmlinuz_initrd_duplicate`, which digests content, *can* make a
  second deployment cost zero extra ESP.
* **It has never had the chance.** Every one of the 14 published images sits on
  its own `base-<same sha>`; no two Rime image builds have ever shared a
  parent. The lever for both the ESP cost and this download is *"do not
  rebuild `base` when nothing in it changed"*, and dracut has nothing to do
  with it.

### The fourth tier: the kernel

Rime builds its own kernel (`ROADMAP/evidence/kernel-build-20260920.md` says
why: every kernel it shipped before was unable to load a sched-ext scheduler).
That compile is ~45 minutes, and the obvious place for it is `core`, because
the rule below says anything that compiles a third-party program goes there.

It is not in `core`, and this document is the place to record why: **`core` is
built with no layer cache.** There is no `--cache-from` any more, and scheduled
and forced runs pass `--no-cache` outright. A kernel compile inside `core` would
be paid in full on every `core` rebuild, and the "Rebuilds when" column above
says `core` rebuilds for a `Containerfile.core` edit, a `force_core`, or a new
`fedora-bootc` digest. None of those are kernel changes.

The 45 minutes are the smaller cost. Each of those rebuilds would produce a
**different kernel binary**: new `vmlinuz`, new modules, new BTF, akmods rebuilt
against it and re-signed, all as a side effect of an edit that had nothing to do
with the kernel, with nothing tying the kernel to its own inputs.
`kernel/kernel.pin` exists so that the kernel moves when the kernel's inputs move
and at no other time.

The kernel therefore uses the same mechanism `base` uses to consume `core`: a
separately published image, pinned by digest. `Containerfile.core` keeps the
`dnf` transaction that *installs* the RPMs (which is what the rule below is
about) and gets them from the kernel image:

```dockerfile
ARG RIME_KERNEL_IMAGE=localhost/rime-kernel:local
FROM ${RIME_KERNEL_IMAGE} AS kernel-rpms
…
COPY --from=kernel-rpms /rpms     /tmp/rime-kernel-rpms
COPY --from=kernel-rpms /manifest /tmp/rime-kernel-manifest
```

**The fleet download cost is unchanged.** `core` moving is a full multi-gigabyte
download either way; the kernel image itself is never pulled by a user, only by
the `core` build. The change is that `core` stops moving for kernel reasons
and the kernel stops moving for `core` reasons.

Two things cross this new tier boundary and must survive any future edit, in the
same way `/usr/lib/rime-kver` crosses core → image: the manifest's `btf_scx`
verdict, which `core` refuses to install without, and its `kver`, which `core`
checks against the kernel that landed in the rpmdb. Both are copied to
`/usr/share/rime-os/kernel/` so a running machine can answer what it is booting
and what built its BTF.

#### What owning the kernel obliges us to, permanently

This is the half of the decision that is not a build cost, and it is the half
that outlives whoever took it.

Before, security updates arrived by themselves: CachyOS tagged a release, the
COPR rebuilt `kernel-cachyos`, and Rime picked it up on the next `force_core`.
Nobody had to do anything. Now `KERNEL_TAG` and `KERNEL_SRC_SHA256` in
`kernel/kernel.pin` decide which kernel Rime ships, and they move when a person
moves them.

The failure mode is quiet. **An unbumped pin is a kernel that stops receiving
security fixes while every gate in this repository stays green.** The sha256
still verifies, against the old tarball. The BTF gate still passes, because the
old kernel's BTF is still fine. CI is all ticks. Green here means "this is the
kernel you pinned"; it has never meant "this kernel is current", and no other
check in the repository can tell the difference.

Two pieces of CI carry the obligation, so nobody has to remember to bump the
kernel:

* **`tests/check-kernel-drift.sh`** asks whether each pinned input is still
  current: is there a newer stable `cachyos-7.2.x` tag (the security-update
  question), has the pinned `dwarves` moved in or out of Fedora, do all five
  pinned URLs still resolve. It has **three** outcomes: `0` no drift, `1` drift,
  and `2` a lookup could not be performed, which counts as neither a pass nor
  drift, and fails.
* **`.github/workflows/kernel-drift.yml`** runs it weekly, on every change to
  the pin, and on demand; it keeps a single issue open until the pin is current
  again. It compiles nothing.

Do not rely on that check for two things. It does not tell you a CVE exists; it
tells you CachyOS has tagged something you are not on. And it cannot tell you a
kernel you already pinned has become unsafe; only a newer tag can do that.

Bumping the pin costs a ~45-minute kernel-tier rebuild and then a `core`
rebuild, which is the usual ~5 GB to the fleet. That is the real recurring
price of owning the kernel, and it is per security update, not per year.

#### The CI question this tier cannot answer for itself

**When this section was written, nothing built the kernel image in CI, and
that needed a decision rather than an implementation.** The plan was a `kernel`
job in `.github/workflows/build-image.yml` that ran before `core` and passed its
digest as `--build-arg RIME_KERNEL_IMAGE=…@sha256:…`. Writing that job is an
afternoon. Running it was the problem:

> **The build tree is ~100 GB**, measured: `/var` on the development machine
> went 552 GB free to 453 GB during the compile. **A hosted GitHub runner has
> 14 GB.**

A hosted runner could not run the compile at all, before the CPU argument
starts. The two realistic options, with what each costs:

| option | what it costs | what it changes about the product |
|---|---|---|
| **Self-hosted runner on katana** | 20 cores and podman are already there, so the compile is roughly what it is locally. Needs ≥120 GB free on katana's `/var`, which is *tight*; check before committing. Adds a machine the release path depends on being up, and a self-hosted runner executing untrusted PR code is its own security decision. | Nothing. Same kernel, same config. |
| **Restructure the spec to build far fewer modules** (`_build_minimal 1` plus a `modprobed.db`) | Brings the tree within a hosted runner's disk. | **Changes what hardware the kernel supports**, because the module set is built from one machine's `modprobed.db`. That is a product decision about which machines Rime boots on, not a CI optimisation. |

##### ANSWERED 2026-09-20: the self-hosted runner, and the disk objection went away

Andre chose the first option and made room for it. The objection in that row
(*"needs ≥120 GB free on katana's `/var`, which is tight"*) was **false**:
katana's `/var` had **51 GB** free, so the build could not have run there at
all. The build no longer uses `/var`.

Katana's second drive was repartitioned: the `games` filesystem shrank from
1362 GiB to 862 GiB (it was 5 % used) and the freed 500 GiB became a partition
mounted at **`/var/lab`**, which is where the runner's work directory and its
container storage live. 484 GB free. The Windows, ESP and recovery partitions
on that disk were not touched; `ROADMAP/evidence/katana-runner-20260920.md`
has the before/after tables, the GPT backup location, and the integrity proof.

The standing costs of this choice:

* **GitHub Actions minutes for this tier: zero.** The compile runs on hardware
  Andre already owns, at `-j$(nproc)` = 20 rather than the 12 the tier was
  measured at.
* **A release now depends on katana being up**, the cost this table exists to
  make visible. The dependency moved from "somebody's laptop, by hand" to "a
  named machine, automatically", which is better and still a cost.
* **The security decision that row flagged was taken.** `rime-os`
  is public, so fork pull requests are untrusted code. They never reach the
  runner: the repository requires approval for **all** external contributors,
  and every self-hosted job additionally refuses a PR whose head is a fork.
  Fork PRs keep getting full CI on `ubuntu-24.04`. The runner is ephemeral and
  runs as an unprivileged user that cannot read Andre's home or reach his LAN,
  and a probe workflow (`katana-probe.yml`) asserts all of that on every change.
  Details, including what is *not* covered, are in the evidence file.

`.github/workflows/kernel-build.yml` is what runs there, and it is now the
producer. It builds `localhost/rime-kernel:ci`, pushes it once to an immutable
tag (`kernel-<kver>-<sha7>`), moves the floating `:kernel` tag on `main` and
`roadmap/**`, and prints the `ARG RIME_KERNEL_IMAGE=…` line for that digest in
its run summary. The job holds `packages: write` for its own length only; the
runner stays unprivileged and ephemeral, and no credential outlives the job.
`Containerfile.core` pins the digest (`ARG RIME_KERNEL_IMAGE=ghcr.io/andrenijman/rime-os@sha256:…`,
from run 35557283953), and `build-image.yml`'s core job reads that line and
fails unless it names a digest. Moving the kernel therefore takes two steps: a
kernel-build run, then a commit that pastes its `ARG` line into
`Containerfile.core`.

### The rule for new content

> If it runs `dnf`, downloads, or compiles a third-party program, it belongs in
> `Containerfile.core`. The base may only COPY repo content, compile rimed, and
> assert against those.

Split a `RUN` that needs both **across the two files**; do not move it
wholesale. Four in the original file straddled the line and are now pairs:

- the zsh/starship verification (packages in core, templates in base)
- the Windows-boot helper (`efibootmgr` in core, helper + sudoers in base)
- the icon-cache and `dconf` rebuilds (they must follow the COPYs, so: base)
- the Hyprland template guards (base)

Two contracts cross the tier boundary and must survive any future edit:
`/usr/lib/rime-kver` (core → `Containerfile.rime`'s initramfs rebuild, the image
job's `sbverify` and `Containerfile.release`) and
`/usr/share/rime-os/secureboot/kernel-signed` (core → the core, base and image
jobs' verification, `Containerfile.release` and the installer). Both are plain
files under `/usr`, and CI asserts both.

### What the AI apps cost, and why they left

From 2026-09-11 to 2026-10-04 the two desktop AI apps (`docs/packages.md`)
shipped in `core`, as third-party downloads must by the rule above, and were the
largest single addition the tier ever took. Measured in a scratch
`fedora-bootc:43` container: **1.3 GB for `/usr/lib/chatgpt` and 548 MB for
`/usr/lib/claude-desktop`**, ~1.9 GB of payload before compression, plus the
Claude Code CLI beside them.

That made every machine carry and download them whether or not anyone used
them, and tied their versions to core rebuilds, the most expensive update the
fleet takes. Moving them up a tier was never an option: a `dnf` transaction
above `core` puts an rpmdb-sized layer into every user's next update. So they
left the image instead. `rime install chatgpt` and `rime install
claude-desktop` fetch them from their vendors, verified against the same pinned
fingerprints, into the user's system extension, which `sudo rime update`
refreshes on the vendors' own cadence; `rime install claude-code` puts the CLI
in the user's `~/.local`, where it updates itself. The machines that use them
pay for them, once.

### What the systemd-boot pivot added

The smallest thing `core` has taken, recorded because the pivot sounds
expensive and the packages are not what makes it so. Measured with
`dnf5 install --assumeno` inside `ghcr.io/andrenijman/rime-os:daily`:

```
Installing:  systemd-boot-unsigned  248.9 KiB
             systemd-ukify           99.9 KiB
Installing dependencies: python3-cffi, python3-cryptography, python3-pefile,
             python3-ply, python3-pycparser, python3-zstandard
Total size of inbound packages is 3 MiB. … 12 MiB extra will be used.
```

`checkpolicy`, `policycoreutils` and `python3-setools` were **already
installed**. The transaction names them only to make the dependency explicit,
the way it names `efibootmgr`, so a future change that drops them fails the
build instead of turning the boot blessing into a silent rollback loop.

The `rime_sdboot` SELinux module is the other half. `semodule -N -i` grows
`/etc/selinux` by **466 bytes**, but rewrites `policy.35`, which is **3.8 MB**,
and an OCI layer carries a changed file whole. That is why `core` compiles the
module and the files tier does not: 4 MB once, instead of 4 MB in every
thin-tier update.

`systemd-boot-unsigned` has to be in the shipped image and cannot be a
build-only tool like `sbsigntools`: `bootc install --bootloader systemd` copies
the loader **out of the image being installed**. With the package absent, bootc
printed "Installing bootloader via systemd-boot", exited 0, and produced an ESP
with an empty `/EFI/systemd/` and no loader binary anywhere: an unbootable disk
from a successful install.

### What the screen reader added

The other end of the same scale, recorded next to the AI apps because it is
the opposite case. P2-003's acceptance line names a screen reader, and until
`Containerfile.core`'s `5a-a11y` stanza there was none in the image. Measured
the same way (`dnf5 install --assumeno orca` inside
`ghcr.io/andrenijman/rime-os:daily`):

    Installing:            orca              21.3 MiB
    Installing dependencies:
                           brlapi            594.9 KiB
                           python3-brlapi    324.7 KiB
                           python3-louis      43.4 KiB
                           python3-pyatspi   414.5 KiB
    Total download 4 MiB · 23 MiB installed

**23 MiB against the AI apps' 1.9 GB**, in the same tier, for the component
without which a blind user cannot use the machine at all. Nothing in that
transaction pulls `speech-dispatcher` or `espeak-ng`, which is the image's own
rpmdb confirming they were already present as transitive dependencies of gtk4
and Qt, so the delta is the reader alone.

The rule this illustrates: the tier argument is about DOWNLOAD SIZE PER UPDATE,
and it says nothing about whether a thing is worth shipping. A 23 MiB addition
to core costs the fleet nothing measurable; a 1.3 GB one is why the AI apps
left it.

### The weekly rebuild

The cron used to rebuild unconditionally. That would now be the dominant cost:
six days of ~50 MiB updates and one Monday of 5 GB, most weeks for nothing.

Core therefore stamps the digest of the `fedora-bootc` image it was built from
as `org.rimeos.fedora-bootc.digest`, and the scheduled run compares that label
against the live upstream digest. Same digest → no rebuild. Security updates
still arrive: a Fedora base respin *changes* the digest, which is the trigger.
The weekly run does not pick up COPR or RPMFusion moving without a Fedora respin
until the next core-relevant change; run the workflow with `force_core=true` to
take those immediately.

### What must NOT be in the core path filter

`build-image.yml` itself. It was at first, and the next CI-only commit (a retry
around `podman push`) rebuilt core and reissued the whole ~5 GB image to every
machine, for a change that could not alter core's content by one byte.
Rebuilding core is the most expensive thing this workflow can do, so only core's
real inputs (`Containerfile.core`, `kernel/**`) drive it.

A workflow change that alters *how* core is built (a new `--build-arg`, a
different base tag) therefore does not rebuild it on its own. That is what
`force_core=true` is for: an explicit action for the rare case instead of a
multi-gigabyte download for the common one.

### Measuring it

Every image build writes an update-cost table into the GitHub Actions run
summary (the "Report update cost" step): total layers, how many are inherited
from core, and how many are new. If a future change pushes content back down
into core, that number climbs, and the regression shows the week it happens
instead of the month someone next runs `rime update` on a hotel connection.

## Also changed: the firmware half of `rime update`

`rime update` ran, unconditionally, on every invocation:

```sh
fwupdmgr refresh --force     # re-download the entire LVFS metadata index
fwupdmgr update -y           # full device enumeration + update pass
```

`--force` means "ignore the cache age". fwupd considers its metadata stale after
24 hours, so forcing it re-downloaded tens of MB of signed XML every run. The
update pass then enumerated every device on a machine that, nine runs in ten,
had nothing to install.

Now:

- `fwupdmgr refresh` **without** `--force`, honouring fwupd's own cache window;
- `fwupdmgr get-updates` first, and the update pass only if it reports something;
- `rime update` reads fwupd's exit codes correctly. `fwupdmgr` returns **2** for
  "nothing to do" and **3** for "nothing found", and both are the *normal*
  outcome on a current laptop. The old code took the maximum of every exit code,
  so dropping `--force` alone would have made `rime update` report failure on
  its most common path.

New flags: `rime update --check` (report only, download nothing),
`--skip-firmware`, `--firmware-only`.

## Also changed: `rime update` requires root

`update`, `rollback`, `pin` and `fan restore --local` now refuse to run
unprivileged, before any hardware probe or subprocess:

```
$ rime update --check
rime: 'update' changes the booted system and must run as root.
       try:  sudo rime update --check
       (being in the wheel group is not enough — bootc writes to /ostree and /boot,
        so the command itself has to run with privileges.)
```

Previously they reached `bootc`/`ostree` and failed there with a bare permission
error that never mentioned sudo, and `rime update` then ran its firmware half
anyway, printing a wall of output and possibly exiting 0 having updated
nothing.

The rule covers those verbs and no others. `rime tier`, `status`, `battery`,
`fan`, `game` and `doctor` stay usable unprivileged: Rime Shell's power tab
shells out to `rime tier` as the session user, and mutations go through rimed's
polkit-authorised D-Bus API, which is how an unprivileged desktop is supposed to
change power state. Gating those would break the desktop's power controls to
improve an error message.

## Also changed: the one update that migrates the boot path

`rime update` on a machine still booting GRUB runs the in-place move to
composefs + systemd-boot **instead of** `bootc upgrade` (docs/boot-v2.md,
"Migrating a machine that already exists"). Two facts about its cost, written
down here so that nobody finds them out on a full disk:

* **It downloads nothing.** The migration deploys the digest the machine is
  already running, and the install has to run as a container of that image, so
  the engine copies the image out of bootc's own storage with `bootc image
  copy-to-storage`: local, with no registry round trip. It deletes that copy as
  soon as the install succeeds.
* **It roughly doubles the image's footprint on disk, and leaves it that way.**
  The ostree repo and deployment stay, because they are the recovery path, and
  `/composefs` is a second copy of the same content. The migration never deletes
  the old copy: a machine that can still boot GRUB cannot be bricked by this
  change. Reclaiming the space is a later, separate decision, and until somebody
  takes it a migrated Rime machine carries about 15 GB it did not carry before.

That is a disk cost, so it does not move the download numbers above; on katana,
whose `/var` is tight, it is the number that matters.

## CI build time: what was measured, and what helped

Profiled rather than guessed (run 30775672845):

| step | base | daily |
|------|------|-------|
| build | 21.9 min | 5.1 min |
| **push** | **12.9 min** | **14.3 min** |
| disk cleanup | 2.1 min | 1.6 min |

Pushes dominate, and they are bandwidth-bound (~14 MB/s to GHCR), not CPU-bound.
Counting blob operations found the waste: every image was uploaded **twice**,
once as `:<tier>-<sha>` and again as the friendly tag, 166 blobs and then 166
more, with zero reused.

**What helped**

- *Push once, then tag with a registry-to-registry copy.* A `docker://` →
  `docker://` copy knows its blob digests up front, so it skips every layer and
  writes only a manifest. Measured: base push 12.9 → **6.2 min**.
- *Stop pre-emptively cleaning the runner disk.* The step was written when
  runners had ~14 GB free. They now have 145 GB with **88 GB free before
  cleaning**, so it was reclaiming 31 GB nobody needed at 4.2 min × 7 jobs. It
  now only sweeps below a threshold.

**What did not help, and why (recorded so nobody retries it)**

Consolidating the tiers into one repository, in the hope the registry would skip
inherited layers. It does not, and no client-side flag changes that:

    push core (zstd:chunked) ............. 99 blobs copied,  0 skipped
    push base into the SAME repo ........ 159 blobs copied,  0 skipped
    push the IDENTICAL core ref again .... 99 blobs copied,  0 skipped
    skopeo instead of podman ............. 159 blobs copied,  0 skipped
    no --compression-format at all ....... 159 blobs copied,  0 skipped

Compressing out of `containers-storage` only produces the blob digest *after*
compression, so there is nothing to ask the registry about first. The remaining
push cost is inherent: ~5 GB compressed and uploaded per image.

When there were three editions, building the flavors inside the base job was
also considered (it would have removed one 5 GB upload and three downloads) and
rejected: each flavor would have built its own base, so the three editions could
no longer be proven to sit on one identical, verified base image. That guarantee
was worth more than the minutes.
