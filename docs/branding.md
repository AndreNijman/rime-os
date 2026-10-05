# Rime OS Branding

## The mark: "spark"

Rime OS uses a single logomark, the **spark**, drawn in several colorways. The
wordmark is the letterspaced "Rime OS".

## Color semantics

| Colorway | Hex (highlight) | Used for | Represents |
|----------|-----------------|----------|------------|
| **Chartreuse** | `#D9F99D` | Rime OS | Everyday |
| **Gold** | `#FDE047` | legacy Gaming accent | Power |
| **Mono (black)** | n/a | neutral | Light backgrounds, print, single-color contexts |
| **Mono (white)** | n/a | neutral | Dark backgrounds, single-color contexts |

Rime publishes ONE image, so chartreuse is the colour of the product: the
default for the boot splash and the greeter. Both follow the owner's own accent
once one is known. The greeter reads it from `/var/lib/rime-greet/accents/<user>`,
and the initramfs starts the splash in the matching one of 24 accent themes
derived from chartreuse (`files/dracut/rime-plymouth-accent`). There is no
edition left for gold to denote.

Gold survives for one reason: a machine still booting a pre-merge image reports
`VARIANT_ID=gaming`, and rime-greet maps that to the gold accent and
`spark-gold.png` so the machine keeps its identity until it updates. New artwork
should not use gold to mean anything.

The mono variants exist for any context that needs a neutral, single-color mark.

## Asset inventory

All assets live under `files/branding/`.

### Logos: `files/branding/logos/<colorway>/`

Colorways: `gold/`, `chartreuse/`, `mono-black/`, `mono-white/`.

Each colorway provides:

- `rime-spark-<colorway>.svg`: vector source (512×512 viewBox)
- `rime-spark-<colorway>.png`: 512×512 base raster
- `rime-spark-<colorway>-{16,32,64,128,256,512,1024}.png`: icon sizes

(Mono files use the suffix `black` / `white`, e.g. `rime-spark-black-256.png`.)

### Plymouth boot themes: `files/branding/plymouth/`

**One splash ships, in 25 themes.** `Containerfile.rime` copies
`rime-os-chartreuse` and the 24 `rime-os-accent-NN` themes (one per 15° of hue),
runs `plymouth-set-default-theme rime-os-chartreuse`, and rebuilds the initramfs
with `--add "plymouth rime-plymouth-accent"` so the splash can start in the
owner's accent. Nothing installs the gold theme, because there is no second
image to install it into.

- `rime-os-chartreuse/`: the default splash, and the source of every accent
  theme
- `rime-os-accent-00/` to `rime-os-accent-23/`: chartreuse hue-rotated by
  `make-accent-themes.sh`
- `rime-os-gold/`: source art only, installed by no Containerfile. It is the
  same animation in the other colourway, kept because deleting commissioned art
  to tidy a build is a bad trade. Its presence is not evidence that a second
  image exists.

Each theme contains its `.plymouth` descriptor, the shared `rime-os.script`
(identical in every theme but for its highlight colour), and the images the
script loads: the spark as 1x and `-hd` masters (`spark.png`, `spark-hd.png`),
two out-of-focus sparks (`spark-blur.png`, `spark-soft.png`), `halo.png`,
the password dot (`bullet.png`, `bullet-hd.png`), the wordmark
(`wordmark.png`, `wordmark-hd.png`) and a fallback prompt (`prompt.png`,
`prompt-hd.png`) for an initramfs without a label plugin.
`make-splash-art.sh` draws them for a colourway; `make-accent-themes.sh`
derives the 24 accent themes from chartreuse.

**Animation: "Focus".** On plain black, the spark comes into focus out of a
soft glow of its own light, a halo blooms behind it and settles, and the
`Rime OS` wordmark fades in; the halo then breathes slowly while the machine
boots. Nothing moves: the script scales every image once, and each frame only
changes opacities along curves of real elapsed time, at 60 Hz rather than
plymouth's default 50. Plymouth truncates sprite positions to whole pixels and
has no vsync, and the "Convergence" comet animation this replaced was choppy
for exactly those reasons (`rime-os.script` records the measurements). The
splash handles a LUKS prompt: it dims, the prompt takes the wordmark's place,
and typed characters show as dots, never as text. Shutdown, reboot and update
modes show the settled splash.

