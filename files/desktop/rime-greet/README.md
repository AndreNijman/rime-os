# rime-greet

A Quickshell/QML **greetd** greeter for Rime OS. It started as a standalone
port of the Brain_Shell lock screen: the same clock, pill auth card,
blurred-wallpaper backdrop and shake on failure, re-implemented so it runs
before any user session exists. It needs nothing from Rime Shell except one
optional component: the password field loads the lock screen's
`PasswordShapes` from the shell tree the image vendors, and falls back to plain
dots without it.

## Files

| File | Role |
|------|------|
| `shell.qml` | Entry point. Inlined palette (`ThemeGreet`), the fullscreen layer-shell window model, wires `GreetContext` to `GreetSurface`. |
| `GreetContext.qml` | Non-visual shared state and the greetd auth backend: edition detection, session list, last-user/last-session memory, the published wallpaper, accent and motion settings, keyboard layout, recovery preselection. |
| `GreetSurface.qml` | The per-output visuals: clock, wallpaper/gradient, logo, username pill, keyboard-layout pill, password pill with its shapes, spinner, error shake, status line, session picker. |
| `assets/spark-{gold,chartreuse,white}.png` | Edition spark logos (256px). |
| `greetd-config.toml` | The image's `/etc/greetd/config.toml`: launches sway through `/usr/libexec/rime-greet-session`. |
| `sway-greet.conf` | The sway host config. Hosts quickshell as its only client and binds one key, the screen reader. |
| `labwc-greet/` | Fallback host: labwc config dir (`rc.xml`, `autostart`, `environment`) + note. |

Outside this directory:

- `/usr/libexec/rime-greet-session` wraps the compositor. It gives the greet
  session a D-Bus session bus and an accessibility bus, then execs the
  compositor, so greetd still holds the compositor's own PID.
- `/usr/libexec/rime-greet-wallpaper` publishes a user's wallpaper, accent and
  motion settings for the login screen (see **Wallpaper, accent and motion**).
  `rime-greet-publish.service` runs it for every account before greetd starts.
- `/usr/libexec/rime-session-watchdog` counts sessions that die at start (see
  **Recovery preselection**).

## How it works

### Window model

The greeter runs under **`sway`** (a wlroots kiosk compositor), *not* as
a session lock. sway implements `wlr-layer-shell` natively, the mechanism
gtkgreet uses, so the fullscreen surface is a Quickshell `PanelWindow`
anchored to all four edges, on the `Overlay` layer, with
`WlrKeyboardFocus.Exclusive`. `Variants` over `Quickshell.screens` creates one
surface per output. (A machine normally has a single output; extra outputs
mirror the same shared state.)

greetd starts sway as
`/usr/libexec/rime-greet-session sway --unsupported-gpu -c /usr/share/rime-greet/sway-greet.conf`.
sway 1.11 exits on any machine where `nvidia-drm` binds unless it gets
`--unsupported-gpu`, and the image carries the NVIDIA driver, so the flag stays
on every machine. `greetd-config.toml` has the full reasoning.

> The host **changed from `cage` to `sway` in M1**: M0 spike A proved
> `cage` 0.2.0 does **not** expose `wlr-layer-shell` to quickshell 0.3.0, so
> the `PanelWindow` never got a surface and the greeter launched but never
> painted. See **Compositor host** below and `docs/m0-results.md` "Spike A".

### Auth flow (PamContext → Greetd)

The lock screen drives PAM directly. A greeter talks to greetd instead, and
greetd owns the PAM conversation. The mapping:

| Lock screen (PAM) | rime-greet (Greetd) |
|---|---|
| `pam.start()` | `Greetd.createSession(username)` |
| `onResponseRequired` → `respond(pw)` | `onAuthMessage(…, responseRequired, echoResponse)` → `Greetd.respond(echoResponse ? username : password)` |
| `onCompleted(Success)` → unlock | `onReadyToLaunch` → `Greetd.launch(argv)` (quickshell exits) |
| `onCompleted(fail)` → `fail()` | `onAuthFailure(msg)` → `fail(msg)` |
| `onError` → `fail()` | `onError(msg)` → `cancelSession()` + `fail()` |

