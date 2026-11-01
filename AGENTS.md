# Rime OS agent contract

All coding agents must work against the contract below; syntax and local correctness are not enough. Do not assume behavior because it resembles Fedora, Bazzite, or another image-based distribution. Before editing, trace the relevant build and runtime path; after editing, run every applicable test in `.github/workflows/pr-validation.yml`.

## Product identity

Rime OS is a production, atomic Fedora bootc operating-system family. Treat it as a product, never as a Fedora customization script. It publishes **one image for every laptop**. It must be a stable, efficient general-purpose system that is also capable of gaming, pursuing better frame pacing and performance than stock alternatives without making unsafe global tuning the default. Hyprland is the primary desktop, niri is selectable, Rime Shell is the native UI, and `rimed` plus the `rime` CLI own system policy.

The single image replaced three flavors (Daily, Gaming Mesa, Gaming NVIDIA) that were one operating system with different payloads bolted on. The split forced a choice at install time that nobody has the information to make: someone with a gaming laptop who plays now and then had to pick, and picking Daily left them with no GPU driver. A technical line, rather than a market segment, now decides where a component lives:

**A kernel module cannot be installed at runtime under Secure Boot; userspace can.** Making a module loadable under an enforcing kernel requires signing it with the Rime MOK private key, which is a CI secret and must never reach a user's machine (`rime-pkg` refuses kernels, modules and akmods for this reason). So CI builds and signs every out-of-tree kernel module (the NVIDIA akmod and the xone/xpadneo controller akmods) into the image, and the gaming userspace (Steam, Proton, gamescope, MangoHud, Sunshine, OBS) installs on demand via `rime install`. Do not move anything across that line for convenience, in either direction: baking the userspace in undoes the reason the editions collapsed, and a module deferred to runtime cannot be made to load.

The installed system must remain image-based, transactional, reproducible, rollback-capable, Secure-Boot-capable, SELinux-enforcing (on the bootc images, until they sunset; the NixOS side has no SELinux and holds the security gates in `nix/tests/security.nix` instead), and traceable from an image digest to an exact source revision. Never accept a change that quietly turns a machine into mutable Fedora, creates machine drift, weakens verification, or adds an independent updater for image-owned components.

## Non-negotiable architecture

- `/usr` is image-owned and read-only at runtime. Persistent mutable state belongs under `/etc`, `/var`, or the user's home as appropriate.
- OS updates use bootc and must remain atomic with a working previous deployment. The build vendors Rime Shell into the signed image, so UI and OS update and roll back together.
- User RPM installation uses the Rime system-extension engine. Never use or recommend `rpm-ostree install`, because a layered deployment blocks bootc upgrades. Flatpak remains preferred for sandboxed desktop apps.
- `Containerfile.kernel` compiles the kernel from the inputs pinned in `kernel/kernel.pin`, and core consumes its RPMs by digest. `Containerfile.core` owns the Fedora base, the kernel install, package transactions, downloads, other third-party compilation, and every out-of-tree kernel module together with its MOK signature. Core is the only tier the signing secret is mounted in, and base consumes it by immutable digest, so a driver placed there is downloaded once and never again. `Containerfile.base` may copy repository content and compile first-party `rimed`; `Containerfile.rime` is the thin final tier that stamps the edition and owns the last initramfs regeneration. Moving volatile content into core causes multi-gigabyte fleet updates; moving a package transaction out of core into the volatile tiers puts an rpmdb-sized layer into every user's next update.
- CI pins the core and base parents by digest. Every published image is keyless cosign-signed, carries the source SHA, contains a signed kernel, and is promoted only after hard verification.
- Runtime sysprofiles select hardware-specific behavior. Device-risky tuning must never leak into generic profiles. Unsupported hardware must degrade safely, not fail boot.
- D-Bus API compatibility matters: the CLI and shell consume `org.rimeos.Rimed1`. Treat member, path, type, and semantic changes as public API changes.

## Review standard

