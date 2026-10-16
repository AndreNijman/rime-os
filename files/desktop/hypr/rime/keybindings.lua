-- Rime keybinds. Loaded by ~/.config/hypr/hyprland.lua.
--
-- Rime-owned: `rime update` replaces this file.
--
-- Rime Shell popup binds (launcher, dashboard, notifications, wallpaper,
-- clipboard, audio, network, power menu, screen-record) are owned by the SHELL,
-- not here. It generates rime/shell-keybinds.lua from
-- ~/.config/rime-shell/src/user_data/keybinds.json and that module loads AFTER
-- this one. Binding them here too caused double-fires (SUPER+Q ran both
-- killactive AND the launcher).
--
-- ── How the shell overrides one of these ─────────────────────────────────────
-- Every bind below is registered in `M.binds` under a canonical combo key, and
-- `hl.bind` returns a handle with `:set_enabled(false)`. So a user who rebinds
-- SUPER+Q in Rime Settings gets the generated module calling
--
--     require("rime.keybindings").disable("SUPER", "Q")
--
-- before it binds its own. The hyprlang generator had to emit `unbind =` lines
-- for every Rime default on every write, whether or not the user had touched
-- them, because hyprlang had no way to disable a static bind — it could only
-- remove one by key and hope nothing else wanted it. Disabling by handle
-- touches exactly the bind that is being replaced.

local M = { binds = {} }