The generic install, as on a mutable Fedora system:

```sh
cp -r rime-os-chartreuse /usr/share/plymouth/themes/
plymouth-set-default-theme -R rime-os-chartreuse   # -R rebuilds the initramfs
```

The image build differs: it copies the accent themes too, runs
`plymouth-set-default-theme` without `-R`, and runs dracut itself with
`--add "plymouth rime-plymouth-accent"`, because a bootc image regenerates its
initramfs at build time. Kernel args need `quiet splash`, which the image sets
in `/usr/lib/bootc/kargs.d/20-rime-plymouth.toml`. See
`files/branding/plymouth/README.md` for install and test details.

### Previews

`files/branding/plymouth/previews/preview-gold.gif` and
`preview-chartreuse.gif`. Regenerate with:

```sh
cd files/branding/plymouth
./render-preview.sh rime-os-gold previews/preview-gold.gif
./render-preview.sh rime-os-chartreuse previews/preview-chartreuse.gif
```

`render-preview.py` (which the `.sh` wraps) is a frame-exact simulation of
`rime-os.script` on plymouth's own pixel arithmetic, and also writes a 60 fps
MP4, a contact sheet and per-frame metrics. It needs python3 with numpy and
Pillow, and ffmpeg.

### Wallpaper: `files/branding/wallpapers/`

`rime-wallpaper-default.jpg`: the default desktop wallpaper (3258×2160).

---

# Distro identity: de-branding Fedora / CachyOS

Rime OS is built `FROM quay.io/fedora/fedora-bootc:43` and swaps in a CachyOS
performance kernel, which Rime now compiles itself from pinned CachyOS source
(`Containerfile.kernel`, `kernel/kernel.pin`). Both upstreams leave their name
in places the user can see. **No user-visible surface may say "Fedora" or
"CachyOS".** This section is the complete inventory: every surface, what it
said, what it says now, and where the fix lives.

Some surfaces cannot be changed; the last table lists each one with its
reason.

## Where each surface is fixed

Fixes land in one of two places:

- **image**: `Containerfile.core` for OS-level branding (os-release,
  fedora-release, issue), `Containerfile.base` for anything COPY'd out of
  `files/`. Applies to every deployment created from a newly built image.
- **runtime**: `files/scripts/rime-debrand-runtime.sh`. Applies to a system that
  is **already installed**. Three of these surfaces live *outside* the ostree
  deployment (EFI NVRAM, `/boot/loader/entries`, a local `/etc` modification), so
  `bootc upgrade` never replaces them, however good the image gets.

## The inventory

