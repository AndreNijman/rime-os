# Installing software on Rime OS

```bash
sudo rime install android-tools           # a package from the repositories
sudo rime install ~/Downloads/vendor.rpm  # an .rpm file you downloaded
sudo rime install --allow-unsigned ~/Downloads/app.deb   # a Debian package
sudo rime remove android-tools
rime search wireshark
rime pkg list
```

That is the whole interface. It works for ordinary Fedora packages (CLI tools,
libraries, development toolchains, GUI applications, fonts, services), for a
local `.rpm` file, for a local `.deb` file (the only Linux format some vendors
publish, Claude Desktop among them), and for an AppImage. None of them stops
the OS from updating.

## Why this is not `rpm-ostree install`

Rime is a bootc image. Layering a package with `rpm-ostree` marks the deployment
as locally modified, and from that point on:

```text
error: Upgrading: Deployment contains local rpm-ostree modifications;
cannot upgrade via bootc.
```

Layer one CLI tool and the machine stops receiving OS updates without telling
you. Packages applied with `--apply-live` were worse: they also disappeared on
the next reboot, so you lost both the software and the update path.

`rime install` builds a **systemd system extension** instead: a squashfs image in
`/var/lib/extensions` that systemd overlays onto `/usr` at boot.

* nothing modifies the bootc deployment, so `bootc upgrade` keeps working
* programs land in the real `/usr/bin` (no wrappers, no PATH edits)
* `.desktop` files, icons, man pages, shell completions, systemd units and udev
  rules work, because they sit where the OS already looks
* removing a package is deleting a file; nothing rots in `/usr`
* `rime rollback` (the OS) and `rime pkg rollback` (packages) are independent

The repository sources are Fedora, RPM Fusion and any COPR you enable, and a
path to an `.rpm` file installs that file. There is no Rime package registry to
host, sign or keep online, and `rime-pkg` checks every RPM against a trusted RPM
keyring before it extracts a single file.

## What happens

1. `dnf5 download --resolve` resolves against the **installed image**, so it
   downloads only the dependencies Rime does not already ship. A local `.rpm`
   file is copied in from its cache at this point instead (see below).
2. `rpmkeys` verifies every RPM's signature.
3. `rpm` extracts the packages into a staging tree with `--noscripts`; the
   scriptlets that matter are emulated below, and links a package publishes
   through `alternatives` are recreated (see *What it refuses, and why*).
4. `rime-pkg` rebuilds caches that are a single file describing a whole
   directory (GSettings schemas, the desktop database, the MIME database, GIO
   modules) from the **union** of the image and the new packages, so the
   extension can never hide the OS's own applications. It drops caches whose
   consumers re-scan safely (icon caches, fontconfig) instead.
5. `setfiles` labels the tree for SELinux, so binaries can execute under
   enforcing.
6. It becomes one squashfs image, replaces the old one atomically, and systemd
   re-merges `/usr`.
7. `/etc` files go to the real `/etc`, and your edits are never overwritten: a
   new version lands beside yours as `*.rimenew`.

Everything you requested lives in **one** extension, rebuilt from the requested
list on every change. Separate per-package images would fight over shared
dependencies, and removing one could delete files another package still needs.

## Installing a local `.rpm` file

Some software is published only as an RPM on a website: vendor browsers,
conferencing clients, editors. Point `rime install` at the file:

```bash
sudo rime install ~/Downloads/some-app.rpm
```

`rime install` treats an argument as a file when it ends in `.rpm`, when it
contains a `/`, or when it is an existing file that starts with an RPM header.
That test runs **before** the Flatpak rule, because `org.foo.Bar.rpm` matches
both.

It goes through the same pipeline as a repository package, so it produces the
same result: programs in the real `/usr/bin`, a `.desktop` entry in the app
launcher, icons, MIME associations, systemd units, udev rules and SELinux labels.
Its dependencies still come from the repositories: `rime-pkg` compares the
file's own `Requires` against what the image already provides and downloads only
the remainder.

### The file is copied, and the copy is what gets rebuilt

`rime-pkg` rebuilds the extension from scratch whenever it has to change: on
`rime update`, and on the first boot after an OS version change. A rebuild that
needed the path you typed would fail the moment you unplugged the USB stick or
cleaned up the download.

The install therefore copies the file into `/var/lib/rime/pkg/local/<NAME>.rpm`,
and **every later rebuild reads that copy**. The requested list records
`local:<NAME>`, not a path.

