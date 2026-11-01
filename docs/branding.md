# Rime OS Branding

## The mark: the layered ridge

Rime's mark is the **layered ridge**: four flat planes stacked into a rising ridge, with the third plane in the
accent colour. The wordmark is a lowercase `rime`; the product lockup sets `RIME OS` in wide capitals. This is the
identity of the NixOS-based Rime (`feat/nixos`). The **spark** mark and its chartreuse/gold colourways are
**legacy**: they belong to the bootc image and are described at the end of this section ("Legacy: the spark").

**Reconstructed, not drawn from source files.** The brand pack in `docs/brand-pack/page1..page5.png` is five
raster mockups; no vector source exists. Every vector here was rebuilt from those pages: the symbol traced plane
by plane and refitted as straight-edged polygons on a 0.5-unit grid, the colours sampled, and the wordmarks matched
to the closest open-licence typeface and outlined. Where the pages disagree with each other (they do, by a few
per cent of plane thickness), page 1's primary symbol wins.

### The symbol

`files/branding/ridge/src/rime-ridge-symbol.svg`, viewBox `0 0 240 175`, exactly four `<polygon>`s, drawn
bottom to top:

| Plane | Points | Colour |
|---|---|---|
| `plane-1` (base) | `11,175 162.5,140 220,159 235.5,175` | ink (`{{fg}}`) |
| `plane-2` | `0,175 153,108 212,91.5 194.5,139 160.5,128` | ink (`{{fg}}`) |
| `plane-3` | `47.5,136 149,75 231,45 217.5,76` | **accent** (`{{accent}}`) |
| `plane-4` (peak) | `91.5,100 129.5,58 240,0 222,36 142.5,67` | ink (`{{fg}}`) |

The left tips of planes 2, 3 and 4 lie on one line (within 0.07 unit), as on the page.

**Small-size master** (`rime-ridge-symbol-small.svg`): the same planes, each 20 % thinner (plane 1 about the
baseline, plane 4 about the peak, planes 2 and 3 about their centres), so the three gaps stay open. Every export
at **32 px or smaller** uses it; above 32 px the regular master.

