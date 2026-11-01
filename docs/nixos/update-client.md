# `rime update` on NixOS: the client

This is the client half of D6 ([updates-and-release.md](updates-and-release.md)). It covers what the code does,
where it stops, and what is still left to do. The contract is the design doc. This page records how the code meets
it, plus the places where it had to choose something the doc left open.

## Where it lives

| What | Where |
|---|---|
| Pure logic: manifest, channel document, plan, boot/switch rule, gc plan, NAR hashes | `rimed/rimed-core/src/nixrel/` |
| Effects: fetch, store, generations, update, trust gate | `rimed/rime/src/nixos/` |
| NixOS module: `/etc/rime/update.toml`, nix.conf trust, greetd, the daily gc | `nix/modules/update.nix` |
| VM test | `nix/tests/update-client.nix` (`checks.x86_64-linux.update-client`) |
| Throwaway test keys and the fixture publisher | `nix/release/fixture-{keys.nix,publish.nix,publish.py}` |

`rime` picks the NixOS backend at runtime when `/etc/NIXOS` exists (`nixos::is_nixos`). The bootc code path does
not change. Dispatch is one `if is_nixos()` at the top of `ops::update`, `ops::pin`, `ops::rollback`, and the
`--gate` branch of `trust::run`, plus the new `rime gc` verb and `rime update --pin/--unpin`.

## What `rime update` does (§8.2)

1. It takes the live engine's lock (`/var/lib/rime/live/lock`), shared with rollback, gc and `--pin`.
2. It fetches the channel document (tag `nix-channels`) and verifies its cosign-shaped signature against the signing
   identity pinned in `update.toml`. A refusal is fail-closed and names its cause. An unsigned document fails with
   "is not signed", the wrong identity fails with the certificate's identity, and a digest that does not match the
   payload fails with "does not verify". The highest verified serial is kept in `/var/lib/rime/nix/channels.json`, so
   an older document served again is a replay and is ignored.
3. It fetches the manifest **by digest** (`docker://repo@sha256:…`), verifies its signature the same way, then
   validates it.
4. It stops if the target is already installed ("up to date"), or if the target is older than the highest release
   this machine ever applied (downgrade).
5. **min_from.** If the manifest carries `min_from`, a machine whose installed release is older than it is refused,
   and nothing changes. `min_from` is optional, and the manifest schema was extended with it. A machine whose
   running release is unknown (not installed by `rime update`) is not refused.
6. It computes the plan and checks headroom: the free space on `/nix` must be at least the missing NAR bytes plus
   1 GB.
7. **Bind.** Before anything is downloaded, `nix path-info --json --store <cache>` must return the manifest's exact
   narHash, narSize, file size and references for every missing path.
8. It runs `nix copy --from <cache> --no-recursive --stdin` with `require-sigs = true`. The trusted keys come from
   the **verified manifest's** `cache.keys`. They are never taken from the command line, and never from nix.conf
   except under `--allow-unverified`. A narinfo signed by any other key is refused ("did not match its signature").
9. It checks the copied paths against the manifest again (`check_local`), and checks the toplevel's `boot.json`
   against the manifest's `boot`.
10. It runs `nix-env --profile /nix/var/nix/profiles/system --set <toplevel>`, then
    `switch-to-configuration boot`. If the boot hook fails, the profile is put back, the new generation is deleted,
    and the hook runs again for the old one (§8.7).
11. **Live.** It runs `switch-to-configuration dry-activate` and applies the rule below. If the rule allows a
    switch, it runs `switch-to-configuration test` and then the per-session activators. If that fails, it runs
    `test` on the old generation, and the update stays installed for the next boot.
12. Then come the Flatpak pass (system remotes) and the fwupd pass (shared with bootc: `ops::firmware_pass`).

**The update notice.** `rime-update-check.timer` (two minutes after boot, then every six hours, as on bootc) runs
`rime update --check`: steps 2-4 without recording the serial, nothing downloaded. As root it writes
`/run/rime-update/state` (`state=available|none`, `digest=<manifest digest>`), the file Settings -> Updates reads.

The channel document's digest is the sha256 of the manifest the registry serves for `nix-channels`, read with
`skopeo copy`, not `skopeo inspect`: inspect parses the config as an image's and refuses an OCI artifact with an empty
config ("unsupported image-specific operation on artifact"), which is how §3.3 publishes it.

