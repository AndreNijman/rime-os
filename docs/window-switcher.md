# Keyboard window switching

Two shortcuts follow one rule: **after the keyboard moves focus, the pointer
goes with it.**

| | Hyprland (Rime Tiling) | labwc (Rime Floating) | niri (Rime Scrolling) |
|---|---|---|---|
| `SUPER`+arrow | moves focus, cursor warps into the window | moves the window to that edge, cursor warps with it | moves focus along the scroll |
| `ALT`+`Tab` | Rime Shell switcher | labwc's thumbnail switcher, all desktops | niri's recent-windows switcher |
| `ALT`+`SHIFT`+`Tab` | the same, backwards | the same, backwards | the same, backwards |

## The pointer follows keyboard focus

The Hyprland and labwc sessions both give focus to the window under the pointer
(`follow_mouse = 1`, `<followMouse>yes</followMouse>`). Rime sets this on
purpose, so focus never depends on where a click lands. The cost is that a
focus change you make with the keyboard lasts only until you touch the mouse.
Press `SUPER`+`Left`, take your hand off the keyboard, nudge the mouse, and
focus snaps back to the window the pointer was still sitting on.

To stop that, the compositor carries the pointer to the middle of the window
that has gained focus:

* **Hyprland:** `cursor { no_warps = false }` in `rime/input-defaults.lua`.
  Hyprland already defaults to this. Rime writes it down anyway, so a later
  Hyprland release cannot change Rime's behaviour by changing its own default.
* **labwc:** `/usr/libexec/rime-labwc-keybinds` generates
  `<action name="WarpCursor" to="window"/>` after each `MoveToEdge` and
  `SnapToEdge`. labwc has no directional-*focus* action (`Focus` takes no
  arguments), so `SUPER`+arrow moves the window, and the warp stops the move
  from handing focus to the window it uncovered.
* **niri:** nothing to do. niri gives focus to the window under the pointer
  only when configured to, so nothing takes the focus back.

`tests/test-rime-hypr-focus.sh` checks the *consequence* on a real nested
Hyprland: the pointer ends up inside the newly focused window, and that window
keeps focus after the test nudges the pointer. It does not check that a config file
contains a line. It then tears the session down and runs everything again with
`no_warps = true`, where those assertions **must** fail, so the suite cannot
pass by agreeing with whatever the configuration says. In that control run the
pointer stays at 480,540 while the focused window starts at x=962, and a
one-pixel nudge hands focus straight back: Andre's report, reproduced on
demand.

## Test coverage

| | suite | what it proves |
|---|---|---|
| rime-os | `tests/test-rime-hypr-focus.sh` | `SUPER`+arrow warps the pointer and holds focus; the control fails; every arrow combo is bound exactly once; the switcher's binds exist and the `Alt_L`/`Alt_R` ones are releases |
| rime-shell | `tests/run-window-switcher-test.sh` | real `ALT` and `TAB` keys reach the switcher: stepping, wrapping, backwards, `ESCAPE`, commit, a tap with no pause at all, a stale flag repairing itself, and a window that has left the screen |
| rime-shell | `tests/run-switcher-activate-test.sh` | committing focuses the selected window, puts the pointer inside it, holds focus through a nudge, and switches workspace for a window on another one |
| rime-shell | `tests/run-niri-keybinds-test.sh` | the niri fragment carries `recent-windows` and `niri validate` accepts it |

No headless suite can press a real `ALT`, hold it and let go. Hyprland 0.56.2
accepts `wtype`'s virtual keyboard, reports it with `active keymap: error`, and
fires no binding from it. niri behaves the same way; only wlroots compositors
take it. The suites therefore assert that the release binding is *registered*
(`release: true` in `hyprctl binds`), and a person holding `ALT` confirms it
works.

## Changing the switcher's key

Rime Settings → Keybinds → Window Switcher. Only the Hyprland session follows it
(niri and labwc keep their own switchers, which stay on `ALT`+`Tab`). The combo must hold exactly one of `ALT`, `SUPER` or
`CTRL` (no `SHIFT`: that is the reverse direction), plus a key. Letting go of that
modifier switches, so the shell moves the release binds, `Escape` and `Return`
with it. A release on a modifier other than `ALT` cannot be driven by a synthetic
keyboard, so only a person holding it confirms it.

Rime Settings → Gaming → *Hide games from Alt+Tab* leaves Steam, its games and
Sober out of the Rime Shell switcher (Hyprland session only), so it never
switches to them or to their workspace. Hyprland's own workspace-slide animation
is not changed.

## The Alt-Tab switcher

`rime shell switcher next | prev | commit | cancel`

Hold `ALT` and tap `Tab` to step through every window on every workspace, most
recently used first, then release `ALT` to switch to the highlighted one.
`ESCAPE` (or `ALT`+`ESCAPE`) cancels and leaves focus where it was. A single tap
and release swaps you back to the last window.

The window list comes from **wlr-foreign-toplevel-management**, the protocol the
app dock uses. It lists each toplevel the compositor has, on any workspace or
output, with no compositor-specific IPC involved.

### Hold and release without a keyboard grab

The obvious implementation gives the switcher surface exclusive keyboard focus
so it can watch for the `ALT` release. That fails here: taking keyboard focus
takes it *away from the window the switcher is about to activate*, the exact
defect the rest of this document is about.