| # | Surface | Was | Is now | Fixed in | Where |
|---|---------|-----|--------|----------|-------|
| 1 | GRUB boot menu entry (`/boot/loader/entries/*.conf` → `title`) | `Fedora Linux 43 (Forty Three) (ostree:0)` | `Rime OS (ostree:0)` | image **+** runtime | ostree derives the title from the deployment's os-release `PRETTY_NAME`; `Containerfile.core` sets it. Existing entries: `rime-debrand-runtime.sh` |
| 2 | Firmware boot entry label (`efibootmgr`) | `Boot0002* Fedora` | `Boot####* Rime OS` | image **+** runtime | bootupd's `get_product_name()` reads `/etc/system-release`; `Containerfile.core` rewrites `/usr/lib/fedora-release`. Existing NVRAM: `rime-debrand-runtime.sh` |
| 3 | `os-release` `NAME` / `PRETTY_NAME` | `Fedora Linux` / `Fedora Linux 43 (Forty Three)` | `Rime OS` / `Rime OS` | image | `Containerfile.core` os-release `sed` |
| 4 | `os-release` `VERSION` | `45 (Forty Five)` (Fedora codename) | `45`, Fedora's own `VERSION_ID` | image | same `sed`, which reads the number from `VERSION_ID` rather than writing it in |
| 5 | `os-release` `LOGO` | `fedora-logo-icon` | `rime-os-logo` | image | same `sed`; the icon itself is installed into `hicolor` from `files/branding/logos/chartreuse/` |
| 6 | `os-release` `CPE_NAME` | `cpe:/o:fedoraproject:fedora:45` | `cpe:/o:rimeos:rime_os:45` | image | same `sed`, plus `/usr/lib/system-release-cpe`; the number is `VERSION_ID` |
| 7 | `os-release` `DOCUMENTATION_URL` / `SUPPORT_URL` | `docs.fedoraproject.org` / `ask.fedoraproject.org` | the rime-os GitHub repo | image | same `sed` |
| 8 | `os-release` `REDHAT_BUGZILLA_*` / `REDHAT_SUPPORT_*` | `"Fedora"` ×4 | deleted | image | `sed -e '/^REDHAT_/d'` |
| 9 | `os-release` `ANSI_COLOR`, `DEFAULT_HOSTNAME`, `HOME_URL`, `BUG_REPORT_URL` | Fedora blue / `fedora` / fedoraproject.org / bugzilla | Rime chartreuse / `rime` / rime-os repo | image | same `sed` (pre-existing) |
| 10 | `/etc/system-release`, `/etc/redhat-release`, `/etc/fedora-release` (all → `/usr/lib/fedora-release`) | `Fedora release 45 (Forty Five)` | `Rime OS release 45` (the number is `VERSION_ID`) | image | `Containerfile.core`. **Content only: the file is NOT renamed**, the symlink chain and `[ -f /etc/fedora-release ]` probes must keep working, and this is the string bootupd turns into the firmware label. The image now **re-creates** those three links itself (plus `os-release`, the CPE and `issue{,.net}`) rather than inheriting them from the base layer, and asserts the branded string *through `/etc`*: writing only `/usr/lib` and trusting an inherited hardlinked symlink is what failed every weekly `core` build from 2026-08-17 on |
| 11 | VT login banner `/etc/issue`, `/etc/issue.net` (→ `/usr/lib/issue*`) | `\S` + `Kernel \r on \m (\l)` → printed `7.1.3-cachyos1.fc43.x86_64` | `\S` only → prints `Rime OS` | image | `Containerfile.core`. agetty expands `\S` from `PRETTY_NAME`. Writing `/usr/lib/issue` (not `/etc/issue`) keeps the symlink intact |
| 12 | `fastfetch` kernel line | `Linux 7.1.3-cachyos1.fc43.x86_64` | `7.1.3` | image | `files/system/fastfetch/config.jsonc`: the `kernel` module replaced by a `command` module running `uname -r \| cut -d- -f1` |
| 13 | `fastfetch` OS line + ASCII logo | Fedora `F` logo (auto-detected from `ID`) | Rime spark + `Rime OS 43` | image (pre-existing) | `config.jsonc` pins `logo.source` to `/etc/fastfetch/rime-logo.txt`; bare `fastfetch` picks up `/etc/fastfetch/config.jsonc`, verified |
| 14 | Kernel-version stamp file | `/usr/lib/rime-cachyos-kver` | `/usr/lib/rime-kver` | image | `Containerfile.core` writes it; `Containerfile.rime` and `Containerfile.release` read it |
| 15 | Installer completion screen | *"look for \"Fedora\" / \"Rime\""* | *"pick it from the one-time boot menu (F12 on ThinkPads)"* | image | `installer/rime-install` |
| 16 | Live-ISO GRUB menu | already `Install Rime OS` | unchanged | image (pre-existing) | `installer/build-live-iso.sh` |
| 17 | Plymouth boot splash | `rime-os-chartreuse` and its 24 accent themes (gold is source art only), wordmark `Rime OS` | unchanged | image (pre-existing) | `files/branding/plymouth/` |
| 18 | Greeter (`rime-greet`) | no distro string at all | unchanged | n/a | verified clean |
| 18a | `hostnamectl` "Operating System" | `Fedora Linux 43 (Forty Three)` | `Rime OS` | image | reads os-release `PRETTY_NAME`; covered by surface 3. Its "Kernel:" line still shows the CachyOS release string; see the unfixable table |
| 18b | `neofetch` / `screenfetch` / `lsb_release` | n/a | n/a | n/a | **not installed** in the image (verified). `fastfetch` is the only fetch tool, and it is handled by surfaces 12–13. If one is ever layered in, it will auto-detect the Fedora logo from `ID` exactly as bare `fastfetch` would, and needs the same `logo.source` pin |
| 19 | Local `/etc/os-release` override on installed systems | a hand-written file with `VERSION="43 (Forty Three)"`, `LOGO=fedora-logo-icon`, `REDHAT_*`, fedora URLs | branded, and removed entirely once the image's own os-release is branded | **runtime** | `rime-debrand-runtime.sh`. See the warning below |