`/run/rime-live/status.json` keeps the shape the Shell already reads on bootc, with `product: "nixos"` added. The
states are `staged`, `deferred`, `activating`, `active` and `rolled-back`.

## The boot/switch rule (§9)

An update is applied live only when it is **proven** not to touch anything a running process depends on.
Everything else installs for the next restart. "Could not tell" always means `boot`.
`rimed-core/src/nixrel/activation.rs::activation` implements the following, and every rule that fires prints one
line.

1. The kernel, initrd, kernel modules or kernel parameters differ from the **booted** generation's. Comparing
   against the booted generation, not the running one, keeps every later update boot-class once a boot-class
   update is pending.
2. The NVIDIA userspace changes. This is skipped on a machine known to have no NVIDIA GPU.
3. The dry activation would restart systemd, would stop or restart any unit outside `RESTART_SAFE_UNITS`
   (`rimed`, `rime-remoted`, `polkit`), would reload any unit outside those and `RELOAD_SAFE_UNITS`
   (`dbus-broker`, reload only), or printed a line the parser does not know. A unit the dry run lists as "would NOT
   stop the following changed units" (`restartIfChanged = false`, which is greetd's case) is not disruptive.
   The notice `Not checking switch inhibitors (action = dry-activate)` (nixpkgs `switchable-system.nix`) is expected
   and skipped.

   **Why polkit and dbus-broker are on the lists (measured in the VM test).** Every Rime package (the Shell, rimed,
   Rime's polkit actions) reaches the system through `environment.systemPackages`, so every Rime release changes
   `system.path`. At the pinned nixpkgs, `dbus.nix` builds dbus-broker's config dir from `system.path` and reloads it
   on change (`reloadIfChanged = true`), and `polkit.nix` restarts polkit on any `system.path` change
   (`restartTriggers = [ config.system.path ]`). With the lists as the contract started them, a Shell-only release
   printed `would reload dbus-broker.service` and `would stop/start polkit.service` and was boot-class, so no
   Rime-only release could ever go live (the contract's §9 expects the opposite). The test proves the reload keeps
   dbus-broker's PID and the switch lands. Not proven: that the Shell's polkit authentication agent registers again
   after polkit restarts (the test has no graphical session).
4. glibc or linux-pam differ from the running or the booted generation.
5. **Gaming Mode / busy.** A game is running (a non-empty `/sys/fs/cgroup/rime-game/cgroup.procs`, or a known game
   process), or this could not be told. The update installs for the next boot and the live part is **deferred**.
6. **Session lock.** The Shell or the Hyprland configuration changes, and a graphical session is locked or its lock
   state is unknown. The update is deferred in the same way.

Systemd, the boot loader and firmware files also always mean boot. So does a Hyprland configuration that changes
together with Hyprland itself.

A Shell-only release therefore goes live, on a machine that is idle and unlocked. On a machine with no graphical
session, the Shell is reported as `active-at-next-use`.

**greetd never restarts on a switch.** `nix/modules/update.nix` sets `restartIfChanged = false`, which is the
nixpkgs default too, so the guarantee does not depend on upstream. The VM test checks the generated unit
(`X-RestartIfChanged=false`) and checks that greetd's PID is the same before and after a live switch that changed
greetd's own unit.

## Rollback, pin, hold, gc

- `rime rollback` (§11.1) switches the profile to the newest generation **older than the booted one**, runs the boot
  hook, and asks for a restart. It never creates a generation.
- `rime pin` adds a GC root (`/nix/var/nix/gcroots/rime/pin-<n>`) for the booted generation, so `rime gc` keeps it.
- `rime update --pin <release>` holds the machine on an installed release (§12). It pins that release's generation,
  makes it the boot default if it is not already, and `rime update` installs nothing newer until `--unpin`. A
  release that is not installed on this machine is refused.
- `rime gc` (also a daily timer) keeps the following generations and deletes every other one. It refuses to run if
  the booted generation cannot be resolved.
  - the current generation, the booted generation, and the one before each of them;
  - every pinned generation;
  - the `max(keepGenerations, 2)` newest;
  - the `from` generation of an update that has not been confirmed healthy.

  After deleting, it runs `nix-store --gc`. `nix.gc.automatic` is forced off, so only `rime gc` removes
  generations.

## `rime trust --gate` on NixOS

This command passes only when the **running** system is a signed release. It looks up `/run/current-system` in
`/var/lib/rime/nix/applied.json` (the releases `rime update` installed). Then it fetches that release's manifest
again from the registry, by the digest recorded at install, and checks two things:

- its signature verifies against the identity pinned in `update.toml`;
- the manifest's toplevel for this machine's variant is the running one.

It exits 0 only when both hold.

It fails on a system that `rime update` did not install, for example the first generation of a fresh install or a
`nixos-rebuild` build. The VM test checks both answers.

## What the VM test proves

`nix build path:.#checks.x86_64-linux.update-client` runs two nodes.

- **server** runs a `docker-registry`, plus **RustFS** serving the binary cache over S3/HTTP. nixpkgs removed MinIO
  in 2026-10 and `minio-client` is marked insecure. RustFS is an S3 store, like R2, so the client still reads the
  cache over plain HTTP from an object store. On the server, `rime-fixture-publish` pushes everything in the order
  §3.3 gives. The keys it uses are throwaway keys, generated in a derivation and never committed:
  - signed narinfos;
  - signed manifests;
  - a signed channel document.
- **client** boots generation A with systemd-boot. Releases B to E are built from the client's own configuration with
  `extendModules`: B has another kernel, C another Shell, and D and E another `/etc` file.

The test checks the following.

- An unsigned channel document is refused. `rime trust --gate` fails before the first update and passes after it.
- **Kernel diff (B):**
  - the plan says it applies at the next restart;
  - the update leaves `current-system` and the boot id unchanged and sets the profile;
  - `status.json` shows `staged`;
  - after a reboot the client runs B's kernel and `trust --gate` passes.
- `rime rollback` returns the profile to A, and after a reboot the client runs A's kernel again.
- **Shell-only diff (C):**
  - while a fake game sits in the rime-game cgroup, the plan names rule 5;
  - without it, the update goes live: no reboot, same boot id, same greetd PID, the new Shell's marker in the running
    system, and `status.json` shows `active`.
- **Refusals:** each refused update leaves the profile unchanged.
  - a tampered manifest (`does not verify`);
  - a wrong signing identity;
  - a replayed channel document;
  - a downgrade;
  - `min_from`;
  - a manifest announcing the wrong cache key, which nix refuses (`did not match its signature`) and which leaves
    the path invalid on the client.
- `rime gc` with `keepGenerations = 2` deletes only the generations it should. It keeps A (booted), B (pinned) and
  the two newest, and A, B, D and E stay valid.
- `--pin` holds and `--unpin` releases. After a final reboot into E, `trust --gate` passes.

## Handover (TODO)

- **The real cache key.** `rime.update.cacheKeys` defaults to `[]` until Andre mints `rime-cache-1` (§6.4). It only
  matters for `--allow-unverified`, because a normal update trusts the keys of the verified manifest.
- **The real signing identity and root.** The defaults pin `nix-release.yml@refs/heads/main` and the Sigstore root
  built into rimed. Nothing has been signed by the real release job yet, so this end of the chain is unproven
  outside the fixture.
- **`rime-live-session` is not packaged on NixOS** (`rime-libexec.nix` drops it). While it is missing, a live Shell
  change in a running graphical session is reported as `pending` ("log out and back in"), not swapped in place.
  The VM test has no graphical session, so it proves `active-at-next-use`, not the in-session swap.
- **Not wired into NixOS:**
  - the live-update transaction record (`/var/lib/rime/live/txn.json`, `history.jsonl`) and `rime live
    status/rollback`;
  - the health-stop `halt_reason` of the channel document;
  - the hooks' `--check`;
  - §8.6 repair (re-copying a path that fails `nix-store --verify`).
- **The catalogue pass** (§8.2 step 15, the replacement for rime-pkg) belongs to the catalogue PR.
- **The Flatpak pass** updates system remotes only. `--user` remotes of `SUDO_USER` are still to do.
- **Gaming Mode.** Rule 5 reads the rime-game cgroup and game process names. A held Gaming mode with no game running
  is not "busy" today.