The shell's overlay never asks for the keyboard. The **compositor** reports the
release instead:

* Hyprland: a bind with `release = true` (`bindr`) on `Alt_L` and `Alt_R`,
  plus `non_consuming = true` so applications still see the release themselves.

That bind fires on *every* `ALT` release for as long as the machine is on, not
only while the switcher is open. For that reason the keybind names
`/usr/libexec/rime-switcher` rather than `rime shell switcher`: with nothing
open, the helper runs one `[ -e ]` on a flag file under `XDG_RUNTIME_DIR` and
exits, and the shell hears nothing.

There are four release bindings: `Alt_L` and `Alt_R`, each with and without
`SHIFT`. After `ALT`+`SHIFT`+`Tab` your fingers do not leave the two modifiers
at the same instant, so the `ALT` release often arrives with `SHIFT` still held.
That is a different modmask, the plain bindings would not match it, and the
switcher would sit open.

Every binding except `ALT`+`Tab` itself is **non-consuming**
(`non_consuming = true`, Hyprland's `n`). You press `ALT`+`Return` and
`ALT`+`Escape` with the switcher closed almost every time (Thunar opens
Properties on `ALT`+`Return`), so a consuming binding would take them away from
every application on the machine to serve a switcher that is not open. A
consumed `ALT` *release* is worse: that is how an application ends up believing
`ALT` is still held. `transparent` (`t`) is a different flag. It means "no
other binding can shadow this one", and a binding carrying only `t` still eats
the key. Three other plausible spellings (`nonConsuming`, `consume = false`,
`ignore_mods`) pass through `hl.bind` without complaint and leave
`non_consuming: false`, so the test suite asserts the value against a running
instance instead of trusting the config.

### The single tap

A quick `ALT`+`Tab` lets go of `ALT` about 60 ms after `Tab` goes down, and the
press and the release each start their own short-lived chain of processes: a
spawn, an `rime`, and a `qs ipc call`. The switcher handles two consequences:

* **The helper writes the flag file on `next`**, before anything reaches the shell.
  If the shell wrote it at the end of that chain, the release would arrive
  before the flag existed, the helper would drop the commit, and the switcher
  would stay open on the wrong entry.
* The two chains can still overtake each other, so the shell *remembers* a
  commit that arrives with nothing open for 600 ms, and a `next` that opens the
  switcher within that window commits at once. A stray `ALT` release cannot arm
  this: the helper forwards a commit only when the flag exists, and only
  `next`/`prev` create it.

A leftover flag (`next` with a single window open writes one and opens nothing)
costs one wasted IPC call, and that call removes it.

`ALT`+`Return` commits as well, as a fallback. No headless test can press the
release binding: Hyprland 0.56.2 accepts a synthetic keyboard (`wtype`,
virtual-keyboard-v1), reports it with `active keymap: error`, and fires no
binding from it. `ALT`+`Return` is an ordinary press binding, so if the release
semantics ever change, you still have a keyboard way to commit, and the
switcher cannot become something you can open but not close.

### The other two sessions keep their own switchers

Both findings come from measurements taken on 2026-09-20.

**labwc** has `onRelease="yes"`, and labwc-config(5) states its purpose: "the
action should fire when the modifier is used **without** another key". Holding
`ALT`, tapping `Tab` (which fires the `A-Tab` binding) and releasing `ALT` fires
an `Alt_L onRelease` binding zero times; pressing and releasing `ALT` with no
`Tab` fires it. That is the documented design, and it rules out a shell-drawn
hold-and-release switcher on labwc. labwc's own switcher does the job well, so
the Floating session keeps it, with `<action name="NextWindow" workspace="all"/>`
covering "all windows including other workspaces" for that session.

**labwc's switcher does not warp the pointer.** With
`<followMouse>yes</followMouse>`, `ALT`+`Tab` gives focus to the window you
picked, and the next mouse movement hands it to whatever is under the cursor:
the complaint this document opens with, in that session's `ALT`+`Tab`.
`<action name="WarpCursor" to="window"/>` exists and would fix it IF it ran
after the cycle rather than at key-press time, and nothing here can tell which.
labwc has no IPC, so a headless test cannot read the pointer's position back,
and an unverified warp risks moving the pointer to the OLD window before the
cycle starts. Rime leaves it undone and records it here.

**niri** 26.04 ships `recent-windows`: hold-and-release, MRU ordering, live
previews. niri has no key-release binding for the shell's switcher to borrow
either, so the Scrolling session keeps niri's switcher. Rime Shell writes the
two binds into `RimeShellKeybinds.kdl` instead of inheriting them, for the same
reason the Hyprland seed spells out `cursor { no_warps = false }` although it
is upstream's default: this is Rime's `ALT`+`Tab`, and a later niri release
should not be able to change it.

`ALT`+`Tab` therefore looks different in each session, by decision. Two of the
three compositors already switch windows well, and replacing a working native
switcher with a worse shell-drawn one to get a matching look would be the wrong
trade.

### Rebinding

The Keybinds page does **not** list `ALT`+`Tab`. The shell's keybind model has
no way to express a key *release*, so rebinding the opening shortcut there
would move `next` and leave the commit on `ALT`: a switcher you can open and
cannot close. Until the model can express a release, the combination stays
fixed in `rime/keybindings.lua` and `rc.xml`.