What follows from that:

* Reinstalling from a newer file of the same package replaces the cached copy:
  that is how you update it.
* `rime update` re-resolves the file's **dependencies** against the repositories,
  but it cannot update the file itself; there is no repository to check. A local
  package stays at the version you installed until you install a newer file.
* `sudo rime remove NAME` uses the package name, not the path. The cached copy is
  retired at the same time (kept for one generation, so `rime pkg rollback` still
  works).

### Signatures: refused by default, opt-in per file

Vendors sign their RPMs with keys Rime has no reason to trust, and some do not
sign them at all. Rime refuses them:

```text
rime-pkg: error: cannot verify /home/you/Downloads/some-app.rpm
rime-pkg: error: rpmkeys says: some-app.rpm: DIGESTS SIGNATURES NOT OK
...
rime-pkg: error: If the vendor's own site is where it came from and you accept that:
rime-pkg: error:   sudo rime install --allow-unsigned /home/you/Downloads/some-app.rpm
```

`--allow-unsigned` applies **only** to the files named on that command line. It
never affects repository packages, and the engine does not remember it as a
mode. It remembers that one decision, recorded against that file's exact
checksum. Replace the cached file with different content and the decision no
longer applies.

Because the decision is recorded, `rime pkg list` keeps reporting it:

```text
$ rime pkg list
packages (system extension):
  htop
  some-app  [local file, signature not verified]
```

`rime pkg verify` names them too. If the software is also published in a COPR,
enable that instead, and signature checking stays on.

### What a local RPM does not get

`%post` and the other scriptlets **do not run** the way they would under `rpm`
(see below). For most packages that changes nothing, but some vendor RPMs create
their `/usr/bin` launcher symlink or register a repository in `%post`, and those
steps do not happen: the program lands under `/opt` with a working `.desktop`
entry, but the short command name may be missing from `PATH`. The one exception
is a link the package declares through `alternatives`, which Rime recreates.
Check with `rime pkg info` what was installed and call the real path, or use the
Flatpak if the vendor ships one.

## Installing a local `.deb` file

```bash
sudo rime install --allow-unsigned ~/Downloads/claude-desktop_1.17282.0_amd64.deb
rime pkg list
sudo rime remove claude-desktop
```

Some software ships for Linux as a Debian package and nothing else. Claude
Desktop forced this feature: Anthropic publishes an apt repository and no RPM
at all (five `rpm`/`yum` prefixes under `downloads.claude.ai` answer 404), so
before `.deb` support the only way to have it on Rime was to unpack the `.deb`
into `/usr/local` by hand and run a per-application update timer beside the
OS's own. The image now ships Claude Desktop itself (see *Desktop AI apps*
below), but vendors package many Electron applications the same way.

A `.deb` therefore goes through the **same** pipeline as everything else: the
same system extension, the same cache under `/var/lib/rime/pkg`, the same
requested-package list, the same `rime pkg rollback`. `rime update` rebuilds it
with everything else, and if a later Rime image starts shipping the same
application, `rime-pkg` drops the extension copy instead of leaving it to shadow
the image.

### Rime does not talk to apt

There is no apt client here. Rime **fetches no `.deb`**, resolves no Debian
dependency graph, tracks no Debian suite and knows nothing about
`sources.list`. `rime install ./thing.deb` installs a file you already have, and
that is the whole feature. A repository client would mean maintaining a second
package database, a second signing-trust store and a second release cadence,
and none of those has a rollback story that composes with bootc.

### Its maintainer scripts are never run

dpkg executes `preinst`, `postinst`, `prerm` and `postrm` as root. Rime runs
none of them, and the install says so, naming the ones it skipped. They assume
dpkg, apt and a Debian filesystem. Claude Desktop's own `postinst` writes an apt
source and an AppArmor profile, and that apt source would be the
per-application update channel Rime's design forbids.

That has a cost, and Rime refuses instead of hiding it. A package whose program
exists only because a maintainer script creates it is **not installed at all**:

```text
rime-pkg: error: refusing 'PacketTracer': it ships no program Rime can start —
no executable in /usr/bin and no desktop entry whose Exec is a path the package
ships. Its entry point is created by a maintainer script (preinst postinst
prerm postrm), and Rime never runs those
```

