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
| **P5 fresh-install download** (`plain`, zstd -19), re-measured at the wave-1 integration (§9) | **3.46 GB** (xz: 3.25 GB); 2.51 GB before linux-firmware | bootc 4.20 GB |
| on disk after install (`plain`, btrfs zstd:3, optimised), re-measured (§9) | 5.45 GB; 4.30 GB before linux-firmware | bootc 5.4 GB (2026.10.04.2, 12 GiB VM) |
| `rime install steam` on top of `plain`, re-measured after linux-firmware (§10) | **+0.50 GB** download, +2.20 GB unpacked, +0.88 GB on disk | |
| **A4** | **gaming is opt-in** (`rime.gaming.enable` stays `false`; installs ship `plain`) | 10-04 decision kept |
| **A11 disk floor** (install + one mass rebuild + boot), recomputed at the wave-1 integration (§9) | **19 GB** as `rime update` is designed (§11.5 gate counts unpacked bytes); **14 GB** physically, and once the gate counts on-disk bytes (16 GB and 12 GB before linux-firmware) | bootc 12 GB, which holds its 5.4 GB install plus one core update, barely (§6) |
| A11 install-only minimum (no update headroom), recomputed (§9) | 7 GB (6 GB before linux-firmware) | no bootc counterpart published |
| A11 floor **with gaming** (§10) | **22 GB** as designed; **16 GB** physical; 8 GB install-only | |

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

(First measurement, before linux-firmware. Re-measured in §10: **+0.50 GB** download (3.46 → 3.95 GB), +2.20 GB
unpacked, +0.88 GB on disk; 14 % more download.)

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

> **Superseded at the wave-1 integration (§9):** linux-firmware joined the closure. The current figures are a P5
> download of **3,458,585,216 B (3.46 GB)** and a floor of **19 GB as designed, 14 GB physical, 7 GB install-only**.
> The text below is the first measurement, kept because §9 is computed the same way.

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
- **With gaming:** 20 GB as designed (superseded: **22 GB** as designed, 16 GB physical, §10). `rime install steam`
  must refuse by name below it.
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
or a closure above the budget (11,274,289,152 B = 10.5 GiB since the wave-1 integration, §9; it was 10,200,547,328 B =
9.5 GiB over 9,530,593,800 B measured). Raising the budget is a deliberate edit with an entry here saying what grew and
why.

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