Trace behavior across files and lifecycle stages: build, first boot, normal runtime, update, rollback, offline operation, and failure recovery. Flag fail-open checks, ignored failures, unpinned mutable inputs, shell quoting bugs, unsafe privilege boundaries, SELinux omissions (bootc) or a broken security gate (NixOS), secrets, destructive operations without validation, update-size regressions, edition leakage, and behavior that works only on the developer's machine.

Require tests or executable assertions for every repaired bug and every destructive, privileged, parser, policy-selection, or release-path change. Assertions must test the shipped artifact or externally visible behavior, not a duplicate implementation. Preserve one logical change per commit and Conventional Commit messages. Never add AI attribution to commits, PR text, release notes, or source files.

Documentation and comments must state measured facts and current behavior. Reject stale claims, aspirational language presented as implemented, and unexplained workarounds. Prefer small, explicit changes over compatibility layers or speculative abstractions.

## Image, kernel, signing, and release rules

- Preserve the tiered BUILD model: `kernel` → `core` → `base` → `rime`. It exists for download size, and the collapse of the editions did not change it. `kernel` compiles only when `kernel/**` changes. `core` is slow-moving and a rebuild makes the next fleet update multi-gigabyte, so only actual core inputs, an upstream digest change, or an explicit force operation may rebuild it. Each tier must consume its parent by immutable digest. Do not reintroduce a per-edition tier; there is one image and one final tier.
- Package installs, network downloads, third-party builds, and out-of-tree kernel modules belong in `Containerfile.core` (the kernel compile itself lives in `Containerfile.kernel`); repository-owned files and first-party `rimed` compilation belong in `Containerfile.base`. Never hide a package transaction in a volatile tier: every `dnf` transaction rewrites the ~200 MB sqlite rpmdb into its own layer, and above `core` that layer ships to every machine on every update.
- Pin every external action, image, package source, kernel/driver pair, and shell checkout, or resolve it once and carry it forward immutably. A long build must not pick up a newer Rime Shell commit halfway through.
- `rime` is the canonical published tag, and `daily`, `gaming-mesa` and `gaming-nvidia` must keep resolving to the SAME digest forever. They are no longer editions; they are live references owned by other people's laptops. A tag that stops being updated does not error: `bootc upgrade` reports "no update available" indefinitely and that machine stops receiving security updates without a word. CI asserts by reading each tag's digest back out of the registry, not by trusting that the promotion command exited zero. Platform tags remain stable inputs for shell-only releases. Promotion must be serialized, must not let an older job overwrite a newer image, and must occur only after verification and signing.
- Secure Boot is a product invariant. The expected chain must sign the shipped kernel AND every out-of-tree module, and each must have a verification step that reads the signature out of the built artifact: `sbverify` for the kernel image, `modinfo -F signer` for modules. Marker files alone are not proof, and neither is a check that iterates over a set that can be empty: assert a non-zero count. **The built artifact is whatever the firmware loads.** The day a UKI becomes the boot object, signing and `sbverify`-ing the inner `vmlinuz` proves nothing: the kernel is signed, the assertion is green, and the thing that boots is unsigned. The signature and its verification move to the UKI in the same change that produces one. Never commit private keys. CI gets signing material only through secrets or ephemeral identity, and the secret must be mounted in whichever tier does the signing.
- Keep least-privilege workflow permissions. Treat `pull_request_target`, interpolation into shell, artifact extraction, cache restore, and code from forks as hostile-input boundaries. Never execute untrusted PR code with write tokens or secrets.
- Preserve OCI source-revision labels and cosign keyless signing identity. Fail closed when digest capture, signature verification, kernel verification, layer-prefix verification, or promotion preconditions are unavailable.
- Keep shell-only releases small. They must inherit platform blobs unchanged and reject excessive new compressed data; recompressing inherited layers creates a full-fleet download.
- Build out-of-tree akmods against the exact shipped kernel and hard-fail on version skew; building them in the same tier that installs the kernel makes skew impossible, where a check would only detect it. Mesa and NVIDIA now ship together on every machine and must keep coexisting through GLVND: neither vendor may own `libGL.so.1` (libglvnd does), and an AMD-only or Intel-only laptop must not regress because the NVIDIA userspace is present. Assert it against the built image.
- Never mask a meaningful command failure with `|| true`. A tolerated upstream defect requires a narrow exception followed by a hard postcondition proving the required artifact exists and is valid.
- For workflow changes, inspect trigger/path-filter consequences. A check required on every PR must always report, including documentation-only PRs and skipped matrix paths.

