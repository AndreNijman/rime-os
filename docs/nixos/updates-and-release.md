# Updates and release on NixOS (design)

Status: **design, not built.** This is the D6 design document the NixOS pivot plan asks for before any code: the
release manifest, the binary cache, the `rime update` client, channels, batching, failure messages and the tests that
prove each part. It is written to be implemented in Rust inside the existing `rimed/` workspace, reusing the bootc
client's trust code wherever the wire format allows. A review step critiques it before the first PR.

Facts marked **(checked)** were read in this repository at `c18800d15` or measured with Nix 2.35.4 while writing.
Facts marked **(not verified)** were not, and each one has a test or a handover item below.

Contents:

1. [What this rests on](#1-what-this-rests-on)
2. [Objects and where they live](#2-objects-and-where-they-live)
3. [The release manifest](#3-the-release-manifest)
4. [The channel document](#4-the-channel-document)
5. [Signing and what `rime trust --gate` verifies](#5-signing-and-what-rime-trust---gate-verifies)
6. [The binary cache](#6-the-binary-cache)
7. [The release workflow](#7-the-release-workflow-nix-releaseyml)
8. [The client: `rime update` on NixOS](#8-the-client-rime-update-on-nixos)
9. [Boot or switch](#9-boot-or-switch)
10. [Live updates: the NixOS equivalent of the sysext engine](#10-live-updates-the-nixos-equivalent-of-the-sysext-engine)
11. [Rollback, pin, garbage collection, disk headroom](#11-rollback-pin-garbage-collection-disk-headroom)
12. [Channels and pinning](#12-channels-and-pinning)
13. [Mass rebuilds and security backports](#13-mass-rebuilds-and-security-backports)
14. [Failure modes and what the user reads](#14-failure-modes-and-what-the-user-reads)
15. [Tests](#15-tests)
16. [Code layout in `rimed/`](#16-code-layout-in-rimed)
17. [Handover and open items](#17-handover-and-open-items)

---

## 1. What this rests on

These are the decisions this document implements. They are restated here so the document stands on its own.

- **Two pipelines, one ID sequence.** bootc keeps shipping from `build-image.yml` until its sunset. NixOS releases come
  from a separate `nix-release.yml` on `main`, with its own tags, concurrency group, path filter and signer identity.
  Sharing `build-image.yml` would collide: it pushes `daily-<sha>` for the same commit, has one global concurrency
  group, and its path filter (`files/**`, `rimed/**`, `.github/**`) would turn Nix-only work into bootc releases.
  Both pipelines use `files/scripts/next-release-id`, so release IDs stay one `YYYY.MM.DD[.N]` sequence.
- **A signed release manifest.** Published as an OCI artifact in GHCR, signed with cosign keyless under the
  `nix-release.yml@refs/heads/main` identity, verified on the machine by the existing `verify.rs` path (skopeo and
  openssl against the pinned Fulcio root, no transparency log, as today). It lists every closure path with its
  narHash, and it carries the class 2 package catalogue and the rime-shell commit.
- **R2 holds the full closure and is the only source clients use.** CI copies the cache.nixos.org paths into R2 and
  uploads Rime's own builds. Machines never contact cache.nixos.org. The narinfo signing key and the R2 write token
  live only in GitHub-hosted job secrets. The self-hosted build host builds, and never holds a key or a bucket
  credential. Until an R2 bucket and token exist, everything is built and tested against a minio stand-in.
- **The rime-shell input moves automatically.** The release job resolves rime-shell `main` at build time and records
  the commit in the manifest, as `build-image.yml` does for the bootc image.
- **Mass rebuilds are batched; security fixes are not.** A whole-system rebuild (a `stdenv` change, about 4 GB to
  download) ships at most once every 4 weeks unless it carries a security fix. Security fixes ship as backport
  overrides within the response times (critical within 72 hours, high within 7 days). When no backport is possible,
  the mass rebuild ships early.
- **The boot/switch rule.** `boot` whenever the kernel, a kernel module, NVIDIA userspace or the initrd changes (a live
  switch of NVIDIA userspace under a loaded module reproduces the `NVRM: API mismatch` crash fixed for bootc in
  rime-os #113). `switch` otherwise, and never in a way that restarts greetd or the user's session.
- **v1 boot chain.** Fedora's shim, Fedora's GRUB, a MOK-signed kernel. Menu entries are written by a Rime
  `boot.loader.external` hook that never runs `bootctl`, `grub-install` or `efibootmgr`. There is no automatic
  rollback in v1: the user picks an older generation in the GRUB menu.
- **Update gates for dogfooding (P4):**
  - bytes per machine over a rolling 30 days are at most the bootc figure at the same cadence;
  - security fixes ship within the response times, whatever the batch schedule;
  - a shell-only update downloads 50 MB or less;
  - a median update takes no longer, wall-clock, than the bootc update of the same class on the same link (download
    plus apply, to "ready to restart" or "applied live");
  - a shell-only release applies without a restart, with the session-aware lock and the Gaming Mode rules from
    rime-os #109 intact.
- **The end-to-end gates for D6:** a VM update refuses a tampered narHash, an unsigned manifest and a wrong identity;
  rollback works after a refused update; the live shell update above.

Measured numbers this design has to live with (Rime-like closure on nixos-unstable `1c89083`): whole closure 1,606
paths, 4.60 GB download; an update with no toolchain rebuild 0.80 GB; a whole-system rebuild 4.02 GB; a one-line
rime-shell change 12 paths, 0.012 GB.

---

## 2. Objects and where they live

| Object | Where | Mutable? | Signed by | Read by |
|---|---|---|---|---|
| Release manifest | GHCR `ghcr.io/andrenijman/rime-os:nix-<os-sha>-<shell-sha>` | never moved once pushed | `nix-release.yml@refs/heads/main` | the client, the installer, the bridge, rimeos.com |
| Channel document | GHCR `ghcr.io/andrenijman/rime-os:nix-channels` | yes, every release, promotion, halt and daily refresh | `nix-release.yml@refs/heads/main` | the client |
| Store paths (NARs + narinfos) | R2 bucket served at `https://cache.rimeos.com` | append-only (see GC in §6.6) | narinfo key `rime-cache-1` | Nix on the client |
| Closure SBOM attestation | GHCR `.att` for the manifest digest | no | `nix-release.yml` | the client (`provenance=warn`) |

**Tag namespace.** Every Nix object's tag starts with `nix-`. bootc never writes a `nix-` tag and the Nix pipeline never
writes anything else. `build-image.yml`'s tag promotion moves a fixed list of tags (`FLAVORS`, the `platform-*` tags,
`rechunk-plan`, `edge`), so the two cannot collide. Both live in the existing `rime-os` package, so no new package
permissions are needed and rimeos.com's sync reads one repository.

**Why `nix-<os-sha>-<shell-sha>` and not `nix-<sha>`.** The plan names the namespace `nix-<sha>`. Because the release
job moves the rime-shell input itself (A15), the rime-os commit alone does not determine a toplevel: the same rime-os
commit with a newer rime-shell is a different release. The nixpkgs pin is in rime-os's `flake.lock`, so the pair
(rime-os commit, rime-shell commit) determines the toplevel, provided every other build input is in rime-os too. The
one candidate that might not be is the kernel's detached MOK signature (D7): it must be committed to rime-os, as
`kernel/kernel.pin` is, and not fed to the build as a workflow artifact, or the pair stops identifying a release.
Both are full 40-character hashes; the tag is 85
characters, under the registry's 128. A tag that already exists is never overwritten: if the pair was released before,
the job publishes nothing new (the channel document may still be refreshed). This is the Nix form of
`next-release-id`'s "the same OS revision and the same Shell revision is not a new release" rule.

**Release IDs.** `next-release-id` keeps its rules and arguments. Its `published-*` inputs become the newest of two
candidates: bootc's `:daily` labels and the release named by the Nix channel document's `edge` entry. The publish job
reads both again immediately before it pushes, and bumps `.N` if bootc published the same ID in the meantime. The two
pipelines do not share a concurrency group, so this re-read is the only guard; the rimeos.com chain test catches a
duplicate if it slips through (residual risk, §17).

---

## 3. The release manifest

### 3.1 Format

One JSON document, UTF-8, keys sorted, no insignificant whitespace, so a rebuild of the same inputs produces the same
bytes. Media type `application/vnd.rimeos.nix-release.v1+json`.

```json
{
  "schema": 1,
  "kind": "rime-nixos-release",
  "release": "2026.12.03",
  "issued": 1796260000,
  "previous": "2026.11.26",
  "system": "x86_64-linux",
  "store_dir": "/nix/store",
  "source": {
    "rime_os": "<40 hex>",
    "rime_shell": "<40 hex>",
    "nixpkgs": "<40 hex>"
  },
  "cache": {
    "url_hint": "https://cache.rimeos.com",
    "keys": ["rime-cache-1:<base64 ed25519 public key>"]
  },
  "variants": {
    "plain": {
      "toplevel": 812,
      "boot": { "kernel": 640, "initrd": 641, "kernel_modules": 642, "kernel_params": ["…"] },
      "components": { "kernel": [640, 642], "nvidia": [700, 701], "shell": [903], "rime-daemon": [904] }
    },
    "luks": { "toplevel": 813, "boot": { "…": "…" }, "components": { "…": "…" } }
  },
  "catalogue": [
    { "name": "helium", "version": "0.9.1", "out": 455, "class": 2 }
  ],
  "paths": [
    { "path": "/nix/store/<hash>-<name>", "nar_hash": "sha256-<base64>", "nar_size": 279592,
      "file_size": 76019, "references": [12, 40] }
  ],
  "batch": { "mass_rebuild": false, "nixpkgs_moved": true, "security": ["CVE-2026-0000"] },
  "delta": { "from": "2026.11.26", "variant": "plain", "paths": 112, "file_size": 860000000 }
}
```

- **`paths`** is one table for every variant and the catalogue, sorted by path. Every other field names a path by its
  index into this table (`toplevel`, `boot.*`, `components.*`, `catalogue[].out`, `references`). Indices keep the
  document small (about 1,700 entries at roughly 150 bytes each, a few hundred KB) and make closure checks plain
  integer work.
- **`nar_hash`** is the SRI form Nix 2.35 prints (`sha256-<base64>`, **(checked)**: `nix path-info --json --json-format 1
  --store https://cache.nixos.org` prints exactly this). The client also accepts the older `sha256:<nix32>` form when
  it reads Nix's own output and normalises both before comparing.
- **`file_size`** is the compressed NAR size in R2 (narinfo `FileSize`, `downloadSize` in Nix's JSON). It is what a
  machine downloads, so the client can say how big an update is from the manifest alone.
- **`variants`.** The installer ships two generic toplevels, `plain` and `luks`; everything per-machine comes from the
  host layer (`/var/lib/rime/host.toml`). The machine picks its variant from `host.toml`'s `variant` key. A variant
  missing from the manifest is a refusal, not a fallback to another variant.
- **`boot`** is copied by CI from the toplevel's bootspec (`boot.json`, `org.nixos.bootspec.v1`: kernel, initrd,
  kernelParams) plus the `kernel-modules` link. The client uses it for `--check` and `--plan`, and after realising the
  closure it re-reads the real `boot.json` and refuses if the two disagree.
- **`components`** maps the live engine's component slugs (`rimed_core::live::Component::slug()`: `shell`,
  `rime-tools`, `rime-daemon`, `rime-services`, `compositor`, `shell-runtime`, `systemd`, `libraries`, `nvidia`,
  `kernel`, `firmware-files`, `bootloader`, …) to the store paths that make up each one. CI computes it with the same
  Rust classifier the client links (`rimed-core`), so there is one rule set, not two.
- **`catalogue`** is the class 2 catalogue (leaf apps that `rime install` adds to the `rime-apps` profile). Class 1
  packages (steam, gamescope, gamemode, the virtualization set) are in the toplevel and are not listed here.
- **`batch`** says what kind of release this is, for the user-facing size message (§8.4) and the release page.
- **`delta`** is the download a machine on `previous` makes, computed by CI for the `plain` variant. It feeds the
  release byte log (the P4 30-day gate) and the release page. It is informational; the client computes its own.
- **`cache.url_hint`** is informational. The client reads its cache URL from its own configuration, never from the
  manifest (§8.1).
- **`cache.keys`** are the narinfo public keys valid for this release (§6.4).

### 3.2 Validation (before anything is downloaded)

`rimed_core::nixrel::Manifest::validate` is a pure function, tested on fixtures. A manifest is refused when any of these
fails:

- `schema` is 1 (a higher major is refused with "this release needs a newer `rime`"; unknown fields in schema 1 are
  ignored, so additions do not need a new major);
- `kind` is `rime-nixos-release`, `system` is `x86_64-linux`, `store_dir` is `/nix/store`;
- `release` parses as a release ID;
- every `path` is a syntactically valid store path under `/nix/store`, and no path appears twice;
- every index is in range;
- the closure of each variant's `toplevel`, walked through `references`, contains that variant's `boot` and
  `components` paths, and every reference is itself in the table (the table is closed). The same holds for every
  `catalogue[].out`, because the catalogue pass downloads through the same bind (§8.5);
- every `nar_hash` parses, every size is positive;
- `cache.keys` is not empty and every entry is `<name>:<base64 of 32 bytes>`. A verified manifest's keys are the only
  narinfo keys trusted for its download (§6.4), so an empty list would make every download fail late instead of here.

### 3.3 Publication

The release job assembles an OCI image manifest with one layer by hand, exactly as `promote-channel.yml` does for the
rollout document **(checked)**: an empty config (`application/vnd.oci.empty.v1+json`), `artifactType` and the layer
media type set to `application/vnd.rimeos.nix-release.v1+json`, written to a `dir:` layout and pushed with
`skopeo copy dir:… docker://…`. The manifest also carries annotations rimeos.com's sync reads:
`org.rimeos.release.id`, `org.opencontainers.image.revision` (rime-os), `org.rimeos.rime-shell.ref` and
`org.rimeos.product=nixos`. Then `cosign sign --yes "$IMAGE@$digest"`.

Order is fixed: every store path is in R2 and verified from the public URL (§7) **before** the manifest is pushed, and
the manifest is signed **before** the channel document names it. A machine can therefore never see a release whose
paths are not downloadable.

---

## 4. The channel document

The channel document says which release each channel offers, to what share of machines, and whether it is halted. It
is the existing rollout document, reused:

- same struct, `rimed_core::channel::RolloutDoc` (`schema`, `serial`, `issued`, `expires`, `repository`, `channels[]`),
  with one optional field added to `RolloutEntry`: `release` (the release ID, for display). serde's defaults keep it
  compatible.
- each entry's `digest` is the **release manifest's digest**, not an image digest;
- same decision function, `rimed_core::channel::decide_rollout`: FNV-1a bucket of `/etc/machine-id`, `percent`,
  `halt`, `expires`, `MAX_DOCUMENT_AGE` (30 days whatever the document says), `MAX_CLOCK_SKEW` (1 hour), and the
  replay rule (a lower `serial` than this machine has accepted is ignored);
- different tag (`nix-channels`), media type (`application/vnd.rimeos.nix-channels.v1+json`) and signer
  (`nix-release.yml`). A bootc rollout document served under the Nix tag is refused by media type and by signer.
- **its own accepted-serial file**, `/var/lib/rime/nix/channels.json` (same shape as `CachedRollout`), never bootc's
  `/var/lib/rime/channel/rollout.json`. The two documents have independent serials; sharing the file would make the
  first Nix document after a bridge look like a replay of a higher bootc serial. `rime channel status` and
  `rime channel rollout --offline` read this file on NixOS. Only a document that **verified** is ever written to it:
  a document read under `--allow-unverified` is used for that one run and not cached, because an unverified document
  with a huge `serial` would otherwise make every genuine document after it a "replay" (a permanent freeze).
- **`target_digest`.** On bootc, `decide_rollout` checks the entry's digest against the resolved channel tag, which
  is what catches a stale entry. On NixOS the entry's digest *is* the target, so the client passes that digest
  through and the check is trivially true. Staleness is covered by `expires`, `MAX_DOCUMENT_AGE` and the serial
  instead.

**Doing without a fallback.** On bootc the rollout document is advisory: when it cannot be read, the update still
follows the channel tag (`rollout_hold` takes the update on any failure to measure). On NixOS the document is the
**only** pointer to a release, so the reader fails closed: no verified, unexpired document means no OS step (F2-F6),
while the Flatpak, catalogue and firmware passes still run.

**Refresh and freeze.** The document is re-signed with `serial + 1` and a 14-day `expires` on every release, every
promotion or halt, and by a **daily** scheduled refresh even when nothing was released. Daily rather than weekly
because this repository has measured GitHub dropping scheduled runs for hours at a time: with a 14-day expiry, 13
consecutive missed refreshes still leave every machine updating. The cost is the freeze window: someone who controls
what GHCR serves can keep offering an old, still-valid document for up to 14 days (never past `MAX_DOCUMENT_AGE`).
That freeze is made **visible** instead of silent: when the document's `issued` is more than 3 days old, `rime update`,
`rime update --check` and `rime trust --gate` print `rime: the newest release information this machine can get is <n>
days old.` Every publish and refresh self-check (§5.6) also fails loudly when the live document has less than 7 days
left, so a broken refresh is noticed by people before machines see F5.

**Fix forward, never promote backward.** The client never installs a release older than the one installed (§8.2
step 6), so `promote` refuses to point a channel at a release older than the one the channel offers now (it checks
the release IDs of both manifests). A bad release is withdrawn by halting the channel and publishing a new release
that reverts the change; the halt stops machines that have not taken it yet, and the new release moves the rest. This
also matches what NixOS state migrations need: a generation that ran newer code may have changed state an older one
does not understand (the same reason bootc's channel move toward `stable` warns about `/etc` and `/var`).

This answers the plan's open question about where channel manifests live: in GHCR, next to the release manifests, on
the transport and verifier the client already has.

---

## 5. Signing and what `rime trust --gate` verifies

### 5.1 Identity

A new constant in `rimed/rime/src/trust.rs`:

```rust
const NIX_SIGNER: &str =
    "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main";
```

with an image-owned override file in the same style as `expected-signer` (one identity per line), set on NixOS from the
module option `rime.trust.nixSigners`. `EXPECTED_SIGNER` and `RENAMED_SIGNER` stay as they are and keep naming
`build-image.yml`. Nix objects are verified against `nix_expectations(roots)` (the `NIX_SIGNER` set and
`EXPECTED_ISSUER`); a manifest signed by `build-image.yml`, by `promote-channel.yml` or by a fork's workflow is refused.

The bootc readiness release ships `NIX_SIGNER` and the `rime-cache-1` public key one release before the bridge first
uses them, as the rename did for `RENAMED_SIGNER`.

### 5.2 Verification chain

For both objects, in this order and no other (the order `channel.rs::fetch_rollout` already uses):

1. resolve the tag to a digest (`verify::resolve`);
2. `verify::verify_signature_as(roots, repo, digest, &nix_expectations(roots))`: the `.sig` tag for that digest, the
   simple-signing payload bound to that digest and repository (`simple_signing_binds`), the certificate chained to the
   pinned Fulcio root, the SAN equal to an expected identity, the issuer equal to GitHub Actions'. The transparency
   log is not checked, as on bootc;
3. read the layer **by that digest** (`verify::artifact_layer_at`, which refuses if the tag moved between resolving and
   reading, and checks the blob against the layer digest);
4. parse, then validate (§3.2).

The release manifest is fetched by digest, because the channel document names it by digest. `verify::fetch` today
copies `docker://{repo}:{tag}`; it gains a by-digest form (`docker://{repo}@{digest}`), and `artifact_layer_at` is
reused unchanged on top of it.

### 5.3 Enforcement

The existing matrix applies unchanged: `signature=enforce`, `provenance=warn` by default, `/etc/rime/trust.conf`
overrides, `--allow-unverified` as the one-off escape, not shared with `--force`. On NixOS a manifest that does not
verify also means its narHashes are unverified. So with `--allow-unverified`, the client trusts only the narinfo keys
compiled into the system (§6.4), never keys the unverified manifest announces, and it never writes an unverified
channel document's serial (§4).

**What the signature does and does not give.** A cosign simple-signing signature carries no trustworthy time (the
transparency log is not checked, and verify.rs verifies at the leaf's own `notBefore`, its `-attime` rule). Every
release manifest Rime ever signed therefore verifies forever. Freshness comes only from the channel document's
`issued`, `expires` and `serial`, and protection from an old-but-valid manifest comes only from the channel document
naming it and from the downgrade check (§8.2 step 6). Any job in `nix-release.yml` on `main` that holds
`id-token: write` can produce a signature the client accepts; the Fulcio certificate does not name the job or the
environment. That is why every such job is treated as a signer in §7.

`verify::refusal` hardcodes "the image it would deploy" and "a cosign signature for every image". It takes the noun as a
parameter instead of being copied: "the release it would install" / "every release". The sentence "could not be
verified — which is not the same as failing to verify" stays.

### 5.4 Provenance

The `.att` attestation is an SPDX SBOM of the closure, attested with `cosign attest` over the manifest digest. SPDX is
chosen so `intoto_binds` (which requires an `spdx` predicate type) is reused unchanged. The generator (sbomnix or
another) is chosen in the SBOM PR before public release; until then `.att` is absent and the client prints the same
warning bootc machines print today ("provenance could not be established").

### 5.5 What `rime trust` says on NixOS

- `rime trust --gate` answers for **the release this machine would install next**: fetch the channel document, decide
  the target, fetch the manifest, verify both, print the same block bootc prints (signature verified, "the
  transparency log was not checked", provenance, enforcement), and answer `yes` or `no`. It does not download store
  paths, and like `rime update --check` it writes nothing (not even the accepted serial).
- `rime trust` (no gate) answers for **what is running**. At install time the client saves the manifest, the signed
  payload, the certificate and the signature under `/var/lib/rime/nix/releases/<release>/`. The report re-verifies that
  saved signature offline with openssl against the pinned root, and checks that `/run/current-system` and
  `/run/booted-system` are toplevels the saved manifest lists. No network is needed, so the readout works without a
  password and without a connection, like `rime channel status`.

### 5.6 cosign version

Plan open question 12 asks whether cosign v3's bundle format still writes the `.sig` tag `verify_signature_as` reads
(not verified). The workflow therefore pins cosign v2.5.2 through `sigstore/cosign-installer` (`cosign-release:`), and
every publish runs a **self-check** after signing: the `rime-nix-release verify` tool (§16), built from the same commit,
verifies the just-pushed object with the client's own code. If a cosign upgrade ever changes the format, the release
fails before any machine sees it, instead of every machine refusing it.

---

## 6. The binary cache

### 6.1 Layout

A standard Nix HTTP binary cache in one R2 bucket:

- `nix-cache-info`: `StoreDir: /nix/store`, `WantMassQuery: 1`, `Priority: 30`;
- `<hash>.narinfo` for every path of every published release;
- `nar/<filehash>.nar.zst`, zstd-compressed (the compression level is chosen by measurement in the fill PR; the size
  probe used zstd -19).

Clients read it anonymously over HTTPS at `https://cache.rimeos.com`, a custom domain on the existing rimeos.com
Cloudflare zone (no DNS record exists today; handover). r2.dev URLs are not used. Writes go through R2's S3 API
(`s3://rime-cache?endpoint=<account>.r2.cloudflarestorage.com&region=auto`) from the hosted publish job only.

**Edge caching.** A custom domain puts the bucket behind Cloudflare's edge cache. `nar/*` objects are named by their
content hash and may be cached for as long as the edge likes. `*.narinfo` and `nix-cache-info` must **not** be edge
cached (a cache rule: bypass, or a TTL of at most 60 s): narinfos are rewritten when `nix store sign` adds a signature
and during key rotation, and a stale narinfo at the edge would both keep a dropped key's signatures in front of
clients and make the publish job's "verify from the public side" check (§6.3) read old data and pass. The rule is part
of the handover item, and `fill-and-publish` (§15.2) re-signs a narinfo and checks the next read sees it.

R2 holds **the full closure**: cache.nixos.org paths and Rime's own builds alike. Reasons: clients depend on one host
Rime controls; nothing depends on cache.nixos.org's retention; and a later delta transport (G2, §17) can cover every
path, not just Rime's.

### 6.2 What clients are allowed to do

The NixOS module sets, for the system Nix:

```nix
nix.settings = {
  substituters = lib.mkForce [ "https://cache.rimeos.com" ];
  trusted-public-keys = lib.mkForce [ "rime-cache-1:<base64>" ];
  require-sigs = true;
  fallback = false;          # a missing path is a refusal, never a local build
  max-jobs = 0;              # nothing is built on a user machine
  builders = "";
  trusted-users = [ "root" ];
  min-free = 0;              # no automatic GC in the middle of an update download (§8.5)
};
nix.gc.automatic = false;    # rime owns GC (§11)
nix.optimise.automatic = false;
```

`fallback = false`, `max-jobs = 0` and `builders = ""` together mean a path missing from the cache can never become a
45-minute kernel or NVIDIA build on a laptop. `rime update` passes these settings again on every Nix call it makes
(`--option …`), so a hand-edited `nix.conf` cannot change what the update path does. It runs as root against the
local store directly (`--store local`), not through `nix-daemon`, so the `--option` values are the ones enforced and
not the daemon's `nix.conf`.

### 6.3 How CI fills it

1. **Build (self-hosted, isolated, no secrets).** The build job builds both variant toplevels and the catalogue, runs
   the flake checks including the update VM tests (§15), and writes `closure.json`: every path with narHash and
   references. For each path it asks cache.nixos.org whether the path is there (`nix path-info --store
   https://cache.nixos.org`). Paths that are not (Rime's own builds: the kernel and modules, NVIDIA, steam-unwrapped,
   rimed, rime-shell, the Hyprland rebuild, about 0.71 GB compressed in the probe) are exported with `nix copy --to
   file://$out?compression=zstd` and handed to the publish job as a workflow artifact. The build job holds only
   `GITHUB_TOKEN` with `contents: read`; no `id-token`, no environment secrets.
   - **Substitute, never rebuild, what is already published.** The build host's substituters are
     `https://cache.rimeos.com` (trusting `rime-cache-*`) first, then cache.nixos.org. R2 is append-only, so a path it
     already holds has one narHash forever; if the build host rebuilt it non-reproducibly instead, `closure.json`
     would carry a second narHash, the public-side check below would never match, and no release could publish until
     someone noticed why.
   - **Upstream paths with a different local narHash.** A path cache.nixos.org has, but which the build host built
     itself first (and non-reproducibly), is treated as a Rime path: exported from the build host and uploaded, so
     `closure.json`, R2 and the manifest agree.
   - **The kernel's MOK signature** is not made here. D7 signs the kernel image in a separate hosted job holding the
     `APEX_SB_*` secrets (`sbsign` detached, `sbattach` inside a derivation); the build job consumes that detached
     signature as an input. Those secrets are not in the `nix-release` environment, and `nix-release.yml` never names
     them.
2. **Publish (GitHub-hosted, environment `nix-release`, `main` only).**
   - Import the artifact into a local file cache and check every Rime-built path's narHash against `closure.json`, so
     an artifact changed in transit is refused.
   - The publish job **executes nothing that came from the build host.** `rime-nix-release` (manifest and self-check,
     §16) is built in the publish job from the checked-out commit (`cargo build --locked`); the artifact is read only
     as data (NARs, narinfos, `closure.json`). Every third-party action in the job is pinned by commit SHA, and Nix is
     installed from a pinned, hash-checked installer.
   - Upstream paths: `nix store verify --store https://cache.nixos.org --no-contents --sigs-needed 1
     --trusted-public-keys cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= <paths>`, then `nix copy
     --from https://cache.nixos.org --to "$R2" --option require-sigs true --option trusted-public-keys
     cache.nixos.org-1:… <paths>`, so the copy enforces the same key itself and the separate verify is not a
     check-then-use gap. Nix streams store-to-store copies, so the 9 GB of upstream
     paths do not have to fit on the runner's disk **(not verified; the fill test in §15 runs with a disk quota)**.
     Paths already in R2 are skipped by `nix copy` itself.
   - Rime's paths: `nix copy --from file://… --to "$R2"`.
   - Sign: `nix store sign --store "$R2" --key-file "$KEY" --recursive <toplevels> <catalogue outs>`. Every path in
     the bucket then carries a `rime-cache-1` signature; upstream paths keep their `cache.nixos.org-1` signature too,
     for audit. Whether `nix store sign` writes narinfos on an S3 store is **not verified**; the fallback is to sign
     in the local file cache before upload, which needs the upstream paths on local disk.
   - **Verify from the public side**, through `https://cache.rimeos.com`, not the S3 API: `nix store verify --store
     https://cache.rimeos.com --no-contents --sigs-needed 1 --trusted-public-keys <rime keys>` over the whole closure,
     and `nix path-info --json` narHash and references compared with `closure.json`. Any difference stops the release.
   - Only then: build the manifest, push, sign, self-check (§5.6), update the channel document, sign, self-check,
     dispatch the rimeos.com deploy as `build-image.yml` does.
3. **Unfree paths.** steam-unwrapped is `unfreeRedistributable` at `1c89083`; NVIDIA, ChatGPT, Claude Desktop and
   claude-code licences are not verified (plan risk R14). The flake's `allowUnfreePredicate` list is the upload
   allowlist: the publish job refuses a closure containing an unfree package that is not on a second, reviewed
   `nix/redistributable.nix` list. A package that may not be redistributed is fetched from its vendor on the device
   with a pinned hash instead (class 2 only).

### 6.4 Keys

- **Narinfo key.** `nix key generate-secret --key-name rime-cache-1`. It is generated once on a machine that is not a
  CI runner, stored as the secret `RIME_NIX_CACHE_KEY` in the GitHub environment `nix-release` (deployment branch:
  `main` only), and the local copy deleted. The public half is committed at `nix/keys/rime-cache-1.pub` and compiled
  into the NixOS module. It never reaches a self-hosted runner: the only job that names the environment runs on
  `ubuntu-24.04`, and a check in `tests/` refuses any workflow where a job with `runs-on: [self-hosted, …]` references
  the environment, the key or the R2 secrets.
- **R2 token.** An R2 API token scoped to object read/write on the one bucket, stored as `RIME_R2_ACCESS_KEY_ID`,
  `RIME_R2_SECRET_ACCESS_KEY` and `RIME_R2_ENDPOINT` in the same environment.
- **Why a leaked narinfo key alone does little harm.** The client binds every path it downloads to the narHash in the
  cosign-signed manifest (§8.5 checks the narinfos before any NAR is fetched; §8.6 checks what was actually imported).
  A forged narinfo, even with a valid `rime-cache-1` signature, does not reach a generation unless it matches a
  manifest signed by the GitHub OIDC identity of `nix-release.yml` on `main`. The narinfo signature is defence in
  depth and what Nix itself requires (`require-sigs`). What a leaked key plus control of what `cache.rimeos.com`
  serves *can* do is plant a wrong-content path in a machine's store that §8.6 then refuses: a denial of service,
  which §8.6's repair step bounds.
- **Rotation without a pre-trusted update.** The plan lists narinfo key custody as a blocker: narinfo signatures have
  no expiry or revocation, and a key change normally needs an update that is itself trusted. The manifest's
  `cache.keys` field removes that. For an update from a **verified** manifest, the client trusts **exactly the keys
  that manifest announces** and passes only those as `--option trusted-public-keys`; the compiled-in keys are not
  added. That makes revocation take effect on the update path at once: a manifest that no longer lists `rime-cache-1`
  stops every client from accepting `rime-cache-1` signatures for that download. Rotation is: generate
  `rime-cache-2`, add `-2` signatures to every path in the bucket (`nix store sign` appends), publish a release whose
  manifest announces both, then drop `-1` from later manifests; the next release also compiles `-2` into the module.
  The compiled-in keys remain what the system `nix.conf` trusts for anything else (and what `--allow-unverified`
  uses, §5.3), so a revoked key stays in a machine's `nix.conf` until it runs a generation built without it.
- **What the keys do not protect.** The self-hosted build host is trusted for **content**. The publish job checks
  Rime-built paths against `closure.json`, but `closure.json` comes from the same host, so that check only catches an
  artifact changed in transit. A compromised build host can put any kernel, Shell or daemon into a release, and Rime's
  key and Rime's OIDC identity will then sign it. Keeping keys off that host limits what a *leak* can do (no signing
  outside a `main` release run, no bucket writes); it does nothing against a *compromised builder*. The mitigations
  are D8's runner isolation (ephemeral VM per job, no host credentials) and, as a later step, a second build on the
  hosted fallback whose narHashes must match before publishing (listed in §17).

### 6.5 Cost

About 140 GB a year at $0.015 per GB-month, egress free (arithmetic from the plan, not measured): about $2 a month. A
Cloudflare billing alert at $20 a month is a handover item, not a release gate.

### 6.6 Retention

No path is deleted before the public release (P5). After that, a monthly hosted job lists every release manifest still
in GHCR (they are never deleted), computes the union of their closures for the releases of the last 12 months plus
every release any channel names, and deletes narinfos (first) and then NARs outside it. Deleting a narinfo before its
NAR means no client ever sees a narinfo whose NAR is gone.

---

## 7. The release workflow (`nix-release.yml`)

| Job | Runner | Secrets | Does |
|---|---|---|---|
| `changes` | hosted | none | path filter: `nix/**`, `flake.nix`, `flake.lock`, `rimed/**`, `files/**`, `config/**`; resolves rime-shell `main` |
| `build` | self-hosted (isolated, ephemeral, fork-PR guard), hosted fallback through a dispatcher job | `GITHUB_TOKEN` read | builds variants and catalogue, runs flake checks and VM tests, writes `closure.json`, exports Rime's paths |
| `publish` | `ubuntu-24.04`, environment `nix-release`, `if: github.ref == 'refs/heads/main'` | narinfo key, R2 token; `id-token: write`, `packages: write` | §6.3 step 2, manifest, channel document, self-checks, site dispatch |
| `refresh` | hosted, daily schedule, environment `nix-release`, `main` only | `id-token`, `packages` | re-signs the channel document (`serial + 1`, new `expires`); fails loudly when the live document has under 7 days left (§4) |
| `promote` | hosted, `workflow_dispatch` (channel, release, percent, halt, reason), environment `nix-release`, `main` only | `id-token`, `packages` | edits one channel entry, carries every other entry forward, refuses a release older than the one the channel offers (§4), re-signs |

- **Every job with `id-token: write` is a signer** (§5.3): the client cannot tell `publish`, `refresh` and `promote`
  apart. So all three run only on `main`, in the `nix-release` environment, on hosted runners, with third-party
  actions pinned by SHA. `promote`'s inputs are validated before use (channel from the fixed four, release matching
  the release-ID pattern and naming a manifest that exists and verifies, percent an integer 0-100, halt a boolean) and
  reach shell steps only through `env:`, never as `${{ inputs.* }}` inside a `run:` script, so the free-text `reason`
  cannot become a command.
- **Concurrency.** One group, `nix-release`, `cancel-in-progress: false`. It never shares `build-image.yml`'s group.
- **Fork PRs never reach a self-hosted runner.** The `build` job carries the guard today's self-hosted jobs use:
  `if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository`,
  and never `pull_request_target`.
- **Before the pivot reaches `main`.** On `feat/nixos` the workflow builds and tests but the `publish` job does not
  run. The day `nix-release.yml` lands on `main`, the same PR adds `!nix/**` and `!.github/workflows/nix-*.yml` to
  `build-image.yml`'s path filter, so adding the Nix pipeline does not itself publish a bootc release.
- **rimeos.com.** The publish job dispatches rime-website's `deploy.yml` with `RIMEOS_SITE_DISPATCH_TOKEN` and waits
  for the page, as `build-image.yml:2601-2651` does (the step "Publish the release page on rimeos.com", `continue-on-error`, so a
  site failure never fails a release). sync-releases reads `nix-*` manifests by their annotations and
  writes a `product` field (rime-website change, out of scope here). A PR touching only Nix code puts its release
  note on NixOS release pages only.

---

## 8. The client: `rime update` on NixOS

### 8.1 Which backend

`rime update` keeps one binary and one CLI. `ops::update` asks `platform::detect()` once: NixOS when `/etc/NIXOS`
exists and `/run/current-system` is a symlink into `/nix/store`, bootc when `bootc status` answers; neither is a named
error. Every path the NixOS backend runs (`nix`, `nix-env`, `nix-store`, `skopeo`, `openssl`, `fwupdmgr`, `flatpak`)
is a fixed store path from the build-time path module (plan PR 8), never a PATH lookup. The cache URL, variant and
channel come from local configuration (`/etc/rime/update.toml` from the module, `/var/lib/rime/host.toml`,
`/var/lib/rime/nix/channel`), never from the manifest.

### 8.2 The order

It mirrors `ops.rs::update` (gate → download → verify → apply) so the two backends read alike:

1. **Root and lock.** `require_root("update")`. Take `/var/lib/rime/live/lock`, the lock the live engine already uses,
   so `update`, `live apply`, `live rollback`, `rollback` and `gc` never overlap. Run the live engine's `recover()` on
   any transaction left from an interrupted run. On NixOS `recover()` has one more case: a transaction that reached
   `nix-env --set` (step 12) but not `staged` means the profile may name a generation the boot menu does not. It
   re-runs `switch-to-configuration boot` for the profile's current generation (or, if that generation fails §8.6's
   check, switches the profile back and deletes it, as §8.7 does on a hook failure). A transaction still in
   `verified` (download started, profile untouched) is **not** closed by `recover()`: it stays open as resumable, so
   `rime gc` keeps protecting its finished paths (§11.3), and the next `rime update` for the same target manifest
   adopts it instead of opening a second one. A different target closes it.
2. **Health stop.** `channel::halt_reason()`, unchanged except that the update record (`LastUpdate`) gains `toplevel`
   and `kind` (`boot` or `switch`) fields and `rebooted_into_new` compares toplevels (§11.4). `--force` skips it, as
   today.
3. **Channel document.** Fetch and verify `nix-channels` (§5.2). Run `decide_rollout`. Expired or missing: fetch once
   more before refusing (the plan's rule). Halted, or outside the ramp: print the hold, skip the OS step, and still run
   the Flatpak, catalogue and firmware passes, exactly as `rollout_hold` does today. `--force` skips the ramp and the
   halt, never the signature.
4. **Manifest.** Fetch by the digest the channel names, verify, validate (§3.2). This is the **trust gate**: a refusal
   prints `verify::refusal`'s text and exits 1 before anything is written.
5. **Up to date?** If the variant's toplevel is the system profile's current generation **and** the boot hook reports
   that generation as the menu default (the hook's `--check` mode, D3), print `rime: the OS is up to date (<release>)`
   (or `… is installed and applies at the next restart` when it is not yet booted) and go to the passes. A profile
   that names the toplevel while the menu does not (an interrupted step 12) is not up to date: step 12 runs again.
6. **Downgrade check.** If the manifest's release sorts before the **newest release this machine has installed**
   (the highest release in `applied.json`, not only the current generation's, so a `rime rollback` does not lower the
   bar), print F11 and skip the OS step. Release IDs compare field by field as integers (`2026.12.03.10` is newer than
   `2026.12.03.9`). The one exception is a move to a slower channel: `rime channel set` records a one-shot consent
   (`/var/lib/rime/nix/channel-move.json`: the target channel and the release it was offered) after its own warning,
   as `channel::direction` does today, and the next update consumes it only for that channel. Installing under that
   consent **resets the high-water mark** to the release it installs, so a machine that moved from `edge` to `stable`
   then takes stable's next releases instead of refusing each one until stable overtakes edge.
7. **Live transaction.** The live transaction is created (`discovered → verified`). The health record is **not**
   written here (see step 12).
8. **Plan** (§8.4): which paths are missing, how many bytes, whether this is `boot` or `switch`, what activates live.
   `--plan` stops here with nothing downloaded beyond the two documents and nothing written except the transaction,
   which it closes. (On bootc `--plan` downloads; on NixOS the manifest has the facts, so it does not need to.)
9. **Headroom** (§11.5). Not enough space is a named refusal before downloading.
10. **Bind, then download** (§8.5).
11. **Verify what arrived** (§8.6).
12. **Install for the next boot.** First `record_update` writes the health record (from = the running toplevel, as
    bootc's `from_digest` is the booted digest; plus the new `toplevel` and `kind`). On bootc it is written before the
    pull because the pull path itself can leave a machine that boots the new image; on NixOS nothing before this step
    can change what boots, so writing it here means a refusal in steps 8-11 never leaves a record of an update that
    did not happen. Then `nix-env -p /nix/var/nix/profiles/system --set <toplevel>`, then
    `<toplevel>/bin/switch-to-configuration boot`, which runs the external boot hook (§8.7). Live transaction:
    `staged`. This is the bootc equivalent of queueing the staged deployment: from here the update applies at the next
    restart whatever happens after.
13. **Live activation** (§10), when the plan allows it: `switch-to-configuration test` on the new toplevel, then the
    per-session activators. `--no-live` skips it; `--live-only` does step 13 without step 12.
14. **Save the release record** under `/var/lib/rime/nix/releases/<release>/` (manifest, signature, certificate) and
    `/var/lib/rime/nix/applied.json` (release ↔ generation ↔ toplevel).
15. **The passes** (`finish_update`, unchanged in order): catalogue (replaces the rime-pkg packages pass), Flatpak,
    firmware. None is fatal to the OS update.

### 8.3 Flags

| Flag | bootc meaning | NixOS meaning |
|---|---|---|
| `--check` | `bootc upgrade --check` | steps 3-5 and the size from the manifest; nothing downloaded beyond two small documents. Writes nothing: no root needed, and the accepted serial is not updated (a later `rime update` does that) |
| `--plan` | stage, verify, print the plan | steps 1-8; prints the plan, downloads no store paths, writes no health record |
| `--live-only` | activate live, do not queue | `test` without `nix-env --set`/`boot`; the next restart runs the old generation. On a `boot`-class update it installs nothing and says which rule (§9) needs the restart |
| `--no-live` | queue only | `boot` only |
| `--force` | skip the health stop and rollout hold | same |
| `--allow-unverified` | deploy despite the trust gate | same, and announced narinfo keys are ignored (§5.3) |
| `--keep-fsync` | keep ostree's per-object fsync | `--option fsync-store-paths true` for the download (Nix 2.35's default is `false` **(checked)**) |
| `--skip-firmware`, `--firmware-only`, `--skip-flatpak` | unchanged | unchanged |
| `--skip-packages` | skip `rime-pkg upgrade` | skip the catalogue pass |

The CLI help texts that name bootc or ostree are made backend-neutral ("Update the OS and firmware").

### 8.4 The plan and the size message

- Missing paths: the variant's closure (and the installed catalogue outs) minus what is already valid:
  `nix-store --check-validity --print-invalid <paths>`.
- Bytes: the sum of the missing paths' `file_size`; unpacked: the sum of `nar_size`.
- Live plan: the manifest's `components` compared with the components of the running toplevel (§10.2).

Printed, in the bootc client's style:

```
rime: 2026.11.26 → 2026.12.03
rime: 112 new store paths, 0.80 GB to download (2.21 GB unpacked)
  Rime Shell             can be activated now
  Kernel                 needs the new kernel (restart)
rime: this update installs for the next restart; Rime Shell is updated now.
```

A mass rebuild adds one line before the download: `rime: this release rebuilds the whole system (<reason>), so most of
it is downloaded again.` A release with security fixes adds `rime: includes security fixes: CVE-…`.

### 8.5 Bind before import, then download

1. For the missing paths, `nix path-info --store <cache> --json --json-format 1 <paths>` reads the narinfos over HTTP
   **(checked: for a binary cache this JSON carries `narHash`, `narSize`, `references`, `signatures`, `downloadSize`,
   `compression` and `url`, keyed by path)**.
2. Each narinfo is compared with the manifest: same `narHash`, `narSize`, `FileSize` (`downloadSize`) and
   `references`. Any difference refuses the update before a byte of any NAR is fetched. `FileSize` is bound too, so a
   hostile cache cannot serve a multi-gigabyte compressed blob that only fails its hash after it has been downloaded
   and unpacked. A path the cache does not have refuses it too ("the release is not fully published yet").
3. `nix copy --from <cache> --to local --no-recursive --stdin` with exactly the missing set, and `--option
   require-sigs true --option trusted-public-keys "<the verified manifest's keys>"` (the compiled-in keys only under
   `--allow-unverified`, §6.4) `--option narinfo-cache-negative-ttl 0 --option fallback false --option min-free 0`.
   Nix checks each downloaded NAR against its narinfo's hash and signature. `--no-recursive` with an explicit set means
   nothing outside the bound set enters the store. Nix sorts the set itself (`--stdin`/`--no-recursive`
   **(checked)** exist in 2.35.4).
4. Speed: Nix's own parallelism (`max-substitution-jobs` 16, `http-connections` 25 by default **(checked)**) is kept;
   the P4 wall-clock gate decides whether it needs raising.
5. A temporary GC root `/nix/var/nix/gcroots/rime/pending` is added (`nix-store --add-root … --realise <toplevel>` after
   the copy, a no-op realise) so the paths survive until the profile owns them. It is removed in step 12 or on any
   refusal.

**What the bind does and does not prove.** Step 2 reads the narinfos once and `nix copy` reads them again, so a
hostile server can answer the two reads differently. Step 2 is therefore a fast refusal for an honest mismatch, not
the guarantee; the guarantee is §8.6, which compares what was actually imported (Nix records the hash of the NAR it
unpacked) with the manifest before anything from the new closure runs. Nothing executes between the copy and §8.6.

**Interrupted downloads.** A download that is killed leaves the paths that finished, valid but not rooted (the pending
root is added only after a complete copy, and the lock dies with the process). The daily `rime gc` must not throw them
away: while `txn.json` names a transaction between `verified` and `staged`, `gc_plan` treats that transaction's target
closure (from its saved manifest) as live and keeps every path of it that is already valid (§11.3). The next
`rime update` fetches only the rest. Nothing changes the profile until step 12.

### 8.6 Verify what arrived

`nix path-info --json --json-format 1 --recursive <toplevel>` on the local store must return **exactly** the manifest's
closure for that variant: the same set and the same narHash for every path. This catches a path that was already valid
locally with different contents, and a narinfo that changed between §8.5's bind and the copy. Then the real
`boot.json` is compared with the manifest's `boot`. Any difference: refuse, remove the pending root, leave the profile
alone.

**Repair, once.** A refusal here would otherwise repeat forever: the wrong path stays valid, so the next run does not
download it again. So, after refusing, the client deletes every mismatching path that no root other than the pending
one holds (`nix-store --delete`, together with the downloaded, unrooted paths that refer to it, since Nix will not
delete a path whose referrers are valid; a path in a live generation is left alone and named in the message) and
records that it did. The next `rime update` downloads those paths again through the full bind. A second mismatch on
the same path is not repaired again: it is reported as F17 with "the update server keeps serving a different copy",
which is the security event worth a person's attention.

### 8.7 The boot hook

`switch-to-configuration boot` calls `system.build.installBootLoader`, which `boot.loader.external.installHook` sets
**(checked in nixpkgs `nixos/modules/system/boot/loader/external/external.nix`)**. Rime's hook (designed in D3, owned
by its own PR) regenerates the menu from the system profile's generations: BLS entries on the ESP for fresh installs,
a Rime-owned file sourced from GRUB's `custom.cfg` for migrated machines; kernels and initrds copied once per distinct
store path; entries for deleted generations pruned. It never runs `bootctl`, `grub-install` or `efibootmgr`.

The update client's contract with the hook:

- the hook is idempotent and rebuilds every entry from the profile, so `rime gc` and `rime rollback` can call it again
  (§11);
- it has a read-only `--check` mode that prints which generation the menu boots by default, so `rime update` can tell
  an installed update from an interrupted one (§8.2 step 5). This is a new requirement on D3's hook;
- it exits non-zero, writing nothing, when the ESP would fall below its floor;
- it writes in an order a power cut cannot break: a kernel or initrd file is written under a temporary name, synced
  and renamed before any menu entry names it, and the menu file itself is replaced the same way (write, sync, rename)
  only after every file it names is in place. An entry never points at a half-written kernel;
- when it fails, `switch-to-configuration boot` fails, and the client puts the profile back to the generation it was on
  (`nix-env -p … --switch-generation <previous>`), **deletes the failed generation** (`nix-env -p …
  --delete-generations <n>`), runs the hook again, and reports the failure. Deleting it matters: the hook rebuilds
  every entry from the profile's generations, so a failed generation left in the list would make every later hook run
  (the next update, `rime gc`, `rime rollback`) try to copy the same kernel and hit the same floor. GRUB then still
  shows exactly the entries it showed before the update. The downloaded paths stay in the store, rooted by nothing,
  until the next update reuses them or `rime gc` removes them.

---

## 9. Boot or switch

A pure function, `rimed_core::nixrel::activation(current, booted, target, machine) -> Activation`, table-tested.
Inputs: the running generation's components and boot facts (`/run/current-system`), the booted generation's boot facts
(`/run/booted-system`), the target's (from the manifest, confirmed against `boot.json` after download), the
dry-activation report, and `MachineState` (the live engine's existing struct: busy, sessions, NVIDIA present).

The update is `boot` (installs for the next restart, nothing is activated now) when **any** of these hold:

1. the kernel, `kernel_modules`, initrd or `kernel_params` of the target differ from the **booted** generation's.
   Compared with `booted`, not `current`, so that after one boot-class update a later shell-only update is also
   boot-class until the machine restarts: a live switch would otherwise put userspace for kernel B on kernel A;
2. any `nvidia` component path differs from the booted generation's (the #113 crash);
3. the dry activation (`<target>/bin/switch-to-configuration dry-activate`, run after §8.6 so only verified code runs)
   reports `would restart systemd`, or would stop, restart or reload any unit outside the live-restart allowlist. The
   allowlist starts as `RESTART_SAFE_UNITS` (`rimed.service`, `rime-remoted.service`) and grows one unit at a time,
   each with a VM test. Units that would only be **started** (new units) are allowed. Output the parser does not
   recognise is treated as a unit outside the allowlist **(checked: nixpkgs `switch-to-configuration-ng` prints
   `would stop/reload/restart/start the following units: …` and `would restart systemd`; re-check at the pinned
   revision)**;
4. the closure changes `glibc` or `linux-pam`. A switch rewrites `/etc/pam.d` to new module paths, and a long-running
   process (the Shell's lock screen) built against the old glibc would then `dlopen` PAM modules linked against the new
   one, and could fail to unlock. Restarting the Shell right away closes that window only when every session can be
   restarted, so these changes always wait for a restart;
5. `MachineState.busy` is not `Some(None)`: a game is running, Gaming Mode is held, or it could not be told. Rule from
   #109: unknown is never permission;
6. any graphical session is locked, or its lock state could not be read, **and** the switch would restart the Shell or
   change Hyprland configuration. (A switch that changes only `rime-tools` may proceed with a locked screen, as it
   does on bootc.)

When rules 5 or 6 make an otherwise live update `boot`, the components are recorded as **Deferred** with the same
reasons the bootc engine records (`busy`, `locked`), and `sudo rime live apply` activates them later (§10.3).
Everything else is `switch`.

What to expect from these rules: almost every nixpkgs move changes some unit's store paths (dbus, NetworkManager,
systemd itself), so rule 3 makes nearly every release that moves nixpkgs `boot`-class. In practice `switch` is for
Rime-only releases (the Shell, Rime's tools and daemons), which is the same split the bootc live engine has today.
Rule 1 compares the manifest's generic `kernel_params`; per-machine arguments from the host layer (LUKS, resume) are
not in the manifest, so a host-layer change is the hook's and the host-layer PR's to classify, not D6's.

Every unit that holds user state and is not on the allowlist (greetd, `rime-agentd`, `rime-lid`, the display manager)
is declared with `restartIfChanged = false` in the Rime modules, so even a hand-run `nixos-rebuild switch` does not
restart it. nixpkgs' greetd module already sets `restartIfChanged = false` ("Don't kill a user session when using
nixos-rebuild") **(checked at nixpkgs d357a2b; re-check at the pinned revision)**; the VM test in §15 proves it.

---

## 10. Live updates: the NixOS equivalent of the sysext engine

### 10.1 What carries over

The bootc engine (rime-os #109) stages, verifies, classifies the diff, builds a sysext layer of changed files under
`/run/extensions/rime-live`, then runs activators. On NixOS the store is immutable and every generation is complete, so
the layer goes away: a running process keeps its own store paths until it is restarted, and a restarted process
starts from the new generation's. What stays:

- `rimed_core::live::{plan, txn}` and `MachineState`/`SessionState`: the busy and lock rules, `DEPENDS`, the
  transaction state machine and its recovery table;
- the status document `/run/rime-live/status.json`, schema 1, with these changes: `booted.digest`, `from.digest` and
  `target.digest` carry the **release manifest digest** of that generation (from `applied.json`), and new fields
  `product: "nixos"`, `booted.toplevel`, `target.toplevel` and `generations` (current, booted, previous, pinned) are
  added. Rime Shell's `liveupdate.js` keeps working; its Recovery page reads `generations` instead of running
  `rpm-ostree status --json`;
- `history.jsonl`, `txn.json`, and the verbs `rime live status|apply|rollback|doctor|explain|logs`;
- the session helper `rime-live-session shell|hypr <revision> <session>`, run as the user through `systemd-run --user
  --machine=<user>@.host`, exit 0 done, 3 locked (deferred), 4 not running.

What changes:

- **Measurement.** `rimed_core::live::diff` (ostree diff, composefs dump, `rpm -qa`) is replaced by comparing the
  manifest's `components` with the running generation's (the running generation's manifest is in
  `/var/lib/rime/nix/releases/`). A generation with no saved manifest (made by hand) is "could not be measured": the
  update installs for the next restart and nothing is activated live, which is what the bootc engine does when it
  cannot measure.
- **Classification.** By store path, not by `/usr` prefix, and done by CI (§3.1).
- **Activation.** `switch-to-configuration test` activates the system side (units restart only inside the allowlist,
  §9). The activators shrink to the per-session ones: the Shell and the Hyprland configuration.
- **`MUST_BE_UNCHANGED (Shell, ShellRuntime)` is dropped on NixOS.** On bootc the live layer could not swap Qt or
  Quickshell under a new Shell. On NixOS a restarted Shell runs the new Quickshell and Qt from the new closure, so a
  ShellRuntime change no longer blocks the Shell. `(HyprlandConfig, Compositor)` stays: the running Hyprland is still
  the old one. Both are table tests.
- **Soft reboot.** `rime live soft-reboot` refuses on NixOS in v1 ("not supported on this system yet"): whether a
  NixOS generation comes up correctly through `systemctl soft-reboot` without the initrd is not verified.

### 10.2 The shell-only release

The case the P4 gate names. The manifest's `components` differ only in `shell` (and the toplevel and `system-path`
derivations that contain it): about 12 paths, 0.012 GB measured for a one-line change. The plan is `switch`. Steps:

1. `nix-env --set` and `switch-to-configuration boot` (the update is already safe for the next restart);
2. `switch-to-configuration test`; the dry run predicted no unit restarts outside the allowlist, so nothing restarts;
3. for each unlocked graphical session (one per user, gamescope sessions excluded, as `graphical()` does today),
   `rime-live-session shell <rime-shell commit>`; the helper restarts the Shell from
   `/run/current-system/sw/share/rime-shell` and confirms the new commit answers its `shell` IPC target;
4. a session that locks between planning and acting: the helper exits 3, and the engine undoes the activation
   (`switch-to-configuration test` on the generation that was running before) and records Deferred(locked), exactly
   as `activate()` does today;
5. any other failure: the same undo, recorded FailedRolledBack; the update still applies at the next restart.

The undo is a forward activation of the old generation, not a transaction rollback: what the new generation's
activation already did outside the store (users and groups created, tmpfiles, activation-script state) stays. That is
acceptable for the shell-only class this path is for, and one more reason rule 3 sends anything that touches system
units to `boot`.

### 10.3 Deferred, apply, rollback

- `sudo rime live apply [--only …]` re-plans against the machine as it is now and runs `test` plus the activators for
  what is allowed. Same verb, same text as bootc.
- `sudo rime live rollback` runs `/run/booted-system/bin/switch-to-configuration test` and the activators: the machine
  runs the booted generation again, and the newer generation stays the boot default, so it still applies at the next
  restart. This is what removing the sysext layer means on bootc.

---

## 11. Rollback, pin, garbage collection, disk headroom

### 11.1 `rime rollback`

Same description as today: "Roll back to the previous deployment" for the next boot.

1. pick the target by number: the newest generation **older than the booted one** (`/run/booted-system`), not
   `nix-env --rollback`'s "one before the profile's current". With an update pending for the next restart, the
   profile's current generation is that pending one, and `--rollback` would only cancel it and land on what is
   already running;
2. `nix-env -p /nix/var/nix/profiles/system --switch-generation <n>`;
3. `/nix/var/nix/profiles/system/bin/switch-to-configuration boot` (the hook makes generation `<n>` the default);
4. print `rime: the next restart boots <release> (generation <n>). Restart when you are ready.`

One difference from bootc is stated in the help text: `bootc rollback` swaps two deployments, so running it twice
returns to where it started; on NixOS running it again after restarting goes one generation further back. The
downgrade check (§8.2 step 6) compares with the newest release ever installed, so a rollback does not stop the next
`rime update` from moving forward again, exactly as on bootc.

`rime rollback --now` is not added in v1. A user who needs the old generation now restarts and picks it; that is also
v1's recovery path when the new generation does not boot: pick an older entry in the GRUB menu (the hook keeps the menu
visible long enough to do that, D3).

A refused update (any refusal in §14) leaves the profile untouched, so `rime rollback` after it goes to the generation
before the last **successful** update. That is a D6 gate.

### 11.2 `rime pin`

Today `rime pin` runs `ostree admin pin 0` (keeps the booted deployment). On NixOS it keeps the **booted** generation:
a GC root `/nix/var/nix/gcroots/rime/pin-<generation>` and an entry in `/var/lib/rime/nix/pins.json`. GC never removes
a pinned generation and the hook keeps its menu entry. `rime pin --remove <generation>` is the one addition.

### 11.3 What GC keeps

`rime gc` (also run by a daily timer and before an update that would not fit) decides with a pure function,
`rimed_core::nixrel::gc_plan`, and keeps:

- the current generation (the profile's) and the booted one;
- the generation before the current one (the rollback target);
- every pinned generation;
- the current and previous generation of the `rime-apps` profile;
- every path rooted elsewhere (users' own profiles, `result` links);
- while `txn.json` names a transaction between `verified` and `staged`, the already-valid paths of that
  transaction's target closure (§8.5, interrupted downloads). `gc_plan` adds them as temporary roots for the length
  of the `nix-store --gc` run.

It deletes older generations by explicit number (`nix-env -p … --delete-generations <n> <m> …`), never with `-d` or
`old`. Then it runs the hook so their menu entries go away, then `nix-store --gc`. It never runs while an update holds
the lock, and it never deletes the previous generation until the health record shows that the machine **booted into**
the current one (for boot-class updates) or ran it for one health check (for switch-class): otherwise a bad release
could take its own rollback target with it.

### 11.4 Health record

`LastUpdate` keeps its fields with the same meaning (`from_digest` is what ran before: on NixOS the running toplevel,
as on bootc it is the booted digest) and gains `toplevel` (the target) and `kind`. It is written at step 12, just
before the profile changes (§8.2), so it only ever describes an update that was installed. `rebooted_into_new`
becomes: for `boot`, `/run/booted-system` resolves to `toplevel`; for `switch`, `/run/current-system` does. A machine
that is still on `from` (not restarted yet, or rolled back) is the permissive state `halt_reason` already treats as
"not taken yet". `channel::verdict` and its regression rows are unchanged.

### 11.5 Disk headroom

Before downloading: free space on the filesystem holding `/nix/store` must be at least the missing paths' unpacked size
(`nar_size`, an over-estimate on `compress-force=zstd:3`) plus a reserve of 1 GB. If it is not, `rime gc` runs first
with the rules above; if that is still not enough, the update is refused by name, with the numbers. For fresh installs
where `/boot` is the ESP, the hook's own floor (D3) covers the ESP: a boot-class update whose kernel and initrd do not
fit is refused by the hook, and the client says so (§14).

---

## 12. Channels and pinning

- **Channels.** `edge`, `beta`, `candidate`, `stable`, as on bootc, now entries in the channel document. Only `edge` is
  published at first; another channel with no entry gets `rime: nothing is published on <channel> yet`. `rime channel
  set <channel>` writes `/var/lib/rime/nix/channel` (root-written, mode 0644 so `--check` and `rime channel status`
  read it without root; no `bootc switch`), and keeps its consent rule for a
  move to a slower channel. `rime channel status|list|report|rollout` read the same document.
- **Staged rollout.** `percent` and `halt` per channel, as specified for bootc but never used there. `promote` in
  `nix-release.yml` is the only writer.
- **The nixpkgs pin** is the new "core tier". It lives in rime-os's `flake.lock` and moves only on Rime's schedule
  (§13). Every override (kernel, msi-ec, scx, NVIDIA, security backports, single-package holds such as a regressing
  Hyprland) is a file under `nix/overrides/` with an owner and an expiry date; a flake check fails when an override is
  past its expiry.
- **A user holding a release** is `rime pin` plus `--no-live`/not updating; there is no per-machine "stay on release
  X" in v1.

---

## 13. Mass rebuilds and security backports

### 13.1 How a nixpkgs move is classified

A weekly hosted job (`nix-bump.yml`) proposes moving the nixpkgs input to the newest `nixos-unstable` that the Rime
closure builds against. The PR body states:

- whether `stdenv` changed (the mass-rebuild signal used by the size probe);
- the download a machine on the current release would make (from the R2 and cache.nixos.org narinfos, the same method
  as the probe);
- the CVEs the weekly scan (vulnix or the nixpkgs tracker, D8) reports as fixed by the move.

### 13.2 The batching rule

- A move is a **mass rebuild** when `stdenv` changed **or** the measured download for a machine on the current
  release is 1.5 GB or more. `stdenv` alone is not enough: the probe window had a systemd+mesa+Qt rebuild on 10-24
  without a `stdenv` change. The 1.5 GB line keeps every unbatched update under bootc's 1.74 GB core-rebuild update
  (#123), so an ordinary week is never worse than the bootc update it replaces.
- A move that is not a mass rebuild merges as an ordinary PR.
- A mass rebuild waits for the batch window: at most one per 4 weeks. The file `nix/batch-window.toml` records the
  last mass rebuild's release ID and date; a check refuses an early mass rebuild unless the PR carries the label
  `security` and names the CVE.
- Expect the pin to move about monthly in practice. The 45 days measured had 2 `stdenv` changes plus one large
  non-`stdenv` rebuild, so roughly every other week brings a mass rebuild; and once one is held back, every newer
  nixpkgs commit contains it, so no bump can merge until the window opens. Everything that matters for security
  between batches therefore travels as a backport (§13.3).
- Release manifests mark `batch.mass_rebuild` and `batch.security`, the release page says why the update is large, and
  the client's size message says so (§8.4).

### 13.3 Security backports

- Response times: critical within 72 hours, high within 7 days, others with the next batch.
- A fix ships as an override under `nix/overrides/security/<cve>.nix` (owner, expiry, upstream reference). An
  override rebuilds everything that depends on the package, and those paths become Rime builds (built on the build
  host, uploaded to R2, not on cache.nixos.org). The size depends on how low the package sits:
  - a leaf or near-leaf package (Firefox, a single app, a daemon) is small and is backported;
  - a mid-level library (openssl, curl, libxml2) rebuilds a large part of the closure: often over 1 GB of download
    and hours of build time. It is backported when the measured download stays under the 1.5 GB line; otherwise it is
    treated like the next item;
  - **glibc, or anything in `stdenv`'s bootstrap closure, cannot be backported small.** Every path depends on it, so
    an override *is* a mass rebuild, built entirely by Rime (the whole closure, including the toolchain, on the build
    host). In v1 such a CVE ships as an **early mass rebuild**: the nixpkgs move that carries the fix, with the batch
    window restarting from it. NixOS's graft mechanism (`system.replaceDependencies`) avoids the compile but not the
    download: every path that refers to the old glibc is rewritten and gets a new hash, so the download is still about
    the whole closure (~4 GB). Only the G2 delta transport (§17) would make it small.
- The 30-day byte budget (P4) must therefore count each glibc-level security fix as one full download. A month with a
  scheduled mass rebuild and a glibc CVE is two full downloads (~8 GB), and that month will fail the bootc comparison.
  K4 measures two consecutive windows for this reason; a single such month is not a kill.
- When no backport is possible within the response time, the pending mass rebuild ships early, and the batch window
  restarts from it.
- Every override is reviewed weekly and removed when nixpkgs carries the fix.
- The release byte log (from `delta` in every manifest) is the artefact for the P4 30-day gate and kill criterion K4.

---

## 14. Failure modes and what the user reads

Every message starts with `rime:` and keeps the bootc client's wording where the situation is the same. Exit code 1
unless stated. "Nothing was changed" is only printed when the profile and the boot menu are untouched.

| # | Condition | Where caught | Message (first line; later lines name the detail) | After |
|---|---|---|---|---|
| F1 | No network / registry unreachable | §8.2 step 3 | `rime: could not reach the release server (<reason>); nothing was changed.` | passes still run; exit 1 |
| F2 | Channel document not signed | step 3 | `rime: ghcr.io/…:nix-channels is not signed (<why>), so it was not read` | as bootc's rollout reader |
| F3 | Channel document or manifest signed by another identity | steps 3-4 | `rime: this update is being held. The release it would install does not verify.` + `verify::refusal` body | `--allow-unverified` |
| F4 | Signature check could not run (skopeo/openssl failed) | steps 3-4 | `… could not be verified — which is not the same as failing to verify.` | as F3 |
| F5 | Channel document expired, still expired after a re-fetch | step 3 | `rime: the newest channel document this machine can find expired on <date>. Rime may be unable to publish, or something is holding updates back. The OS was not updated.` | passes run |
| F6 | Replay (serial lower than accepted) | step 3 | `rime: the channel document is serial <n> and this machine has already accepted <m>; an older document served again is a replay, so it was ignored` (existing text) | as F5 |
| F7 | Channel halted / outside the ramp | step 3 | existing `rollout_hold` text | exit 0, passes run |
| F8 | Manifest invalid (schema, system, closure not closed, bad path) | step 4 | `rime: the release manifest for <release> is not usable: <reason>. Nothing was changed.` | |
| F9 | Manifest needs a newer client (schema > 1) | step 4 | `rime: release <release> needs a newer rime than this one. Nothing was changed.` | |
| F10 | Variant missing | step 4 | `rime: release <release> has no <variant> system; this machine was installed as <variant>. Nothing was changed.` | |
| F11 | Downgrade | step 6 | `rime: the channel offers <older>, which is older than the installed <newer>; this machine stays on <newer>.` | exit 0 |
| F12 | Not enough disk | step 9 | `rime: this update needs <x> GB free on /nix and <y> GB is free after removing old generations. Nothing was changed.` | |
| F13 | Cache does not have a path | §8.5 | `rime: the update server does not hold all of release <release> yet (<path>). Try again later; nothing was changed.` | no build ever |
| F14 | Narinfo differs from the manifest | §8.5 | `rime: the update server's copy of <path> does not match the signed release. Nothing was downloaded or changed.` | security event, logged |
| F15 | NAR hash or narinfo signature wrong during download | `nix copy` | `rime: a download did not match its signature (<nix error>). The partly downloaded paths are not used; nothing was changed.` | |
| F16 | Interrupted download (power, kill) | next run | none needed: `rime update` resumes; finished paths are kept (also through a `rime gc` in between, §11.3) | |
| F17 | Local closure differs from the manifest after download | §8.6 | `rime: the installed copy of <path> differs from the signed release. It was removed and will be downloaded again; nothing else was changed.` A second time for the same path: `… the update server keeps serving a different copy of <path>; \`rime doctor\` explains.` | security event, logged |
| F24 | Channel document valid but more than 3 days old | step 3 | `rime: the newest release information this machine can get is <n> days old.` (a warning line; the update continues) | exit unchanged |
| F18 | `boot.json` differs from the manifest | §8.6 | `rime: release <release> describes its kernel differently from what it contains. Nothing was changed.` | |
| F19 | Boot hook failed (ESP full, write error) | §8.7 | `rime: the boot menu could not be updated (<reason>). This machine still boots <current release>; nothing else changed.` | profile put back |
| F20 | Live activation failed | §10.2 | `rime: live activation failed: <err>; undoing it` then `rime: undone; the update applies at the next restart` (existing text) | exit 1, still installed for boot |
| F21 | Session locked / Gaming Mode / game | §9 | existing deferral lines: `deferred until the session is known to be unlocked: …`, `deferred while the machine is busy: …` | exit 0 |
| F22 | Health stop | step 2 | existing `halt_reason` text | `--force` |
| F23 | Flatpak, catalogue or firmware pass failed | step 15 | existing pass lines | never fatal |

`rime doctor` gains an `update` section that re-runs the offline parts (saved signature, applied closure check, keys,
cache URL reachability) and names the first failure.

---

## 15. Tests

### 15.1 Unit tests (`cargo test`, fixtures, no network)

In `rimed-core` (pure logic):

- manifest parse and validate: a good manifest; each refusal in §3.2; unknown fields ignored; schema 2 refused; both
  narHash spellings;
- channel document: the existing `decide_rollout` tests run against a Nix-tagged document, plus the `release` field;
- `activation()`: one row per rule in §9, including "pending boot-class update, then a shell-only update stays
  `boot`", "NVIDIA changed on a machine without NVIDIA", "dry-run output not understood → `boot`", "locked plus
  rime-tools only → `switch`", "locked plus shell → `boot`, deferred";
- the NixOS plan rules (§10.1), including the dropped `(Shell, ShellRuntime)` rule and the kept
  `(HyprlandConfig, Compositor)` rule;
- `gc_plan`: keeps current, booted, previous, pinned and the `rime-apps` pair; never keeps nothing; never deletes the
  previous generation before the health record says so;
- size and plan from a manifest and a validity set;
- the narinfo-vs-manifest comparison (`nix path-info --json` from a fixture of real 2.35 output).

In `rime` (the CLI), with `RIME_TRUST_ROOT` fixtures and stub `nix`/`skopeo` programs, following `tests/test-rime-live.sh`
(`unshare -Urm`, stubs bind-mounted): every message in §14 is produced by its condition. Under a fixture root the
client still refuses to deploy ("this program will not deploy on fixture facts"), so these tests stop before step 12.
Whether `unshare -Urm` works inside the Nix build sandbox is not verified (plan open question 6); if not, these run as
a separate CI step, not inside `nix flake check`.

Mutants for each security check: removing the narinfo comparison (including `FileSize`), the post-download closure
comparison, the signer check, the replay check, the "cache only verified documents" rule, the "only the verified
manifest's keys" rule, the downgrade high-water mark, or the `booted` comparison in rule 1 must each fail at least one
test.

### 15.2 VM tests (`runNixOSTest`, `nix/tests/update-*.nix`)

Cosign keyless cannot run inside a VM (no Fulcio, no GitHub OIDC), and the fixture root refuses to deploy. So the VM's
client is built with **test trust anchors through the production code path**: the NixOS options
`rime.trust.fulcioRoot`, `rime.trust.nixSigners` and the narinfo key point at a throwaway CA, a throwaway identity and
a throwaway `rime-test-cache-1` key generated inside the test. A test helper (`nix/tests/lib/fake-sigstore.py`) issues
a leaf certificate with the GitHub OIDC issuer extension and the identity as SAN, signs the simple-signing payload, and
pushes `.sig` the way cosign does. verify.rs checks chain, SAN, issuer and payload with openssl, so the same code
verifies it. No real Rime key is ever used.

Nodes:

- `registry`: an OCI registry (the NixOS `docker-registry` service) standing in for GHCR;
- `cache`: **minio** with a bucket `rime-cache`, anonymous read, standing in for R2. Clients read it over plain HTTP
  (`http://cache:9000/rime-cache`); the test driver writes it through the S3 API (`s3://rime-cache?endpoint=http://cache:9000&region=auto`),
  which also exercises the fill path;
- `client`: a Rime NixOS system at generation A, with releases B (shell-only change), C (kernel change), D (glibc
  change) built by the test and published to `registry` and `cache` by the same publish script CI runs.

The test network has no route to the internet, so passing tests also prove the client never needs cache.nixos.org.

| Test | Proves |
|---|---|
| `update-shell-live` | A→B: `/run/current-system` is B without a reboot; the Shell process in an unlocked session restarted onto B's store path; the downloaded bytes (minio access log) are ≤ 50 MB; status.json `active` |
| `update-kernel-boot` | A→C: current stays A, profile is C, the hook's menu file names C as default; after reboot `/run/booted-system` is C |
| `update-pending-boot-then-shell` | A→C without reboot, then C→C' (shell-only) is still `boot` |
| `update-glibc-boot` | A→D is `boot` (rule 4) |
| `refuse-tampered-narhash` | a manifest whose narHash for one path differs: refused before any NAR request (minio log), no new valid path, F14 text |
| `refuse-unsigned` / `refuse-wrong-identity` | manifest unsigned; signed by an identity naming `build-image.yml` or a fork: refused, F2/F3 text, profile unchanged |
| `refuse-expired` / `refuse-replay` | expired channel document re-fetched once, then refused; a lower serial ignored; a document read under `--allow-unverified` with a huge serial does not block the next genuine one |
| `refuse-downgrade` | the channel names an older (validly signed) release: F11, profile unchanged; after `rime channel set` to a slower channel the older release installs once |
| `refuse-oversized-nar` | a narinfo whose `FileSize` differs from the manifest: refused before any NAR request |
| `narinfo-swap` | the cache serves a good narinfo to the bind and a different validly signed one to `nix copy`: §8.6 refuses, deletes the path, the next run re-downloads it; a second swap is F17's second message |
| `revoked-key` | a manifest that drops `rime-test-cache-1` and announces only `-2`: a path signed only with `-1` is refused even though the client's `nix.conf` still lists `-1` |
| `interrupted-then-gc` | `nix copy` killed mid-way, then `rime gc`, then `rime update`: the finished paths are not downloaded twice (minio log) |
| `hook-crash-mid-step-12` | power cut after `nix-env --set`, before the hook finished: the next `rime update` re-runs the hook instead of reporting "up to date" |
| `refuse-cache-miss` | a path deleted from minio: F13, and no build was attempted (`max-jobs = 0`) |
| `refuse-untrusted-narinfo-key` | narinfos signed by a key the client does not trust: F15 |
| `rollback-after-refusal` | after any refusal, `rime rollback` then reboot boots the generation before the last successful update |
| `deferred-locked` | B with the session locked: installed for boot, Shell deferred; after unlock `rime live apply` activates it |
| `deferred-gaming` | B with a gamescope session or Gaming Mode held: deferred busy |
| `greetd-survives-switch` | the greetd PID is the same before and after a switch that changes greetd's unit |
| `live-rollback` | `rime live rollback` runs the booted generation; the newer one is still the boot default |
| `gc-keeps` | five updates and a pin: current, booted, previous and pinned remain with menu entries; the rest are gone |
| `headroom` | a small `/nix` disk: F12 before any download |
| `hook-failure` | a hook made to fail: F19, profile and menu as before, the failed generation deleted; the next update (hook fixed) succeeds |
| `interrupted-download` | `nix copy` killed mid-way, then `rime update` completes; profile unchanged in between |
| `fill-and-publish` | the publish script against minio with a fake upstream cache (signed with a fake upstream key): upstream signatures kept, Rime key added, verification from the public URL catches a deleted NAR, the manifest is not pushed when any path is missing; a re-signed narinfo is seen by the next public read; runs with a disk quota smaller than the closure |
| `trust-offline` | `rime trust` with networking off re-verifies the saved signature and the running toplevel |

### 15.3 Workflow checks

- `actionlint` on `nix-release.yml`, `nix-bump.yml`;
- a repo test that every self-hosted job carries the fork-PR guard and none references the `nix-release` environment,
  `RIME_NIX_CACHE_KEY` or the R2 secrets;
- a repo test that every job in `nix-release.yml` with `id-token: write` runs on a hosted runner, only on `main`, in
  the `nix-release` environment, that every `uses:` is pinned by a 40-character SHA, and that no `run:` step contains
  `${{ inputs.` or `${{ github.event.`;
- the publish self-checks (§5.6) on every release.

### 15.4 The P4 update gates, measured

| Gate | Measured by |
|---|---|
| 30-day bytes ≤ bootc at the same cadence | release byte log (`delta` per manifest) plus the client's `history.jsonl`, which records bytes downloaded per update; compared with plan PR 4's bootc figure |
| security response times | the `security` label timestamps on backport PRs against the CVE publication dates in the scan report |
| shell-only ≤ 50 MB | the `update-shell-live` VM test, and `delta` on every shell-only release |
| wall-clock | `history.jsonl` records download and apply durations; a VM harness with a shaped link (`tc`) for both backends |
| live shell update with the lock and Gaming Mode rules | `update-shell-live`, `deferred-locked`, `deferred-gaming` |

---

## 16. Code layout in `rimed/`

New, pure (`rimed-core/src/nixrel/`):

- `manifest.rs`: `Manifest`, `Variant`, `PathEntry`, `validate()`, closure walk, narHash normalisation;
- `activation.rs`: `activation()`, the dry-run output parser, the live-restart allowlist;
- `gc.rs`: `gc_plan()`;
- `classify.rs`: store path → `Component` (shared by CI and the client);
- `mod.rs`: media types and tag names (`NIX_CHANNELS_TAG`, `NIX_RELEASE_MEDIA_TYPE`, `NIX_CHANNELS_MEDIA_TYPE`).

New, side effects (`rime/src/nixos/`):

- `update.rs`: §8.2's steps, called from `ops::update` when `platform::detect()` says NixOS;
- `fetch.rs`: channel document and manifest through `verify::{resolve, verify_signature_as, artifact_layer_at}` and
  the new by-digest `fetch`;
- `store.rs`: the `nix`, `nix-env`, `nix-store` calls with the fixed `--option` set, JSON parsing for `--json-format 1`;
- `activate.rs`: `switch-to-configuration boot|test|dry-activate`, the per-session activators (reusing
  `live.rs`'s `sessions()`, `machine_state()`, `session_helper()`), undo;
- `gc.rs`, `pin.rs`, `rollback.rs`.

New binary: `rime-nix-release` (in the `rime` crate or `rimed-core/src/bin/`): `manifest` (build the manifest from
`closure.json` and the toplevels, with the shared classifier), `verify` (the publish self-check, using `verify.rs`).

Changed: `trust.rs` (`NIX_SIGNER`, `nix_expectations`), `verify.rs` (by-digest fetch, refusal noun),
`channel.rs` (`LastUpdate.toplevel`/`kind`, the Nix channel reader), `ops.rs` (backend dispatch; `pin`, `rollback`,
`changelog`, `flatpak_pass` without rime-pkg: `flatpak update --system -y --noninteractive` and, for `SUDO_USER` with a
user remote, the same `--user` under `runuser`, as rime-pkg does today), `live.rs` (the NixOS measure/activate path
behind the backend switch), `main.rs` (help texts).

Unchanged and reused: `verify.rs`'s Fulcio, certificate, payload and enforcement code; `rimed_core::channel`;
`rimed_core::live::{plan, txn}` and the status schema; `firmware_pass`.

Thrown away on NixOS only (kept for bootc until its sunset): `parse_bootc_status`, `build_layer`, sysext refresh,
`swap_in`, `FsyncGuard`, `migrate_boot_path`, the apex→rime `bootc switch` block, `rimed_core::live::diff`.

Also outside rimed: `rime-update-check` (runs `bootc upgrade --check`) becomes `rime update --check --json` writing
`/run/rime-update/available.json` for `rime-update-notice`; `/etc/rime/release.json` (an `environment.etc` entry of
each generation, so it changes when the generation activates) replaces `/usr/share/rime/release.json` for Rime Shell's
`release.js` and the what's-new page.

---

## 17. Handover and open items

- **TODO(handover): the real R2 bucket.** Create bucket `rime-cache`, the custom domain `cache.rimeos.com` on the
  rimeos.com zone, a cache rule so `*.narinfo` and `nix-cache-info` bypass the edge cache (§6.1), an API token scoped
  to that bucket, the GitHub environment `nix-release` (deployment branch `main`) with `RIME_R2_ACCESS_KEY_ID`,
  `RIME_R2_SECRET_ACCESS_KEY`, `RIME_R2_ENDPOINT`, and a $20/month billing alert. Until then every test uses minio.
  R2's S3 compatibility with Nix's S3 store is not verified.
- **TODO(handover): someone receives the refresh alarm.** The daily `refresh` fails loudly when the live channel
  document has under 7 days left (§4); a failed scheduled run must reach a person (GitHub's failed-workflow email to
  the repository owner is the minimum).
- **Build-host trust (§6.4).** The self-hosted builder is trusted for content. A later step: the hosted fallback
  rebuilds the Rime-built paths (not the kernel, which is too slow there) and the publish job requires matching
  narHashes. Not designed here; needs reproducibility work on rimed and rime-shell first.
- **Kernel signing as a signing oracle.** D7's hosted signing job signs the kernel image the build host hands it. The
  same build-host trust applies; D7 owns the answer.
- **TODO(handover): the narinfo key.** Generate `rime-cache-1` on a machine that is not a CI runner, set
  `RIME_NIX_CACHE_KEY`, commit the public key, delete the local copy.
- cosign v2.5.2 pin and the publish self-check (§5.6); move to v3 only when the self-check passes with it.
- `nix store sign --store s3://…` writing narinfos in place: verified in `fill-and-publish` with minio; fallback in
  §6.3.
- Streaming of `nix copy` between two remote stores without local disk: verified in `fill-and-publish` with a quota.
- `switch-to-configuration-ng`'s dry-run output and greetd's `restartIfChanged` were read at nixpkgs `d357a2b`
  (26.11pre); re-check at the pinned revision when the flake lands.
- Release-ID race between the two pipelines on the same day: re-read before push (§2); residual risk caught by the
  rimeos.com chain test.
- Soft reboot into a NixOS generation: not supported in v1 (§10.1).
- G2 delta transport (`zstd --patch-from` against the previous release's NARs): measured in plan PR 5 first. If it is
  adopted, it becomes a step between §8.5's binding and the import: fetch delta, rebuild the NAR, check the narHash
  against the manifest, `nix-store --import`. The manifest's narHash binding is what makes that safe without trusting
  the delta.
- fwupd UEFI capsules with Secure Boot on (plan open question 17): the firmware pass runs as today; the capsule path's
  signing is decided with the boot chain, not here.
- Licences of unfree paths before anything is uploaded to a public bucket (§6.3, plan R14).
