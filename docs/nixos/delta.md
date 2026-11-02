# Delta transport (G2): `zstd --patch-from` against the previous release

Status: **measured** on 2026-11-02 in the `rime-nix` container on katana (Nix 2.35.4, zstd 1.5.7). Every number
below comes from a command in this file; the tool is `nix/release/delta/rime_delta.py`, the raw per-path data is in
`nix/release/delta/data/<pair>/`, and `checks.x86_64-linux.delta` proves the tool on fixtures. GB is 10^9 bytes.

This closes the P2 rows **G1**, **G2** and **G3** of `docs/nixos/p2-gates.md` (PLAN D6 "G2 delta transport", §7.2
Q4, decision A3: "G2 delta transport is measured before P4").

## 1. Results in one place

<!-- RESULTS -->

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
(`nix build --store /work/scratch/g2/store-<rev>`, `scratch/g2/buildold.sh`) so the shared store was not filled.

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

<!-- G2 -->

## 4. G2: a mass rebuild

<!-- G2 -->

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
output), measuring (every delta re-decoded), `apply` into a chroot store (`diff -r` against the release's path,
`nix store verify` with the trusted key), five refusals (a bit-flipped delta, a truncated delta, a narinfo whose
NarHash is another path's, the delta applied to the wrong base, a narinfo signed by an untrusted key: each exits
non-zero, says why, and imports nothing), and `apply-all` moving a store from the old release to the new one with
two paths rebuilt from deltas (the server holds no NAR for them) and one fetched whole. Five mutants each make the
check fail (`artifacts/g2/delta-check-mutants.log`): `no-narhash-check`, `pair-ignores-output`, `nix32-reversed`,
`keep-nothing`, `apply-all-no-delta`.

**Benchmark** (`nix/release/delta/lab/g3.sh`, `g3-baseline.sh`; logs in `data/g1-nixpkgs/g3-*.log`): the machine is
the chroot store holding the g1-nixpkgs old release (`3ec6363`); the server is a file binary cache of the new
release's whole closure written by `nix copy ?compression=zstd` and signed with a throwaway TEST key; the deltas are
the ones `measure --keep` wrote. Everything is local disk, so the numbers are the client's own cost with the network
taken out. katana was shared with other agents' VM tests the whole time (load average 34-58 on 20 threads).

| g1-nixpkgs, 161 paths, 2.11 GB NAR | downloaded | wall | CPU (user + sys) | peak RSS | disk read / written |
|---|---|---|---|---|---|
| today: `nix copy` from the release cache | 746.2 MB (sum of the narinfos' `FileSize`) | **21.0 s** | 18.6 s | 489 MiB | 0.8 / 2.1 GB |
| deltas, run 1 (load 34-37) | **≈231 MB** (228.4 MB of deltas + 4 new packages whole, 9.2 MB NAR, ≈2.5 MB compressed) | **73.5 s** | not kept | **788 MiB** | not kept |
| deltas, run 2 (load 55-58) | same | 455.8 s (12 % CPU: waiting on disk) | 57.1 s | 786 MiB | 4.8 / 6.6 GB |

Both delta runs end with the new toplevel valid in the machine store and `nix store verify` (contents and TEST
signature) clean on all 161 added paths. The `FileSize` sum of what `nix copy` wrote (746,244,317 B) agrees with
the measured `zstd -3` column (745,915,063 B) to 0.04 %, which confirms the "shipped" baseline of §2.1.

What the client pays, measured:

- **3x the CPU and 3x the disk traffic** of a plain `nix copy`: every delta path dumps its base NAR, writes the
  rebuilt NAR, hashes it, and `nix copy` reads it back. All of it is sequential in the prototype (one path at a
  time); a real implementation would stream the rebuilt NAR into the import and run paths in parallel.
- **Memory: the decoder window.** The window is the base NAR's size, up to 2^29 (512 MiB, firefox-unwrapped's and
  mesa's NARs) in this pair; peak RSS 788 MiB. That is fine on 8 GB+ machines and needs a cap
  (`--memory=`/skip the delta above N MiB) on small ones.
- **Break-even:** deltas saved 515 MB and cost 52-435 s of extra local work. They pay off below ~10 MB/s (~80 Mbit/s)
  of download bandwidth on an idle disk (run 1), and only below ~1.2 MB/s on a disk as busy as run 2's. A home or
  school connection is usually under the first figure; the cost moves to the user's disk and CPU.


## 6. What deltas cannot shrink

<!-- LIMITS -->

## 7. Recommendation (A3, P4)

<!-- REC -->

## 8. Not measured

<!-- NOT -->

## 9. Reproduce

```
# inside the rime-nix container, from a worktree of this branch
nix build path:.#checks.x86_64-linux.delta -L                       # the tool on fixtures
nix build path:.#checks.x86_64-linux.delta.mutants.<name> -L        # each must FAIL (5 mutants, nix/tests/delta.nix)

# a pair's old side, against an older nixpkgs, into its own chroot store
nix build --store /work/scratch/g2/store-<rev7> --impure --print-out-paths \
  --expr 'import ./nix/release/delta/pair.nix { rimeFlake = "path:/work/<worktree>"; nixpkgsRev = "<rev>"; }'

# one pair: closures, pairing, every delta (kept for the client), summary
nix shell <nixpkgs>#python3 <nixpkgs>#zstd -c \
  nix/release/delta/measure-pair.sh OUTDIR OLD_STORE OLD_TOPLEVEL NEW_STORE NEW_TOPLEVEL 6
```
