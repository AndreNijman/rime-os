# Delta transport (G2): `zstd --patch-from` against the previous release

Status: **measured** on 2026-11-02 in the `rime-nix` container on katana (Nix 2.35.4, zstd 1.5.7). Every number
below comes from a command in this file; the tool is `nix/release/delta/rime_delta.py`, the raw per-path data is in
`nix/release/delta/data/<pair>/`, and `checks.x86_64-linux.delta` proves the tool on fixtures. GB is 10^9 bytes.
Since 2026-11-02 the shipped client and pipeline (§7) are measured too: §5.2 (VMs, the client's Nix 2.34.9), §6.1, §8.1.

This closes the P2 rows **G1**, **G2** and **G3** of `docs/nixos/p2-gates.md` (PLAN D6 "G2 delta transport", §7.2
Q4, decision A3: "G2 delta transport is measured before P4").

## 1. Results in one place

Bytes a machine on the previous release downloads (GB = 10^9 bytes; "shipped" = what `nix copy
?compression=zstd` writes today, level 3; "delta" = `zstd -19 --long=27 --patch-from` against the same path in the
previous release, a path with no base counted whole):

| release (all `nixosConfigurations.rime-vm`) | paths added | shipped | full at -19 | **delta** | delta / shipped |
|---|---|---|---|---|---|
| G1 Rime-only (rimed + rime-shell change, same nixpkgs) | 31 | 27.6 MB | 20.5 MB | **3.2 MB** | 11.8 % |
| G1 weekly nixpkgs bump (no glibc/stdenv change, kernel .58 → .59) | 161 | 745.9 MB | 589.3 MB | **230.9 MB** | 31.0 % |
| G2 mass rebuild (glibc, systemd 262, Qt 6.12, kernel, firmware) | 2067 of 2088 | 4,100.2 MB | 3,189.9 MB | **582.0 MB** | 14.2 % |

Since 2026-11-02 `stage` writes level 19 without `--long` (§8.1), so a weekly release downloaded whole is 637.6 MB
for these 161 paths by the zstd CLI, not 745.9 MB (the pipeline itself wrote 641.5 MB for the 156 a machine lacked
in §5.2: §8.1).

Client import, prototype (G3 before the client shipped: `rime_delta.py apply-all`, local disk, network excluded):

| release | plain `nix copy` | deltas: rebuild + verify + import | peak RSS (deltas) | break-even link speed |
|---|---|---|---|---|
| weekly nixpkgs bump | 3.4 s | 42.1 s | 788 MiB | ~13.3 MB/s |
| mass rebuild | 10.3 s | 271.2 s | 1,640 MiB | ~13.5 MB/s |

Every delta import ended with the toplevel valid and `nix store verify` clean; a delta that does not rebuild the
signed `NarHash` is refused before anything is imported (`checks.x86_64-linux.delta`, 6 mutants caught).

**G3 with the shipped client** (§5.2: `rime update --download-only` in a VM, the weekly pair, a release made by the
real pipeline; whole paths now at zstd level 19, §8.1):

| weekly pair, 156 paths | downloaded | wall | client CPU |
|---|---|---|---|
| whole, unshaped virtual link | 641.5 MB | **11.4 s** | 21.6 s |
| deltas, unshaped virtual link | **235.6 MB** | 28.4 s | 34.6 s |
| whole, 100 Mbit/s | 641.5 MB | 55.8 s | 41.3 s |
| deltas, 100 Mbit/s | **235.6 MB** | **35.1 s** | 37.8 s |

Over 30 days at A3's cadence (assumed: 3 weekly bumps + 1 mass rebuild): **6.34 GB** today, **4.96 GB** with -19
alone, **1.27 GB** with deltas. A bootc core rebuild is 1.74 GB per update (rime-os #123); four of them are 6.96 GB.
The mass rebuild with deltas (0.58 GB) is 3x smaller than one bootc core rebuild. Recommendation: §7.

## 2. Method

### 2.1 What is measured

For a release pair (old toplevel A, new toplevel B):

1. `closure`: both closures with `nix path-info -r --json` (path, name, narSize, narHash).
2. `pair`: every path in B that A does not have is **added**; a machine on A downloads exactly these. For each added
   path the base candidates in A are, best rule first: the same name (a rebuild: same pname, version and output),
   the same package (pname + output, any version: `hyprland-0.56.2` for `hyprland-0.57.0`, never `-dev` for `out`),
   the same package with digits dropped from the pname (`python3.13-foo` for `python3.14-foo`). Up to four
   candidates of the first rule that matches, closest NAR size first. No base = the path costs its full size.
3. `measure`, per added path: its NAR (`nix-store --dump`, checked against the narHash from path-info), then
   - `full_nix`: `zstd -3` of the NAR, what the D6 pipeline ships today (`nix copy --to file://…?compression=zstd`
     with `compression-level` unset is libzstd's default level 3; checked against the real `FileSize` of a
     `nix copy` cache in §5);
   - `full_zstd`: `zstd -19 --long=27` of the NAR (the probe's level, the best a full download can do with zstd);
   - `delta`: `zstd -19 --long=27 --patch-from=<base NAR>` against each candidate, the smallest kept. **Every delta
     is decoded again and its sha256 compared with the new NAR**: a delta that does not rebuild the path exactly
     fails the run.
4. `summary`: totals and the 25 largest paths after delta.

`--long=27` and `--patch-from`: in patch-from mode zstd raises the window to cover the base, whatever `--long`
says. Measured on a 550 MiB `llvm-21.1.8-lib` NAR (`artifacts/g2/window-test.log`): `--long=27` and `--long=30`
give the same 278,931 B delta and the frame header says `Window Size: 550 MiB`. So the encoder flags are the
specified ones, and the **decoder must allow the base's window** (`zstd -d --long=<log2 of the larger NAR, rounded
up>`; up to 2^29 = 512 MiB+ in these pairs, see `wlog` in the TSVs). That is the client's memory cost (§5).

### 2.2 The pairs, and why each is faithful

All three are `nixosConfigurations.rime-vm` (every Rime area on, gaming on, nixpkgs' kernel, no NVIDIA userspace;
`docs/nixos/closure.md` §1). The installed `plain` shape differs (Rime's kernel + NVIDIA, no gaming); the same
mechanisms apply and §6 says where the difference matters.

| pair | old → new | what changed | why it is the case it stands for |
|---|---|---|---|
| **g1-rime** | feat/nixos `0545be610` with rime-shell `a88d034` and an older rimed → `0545be610` as locked (rime-shell `8932539`); nixpkgs `1c89083` on both sides | rime-shell `a88d034` → `8932539`; rimed: 12 source files, +336/−40 lines (`data/g1-rime/rimed-old-to-new.patch`: recent `rime trust`, agent sandbox and NixOS update fixes taken back out); every /etc and unit path that names them | a Rime-only release (the task's "rimed + rime-shell bumped"): about a week of rimed work and one Shell merge |
| **g1-nixpkgs** | nixpkgs-unstable `3ec6363` (10-23) → `1c89083` (10-30), Rime at `0545be610` on both sides | one week of the channel: 34 version changes (`data/g1-nixpkgs/versions.txt`: kernel 6.18.58 → .59, firefox 158 → 159, mesa 26.2.5 → .6, hyprland 0.56.2 → 0.57.0, …), the rest rebuilt under the same name | a weekly release **without** a mass rebuild: glibc (`pqyiidz5…-glibc-2.44-57`) and gcc-lib (`7a2cjhcg…`) are the same store paths on both sides. PLAN §3.3 used this exact window as "no toolchain rebuild" (0.80 GB) |
| **g2-mass** | nixpkgs-unstable `55bfb4d` (10-14) → `3ec6363` (10-23), Rime at `0545be610` on both sides | glibc 2.44-25 → 2.44-57 (`h4wfwic1…` → `pqyiidz5…`), gcc-lib rebuilt, systemd 261.3 → 262, util-linux 2.42.3 → .4, kernel 6.18.56 → .58: **2067 of 2088 paths change** | the mass rebuild A3 batches (glibc/stdenv). It is 9 days, not 2-4 weeks: the criterion is "stdenv changed", which this pair meets, and it is the 10-19 stdenv change PLAN §3.3 counted |

The old sides of g1-nixpkgs and g2-mass are built by `nix/release/delta/pair.nix`: Rime's modules evaluated
against the older nixpkgs, as if Rime had released then. Two compat shims make that evaluate (an option sink for
`services.avahi.settings`, which arrived between 10-14 and 10-23, and `hyprland-guiutils` falling back to the old
`hyprland-qtutils`); assertions are dropped (the Hyprland >= 0.57 one). Nothing from the newer nixpkgs is overlaid,
so the old side keeps the packages that revision really had. The shims change a few /etc lines on the old side
only; those paths are rebuilt in every release anyway. Each old side was built into its own chroot store
(`nix build --store /work/scratch/g2/store-<rev>`, `/var/lab/rime-nix/artifacts/g2/scratch/buildold.sh`) so the shared store was not filled.

## 3. G1: a release without a mass rebuild

`nix/release/delta/data/g1-rime/summary.txt`, `data/g1-nixpkgs/summary.txt` (measure wall: 80 s and 1128 s, 6 jobs):

| pair | paths added | NAR | full, zstd -3 (shipped) | full, zstd -19 --long=27 | **delta** | delta / shipped |
|---|---|---|---|---|---|---|
| g1-rime | 31 of 2079 | 67.8 MB | 27.6 MB | 20.5 MB | **3.2 MB** | **11.8 %** |
| g1-nixpkgs | 161 of 2079 | 2,111 MB | 745.9 MB | 589.3 MB | **230.9 MB** | **31.0 %** |

(The delta column counts a path with no base at its full -19 size: 4 paths, 2.4 MB, in g1-nixpkgs.)

Where the bytes go:

- **g1-rime.** rimed: 14.6 MB shipped → 3.24 MB delta (a Rust rebuild after a +336/−40 line change moves code
  and offsets across the binaries). rime-shell (QML, one merge): 12.1 MB shipped → **6.3 kB**. The other 29 paths
  (units, /etc, system-path, the toplevel) are 0.9 MB shipped → under 5 kB together. A Rime-only release is ~3 MB
  with deltas against ~28 MB today, and the cost is almost entirely the compiled Rust.
- **g1-nixpkgs.** The kernel bump (6.18.58 → .59) is 4 paths and **137.7 MB of the 230.9 MB delta**: the modules
  path (133.8 MB full → 92.0 MB delta), the initrd (36.0 → 32.1) and the kernel image (14.8 → 13.7). Every one of
  the 7,294 modules is stored `.ko.xz` and the initrd and bzImage are compressed: a changed input changes every
  compressed byte after it, so there is nothing for `--patch-from` to match. Without the kernel the release is
  560.1 MB shipped → **93.1 MB** delta (16.6 %). The rest is firefox 159 (104.0 → 38.8 MB), mesa (2 × ~33 → ~10),
  sing-box, deno, hyprland 0.57 (51.8 → 3.8); 121 paths rebuilt under the same name cost 4 MB together.
- A full download at `zstd -19 --long=27` alone is 21 % smaller than what the pipeline ships at level 3 (589 vs
  746 MB), with no client change at all (§7).

## 4. G2: a mass rebuild

`data/g2-mass/summary.txt` (measure wall: 3921 s, 6 jobs, on a machine shared with other agents' VM tests):

| pair | paths added | NAR | full, zstd -3 (shipped) | full, zstd -19 --long=27 | **delta** | delta / shipped |
|---|---|---|---|---|---|---|
| g2-mass | 2067 of 2088 | 10,616 MB | 4,100.2 MB | 3,189.9 MB | **582.0 MB** | **14.2 %** |

The pair is close to the worst case for a mass rebuild: besides glibc 2.44-25 → -57 (and so every path), it carries
systemd 261.3 → 262, Qt 6.11.2 → 6.12.0, util-linux 2.42.3 → .4, python 3.14.7 → .8, a kernel bump (6.18.56 →
.58) and a new linux-firmware (20260916 → 20261019): 239 version changes in all (`data/g2-mass/versions.txt`).

- **The 1,783 paths rebuilt under the same name** (the "mass" in mass rebuild: same source, new glibc in their
  references) are 1,998 MB shipped and **76 MB as deltas (3.8 %)**. Their bytes barely change: mostly the store
  paths they reference.
- **The 283 paths that changed version** are 2,102 MB shipped and 506 MB as deltas. Of that, the kernel is 182 MB
  (4 paths, 186 MB shipped: the `.ko.xz` modules alone are 133.8 → 133.3 MB), linux-firmware 76.5 MB (858.8 MB
  shipped: its files are compressed one by one, so unchanged firmware files stay identical and match), then
  qtwebengine (117.1 → 49.7), firefox 158.0.1 (104.2 → 35.8), the initrd (35.9 → 35.0), python3 (55.4 → 33.5),
  qtdeclarative, mesa.
- Without the kernel the release is 3,914 MB shipped → 400 MB as deltas.
- `zstd -19 --long=27` alone, no delta, would ship 3,190 MB instead of 4,100 MB (−22 %).
- The decoder needs a 1 GiB window for linux-firmware (865 MB NAR, `wlog` 30) and 256-512 MiB for 12 more paths;
  2052 of 2067 deltas fit the default 128 MiB.

**Against bootc** (PLAN §3.3, `docs/nixos/closure.md`): a bootc core rebuild is 1.74 GB since rime-os #123's
rechunking. A NixOS mass rebuild is 4.10 GB shipped as today (PLAN §3.3 measured 4.02 GB for the
similar window `c59305b → 1c89083` from cache.nixos.org's narinfos), **0.58 GB with deltas**: smaller than bootc's core rebuild by 3x. A weekly nixpkgs
release is 0.75 GB → 0.23 GB; a Rime-only release 28 MB → 3 MB.

## 5. G3: the client import step

The prototype is `rime_delta.py apply` (one path) and `apply-all` (a whole release), standard library only:

1. fetch the path's narinfo (signed, from the release cache) and its delta;
2. dump the base path's NAR from the machine's store and rebuild the new NAR with `zstd -d --long=<w>
   --patch-from=<base NAR>` (`w` covers the larger of the two NARs, §2.1);
3. **refuse** unless the rebuilt NAR's size and sha256 equal the narinfo's `NarSize` and `NarHash`;
4. write a one-path local file cache: the narinfo exactly as signed, with only the fields the signature does not
   cover (`URL`, `Compression`, `FileHash`, `FileSize`; `ValidPathInfo::fingerprint` signs path, narHash, narSize,
   references) pointed at the rebuilt NAR, and `nix copy --no-recursive --from file://…` it into the store, so **Nix
   itself checks the signature and the hash again** before the path becomes valid;
5. `apply-all`: references first, a path with no base is fetched whole from the release cache with `nix copy`.

`checks.x86_64-linux.delta` (`nix/tests/delta.nix`) proves it on fixtures: pairing (outputs pair with the same
output), measuring (every delta re-decoded; a zstd shim that corrupts what it decodes makes `measure` fail with
"does not rebuild the NAR"), `apply` into a chroot store (`diff -r` against the release's path,
`nix store verify` with the trusted key), five refusals (a bit-flipped delta, a truncated delta, a narinfo whose
NarHash is another path's, the delta applied to the wrong base, a narinfo signed by an untrusted key: each exits
non-zero, says why, and imports nothing), and `apply-all` moving a store from the old release to the new one with
two paths rebuilt from deltas (the server holds no NAR for them) and one fetched whole. Six mutants each make the
check fail (`artifacts/g2/delta-check-mutants.log` for the first five, `artifacts/verify-g2/delta-check-mutants-6.log`
for all six): `no-narhash-check`, `pair-ignores-output`, `nix32-reversed`, `keep-nothing`, `apply-all-no-delta`,
`measure-trusts-delta` (added in review: `measure` no longer compares the decoded delta with the NAR).

**Benchmark** (`nix/release/delta/lab/g3.sh`, `g3-baseline.sh`, `g3-all.sh`; logs `data/g3-all.log`, `data/g1-nixpkgs/g3-*.log`,
`data/*/apply-all*.{json,time}`, `data/*/nixcopy-baseline*.time`): the machine is a chroot store holding the old
release; the server is a file binary cache of the new release's whole closure written by `nix copy
?compression=zstd` and signed with a throwaway TEST key; the deltas are the ones `measure --keep` wrote. The
baseline removes the added paths from the machine store again (`nix store delete --ignore-liveness` on that scratch
chroot store only) and runs a plain `nix copy` of the new toplevel from the same cache. Everything is local NVMe
with a warm page cache, so the numbers are the client's own cost with the network taken out. Load average is the
whole of katana, shared with other agents' VM tests (20 threads).

| run | downloaded | wall | CPU user + sys | peak RSS | disk read / written |
|---|---|---|---|---|---|
| **g1-nixpkgs** (161 paths, 2.11 GB NAR): plain `nix copy`, load 5 | 746.2 MB (sum of the narinfos' `FileSize`) | **3.4 s** | 6.8 s | 540 MiB | 0.75 / 2.1 GB |
| same, load 34 | same | 21.0 s | 18.6 s | 489 MiB | 0.8 / 2.1 GB |
| g1-nixpkgs deltas, load 6 (run 3) | **≈231 MB** (228.4 MB of deltas + 4 new packages whole: 9.2 MB NAR, ≈2.5 MB compressed) | **42.1 s** | 17.8 s | **788 MiB** | 3.0 / 6.6 GB |
| same, load 34-37 (run 1) | same | 73.5 s | not kept | 788 MiB | not kept |
| same, load 55-58 (run 2) | same | 455.8 s (12 % CPU: waiting on disk) | 57.1 s | 786 MiB | 4.8 / 6.6 GB |
| **g2-mass** (2067 paths, 10.62 GB NAR): plain `nix copy`, load 6 | 4,102.4 MB | **10.3 s** | 31.4 s | 1,425 MiB | 4.1 / 11.0 GB |
| g2-mass deltas, load 7 | **582.0 MB** (2066 deltas + 1 path whole, 1.2 kB) | **271.2 s** | 138.2 s | **1,640 MiB** | 11.5 / 33.1 GB |

Every delta run ends with the new toplevel valid in the machine store and `nix store verify` (contents and TEST
signature) clean on all added paths (161 and 2067). The `FileSize` sums of what `nix copy` wrote (746,244,317 B and
4,102,426,733 B) agree with the measured `zstd -3` columns (745,915,063 B and 4,100,221,824 B) to 0.05 %, which
confirms the "shipped" baseline of §2.1.

What the client pays, measured:

- **3x the disk writes and 2.5-4.5x the CPU** of a plain `nix copy`: every delta path dumps its base NAR, writes
  the rebuilt NAR, hashes it, and `nix copy` reads it back and writes the store. The prototype does one path at a
  time (42-50 % of one CPU); a real implementation would stream the rebuilt NAR into the import and run paths in
  parallel.
- **Memory: the decoder window.** The window is the base NAR's size: 2^30 (1 GiB) for linux-firmware's 865 MB NAR,
  2^29 for firefox, mesa, llvm, qtwebengine. Peak RSS 788 MiB (weekly) and 1,640 MiB (mass). Fine on an 8 GB
  machine; a client on a small machine needs a cap (skip the delta for a base above N MiB and fetch that path whole).
- **Break-even bandwidth.** The weekly release saves 515 MB for 38.7 s of extra local work; the mass rebuild
  saves 3,520 MB for 260.9 s. Both come to **~13.5 MB/s (~108 Mbit/s)**: below that link speed the prototype is
  also faster end to end, above it a plain `nix copy` finishes first but deltas still move 3-7x fewer bytes. On a
  busy disk (run 2) the break-even falls to ~1.2 MB/s. A streamed, parallel import would move the break-even up.

### 5.2 G3 in VMs: `rime update` with the shipped client (2026-11-02)

`nix/release/delta/lab/g3-vm.sh` runs `nix/tests/update-delta.nix` with `pair` = the weekly pair of §3 (old
`nixos-system-rime-vm-26.11.20261023.3ec6363`, new `…20261030.1c89083`). A server VM (4 vCPUs, 6 GiB) runs the
release pipeline as the build job does: `stage` (level 19) and `publish` of the old release, then `stage`, `deltas`
(against the old release, fetched from the channel and verified with cosign, `--jobs 2`) and `publish` of the new
one, to the fake GitHub with TEST keys. A client VM (4 vCPUs, 8 GiB, the client's Nix 2.34.9, its own 6.12 kernel so
the pair's kernel is not already on its disk) holds the old closure and runs `rime update --download-only` four
times; between runs the new paths are deleted again and both VMs drop their page caches. Wall is the whole command;
CPU is the client VM's busy time from `/proc/stat` (rime, curl, zstd, nix and the daemon); bytes are what the client
asked GitHub for (the manifest's sizes) and what its NIC received. Log `artifacts/deltas/g3-vm.log`, one JSON line per
run in `artifacts/deltas/g3-results.jsonl`, committed as `nix/release/delta/data/g3-vm/g3-results.jsonl` with the
run's G3 lines (`g3-summary.txt`); whole test 1,705 s, katana load 2-3.

| run | GitHub files | GitHub bytes | NIC received | wall | `rime` download phase | client CPU | iowait |
|---|---|---|---|---|---|---|---|
| deltas, unshaped | 150 deltas + 6 whole | **235,562,875 B** | 247.8 MB | 28.4 s | 25.8 s | 34.6 s | 19.6 s |
| `--no-deltas`, unshaped | 156 whole | 641,503,083 B | 671.4 MB | **11.4 s** | 10.2 s | 21.6 s | 5.8 s |
| deltas, 100 Mbit/s (`tc tbf` on the server) | 150 deltas + 6 whole | **235,562,875 B** | 247.4 MB | **35.1 s** | 33.9 s | 37.8 s | 10.9 s |
| `--no-deltas`, 100 Mbit/s | 156 whole | 641,503,083 B | 671.4 MB | 55.8 s | 54.6 s | 41.3 s | 6.4 s |

- **Every run passed**: the new toplevel valid (Nix imported every path only after checking its signed NarHash),
  `nix-store --verify-path` clean on the first 40 of the 156 new paths (the script checks no more), 0 deltas refused. `rime` said
  "156 new store paths, 0.24 GB to download with deltas (0.64 GB without; 2.11 GB unpacked)" and "150 paths rebuilt
  from deltas (182.3 MB), 6 downloaded whole (53.3 MB)". The release's table has 153 deltas (235.9 MB); 3 are for
  paths the client already held, so it never fetched them. The 6 whole paths have no delta in the table (a new
  package, or a delta over 80 % of the whole file).
- **Bytes: 63 % fewer than whole at level 19, 68 % fewer than today's level 3** (745.9 MB, §3), matching §3's
  measurement (230.9 MB for 161 paths) within 2 %.
- **Time: deltas win on a 100 Mbit/s link (35 s against 56 s) and lose on the unshaped virtual link (28 s against
  11 s; whole paths arrived at ~63 MB/s there)**. From the 100 Mbit/s runs the client's own work is ~16 s with deltas and ~5 s without, which puts
  the break-even near **34 MB/s (~275 Mbit/s)**, 2.6x the prototype's ~13 MB/s (§5): the streamed, parallel import
  (§7 item 3) did what §5 said it would. Above that speed a machine still moves 2.7x fewer bytes and pays ~17 s more.
  The unshaped run also spent 19.6 s in iowait (both VMs share katana's NVMe and the host's page cache).
- The pipeline side: `deltas` + `publish` of the new release took 965 s in the server VM (2 jobs, 4 vCPUs); `stage`
  at level 19 took 470-490 s per release there (§8.1).
- Not measured here: peak RSS in the VM (the prototype's 788 MiB was the decoder windows, which the client now caps,
  §7 item 4), the mass-rebuild pair (its old side lives in a chroot store, not in a toplevel the VM test can take; the
  prototype's §5 numbers stand for it), and a real internet link.

## 6. What deltas cannot shrink, and what they cost the release pipeline

- **Compressed content inside a path.** The kernel's `.ko.xz` modules, the zstd initrd and the bzImage (g1-nixpkgs:
  138 of 231 MB), `linux-firmware-*-zstd` (§4), and anything else a package compresses itself. Those bytes change
  everywhere when one input byte changes. Rime builds its own kernel (D3): uncompressed modules
  (`CONFIG_MODULE_COMPRESS_NONE`) would put plain ELF in the NAR, let the transport compress it, and let deltas see
  it. Measured in §6.1: the biggest lever on the weekly delta, and not shipped yet.
- **Compiled code after a source change.** rimed (Rust) kept 22 % of its full size as a delta for a +336/−40 line
  change; firefox, mesa, deno and sing-box keep 25-40 % across a version bump.
- **New packages** have no base (4 paths in g1-nixpkgs, 2.4 MB).
- **Machines more than one release behind.** A delta is made against the previous release only; a machine two
  releases behind gets full paths (or a chain of deltas, which multiplies the client's work). The manifest's
  `delta.from` (updates-and-release.md) already names the one release a delta applies to.
- **Release pipeline cost.** Making the weekly release's deltas took 2,193 single-thread CPU seconds at zstd -19
  (on a shared, loaded machine), about 10 minutes on a 4-core hosted runner; the mass rebuild's took 8,262 s
  (about 35 minutes on 4 cores). The pipeline needs the previous
  release's NARs (already release assets of rime-cache, D6) and publishes one more file per path (`<hash>.delta`,
  228 MB for this pair).
- **Decoder window.** 155 of the 161 weekly deltas fit zstd's default 128 MiB window; 6 need 256-512 MiB (firefox,
  mesa, llvm, the modules). In the mass rebuild 2052 of 2067 fit, 12 need 256-512 MiB and linux-firmware needs
  1 GiB. The client must pass `--long=<w>` (`ZSTD_d_windowLogMax`) or the decode fails, and the window is resident
  memory (peak RSS 788 MiB and 1,640 MiB, §5).

### 6.1 Uncompressed kernel modules (`MODULE_COMPRESS` unset): measured, not shipped

Rime's 7.2.9 kernel has no `MODULE_COMPRESS_NONE` symbol: "none" is `MODULE_COMPRESS` unset (`"-d MODULE_COMPRESS"`
in `nix/pkgs/kernel/config.nix`, which also drops `_ZSTD`, `_ALL` and `MODULE_DECOMPRESS`). Built that way in the
`rime-nix` container (65 min, `checks.kernel-gate` passed; `artifacts/deltas/kernel-build-2.log`), then measured:

| | NAR | whole at zstd -3 | whole at zstd -19 | whole at -19 --long=27 | weekly delta |
|---|---|---|---|---|---|
| Rime 7.2.9 modules, `.ko.zst` (as today) | 175.1 MB | 166.5 MB | 165.4 MB | 165.3 MB | |
| Rime 7.2.9 modules, plain `.ko` | 668.5 MB | 138.4 MB | **106.1 MB (-36 %)** | 103.7 MB | |
| weekly pair `linux-6.18.59-modules`, `.ko.xz` | 144.3 MB | 134.4 MB | | 133.8 MB | 92.0 MB |
| same, every module decompressed | 703.1 MB | 146.6 MB | | 109.4 MB | **19.3 MB (-79 %)** |

(`nix/release/delta/lab/modules-rime.sh`, `artifacts/deltas/modules-rime.log`; `modules-uncompressed.sh`,
`artifacts/deltas/modules-uncompressed.log`. A delta of Rime's own kernel needs two Rime kernel versions, so the
weekly pair's tree stands in for it; its plain NAR needs a 1 GiB window, `wlog` 30.) Plain modules ship 36 % fewer
bytes whole and turn the modules' weekly delta from 92 MB into 19 MB: the single biggest saving left on a weekly
release. The disk cost on `compress-force=zstd:3` is about the same as the compressed files'.

**Not shipped, because the module signatures do not survive.** `checks.kernel-vm` failed
(`artifacts/deltas/kernel-vm.log`): "ntsync loaded UNSIGNED/unverified (taint 'E')". The kernel signs every module
at `modules_install` (kmodigest certificates, D3); the compressed `.ko.zst` keeps that signature, but a plain `.ko`
is an ELF file, and nixpkgs' fixup phase strips it (`stripDebugList` covers `lib`, `stripExclude` names only the
build tree and `vmlinux`): 0 of 200 sampled plain modules still carried `~Module signature appended~`. Shipping
unsigned modules is not an option under lockdown, so the kernel files were reverted on this branch and the change is
**decided not to ship here**. To ship it: `stripExclude = [ "*.ko" ]` (or `dontStrip`) on the kernel derivation and
on every out-of-tree module package (msi-ec, nvidia-open, xone, xpadneo), then `checks.kernel-vm` must pass again;
it is one kernel rebuild (~65 min) and one VM check. Do it together with the next `kernel.pin` bump, which also gives
the first real delta of Rime's own modules.

## 7. What shipped (2026-11-02), and A3

The measurement recommended deltas for P4. They shipped on `nix/deltas` instead, with G3's VM timing (§5.2), so
this section is now what the code does; each item names the recommendation it closes.

1. **Pipeline** (`rime-nix-release deltas`, `nix/release/rime_nix_release.py`; the build job's step "Deltas against
   the previous release (github backend)" in `.github/workflows/nix-release.yml`). One delta per changed path
   against the channel's **previous release only**, verified with cosign as a client verifies it; `zstd -19
   --long=27 --patch-from`; every delta decoded again and compared with the NAR; a path whose delta is over **80 %**
   of its full download is skipped (fetched whole). `publish` binds the table into the signed manifest
   (`delta.files`: path, base, base NarHash/NarSize, file, file sha256/size, window; `delta.delta_size`) and uploads
   the `.delta` files as release assets beside the NARs. On the hosted runner: `--jobs 4` (the mass rebuild's 8,262
   CPU-s is ~35 min), a 150-minute limit, and only with 25 GB free for the bases; any failure warns and ships the
   release without deltas, so clients download whole paths. The S3 fallback backend publishes none.
2. **Client** (`rimed/rime/src/nixos/delta.rs`, wired into `rime update`). A path is rebuilt from its delta only when
   the machine holds the base with exactly the manifest's `base_nar_hash`/`base_nar_size`; the delta file is
   downloaded from the client's own `rime.update.deltas.url` and refused unless its sha256 and size are the
   manifest's; the rebuilt NAR's size and NarHash are checked while it streams; Nix imports it from a one-path file
   cache whose narinfo is the signed one, so the signature and NarHash are checked again. **Any refusal** (no base,
   wrong base, tampered or truncated delta, window over the cap, decode or import failure) fetches that path whole and
   says why (`rime: delta refused, downloaded whole instead: <path>: <reason>`). `rime update --no-deltas` turns it
   off for one run, `rime.update.deltas.enable = false` for good.
3. **Client speed.** The rebuilt NAR is never written to disk: `zstd -d` feeds a FIFO that `nix copy` reads (the
   client holds the FIFO read-write, because Nix opens a `file://` NAR twice: once to try a memory map, then to read
   it). Paths run in parallel, references first, `jobs` = half the CPUs (1-6), within a memory budget of half of
   MemAvailable counted in decoder windows; whole paths are batched into one `nix copy`. Measured in §5.2.
4. **Memory.** The decoder window cap is `wlog` 29 (512 MiB), 30 (1 GiB) on a machine with 8 GiB of memory or
   more (linux-firmware's base needs 1 GiB and its delta saves ~780 MB); `rime.update.deltas.windowLogMax` sets it.
   A delta over the cap is not downloaded (`rime: delta not tried: ... decoder window ...`).
5. **Kernel: not shipped.** Uncompressed modules were built and measured (-36 % whole, the modules' weekly delta
   92 → 19 MB), but nixpkgs' strip removes their signatures and `checks.kernel-vm` refuses them (§6.1). Reverted;
   the fix and its cost are in §6.1.
6. **The cache's own level.** `stage` writes Rime's NARs at zstd level 19 (§8.1).

**Checks.** `checks.x86_64-linux.update-delta` (`nix/tests/update-delta.nix`): a server VM runs the real release
pipeline (stage, deltas, publish to a fake GitHub with a TEST cosign identity) for release A and then B with deltas
against A; a client VM on A runs `rime update --download-only` five times: deltas used (B's payload rebuilt from its
delta, its whole NAR never fetched), a tampered delta (refused for its hash, payload fetched whole), a wrong base
(the client's copy of payload-1 changed: refused, fetched whole), a window over the cap (`delta_window_log_max = 26`:
the delta is not even fetched), `--no-deltas`. Five mutants of `delta.rs` each make it fail
(`nix/release/delta/lab/update-delta-mutants.sh`, §5.2). rimed's unit tests cover the scheduler (references first,
refusal → whole, a failed whole download stops the update, the memory budget, cycles), the narinfo rewrite (the
signature lines survive, NarHash/NarSize must be the manifest's), the window cap, and the FIFO stream against real
processes (a reader that opens the NAR twice, one that quits unread, a stream that does not hash right and whose
reader must never see its end; each runs under a 60 s deadline, so a stream that never ends fails the test instead
of hanging the build). Mutants of `stream()` (verifier, 2026-11-03): dropping the final NarHash comparison fails
that test; dropping the `importer.kill()` before the pipe closes used to HANG `cargo test` (the check could only
fail by never finishing) and now fails it on the deadline.
`checks.release-unit` covers `deltas` and `validate_delta`.

**A3 (decided, not closed): keep batching non-security mass rebuilds to at most once every 4 weeks for now.** With
deltas a mass rebuild measures ~0.6 GB (§4) instead of 4.1 GB, so the size reason for batching is gone in the lab.
It stays until the release byte log (`delta` in every manifest, updates-and-release.md §3.1) shows the same on real
releases: the P4 30-day gate (30-day bytes <= bootc at the same cadence) revisits A3 with that number.

## 8. Measured since, and still not measured

### 8.1 `nix copy` at zstd level 19, without `--long`

`nix/release/delta/lab/level19.sh` (`artifacts/deltas/level19.log`), over the weekly pair's 161 added paths:

| | bytes | vs level 3 |
|---|---|---|
| NAR | 2,111.2 MB | |
| zstd -3 (what `nix copy ?compression=zstd` wrote) | 745.9 MB | |
| **zstd -19** (`?compression=zstd&compression-level=19`) | **637.6 MB** | **-14.5 %** |
| zstd -19 --long=27 | 589.3 MB | -21.0 % |

`nix copy ?compression=zstd&compression-level=19` writes what the CLI does: 133,809,352 B for
`linux-6.18.59-modules` against `zstd -19 -T1`'s 133,780,419 B (0.02 %). A level-19 NAR decodes with any zstd's
default window (no `--long` on the client). The cost is the pipeline's CPU: the openvino closure (11 paths) took
1.0 s at level 3 (64.9 MB), 87 s / 96 CPU-s at level 19 (51.8 MB, -20.2 %), 61 s / 172 CPU-s with
`parallel-compression=true` (same bytes; `artifacts/deltas/level19-nixcopy-timing.log`). On the weekly pair the
pipeline's parallel level 19 wrote 641.5 MB for 156 of the 161 paths (§5.2), ~1 % more than the CLI's 637.6 MB for
all 161 (not taken apart; parallel compression's chunks are the likely cost). In the update-delta VM check, `stage`
of a ~660-path client closure went from 7-19 s to 185-480 s (4 vCPUs, katana loaded), and `checks.update-e2e`
(three releases staged) from 212 s to 1,078 s of test script, ~302 s per stage, inside its 2-hour limit
(`artifacts/deltas/update-e2e-level19.log`; `release-github` stages a tiny closure, 0.1 s). **Shipped**:
`stage` now uses level 19 with parallel compression (`STAGE_LEVEL`, pinned by `test_stage_writes_level_19`).
The remaining 7 % of `-19 --long=27` is out of reach of `nix copy` (it has no long-distance or window setting);
it would need the pipeline to compress the NARs itself, which is not done.

### 8.2 Still not measured

- A real network: §5.2's VM runs use a virtual link (unshaped, and shaped to 100 Mbit/s with `tc`), not the internet.
- The installed `plain` shape (Rime's CachyOS kernel + NVIDIA 615): its kernel and NVIDIA paths change on every
  kernel bump; by §6 they behave like the g1-nixpkgs kernel paths.
- A delta of Rime's own kernel with plain modules: it needs two Rime kernel versions and the signature fix of §6.1,
  so it is measured at the next `kernel.pin` bump. The weekly pair's proxy (§6.1) stands in for it.
- A delta chain (two releases behind: those machines fetch whole paths), `--patch-from` at other levels, and
  bsdiff/xdelta-style per-file deltas.

## 9. Reproduce

```
# inside the rime-nix container, from a worktree of this branch
nix build path:.#checks.x86_64-linux.delta -L                       # the tool on fixtures
nix build path:.#checks.x86_64-linux.delta.mutants.<name> -L        # each must FAIL (6 mutants, nix/tests/delta.nix)

# a pair's old side, against an older nixpkgs, into its own chroot store
nix build --store /work/scratch/g2/store-<rev7> --impure --print-out-paths \
  --expr 'import ./nix/release/delta/pair.nix { rimeFlake = "path:/work/<worktree>"; nixpkgsRev = "<rev>"; }'

# one pair: closures, pairing, every delta (kept for the client), summary
nix shell <nixpkgs>#python3 <nixpkgs>#zstd -c \
  nix/release/delta/measure-pair.sh OUTDIR OLD_STORE OLD_TOPLEVEL NEW_STORE NEW_TOPLEVEL 6

# the shipped client and pipeline in VMs (§7 Checks), its five mutants (each must FAIL), and G3 on the weekly pair (§5.2)
nix build path:.#checks.x86_64-linux.update-delta -L
nix/release/delta/lab/update-delta-mutants.sh /work/artifacts/deltas/mutants
nix/release/delta/lab/g3-vm.sh /work/artifacts/deltas/g3-vm.log [OLD_TOPLEVEL NEW_TOPLEVEL]

# level 19 (§8.1) and the kernel modules (§6.1)
nix/release/delta/lab/level19.sh OUT.tsv
nix/release/delta/lab/modules-uncompressed.sh OLD_MODULES NEW_MODULES WORKDIR
nix/release/delta/lab/modules-rime.sh MODULES_OUT WORKDIR    # needs a kernel built with "-d MODULE_COMPRESS"
```