**Fit to the pages** (IoU per plane 1/2/3/4, the vector against the page's own render, aligned by bounding box):

| Page render | IoU | Best two renders of the same mark agree to |
|---|---|---|
| page 1 primary symbol | 0.950 / 0.907 / 0.917 / 0.970 | page 1 symbol vs page 1 lockup: 0.968 / 0.925 / 0.883 / 0.888 |
| page 1 lockup symbol | 0.955 / 0.899 / 0.871 / 0.896 | page 1 symbol vs page 4 hero: 0.894 / 0.814 / 0.930 / 0.869 |
| page 4 hero | 0.907 / 0.790 / 0.890 / 0.870 | page 1 lockup vs page 4 hero: 0.887 / 0.823 / 0.877 / 0.784 |
| page 5 symbol | 0.889 / 0.793 / 0.821 / 0.828 | |
| page 5 lockup symbol | 0.895 / 0.807 / 0.898 / 0.838 | |

The right-hand column is the ceiling: the pages' own renders of one mark disagree about as much as the vector
disagrees with any of them, so a closer fit to one page would be a worse fit to another. Plane 2 is the thin plane
where antialiasing moves the most pixels. The whole horizontal lockup against page 1's
lockup: ink IoU 0.815 (most of the miss is the wordmark's antialiased edge). The overlays are in the review
sheet `/var/lab/rime-nix/artifacts/brand/review/assets-sheet.png` on the build host (not in the repo).

### Colours

`files/branding/ridge/src/palette.json` holds these; the generator reads nothing else. Samples are medians over
flat interiors (no antialiased edges) of the page pixels.

| Name | Hex | Where it was sampled (n = pixels) | Spread across the pages |
|---|---|---|---|
| **Charcoal** (ink on light) | `#212224` | page 1 lockup symbol, n = 4309 | page 1 symbol-only `#202123`, page 1 wordmark `#181A1C`, page 4 hero `#151516`, page 5 `#1F2328`/`#17181A`; one ink is used for symbol and wordmark |
| **Rime Blue** (plane 3) | `#91C2DC` | page 1 lockup | page 1 symbol `#8FC2DB`, swatch `#92C3DC`; page 4 hero `#83AECB`, page 5 `#6DAACD`/`#6EAACC`, page 4 small accent `#77A4C2` (later pages are shaded renders; page 1 is canonical) |
| **Off-white** (ink on dark) | `#FAFAFA` | page 1 mark on the dark tile | page backgrounds `#F9F8F8` to `#FDFDFE` |
| **Dark tile** | `#1F2327` | page 2 app icon | avatar `#1F2328`, page 1 dark tile `#1D2022` |
| **Light tile** | `#FAFAFB`, border `#E2E4E6` | page 2 app icon | |
| **Mono black / mono white** | `#000000` / `#FFFFFF` | page 1 mono marks | the page's mono-black sample was `#090909` with plane 3 in grey `#54585C`; mono is one ink here, so it survives a one-colour print or a stencil |

**Named accents** (page 4's accent row; small slivers, so the most saturated half of each was taken; antialiasing
pulls these slightly dark): Teal `#0A7661`, Emerald `#38A12B`, Amber `#D69E1A`, Violet `#A664D9`, Rose `#E4708B`,
plus Rime Blue `#91C2DC`.

**Per-owner slots**: 24 accents, slot *n* = hue *n* x 15 deg at Rime Blue's HLS lightness and saturation
(`00` `#DC9191` ... `13` `#91C9DC` ... `23` `#DC91A4`). Rime Blue's hue is 200.8 deg, nearest slot `13`. The table
is `accents/accents.json` (`base`, `base_slot`, `named`, `slots`). They play the part of the spark's 24
Plymouth accent themes, but those are chartreuse rotated by *n* x 15 deg, so the same index is not the same hue:
map an owner's stored accent by hue, not by index.

### The adaptive rule

**Plane 3 is the only part that ever takes an accent. Every other plane, and the wordmark, is the foreground ink.**
`templates/*.svg.in` carry exactly two tokens: `{{accent}}` on plane 3 and `{{fg}}` everywhere else; no literal
colour. A consumer substitutes the owner's accent (or Rime Blue) and the ink for its background (Charcoal on light,
Off-white on dark), e.g. `sed -e 's/{{accent}}/#91C2DC/' -e 's/{{fg}}/#FAFAFA/' rime-symbol.svg.in`. Mono variants
put the ink on plane 3 too. `checks.x86_64-linux.branding` fails if a template or an accent export breaks this.

Variant names describe the background they are for: `light` = charcoal ink + Rime Blue for light backgrounds,
`dark` = off-white ink + Rime Blue for dark ones, `mono-black`, `mono-white`.

### Wordmarks and lockups

Both wordmarks are outlined paths: no font is needed at runtime or to regenerate anything.

- `rime`: **Golos Text**, wght 600, tracking -0.025 em (IoU with page 1's wordmark 0.894; Onest 650 scored 0.893,
  Hanken Grotesk 700 0.910 but has a square dot on the i).
- `RIME OS`: **Archivo**, wdth 125 (expanded), wght 700, tracking +0.045 em, word space -0.16 em (per-glyph IoU
  0.866 mean, glyph widths within 1-7 % of the page).

Both are SIL OFL 1.1 (nixpkgs `google-fonts`); `src/FONTS.md` has the source and the licence texts sit beside the
outlines. Proportions measured on the pages and built into the generator:

| Lockup | Rule |
|---|---|
| horizontal | wordmark x-height = 0.46 x symbol height; gap = 0.22 x-height; wordmark centred on the symbol |
| stacked | symbol width = 0.87 x wordmark width, centred; gap from symbol to the dot of the i = 0.20 x-height |
| RIME OS | cap height = 0.46 x symbol height; gap = 0.45 cap height; caps centred 0.08 symbol height below the symbol's centre |
| wordmark only | at the horizontal lockup's scale |

**Clear space** = the wordmark's x-height on every side (cap height for the RIME OS lockup; for the bare symbol,
0.46 x its height). The `clearspace/` exports carry it as transparent padding.

**Minimum sizes** (chosen, by eye on the review sheet): lockups 24 px high, the symbol 16 px. The symbol switches
to the small-size master at 32 px and below. Don't recolour planes 1, 2 or 4, outline, rotate, add effects or set the
wordmark live in another font.