### The `/etc/os-release` trap (surface 19)

On the running install `/etc/os-release` is a **regular file**, not the image's
symlink into `/usr/lib`. On ostree, that is a local `/etc` modification, and
ostree **3-way-merges `/etc` forward into every future deployment**. So the
override outlives the image fix: after upgrading to a branded image it keeps
shadowing `/usr/lib/os-release`. It pins the stale `VERSION="43 (Forty Three)"`
and the Fedora `LOGO`/URLs, and, worst of all, it pins `VERSION_ID=43` across a
future rebase to an F44 base (which would break `$releasever`).

`rime-debrand-runtime.sh` handles both states:

- image os-release already branded → **removes** the override and restores the
  symlink `../usr/lib/os-release`;
- image os-release not yet branded → **refreshes** the override with a fully
  branded copy and stamps it `TRANSITIONAL`, with a comment telling you to delete
  it after the next `bootc upgrade`.

## Why `ID=fedora` stays

`ID` and `VERSION_ID` are the only os-release keys left un-branded, on purpose.
They are **machine-facing**: nothing that renders on screen reads them (the boot
menu, Plymouth, the greeter, fastfetch's `{name}`, and agetty's `\S` all read
`PRETTY_NAME`).

The textbook derivative pattern is `ID=rime` + `ID_LIKE="fedora"`. Tested, it
**breaks the build**. `dnf copr` derives its chroot name from
`ID`-`VERSION_ID`-`arch`, and `ID_LIKE` does not help:

```
$ sed -i -e 's|^ID=.*|ID=rime|' -e '/^ID=rime/a ID_LIKE="fedora"' /usr/lib/os-release
$ dnf5 -y copr enable bieszczaders/kernel-cachyos
Chroot not found in the given Copr project (rime-43-x86_64).
You can choose one of the available chroots explicitly:
 …
 fedora-43-x86_64
```

The test above hit `bieszczaders/kernel-cachyos`, which the build no longer
uses now that Rime compiles its own kernel. The build path still enables seven
COPRs (`bieszczaders/kernel-cachyos-addons`, `ublue-os/akmods`,
`ublue-os/packages`, `shdwchn10/xpadneo`, `sdegler/hyprland`,
`errornointernet/quickshell`, `zeno/scrcpy`), and a user can enable more later.
`$releasever` is safe, checked separately: it comes from `VERSION_ID`, not `ID`
(`dnf5 --dump-variables` → `releasever = 43` with `ID=rime`). That narrows the
blast radius without removing it.

Verdict: **`ID=fedora` and `VERSION_ID=43` stay.** The user-visible mandate is
met without them. Before anybody revisits this, every `dnf copr enable` call
site needs an explicit `fedora-43-x86_64` chroot argument, and the whole build
has to run again.

`Containerfile.core` asserts this invariant at build time: the branding step
fails the build if any line other than `ID=fedora` still matches `fedora`:

```sh
test "$(grep -ci fedora /usr/lib/os-release)" = 1
grep -qx 'ID=fedora' /usr/lib/os-release
```

## What CANNOT be fixed

