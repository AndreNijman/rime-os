# Other desktops, Rime Settings, quick toggles and power profiles on NixOS (design)

Status: **design, nothing built.** Andre (2026-11-03): *"with the rime nix release there should also be first party
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
| Secret Service | gnome-keyring (unlocked by greetd's PAM hook) is the one `org.freedesktop.secrets` everywhere. KDE: `/etc/xdg/kwalletrc` `[org.freedesktop.secrets] apiEnabled=false`; no `pam_kwallet` on greetd (plasma6 adds it to `login` only, not greetd **(checked)**). KDE apps using the KWallet API get a wallet that asks once. kwallet-pam's `pam_kwallet_init.desktop` autostart has no ShowIn **(checked)** and will exist in every session once KDE is installed: ship an override with `OnlyShowIn=KDE;`, and the VM tests assert no kwalletd process and one secrets owner (gnome-keyring) in a Hyprland login on a KDE variant. | Two owners of the name was the bootc keyring failure (two daemons, one locked). |
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
| `ActiveProfile` (s, rw) | read: the effective tier (a hold wins, below). write: polkit `org.freedesktop.UPower.PowerProfiles.switch-profile` (allow_active yes), then exactly `Power.SetTier` (same code path; auto-switch is left as it is, so unplugging still follows Rime's AC/battery rule, as `rime tier` does today), and **releases every hold** with `ProfileReleased(cookie)` for each, as PPD does. Unknown id → InvalidArgs. |
| `Profiles` (aa{sv}) | one dict per tier the machine has (`Power.Tiers`): `Profile`, `Driver` = `rimed`, `PlatformDriver`, `CpuDriver` (what rimed writes: `platform_profile`, `intel_pstate`/`amd_pstate`). Never `Driver = placeholder` (KDE treats that as "no profiles"). |
| `PerformanceDegraded` (s) | "" or PPD's reasons (`high-operating-temperature`, `lap-detected`) when rimed's own thermal/lap state says performance is capped. |
| `PerformanceInhibited` (s) | "" (deprecated upstream; present for old clients). |
| `HoldProfile(s profile, s reason, s application_id) → u` | polkit `...hold-profile` (allow_active yes); only `performance` or `power-saver`. Holds are tied to the caller's unique bus name and dropped on `NameOwnerChanged`. Precedence as PPD: **any power-saver hold beats every performance hold**. While a hold is active the effective tier is the held one; when the last hold goes, the tier returns to the one chosen before. |
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
virtualization's is (`withVirtualization`). The script reproduces today's published figures (16/18/17/19 disk,
14/16/14/16 partition) before adding anything **(checked, measure-sizes.txt)**:

| Base | none | + GNOME | + KDE Plasma | + COSMIC |
|---|---|---|---|---|
| plain | 16 / 14 | **17 / 15** | **18 / 16** | **19 / 16** |
| plain-gaming | 18 / 16 | **19 / 17** | **21 / 18** | **21 / 19** |
| plain-virtualization | 17 / 14 | **18 / 16** | **19 / 17** | **19 / 17** |
| plain-gaming-virtualization | 19 / 16 | **20 / 18** | **21 / 19** | **21 / 19** |

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

- Nothing was booted: GNOME, KDE and COSMIC under rime-greet, the greeter env, Quickshell as a plain xdg client under
  Mutter, the lid inhibitor under each desktop and every dry-activate prediction in §3.5 are for the VM tests.
- The dry-activate list is predicted from unit content diffs, not from `switch-to-configuration dry-activate` (not run
  here by rule: no activation inside the build container).
- Download sizes are against this container's store, which held more than plain; the per-variant "Rime-built" sizes
  are outputs of the derivations the dry run listed.
- Disk floors for desktop variants are estimates (NAR delta x 0.55), not an installed measurement.
- The hosted runner's free disk (14 GB) is the figure from updates-and-release.md §18, still not verified.

---

## 12. Settings piece: decisions taken while building

Andre asked for no questions; each call below was made while building rime-shell `nix/settings-app` (ab8ca519) and
rime-os `nix/settings-pkg`, with the reason.

**Rime Settings (rime-shell).**

- **About is app-only.** In the Shell, About lives in the bar's menu; in the app it is the only place that names the
  release, so the app lists it and the Shell's Nexus does not grow a second copy.
- **Desktop sizes on the Desktops page come from the catalogue's floors (`store_bytes`)**, the same figures `rime
  install <desktop>` compares, never a second estimate in QML.
- **Shell-only pages** (`PageRegistry` `shellOnly`): Appearance, Layout, Display, Input, Keybinds, Misc. Every kept page
  was read for `hyprctl` and Hyprland IPC; run-settings-app-test.sh puts a `hyprctl` stub on PATH and opens every page.
- **`rime-settings` takes `rime-settings:<page>` URLs** as well as a bare page, so KDE's `Qt.openUrlExternally` and the
  `.desktop` file's `%u` reach the same code; an empty page opens the app's first page.
- **The `.desktop` file names one main category** (`Settings;DesktopSettings;`), not `Settings;System;`: two main
  categories put it in two menus (desktop-file-validate's hint, ab8ca519).
- **PowerProfileService** follows Quickshell's UPower `PowerProfiles` (the PPD API rimed serves, §7) and falls back to
  `Rimed1.Power.Tier` only while that name has no owner: the two pieces land in either order.

**Quick settings (rime-os `nix/integrations/`).**

- **Restart goes through each desktop's own dialog**: GNOME `org.gnome.SessionManager.Reboot` (its countdown dialog),
  Plasma `org.kde.LogoutPrompt.promptReboot`, COSMIC `cosmic-osd restart` (what cosmic-applet-power runs). A desktop's
  restart dialog knows about unsaved work; a direct `systemctl reboot` would not.
- **Update item titles differ by desktop, on purpose.** GNOME's quick toggle has a title and a subtitle, so it reads
  "Update" / "Rime 2026.11.04"; Plasma's popup and COSMIC's menu have one line ("Update to 2026.11.04" in Plasma,
  title and subtitle joined in COSMIC). The states shown are the same everywhere: available (clickable),
  downloading/installing (shown, not clickable), restart (clickable); up-to-date, deferred, failed and unknown hide it
  (Rime Settings' Updates page says why an update failed or waits).
- **Test hook**: an integration reports what it draws to `org.rimeos.QuickSettingsTest` on the session bus
  (`Report(ss)`), only while that name has an owner, which it watches (no polling). Only the VM tests own that name.
- **GNOME's power property** read by the VM test is PPD's `ActiveProfile` on the same proxy GNOME's own power menu uses;
  Plasma's is powerdevil's (`org.kde.Solid.PowerManagement.Actions.PowerProfile currentProfile`).
- **GNOME `enabled-extensions`**: one dconf key, so the GNOME entry in `nix/desktops` (piece 1's file) now lists both
  `kimpanel@kde.org` and `rime@rimeos.org`; a second dconf database would hide one list behind the other.
  `checks.settings-integrations-eval` fails a variant with two lists.
- **GNOME `shell-version`** is written from `gnome-shell.version`'s major and the build asserts that major is in
  `knownMajors` (50): a nixpkgs move to 51 fails the build until someone reads 51's `quickSettings.js`.
- **KDE: pure QML on `org.kde.plasma.workspace.dbus`**, no C++ plugin to build. Two facts about that module (6.8.2),
  both found by experiment: `asyncCall`'s `signature` must be a struct (`"(s)"`, `"(ssv)"`) whenever there are
  arguments, and it cannot set `ALLOW_INTERACTIVE_AUTHORIZATION` on a call. Open Rime Settings uses
  `Qt.openUrlExternally("rime-settings:<page>")` rather than `KIO::ApplicationLauncherJob` (C++ only).
- **KDE's Update click and polkit (handover to rimed):** because the plasma module cannot ask for interactive
  authorization, `Updates.Apply` (`auth_admin_keep`) from Plasma reaches rimed without the flag. rimed's polkit check
  for `Apply` (and `Mode.Set`) must pass `AllowUserInteraction` whatever the message flag says, or Plasma's click is
  refused with `NotAuthorized` instead of showing Plasma's polkit agent. GNOME's and COSMIC's calls set the flag.
- **COSMIC: two crates.** `rime-cosmic-model` (zbus only) holds every D-Bus read and both actions and is what the
  unit test compiles in about a minute; `cosmic-applet-rime` (libcosmic, about 4 minutes) only draws. The applet
  uses `tokio::spawn` from `update()` with `SingleThreadExecutor`, the pattern every shipped cosmic-applets applet
  uses (checked in cosmic-applets 1.11.0).
- **COSMIC panel default**: nixpkgs' cosmic-panel installs no `share/cosmic`, so cosmic-panel's own compiled default
  is what a new user gets. Rime installs cosmic-panel's `data/default_schema` (Panel and its entries) with
  `org.rimeos.CosmicAppletRime` inserted before the power applet, linked by the COSMIC module's `pathsToLink`
  `/share/cosmic`. The build fails if cosmic-panel's default stops listing the power applet. It is a default: a user
  who already changed the panel keeps their own list and adds Rime in COSMIC Settings → Desktop → Panel → Applets.
- **No power-profile toggle in any integration** (§6.1): unchanged.

**Branches.**

- **rime-os `nix/settings-pkg` is based on `feat/nixos` c54c25060 + `nix/desktops` 269263491** (fast-forward), not
  on `df8a25051`: the GNOME entry it edits and the module list it adds to exist only from there.
- **Pushes**: the task asked both not to push and to push rime-shell `nix/settings-app` through the push clone so
  rime-os can lock it. rime-shell `nix/settings-app` was pushed (ab8ca519); rime-os `nix/settings-pkg` was not.
  `flake.lock` locks rime-shell at ab8ca519 while its `original` still follows `feat/nixos`: **merge
  `nix/settings-app` into rime-shell `feat/nixos` before the release job re-locks the input**, or a re-lock drops
  Rime Settings.
- **`nix/desktops` was not merged into `nix/settings-pkg`**: piece 1's rimed D-Bus commit (§6.2 `Mode.Active`/`Set`,
  `Updates`, §7 PPD API) does not exist yet. The VM checks' assertions that need it (Power → performance → rimed
  `Tier` and `powerprofilesctl get`; `rime mode set gaming` → the toggle) wait for it.