A half-installed package that reports success is worse. `rime install wine`
once did that through the RPM path: twelve `/usr/bin` entries shipped
as dangling symlinks, `/usr/bin/wine` was absent, and the install printed
"done". (The `alternatives` pass described under *What it refuses, and why* now
creates those links.)

A package with no program **and no maintainer script** is a different thing
and installs normally: a font, an icon theme or a set of headers has nothing
to start, and nothing that was ever going to create one. The rule is "no
program, and a maintainer script that might have created one", so a package
with no maintainer script cannot trip it.

### Its dependencies are reported, never resolved

`libgtk-3-0` is not a Fedora package name, and no mapping between the two
naming schemes would be reliable, so Rime does not invent one. `rime install`
prints the `Depends:` line before you accept the package, prints it again as a
warning when it installs, and records it in `rime pkg info`:

```text
rime-pkg: warning: 'claude-desktop': Rime resolved none of its Debian
dependencies (libgtk-3-0, libnotify4, libnss3, xdg-utils, …). Debian package
names do not exist on Fedora; anything Rime OS does not already provide under
another name is yours to install
```

A bundled Electron application needs nothing a desktop Rime install does not
already have. If a package needs a library, install it with `rime install`
first.

### Signatures: a `.deb` carries none that Rime can check

Every `.deb` needs `--allow-unsigned`, because of how Debian's trust model
works: it signs the apt **index** a package is downloaded through, not the
package file. Detach the file from that chain (download it from a website, copy
it off a USB stick) and nothing is left to check. Some vendors embed a
`debsigs` `_gpgorigin` member; Rime carries no deb keyring and no policy saying
which key may sign what, so it does not treat one as verification either.

`rime-pkg` records your acceptance against that file's exact bytes under
`/var/lib/rime/pkg/deb`, as it does for an RPM, so `rime pkg list` and
`rime pkg verify` keep saying which packages Rime never vouched for.

(The image build does verify Claude Desktop, by reconstructing the whole apt
chain with the signing-key fingerprint pinned in this repository. That takes a
network fetch and twenty lines of `Containerfile.core`, and a file on your disk
cannot be put back into that chain.)

### Where the payload may land

A system extension merges `/usr` and `/opt`, so those are the only two
hierarchies a `.deb` may write. `rime-pkg` refuses everything else by name:

| Refused | Reason |
|---|---|
| Anything outside `/usr` and `/opt` (`/etc`, `/var`, …) | a system extension merges nothing else, and a Debian conffile's whole lifecycle is dpkg's |
| `/usr/local` | it is a symlink into `/var` on an ostree system, so a payload directory lands on the symlink instead of inside the merged tree |
| A shared library in `/usr/lib`, `/usr/lib64`, `/lib`, `/lib64` | a Debian build of a library in front of the image's own is unrecoverable without a rollback |
| Anything in a Debian multiarch directory (`/usr/lib/x86_64-linux-gnu`) | Fedora's linker never looks there, and moving it to `/usr/lib64` is the row above |
| Kernel modules and firmware | they need an initramfs and a real deployment |
| A symlink pointing out of `/usr` and `/opt` | it cannot resolve once merged, and it is how an archive escapes its own tree |
| A path Rime OS already provides | an extension may not shadow the image; that would be an OS update |
| An `i386` package on `x86_64` | half a Debian 32-bit userspace is worse than a refusal |

A library under the package's **own** directory is fine, and is what most
`.deb`s ship: that application's RPATH finds
`/usr/lib/claude-desktop/libEGL.so`, and nothing else does.

### Ownership, for a format the rpmdb cannot see

`rime-pkg` normally asks the rpmdb whether a path belongs to the OS. The rpmdb
has no answer for a `.deb`, and none for Claude Desktop **as the image ships
it** either, because the image installs it with `cp -a`, not from an RPM. The
`.deb` route therefore asks the booted ostree deployment (the pristine image
tree, files and all) instead of the running `/usr`, which is an overlay carrying
the extension being rebuilt. `rime-pkg` writes what each `.deb` contributed to
`/var/lib/rime/pkg/deb/NAME.files`, the `rpm -qf` equivalent for those paths.

## Installing an AppImage

```bash
sudo rime install --allow-unsigned ./Thing.AppImage
```

An AppImage is one executable file with a whole filesystem glued to its back.
Rime unpacks it once, at install time, and installs the application inside it:
launcher entry, icon and command. **Rime never runs the AppImage**, at install
time or afterwards.