| Surface | Shows | Why it cannot change |
|---------|-------|----------------------|
| `uname -r`, `/usr/lib/modules/<kver>/`, `/usr/src/kernels/<kver>/`, `/usr/share/licenses/kernel-cachyos-core/`, `/proc/version` | `7.1.3-cachyos1.fc43.x86_64` | The release string is compiled into the kernel package (`CONFIG_LOCALVERSION` + the RPM dist tag). Rime now builds that kernel from source itself (`Containerfile.kernel`), and the release still reads `…-cachyos1.rime1.fc43…`: nobody has changed the kernel tier's release naming. **Mitigated**: it is hidden from the boot menu (BLS `title` never contains the kernel version), from the VT login banner (surface 11) and from fastfetch (surface 12). It remains visible to anyone who runs `uname -r`. |
| The ESP directory `EFI/fedora/` | `\EFI\fedora\shimx64.efi`, `/boot/efi/EFI/fedora/` | The path is **hardcoded inside Fedora's signed `grubx64.efi`** (its build-time prefix) and in `shim`'s fallback CSV. Renaming the directory breaks the Secure Boot chain: grub would not find its config and the machine would not boot. Only the **NVRAM label** can change, and it does (surface 2). Also note `installer/rime-install` deliberately keeps its "a Fedora-family install already uses `\EFI\fedora`" warning: that text is about a genuine neighbouring Fedora install and a genuine path, and making it say Rime would be a lie. |
| `os-release` `ID` / `VERSION_ID` | `fedora` / `43` | See "Why `ID=fedora` stays" above: verified build breakage. |
| `/etc/yum.repos.d/fedora*.repo`, `rpmfusion-*.repo`, `_copr:…kernel-cachyos*.repo` (left behind, `enabled=0`, by `dnf copr disable`), `rpm -E %fedora`, `%dist_vendor` | `Fedora`, `cachyos` | Package-manager plumbing pointing at real Fedora / COPR repositories. Renaming would break package resolution, and none of it is user-visible. |
| Engineering comments in `Containerfile.*`, `installer/build-live-iso.sh`, `rime-greet/README.md`, `mpv.conf` | `Fedora`, `CachyOS` | Load-bearing rationale ("Fedora ships crippled ffmpeg", "Fedora's signed grub has prefix /EFI/fedora"). These are developer-facing and accurate; scrubbing them would destroy the reasoning. |

## Fixing an already-installed system

Do the boot-menu change and the NVRAM change as **separate steps**, in this
order. The BLS rewrite has been tested against a copy of the real entries; the
NVRAM write has not (there is no way to dry-run firmware). With the two apart,
if the firmware misbehaves on the relabel, you are already booted through a
verified-good boot menu instead of debugging two changes at once.

```sh
# 1. See what would change — writes nothing.
sudo /usr/libexec/rime-debrand-runtime            # or ./files/scripts/rime-debrand-runtime.sh

# 2. Boot menu titles + the /etc/os-release override. Reboot and confirm the
#    GRUB menu now reads "Rime OS (ostree:N)".
sudo /usr/libexec/rime-debrand-runtime --apply --skip-efi

# 3. A second install on another partition has its own /boot but SHARES the ESP
#    (and therefore the single firmware boot entry) — do its BLS titles too.
sudo /usr/libexec/rime-debrand-runtime --apply --boot-dir /mnt/other/boot

# 4. Only now, the firmware boot entry label ("Fedora" -> "Rime OS"). Run this
#    ONCE for the whole disk, not once per install.
sudo /usr/libexec/rime-debrand-runtime --apply --skip-bls --skip-os-release

# 5. AFTER the first `bootc upgrade` onto a branded image, run it once more.
#    /usr/lib/os-release is branded by then, so the script takes the other
#    branch: it deletes the transitional /etc/os-release override and restores
#    the image symlink. THIS STEP IS NOT OPTIONAL — skipping it leaves the
#    override merging forward forever, pinning VERSION_ID=43 across a future
#    rebase to an F44 base.
sudo /usr/libexec/rime-debrand-runtime --apply --skip-bls --skip-efi
```

Safety properties (all exercised against a copy of the real
`/boot/loader/entries`; see the script header):

- dry-run by default; `--apply` is required to write anything;
- the script validates an entry before touching it: exactly one `title` line,
  a `linux` line, and the referenced vmlinuz/initramfs must exist. It skips a
  broken entry loudly, leaves it untouched, and exits non-zero;
- **only the `title` line is rewritten**; the script rejects the rewrite unless
  every other line is byte-for-byte identical, and never reformats `options`,
  `linux`, `initrd` or `version`;
- it writes with `cat >` (not `mv`), which preserves inode, mode, owner and
  SELinux label;
- backups go to `/var/lib/rime-debrand/<timestamp>/`, on purpose **not** next to
  the entries: grub's `blscfg` globs `*.conf`, and an in-place backup would show
  up as a phantom boot menu entry;
- it remounts `/boot` rw only if it is mounted ro, and restores the mount on
  exit, failure included;
- it checks `grubenv` for a `saved_entry` that names a title (none on the
  current installs; the script prints the exact `grub2-editenv` fix if one
  appears);
- the EFI step creates and **verifies** the new entry (same ESP PARTUUID, same
  loader) *before* deleting the old one, and restores `BootOrder` with the new
  entry in the old entry's position. If any step fails, the original entry is
  left alone and the machine still boots;
- idempotent: a second run reports "already branded" and writes nothing.