## Runtime, desktop, and edition rules

- Rime Shell is image-vendored at `/usr/share/rime-shell`; do not restore per-user clones or an independent in-app updater. Runtime-generated shell/config state belongs under the user's home or `/var`, never the read-only source tree.
- Hyprland is primary and niri remains selectable. Greeter, session startup, lock screen, portals, polkit agent, audio, networking, notifications, and shell IPC must work without GNOME dependencies or a network fetch at first login.
- Branding is chartreuse; `VARIANT_ID=rime` is the one edition. The greeter still recognises `daily` and `gaming` so a machine that has not yet updated past the split keeps its accent, and `/etc/rime-greet/edition` still overrides per machine. Do not hardcode an identity into shared runtime paths.
- The image prioritizes stability, battery life and suspend reliability, because every laptop runs it, including the ones that never game. Risky tuning stays behind an explicit `rime` action (game mode, tiers, modes), never in the default boot. A desktop entry for software that installs on demand must be gated on that software being present (`TryExec`), or the user is offered a session that bounces them back to the greeter.
- On the bootc images, preserve SELinux enforcing until they sunset. New daemons, D-Bus services, polkit actions, privileged helpers, writable paths, device access, and systemd units require correct ownership, permissions, labeling, and the narrowest policy. Do not solve AVCs by disabling enforcement or granting broad access.
- On NixOS there is no SELinux: nixpkgs builds systemd without it, and Rime's own policy modules confined none of Rime's daemons (`rimed` and `rime-secretd` ran `unconfined_service_t`). The same review applies without the labels: correct ownership, permissions, the narrowest systemd sandboxing (measured with `systemd-analyze security`), polkit actions that need a human, and the agent sandbox's default-deny mounts. `nix/modules/security.nix` and `nix/tests/security.nix` hold the gates (PLAN.md D4, G1-G16); a change that makes one fail, or that removes the mutant proving it can fail, is a security regression.
- Systemd units must have correct ordering, restart behavior, shutdown cleanup, and enablement. Avoid boot-critical dependencies on network availability. Optional hardware/services must fail without degrading the boot transaction.
- Shell scripts run with their declared interpreter. Quote expansions, handle spaces/newlines where relevant, use atomic replacement for persistent configuration, and do not overwrite user edits without saying so. `/etc` defaults and provisioned user files need explicit ownership semantics.
- Default applications and MIME handlers must remain valid after install and update. Rebuild cached schemas/icons/MIME/dconf data after copying image-owned files, with postconditions checking the effective result.
- Added third-party runtime software belongs in core, not base. Decide explicitly whether an application is image-owned, Flatpak, or user system-extension content; do not create another package/update mechanism.

## Agent runtime rules