### Why it is never run

The classic AppImage runtime mounts its own payload with FUSE, and on Rime that
cannot work. A check of the image found `fusermount3`, but **not
`libfuse.so.2`, not `fusermount` (the libfuse2 helper), and not `squashfuse`.**
Double-click a type-2 AppImage on a stock Rime machine and you get the familiar
error:

```
dlopen(): error loading libfuse.so.2
```

There were three ways to answer that, and the one Rime took costs the least:

| | What it means | Why not |
|---|---|---|
| Ship a fuse2 compatibility package | `libfuse.so.2` in the image | A deprecated ABI on every machine in the fleet, whether or not it ever sees an AppImage, so that a format Rime does not control can mount itself |
| Run each launch with `--appimage-extract-and-run` | Unpack on every start | Hundreds of megabytes of I/O before an Electron app's splash screen, done by **executing the vendor's binary**, which this engine refuses to do for a `.deb`'s maintainer scripts and sandboxes for an RPM's `%post` |
| **Unpack once at install time** | `unsquashfs` into `/usr/local` | **Chosen.** No FUSE at install or at run time, no kernel mount, and nothing from the download is ever executed as root |

The payload's offset inside the file is `e_shoff + e_shentsize × e_shnum`, read
out of the ELF header with `od`. That is the number `--appimage-offset` prints,
computed without asking the file about itself. `unsquashfs -o` does the rest.

It also fits the OS. RPM packages have to become a systemd system extension
because `/usr` is read-only composefs; an AppImage does not, because it is
self-contained and `/var` is writable. **An installed AppImage is not part of
`rime-user.raw` and appears in no requested list**, so it survives an OS
upgrade, a `bootc rollback`, an extension rebuild and `rime remove` of every RPM
on the machine.

### Where it goes

| Path | Holds |
|---|---|
| `/usr/local/lib/rime-appimage/NAME/` | the unpacked payload (`AppRun` and everything under it) |
| `/usr/local/bin/NAME` | a generated launcher |
| `/usr/local/share/applications/ID.desktop` | the launcher entry, rewritten to point at it |
| `/usr/local/share/icons/hicolor/**/apps/` | the icon named by the entry's `Icon=` key, and only that one |
| `/var/lib/rime/appimage/NAME.AppImage` | the accepted bytes, kept |
| `/var/lib/rime/appimage/NAME.{trust,files,json}` | the checksum you accepted, what was installed where, and the record |

`/usr/local` and not `/var` directly: on Rime `/usr/local` is a symlink to
`../var/usrlocal`, so it is writable; `/usr/local/bin` is already on `PATH` and
`/usr/local/share` is already in `XDG_DATA_DIRS`, so nothing needs a wrapper or
an `environment.d` drop-in; and SELinux's `/var/usrlocal → /usr/local`
substitution labels the tree `bin_t`/`lib_t` instead of `var_lib_t`, which is
what lets the desktop session execute it.

The launcher sets the four variables the AppImage runtime would have set
(`APPDIR`, `APPIMAGE`, `ARGV0` and `OWD`), because `AppRun` scripts read them.

### `rime update` does nothing to an AppImage. It is pinned.

This is the cost of the format. An installed AppImage stays at the version you
installed. `sudo rime update` does not move it, and it tells you so, by name,
on every run. (`rime update`'s package pass used to return early on a machine
with no system extension, and a machine whose only user software is an
AppImage has none. It now also runs when `/var/lib/rime/appimage` holds a
record, so you do see the line below.)

```
rime-pkg: AppImages are pinned and not updated by this command: obsidian
rime-pkg: to move one, run: sudo rime install --allow-unsigned /path/to/the/newer.AppImage
```

**Rime does not write an updater for AppImages.** *Why Zen is a Flatpak*,
further down, says a tarball or an AppImage "would need Rime to write and
maintain its own updater to keep 'always the latest stable' true". That still
holds, and this feature declines that cost instead of paying it. `rime install`
**reports** a vendor's zsync channel (`X-AppImage-UpdateInformation`, the one
AppImageUpdate follows) **at install time and never follows it**:

```
rime-pkg: it advertises the update channel 'zsync|https://…'; Rime does not follow it — this AppImage is pinned
```

If the software has an RPM, a COPR or a Flatpak, use that instead: those track
upstream through the update path that already exists. Reach for an AppImage
when there is nothing else, and expect to update it by hand.

