# The NixOS closure: size, cuts, A4 and A11 (P1)

Status: **measured** on 2026-11-01 at nixpkgs `1c89083e451e`, Nix 2.35.4, in the `rime-nix` container on katana.
Every number below comes from a command in this file; the commands are committed (`nix/tests/size.nix`,
`nix/tests/size-measure.sh`). GB is 10^9 bytes (bootc's published figures are decimal); GiB is 2^30 where it appears.

This is the P1 exit item "the real closure is measured (download, unpacked, under `compress-force=zstd:3`) and the P5
fresh-install figure and the disk floor (A11) are set from it", plus decision A4, which PLAN.md and STATE.md left to
this measurement.

## 1. Results in one place

| | value | against bootc |
|---|---|---|
| rime-vm closure before the cuts | 1995 paths, 11,039,742,336 B unpacked (11.04 GB) | |
| rime-vm closure after the cuts | 1957 paths, 9,530,593,800 B unpacked (9.53 GB) | |
| saved by the cuts (unpacked) | **1,509,148,536 B (1.51 GB)**, 38 paths | |
| **P5 fresh-install download** (`plain`, zstd -19) | **2.51 GB** (xz: 2.31 GB) | bootc 4.20 GB |
| on disk after install (`plain`, btrfs zstd:3, optimised) | 4.30 GB | bootc 5.4 GB (2026.10.04.2, 12 GiB VM) |
| `rime install steam` on top of `plain` | **+0.53 GB** download, +2.46 GB unpacked, +0.99 GB on disk | |
| **A4** | **gaming is opt-in** (`rime.gaming.enable` stays `false`; installs ship `plain`) | 10-04 decision kept |
| **A11 disk floor** (install + one mass rebuild + boot) | **16 GB** as `rime update` is designed (§11.5 gate counts unpacked bytes); **12 GB** physically, and once the gate counts on-disk bytes | bootc 12 GB, which holds its 5.4 GB install plus one core update, barely (§6) |
| A11 install-only minimum (no update headroom) | 6 GB | no bootc counterpart published |

`plain` is the shape a fresh install ships (D9's generic rime-plain toplevel): every Rime area on, Rime's kernel with
NVIDIA 615, no gaming, no test account. `rime-vm` is `nixosConfigurations.rime-vm` (gaming on, nixpkgs' kernel, no
NVIDIA userspace, because `qemu-vm.nix` forces `videoDrivers = [ "modesetting" ]` with `mkVMOverride`). Both are
defined in `nix/tests/size.nix` (`measure.*`).

## 2. How each number is measured

```
# the closure, path by path (unpacked = NAR size)
nix path-info -rsS path:.#nixosConfigurations.rime-vm.config.system.build.toplevel
# who pulls a path in
nix why-depends --all <toplevel> /nix/store/<hash>-<name>
# all six variants: download (zstd -19 and xz), unpacked, on disk (btrfs compress-force=zstd:3), kernel+initrd
R=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
sudo -n podman exec -w /work/wt-size rime-nix nix shell $R#bash $R#coreutils $R#findutils $R#gawk $R#gnugrep \
  $R#util-linux $R#btrfs-progs $R#compsize -c bash nix/tests/size-measure.sh /work/scratch/size/run
# the guard that keeps the cuts (a flake check) and its mutants (each must fail)
nix build path:.#checks.x86_64-linux.size
nix build path:.#checks.x86_64-linux.size.mutants.second-nerd-font
nix build path:.#checks.x86_64-linux.size.mutants.registry-store-path
nix build path:.#checks.x86_64-linux.size.mutants.registry-unlocked
```

- **Download** is what a machine fetches from cache.rimeos.com (D6: machines fetch only from Rime's cache, zstd). All
  six toplevels are copied with `nix copy --to 'file://…/cache-zstd19?compression=zstd&compression-level=19'` and,
  separately, `…/cache-xz?compression=xz` (Nix's default). A variant's download is the sum of `FileSize` in the
  narinfos of its closure. Paths shared between variants are compressed once, so the sums are per closure, not per
  cache. Method check, printed by the script: over every distinct NAR file in a cache (two store paths with
  identical contents share one), the `FileSize` sum equals `du -sb nar/` (zstd: 2,146 NARs, FileSize sum 3,230,563,556 B, `du -sb nar/` 3,230,563,556 B; xz: 2,146 NARs, FileSize sum 2,974,045,376 B, `du -sb nar/` 2,974,045,376 B).
- **Unpacked** is `closureSize` from `nix path-info -S --json` (the NAR bytes; what `nix-store --verify` checks).
- **On disk** is the closure copied with `nix copy --to 'local?root=<mnt>'` into a fresh btrfs (`mkfs.btrfs`, 40 GB
  sparse loop file) mounted `compress-force=zstd:3`, as D2/D9 mount `@nix`. Measured with `df -B1` (used, includes
  metadata) and `compsize -b <mnt>/nix/store` (data extents), before and after `nix store optimise` (base.nix sets
  `auto-optimise-store`, so a running machine is the optimised figure). The download/disk join checks that both were
  measured on the same toplevel store path.
- **Kernel + initrd** is `stat -L` of `<toplevel>/kernel` and `<toplevel>/initrd`, the bytes the boot hook copies to
  the ESP per generation.

The measurement ran while katana carried other agents' builds (load average 110-130), which changes only the time
it took. Compression is deterministic for a given level and input.

## 3. The cuts

Each cut keeps parity with bootc (it removes nothing a bootc user has) and is held by `checks.size` (§6).
Bytes are unpacked NAR bytes of the paths that left the rime-vm closure, from `nix path-info -rsS` before and after.

| cut | what it was | who pulled it | saved | bootc parity | change |
|---|---|---|---|---|---|
| MBROLA voices | `mbrola-voices` (675,969,496 B) + `mbrola-bin` + `mbrola` | `services.speechd` → `speech-dispatcher` → `espeak-ng` (built `mbrolaSupport = true` by default) → `mbrola` → voices | **676,036,280 B** | Fedora's espeak-ng 1.52 is built with MBROLA code but `/usr/share/mbrola` is absent on bootc: no voices. Orca speaks with espeak-ng's own voices, as on bootc | `desktop.nix`: `services.speechd.package = pkgs.speechd.override { espeak = pkgs.espeak-ng.override { mbrolaSupport = false; }; }`. Orca's own `speechd-minimal` already had none |
| Second Nerd Font | `nerd-fonts-jetbrains-mono-3.5.0+2.304` (nixpkgs) | `fonts.packages` in `desktop.nix` listed `pkgs.nerd-fonts.jetbrains-mono` beside Rime's 3.5.1 pin `nerdFontJetBrainsMono` (X11-fonts, fontconfig) | **239,488,096 B** | bootc ships one JetBrainsMono Nerd Font (v3.5.1) | `desktop.nix`: the list names the 3.5.1 pin |
| nixpkgs source | `<hash>-source` (the nixpkgs tree) | `etc-nix-registry.json`: `nixpkgs.flake.setFlakeRegistry` (default on for flake systems) writes `registry.nixpkgs.to = { type = "path"; path = <store path>; }`, which makes the whole source a runtime dependency | **217,222,584 B** | bootc has no Nix at all; there is no evaluation on the device (D5) | `base.nix`: `setFlakeRegistry = false`, `setNixPath = false`, and the registry names the same nixpkgs as `github:NixOS/nixpkgs/<config.system.nixos.revision>` (locked rev). `nixPath` is unchanged (`nixpkgs=flake:nixpkgs`). `nix run nixpkgs#x` still resolves to the system's nixpkgs, fetched from GitHub on first use |
| webkitgtk 4.1 | `webkitgtk-2.54.1+abi=4.1` | `networking.networkmanager.plugins` → `networkmanager-openconnect` (built `withGnome = true`) → its GNOME auth dialog, the only 4.1 user | **184,121,472 B** | Fedora's `NetworkManager-openconnect` on bootc is installed without `-gnome` (no webkit2gtk4.1). webkitgtk 6.0 stays: Bazaar uses it and bootc ships it | `desktop.nix`: `networkmanager-openconnect.override { withGnome = false; }` (its configure uses webkit only for `--with-gnome`) |
| Documentation | 32 `-doc` outputs and the NixOS manual: python3 doc 59.9 MB, pipewire-doc 34.7 MB, nixos-manual-html 30.8 MB, nix-manual 24.4 MB, wireplumber-doc 15.0 MB, imagemagick-doc 9.9 MB, sane-doc 5.0 MB, nixos-help's w3m/gpm/boehm-gc, the configuration-reference man page, and 20 small ones | `documentation.doc.enable` (default on) adds `doc` to `extraOutputsToInstall` and `/share/doc` to `pathsToLink`; `documentation.nixos.enable` adds the manual and `nixos-help` | **191,641,672 B** | bootc installs no `-doc` subpackages (no python3-docs, no pipewire-doc). Man pages stay on both | `base.nix`: `documentation.doc.enable = false; documentation.nixos.enable = false;` |
| 32-bit ALSA | `pkgsi686Linux.pipewire` ALSA plugin, moved, not removed | `services.pipewire.alsa.support32Bit` in `desktop.nix` | 0 on rime-vm (gaming on); part of the gaming delta on `plain` | bootc's image has no i686 audio; its Steam set (`rime install`) brings i686 `alsa-lib` but not the PipeWire ALSA plugin (katana, 2026-11-01: `/usr/lib/libasound.so.2` present, `/usr/lib/alsa-lib/` absent), so the gaming toplevel keeps more than bootc here | moved to `gaming.nix`, next to `enable32Bit` |

Rebuilt paths: 40 store paths changed hash because their inputs changed (speech-dispatcher, espeak-ng,
NetworkManager-openconnect, X11-fonts, fc-cache, hwdb.bin, both Steam FHS rootfs, system-path, etc and the units).
They left 128,903,896 B and came back as 128,265,464 B. speech-dispatcher, espeak-ng and NetworkManager-openconnect are
now built by Rime (not on cache.nixos.org with these overrides); the rest are NixOS-generated and are built anyway.

Total: 11,039,742,336 − 1,637,414,000 removed + 128,265,464 added = **9,530,593,800 B** (1957 paths).

**The documentation cut needed two files kept.** With `documentation.doc` off, `/share/doc` is no longer linked into
the system profile, and `checks.vm-boot` failed: `rime-shell-firstrun` copies niri's `default-config.kdl` from
`/usr/share/doc/niri/` (rewritten to `/run/current-system/sw/share/doc/niri/`), got nothing, and later failed niri's
config validation; `rime-input-apply` reads labwc's `rc.xml.all` from `/usr/share/doc/labwc/` the same way.
`desktop.nix` now installs `niri.doc` and `labwc.doc` and links only `/share/doc/niri` and `/share/doc/labwc`
(563,064 B). These are the only two `share/doc` reads in `files/` (the other hits are `Documentation=` links in bootc
units). With them, `checks.vm-boot` passes again.

### Not cut, and why

| path | size | why it stays |
|---|---|---|
| two `systemd-262` | 51,277,336 B each | NixOS builds `systemd.package` with its own search-path patch; qtbase, bolt, sudo, rimed and others reference `pkgs.systemd`. Unavoidable without rebuilding them |
| two `llvm-21.1.8-lib`, two `mesa-26.2.6` | 577 + 564 MB, 295 + 288 MB | the second of each is i686, in Steam's FHS rootfs. Gaming only |
| `glibc-locales-2.44-57` | 233,362,928 B | only through the `steam` and `steam-run` FHS envs. Gaming only |
| `webkitgtk-2.54.1+abi=6.0` | 182,653,528 B | Bazaar (bootc ships webkitgtk6.0 for it) |
| `qtwebengine-6.140.0` | 438,819,576 B | fcitx5-chinese-addons (bootc ships qt6-qtwebengine and fcitx5-chinese-addons) |
| `openvino`, `onnxruntime` | 172,770,944 + 58,993,024 B | Firefox's local ML features. Rebuilding Firefox without them puts Firefox on Rime's builders for every update |
| second `python3-3.14.8` | 84,469,712 B | gstreamer's own reference (`pipewire` → `gstreamer` → python3). Removing it means overriding gstreamer and rebuilding everything above it |
| `deno` | 145,276,688 B | yt-dlp's JavaScript runtime (mpv → yt-dlp); bootc ships nodejs for the same yt-dlp need |
| `nodejs-slim` | 79,512,944 B | `rime-libexec`'s node scripts; bootc ships nodejs |
| `sing-box` | 112,281,384 B | Rime's proxy service (bootc ships it) |
| `flite` | 63,390,232 B | speech-dispatcher's flite module; bootc ships flite |
| `git-doc`, `mpv-doc` | small | referenced directly by their packages, not through `documentation.doc` |

## 4. The six variants

| variant | paths | unpacked | download zstd -19 | download xz | on disk (df) | on disk optimised (df) | compsize data, optimised | kernel + initrd |
|---|---|---|---|---|---|---|---|---|
| `rime-vm` | 1957 | 9.53 GB | 2.44 GB | 2.23 GB | 4.51 GB | 4.47 GB | 3.83 GB | 49.9 MB |
| `rime-vm-no-gaming` | 1653 | 7.70 GB | 2.06 GB | 1.89 GB | 3.67 GB | 3.69 GB | 3.24 GB | 49.9 MB |
| `rime-vm-gaming-no32` | 1956 | 9.53 GB | 2.44 GB | 2.23 GB | 4.52 GB | 4.48 GB | 3.83 GB | 49.9 MB |
| `plain` | 1674 | 9.10 GB | 2.51 GB | 2.31 GB | 4.28 GB | 4.30 GB | 3.83 GB | 52.4 MB |
| `plain-gaming` | 1985 | 11.55 GB | 3.04 GB | 2.78 GB | 5.33 GB | 5.28 GB | 4.62 GB | 52.4 MB |
| `plain-gaming-no32` | 1984 | 11.55 GB | 3.04 GB | 2.78 GB | 5.33 GB | 5.28 GB | 4.62 GB | 52.4 MB |

`rime-vm-gaming-no32` and `plain-gaming-no32` are not configurations Rime ships (`programs.steam` turns
`enable32Bit` on; these force it off). They show what `hardware.graphics.enable32Bit` costs on its own when Steam
stays: **39,648 B unpacked on `plain`** (the `graphics-drivers-32bit` link farm behind `/run/opengl-driver-32`, 39,584 B, and the config naming it). Steam's FHS rootfs carries
its own i686 mesa, llvm and `nvidia-x11-615.78.08-lib32` whatever `enable32Bit` says (`nix why-depends <plain-gaming-no32>
<i686 mesa>`: system-path → steam → steam-bwrap → steam-fhsenv-rootfs → mesa), so the 32-bit graphics cost cannot be
split from Steam: it is part of the gaming delta in §5 either way.

Exact bytes (`run/summary.tsv`, written by `size-measure.sh`):

```
variant	paths	nar_bytes	zstd19_bytes	xz_bytes	kernel_initrd_bytes	df_used	compsize_disk	compsize_uncompressed	df_used_optimised	compsize_disk_optimised
rime-vm	1957	9530593800	2439916861	2232978552	49919566	4513853440	3895513835	9582684452	4472229888	3825992084
rime-vm-no-gaming	1653	7703861632	2055709939	1891218336	49919566	3671695360	3249006486	7774581612	3693948928	3238376847
rime-vm-gaming-no32	1956	9530592896	2439916633	2232978296	49919566	4520726528	3896855194	9582684385	4483784704	3827291595
plain	1674	9098365168	2513265034	2314174960	52439626	4277587968	3848573912	9167053027	4296024064	3834339989
plain-gaming	1985	11553820984	3036423110	2780117640	52439626	5330403328	4702665198	11603744441	5281521664	4623182428
plain-gaming-no32	1984	11553781336	3036420987	2780115444	52439626	5331300352	4701503740	11603744374	5278437376	4622017917
```

## 5. A4: the gaming stack is opt-in

**Decision: opt-in.** `rime.gaming.enable` stays `false` (its default in `gaming.nix`), fresh installs ship `plain`, and
`rime install steam` moves the machine to the gaming toplevel. This keeps Andre's 2026-10-04 decision ("install as
small as possible").

The delta is not trivial:

| | `plain` | `plain-gaming` | delta |
|---|---|---|---|
| download (zstd -19) | 2.51 GB | 3.04 GB | **+0.53 GB** (paths in `plain-gaming` and not in `plain`) |
| unpacked | 9.10 GB | 11.55 GB | +2.46 GB |
| on disk, optimised | 4.30 GB | 5.28 GB | +0.99 GB |

That is 21 % more download for every install, against bootc's 4.20 GB. PLAN's lower bound (0.12 GB, from
the probe) left 32-bit graphics on in both closures; with them following gaming, the real cost is the figure above.
Turning 32-bit graphics off does not shrink gaming (§4), so there is no cheaper "gaming in every install".

What opt-in costs, given D5 (no evaluation on the device) and D9 (prebuilt generic toplevels): CI builds and the cache
serves a second toplevel per release, `plain-gaming`. The two share every path except the delta, so the extra build
and storage is the delta, not a second system. `rime install steam` is then a `rime update` to the gaming toplevel:
the same download path, signature and gates. A machine that turns gaming off again goes back to `plain` and GC frees
the delta. `rime.gaming.enable`'s option text in `gaming.nix` quotes these numbers.

## 6. A11: the disk floor and the P5 fresh-install figure

### The P5 fresh-install figure

**2,513,265,034 B (2.51 GB)**, the zstd -19 download of `plain`, set here as the P5 gate. That is
1.69 GB less than bootc's 4.20 GB fresh install. With xz (Nix's default compression) it would be 2.31 GB. The P2 ports
that are still landing add to it; under PLAN §6, a P5 figure above this one goes to Andre.

### The disk floor

A NixOS machine holds whole closures. A mass rebuild (a stdenv or glibc change, every 2-3 weeks, PLAN §3.3)
replaces about 99 % of the paths, so for a while the store holds two systems. GC (D6 §11.3) keeps the current, booted
and previous generations, and deletes the previous one before an update that would not fit, once the machine has
booted into the current one. So the floor is **one system installed, plus room for one more, plus boot**:

| part | bytes | source |
|---|---|---|
| BIOS-boot + ESP partitions | 537,919,488 B (1 MiB + 512 MiB) | the installer's layout (`installer/rime-install`, `sgdisk -n1:0:+1M -n2:0:+512M`) |
| the installed system (`plain`, optimised, `df` used) | 4,296,024,064 B | §4 |
| room for one mass rebuild, **as designed**: `rime update` refuses unless free space ≥ the missing paths' unpacked size + 1 GB (D6 §11.5) | 10,098,365,168 B (9.10 GB + 1 GB) | §4 unpacked |
| room for one mass rebuild, **physical**: a second `plain` on disk + the same 1 GB reserve | 5,296,024,064 B | §4 on disk |
| home, logs, `/var` (the same allowance bootc's figure carries) | 1,000,000,000 B | |

- **Floor (A11), as `rime update` is designed: 16 GB** (15.93 GB rounded up to a whole GB).
- **Floor if D6 §11.5 counts compressed bytes: 12 GB** (11.13 GB rounded up). The cache knows
  each NAR's size and the compression ratio measured here (0.42 on disk per unpacked byte for `plain`), so the gate
  could estimate on-disk bytes instead. That is a D6 change, listed in the handover; until it lands, the published
  floor is the first figure.
- **Install-only minimum: 6 GB** (partitions + installed system + 1 GB, rounded up), with no room for an update.
  bootc publishes no figure of this shape.
- **With gaming:** 20 GB as designed. `rime install steam` must refuse by name below it.
- **bootc for comparison: 12 GB.** `installer/rime-install` (~2312-2333) derived it on 2026-10-04 as an install floor
  (8.1 GB peak install + 2 GiB of partitions + headroom), but the release that followed (2026.10.04.2, the apps out of
  the image) installs in 5.4 GB, and a core-rebuild update needs about 5.1 GB free, so a 12 GB bootc disk holds the
  install plus ONE core update, barely (repo CLAUDE.md, 2026-10-04). That is the same shape as this floor: the
  physical 12 GB here matches bootc's 12 GB, and the 16 GB as designed is worse than bootc only because D6 §11.5
  counts unpacked NAR bytes, a ~2.4× over-estimate on `compress-force=zstd:3` (ratio 0.42 above). bootc's own update
  cycle measured about 14.1 GB of root plus 0.5-2 GB of partitions (2026-10-04, 30 GiB VM): "20 GB recommended".

**The ESP.** On fresh installs `/boot` is the ESP (D9) and holds a kernel and initrd per kept generation. During a
mass-rebuild update that is three (previous, current, new): 3 × 52.4 MB = 157.3 MB, plus Fedora's shim
and GRUB. That fits the installer's 512 MiB ESP with 359.6 MB (allowing 20 MB for shim and GRUB) to spare. The boot hook's own refusal (D3) is still
the guard.

## 7. The guard: `checks.size`

`nix/tests/size.nix` reads the rime-vm toplevel's closure (`closureInfo`) and fails when a cut comes back: any
`mbrola-voices`, more or fewer than one JetBrainsMono Nerd Font, `webkitgtk-*+abi=4.1`, the nixpkgs source (or a
module's re-import of it), `/etc/nix/registry.json` naming a store path or not locking nixpkgs to this flake's rev
(`self.inputs.nixpkgs.rev`, `1c89083e451e…` today), the large `-doc` outputs or the NixOS manual, either share/doc
file the scripts read (`niri/default-config.kdl`, `labwc/rc.xml.all`) missing, the 32-bit PipeWire ALSA plugin missing
while gaming is on,
or a closure above the budget (10,200,547,328 B = 9.5 GiB; 9,530,593,800 B measured, so ~640 MiB for the P2 ports
still landing). Raising the budget is a deliberate edit with an entry here saying what grew and why.

Seen:

- `nix build path:.#checks.x86_64-linux.size`: `closure: 1957 paths, 9530593800 bytes NAR (budget 10200547328)`,
  `size guard: all cuts hold`.
- Mutant `second-nerd-font` (nixpkgs' 3.5.0 font back in `fonts.packages`): fails with `FAIL: 2 JetBrainsMono Nerd
  Font paths in the closure, expected exactly 1 (the 3.5.1 pin)`.
- Mutant `registry-store-path` (the registry back on `pkgs.path`): fails three times, `the nixpkgs source (…-source)
  is in the closure`, `/etc/nix/registry.json names a store path` and `does not lock nixpkgs to 1c89083e…`.
- Mutant `registry-unlocked` (the registry on `github:NixOS/nixpkgs` with no rev, which would follow master): fails
  with `/etc/nix/registry.json does not lock nixpkgs to 1c89083e451e891152365b799595cf7790a43325`.
- The shipped registry (`<rime-vm toplevel>/etc/nix/registry.json`): `{"from":{"id":"nixpkgs","type":"indirect"},
  "to":{"owner":"NixOS","repo":"nixpkgs","rev":"1c89083e451e891152365b799595cf7790a43325","type":"github"}}`.
- `checks.vm-boot`, `checks.desktop` and `checks.gaming` pass with the cuts (firstrun seeds niri and labwc from the two
  kept doc files; `fc-list` finds the JetBrainsMono Nerd Font). 32-bit ALSA is asserted by the guard, not by a VM
  test: rime-vm's `/etc/alsa/conf.d/49-pipewire-modules.conf` names `libs.32Bit`; `rime-vm-no-gaming`'s does not.
- Mutants added in review, each seen to fail: `mbrola-back` (stock `pkgs.speechd`: mbrola-voices in the closure and
  the budget passed, 10,206,762,856 B), `webkit-back` (stock `networkmanager-openconnect`: webkitgtk 4.1),
  `docs-back` (`documentation.doc` on: python3, pipewire and wireplumber `-doc`), `manual-back` (`documentation.doc`
  and `documentation.nixos` on: also `nixos-manual-html`), `niri-doc-unlinked` (`/share/doc/niri` dropped from
  `pathsToLink`: `default-config.kdl is missing`), `alsa32-dropped` (`alsa.support32Bit = false` with gaming on).
- `documentation.nixos.enable = false` on its own saves only the 1.5 MB `configuration.nix` man page: nixpkgs installs
  the HTML manual and `nixos-help` only when `documentation.doc` is also on (an earlier `manual-back` with only
  `documentation.nixos` on passed the guard, which is how this was found).

## 8. Handover

- **STATE.md A4 and A11** (the integrator's file): A4 = gaming opt-in (§5); A11 floor = 16 GB as designed
  (12 GB physical, = bootc's 12 GB, once D6 §11.5 counts on-disk bytes), install-only 6 GB, gaming 20 GB; P5 figure =
  2.51 GB (zstd -19; bootc 4.20 GB).
- **D6 §11.5:** count estimated on-disk bytes, not unpacked NAR bytes, in the free-space gate; that drops the floor to
  12 GB (§6).
- **Installer and catalogue:** refuse by name below the floor, and refuse `rime install steam` below the gaming floor.
- **CI builders:** speech-dispatcher, espeak-ng and NetworkManager-openconnect are now built by Rime (overrides are
  cache misses on cache.nixos.org); together a few minutes per mass rebuild.
- **`nix run nixpkgs#…` on a machine** fetches nixpkgs from GitHub on first use (the registry is a locked GitHub rev).
  Nothing in Rime runs it (D5); a user who does needs the network once.
- **The doc cut depends on two files:** a new script that reads `/usr/share/doc/<pkg>/…` must add `<pkg>.doc` and its
  `/share/doc/<pkg>` link in `desktop.nix`, as niri and labwc do.
- **Measured on a laptop-shaped stand-in.** `plain` is built from `nixosModules.default` with the areas on
  (`nix/tests/size.nix`), not from D9's real rime-plain toplevel, which does not exist yet. Re-run
  `size-measure.sh` against it when it lands.