- `rime-agentd` is unprivileged, per-user, and must stay that way. It handles untrusted model output and spawns arbitrary user programs, so its worst case must remain a user-session compromise. Never move agent orchestration into `rimed`, never give the daemon a polkit action, a system-bus name, or a setuid helper, and never let it call `rimed` on a session's behalf. A session that needs a system change is served by the user's own `rime` invocation over the frozen `org.rimeos.Rimed1` surface.
- The daemon is opt-in. Do not `systemctl --global enable` the user unit: a per-user daemon holding PTYs must not start for users who never run an agent.
- `rime-secretd` is where credentials live, and it is the one privileged piece of the agent stack. It runs as root because the store must be unreadable by the user's uid and the git operation must run as the user; it drops to the owner for every child and never gains anything. Do not move a credential back into `rime-agentd`, do not give the protocol a verb that returns a value, and do not add a `Serialize` impl to `SecretValue`: the compile error is the acceptance criterion. Do not bind its socket into the sandbox: brokered use goes through `rime-agentd`, the only component that knows which session is asking. The store, the grant table and the audit trail all live behind that boundary, so a session cannot widen or rewrite any of them.
- The sandbox is default-deny and fails closed. `$HOME` and `$XDG_RUNTIME_DIR` are masked and only an explicit allowlist is bound back; the environment is cleared and rebuilt from a declared list. A confined session that cannot be confined as requested must not start: never downgrade a policy without saying so, and never widen the allowlist to make a tool work without saying why in the code. Treat every new entry as a credential-exposure question: `~/.ssh`, `~/.gnupg`, `~/.aws`, browser profiles and agent sockets must stay unreachable.
- `bubblewrap` is a security dependency satisfied indirectly (it arrives with flatpak) and asserted in `Containerfile.base`. Keep the assertion. If the indirect source ever goes away, add the package to core deliberately rather than deleting the check.
- Session state detection must not invent certainty. Only a published event ever sets `permission_request`; the runtime never infers it from terminal output. Do not add pattern matching over an agent's prose to guess intent. Inferred states come from documented terminal signals (bell, OSC 9/777, OSC 133), idle time and exit status.
- Upstream agent CLIs are launched unmodified in a real PTY. Never wrap, patch, proxy or reimplement them, and never make the runtime a prerequisite for running `claude`, `opencode`, `codex` or `gemini` directly.
- Checkpoints must not disturb the user's git state. Capture goes through a temporary index (`GIT_INDEX_FILE`) so the index, stash and branch are untouched; checkpoint refs live under `refs/apex/` (kept through the rebrand: existing checkpoints are there), never `refs/heads/`. Restore takes a safety checkpoint first. Ignored files stay out of checkpoints, and package changes are reported, never removed without a word. <!-- rime-rename: keep -->
- The control protocol is a compatibility surface: the CLI and Rime Shell both parse `SessionInfo`. Treat field renames, removals and semantic changes the way you treat `org.rimeos.Rimed1` changes, and bump `PROTOCOL_VERSION` when a change is not backward compatible. Serialised requests and responses must never contain a raw newline, because the framing is line-based.

## Editing a live machine's configuration

These rules exist because breaking them destroyed the developer's desktop. A
single `re.S` in a one-line substitution deleted 217 of 256 lines from his live
`hyprland.conf`, taking every `exec-once` with it: next reboot, no shell, no
wallpaper daemon, no polkit agent, no clipboard, no input method.

1. **Back up before touching a live config.** Keep a `.pre-<change>` copy
   alongside it. That copy turned the outage into a two-minute restore.
2. **`re.M`, never `re.S`, for a line-oriented edit.** With `re.S` a trailing
   `.*$` matches to the end of the FILE, not the end of the line. If you need a
   multi-line match, bound it explicitly, never with `.*$`.
3. **Assert the line count is unchanged** before writing a config whose shape
   you do not intend to change, and assert the substitution count is exactly
   what you expected. One `assert` turns a silent truncation into a refusal.
4. **Grepping for what you added cannot detect what you deleted.** Verify with
   a size or line count AND a landmark that must still be present: for
   hyprland.conf, `grep -c exec-once`. A truncated config is still a *valid*
   config, so `Hyprland --verify-config` and `hyprctl reload` both report `ok`.
5. **`hyprctl reload` re-reads the config; it does not re-run `exec-once`.**
   After you restore one, the services it starts are still dead. `hyprctl
   dispatch exec` restarts them parented to the compositor, as `exec-once`
   does.
6. **Do not use `pgrep -f <pattern>` to decide whether something is running.**
   It matches your own shell's command line, which produced five consecutive
   false "running" results during that incident. Use `pgrep -x`, or
   `ps -eo comm=`, remembering that `comm` truncates to 15 characters, which
   is how `polkit-mate-authentication-agent-1` reads as `polkit-mate-aut`.

## Touching a machine's boot path

`/usr` being read-only and `bootc rollback` existing make most mistakes on an
Rime box recoverable. The boot path is the exception: there is no rollback for
an ESP you overwrote or an EFI variable you replaced, because the thing that
would perform the rollback is what you broke.

The katana is a development machine running Rime (`VARIANT_ID=gaming`,
composefs root, GRUB 2.12, TPM2 present, Secure Boot disabled). It is also the
build box. Bricking it does not cost an afternoon; it costs every remaining
phase.