Self-updating is **impossible**, as well as forbidden. The application runs out
of a root-owned `0755` tree and `$APPIMAGE` points at a root-owned `0644` file,
so an AppImage that tries to rewrite itself gets `EACCES` instead of becoming a
second update channel beside `rime update`.

### Signatures: the same rule as an RPM

**Every AppImage needs `--allow-unsigned`.** Some embed a signature in a
`.sha256_sig` ELF section with the signing key in `.sig_key`. A key taken from
the file it signs proves nothing, so Rime does not accept it as verification:
the conclusion the `.deb` route reached about `debsigs`, and the reason the
image *pins* both AI vendors' key fingerprints. `rime-pkg` records your
acceptance against that file's exact bytes under `/var/lib/rime/appimage`, so
`rime pkg list` and `rime pkg verify` keep reporting where the software came
from, and swapping the file for different content revokes the decision instead
of inheriting it.

### What it refuses

| Refused | Why |
|---|---|
| A **type-1** AppImage (ISO 9660 payload) | Superseded in 2016. Rime unpacks only type 2 (squashfs) |
| A foreign architecture, or a 32-bit runtime | Read from the ELF header. It would never run |
| A payload with **no** `.desktop` file at its root, or more than one | The format allows exactly one. Zero means nothing says what the application is; several means Rime would be choosing on the vendor's behalf |
| A payload with no `AppRun` | That is the entry point every AppImage is required to provide |
| A name that would **shadow** something the OS provides | `/usr/local/bin` comes before `/usr/bin` on `PATH` and `/usr/local/share` before `/usr/share` in `XDG_DATA_DIRS`, so `./firefox.AppImage` would take over the browser for every user on the machine. `rime-pkg` decides image ownership by asking the rpmdb, which has no answer for an AppImage, so it asks about the path the install would *hide* |
| Overwriting any file Rime did not itself install | Under `/usr/local` as much as anywhere else |
| A `.desktop` or icon that resolves **outside** the payload | `.DirIcon` is conventionally a symlink, which is the obvious way to make a root process copy `/etc/shadow` somewhere world-readable |

The unpack strips two things from every payload: **setuid and setgid bits**,
and **ownership**. A FUSE-mounted AppImage is mounted `nosuid`, so preserving a
`4755` helper out of a download would grant *more* than running the AppImage
normally ever does; and a squashfs built on the packager's laptop records uid
1000, which is the desktop user on nearly every Rime machine. Everything lands
`root:root` with no group or other write.

`rime pkg verify` re-checks all of this later, including the one question only
time can answer: whether an RPM installed since has put the same command in
`/usr/bin`, where the AppImage's launcher now sits in front of it.

### Removing one

```bash
sudo rime remove NAME                    # the command name it installed
sudo rime remove ./Thing.AppImage        # or the file it came from
```

Either works: `rime remove` matches the file by checksum, so the same download
in a different directory still resolves. Removal deletes what the manifest
records and nothing outside `/usr/local`.

### What it costs

An installed AppImage occupies about **two to three times** what the file does:
the unpacked tree (the payload uncompressed, so larger than the file it came
in) plus the original, which Rime keeps because the trust marker is a checksum
*of those bytes* and because `$APPIMAGE` has to point at a file that exists. A
1 GB AppImage therefore takes 2–3 GB of `/var`. All of it is machine-local and
none of it touches the image, so it costs the fleet nothing.

## OS upgrades

An extension records the OS version it was built for, and systemd refuses to
merge a mismatched one. That refusal is what makes user packages safe alongside
atomic updates: systemd never overlays a Fedora 43 build onto Fedora 44.

`rime-sysext-rebuild.service` covers the rest: on the first boot after an OS
version change it rebuilds the extension against the new OS. It does nothing on
a normal boot, and if the machine is offline it says so and leaves the rebuild
for later instead of failing the boot.

Rime also records a package compatibility level. When an image starts baking a
package that users may already have in their extension, the level changes and
triggers one rebuild even if the Fedora version is unchanged. The rebuild drops
requested packages the image now provides, so an older extension copy cannot
shadow the OS package.

That level is the only signal a Rime image build gives the package engine:
`VERSION_ID` is the Fedora release and does not move when Rime rebuilds, and the
resolved package set comes from Fedora's repositories, which know nothing about
what Rime baked. To check on a machine that a rebuild happened and was not
skipped, read the level the extension was built at and the unit's own log:

```bash
jq -r .pkg_compat_level /var/lib/rime/pkg/state.json
journalctl -u rime-sysext-rebuild -b
```

A boot that rebuilt logs `extension compatibility changed … — rebuilding`, and
the unit takes minutes instead of finishing in the second it started. A state
file still holding the older level means the rebuild has not run yet: an
offline boot leaves it for the next boot, or for the next `rime update`.

`rime update` also re-resolves user packages, so they receive Fedora security
fixes instead of staying pinned at whatever was current on install day. If
nothing changed it stops early and does not re-merge `/usr`.

## Coming from a layered system

If a machine already has rpm-ostree layered packages, `rime update` says so and
points at:

```bash
sudo rime pkg adopt
```

which rebuilds those same packages as an extension, then runs `rpm-ostree reset`
so the OS can update again. Reboot afterwards to drop the layered deployment.

## What it refuses, and why

| Refused | Reason |
|---|---|
| Kernels, `kmod-*`, `akmod-*` | need an initramfs and a real deployment; they belong in the image |
| `glibc`, `systemd`, `rpm`, `dnf`, `bootc`, `filesystem`, … | overlaying a second copy of the running userspace ABI is unrecoverable without a rollback |
| A **newer** version of something the image ships | that is an OS update, not a package install |
| Anything already in the image | already provided; nothing to do |
| An `.rpm` built for another architecture | it cannot run here |
| A `.deb` whose entry point only a maintainer script would create | Rime never runs maintainer scripts, so the program would not exist |
| A `.deb` shipping outside `/usr` and `/opt`, or a library into a linker path | see the `.deb` section above |
| An `.rpm` no trusted key covers | unless you pass `--allow-unsigned` for that file |
| A file that is not an RPM, is unreadable, or is a directory | refused by name, before anything is copied |

Packages with custom scriptlets install their files correctly, but Rime does not
run arbitrary `%post` scripts against a live system: extraction runs with
`--noscripts --notriggers`. Rime emulates the scriptlets that matter in practice
against the union of image and extension (`ldconfig`, `systemd-sysusers`,
`systemd-tmpfiles`, the GSettings/desktop/MIME/GIO caches, `udevadm`), so
libraries resolve, users and directories exist, and applications appear in the
launcher.

One kind of scriptlet output is recovered: the links a package publishes through
`alternatives` (wine, java, `nc`, the iptables and nftables wrappers). A package
states those only in its scriptlets, so `rime-pkg` runs each `%post` and
`%posttrans` shell body once, as `nobody` under `setpriv` with no way to regain
privileges, with an empty environment, a `PATH` holding only a recorder, and a
hard timeout. Nothing survives the run except the arguments of `alternatives
--install`. `rime-pkg` then creates each link as a direct symlink in the
extension's `/usr` or `/opt`. It never creates a link the image owns, or one
whose target is in neither the package set nor the image, and it skips `<lua>`
scriptlets. `alternatives --display` and `--config` do not know about these
links, so you cannot switch them. If `setpriv`, `timeout` or the `nobody`
account is missing, no scriptlet runs, and the install warns that those programs
will be absent.

Anything else a package does for itself in a scriptlet does not happen:
creating other symlinks, registering an external repository, generating keys,
running a first-time setup. If a package needs one of those to be useful, it
belongs in the image; open an issue.

## Commands

| Command | Does |
|---|---|
| `rime install PKG…` | add packages (`--no-weak-deps`, `--enable-repo=REPO`) |
| `rime install FILE.rpm` | add a local RPM file (`--allow-unsigned` if no trusted key covers it) |
| `rime install FILE.deb` | add a local Debian package (`--allow-unsigned` always; see above) |
| `rime install FILE.AppImage` | unpack an AppImage into `/usr/local` (`--allow-unsigned` always). Pinned: `rime update` never moves it |
| `rime remove PKG…` | remove packages (a local one by its package name, an AppImage by its command name or its file) |
| `rime search TERM…` | search the repositories |
| `rime repo list` | list enabled and disabled RPM repositories |
| `rime repo enable-copr OWNER/PROJECT` | opt into a Fedora COPR for search/install/upgrade |
| `rime repo disable-copr OWNER/PROJECT` | disable an opted-in COPR |
| `rime pkg list` | requested packages and dependency count |
| `rime pkg status` | extension state, what it was built for, whether merged |
| `rime pkg upgrade` | re-resolve everything against the repositories |
| `rime pkg rebuild [--if-needed]` | rebuild for the running OS version |
| `rime pkg rollback` | restore the previous extension |
| `rime pkg verify` | check the extension against its recorded checksum, and each AppImage against the bytes you accepted |
| `rime pkg adopt` | convert rpm-ostree layers into Rime packages |