### Asset inventory: `files/branding/ridge/`

| Path | What |
|---|---|
| `src/rime-ridge-symbol.svg`, `src/rime-ridge-symbol-small.svg` | the two symbol masters (four polygons each) |
| `src/rime-wordmark.svg`, `src/rime-os-wordmark.svg` | outlined wordmarks; `src/outline-wordmarks.py` made them |
| `src/palette.json`, `src/FONTS.md`, `src/OFL-*.txt` | colours, font sources, licences |
| `svg/rime-{symbol,symbol-small,wordmark,lockup-horizontal,lockup-stacked,lockup-os}-<variant>.svg` | tight vector exports, 4 variants each |
| `svg/clearspace/*-clearspace.svg` | the same with clear space |
| `svg/square/rime-symbol-square[-small]-<variant>.svg` | the symbol centred on a square canvas (icon use) |
| `svg/rime-app-icon-{dark,light}[-small].svg` | app icon tiles: dark tile with off-white ink, light tile with charcoal ink and a hairline border; radius 0.17 x side, mark 0.74 x side, centred at 0.46 height |
| `templates/rime-*.svg.in` | adaptive templates (`{{fg}}`, `{{accent}}`) for symbol, small symbol, square symbol, wordmark and the three lockups |
| `png/symbol/rime-symbol-<variant>-{16..1024}.png` | square, transparent; 16/24/32 from the small master |
| `png/lockups[/clearspace]/rime-<lockup>-<variant>@{1,2}x.png` | 1x heights: horizontal 96, stacked 192, wordmark 96, RIME OS 64 px; transparent |
| `icons/app/rime-app-icon-{dark,light}-{16..1024}.png` | opaque tiles, transparent rounded corners |
| `favicon/favicon-{16,32,48}.png`, `favicon.ico`, `favicon.svg` | the .ico holds PNG entries 16/32/48; the SVG switches ink with `prefers-color-scheme` |
| `accents/named/`, `accents/slots/`, `accents/accents.json` | the symbol per named accent and per slot: SVG light/dark + 256 and 512 px PNG |

Sizes: 16, 24, 32, 48, 64, 128, 256, 512, 1024. Accents get only SVG + 256/512 PNG to keep the repo small
(the whole tree is about 2.8 MB); render any other size from the SVG.

**Package**: `nix/pkgs/rime-branding.nix`, flake attr `packages.x86_64-linux.rime-branding` (`pkgs.rime.rime-branding`
in the overlay) installs the tree to `$out/share/rime/branding/` and the dark app tile as the hicolor icon
`rime-os` (16..512 PNG, `scalable` SVG, `symbolic/apps/rime-os-symbolic.svg` in one ink), with
`distributor-logo`, `start-here` and `rime-os-logo` as aliases at every size, so `LOGO=rime-os-logo` in
os-release and desktop "start here" icons resolve to the ridge. No NixOS module uses it yet.

### Regenerating

Change a master (`src/*.svg`, `src/palette.json`) or a constant in `make-ridge-assets.py`, then from
`files/branding/ridge/`:

```sh
nix shell github:NixOS/nixpkgs/<rev in flake.lock>#resvg github:NixOS/nixpkgs/<rev>#python3 \
  --command ./make-ridge-assets.sh
```

and commit every changed export. The generator deletes and rewrites `svg/ templates/ png/ icons/ favicon/
accents/`, needs only resvg and the Python standard library, and is deterministic. `checks.x86_64-linux.branding`
(`nix/tests/branding.nix`) regenerates into a scratch tree and fails on any export that differs from the committed
one (PNGs by pixels), on a master that is not four polygons, on a template or accent that colours anything but
plane 3, on wrong sizes, opaque backgrounds, an @2x that is not twice the @1x, or a favicon.ico without 16/32/48;
it also shellchecks the wrapper and checks the package's hicolor names. To change a wordmark's typeface or
tracking, run `src/outline-wordmarks.py` first (`src/FONTS.md`).

## Terminal greeting

Every new interactive terminal opens with fastfetch and the ridge symbol (`files/desktop/shell/rime-greeting.sh`,
installed as `share/rime/shell/greeting.sh`; NixOS module `nix/modules/greeting.nix`, option `rime.greeting.enable`,
on with the desktop). zsh reaches it from the seeded `~/.zshrc`, bash from `/etc/bashrc`. Opt out with
`RIME_NO_GREETING=1`.