1. **Boot v2 work runs against guest ESPs only.** On any real Rime host
   (laptop or katana), never run `bootctl install`, `bootctl update`,
   `bootupctl`, `grub2-install`, `grub2-mkconfig -o` against a live
   `/boot/grub2/grub.cfg`, or `efibootmgr -c`/`-B`/`-o`. Never write under
   `/boot`, `/boot/efi` or `/efi`.
2. **A VM's ESP is a loopback-mounted image file**, under a scratch directory
   or `/var/lib/rime/bootlab/`, never the host's. Mount it, write it, unmount
   it; if a command needs a `--esp-path`, pass the mountpoint explicitly rather
   than letting the tool discover the host's.
3. **`ukify`, `qemu`, `mkosi` and `virt-install` are build-time tooling, not
   host software.** They arrive in a capsule or a system extension, never
   through `rpm-ostree install`, and never as an ad-hoc `dnf` on a host: that is
   the machine drift this contract prohibits, and a build box is where it is
   most tempting. This rule is about a RUNNING machine, not about the image: a
   tool baked into the image by `Containerfile.core` is the opposite of drift,
   and `systemd-ukify` is there because `bootc container ukify` has to run
   inside a build derived from that tier.
4. **No script in this repo generates Secure Boot keys on, or writes them to, a
   real machine's firmware.** Enrollment is a documented, explicitly
   user-initiated path. CI and VMs get ephemeral keys; a private key never
   reaches the repository.