--- Split a modifier string on whitespace and `+` alike, uppercased.
--- The shell's keybinds.json spells combos "SUPER + SHIFT", the hyprlang config
--- spelled them "SUPER SHIFT", and both have to mean the same thing.
local function split_mods(mods)
    local parts = {}
    for word in tostring(mods or ""):upper():gmatch("[^%s+]+") do
        parts[#parts + 1] = word
    end
    return parts
end

--- Canonical key for a combo: modifiers uppercased and sorted, then the key.
--- "SUPER + SHIFT", "Q" and "shift super", "q" both give "SHIFT+SUPER|Q".
--- Sorted because the generated module writes modifiers in whatever order the
--- user's keybinds.json lists them, and "SUPER SHIFT" must match "SHIFT SUPER".
function M.key(mods, key)
    local parts = split_mods(mods)
    table.sort(parts)
    return table.concat(parts, "+") .. "|" .. tostring(key or ""):upper()
end

--- Turn off the Rime default bound to this combo. Returns true if there was one.
---
--- `:remove()` rather than `:set_enabled(false)`. Both stop the bind firing, and
--- both are handle methods — the point of a handle is that it addresses THIS
--- bind rather than "whatever is on this key", which is all `unbind` could do.
--- The difference is what the rest of the system sees: a disabled bind is still
--- listed by `hyprctl binds`, with no field saying it is inert, and Rime Shell
--- reads that list to warn about conflicting shortcuts. A ghost there makes
--- that list lie about what the keyboard actually does.
---
--- Nothing is lost by removing: `hyprctl reload` re-runs this whole file, so
--- every default comes back and is re-disabled by whatever claimed it.
function M.disable(mods, key)
    local id = M.key(mods, key)
    local handle = M.binds[id]
    if not handle then return false end
    handle:remove()
    M.binds[id] = nil
    return true
end

-- Registers the bind and remembers its handle. `mods` and `key` stay separate
-- so the canonical key can be computed from the same values Hyprland is given.
--
-- Every element of the combo is joined with " + ". hl.bind parses ONLY that
-- separator: passing "SUPER SHIFT + SPACE" fails with `Unknown keysym: "SUPER
-- SHIFT", did you forget a +?`, and it fails per bind rather than raising, so
-- the mistake shows up as a `hyprctl configerrors` entry and a dead shortcut
-- instead of a broken config.
local function bind(mods, key, dispatcher, opts)
    local parts = split_mods(mods)
    parts[#parts + 1] = key
    local handle = hl.bind(table.concat(parts, " + "), dispatcher, opts)
    M.binds[M.key(mods, key)] = handle
    return handle
end

local mod         = "SUPER"
local terminal    = "alacritty"
-- Not a browser name. `rime-open-browser` opens whatever the user has set as
-- their default, so installing another browser and making it default changes
-- this key too. It used to say `firefox`, which meant SUPER+W contradicted the
-- user's own setting the moment a second browser existed.
local browser     = "/usr/libexec/rime-open-browser"
local fileManager = "thunar"
local screenshot  = "bash /usr/share/rime-shell/src/scripts/screenshot.sh"

-- ── Apps and window management ───────────────────────────────────────────────
bind(mod, "T", hl.dsp.exec_cmd(terminal),    { description = "Terminal" })
bind(mod, "Q", hl.dsp.window.close(),        { description = "Close window" })
bind(mod, "W", hl.dsp.exec_cmd(browser),     { description = "Browser" })
bind(mod, "E", hl.dsp.exec_cmd(fileManager), { description = "File manager" })

-- Lock. Not a Rime Shell default keybind, so it is bound here.
bind(mod, "L", hl.dsp.exec_cmd("rime shell lock"), { description = "Lock session" })

-- ── Screenshots (grimblast) ──────────────────────────────────────────────────
bind("",  "Print", hl.dsp.exec_cmd(screenshot .. " area"),   { description = "Screenshot area" })
bind(mod, "Print", hl.dsp.exec_cmd(screenshot .. " screen"), { description = "Screenshot screen" })

-- ── Layout ───────────────────────────────────────────────────────────────────
bind(mod,           "F",     hl.dsp.window.fullscreen({ mode = 0 }),   { description = "Fullscreen" })
bind(mod .. " SHIFT", "SPACE", hl.dsp.window.float({ action = "toggle" }), { description = "Toggle floating" })
bind(mod,           "P",     hl.dsp.window.pseudo(),                   { description = "Pseudotile" })
bind(mod,           "J",     hl.dsp.layout("togglesplit"),             { description = "Toggle split" })

-- Focus with the arrow keys. Vim hjkl focus was removed: SUPER+l collided with
-- SUPER+L (lock), and letter keys are better left free for app/popup binds.
--
-- hyprlang took one-letter directions (`movefocus, l`); the Lua dispatcher takes
-- the direction spelled out.
local directions = { left = "left", right = "right", up = "up", down = "down" }
for key, direction in pairs(directions) do
    bind(mod, key, hl.dsp.focus({ direction = direction }),
         { description = "Focus " .. direction })
    bind(mod .. " SHIFT", key, hl.dsp.window.move({ direction = direction }),
         { description = "Move window " .. direction })
end

-- ── Alt-Tab window switcher ──────────────────────────────────────────────────
-- The Rime Shell switcher: every window on every workspace, most-recently-used
-- first. It is bound HERE rather than in the shell's keybind model, and that is
-- a deliberate limitation rather than an oversight — see below.
--
-- ── Hold Alt, tap Tab, release Alt ───────────────────────────────────────────
-- The release is what makes it an alt-tab rather than a menu, and the shell
-- cannot see it: a switcher that took keyboard focus in order to watch for the
-- Alt release would itself be taking focus away from the window it is about to
-- give focus to, which is the bug this whole unit exists to fix. So the
-- compositor reports the release instead, and the shell's overlay never asks
-- for the keyboard at all.
--
-- `release = true` is Hyprland's `bindr`.
--
-- ── `non_consuming`, not `transparent` ───────────────────────────────────────
-- The flag that lets the key ALSO reach the focused application is `n`,
-- `non_consuming = true`. `t` (`transparent = true`) means something else
-- entirely — this bind cannot be shadowed by another bind — and a bind carrying
-- only `t` still eats the key.
--
-- That distinction is not cosmetic here. ALT+Return and ALT+Escape are ordinary
-- press binds that exist for the switcher and are pressed with the switcher
-- CLOSED almost every time: Thunar opens Properties on ALT+Return, and browsers
-- use it in the address bar. A consuming bind would take both away from every
-- application on the machine, permanently, to serve a switcher that is not open.
-- A consumed ALT *release* is worse — that is how an application ends up
-- believing Alt is still held.
--
-- Checked as data rather than by eye: `hyprctl binds -j` reports
-- `non_consuming` per bind, tests/test-rime-hypr-focus.sh asserts it is true on
-- every one of these, and the three other spellings that look plausible
-- (`nonConsuming`, `consume = false`, `ignore_mods`) are all ACCEPTED by
-- hl.bind and all leave `non_consuming: false`. A wrong name here is silent.
--
-- Both Alt keys, because a keyboard has two and a user who started the switch
-- with the right-hand one has to be able to finish it.
--
-- And both with and without SHIFT: after ALT+SHIFT+Tab the fingers do not come
-- off the two modifiers at the same instant, so the Alt release very often
-- arrives with SHIFT still down. modmask 8 would not match it, and the switcher
-- would sit open until something else closed it.
--
-- ── ALT+Return commits too, and that is not decoration ───────────────────────
-- The release bind cannot be verified without a person: a synthetic keyboard
-- (wtype's virtual-keyboard-v1) is accepted by 0.56.2 and then reports
-- `active keymap: error`, and no bind fires from it at all — measured
-- 2026-09-20 — so nothing headless can press a real ALT and let go of it.
--
-- If `release = true` ever stops firing after a chord, the way labwc's
-- onRelease deliberately does not fire after one, the switcher would open and
-- have no keyboard way to close. ALT+Return is the floor under that: it is an
-- ordinary press bind, it cannot be affected by whatever the release semantics
-- are, and with `transparent = true` plus rime-switcher's flag test it costs a
-- closed switcher one `[ -e ]` and still reaches the application underneath.
--
-- ── The shell can move all of this ───────────────────────────────────────────
-- The shell's keybind model carries the switcher as ONE action ("window-switcher",
-- default ALT+Tab) and generates the whole set below from it: next, previous,
-- cancel, commit, and the release of whichever modifier the user chose. It
-- switches these defaults off with `disable()` before binding its own, so a
-- user who rebinds the switcher in Rime Settings does not get two. This block
-- stays as the shell-less default: a session without the shell's generated
-- module still has a working ALT+Tab.
--
-- ── The cost of the release bind, stated ─────────────────────────────────────
-- It fires on EVERY Alt release, all day, not only while the switcher is open.
-- That is why the target is /usr/libexec/rime-switcher and not `rime shell …`
-- directly: the helper's whole closed-state path is one `[ -e ]` test on a file
-- under XDG_RUNTIME_DIR and an exit. One short-lived shell per Alt release,
-- never the shell's IPC.
local switcher = "/usr/libexec/rime-switcher"
local commit   = { non_consuming = true, transparent = true,
                   description = "Commit the window switcher" }
local release  = { release = true, non_consuming = true, transparent = true,
                   description = "Commit the window switcher" }

-- Tab IS consumed: ALT+Tab is the window manager's, and an application that
-- also wanted it would be fighting the switcher on every press.
bind("ALT",       "Tab", hl.dsp.exec_cmd(switcher .. " next"),
     { description = "Window switcher" })
bind("ALT SHIFT", "Tab", hl.dsp.exec_cmd(switcher .. " prev"),
     { description = "Window switcher (reverse)" })

bind("ALT",       "Escape", hl.dsp.exec_cmd(switcher .. " cancel"),
     { non_consuming = true, transparent = true,
       description = "Cancel the window switcher" })
bind("ALT",       "Return", hl.dsp.exec_cmd(switcher .. " commit"), commit)

bind("ALT",       "Alt_L", hl.dsp.exec_cmd(switcher .. " commit"), release)
bind("ALT",       "Alt_R", hl.dsp.exec_cmd(switcher .. " commit"), release)
bind("ALT SHIFT", "Alt_L", hl.dsp.exec_cmd(switcher .. " commit"), release)
bind("ALT SHIFT", "Alt_R", hl.dsp.exec_cmd(switcher .. " commit"), release)

-- ── Workspaces ───────────────────────────────────────────────────────────────
-- 1-9 then 0, where 0 is workspace 10.
for i = 1, 10 do
    local key = tostring(i % 10)
    bind(mod, key, hl.dsp.focus({ workspace = i }),
         { description = "Workspace " .. i })
    bind(mod .. " SHIFT", key, hl.dsp.window.move({ workspace = i }),
         { description = "Move window to workspace " .. i })
end

-- Scratchpad / special workspace
bind(mod,             "S", hl.dsp.workspace.toggle_special("magic"),
     { description = "Toggle scratchpad" })
bind(mod .. " SHIFT", "S", hl.dsp.window.move({ workspace = "special:magic" }),
     { description = "Move window to scratchpad" })

-- Cycle workspaces with the mouse wheel
bind(mod, "mouse_down", hl.dsp.focus({ workspace = "e+1" }))
bind(mod, "mouse_up",   hl.dsp.focus({ workspace = "e-1" }))

-- ── Mouse drag move/resize ───────────────────────────────────────────────────
-- hyprlang's `bindm`, written the way upstream's /usr/share/hypr/hyprland.lua
-- writes it. window.drag() and window.resize() are PRESS binds: they start the
-- move on button-down and end it on button-up themselves (v0.56.2 sets
-- `releasePending` inside the dispatcher). `{ drag = true }` looks like the
-- right option and is the opposite: it forces `release = true`, so the bind
-- fires only on button-up and SUPER+drag does nothing at all. That is what
-- shipped from the Lua migration until 2026-09-27; measured in a nested 0.56.2
-- with a virtual pointer holding SUPER, `drag = true` moved no window and
-- `mouse = true` floated it and followed the pointer. `hyprctl binds -j`
-- reports `mouse=false` for both, which is why it was misread: the field that
-- tells them apart is `release` (true = broken).
bind(mod, "mouse:272", hl.dsp.window.drag(),   { mouse = true, description = "Move window" })
bind(mod, "mouse:273", hl.dsp.window.resize(), { mouse = true, description = "Resize window" })

-- ── Media and brightness keys ────────────────────────────────────────────────
-- CTRL+SUPER equivalents for keyboards without these keys are shell defaults,
-- so they arrive through rime/shell-keybinds.lua.
--
-- hyprlang `bindel` = locked + repeating (fires with the screen locked or off,
-- and repeats while held). `bindl` = locked only. Volume and brightness are
-- useless without the repeat; the transport keys must stay one-shot.
local held   = { locked = true, repeating = true }
local locked = { locked = true }

bind("", "XF86AudioRaiseVolume",  hl.dsp.exec_cmd("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+"), held)
bind("", "XF86AudioLowerVolume",  hl.dsp.exec_cmd("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"), held)
bind("", "XF86MonBrightnessUp",   hl.dsp.exec_cmd("brightnessctl set 5%+"), held)
bind("", "XF86MonBrightnessDown", hl.dsp.exec_cmd("brightnessctl set 5%-"), held)

bind("", "XF86AudioMute",    hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),   locked)
bind("", "XF86AudioMicMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"), locked)