**The logo is the real symbol or a label, never ASCII art.** `libexec/rime-term-graphics` (C, so it costs nothing
before the first prompt) decides how to draw it:

| Terminal | Answer |
|---|---|
| local Ghostty or kitty | `kitty-direct` (no query; the image is read from the file) |
| local WezTerm | `iterm` (WezTerm's kitty graphics are off by default) |
| Alacritty, the Linux console, kmscon, `TERM` dumb/unset, no `/dev/tty` | `none` |
| GNU screen; tmux without `allow-passthrough`, or not inside Ghostty/kitty | `none` |
| anything else (SSH, foot, xterm, unknown) | ONE query: kitty graphics + DA1 (+ `CSI 14 t` when the pty has no pixel size), at most 35 ms (`RIME_TERM_GRAPHICS_TIMEOUT_MS`) |

kitty or sixel need the cell size in pixels (from the pty or `CSI 14 t`); without it fastfetch would fall back to
its built-in ASCII logo, so the answer is `none`. The terminal's modes are restored on every path, Ctrl-C
included, and a reply that arrives after the deadline is flushed. The answer is exported as
`RIME_TERM_GRAPHICS=<proto>@<tty,TERM,SSH_TTY>`, so a nested shell in the same terminal does not ask again;
`RIME_TERM_GRAPHICS_FORCE=<proto>` overrides it. The logo is 8 rows tall, its width in cells is derived from
the cell size and the PNG's aspect; a window too narrow for logo and text gets the label.

With `none`, fastfetch prints a bold **RIME OS** label in the accent (Rime Blue `#91C2DC` before any
wallpaper), from `/etc/fastfetch/config.jsonc`.

**Recoloured from the wallpaper.** Rime Shell's matugen config (`rime-shell src/config/matugen.toml.in`,
`[templates.rime_logo]`) writes `~/.cache/rime/logo/colors` on every wallpaper change: `primary` and
`on_surface` for the dark and the light scheme. Its post_hook runs `libexec/rime-logo-render`, which fills the
adaptive template (`templates/rime-symbol.svg.in`: plane 3 = accent, the other planes = foreground) and renders
`rime-symbol-{dark,light}.png` (512 px, resvg) next to it, each written under a temporary name and renamed into
place. The greeting shows the light or dark one by the desktop's colour scheme (dark over SSH), and the shipped
`png/symbol/rime-symbol-{dark,light}-512.png` until the Shell has rendered one. An existing user gets the logo at
the next Shell start: the Shell re-runs matugen when any of its outputs is missing.

Checks: `checks.x86_64-linux.greeting` (`nix/tests/greeting.nix`: the helper on a fake terminal, the greeting's
choices in bash/zsh/dash, the built Shell's matugen config on two wallpapers, three mutants) and
`checks.x86_64-linux.greeting-vm` (`nix/tests/greeting-vm.nix`: Ghostty draws the recoloured symbol, Alacritty
the label, a new wallpaper recolours the next greeting).

## Legacy: the spark (bootc image)

Everything below in this section describes the bootc-based Rime OS image, which still ships the spark. Keep it
working while that image is the product; do not use the spark, chartreuse or gold for new NixOS-side artwork.

### The mark: "spark"

Rime OS uses a single logomark, the **spark**, drawn in several colorways. The
wordmark is the letterspaced "Rime OS".

### Color semantics

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

### Asset inventory

All assets live under `files/branding/`.

#### Logos: `files/branding/logos/<colorway>/`

Colorways: `gold/`, `chartreuse/`, `mono-black/`, `mono-white/`.

Each colorway provides:

- `rime-spark-<colorway>.svg`: vector source (512×512 viewBox)
- `rime-spark-<colorway>.png`: 512×512 base raster
- `rime-spark-<colorway>-{16,32,64,128,256,512,1024}.png`: icon sizes

(Mono files use the suffix `black` / `white`, e.g. `rime-spark-black-256.png`.)

#### Plymouth boot themes: `files/branding/plymouth/`

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

#### Previews

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

#### Wallpaper: `files/branding/wallpapers/`

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