Read-only verbs work as an ordinary user; anything that writes needs `sudo`.

RPM Fusion Free and Nonfree are enabled in every Rime image, so their packages
work with `rime search` and `rime install` without extra setup. For software from
a Fedora COPR, enable the project once and then use the normal commands:

```bash
sudo rime repo enable-copr OWNER/PROJECT
rime search PACKAGE
sudo rime install PACKAGE
sudo rime repo disable-copr OWNER/PROJECT
```

COPRs are third-party repositories, run by neither Fedora nor Rime. Enabling one
trusts its owner to publish RPMs for that repository until you disable it.
Enabling stores that COPR's signing key in Rime's writable keyring under
`/var/lib/rime/pkg` (the OS keyring is immutable); Rime still verifies every
downloaded RPM against a trusted key and still refuses kernel and core-system
replacements in an extension. Disabling the COPR also removes its key from the
Rime keyring.

## Flatpak

`rime install` also handles Flatpak, chosen by the name you give it:

```bash
sudo rime install org.gimp.GIMP     # reverse-DNS id -> Flatpak (Flathub)
sudo rime install gimp              # plain name     -> RPM (system extension)
sudo rime install ./gimp.rpm        # a path         -> that RPM file
```

The rule is simple and unambiguous: Flathub ids are three or more dot-separated
segments, each starting with a letter, and no RPM is named that way
(`python3.12` has two segments, `java-1.8.0-openjdk` has segments starting with
digits). The file test runs first, so `org.foo.Bar.rpm` is a file and not a
Flathub id. `rime remove` follows the same rules, and `rime pkg list` shows both.

A Flatpak-only install never rebuilds the extension, so it costs nothing.

`rime update` updates Flatpak apps too, system-wide and for the invoking user,
because otherwise a machine could report itself fully up to date while every
graphical application on it was months stale. Skip it with `--skip-flatpak`. A
Flathub outage can never fail an OS update.

## Notes

* Flatpak is still the better choice for sandboxed desktop applications, and
  Bazaar is still the graphical store. The RPM side of `rime install` is for
  what Flatpak is a poor fit for: CLI tools, libraries, headers, drivers'
  userspace, anything that must exist in `/usr`.
* Set `RIME_PKG_FORMAT=tree` to build an uncompressed directory extension
  instead of squashfs. The engine falls back to this when `mksquashfs` is
  unavailable.
* State lives in `/var/lib/rime/pkg` (`requested`, `state.json`, `local/` with
  the cached local RPM files and their trust markers, and a one-generation
  rollback copy of all of it). The extension itself is
  `/var/lib/extensions/rime-user.raw`.
* `state.json` records `local_files` and `unsigned_accepted` so provenance
  survives a reboot and is not something only the person who typed the command
  knows.

## Browsers: one shipped, more on demand

The image ships **Firefox** (RPM, in `core`), and it is the default browser.
**Zen Browser** installs on demand:

```sh
sudo rime install app.zen_browser.zen
```

Zen used to be installed on every machine's first boot. With its Flatpak
runtimes that was about 2 GB (measured 2026-10-04: a fresh install went from
7.8 GB to 9.9 GB at first boot), which on the 12 GB minimum disk is 91% full and
a storage warning on day one. Machines that already have it keep it, and
`rime update` keeps updating it.

**Firefox stays the default.** Zen is a Firefox fork with an opinionated
interface (vertical tabs, workspaces, a compact chrome), and
`files/desktop/xdg/mimeapps.list` keeps `x-scheme-handler/http` and `https`
pointed at `firefox.desktop`.

The build asserts that, because nothing else holds it in place: a Flatpak's
exported `.desktop` can win the handler race depending on XDG data directory
ordering, so without the check an image could change every user's default
browser and nobody would be told.

### Why Zen is a Flatpak

