# Other desktops, Rime Settings, quick toggles and power profiles on NixOS (design)

Status: **piece 1 (desktops) built on `nix/desktops`; as built in §12. Piece 2 (Rime Settings, quick toggles) not
started.** Andre (2026-11-03): *"with the rime nix release there should also be first party
support for other desktop environments, gnome, kde etc, if a user installs them they can instantly use them. and make
the rime settings an actual app for them to control when theyre not running an environment that has rime shell"*;
*"gaming mode and those important things will show up in like gnome quick access too. all power profiles in desktops
should use the rime power profiles"*; *"dont ask me any questions figure it out yourself"*. Every choice below was
made here and carries its reason. Two build pieces follow from it (§10).

Facts marked **(checked)** were read in rime-os `df8a25051` (`feat/nixos`), rime-shell `90f4d49a` (`feat/nixos`), the
pinned nixpkgs `1c89083` (source `/nix/store/cm5m4hjjh37s6z82rwa0vhkbcga3m8bq-source`), or measured on 2026-11-03 in
the `rime-nix` container (Nix, `--max-jobs 2 --option cores 4`, substitutes from cache.nixos.org, no local compiles).
The measurement inputs and outputs are in `/var/lab/rime-nix/artifacts/desktops/` (`measure.nix`, `measure-sizes.txt`,
`dry-*.log`, `unit-diff.txt`, `unit-content-diff.txt`, `opts.json.txt`, `toplevels.txt`). Facts marked
**(not verified)** were not, and each has a test in §8.

Contents:

1. [Decisions in one table](#1-decisions-in-one-table)
2. [Which desktops, and the one-entry mechanism](#2-which-desktops-and-the-one-entry-mechanism)
3. [How a desktop reaches a machine](#3-how-a-desktop-reaches-a-machine)
4. [Coexistence rules](#4-coexistence-rules)
5. [Rime Settings, the app](#5-rime-settings-the-app)
6. [Quick toggles in GNOME, KDE and COSMIC](#6-quick-toggles-in-gnome-kde-and-cosmic)
7. [Power profiles: rimed serves the power-profiles-daemon API](#7-power-profiles-rimed-serves-the-power-profiles-daemon-api)
8. [Tests and mutants](#8-tests-and-mutants)
9. [Disk floors and the installer](#9-disk-floors-and-the-installer)
10. [Work split, order and shared files](#10-work-split-order-and-shared-files)
11. [What is not proven here](#11-what-is-not-proven-here)
12. [As built (piece 1, desktops)](#12-as-built-piece-1-desktops-branch-nixdesktops)

---

## 1. Decisions in one table

| # | Decision | Reason (details in the section named) |
|---|---|---|
| D1 | GNOME 50.4, KDE Plasma 6.8.2 and COSMIC 1.11.0, from nixpkgs' own modules. Xfce is the worked example of "one more entry", not shipped. | §2: the three are the desktops people ask for by name and nixpkgs maintains their modules; Xfce's Wayland session is experimental. |
| D2 | A desktop is a **catalogue feature** (class 1, like `gaming`), turned on by a switch file. The release prebuilds every variant. No local build, ever. | §3.1: the variant machinery, the signed manifest and the cache already exist and are tested (`checks.update-e2e` installs `plain-gaming` live from the same release). |
| D3 | **One other desktop at a time.** Variants = {plain, gaming, virtualization, both} x {none, gnome, kde, cosmic} = 16, linear in desktops (+4 per desktop), not 2^n. | §3.2: two extra desktops on one machine are rare and cost the union closure; the honest cost of "any combination" is 32 variants today and doubling per entry. |
| D4 | `rime install gnome` installs the **same release's** GNOME variant from the saved, re-verified manifest, live, without a reboot; it never moves the channel. `rime remove gnome` does the reverse. | §3.4: adding a desktop must not slip in a kernel update. |
| D5 | The activation classifier learns: `systemd-udevd.service` and `geoclue.service` are restart-safe (one VM test each), and a unit present in only one of the two systems may start or stop. | §3.5: measured: those are the only restarts a desktop install causes; without this every install degrades to "at next restart". |
| D6 | power-profiles-daemon is **not run**. rimed owns `org.freedesktop.UPower.PowerProfiles` and `net.hadess.PowerProfiles` and maps them 1:1 to its tiers. system76-power is forced off. | §7: GNOME, KDE, COSMIC and Quickshell all read that API; rimed already writes platform_profile/EPP itself, so PPD today is a second writer of the same files. |
| D7 | Rime's session-only environment (`QT_QPA_PLATFORMTHEME=qt6ct`, `ELECTRON_OZONE_PLATFORM_HINT`) moves out of `environment.sessionVariables` into Rime's own sessions. | §4: measured: COSMIC does not even evaluate with it global, and it would theme Plasma with qt6ct. |
| D8 | The greeter stays the display manager for every desktop; it passes `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP` and `XDG_SESSION_DESKTOP` from the session file. GDM and SDDM stay off. | §4.1. |
| D9 | Rime Settings is a second Quickshell config in the rime-shell tree (`settings.qml` beside `shell.qml`), a `FloatingWindow` (xdg_toplevel) with app id `org.rimeos.Settings`, single instance, reusing the Shell's pages that do not need Hyprland. Inside a Rime session its launcher opens the Shell's Nexus instead. | §5: GNOME has no layer-shell; the pages import relatively, so the entry must live in the same tree. |
| D10 | Quick toggles: Gaming (Daily/Gaming), Update (available/apply/restart) and an "Open Rime Settings" entry, in a GNOME Shell Quick Settings extension, a Plasma 6 system-tray plasmoid and a COSMIC panel applet. Power profiles are NOT duplicated there: each desktop's native power UI drives rimed (D6). | §6. |
| D11 | rimed gains two additive D-Bus surfaces the toggles need: `Mode.Set(s)` + `Mode.Active`, and an `org.rimeos.Rimed1.Updates` interface. Every property the toggles read emits `PropertiesChanged`. | §6.2: no CLI polling; one implementation of "set the mode" instead of three copies of `rime mode set`'s composition. |
| D12 | gnome-keyring stays the one Secret Service in every desktop; KWallet's Secret Service API is turned off. | §4.4: two owners of `org.freedesktop.secrets` was the bootc keyring lesson. |
| D13 | The installer does **not** offer a desktop. It says in one line that GNOME, KDE Plasma and COSMIC can be added later. | §9: smallest install, no new installer matrix, and adding one later is live anyway. |
| D14 | Floors per desktop variant = measured base + measured desktop delta (the model `floors.nix` already uses for virtualization), published per variant; no published variant may fall back to `floor_for`'s "largest contained floor". | §9: that fallback is a lower bound. |
| D15 | GNOME's remote desktop, rygel, user share, initial setup, Software, Tour and Web are off; Plasma's Discover is excluded. | §4: network sharing surfaces stay off on a machine with rime-firewall; Rime is the app store (`rime install`, Flatpak). |

---

## 2. Which desktops, and the one-entry mechanism

**In scope (checked versions at the pin):** GNOME (gnome-shell 50.4, `services.desktopManager.gnome`), KDE Plasma 6.8.2
(`services.desktopManager.plasma6`), COSMIC 1.11.0 (`services.desktopManager.cosmic`). nixpkgs maintains these modules
and their sessions; Rime adds policy, never a fork of the module.

**The mechanism.** A new file `nix/desktops/default.nix` is a table, one entry per desktop:

```nix
gnome = {
  title = "GNOME";
  summary = "GNOME 50 desktop (GNOME Shell, Files, Settings)";
  aliases = [ ];                       # catalogue names that also mean this desktop
  sessions = pkgs: [ pkgs.gnome-session.sessions ];   # what rime.desktop.extraSessions gets
  sessionIds = [ "gnome" ];            # must equal the packages' passthru.providedSessions (checked by eval)
  desktopNames = [ "GNOME" ];          # XDG_CURRENT_DESKTOP; must match the session file's DesktopNames
  config = { pkgs, lib, ... }: {       # applied under mkIf rime.desktops.gnome.enable
    services.desktopManager.gnome.enable = true;
    services.gnome.gnome-initial-setup.enable = false;
    services.gnome.gnome-remote-desktop.enable = false;
    services.gnome.rygel.enable = false;
    services.gnome.gnome-user-share.enable = false;
    environment.gnome.excludePackages = with pkgs; [ gnome-tour epiphany gnome-software ];
  };
};
kde = { title = "KDE Plasma"; aliases = [ "plasma" ]; sessions = pkgs: [ pkgs.kdePackages.plasma-workspace.sessions ];
        sessionIds = [ "plasma" ]; desktopNames = [ "KDE" ];
        config = { pkgs, ... }: { services.desktopManager.plasma6.enable = true;
                                  environment.plasma6.excludePackages = [ pkgs.kdePackages.discover ]; }; };
cosmic = { title = "COSMIC"; sessions = pkgs: [ pkgs.cosmic-session ]; sessionIds = [ "cosmic" ]; desktopNames = [ "COSMIC" ];
           config = { lib, ... }: { services.desktopManager.cosmic.enable = true;
                                    services.system76-scheduler.enable = lib.mkForce false; }; };
```

`nix/modules/desktops.nix` turns the table into options `rime.desktops.<id>.enable`, applies each entry's `config`
under `mkIf`, appends `sessions` to `rime.desktop.extraSessions`, and applies the shared rules of §4 when any entry is
on. `nix/catalogue/catalogue.nix` reads the same table: each entry becomes a feature (`option = [ "rime" "desktops"
<id> "enable" ]`, `offered = true`, `group = "desktop"`) and a class 1 toggle `<id> = { kind = "toggle"; feature =
<id>; }` plus one toggle per alias. `nix/release/variants.nix` and `floors.nix` read it too (§3.2, §9).

**Why sessions go through `rime.desktop.extraSessions`:** `desktop.nix` sets `services.displayManager.sessionPackages =
lib.mkForce ([ rimeSessions ] ++ cfg.extraSessions)` **(checked, desktop.nix:405)**, so a desktop module's own
`sessionPackages` never reaches the greeter. Measured: with the entry's `sessions` in `extraSessions`, the session
names are `hyprland niri rime-labwc rime-safe-graphics gnome` (resp. `plasma`, `cosmic`) **(checked, opts.json.txt)**.

**Adding a desktop is one entry plus one measured number** (its closure delta, §9; `checks.release-plan` refuses an
offered desktop without one). Xfce, as the worked example:

```nix
xfce = { title = "Xfce"; sessions = pkgs: [ (pkgs.writeTextDir "share/wayland-sessions/xfce-wayland.desktop" ''
           [Desktop Entry]
           Name=Xfce (Wayland)
           Exec=startxfce4 --wayland
           DesktopNames=XFCE
         '' // { providedSessions = [ "xfce-wayland" ]; }) ];
         sessionIds = [ "xfce-wayland" ]; desktopNames = [ "XFCE" ];
         config = { ... }: { services.xserver.desktopManager.xfce = { enable = true; enableWaylandSession = true; }; }; };
```

nixpkgs has `enableWaylandSession` ("the experimental Xfce Wayland session", labwc) **(checked, xfce.nix:85)**. The
greeter only lists `wayland-sessions` (§4.1), which is why the example uses that session; an X11-only desktop would
also need an X11 launch path in the greeter, which this design does not add. Xfce is not shipped because upstream
calls its Wayland session experimental.

---

## 3. How a desktop reaches a machine

### 3.1 Features, switch files and variants (what exists)

`rime install steam` writes `/var/lib/rime/switch/rime-feature@gaming.target.on`; the switch files are the only input
for which variant a machine installs; `variant::choose` takes the largest subset of the switched-on features the
release builds and reports the rest as waiting; `checks.update-e2e` proves a machine switching gaming on and the
**same release** installing `plain-gaming` live **(checked, update-e2e.nix "the switch files pick the variant")**. A
desktop uses exactly this. No local evaluation or build happens on the machine: the release manifest names the
variant's toplevel and every path's narHash, the client substitutes them, binds each narHash, sets the profile and
activates.

Feature ids are one word (`gnome`, `kde`, `cosmic`) so variant names stay readable: `plain-gaming-gnome`,
`plain-gnome-virtualization`. Names are the sorted features joined by `-` on both sides (variants.nix `name`,
variant.rs `name`) **(checked)**; nothing parses a name back into features.

### 3.2 Combinatorics (honest)

Features offered after this work: gaming, virtualization, gnome, kde, cosmic.

| Rule | Variants | Growth |
|---|---|---|
| any combination (today's "every subset") | 2^5 = 32 | doubles per desktop |
| **one other desktop at a time (chosen)** | 4 x 4 = **16** (12 new) | +4 per desktop |

`variants.nix` builds every subset that holds at most one member of each `group` (here only `desktop`). `rime install
kde` on a machine with GNOME switched on refuses by name: "GNOME is installed; Rime adds one other desktop at a time:
`sudo rime remove gnome` first". No desktop is ever removed implicitly.

**`choose()` priority.** With full coverage no release lacks a combination, but a later release can drop a desktop.
Today's tie-break is size, then lexical order, so a machine with gaming + virtualization + gnome on a release without
`plain-gaming-gnome-virtualization` would get `plain-gaming-gnome` and park virtualization (its VMs gone because a
desktop was added). New rule: among the largest subsets, prefer the one that keeps every switched-on non-`desktop`
feature; a desktop is the thing that waits. Unit test: exactly that triple, release has `plain-gaming-gnome` and
`plain-gaming-virtualization` → picks `plain-gaming-virtualization`, `gnome` waiting.

### 3.3 Measured size and CI cost

Closures (NAR bytes, from the store DB, `measure-sizes.txt`) **(checked)**:

| Toplevel | Paths | Closure | Delta vs its base |
|---|---|---|---|
| plain | 1,863 | 10.43 GB | |
| plain + GNOME | 1,985 | 11.54 GB | +166 / -44 paths, **+1.111 GB** |
| plain + KDE Plasma | 2,206 | 12.86 GB | +378 / -35 paths, **+2.424 GB** |
| plain + COSMIC | 1,919 | 12.95 GB | +85 / -29 paths, **+2.520 GB** |
| plain + all three (not offered, D3) | 2,359 | 16.26 GB | +5.829 GB |
| plain-gaming | 2,164 | 12.63 GB | |
| plain-gaming + GNOME | 2,285 | 13.74 GB | +1.111 GB (same as on plain) |
| plain-gaming + KDE | 2,507 | 15.06 GB | +2.424 GB (same as on plain) |

The desktop delta is the same on plain and on gaming (to 0.07 MB), so floors can add it (§9). The three desktops share
almost nothing new (GNOME/KDE: 15 paths, 0.20 GB).

Download on a machine that has plain (dry run against the local store, which holds plain): GNOME 111 paths, **264.6
MiB download / 881 MiB unpacked**; KDE 329 paths, **880.6 MiB / 2.2 GiB**; COSMIC 52 paths, **902.3 MiB / 2.3 GiB**
**(checked, dry-*.log; an upper bound for the store paths not already present locally, so treat as approximate)**.

**What Rime must build and publish per desktop variant** (outputs of the derivations the dry run would build that are
in the closure): GNOME 49 paths 51.4 MB, KDE 40 paths 60.2 MB, COSMIC 31 paths 33.4 MB, gaming+GNOME 55 paths 80.5 MB
**(checked)**. They are NixOS glue (system-path, X11-fonts, hwdb.bin, fontconfig, units, etc). Every desktop package
comes from cache.nixos.org: the GitHub cache backend serves only Rime-built paths and the manifest binds narHash for
all of them (updates-and-release.md §18) **(checked)**. A full release adds 12 variants x 30-80 MB of glue, about
0.7 GB unpacked, and roughly 600 narinfos/assets; the publish tool already rolls over at GitHub's 1,000 assets per
release (§18) **(checked)**.

**Build time:** each variant evaluates in 5-6 s; building a toplevel whose inputs are cached took 39 s (GNOME), 89 s
(KDE) and 158 s (COSMIC) on katana including the downloads, 14-16 s for a second variant of the same desktop
**(checked, build-all.txt)**. Estimate for the release `build` job: +15 min for 12 variants.

**Disk on the build runner:** the union of plain, plain-gaming, the three desktops, gaming+GNOME, gaming+KDE and all
three is **2,913 paths, 18.80 GB** **(checked)**; with virtualization (+0.65 GB) and COSMIC on gaming, about 19.5 GB.
A hosted runner has about 14 GB free (§18, not verified) and plain-gaming alone is 12.6 GB, so **desktop variants
build on the D8 self-hosted builder only**. If the build job falls back to a hosted runner, it builds the four
non-desktop variants, the release manifest omits the desktop variants, and machines with a desktop switched on are
told it is waiting (the existing waiting message); the release is not blocked. The publish job is unaffected: it
handles only Rime-built paths.

### 3.4 `rime install <desktop>` and `rime remove <desktop>`

`rime install gnome` (root; refuses without, like `rime update`):

1. Catalogue lookup: `gnome` (or alias) → feature `gnome`, group `desktop`.
2. Another desktop switched on → refuse by name (§3.2). Nothing written.
3. Floor: the exact floor of the variant it would select (§9), against the filesystem holding `/nix`; refuse below it.
4. Write `rime-feature@gnome.target.on` (`gnome` on one line, as the existing toggles do).
5. **Same-release install.** Read the running release from `/var/lib/rime/releases/<id>/` (`save_release` writes
   `manifest.json`, `digest`, `verified` there, update.rs:632 **(checked)**). If that manifest names the target
   variant: run `os_step` with `Pointer::Take { digest: <saved>, release: <running> }` instead of `fetch::channel`.
   The manifest is fetched again **by that digest** and verified again (`fetch::manifest`, the trust gate), so a
   tampered local file proves nothing; the channel is never read, so no newer release slips in. Then the usual plan →
   headroom → fetch → bind → profile → boot entry → classify → live switch.
6. If the running release builds no such variant (an old release, or a hosted-fallback release, §3.3): keep the switch
   and print the existing message: selected; `sudo rime update` installs it when a release carries it.
7. Success says: "GNOME is installed. Log out and choose GNOME at the login screen." The greeter reads
   `/etc/rime/wayland-sessions/*.desktop` when it starts **(checked, GreetContext.qml)**; the next logout starts it
   again, so the session is there with no reboot.

`rime remove gnome`: delete the switch file, then the same-release install of the variant without it. If a session of
that desktop is live (`loginctl` sessions whose `Desktop` is one of the entry's `sessionIds`), the new system is
installed **for the next boot only** and the message says so ("you are logged in to GNOME; it is removed when you
restart"). Otherwise it is live. User files (`~/.config`, `~/.local/share`) are left alone and the message says that
too. Store space returns when the old generation is pruned by Rime's existing generation policy, not at once.

`rime remove` of a desktop that is the remembered greeter session: the greeter already ignores a remembered session
that is no longer installed **(checked, GreetContext.qml "A remembered session wins — WHILE IT IS STILL INSTALLED")**.

### 3.5 Live, not "at next restart": the classifier

`switch-to-configuration test` runs only when the classifier says Switch. Rule 3 forces Boot for any restart outside
`RESTART_SAFE_UNITS = [rimed.service, rime-remoted.service, polkit.service]` or reload outside `[dbus-broker.service]`
**(checked, activation.rs:19-24, 228-242)**. Measured unit diffs between plain and each desktop (content, not store
path, `unit-content-diff.txt`) **(checked)**:

| | system units whose content changes | added | user units |
|---|---|---|---|
| GNOME | dbus-broker (reload, safe), polkit (safe), **systemd-udevd**, **geoclue** | accounts-daemon, colord, gnome-remote-desktop (off by D15), configure-printer@ | dbus-broker; 72 added (gnome-session, gsd-*) |
| KDE | dbus-broker, polkit, **systemd-udevd** | accounts-daemon, drkonqi-coredump-processor@, configure-printer@ | dbus-broker, nixos-activation (changed), nixos-rebuild-sycoca (added) |
| COSMIC | dbus-broker, polkit | accounts-daemon | dbus-broker; cosmic-session.target, cosmic portal |

udevd restarts because its restart trigger is the rules directory (`udev.nix:535`, `stopIfChanged = false`)
**(checked)**; geoclue because its trigger names its config. Kernel, initrd, modules and kernel parameters are
identical across all variants **(checked, artifacts boot-facts.txt)**, so rule 1 never fires.

Changes (desktops piece):

- `RESTART_SAFE_UNITS` += `systemd-udevd.service` (a udevd restart keeps every device; it re-reads rules) and
  `geoclue.service` (a location daemon; clients reconnect). One unit, one VM test each, as the file's own rule says.
- **Units present in only one of the two systems** may start (install) or stop (remove) without forcing Boot.
  `update.rs` lists `etc/systemd/{system,user}` of both toplevels and passes the symmetric difference to `Inputs`;
  rule 3 ignores stops of those. Without it, `rime remove gnome` stops accounts-daemon and is Boot-class.
- The KDE user unit `nixos-activation.service` change: measured by the VM test; if dry-activate restarts it, it joins
  the user-unit allowlist with its own test.
- **Rule 6 and non-Rime sessions.** Rule 6 defers when a session is locked or its lock state is unknown and the switch
  would restart the Shell. A GNOME session has no Rime Shell, so "unknown" there must not count: only sessions that
  run Rime Shell (the Shell IPC answers, or a quickshell process with the shell path) are asked. Without this every
  Shell update defers while anyone is logged in to GNOME.

---

## 4. Coexistence rules

| Area | Rule | Why / evidence |
|---|---|---|
| Display manager | greetd + rime-greet for every desktop; GDM/SDDM off. | One login path, one PAM stack, Rime's recovery watchdog. plasma6 configures SDDM but does not enable it; gdm/sddm are false in every variant **(checked, opts.json.txt)**. |
| Session env | rime-greet passes `XDG_SESSION_TYPE=wayland` always, and `XDG_CURRENT_DESKTOP` (DesktopNames, `;` → `:`) and `XDG_SESSION_DESKTOP` (session id) when the file has DesktopNames, through Quickshell's `Greetd.launch(command, environment)` (the overload with a `KEY=VALUE` list exists in quickshell 0.3.2 **(checked, Greetd qmltypes)**). | It launches `["sh","-lc",cmd]` with no environment today **(checked, GreetContext.qml launch())**; gnome-session and portal selection need them. The greeter files are shared with bootc, so the change is additive. Rime's own files: hyprland.desktop has no DesktopNames (gets `XDG_SESSION_TYPE` only); niri, labwc and gaming have `DesktopNames=niri`/`labwc`/`gamescope` **(checked)**, values those sessions already set themselves. The test asserts each Rime session's environment inside the session is the same with and without the change, apart from `XDG_SESSION_TYPE`. |
| Rime-only env | `QT_QPA_PLATFORMTHEME=qt6ct` and `ELECTRON_OZONE_PLATFORM_HINT=auto` move from `environment.sessionVariables` (desktop.nix:646) into Rime's sessions (the session wrappers Hyprland, niri and labwc start from). | COSMIC does not evaluate otherwise: `environment.variables.QT_QPA_PLATFORMTHEME has conflicting definition values: shells-environment.nix "qt6ct", qt.nix "qt5ct"` **(checked)**. KDE evaluates but would export qt6ct globally and theme Plasma with it **(checked, opts.json.txt)**. `sh -lc` sources /etc/profile, so anything global reaches every desktop. |
| GTK defaults | `/etc/xdg/gtk-{3,4}.0/settings.ini` and the dconf `prefer-dark`/Adwaita default stay. | Defaults only; GNOME's Appearance panel and KDE's GTK settings write user values that win. |
| Cursor | Nothing to move: no cursor theme or size is exported globally (no `XCURSOR_*` in `nix/modules/` **(checked)**); each desktop's own cursor settings apply. | |
| Power | PPD off, rimed serves its API (§7); `hardware.system76.power-daemon.enable = mkForce false`; COSMIC's system76-scheduler forced off. | COSMIC's module turns system76-power on when PPD and tuned are off and its applet prefers `com.system76.PowerDaemon` **(checked, cosmic module and applet)**; system76-scheduler would fight rimed's scx/cpuset policy. KDE powerdevil stays (it is the KDE power UI and reads the PPD API). |
| Portals | Upstream portal config per desktop (`configPackages`), Rime's `xdg.portal.config` only for hyprland/labwc/niri **(checked)**. GNOME → gnome+gtk, KDE → kde+gtk(+kwallet), COSMIC → cosmic+gtk, chosen by `XDG_CURRENT_DESKTOP`. | Needs the greeter env row above. |
| Polkit agent | Rime's mate-polkit is started only by Rime's session.lua and its autostart file is `OnlyShowIn=MATE` **(checked)**. GNOME Shell, polkit-kde-agent and cosmic-osd are the agents in theirs. | No duplicate prompts. |
| Secret Service | gnome-keyring (unlocked by greetd's PAM hook) is the one `org.freedesktop.secrets` everywhere. KDE: `/etc/xdg/kwalletrc` `[org.freedesktop.secrets] apiEnabled=false`; no `pam_kwallet` on greetd (plasma6 adds it to `login` only, not greetd **(checked)**; **wrong**: greetd's PAM substacks `login`, so it ran at every greetd login; as built, Rime turns it off in `login`, §12.1). KDE apps using the KWallet API get a wallet that asks once. kwallet-pam's `pam_kwallet_init.desktop` autostart has no ShowIn **(checked)** and will exist in every session once KDE is installed: ship an override with `OnlyShowIn=KDE;`, and the VM tests assert no kwalletd process and one secrets owner (gnome-keyring) in a Hyprland login on a KDE variant. | Two owners of the name was the bootc keyring failure (two daemons, one locked). |
| Network/Bluetooth applets | nm-applet is already `NotShowIn=KDE;GNOME;COSMIC` **(checked)**. blueman's autostart has no ShowIn **(checked)**: ship an override `/etc/xdg/autostart/blueman.desktop` with `NotShowIn=GNOME;KDE;COSMIC;`. | Each desktop has its own NM and Bluetooth UI; two Bluetooth agents fight over pairing. |
| Audio | PipeWire + WirePlumber as today; every desktop is a PipeWire client. | Nothing to change. |
| Input method | fcitx5 everywhere (Rime's `i18n.inputMethod` wins over GNOME's ibus default: `im = fcitx5` in every variant **(checked)**). GNOME: ship the kimpanel extension enabled; KDE: KWin's input method set to fcitx5 (`kwinrc [Wayland] InputMethod`). ibus-daemon must not run. | One input method, Rime's. |
| Screen lock / idle | Each desktop's own locker. Rime's hypridle and Rime lock screen start only from Rime sessions. | They are started by session.lua, not autostart. |
| Notifications / OSD | Rime Shell owns notifications in-process; no session D-Bus activation file for `org.freedesktop.Notifications` exists in the system profile **(checked: none in sw/share/dbus-1/services)**, so gnome-shell/plasmashell/cosmic-notifications own it in theirs. `rime-update-notice` and the GPU/storage notices use `notify-send` and work in any desktop. | |
| Rime Shell | Never autostarted outside Rime sessions: it is started by session.lua / labwc autostart / niri config only **(checked)**. `rime-shell-firstrun` (user, default.target) only seeds files and stays. | |
| Lid | rime-lid's `handle-lid-switch` block inhibitor stays. KDE powerdevil and GNOME settings-daemon may handle the lid themselves; the VM test records `systemd-inhibit --list` per desktop. If a desktop suspends anyway, "keep working with the lid closed" is a documented limit in that desktop, not silently broken. | **(not verified)** |
| Sharing surfaces | D15: GNOME remote desktop, rygel, user share off; Discover and GNOME Software out. | rime-firewall machines expose nothing by default; Rime installs apps. |

**What works inside GNOME/KDE/COSMIC:** `rime` CLI, updates (notices, quick toggle, Settings app), Daily/Gaming mode,
game mode and gamemoded, power tiers through the desktop's own power UI, battery charge limit (Settings app),
firewall, agents (`rime-agentd`), remote (`rime-remoted`), Flatpak, `rime install`, keyring, fcitx5. **What does not:**
the Shell's surfaces (bar, notch, launcher, Nexus, plugins), Hyprland keybinds and window rules, `rime-display-apply`
(Hyprland monitors), the Rime lock screen, live Shell swaps (nothing to swap). Gaming Mode the *session* stays its own
greeter entry.

---

## 5. Rime Settings, the app

**Shape.** `settings.qml` beside `shell.qml` in rime-shell, launched as `quickshell -p <shellDir>/settings.qml` with
`QS_APP_ID=org.rimeos.Settings` (quickshell 0.3.2 reads `QS_APP_ID` and has `-n/--no-duplicate` **(checked, binary
strings)**). Its root is a `FloatingWindow` (xdg_toplevel; the Shell already uses FloatingWindow in
`BatteryWarning.qml` **(checked)**) hosting `SettingsHost` with a page list. It must live in the same tree: the pages
import relatively (`"../../../"`), and the Shell's path contract hashes the config path string (rime-shell.nix
header) **(checked)**, so the app's path is `/run/current-system/sw/share/rime-shell/settings.qml`, a separate instance
from the Shell.

**Why not Nexus as is:** Nexus is a layer-shell `PanelWindow`; GNOME (Mutter) has no wlr-layer-shell, so it cannot
show there.

**Launcher:** `rime-settings [page]` (shipped by the rime-shell package):
- inside a Rime session (the Shell instance answers `qs -c <shellDir> ipc`): open the Shell's Nexus at `page`;
- else, if the app instance runs: `qs -p <settings.qml> ipc call settings open <page>` (raise + switch page);
- else start it. Single instance by `--no-duplicate` plus the IPC call.
`org.rimeos.Settings.desktop`: `Exec=rime-settings %u`, `Icon=org.rimeos.Settings`, `Categories=Settings;System;`,
`StartupWMClass=org.rimeos.Settings`, `X-GNOME-UsesNotifications=true`. GNOME's Settings app list and KDE's launcher
show it.

**Pages.** Kept: Updates, Gaming, Power (new: tiers, auto switch, battery charge limit, from `org.rimeos.Rimed1.Power`
and `.Battery`), Desktops (new, also in the Shell's Nexus: install/remove GNOME, KDE Plasma, COSMIC with size and
floor from `/etc/rime/catalogue.json`), Lid, Privacy, Firewall, Agents, Remote pairing, Remote devices, Data,
Recovery, Blueprint, About. Shell-only (listed by `PageRegistry` with a `shellOnly` flag, hidden in the app):
Appearance, Layout, Display, Input, Keybinds, Misc. The settings piece audits every kept page: a page that runs
`hyprctl` or reads Hyprland IPC is either guarded or moved to shell-only; the test proves no `hyprctl` call happens.

**Privileged actions** go through rimed over D-Bus where rimed has the method (polkit `allow_active=yes` for
power/battery/mode, no prompt), and through `pkexec rime <verb>` otherwise (install/remove desktop, apply update, firewall
changes), with a polkit action `org.rimeos.rime.system` (`auth_admin_keep`, `exec.path` = the system `rime`). The
desktop's own polkit agent shows the prompt.

**Packaging:** the rime-shell package already ships the tree; it gains `bin/rime-settings`, the `.desktop` file and an
icon. It installs from `feat/nixos`'s rime-shell input in every system (also Rime-only ones, where it opens Nexus).

**Shell power service.** `PowerProfileService.qml` polls `org.rimeos.Rimed1 Tier` with busctl every 4 s and sets via
`rime tier` **(checked)**. Once rimed serves the PPD API (§7), it uses Quickshell's own
`Quickshell.Services.UPower.PowerProfiles` (`profile`, `hasPerformanceProfile`, `degradationReason`, `holds`
**(checked, qmltypes)**): event-driven, no polling. It falls back to the busctl path when the bus name has no owner
(a machine on an older rimed), so the two pieces can land in either order.

---

## 6. Quick toggles in GNOME, KDE and COSMIC

### 6.1 What they show

- **Gaming**: on/off = `Mode.Set("gaming"|"daily")`; subtitle "Game running" while `GameMode.Active`.
- **Update**: hidden when up to date. "Update to <release>" → `Updates.Apply()` (admin prompt by the desktop's agent);
  while installing, progress text; after a boot-class install, "Restart to finish".
- **Open Rime Settings** (`gtk-launch org.rimeos.Settings` / `KIO::ApplicationLauncherJob` / COSMIC launch).
- **Not** a power profile toggle: each desktop's native power UI drives rimed (§7), so a second control would only
  disagree with it.

All three read and write rimed on the system bus only and follow `PropertiesChanged`; a change from the CLI, the
Shell or another desktop shows at once.

### 6.2 rimed D-Bus additions (desktops piece; contract fixed here so the settings piece can build against it)

On `org.rimeos.Rimed1`, `/org/rimeos/Rimed1`, all additive (the frozen interfaces keep every existing member):

- `org.rimeos.Rimed1.Mode`: + property `Active` (s, `daily`|`gaming`, derived as `rime mode status` derives it),
  + method `Set(s mode)` doing server-side exactly what `rime mode set` composes today (Hold first, then
  SetAutoSwitch/SetTier/GameMode.SetActive), polkit `org.rimeos.rimed.manage-power` (allow_active yes). `rime mode set`
  then calls `Set` (its `RIME_MODE_NO_APPLY` guard stays client-side and first). `Held` and `Active` emit
  `PropertiesChanged`.
- `org.rimeos.Rimed1.GameMode.Active` emits `PropertiesChanged` (the `ActiveChanged` signal stays).
- New `org.rimeos.Rimed1.Updates`: properties `State` (s: `up-to-date|available|downloading|installing|deferred|restart|failed|unknown`),
  `Available` (s, release id or ""), `Running` (s), `Message` (s); methods `Check()` (starts `rime-update-check.service`,
  allow_active yes) and `Apply()` (starts a new `rime-update.service` = `rime update`, polkit
  `org.rimeos.rimed.apply-update`, `auth_admin_keep`). rimed fills it from `/run/rime-update/state` and
  `/run/rime-live/status.json` (the files `rime update` writes, mod.rs:36-38 **(checked)**) by inotify.

### 6.3 Where they live and how they are enabled (settings piece, rime-os `nix/integrations/`)

| Desktop | Form | Enabled by | Version guard |
|---|---|---|---|
| GNOME | Extension `rime@rimeos.org` (GJS ES module, `QuickSettings.SystemIndicator` + `QuickToggle`/`QuickMenuToggle`, `Gio.DBusProxy` on the system bus) in `share/gnome-shell/extensions/` | system dconf default `org/gnome/shell/enabled-extensions = ['rime@rimeos.org', 'kimpanel@kde.org']` in the GNOME entry | `metadata.json` `shell-version` = the major of `pkgs.gnome-shell.version` (50), asserted at eval: a nixpkgs bump to 51 fails the build until the extension is checked |
| KDE Plasma 6 | Plasmoid `org.rimeos.quicksettings` (pure QML, `org.kde.plasma.workspace.dbus` `SystemBus`/`Properties`/`SignalWatcher`/`asyncCall`, present in plasma-workspace 6.8.2 **(checked, dbusplugin.qmltypes)**) | `X-Plasma-NotificationArea=true`, `X-Plasma-NotificationAreaCategory=SystemServices`, `X-KDE-PluginInfo-EnabledByDefault=true`: shown in the default panel's system tray | `X-Plasma-API-Minimum-Version: 6.0` |
| COSMIC | Applet `cosmic-applet-rime` (Rust, libcosmic applet API, zbus) | a panel default listing it, placed earlier in `XDG_DATA_DIRS` than cosmic-panel's own default | built against the pinned libcosmic; the applet test runs on each bump |

---

## 7. Power profiles: rimed serves the power-profiles-daemon API

**Why.** Every desktop's power UI talks to PPD's API: gnome-shell's power-profiles menu reads
`org.freedesktop.UPower.PowerProfiles` (`ActiveProfile`, `Profiles` by key `Profile`, `g-properties-changed`; knows
performance/balanced/power-saver), KDE powerdevil does GetAll + PropertiesChanged on the same name and uses
HoldProfile/ReleaseProfile, COSMIC's battery applet uses it when `com.system76.PowerDaemon` is absent **(checked,
sources at the pin)**. Rime's tier ids are already exactly `performance`, `balanced`, `power-saver` **(checked,
tier.rs)**. rimed writes platform_profile and EPP itself (tier.rs, perf.rs, syswriter.rs) **(checked)** and does not
call PPD at all, while `desktop.nix:554` enables PPD "so rimed layers its policy on it": today two daemons write the
same sysfs files. Turning PPD off and serving its API from rimed removes the second writer and makes every desktop's
control a Rime control.

**API served (PPD 0.30, the pinned version; member list read from the binary)** **(checked)**:

`org.freedesktop.UPower.PowerProfiles` at `/org/freedesktop/UPower/PowerProfiles` and `net.hadess.PowerProfiles` at
`/net/hadess/PowerProfiles` (the legacy name without `SetActionEnabled`, `ActionsInfo`, `BatteryAware`):

| Member | rimed behaviour |
|---|---|
| `ActiveProfile` (s, rw) | read: the effective tier (a hold wins, below). write: polkit `org.freedesktop.UPower.PowerProfiles.switch-profile` (allow_active yes), then exactly `Power.SetTier` (same code path; auto-switch is left as it is, so unplugging still follows Rime's AC/battery rule, as `rime tier` does today; with auto-switch on the tier written is the user's pick for the power source the machine is on, kept across AC changes and restarts, `choice.rs`), and **releases every hold** with `ProfileReleased(cookie)` for each, as PPD does. Unknown id → InvalidArgs. |
| `Profiles` (aa{sv}) | one dict per tier the machine has (`Power.Tiers`): `Profile`, `Driver` = `rimed`, `PlatformDriver`, `CpuDriver` (what rimed writes: `platform_profile`, `intel_pstate`/`amd_pstate`). Never `Driver = placeholder` (KDE treats that as "no profiles"). |
| `PerformanceDegraded` (s) | "" or PPD's reasons (`high-operating-temperature`, `lap-detected`) when rimed's own thermal/lap state says performance is capped. |
| `PerformanceInhibited` (s) | "" (deprecated upstream; present for old clients). |
| `HoldProfile(s profile, s reason, s application_id) → u` | polkit `...hold-profile` (allow_active yes); only `performance` or `power-saver`. Holds are tied to the caller's unique bus name and dropped on `NameOwnerChanged`. Precedence as PPD: **any power-saver hold beats every performance hold**. While a hold is active the effective tier is the held one; when the last hold goes, the tier returns to the one chosen before. A release that leaves the holds asking for the same tier writes nothing; a hold never records a tier pick. |
| `ReleaseProfile(u)` | releases the caller's own cookie; `ProfileReleased(u)`. |
| `ActiveProfileHolds` (aa{sv}) | `ApplicationId`, `Profile`, `Reason` per hold. |
| `Actions`, `ActionsInfo`, `SetActionEnabled` | `[]`, `[]`, error NotSupported: rimed's own policy replaces PPD's actions. |
| `BatteryAware` (b, rw) | mapped to `Power.AutoSwitch` (polkit `...configure-battery-aware`). |
| `Version` (s) | `0.30`, so clients gating on version behave as with the pinned PPD. |
| `PropertiesChanged` | for every property above, from every path that changes the tier (CLI, Shell, AC change, gaming, holds). |

**Gaming Mode** takes an internal hold of `performance` (ApplicationId `org.rimeos.GameMode`, reason "Gaming Mode"),
visible in `ActiveProfileHolds`. A desktop writing `ActiveProfile` while it is active sets the tier the machine
returns to and re-emits `ActiveProfile = performance`; the game keeps its tier. Ending game mode drops the hold. This
is the one place rimed differs from PPD (PPD would drop every hold on a write); a game losing its tier because someone
clicked the panel is worse than a menu that snaps back.

A **`power-saver` hold beats Gaming Mode** as it beats every other `performance` hold (decided at the wave-5
integration, §12.8): while one stands, `ActiveProfile` and the machine are `power-saver`; every other game mode lever
(cpuset, IRQs, GPU locks, sched-ext, fans) stays, `ActiveProfileHolds` lists both holds, and `Mode.Active` stays
`gaming` (Gaming is still on; only its tier is overridden). When the hold goes the game's tier comes back; when game
mode ends first the hold still stands and the machine stays at `power-saver` until it goes. The usual holder is a
desktop's low-battery saver (gnome-settings-daemon takes one), and a battery running out mid-game is the case PPD's
rule exists for.

**Packaging.** rimed ships `share/dbus-1/system.d/org.freedesktop.UPower.PowerProfiles.conf` and
`net.hadess.PowerProfiles.conf` (own: root; send: everyone), `system-services/*.service` with
`SystemdService=rimed.service`, and `polkit-1/actions/org.freedesktop.UPower.PowerProfiles.policy` with the four
action ids. `services.power-profiles-daemon.enable = lib.mkForce false` and an eval assertion that no
power-profiles-daemon path is in `system.path` and no other package installs those bus names' system.d files.

---

## 8. Tests and mutants

Every VM test boots no more than two VMs at once on katana (L16 when katana load > 30) and deletes its disks.

### 8.1 Desktops piece (rime-os `nix/desktops`)

| Check | Proves | Mutants that must fail it |
|---|---|---|
| `checks.desktops-eval` (eval only, every published variant via `nix/ci/eval-each.sh`) | 16 variants evaluate; no PPD/system76-power path in any; `QT_QPA_PLATFORMTHEME` absent from `/etc/set-environment`; each desktop's sessionIds present in `sessionData`; desktops never pairwise in a variant; kernel/initrd/params identical to plain | PPD left on; QT var left global (COSMIC eval fails); a variant with two desktops generated |
| rimed unit tests (private system bus, zbus) for §7 | every row of the §7 table: write → SetTier path; write releases holds + `ProfileReleased`; power-saver beats performance; hold dies with its bus name; gaming hold survives a write; `Profiles` never placeholder; PropertiesChanged on AC change; legacy name answers the same | write not releasing holds; precedence swapped; no NameOwnerChanged cleanup; no PropertiesChanged from the AC path |
| `powerprofilesctl` (in the VM below) | `powerprofilesctl list/get/set`, `launch -p performance sleep 5` (hold seen, released) against rimed | — |
| variant.rs/pkg.rs unit tests | choose() keeps non-desktop features (the triple in §3.2); second desktop refused by name; floor per desktop variant exact | lexical tie-break restored; refusal removed |
| activation.rs unit tests | udevd/geoclue restarts are Switch; stops of units only in the old system are Switch; a non-Rime session does not defer rule 6 | each allowlist entry removed |
| `checks.desktops-install-gnome` (VM, release stand-in from update-e2e: server + client) | client on plain runs `rime install gnome` against a release with `plain` and `plain-gnome`; a NEWER release is on the channel and must NOT be installed; installs live (`/run/booted-system` unchanged, `/run/current-system` = the gnome toplevel); log out; greeter lists GNOME (screenshot); log in through rime-greet as the test user (correct password only); gnome-shell runs, screenshot; no quickshell/rime-shell, mate-polkit, blueman-applet, hypridle, ibus-daemon, power-profiles-daemon, system76-power process; exactly one owner of `org.freedesktop.secrets` (gnome-keyring); `XDG_CURRENT_DESKTOP=GNOME`; GNOME's power-profiles property = rimed's tier after `rime tier performance`; `systemd-inhibit --list` recorded; `rime remove gnome` while logged in → next boot only; log out, `rime remove gnome` → live, session gone from `/etc/rime/wayland-sessions`, accounts-daemon stopped | same-release path reading the channel (installs the newer release); greeter env not passed (record what breaks; if nothing does, the test asserts the env var instead); udevd not allowlisted (install becomes "at next restart"); blueman override removed |
| `checks.desktops-install-kde`, `-cosmic` | the same sequence, smaller: install, greeter, login, screenshot, process/bus-owner assertions, powerdevil reads rimed's profile, remove. COSMIC: `com.system76.PowerDaemon` has no owner and the battery applet shows rimed's tier | system76-power left on; kwallet secrets API left on (two owners) |
| `checks.release-plan` (existing, extended) | every published variant has its own floor; desktop floors = base + measured delta; figures in §9 | a desktop variant without a floor |

### 8.2 Settings piece (rime-shell `nix/settings-app`, rime-os `nix/settings-pkg`)

| Check | Proves | Mutants |
|---|---|---|
| rime-shell `tests/run-settings-app-test.sh` (headless labwc like the existing launcher tests) | `rime-settings` maps one xdg_toplevel with app id `org.rimeos.Settings` (wlr-foreign-toplevel); a second launch maps no second window and switches page; Shell-only pages absent; a `hyprctl` stub on PATH is never called while every page is opened | `QS_APP_ID` dropped; `--no-duplicate`/IPC dropped; Display page left in |
| rime-shell `tests/run-power-profile-service-test.sh` (fake PPD on a private system bus) | the Shell's power control follows `PropertiesChanged`, sets via `ActiveProfile`, falls back to Rimed1 when the name is unowned | fallback removed; polling reintroduced (counted calls) |
| GNOME extension: `gjs` test against a fake rimed on a private bus | toggle state follows `Mode.Active` changes made elsewhere; click calls `Mode.Set`; Update item hidden/shown by `Updates.State` | PropertiesChanged ignored |
| plasmoid: `qmltestrunner` with the same fake rimed | same | same |
| COSMIC applet: Rust unit tests (zbus, fake rimed) | same | same |
| `checks.settings-app-gnome`, `-kde` (VM, desktop enabled directly in the test config; installing it is the desktops piece's test) | log in through rime-greet; `gtk-launch org.rimeos.Settings` → window, screenshot; change a real setting through the app's own handler (an IPC test hook that runs the same function the control runs): Power → performance → rimed `Tier` = performance, `powerprofilesctl get` = performance, the desktop's own power property agrees; extension ACTIVE (`gnome-extensions info`) / plasmoid in the tray config; `rime mode set gaming` from a shell → the toggle's state changes (read through the extension's/plasmoid's test hook) | app id wrong (`.desktop` match fails); extension `shell-version` wrong (not ACTIVE) |
| `checks.settings-app-cosmic` | applet process in cosmic-panel, screenshot, reflects `Mode.Set` | panel default not found |

---

## 9. Disk floors and the installer

Computed with `floors.nix`'s own formula (store = on-disk + on-disk estimate of a whole second closure + 1 GB reserve
+ 1 GB home; ratio 0.55; ESP 2 GiB), the desktop's measured NAR delta added to the measured base exactly as
virtualization's is (`withVirtualization`). The script reproduces the published figures of the time it was run
(16/18/17/19 disk, 14/16/14/16 partition) before adding anything **(checked, measure-sizes.txt)**. Since then wave 4
("updates2", in this branch's base) MEASURED the two virtualization bases instead of estimating them (smaller on disk:
plain-virtualization 5,564,084,224 B against the 5,800,374,254 B estimate), so their published figures are now 16/18
disk, 14/16 partition, and the virtualization rows below are the as-built `floors.nix` figures, which
`checks.release-plan` recomputes:

| Base | none | + GNOME | + KDE Plasma | + COSMIC |
|---|---|---|---|---|
| plain | 16 / 14 | **17 / 15** | **18 / 16** | **19 / 16** |
| plain-gaming | 18 / 16 | **19 / 17** | **21 / 18** | **21 / 19** |
| plain-virtualization | 16 / 14 | **18 / 15** | **19 / 17** | **19 / 17** |
| plain-gaming-virtualization | 18 / 16 | **20 / 17** | **21 / 19** | **21 / 19** |

(disk GB / partition GB). The deltas are `gnomeDelta = 1110685288`, `kdeDelta = 2424402376`, `cosmicDelta =
2519964376` bytes (NAR, measured 2026-11-03). They are estimates on disk (marked `estimated = true`, like
virtualization) until a VM measures one installed. `rime install gnome` compares with the exact floor of the variant
it selects; `floor_for`'s "largest contained floor" fallback stays only for variants a release does not publish.

**Installer: no desktop choice (D13).** It installs plain (or plain-gaming when gaming is chosen, as today) and shows
one line: "Other desktops (GNOME, KDE Plasma, COSMIC) can be added later in Rime Settings → Desktops." Reasons: Andre
wants the install as small as possible (2026-10-04 decision); adding one later is live and downloads 0.26-0.9 GB; and
the installer test matrix does not grow. The installer's disk check keeps reading the plain/gaming floors.

---

## 10. Work split, order and shared files

**Piece 1, desktops (rime-os, branch `nix/desktops`, worktree `/var/lab/rime-nix/wt-desktops`):** §2 table + module,
catalogue features/toggles, variants with exclusive groups, floors, `rime install/remove` for desktops (same-release
path), classifier changes, greeter env, Rime env moved into Rime sessions, PPD off + rimed PowerProfiles API (§7),
rimed `Mode.Set`/`Active` and `Updates` (§6.2), blueman override, kwalletrc, and the §8.1 tests.

**Piece 2, settings (rime-shell `nix/settings-app` from `feat/nixos` 90f4d49a in `/var/lab/rime-nix/wt-settings-app`;
rime-os `nix/settings-pkg` from `df8a25051` in `/var/lab/rime-nix/wt-settings-pkg`):** the app (§5), the Shell power
service migration, the Desktops page, and the three quick-toggle integrations in rime-os `nix/integrations/{gnome,kde,cosmic}`
(§6.3), with the §8.2 tests. Its VM tests need piece 1's rimed: merge `nix/desktops` into `nix/settings-pkg` once
piece 1's rimed D-Bus commit exists; until then its unit tests run against the fake rimed built from §6.2/§7.

**Shared files** (keep hunks minimal, rebase onto `feat/nixos` after the wave-4 branches land, before touching them):
`nix/modules/default.nix` (one import), `nix/modules/desktop.nix` (PPD line, sessionVariables move),
`nix/catalogue/catalogue.nix` (features read from the table), `nix/release/variants.nix`, `nix/release/floors.nix`,
`rimed/rime/src/nixos/{variant,pkg,update}.rs`, `rimed/rimed-core/src/nixrel/activation.rs`, `rimed/rimed/src/dbus.rs`,
`files/desktop/rime-greet/GreetContext.qml` (shared with bootc: additive only). Everything else goes in new files.

---

## 11. What is not proven here

Written with the design; what the build went on to prove, and what it still does not, is §12.

- Nothing was booted: GNOME, KDE and COSMIC under rime-greet, the greeter env, Quickshell as a plain xdg client under
  Mutter, the lid inhibitor under each desktop and every dry-activate prediction in §3.5 are for the VM tests.
- The dry-activate list is predicted from unit content diffs, not from `switch-to-configuration dry-activate` (not run
  here by rule: no activation inside the build container).
- Download sizes are against this container's store, which held more than plain; the per-variant "Rime-built" sizes
  are outputs of the derivations the dry run listed.
- Disk floors for desktop variants are estimates (NAR delta x 0.55), not an installed measurement.
- The hosted runner's free disk (14 GB) is the figure from updates-and-release.md §18, still not verified.

---

## 12. As built (piece 1, desktops; branch `nix/desktops`)

Built on `feat/nixos` c54c25060. Every design row of §2-§9 that belongs to piece 1 is built as written except where
this section says otherwise. Evidence (logs, screenshots) is in `/var/lab/rime-nix/artifacts/desktops/`
(`mutants/`, `vm/`).

### 12.1 What exists

| Area | Where | Notes |
|---|---|---|
| Desktop table, module | `nix/desktops/default.nix`, `nix/modules/desktops.nix` | `rime.desktops.<id>.enable`; one table drives the catalogue, variants, the greeter's session list and the eval check. |
| Variants and floors | `nix/release/variants.nix`, `nix/release/floors.nix` | 16 published variants, at most one desktop each; names list their features sorted (`plain-gaming-gnome`, `plain-cosmic-gaming`). Floors = measured base + measured NAR delta x 0.55, marked `estimated`. Published: plain 16, plain-gnome 17, plain-kde 18, plain-cosmic 19, plain-gaming 18, plain-gaming-gnome 19, plain-gaming-kde 21, plain-cosmic-gaming 21 (disk GB); the virtualization rows are §9's table (plain-gnome-virtualization 18/15, plain-gaming-gnome-virtualization 20/17): computed on the wave-4 MEASURED virtualization bases, which are smaller than the estimates the first draft of §9 used. |
| Catalogue | `nix/catalogue/catalogue.nix`, `rime/src/nixos/pkg.rs` | features carry `group = "desktop"` and `session_ids`; `variant::choose` prefers, among the largest variants a release builds, one that keeps every non-desktop feature. |
| CI | `.github/workflows/nix-release.yml` | The 16 closures (~19.5 GB) do not fit a hosted runner: the hosted job builds `rime-release-toplevels-hosted` (variants without a desktop). No job builds the desktop variants yet: the self-hosted builder that would build `rime-release-toplevels` is a TODO(handover) in the workflow (every job is `ubuntu-24.04`), so CI does not build the twelve desktop closures. `update-floors` installs only the variants `floors.nix` records as measured. |
| PPD off | `nix/modules/desktop.nix`, `nix/modules/desktops.nix` | `services.power-profiles-daemon.enable = mkForce false`, `hardware.system76.power-daemon.enable = mkForce false`, an assertion refusing either (and a second desktop). |
| Rime-only env | `nix/modules/desktop.nix` | `QT_QPA_PLATFORMTHEME=qt6ct` and `ELECTRON_OZONE_PLATFORM_HINT=auto` are exported by a one-line wrapper each of Rime's four sessions (Tiling, Scrolling, Floating, Safe Graphics) runs as its whole Exec, not globally. The Exec stays one word because `rime-session-select` (the Gaming Mode switch back) refuses an Exec with `=` in it, and the build refuses an Exec with a space. `rime-gaming` and `extraSessions` do not get them (§12.4). |
| Greeter env and command | `files/desktop/rime-greet/GreetContext.qml` | as §4; Rime's own sessions are named in `ownSessions` (hyprland, niri, rime-labwc, rime-safe-graphics, rime-gaming) and get `XDG_SESSION_TYPE` only, so their environment is byte-for-byte what it was. A desktop is recognised by DesktopNames, not by an allowlist of other desktops. The command is single-quoted for greetd (§12.6): `launch()` hands greetd `["sh", "-lc", '<Exec>']`. |
| Autostart overrides | `nix/modules/desktops.nix` | blueman `NotShowIn=GNOME;KDE;COSMIC;`, `pam_kwallet_init` `OnlyShowIn=KDE;`, `/etc/xdg/kwalletrc` secrets API off. |
| pam_kwallet (D12) | `nix/desktops/default.nix` | `security.pam.services.login.kwallet.enable = mkForce false` on KDE systems. plasma6 puts pam_kwallet in `login`, and greetd's PAM is `auth substack login` / `session include login`, so it ran at every greetd login: the first KDE run (`kde-3.log`) shows `ksecretd --pam-login` (KWallet 6.31's daemon; its test looked for the old name, kwalletd) in a Hyprland login. Plasma's screen locker (`kde` PAM service) keeps it; KDE apps using the KWallet API get a wallet that asks once, as D12 says. |
| App stores (D15) | `nix/desktops/default.nix` | GNOME Software and Discover excluded as designed; the COSMIC Store too (the COSMIC module adds `cosmic-store` whenever Flatpak is on; it is not one of its core packages). Found in the first COSMIC run (its dock showed the store). `desktops-eval` refuses any of the three in any variant. |
| install/remove | `rime/src/nixos/desktop.rs`, `update.rs` (`install_running_release`) | as §3.4. `remove` deletes the whole switch file (every alias that put the desktop there). The same-release path calls `os_step` with the saved digest and `allow_unverified = false`; the manifest is fetched again by digest and verified again. Outcomes: Live, NextBoot (a session of the desktop is open, or the classifier says Boot), NotInRelease, NewerStaged (a newer release is already staged: it is not replaced). |
| Classifier | `rimed-core/src/nixrel/activation.rs` | as §3.5: `systemd-udevd.service` and `geoclue.service` restart-safe; one-sided units may start/stop; rule 6 asks only sessions that run Rime Shell (conservative: a session whose Shell state is unknown still defers if it runs the Shell). Added once the VM test ran: `systemd-tmpfiles-resetup.service` restart-safe (a oneshot every switch starts; the second switch of a boot restarts it, so `rime remove gnome` after `rime install gnome` was "next boot"); the account lines of `update-users-groups.pl` (`would remove group/user ‘X’`, `would revive group/user 'X' with …`) are recognised; removing the account of a user with an open session is Boot. `warning: would not apply …` stays unrecognised. |
| Up to date means running | `rime/src/nixos/update.rs` `os_step` | Only `running == target` is up to date. Before, `booted == target` also counted, so install (live) → remove while logged in (next boot = the booted system) → log out → `rime remove` said "up to date" and left the desktop running. Found while writing the VM test; fixed in 5fafdace5. |
| PPD API | `rimed/src/ppd.rs` | as §7. Properties are served through a custom `org.freedesktop.DBus.Properties` so `ActiveProfile` writes run polkit and SetTier; `Power.SetTier` (CLI, Shell) also releases holds, as a PPD write does; an AC change emits PropertiesChanged; `PerformanceDegraded` carries rimed's thermal/lap reasons. `ReleaseProfile` checks no owner, as PPD does not. The names are claimed on every system (`ReplaceExisting | DoNotQueue`, the result logged); `RIMED_POWER_PROFILES=0` opts out (the NixOS module sets `1`). On bootc this is rime-os #139's one-writer rule: the image masks `power-profiles-daemon.service` and `org.rimeos.Rimed1.conf` lets rimed own both names; the action ids come from the power-profiles-daemon package's own policy there, and Containerfile.base fails the build if it stops defining `switch-profile`, `hold-profile` or `configure-battery-aware`. Bus policy `org.rimeos.Rimed1.PowerProfiles.conf`, activation files for both names (`SystemdService=rimed.service`), policy `org.rimeos.rimed.power-profiles.policy` (a unit test checks the shipped policies name exactly the action ids rimed asks for). |
| Mode.Set / Active | `rimed/src/dbus.rs`, `hold.rs`, `rime/src/mode.rs` | as §6.2. `rime mode set` calls `Mode.Set`; an older rimed (UnknownMethod) gets the old client path. |
| Updates | `rimed/src/updates.rs`, `nix/modules/update.nix` | as §6.2. The checker's `/run/rime-update/state` now carries `state=`, `digest=`, `release=` (on offer) and `running=`; rimed reads it and `/run/rime-live/status.json` by inotify. `Apply` (polkit `apply-update`, interactive, `auth_admin_keep`) starts the new `rime-update.service` (`rime update`, oneshot, 6 h timeout). The unit has `restartIfChanged = false` and `X-StopOnRemoval = false` (added in verification, §12.7; nixpkgs' `nixos-upgrade.service` does the same): `rime update` runs `switch-to-configuration` inside it, its ExecStart names rimed's store path, and switch-to-configuration-ng at the pin restarts a changed unit that is activating, so without it an Apply whose release changes rimed would be classified `boot` (rule 3) and a live switch would stop its own unit. With it the dry run prints `would NOT stop the following changed units: rime-update.service`, which the parser reads as kept (as for greetd). Read from the source at the pin, not observed in a VM. |

### 12.2 Tests and mutants (every mutant listed was observed failing the check, except the one the KDE row says survives; logs in `artifacts/desktops/mutants/`, the VM checks' in `artifacts/desktops/vm/mut-<desktop>-<mutant>.log`)

| Check | Mutants killed |
|---|---|
| `checks.desktops-eval` | ppd-on, qt-global, two-desktops; mF-restart, mG-stoprm (verification, §12.7) |
| `checks.release-plan` | drop-floor |
| rimed private-bus tests (`bus_tests.rs`, inside `path:.#rimed`'s check) | ppd-write-keeps-holds, ppd-precedence-swapped, ppd-no-owner-cleanup, ppd-ac-no-props, ppd-write-not-polkit, mode-set-no-hold, updates-no-watch, updates-apply-not-interactive |
| variant/pkg/desktop/update unit tests (`rime`) | variant-lexical-tiebreak, desktop-refusal-removed, desktop-floor-without-feature, onesided-no-guard |
| activation unit tests (`rimed-core`) | act-no-udevd, act-no-geoclue, act-no-onesided, act-no-template, act-rule6-all, act-no-tmpfiles-resetup, act-accounts-unrecognised, act-removed-user-logged-in-ignored |
| greeter (`tests/test-rime-greet-sessions.sh` §8 env, §9 command) | §8: see the VM row greeter-env; §9: greet-argv-unquoted (the old `["sh","-lc",cmd]`: 4 of 5 Exec shapes lose their arguments, `env …` prints the environment) |
| `checks.desktops-eval` (rows added while building) | cosmic-store (the COSMIC Store exclusion removed: the four COSMIC variants fail), kwallet-pam-login-eval (pam_kwallet back in `login`: the four KDE variants fail), kwalletrc-api-on (`apiEnabled=true`: the four KDE variants fail) |
| `checks.desktop` (VM) | the multi-word Exec this branch first shipped (`desktop-head.log`: the session exited at once) |
| `checks.desktops-install-gnome` (VM) | same-release-channel (`rime install gnome` printed `2026.11.10 → 2026.11.11` and activated the newer release's plain-gnome; the test refuses the system), udevd-not-safe (`mut-gnome-udevd-not-safe-2.log`: "needs a restart: rule 3: it would stop or restart systemd-udevd.service", GNOME installed for the next restart, the test wants it live; the first run, `mut-gnome-udevd-not-safe.log`, never booted: its anchor was stale, fixed in 9312d695a), greeter-env (GNOME never started: the login timed out at 300 s waiting for gnome-shell; this is what the missing environment breaks), blueman-override ("blueman-applet runs in the GNOME session") |
| `checks.desktops-install-kde` (VM) | kwallet-pam-login (`mut-kde-kwallet-pam-login.log`: "ksecretd runs in a Rime session"). Its first mutant, kwallet-secrets (KWallet's Secret Service API on), was killed before the pam fix (`mut-kde-kwallet-secrets.log`: ksecretd, started by pam_kwallet, owned `org.freedesktop.secrets`) and SURVIVES at 29e3fc0a5 (`mut-kde-kwallet-secrets-3.log`; the `-2` run failed only at a logout wait that kde-4 passes): with pam_kwallet out of `login` nothing starts ksecretd before gnome-keyring holds the name. It is no longer a VM mutant; `checks.desktops-eval` guards the setting (kwalletrc-api-on). |
| `checks.desktops-install-cosmic` (VM) | system76-power ("system76-power runs") |

#### What the VM runs showed

Logs and screenshots: `/var/lab/rime-nix/artifacts/desktops/vm/` (`<run>.log`, `shots-<run>/`).

- **KDE** (`kde-4.log`, PASS, 459 s, at 29e3fc0a5; screenshots `shots-kde-4/`): `rime install kde` while the user was
  logged in to Hyprland was live (`2026.11.10 → 2026.11.10`): the user's `nixos-activation.service` was restarted by the
  switch (new InvocationID, NRestarts 0) and the Hyprland socket and session survived. In a Hyprland login on the KDE
  system neither `kwalletd` nor `ksecretd` ran and the one `org.freedesktop.secrets` was gnome-keyring. The run before
  the fix (`kde-3.log`, PASS, 544 s) passed only because it looked for `kwalletd`: its process list shows
  `ksecretd --pam-login` in the Hyprland login (§12.1, pam_kwallet). In Plasma (both runs): logind `Desktop=plasma Type=wayland Service=greetd`; plasmashell's `XDG_CONFIG_DIRS`
  starts with plasma-workspace's own `etc/xdg`; secrets owner gnome-keyring; powerdevil's `currentProfile` followed
  `rime tier performance` then `power-saver`, `profileChoices` = `performance balanced power-saver`. Inhibitors in
  Plasma: PowerDevil holds `handle-power-key:handle-suspend-key:handle-hibernate-key:handle-lid-switch` (block, "KDE
  handles power events"); kwin, ModemManager, NetworkManager, rtkit and UPower hold sleep delays. Remove while logged
  in: next boot; after logout: live, session file gone.
- **GNOME** (`gnome-7.log`, PASS, 341 s, at the final test; `gnome-6.log` the same run one commit earlier;
  screenshots `shots-gnome-7/`): `rime install gnome` printed
  `2026.11.10 → 2026.11.10` and installed `plain-gnome` live while 2026.11.11 was on the channel. Greeter lists GNOME;
  the login gives logind `Desktop=gnome Type=wayland Service=greetd`, gnome-shell's `XDG_CURRENT_DESKTOP=GNOME`,
  `XDG_SESSION_DESKTOP=gnome`, `XDG_SESSION_TYPE=wayland`; secrets owner gnome-keyring; none of Rime's session
  programs, no power-profiles-daemon or system76-power. `rime tier performance` is what GNOME reads over the system
  bus; `powerprofilesctl list/get/set` and `launch -p performance` (hold seen, released) work against rimed.
  Inhibitors in GNOME: gsd-power `handle-lid-switch` (block, "External monitor attached or configuration changed
  recently") and a sleep delay "GNOME needs to lock the screen"; gsd-media-keys the power/suspend/hibernate keys
  (block); gnome-shell, ModemManager, NetworkManager, rtkit and UPower sleep delays. GNOME shows "Screen Lock
  disabled: Screen Locking requires the GNOME display manager" at login (`gnome-session.png`; §12.4), and nothing owns
  `org.gnome.DisplayManager` (`busctl status` in the log).
- **COSMIC** (`cosmic-3.log`, PASS, 509 s, with the COSMIC Store left out; `cosmic-2.log` the run before it;
  screenshots `shots-cosmic-3/`): installed live from the running release; logind `Desktop=cosmic Type=wayland
  Service=greetd`, `XDG_CURRENT_DESKTOP=COSMIC`; secrets owner gnome-keyring; `com.system76.PowerDaemon` has no
  owner and no system76-power process runs; rimed's `ActiveProfile` follows `rime tier power-saver`/`performance`.
  Inhibitors: cosmic-session holds the power key (block); cosmic-greeter, COSMIC's own locker, holds a sleep delay
  "COSMIC Greeter needs to display a lock screen"; COSMIC holds no lid inhibitor. The first login opens COSMIC
  Initial Setup with its screen reader on (orca runs as a child of cosmic-session; `cosmic-session.png`), in both
  runs: cause not found (§12.5, open-items). `cosmic-2`'s dock showed the COSMIC Store; `cosmic-3`'s does not.
- **`checks.desktop`** (`desktop-head.log` FAIL at the pre-fix head, `desktop-fix2.log` PASS): the wrapper exports
  both variables into Hyprland's own environment (read from `/proc/<Hyprland>/environ`), and every Rime session file's
  Exec is one word.

### 12.3 How the VM tests stand in for a release

`nix/tests/desktops-install.nix` uses the fixture publisher (`fixture-publish.py`, the same cache/channel layout
`rime update` reads, signed with the test CA) plus a multi-variant manifest writer, not update-e2e's full release
pipeline: the pipeline would build and push every variant of two releases (four toplevels per desktop here) through
the real signing path, which update-e2e already proves; what this test needs is two releases, each with plain and
plain-<desktop>, and a newer one on the channel. Release 1 (2026.11.10) is installed first through `rime update`;
release 2 (2026.11.11, marked by `/etc/rime-test-release`) is on the channel before `rime install`, so installing it
would be visible. The greeter's session is preselected through `/var/lib/rime-greet/last-session` (rime-greet's own
memory) and greetd restarted, because the test cannot pick a session with the keyboard reliably.

### 12.4 Decided not to

| What | Why |
|---|---|
| **No screen lock in GNOME under rime-greet.** GNOME Shell's screen shield needs GDM (the notification "Screen Lock disabled: Screen Locking requires the GNOME display manager" at login, `shots-gnome-7/gnome-session.png`; gnome-shell 50.4 creates the shield only when `LoginManager.canLock()`, which asks `org.gnome.DisplayManager`, answers **(checked, js/ui/main.js:237, js/misc/loginManager.js:38)**). Rime keeps greetd + rime-greet for GNOME too and ships no locker for it in this piece. | D8 (one login path, one PAM stack, Rime's recovery watchdog) outweighs it for now, and the alternatives each change more than this piece should: run GDM only for GNOME (two display managers), patch gnome-shell's check (a fork of a module, against D1), or start Rime's own lock from GNOME (not designed for Mutter, no layer-shell). Handed over (open-items, build:desktops): until it is fixed, nothing locks a GNOME session, and the release note that introduces desktops must say so. |
| **Rime's session-only environment only in Rime's four wrapped sessions** (Tiling, Scrolling, Floating, Safe Graphics). `rime-gaming` and `extraSessions` do not get `QT_QPA_PLATFORMTHEME`/`ELECTRON_OZONE_PLATFORM_HINT`. | Gaming Mode is gamescope + Steam (no Qt theme to follow); an extra session is someone else's. Before this piece every session got them globally, so Gaming Mode loses two variables it did not use. |
| **No activation allowlist entry for the user `nixos-activation.service` restart.** | `switch-to-configuration` restarts it in every user manager on a real switch, but its dry-activate returns before that step (`do_user_switch`), so the classifier never sees it; the KDE run shows the restart is harmless to an open Hyprland session. |
| **COSMIC Initial Setup stays.** It opens at the first COSMIC login. | The COSMIC module lists `cosmic-initial-setup` among the packages without which COSMIC fails to start, and excluding a core package is unsupported upstream (it warns). GNOME's initial setup is off (D15) because GNOME's module makes that supported. |
| **Lid policy inside GNOME and KDE is theirs.** rime-lid's inhibitor is taken only for live work; in GNOME (gsd-power) and Plasma (PowerDevil) the desktop holds its own `handle-lid-switch` block inhibitor and applies its own lid setting. | §4's "documented limit": "keep working with the lid closed" in those desktops follows the desktop's setting. Not proven on hardware (§12.5). |

### 12.5 Not proven by this piece

- Screen lock in KDE and COSMIC under rime-greet (not exercised by the VM tests); GNOME has none (§12.4).
- What a lid close does in each desktop: the VM tests record the inhibitors, a VM has no lid.
- `checks.polish` was not re-run: it boots three VMs at once, over this host's two-VM limit for this work.
- Real hardware: GNOME/KDE/COSMIC on katana's NVIDIA GPU and its panel.
- The COSMIC battery applet showing rimed's profile: the VM has no battery, Initial Setup covers the panel in the
  screenshots, and rimed's journal does not name its callers. What is proven is the API the applet reads (rimed's
  `ActiveProfile`) and that no System76 daemon answers instead.
- bootc's niri session (`Exec=niri --session`, Containerfile.base) under greetd: by §12.6 it starts `niri` without
  `--session`. The greeter fix in this branch fixes it once it ships to bootc; not booted there.
- Why COSMIC's first login turns its screen reader on (§12.2, the COSMIC run; open-items, build:desktops).
- A desktop WRITING a profile through rimed in KDE: the KDE VM run proves powerdevil READS rimed's profile
  (`currentProfile` follows `rime tier`); no test makes powerdevil (or Plasma's battery applet) set one. The write path
  is proven only from GNOME's side (`powerprofilesctl set`, `launch -p`) and by the private-bus tests.
- `rime-update.service`'s `restartIfChanged = false` (§12.1 Updates) is proven by evaluation only
  (`checks.desktops-eval`, mutant `mF-restart`) and from the switch-to-configuration-ng source: no VM runs an
  `Updates.Apply` whose release changes rimed, so neither the defect nor the fix was observed.

### 12.5a Behaviour that differs from §2-§9 or from bootc (found in verification)

- ~~**A power-saver hold during Gaming Mode does not win.**~~ Fixed at the wave-5 integration (§7, §12.8): a
  power-saver hold now beats Gaming Mode's tier, `Mode.Active` stays `gaming`; bus test
  `a_power_saver_hold_beats_game_mode_and_gaming_stays_on`.
- **The second-power-daemon assertion matches package names** (`power-profiles-daemon`, `system76-power` in
  systemPackages, dbus.packages and systemd.packages), not the closure. A module that ships either daemon under a
  wrapper or inside another package's output would pass it; the VM tests' process checks are the backstop.
- **One-sided units relax rule 3 for every update**, not only `rime install`: `one_sided_units` is computed in
  `os_step` for each switch, so a release that only adds or only removes a unit may start or stop it live.
- **The new D-Bus objects exist on bootc too.** `serve()` registers the Updates and PowerProfiles objects on every
  rimed, and since the wave-5 integration rimed owns the power-profiles names on bootc as well (rime-os #139 did that
  on main first). On bootc `Updates.Apply` would start `rime-update.service`, which the bootc image does not ship
  (handover).
- **The greeter change reaches bootc** (`GreetContext.qml` is shared): every bootc session now gets
  `XDG_SESSION_TYPE=wayland` through greetd's environment (logind `Type=wayland`), and multi-word Execs run whole
  (bootc's `niri --session` gets `--session`). `Greetd.launch(argv, env)` needs quickshell 0.3.2, which the bootc
  image has. Not booted on bootc.

### 12.6 Found while building: greetd re-splits the session command

greetd 0.10.3 runs a session as `/bin/sh -c "[ -f /etc/profile ] && . /etc/profile; exec " + cmd.join(" ")`
(`greetd/src/session/worker.rs`). rime-greet passed `["sh", "-lc", cmd]`, so a `cmd` with a space in it became
`exec sh -lc <first word> <rest>`: `sh -lc` ran only the first word and the rest became `$0`, `$1`, ... Every session
whose Exec has more than one word lost its arguments:

- Plasma's Exec is `plasma-dbus-run-session-if-needed …/startplasma-wayland`: the session exited at once (`kde-1`,
  `kde-2` FAIL).
- Hyprland's Exec in this branch's first version (`env QT_QPA_PLATFORMTHEME=… start-hyprland`) printed the environment
  and exited (`desktop-head.log`, bisected in `desktop-bis{A,B,C}.log`).
- bootc's niri entry (`niri --session`) has been starting `niri` without `--session` (§12.5).

Fixed twice, each enough on its own: the greeter single-quotes the command (`launch()` hands greetd
`["sh", "-lc", '<Exec>']`, `tests/test-rime-greet-sessions.sh` §9 lifts the shipped function and compares what greetd
runs with the Exec run directly; mutant `greet-argv-unquoted` fails it), and Rime's own session files carry a
one-word Exec (`desktop.nix` refuses a space at build time). The greeter file is shared with bootc.

### 12.7 Verification (adversarial pass, at f817a388a)

Evidence: `/var/lab/rime-nix/artifacts/verify-desktops/` (`<check>.log`, `mut-<id>.log`).

- Re-run at HEAD's drvPaths, all PASS: `activation` (VM), `update-client` (VM), `update-e2e` (VM), `security` (VM),
  `catalogue`, `catalogue-data`, `eval`, `ci`, `fedora-parity`, `unit-docs`, `libexec`, `release-unit`; the
  `desktops-eval`, `release-plan`, `desktop`, `desktops-install-{gnome,kde,cosmic}` and `rimed` outputs in the store
  match HEAD's drvPaths. Not run on this branch: `checks.polish` (three VMs), `update-floors` and `update-variants`
  (binary cache), and the rest of the 83 checks not named here.
- Own mutants, each observed failing: GNOME kernel parameter (`desktops-eval`), plain-kde published partition
  (`release-plan`), system76-power `mkForce` removed (module assertion), `hyprland` dropped from `ownSessions`
  (`tests/test-rime-greet-sessions.sh` §8), `restartIfChanged` removed from `rime-update.service` (`mF-restart`:
  `desktops-eval` fails every variant), its `X-StopOnRemoval = false` removed (`mG-stoprm`: 16 variants fail), the game-mode branch of `set_base_tier` removed (`mD-gamewrite`: the bus test
  `game_mode_is_a_performance_hold_and_a_write_sets_where_it_returns` fails).
- `tests/test-rime-greet-sessions.sh` has 3 failures at both this branch and its base c54c25060 (pre-existing).
- Fixed here: `rime-update.service` `restartIfChanged = false` and `X-StopOnRemoval = false` (§12.1), the §9
  virtualization rows and the §12.1 floors explanation. That fix changes the module, so the VM outputs above are for
  f817a388a. Re-run after it (`new-<check>.log`), PASS: `desktops-eval`, `release-plan`, `activation` (VM). NOT re-run
  after it: `desktop`, `desktops-install-{gnome,kde,cosmic}`, `update-client`, `update-e2e`, `security` (the change is
  two `[Unit]`/`X-` keys of one oneshot unit; the integrator re-runs them at the merge).

### 12.8 Integrated with rime-os main (wave 5): one power-profiles implementation

rime-os #139 (main) and this branch both served PPD's API from rimed. The integration keeps ONE implementation, this
branch's `ppd.rs` (custom `Properties` so writes run polkit, PPD 0.30 semantics, `Mode.Set`/`Mode.Active`, the AC
absorb), and carries #139's intents into it:

- names claimed by default with `ReplaceExisting | DoNotQueue` (above, §12.1); one subscribe-first peer watch
  (`main.rs`) releases a departed caller's UI boost and holds and forgets its uid before the names are public;
- an `ActiveProfile` write is a tier pick like `Power.SetTier` (`record_choice`); holds record nothing;
- a release that leaves the holds asking for the same tier writes nothing (`Holds::release`, and `set_base_tier`
  writes nothing when the machine is already on the tier it should be on);
- #139's model tests are ported to `Holds` (`the_last_release_puts_back_what_the_first_hold_found`,
  `a_release_that_changes_nothing_applies_nothing`, `a_caller_leaving_the_bus_releases_its_holds`,
  `a_user_choice_drops_every_hold_and_keeps_the_choice`), and the bus test
  `a_choice_drops_the_holds_and_is_kept_and_a_release_that_changes_nothing_writes_nothing` checks them end to end.

What #139 did that is NOT kept, and so changes on bootc: a write to `ActiveProfile` is authorised by polkit
(`switch-profile`) instead of refused; holds ask `hold-profile` instead of `org.rimeos.rimed.manage-power` and are
allowed during game mode instead of refused; a hold leaves auto-switch alone (an AC change while holds stand moves the
tier they return to) instead of turning it off until the last release; `Version` is `0.30` instead of `rimed <ver>`;
`BatteryAware` is auto-switch and writable instead of always true. `Profiles` is #139's order, lowest first as
power-profiles-daemon lists it (this branch had highest first; `powerprofilesctl list` and GNOME's menu reverse the
list to put `performance` on top, so highest first showed `power-saver` there). All four polkit
actions are `allow_active yes` in both policies, so an active session's user is not asked more than before.

Decision (§12.5a): a power-saver hold beats Gaming Mode's tier (§7). It does not conflict with Gaming Mode's
semantics, which are "game mode is an internal performance hold" (§7), as long as the mode stays identified as
Gaming; `Mode.Active` therefore reads the game's own tier while a power-saver hold overrides it. Tests: bus
`a_power_saver_hold_beats_game_mode_and_gaming_stays_on` (hold during, hold before, hold outliving game mode,
`Mode.Set daily` ending both), unit `a_power_saver_hold_beats_game_mode_and_a_performance_hold_does_not`. Mutants,
each observed failing: `live_in_game` always the game's tier (both tests), `Mode.Active` ignoring the override,
game enter ignoring a standing hold, and the `ActiveProfile` write recording no pick (the choice bus test).
`tests/test-rimed-ui-boost.sh` (real rimed, `powerprofilesctl`, power-profiles-daemon 0.30) now expects
`powerprofilesctl set` to be a tier pick for the session user and refused for anyone else.