5. **Rime is moving to systemd-boot on every machine (Andre's decision,
   2026-09-20), and the reasons GRUB was kept did not evaporate with it.** They
   are now the conditions the pivot has to satisfy, and the design that
   satisfies them is `docs/boot-v2.md`, "The pivot to systemd-boot". In short:

   * It is a **storage-backend change**, not a bootloader flag. On the ostree
     backend `bootc install --bootloader systemd` is refused outright
     (`bootupd is required for ostree-based installs`), because bootupd ships
     only a grub2+shim payload and ostree's entries and kernels live on btrfs,
     which sd-boot cannot read. Only `--composefs-backend` works. `bootc --help`
     lists no migration verb, but **`bootc install to-existing-root
     --composefs-backend --bootloader systemd` converts a running machine in
     place** (measured 2026-09-21), and `rime update` is what runs it. It is a
     reinstall for nobody.
   * **The migration commits at exactly one write, and everything before it is
     discardable.** Run bare, that bootc command deletes `/EFI/fedora` (the
     file the machine's own NVRAM entry points at), overwrites
     `/EFI/BOOT/BOOTX64.EFI`, and wipes the root filesystem's `/boot`. So the
     install runs with a throwaway FAT filesystem bound over `/target/boot`,
     the migration saves the fallback and puts it back, and the commit is a
     single `BootNext`: a one-shot the firmware consumes before the loader
     runs, so if the new path does not come up, the old one follows with nobody
     doing anything. `files/system/libexec/rime-boot-migrate`, proven with the
     power cut: `ROADMAP/evidence/sdboot-migrate-20260921-lab.md`.
   * **GRUB is demoted, never removed.** A machine that cannot migrate safely
     refuses and keeps working: Secure Boot with an unsigned loader, an ESP
     too small for two deployments, an update already staged. A refusal is a
     safe outcome; a half-migration is not.
   * **Nothing in the image may install a bootloader**, and that is unchanged:
     no `bootctl install`, `bootctl update`, `bootupctl`, `grub2-install` or
     `efibootmgr -c`, and `tests/test-boot-v2.sh` scans every shipped unit and
     helper for those commands on executable lines. There is no rollback for
     an ESP a script overwrote. Enrolment stays the documented, user-initiated
     procedure.
   * **Per-machine configuration is systemd credentials on the ESP.** Under a
     signed UKI the initramfs is inside the PE and the cmdline cannot differ
     per machine. `systemd-stub(7)`: with Secure Boot on and a `.cmdline`
     section present, the stub ignores a loader-supplied command line. Addons
     are validated against db/Shim MOK, and Rime's key is a CI secret, so a
     machine cannot make one. LUKS is found by GPT partition type GUID, not
     named by `rd.luks.uuid=`.
   * **The blessing must work or nothing else matters.** `systemd-bless-boot`
     could not rename a loader entry on a FAT ESP under enforcing SELinux
     (measured, with the AVC, in
     `ROADMAP/evidence/sdboot-image-20260920-decision.md`). Unrepaired, every
     deployment rolls itself back on its fourth boot. Any change that touches
     `systemd-bless-boot.service`, the `rime_sdboot` policy module or
     `rime-boot-count` is touching that.
   * **systemd-boot is UEFI-only.** `bootc install` creates a 1 MiB BIOS boot
     partition on every install and never writes anything into it (the L16's
     `bootupd-state.json` records only the `EFI` component), so the pivot drops
     a configuration that was created but never functional. The live ISO does
     boot legacy BIOS and is not affected.

   Until the five gates at the end of that document's pivot section are met, no
   real machine is migrated. (There is one image; `daily`, `gaming-mesa` and
   `gaming-nvidia` are tags on its digest, so there is no per-edition exception
   to find here.)
6. **A container that runs `bootc install` against anything loop-backed must
   pass `--generic-image`.** That is the whole rule, and it is the only thing
   here that prevents the 2026-09-20 breakage. bootc's own help: *"Changes to
   the system firmware will be skipped."* A loopback image is never this
   machine's firmware's business.

   Launch every `bootc install to-disk --via-loopback` through
   `tests/lab/bootc-install-lab`. It builds the podman argv itself, puts
   `--generic-image` in it, and **refuses to launch without it** (assertion
   `generic-image-present`, exit 7). It also refuses a block-device or tmpfs
   target, and runs the whole thing under `tests/lab/nvram-guard`. For a path
   that is not a literal `--via-loopback` (`installer/rime-install` handing
   bootc a loop device it attached itself), the engine's own
   `set_nvram_args_for` adds the flag: the same requirement, in a different
   file.

   **Which layer is which, because the first version of this rule got it
   wrong.** `--generic-image` is PREVENTION. `nvram-guard` is DETECTION: a
   before/after `efibootmgr -v` plus efivarfs `Boot*` digest diff that fails
   the run if the host's boot entries moved. It cannot stop a write, and it is
   the only layer that has ever caught this, twice. Wrap anything that touches
   a loopback install or boots a lab guest in `nvram-guard -- <command>`; it is
   read-only and costs nothing.

   **The efivars tmpfs is DEFENCE IN DEPTH and does not prevent this.** Keep
   it, because it bounds a process that stays inside the container, but do not
   describe it as the guard. `bootc` takes `--pid=host` and re-enters the
   HOST's mount namespace for the bootloader step (`strings /usr/bin/bootc`:
   `nsenter`, `/proc/1/ns/mnt`, `/proc/1/root`), and an unmasked privileged
   container shows *zero* entries under `/sys/firmware/efi/efivars` anyway, so
   the container never had a host efivarfs in view to mask.

   What it cost to learn that: on 2026-09-20 at 09:02 an install with no
   `--generic-image` had bootupd delete `Boot0000 Rime OS` and recreate it
   against the ESP inside the image file being built; Andre's laptop would not
   boot and was repaired from a live USB. The tmpfs was added the same evening
   and called the fix. At 21:53 a second install ran **with the mask applied
   and its promise printed** (*"this machine's UEFI boot entries are masked
   off and will not be touched"*) and moved `Boot0000` again four log lines
   later. `nvram-guard` caught that one when the command returned and restored
   the exact bytes from its own before-snapshot. Neither run contained the
   string `--via-loopback`; both were `rime-install` on a `to-filesystem`
   install against a loop device.

   `tests/test-bootc-install-guard.sh` holds both halves open for both layers:
   it proves each flag reaches podman in the position that matters, and it
   proves the launch is refused when either is removed from a copy of the
   wrapper. It does not reproduce the damage to do so: the claims are about
   the argv and the refusal. See BOOT-BREAKAGE-2026-09-20.md.