Zen is not in Fedora's repositories and ships no Fedora RPM. The alternatives
are a tarball in `/opt` or an AppImage, and both would need Rime to write and
maintain its own updater to keep "always the latest stable" true.

`rime install ./Thing.AppImage` exists now, and it does **not** change that
answer. It declines the updater instead of writing one: an installed AppImage
is pinned to the bytes that were installed, the one property Zen must not have.
Zen stays a Flatpak. See *Installing an AppImage* above.

As a Flatpak it needs none: `rime update` already runs
`flatpak update --system` (`cmd_flatpak_upgrade` in `rime-pkg`), so Zen tracks
latest stable through the update path that already exists.

It cannot be part of the image at all: `flatpak install` needs a running
system, and bootc seeds `/var` once and never updates it.

### Making Zen your default, per machine

A per-user choice, never an image one:

```sh
xdg-settings set default-web-browser app.zen_browser.zen.desktop
```

### Moving a Firefox profile into Zen

Zen reads a Firefox profile directly (same Gecko, same layout), with one trap.
Zen's **application** version is its own (`1.21.16b`), not the Gecko version it
is built on (`154.0.1`). Gecko's downgrade protection compares the
*application* version in `compatibility.ini`, so a profile last used by Firefox
153 looks like a downgrade to Zen 1.21 no matter how new its Gecko is, and Zen
opens with *"You've launched an older version of Zen Browser"*.

Copy the profile, then **delete `compatibility.ini` from the copy.** Zen
regenerates it and runs its normal profile-upgrade path. Do not delete the
databases, and do not do any of this while the source browser is running.

## Desktop AI apps: shipped with the system

**ChatGPT** and **Claude Desktop** are part of Rime OS. Both are in the image
(stage `5a-aiapps` in `Containerfile.core`), both are on a fresh install, and
both arrive on an existing machine through a normal `sudo rime update`, with no
separate install step and nothing to download by hand.

| | source | how it is installed | where it lands |
|---|---|---|---|
| ChatGPT | OpenAI's rpm-md repo (`persistent.oaistatic.com`) | `dnf5` from the vendor rpm | `/usr/lib/chatgpt`, `/usr/bin/chatgpt` |
| Claude Desktop | Anthropic's apt repo (`downloads.claude.ai`) | deb unpacked into `/usr` | `/usr/lib/claude-desktop`, `/usr/bin/claude-desktop` |

Anthropic publishes no rpm, so the build unpacks the deb instead of installing
it, and unpacking also keeps its maintainer script from running. ChatGPT goes
through `dnf` on purpose: `rime-pkg` asks the system rpmdb whether something is
image-owned, so an rpm-installed ChatGPT makes `rime install chatgpt` refuse to
shadow it. Unpacking it would have left that guard blind to 442 MB of
application.

### They do not update themselves

**A version bump is an image rebuild.** Both vendors package for mutable
distributions, where installing the app also subscribes the machine to the
vendor's repository: OpenAI's rpm ships `/etc/yum.repos.d/chatgpt.repo` with
`enabled=1`, and Anthropic's `postinst` writes an apt source and an
unattended-upgrades snippet. The build removes the first, never runs the
second, and asserts both.

`/usr` is read-only, so neither updater could ever succeed. But `rime-pkg`
builds user system extensions with `dnf` against the **host's** repo set, so an
enabled vendor repo would turn `rime install chatgpt` into a newer build layered
into an extension that shadows the image's own `/usr`: a self-update through a
side channel, which shipping these apps in the image exists to prevent.

If you find `/etc/yum.repos.d/chatgpt.repo` on a machine, a hand-install of the
vendor rpm put it there, not a Rime image.

### Scheme handlers, and the one surprise

`claude://` opens Claude Desktop and `codex://` opens ChatGPT: ChatGPT's scheme
is `codex`, not `chatgpt`. The build asserts both by reading them back out of
`mimeinfo.cache`, because an entry on disk that never reached that cache is not
a registered handler.

ChatGPT's desktop entry also registers `x-scheme-handler/http` and `https` for
itself, so it appears in the "Open With" list for any web link. It does **not**
become the default browser: `files/desktop/xdg/mimeapps.list` keeps http and
https pointed at `firefox.desktop`, and the build asserts that.

Both apps are Electron, and Electron defaults to X11. `/etc/environment` sets
`ELECTRON_OZONE_PLATFORM_HINT=auto` so they run as native Wayland clients under
Hyprland instead of going through XWayland.