- **STATE.md A4 and A11** (the integrator's file): A4 = gaming opt-in (§5); A11 floor = 19 GB as designed
  (14 GB physical, once D6 §11.5 counts on-disk bytes), install-only 7 GB; P5 figure = 3.46 GB (zstd -19; bootc
  4.20 GB). These are the §9 figures, after linux-firmware joined the closure. The gaming floor is **22 GB** as
  designed (16 GB physical, 8 GB install-only) and the `rime install steam` delta **+0.50 GB** download, both
  re-measured after linux-firmware (§10).
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

## 9. Wave-1 integration: what grew, and the budget raised once

Measured on 2026-11-02 on `feat/nixos` after the eleven wave-1 branches merged (rime-shell overridden to the local
`feat/nixos` with the polish merge, `--override-input rime-shell path:/work/shell`).

**`checks.size` failed on the merged tree:** `closure: 2079 paths, 10556067672 bytes NAR (budget 10200547328)`,
`FAIL: closure NAR size 10556067672 > budget 10200547328`. (The first integration run, on the tree before the
`rime trust` fix commit, printed 10,556,067,944 B; that commit rebuilt rimed, which is the likely source of the 272 B
difference, not measured.)

What grew, rime-vm toplevel, by store name (`nix path-info -r --json` of the size branch's toplevel `1kzkv1dp…` against
the merged tree's `mxgwq8km…`, NAR bytes summed per name): 9,530,593,800 B over 1676 names → 10,556,067,944 B over
1793 names. 1,038,712,400 B added, 18,068,616 B removed (17.2 MiB of it is rime-shell under its old name, back under
the local name).

| added | MiB | brought by |
|---|---|---|
| `linux-firmware-20261019-zstd` | 825.2 | `fedora-parity.nix`: `hardware.enableRedistributableFirmware` (Fedora installs linux-firmware; without it Wi-Fi, GPUs and Bluetooth on real hardware have no firmware) |
| `skopeo-1.24.1` | 32.4 | `update.nix`: the update client fetches the signed documents with skopeo |
| `nodejs-slim-24.21.0-npm` | 12.2 | `packages-catalogue.nix`: `/etc/rime/catalogue.json` names `${nodejs}/bin/npm` for the `user-npm` route (claude-code), so npm is in the system closure by design (`rime.catalogue.npm.package`) |
| `sof-firmware`, `libreelec-dvb-firmware`, `alsa-firmware` | 12.1 + 9.4 + 4.4 | the same firmware switch |
| `inotify-tools` | 11.7 | kmscon's runtime dependency (polish: kmscon owns the text consoles) |
| `timidity`, `appimage-run` FHS env | 9.7 + 6.9 | `packages-catalogue.nix`: appimage-run (with binfmt) and its FHS environment |
| `zsh` (+ info) | 7.4 + 1.7 | `fedora-parity.nix` and `hostlayer.nix`: zsh is the default login shell, as on bootc |
| `ipp-usb` | 7.3 | `fedora-parity.nix`: driverless USB printers and scanners, as Fedora installs |
| `kmscon`, `zram-generator` | 2.0 + 1.0 | polish (consoles) and `fedora-parity.nix` (zram swap) |

linux-firmware is 80 % of the growth and is not a cut candidate: it is what makes real hardware work. It is already
zstd-compressed, so it costs nearly its full size in the download as well.

**Budget raised once**, in `nix/tests/size.nix`: 10,200,547,328 B (9.5 GiB) → **11,274,289,152 B (10.5 GiB)**, which
leaves ~690 MiB over 10,556,067,672 B for P2. `checks.size` passes with it; every cut in §3 still holds (the guard's
other assertions did not change).

**`plain` re-measured** (`PHASES="download disk" size-measure.sh <dir> plain`, same tools as §2; the run used the
committed flake.lock, whose rime-shell differs from the override only by the polish merge):

| `plain` | before (§4) | now |
|---|---|---|
| paths | 1674 | 1807 |
| unpacked (NAR) | 9,098,365,168 B | 10,382,099,736 B |
| download, zstd -19 | 2,513,265,034 B | **3,458,585,216 B (3.46 GB)** |
| download, xz | 2,314,174,960 B | 3,246,717,716 B |
| on disk, `df` used, optimised | 4,296,024,064 B | **5,450,555,392 B (5.45 GB)** |
| on disk per unpacked byte | 0.47 | 0.52 |
| kernel + initrd | 52,439,626 B | 52,435,933 B |

The P5 fresh-install figure is now **3.46 GB**, still 0.74 GB under bootc's 4.20 GB. Recomputed A11 floor, same parts
as §6: partitions 537,919,488 + installed 5,450,555,392 + one rebuild + 1,000,000,000 for home and logs:

- **as designed** (rebuild room = unpacked 10,382,099,736 + 1 GB reserve): 18,370,574,616 B → **19 GB**;
- **physical** (rebuild room = a second install on disk + 1 GB): 13,439,030,272 B → **14 GB**;
- **install-only**: 6,988,474,880 B → **7 GB**.

The ESP check in §6 is unchanged (3 × 52.4 MB). Not re-measured: `rime-vm`, the `*-no32` variants and `plain-gaming`,
so the gaming floor and the `rime install steam` delta in §1 are pre-firmware figures.

## 10. `plain-gaming` re-measured (D5, 2026-11-02)

Measured on 2026-11-02 on the `nix/d5` branch (base `0545be610`, wave 1 merged, plus D5's rimed and catalogue
changes), with the committed flake.lock, `PHASES="download disk" size-measure.sh <dir> plain plain-gaming`, same tools
as §2. Both variants come from the same run, so the delta is a difference of two totals measured together. Raw rows
(columns as §4):

```
plain         1807  10382141376  3458656531  3246764060  52435933  5414600704  4890623218  10449990790  5442637824  4869867905
plain-gaming  2108  12579425640  3954671995  3694696704  52435933  6388350976  5664384969  12627766927  6327009280  5583781592
```

| | `plain` | `plain-gaming` | delta |
|---|---|---|---|
| paths | 1807 | 2108 | +301 |
| download, zstd -19 | 3,458,656,531 B | 3,954,671,995 B | **+496,015,464 B (+0.50 GB, +14 %)** |
| download, xz | 3,246,764,060 B | 3,694,696,704 B | +447,932,644 B |
| unpacked (NAR) | 10,382,141,376 B | 12,579,425,640 B | +2,197,284,264 B (+2.20 GB) |
| on disk, `df` used, optimised | 5,442,637,824 B | 6,327,009,280 B | +884,371,456 B (+0.88 GB) |

`plain` here differs from §9 by D5's rimed and catalogue changes (zstd -19 3,458,656,531 against 3,458,585,216 B;
on disk 5,442,637,824 against 5,450,555,392 B, which includes run-to-run btrfs variance); the floor below uses only
this run's `plain-gaming`.

The delta is 0.03 GB smaller than §5's +0.53 GB; the paths that changed between the two measurements were not
compared, so the cause is not stated here. A4 (opt-in) stands. **Gaming floor**, the §6 parts with `plain-gaming`:
partitions 537,919,488 + installed 6,327,009,280 + one rebuild + 1,000,000,000 for home and logs:

- **as designed** (rebuild room = unpacked 12,579,425,640 + 1 GB reserve): 21,444,354,408 B → **22 GB**;
- **physical** (rebuild room = a second install on disk + 1 GB): 15,191,938,048 B → **16 GB**;
- **install-only**: 7,864,928,768 B → **8 GB**.

`rime install steam` must refuse by name below the gaming floor (22 GB until D6 §11.5 counts on-disk bytes, then
16 GB). `rime.gaming.enable`'s option text quotes the new delta. Log: `/var/lab/rime-nix/artifacts/d5/size-run.log`
(not committed).

## 11. The floors counted on disk, and the release variants (wave 3 "updates", 2026-11-02)

D6 §11.5 now counts on-disk bytes, so the floors of §6 and §10 are recomputed. All of them, and the ratio the update
gate uses, live in ONE file, `nix/release/floors.nix`, which `/etc/rime/update.toml` (`on_disk_ratio`),
`/etc/rime/catalogue.json` (`floors`, read by `rime install`) and `checks.release-plan` read. The installer should read
`diskGB` from the same file (handed to installer2, who owns `nix/installer` and `installer/rime-install-nixos.sh`).

**The ratio.** The measured on-disk/unpacked ratios of §10 are 0.524 (`plain`: 5,442,637,824 / 10,382,141,376) and
0.503 (`plain-gaming`). The gate uses **0.55**, the larger plus a margin, so its estimate errs high. Only a store on
btrfs mounted `compress-force=zstd` uses it; any other filesystem counts the unpacked size (ratio 1.0). §6's 0.42 came
from the first measurement and is not used.

**The ESP is 2 GiB, not 512 MiB.** §6 took the partitions from the bootc installer (`installer/rime-install`:
1 MiB + 512 MiB). The NixOS install path (`installer/rime-install-nixos.sh`, `NIXOS_ESP_MIB=2048`) makes a 2 GiB ESP,
because `/boot` holds a kernel and initrd per kept generation (D9). The floors below count 2 GiB + 2 MiB of GPT and
alignment. That is why they are 2 GB above the "14 GB plain, 16 GB gaming" the D5 handover expected.

A floor = partitions + the installed system on disk + the gate's need for one mass rebuild (a whole second closure,
`ceil(unpacked × 0.55)` + 1 GB reserve) + 1 GB for home and logs:

| variant | installed on disk | gate need for a rebuild | floor | published |
|---|---|---|---|---|
| `plain` | 5,442,637,824 B | 6,710,177,757 B | 15,302,396,381 B | **16 GB** |
| `plain-gaming` | 6,327,009,280 B | 7,918,684,102 B | 17,395,274,182 B | **18 GB** |
| `plain-virtualization` (estimated) | 5,800,374,254 B | 7,067,914,187 B | 16,017,869,241 B | **17 GB** |
| `plain-gaming-virtualization` (estimated) | 6,684,745,710 B | 8,276,420,532 B | 18,110,747,042 B | **19 GB** |

The two virtualization rows are estimates: no virtualization system was installed and measured with `df`. They add
the closure delta measured when the four release variants were built together (2026-11-02, this wave:
`plain-virtualization` adds 89 paths of 658,470,728 B and drops 29 of 8,040,856 B, net 650,429,872 B) at the gate's
ratio. Measuring them on disk is a follow-up; `nix/release/floors.nix` marks them `estimated = true`.

**Who refuses what.** `rime install steam` (and every package of a feature the system does not carry) looks up the
floor of the system it would select (base + every switched-on feature + this one, or the largest floor of its subsets
when that exact system has none) and refuses by name, before the switch file is written, when the filesystem holding
`/nix` is smaller than the floor's store part (15,245,693,382 B for `plain-gaming`): "steam: Rime's Gaming system needs a
disk of at least 18 GB, and the filesystem holding /nix here is N GB (...). Nothing was changed." Proven in
`checks.update-e2e` on its real 12 GiB disk and, for the combined lookup, in `checks.catalogue` with lab floors.
`rime update` itself does not apply floors: its own gate (D6 §11.5) refuses an update that does not fit.

**The four release variants**, all built at once (2026-11-02, `.#rime-release-toplevels`; NAR sizes from
`nix path-info -rS`):

| variant | paths | unpacked (NAR) |
|---|---|---|
| `plain` | 1851 | 10,442,212,352 B |
| `plain-gaming` | 2152 | 12,639,497,504 B |
| `plain-virtualization` | 1911 | 11,092,642,224 B |
| `plain-gaming-virtualization` | 2212 | 13,289,927,384 B |

These `plain` figures are of the wave-3 tree (44 more paths than §10's run, +60 MB unpacked); the floors keep §10's
on-disk measurement, which this change does not move by more than the rounding.

**The class 2 catalogue** (`.#rime-release-catalogue`, the 13 buildable apps, now published with every release):
622 paths, 7,128,130,304 B unpacked as a union; 238 of them (5,313,430,424 B) are not already in `plain`. Per app
(closure, unpacked): clang 2.12 GB, rust 1.84 GB, wine 1.83 GB, equibop 1.55 GB, java 1.01 GB, mangohud 0.92 GB,
android-tools 0.29 GB, neovim 0.27 GB, go 0.27 GB, llama-cpp 0.15 GB, wireguard-tools 0.12 GB, htop 0.06 GB,
usbutils 0.06 GB. Upstream nixpkgs serves most of it, so Rime's own cache grows by much less, but the hosted release
runner now builds or fetches about 5.3 GB more unpacked per release; the "Free disk for the closure" step must keep
room for it.