`Greetd` is `Quickshell.Services.Greetd` (docs:
<https://quickshell.org/docs/v0.1.0/types/Quickshell.Services.Greetd/Greetd>).
Guards on `Greetd.available` and `GreetdState.Inactive` keep the
conversation well-formed. On failure greetd tears the session down, so
the next Enter starts a fresh `createSession`.

A refused attempt clears the field, shakes the card and turns the password
pill's outline red. The outline stays red until you type again.

### Password field

The password field draws its feedback with Rime Shell's `PasswordShapes`, the
component the lock screen uses: one Material 3 Expressive shape per character.
`GreetSurface` loads it through a `Loader` from
`/usr/share/rime-shell/src/components/auth/PasswordShapes.qml` and hands it the
field's length, never its text. The field itself stays `echoMode` Password with
mask delay 0; with the shapes loaded it paints no mask characters, caret or
selection of its own.

If the component cannot load (a shell tree older than it, a dev box without
`/usr/share/rime-shell`), the field shows the plain masked dots it always had,
so the shapes can never stop the login screen loading. `RIME_GREET_SHELL_ROOT`
points the loader at a checkout for the test suites.

The shapes move on the last user's own motion settings: Reduce Motion, speed
and duration scale, read from `/var/lib/rime-greet/motion/<user>` (see
**Wallpaper, accent and motion**). A user who turned Reduce Motion on sees no
shape animation at the login screen either.

### Editions

`GreetContext` resolves the edition with this precedence:

1. `/etc/rime-greet/edition` (first line: `rime`, `gaming`, `daily`, or `mono`), an override.
2. `/etc/os-release` `VARIANT_ID`. `Containerfile.rime` stamps `rime`, the
   one published image. `gaming` and `daily` still work for a machine that has
   not updated past the three-edition split.
3. Fallback `mono` (dev box / unset).

The edition drives the spark logo **and** the accent colour:

| Edition | Logo | Accent |
|---|---|---|
| `rime` | chartreuse spark | `#d9f99d` |
| `gaming` | gold spark | `#fde047` |
| `daily` | chartreuse spark | `#d9f99d` |
| `mono` (fallback) | white spark | `#a6d0f7` (the Brain_Shell blue, so off-target the greeter matches the Brain_Shell lock screen pixel for pixel) |

A user's published accent overrides the edition colour. The greeter then draws
the white spark and tints it to that accent, so the login screen matches the
desktop.

### Sessions & memory

The greeter parses `/usr/share/wayland-sessions/*.desktop` (`Name` + `Exec`,
field codes stripped) into a bottom-centre `‹ Session ›` picker. Click the
arrows, Tab to them and press Space, or press `Alt+Left/Right` in the password
field. The greeter skips an entry whose `TryExec` binary is not on `PATH`, as
the desktop-entry spec says.

The greeter prefills the username from `/var/lib/rime-greet/last-user` and
preselects the session from `/var/lib/rime-greet/last-session`, while that
session is still installed. With no memory, or a remembered session that has gone, the
picker lands on `hyprland` by name. `GreetContext.defaultSession` names it
instead of taking the first sorted entry, because adding one `.desktop` file
would otherwise change what every fresh install boots into.

A successful launch (re)writes both files, passing the values through the
environment so a hostile username cannot inject shell, and defers the actual
`Greetd.launch` until the write flushes. The greeter never writes the recovery
session to `last-session` (see **Recovery preselection**). The greeter
tolerates an unwritable state dir without a message.

The greeter prints `Name` verbatim, so the entries set the wording, not this
file. The image ships five: **Rime Tiling** (Hyprland), **Rime Floating**
(labwc), **Rime Scrolling** (niri), **Rime Safe Graphics** and **Rime Gaming
Mode**. Gaming Mode's `TryExec` is gamescope, so the picker offers it only on a
machine where gamescope is installed. Each entry keeps the compositor's own
name in `DesktopNames` (and, for the three desktops, in `Comment`), and its
file name is the id that
`rime-session-select` and `last-session` store; the portals config keys off
`DesktopNames`. `tests/test-rime-greet-sessions.sh` runs this enumeration and
fails if a compositor's name reaches the picker.

### Recovery preselection

If a desktop dies right after login, greetd brings the login screen back and
counts nothing. `/usr/libexec/rime-session-watchdog` does the counting: the
greeter calls `record` on the way out and `check` on the way in. After three
failed starts in a row, `check` names `rime-safe-graphics`, and when that
session is in the picker the greeter preselects **Rime Safe Graphics** and says
why on its status line ("Your desktop did not start"). It is a suggestion.
Cycling the picker clears the preselection and the notice, and your choice
sticks.

The greeter compares the helper's answer against the one id it will accept
(`recoverySession`), so garbage or a different id selects nothing, and
`timeout 5` caps both calls.

### Keyboard layout

A pill between the username and the password shows the live keyboard layout, so
you can check it before you type a password. The greeter reads it from sway
(`swaymsg -t get_inputs`), not from the environment. When the system has more
than one layout configured, Space or Return on the pill cycles them
(`xkb_switch_layout`); with one layout it is an indicator only. Under the labwc
fallback `swaymsg` gets no answer, so the pill shows `XKB_DEFAULT_LAYOUT` and
offers no switch.

The Tab ring runs username → keyboard layout → password → session picker.

### Wallpaper, accent and motion

The greeter runs as the `greetd` user and cannot read home directories, so Rime
Shell publishes what the login screen needs through
`/usr/libexec/rime-greet-wallpaper`, a root helper with a no-argument
`NOPASSWD` sudoers rule. It reads each file as the calling user and writes:

- `/var/lib/rime-greet/wallpapers/<user>.{jpg,png,webp}`: the current
  wallpaper, validated as a real image and capped at 64 MiB;
- `/var/lib/rime-greet/accents/<user>`: the accent from the shell's matugen
  output;
- `/var/lib/rime-greet/motion/<user>`: three lines (`reduce=`, `speed=`,
  `scale=`) from the shell's `settings.json`.

Rime Shell calls it after each wallpaper change (`WallpaperService`) and each
motion-setting change (`SettingsService`), and
`/usr/libexec/rime-shell-autostart` calls it once per session.
`rime-greet-publish.service` runs it for every account at boot, before greetd,
so the first login screen after an update is already right. The greeter
validates the accent and motion values again as it reads them, and re-reads all
three when the username changes (debounced 400 ms), so typing a different
account shows that account's wallpaper and accent.

The greeter blurs the wallpaper (`MultiEffect`, blurMax 48, brightness −0.30,
saturation −0.10) under a 0.35 black scrim. With no published wallpaper the
greeter uses `/usr/share/backgrounds/rime/default.jpg`; a missing file falls
back to the vertical gradient (`darker(background)` → accent).

## Compositor host

The greeter is a quickshell **layer-shell** surface (`PanelWindow`, Overlay
layer, exclusive keyboard), so its host compositor must serve
`wlr-layer-shell` to quickshell.

| Host | Layer-shell? | Status | Config |
|---|---|---|---|
| **`cage` 0.2.0** | **No** (for quickshell 0.3.0) | **Abandoned**: greeter launches but never paints | none |
| **`sway` 1.11** | Yes (native) | **Primary host** | `sway-greet.conf` |
| **`labwc` 0.9.6** | Yes (native) | **Fallback host** | `labwc-greet/` |

**Why not cage.** M0 spike A found that `cage` 0.2.0 does not expose
`wlr-layer-shell` to quickshell 0.3.0 (`Failed to initialize layershell
integration`), so the `PanelWindow` never gets a surface: greetd starts,
rime-greet launches, the greetd/PAM conversation is reachable, but nothing
paints. (gtkgreet works under cage only because it falls back to an
xdg-toplevel; quickshell's `PanelWindow` does not.) `sway` and `labwc` both
implement `wlr-layer-shell` v4 natively, which is why M1 swapped hosts. Full
detail in `docs/m0-results.md` "Spike A".

`sway-greet.conf` is a bare config: it does **not**
`include /etc/sway/config.d/*`, has no bar and no app launchers, hides the
cursor after 8 s idle and while typing, disables titlebars/borders, disables
Xwayland, paints a black root as a pre-map fallback, leaves output resolution
to autodetect, and sets no keyboard layout (sway takes `XKB_DEFAULT_LAYOUT`
from the Rime keymap generator). Its one keybinding, `SUPER+ALT+S`, starts the
screen reader (`/usr/libexec/rime-screen-reader toggle`), the same combination
the desktop uses. It runs quickshell as its **only** client via
`exec "qs -p /usr/share/rime-greet/shell.qml; swaymsg exit"`, so sway exits
the moment quickshell quits (mirroring `cage -d`'s exit-with-child). On a
successful login greetd itself terminates the greeter and starts the user
session; the `swaymsg exit` covers the cancel/crash path.

### labwc fallback

If sway misbehaves on some hardware, swap to labwc, which also serves
layer-shell natively. Point greetd at:

```toml
command = "labwc -C /usr/share/rime-greet/labwc-greet"
```

The commented swap-in line in `greetd-config.toml` also runs the wrapper
(`/usr/libexec/rime-greet-session labwc -C /usr/share/rime-greet/labwc-greet`);
without it the login screen has no session bus and no screen reader can read
it.

The `labwc-greet/` config dir holds `rc.xml` (no decorations, tap-to-click off,
one keybind: `W-A-s` for the screen reader), `autostart` (runs quickshell, then
`labwc -e` on quit), and `environment` (no keyboard layout set; the commented
no-GPU variables). See `labwc-greet/README.md` for caveats (no
cursor-idle-hide equivalent).

### No-GPU render fallback (untested, a hardware verify item)

On a machine with **no working GL**, the software-render combo is:

```sh
WLR_RENDERER=pixman LIBGL_ALWAYS_SOFTWARE=1 QT_QUICK_BACKEND=software \
  sway -c /usr/share/rime-greet/sway-greet.conf
```

Set these in the greetd session **environment** (e.g. an `environment.d` drop-in
or a wrapper), not in the sway config. Caveat: M0 spike A found that
`WLR_RENDERER=pixman` **crash-looped cage**. sway's pixman renderer is more
robust, but this exact combo under sway is untested; verify it on real
hardware or a GL-capable runner. (For labwc, `labwc-greet/environment` lists
the same three variables, commented.)

## Integration (greetd + sway)

The image installs the greeter tree to `/usr/share/rime-greet/` (so
`Qt.resolvedUrl("assets/…")` resolves to `/usr/share/rime-greet/assets/…`) and
copies `greetd-config.toml` to `/etc/greetd/config.toml`. `sway-greet.conf` and
`labwc-greet/` ship inside the same tree.

**Packages.** `Containerfile.core` installs `greetd`, `sway`, `labwc` and
`quickshell-git` (a rolling Quickshell snapshot; the comment there explains
why), and still installs `cage`. `Containerfile.base` copies the greeter tree,
the wrapper and the config, writes the tmpfiles rule and enables greetd.

### Containerfile snippet

The snippet below is the original integration sketch. The image differs from
it: the packages come from `Containerfile.core` as above, greetd's command runs
through `/usr/libexec/rime-greet-session` with `--unsupported-gpu`, and the
tmpfiles rule has a second line that creates `/var/lib/rime-greet/wallpapers`
as `root:root 0755`.

```dockerfile
# Greeter runtime — sway is the host, labwc the fallback (both native
# wlr-layer-shell); cage is abandoned (M0 spike A: no layer-shell for quickshell).
RUN dnf install -y greetd sway labwc quickshell && dnf clean all

# Greeter files (from this repo's files/desktop/rime-greet) — includes
# sway-greet.conf and labwc-greet/.
COPY files/desktop/rime-greet /usr/share/rime-greet
COPY files/branding/wallpapers/rime-wallpaper-default.jpg \
     /usr/share/backgrounds/rime/default.jpg

# greetd config — its default_session.command launches
#   sway -c /usr/share/rime-greet/sway-greet.conf
COPY files/desktop/rime-greet/greetd-config.toml /etc/greetd/config.toml

# Writable state dir, owned by the greetd session user. On Fedora the
# greetd package provisions a `greetd` user (sysusers.d) — there is no
# `greeter` user — so the dir is owned by `greetd`. tmpfiles creates it at
# boot (after systemd-sysusers has run).
RUN printf 'd /var/lib/rime-greet 0755 greetd greetd -\n' \
      > /usr/lib/tmpfiles.d/rime-greet.conf

# Make greetd the display manager
RUN systemctl enable greetd.service
```

The image installs `quickshell-git`; these files were first written against
Quickshell 0.3.0.

### State dir & SELinux

The greeter runs as the **`greetd`** user (Fedora's packaged greetd user),
so `/var/lib/rime-greet` must be writable by it (the tmpfiles rule). The
`wallpapers/`, `accents/` and `motion/` directories under it are root-owned:
only `rime-greet-wallpaper` writes there, and the greeter only reads them.
Under SELinux the dir gets `var_lib_t`. If the greeter session is
confined and the kernel denies the `last-user`/`last-session` write, the
greeter still works: the write fails and the next boot skips the prefill. To
keep the memory feature, either relabel the dir for the greetd domain or ship
an allow rule; check with `ausearch -m avc -ts recent` after the first login.

## Testing

**Parse check (no compositor needed).** The layer-shell window cannot
map without a Wayland backend, but the QML still compiles:

```sh
QT_QPA_PLATFORM=offscreen qs -p files/desktop/rime-greet/shell.qml
```

A clean parse ends with a runtime error `No PanelWindow backend loaded`
at the `PanelWindow` line; QML syntax/type errors appear *before* that. (Auth
needs a live greetd socket, so you can test full behaviour only in a VM.)

**Nested run on a dev box.** With sway installed, run the host config nested
inside a window on your existing Wayland session (edit the `exec` path to the
in-tree `shell.qml`, or install the tree to `/usr/share/rime-greet` first):

```sh
sway -c files/desktop/rime-greet/sway-greet.conf
```

greetd is absent, so `Greetd.available` is false and Enter shows
"greetd is not available". The visuals, clock, caps-lock hint, session picker
and shake animation all still run. Full auth and launch need a Rime OS VM
where greetd owns the VT.

**Config syntax check (no HW).** The sway config parses without errors under
`sway --validate`. sway 1.11's `--validate` still brings up the wlroots
backend first, so in a headless container force the software/headless path:

```sh
# in a Fedora 43 container with sway installed:
export XDG_RUNTIME_DIR=/tmp/xrt; mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
WLR_BACKENDS=headless WLR_RENDERER=pixman \
  sway --validate -c /usr/share/rime-greet/sway-greet.conf   # exit 0 = clean
```

(`sway-greet.conf` passed this check against sway 1.11 in a
`fedora-bootc:43` container, exit 0. labwc has no `--validate`; check its
`rc.xml` for XML well-formedness with `xmllint --noout`.)

**Suites.** `tests/test-rime-greet-*.sh` run headless and cover:
`sessions` (picker names), `layout` (the layout readout against sway's JSON),
`accent` (the published accent), `motion` (the password shapes and the motion
file), `a11y` and `atspi` (what a screen reader and a keyboard-only user get),
and `session-bus` (the wrapper's buses). `tests/mutate-greet-atspi.sh` and
`tests/mutate-greet-session-bus.sh` prove those two suites can fail.

## Differences from the Brain_Shell Lockscreen

| Aspect | Brain_Shell `Lockscreen.qml` | rime-greet |
|---|---|---|
| Window primitive | `WlSessionLock` + `WlSessionLockSurface` | `PanelWindow` (layer-shell) over `Variants`/`Quickshell.screens`, under sway (labwc fallback) |
| Auth backend | `Quickshell.Services.Pam` (`PamContext`, config `system-auth`) | `Quickshell.Services.Greetd` (`createSession`/`respond`/`launch`) |
| Unlock/exit | sets `LockState.locked = false` | `Greetd.launch(argv)` → quickshell exits, session starts |
| Palette | `Theme`/`Colors` singletons (live, from colors.json) | inlined `ThemeGreet` (colors.json fallbacks); the user's published accent, else the edition accent |
| Wallpaper | `WallpaperService.currentWall` | the last user's published wallpaper, else `/usr/share/backgrounds/rime/default.jpg` |
| Username | display only (`$USER`); PAM resolves the auth user | editable field, prefilled from `last-user`, passed to `createSession` |
| Extra UI | none | edition spark logo, keyboard-layout pill, session picker, recovery notice |
| State | per-surface | shared `GreetContext` across surfaces |
| Clock, pill geometry, spinner, shake, caps heuristic, Escape-clears | n/a | **identical** |

## Known limitations / to verify

- **Sway host.** The sway host is the login screen on the shipped image and
  renders on real hardware: `tests/test-rime-greet-accent.sh` records the L16's
  login screen on 2026-09-23. Still untested: the no-GPU
  `WLR_RENDERER=pixman` fallback, which crash-looped *cage* in M0 spike A (see
  **No-GPU render fallback**).
- **labwc host.** The labwc fallback has no recorded run on real hardware.
- **`echoResponse` heuristic.** Hidden prompts get the password, visible
  prompts get the username. That is correct for the usual `pam_unix` password
  prompt after `createSession`; a PAM stack that asks something else
  (e.g. an OTP) would need extra handling.
- **`launch` wrapping.** The session `Exec` runs as `sh -lc "<Exec>"`
  for PATH/profile resolution. Some sessions may prefer the raw argv or
  a `dbus-run-session`/`uwsm` wrapper; adjust `launch()` if so.
- **Empty password.** The greeter does not submit an empty password (parity
  with the lock screen), so passwordless accounts cannot log in here.
- The greeter was first written against Quickshell **0.3.0** and the greetd
  docs **v0.1.0**. The image now installs `quickshell-git`, so re-check the
  `Greetd` API when that snapshot moves.