-- Transport keys. These were only in the labwc session, which the two configs
-- are meant to keep in step.
bind("", "XF86AudioPlay", hl.dsp.exec_cmd("playerctl play-pause"), locked)
bind("", "XF86AudioNext", hl.dsp.exec_cmd("playerctl next"),       locked)
bind("", "XF86AudioPrev", hl.dsp.exec_cmd("playerctl previous"),   locked)

-- ── One table under both module names ────────────────────────────────────────
-- A hyprland.lua seeded by an APEX image is the user's file, so it is never
-- rewritten, and it still loads this module as `apex.keybindings`: since the
-- rename ~/.config/hypr/apex is a symlink to rime/. A generated module or a
-- user-overrides.lua can ask for it under the other name. Two names are two
-- package.loaded entries, and a `require` under the second one would run this
-- file again: every default bound twice (both fire), and a disable() that
-- reaches only one of the two copies.
--
-- So whichever name ran it, both names get THIS table, on every run. That is
-- also what keeps `hyprctl reload` honest: a loader clears only its own prefix
-- and re-runs this file, and the other name is overwritten here rather than
-- left serving the previous pass's handles, which Hyprland dropped.
package.loaded["rime.keybindings"] = M
package.loaded["apex.keybindings"] = M  -- rime-rename: keep — the module name APEX-seeded configs require

return M
