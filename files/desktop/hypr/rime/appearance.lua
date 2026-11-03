-- Rime look and feel. Loaded by ~/.config/hypr/hyprland.lua.
--
-- Rime-owned: `rime update` replaces this file. Put your own overrides in
-- hyprland.lua, which is yours and is never rewritten — it loads after nothing
-- and before nothing that would undo them, because hl.config is last-write-wins.

hl.config({
    general = {
        gaps_in     = 5,
        gaps_out    = 10,
        border_size = 2,

        -- The active border is re-tinted from the wallpaper by Rime Shell's
        -- matugen pipeline (WallpaperService.updateBorders), which pushes the
        -- new colour over `hyprctl keyword` at runtime. These are the values a
        -- session starts with before any wallpaper has been picked.
        --
        -- The hyprlang spelling was one string with a trailing `45deg`:
        --     col.active_border = rgba(a1c999ee) rgba(8d748cee) 45deg
        -- A gradient in Lua is a table of stops plus an angle instead.
        col = {
            active_border   = { colors = { "rgba(a1c999ee)", "rgba(8d748cee)" }, angle = 45 },
            inactive_border = "rgba(252733aa)",
        },

        layout           = "dwindle",
        resize_on_border = true,
    },

    decoration = {
        rounding = 10,
        blur   = { enabled = true, size = 6, passes = 2 },
        shadow = { enabled = true, range = 8, render_power = 3 },
    },

    animations = {
        enabled = true,
    },

    dwindle = {
        preserve_split = true,
    },

    misc = {
        disable_hyprland_logo    = true,
        disable_splash_rendering = true,
        force_default_wallpaper  = 0,
    },

    -- Hyprland's own "updated to <version>" window (hyprland-update-screen) at the
    -- first start after its version moves. Hyprland moves only with a Rime release,
    -- whose notes are the "what's new" (the page the Shell opens), and Rime's config
    -- changes are migrated for the user: the window would announce an upstream
    -- release nobody chose. Off, like the logo and splash above. (Hyprland's
    -- twice-a-year donation window, ecosystem.no_donation_nag, is left on.)
    ecosystem = {
        no_update_news = true,
    },
})

-- ── Motion ───────────────────────────────────────────────────────────────────
-- The compositor moves on the same beats and curves as Rime Shell (UI/UX
-- roadmap v3 Phase 20, retuned 2026-09-26 with the shell's fluid motion, and
-- 2026-09-27 onto real springs), so a window opening and a panel opening read
-- as one system. The curves are the shell's own tokens (rime-shell
-- src/theme/motion.js, at the default "balanced" speed); a speed here is in
-- tenths of a second. The shell scales every one of them by its own speed
-- setting at runtime (hyprMotion.js).
--
-- Andre, 2026-09-27: "make the … hyprland window animations cleaner and sleek,
-- not just fading." Measured in a nested Hyprland 0.56.2 with alacritty, the
-- 2026-09-26 tuning was mostly a fade: a window's first frame was already 95 %
-- of its size and 61 % opaque, and a close faded over the whole of its 280 ms
-- while shrinking by only a tenth. Now the SIZE carries the change:
--
--   arriving  grows from 80 % on a spring, opaque within 180 ms     rimeArrive
--   leaving   shrinks to 85 %, gathering speed, gone in 200 ms      emphasizedAccel
--             its opacity held until the end                     exitFade
--   moving    a tile glides to its place, velocity kept            rimeGlide
--   a switch  the workspace slides, and stops without a crawl      rimePage
--   fading    Core Animation's ease, no hard edge                  effects
--
-- A spring is not a curve over a duration. Hyprland steps it in real time and
-- it finishes when it is at rest, so a window moved again half-way keeps its
-- velocity instead of restarting from rest and kinking; `speed` does not time
-- it. On a spring leaf `speed` is the spring's response (its undamped period,
-- in tenths of a second): Hyprland ignores it, and the shell scales it with
-- the curve so it can tell its own push from a reload.
--
-- Exits stay on a curve. A spring finishes only when it is still (Hyprland's
-- rest threshold, about 0.7 s for these), and a closing window lives until its
-- animation ends.
--
-- hyprlang carried these as `bezier =` / `animation =` keys; each is its own
-- call now, `enabled / speed / curve` becoming named fields.
hl.curve("rimeSpring",      { type = "bezier", points = { { 0.25, 0.2 }, { 0.15, 1.0 } } })
hl.curve("rimeAccel",       { type = "bezier", points = { { 0.4, 0.0 }, { 0.65, 1.0 } } })
hl.curve("rimeStandard",    { type = "bezier", points = { { 0.25, 0.1 }, { 0.25, 1.0 } } })
hl.curve("rimeEffects",     { type = "bezier", points = { { 0.25, 0.1 }, { 0.25, 1.0 } } })
hl.curve("emphasizedAccel", { type = "bezier", points = { { 0.4, 0.0 }, { 0.75, 0.9 } } })
-- A closing window's opacity: Material 3's own emphasized-accelerate, harder
-- than the shell's. It holds (84 % at half-time) and lets go at the end, so
-- the shrink is what is seen; on the shell's softer curve the same close read
-- as a crossfade with a slight zoom.
hl.curve("exitFade",        { type = "bezier", points = { { 0.3, 0.0 }, { 0.8, 0.15 } } })

-- The springs. Parameterised as the shell's are (motion.js SPRINGS): a
-- response in seconds and a damping fraction, 1 landing without overshoot and
-- below it with a whisper of one. Hyprland takes mass, stiffness and damping
-- instead; with mass 1, stiffness = (2π / response)² and dampening =
-- 2 · damping · √stiffness. (The key is `dampening`: this Hyprland rejects
-- `damping`, whatever its wiki says.)
--
-- Each is also kept in RIME_SPRINGS, a global the shell's own `hyprctl eval`
-- can read: Hyprland reports a spring leaf as "spring:<name>" and nothing
-- more, so this table is how the shell scales a spring by its speed setting
-- (stiffness / s², dampening / s: the same motion, s times slower) without
-- a copy of these numbers of its own.
RIME_SPRINGS = {}
local function spring(name, response, damping)
    local stiffness = (2 * math.pi / response) ^ 2
    local s = { mass = 1, stiffness = stiffness, dampening = 2 * damping * math.sqrt(stiffness) }
    RIME_SPRINGS[name] = s
    hl.curve(name, { type = "spring", mass = s.mass, stiffness = s.stiffness, dampening = s.dampening })
end
-- A window arriving: response 0.48 s, damping 0.8. Slower than the 2026-09-26
-- curve on purpose: a new window's first frame reaches the screen a few frames
-- after it maps (50-100 ms in the nested measurement), and on a faster spring
-- that gap swallowed half the growth. It lands 1.5 % past its size and settles.
spring("rimeArrive", 0.48, 0.80)
-- A tile gliding to a new place: the shell's selection spring (0.40 s, 0.86).
spring("rimeGlide",  0.40, 0.86)
-- A workspace sliding: the shell's page response (0.38 s), damped 0.92 rather
-- than 1 — a critically damped slide crawls its last 60 px for 300 ms; this
-- one overshoots by about a pixel and is still by 330 ms.
spring("rimePage",   0.38, 0.92)

-- Windows: in on the arrival spring from 80 %, out in 200 ms to 85 %; a window
-- moving (a tile reflowing, a float dragged into a slot) glides.
hl.animation({ leaf = "windowsIn",   enabled = true, speed = 4.8, spring = "rimeArrive",      style = "popin 80%" })
hl.animation({ leaf = "windowsOut",  enabled = true, speed = 2.0, bezier = "emphasizedAccel", style = "popin 85%" })
hl.animation({ leaf = "windowsMove", enabled = true, speed = 4.0, spring = "rimeGlide" })

-- Fades: a window is opaque within 180 ms of opening, well before it has
-- grown, so the growth is what is seen; closing, it holds its opacity while
-- it shrinks and lets go at the end. Focus and dim changes on the state beat
-- (220 ms); an application's own menus quicker (micro, 140 ms).
hl.animation({ leaf = "fadeIn",     enabled = true, speed = 1.8, bezier = "rimeEffects" })
hl.animation({ leaf = "fadeOut",    enabled = true, speed = 2.0, bezier = "exitFade" })
hl.animation({ leaf = "fadeSwitch", enabled = true, speed = 2.2, bezier = "rimeEffects" })
hl.animation({ leaf = "fadeShadow", enabled = true, speed = 2.2, bezier = "rimeEffects" })
hl.animation({ leaf = "fadeDim",    enabled = true, speed = 2.2, bezier = "rimeEffects" })
hl.animation({ leaf = "fadePopups", enabled = true, speed = 1.4, bezier = "rimeEffects" })

-- Other programs' layer surfaces (a wallpaper daemon, an input method, a
-- third-party launcher) fade on the small-surface beats. Rime Shell's own are
-- exempt below: it draws every one of their motions itself.
hl.animation({ leaf = "layersIn",      enabled = true, speed = 3.6, bezier = "rimeSpring",  style = "fade" })
hl.animation({ leaf = "layersOut",     enabled = true, speed = 2.4, bezier = "rimeAccel",   style = "fade" })
hl.animation({ leaf = "fadeLayersIn",  enabled = true, speed = 3.6, bezier = "rimeEffects" })
hl.animation({ leaf = "fadeLayersOut", enabled = true, speed = 2.4, bezier = "rimeAccel" })

-- Workspaces: a keyboard switch slides on the page spring. A touchpad swipe
-- (input-defaults.lua) follows the fingers directly and uses it only for the
-- settle after release.
hl.animation({ leaf = "workspaces",       enabled = true, speed = 3.8, spring = "rimePage", style = "slide" })
hl.animation({ leaf = "specialWorkspace", enabled = true, speed = 3.8, spring = "rimePage", style = "slidefadevert 15%" })

-- A focus change re-colours the border on the state beat; it was 600 ms.
hl.animation({ leaf = "border", enabled = true, speed = 2.2, bezier = "rimeStandard" })

-- ── Rime Shell's surfaces are not animated by the compositor ────────────────
-- Every one of them — the bar, a panel pouring out of it, the OSD, a toast —
-- is drawn in motion by the shell, frame by frame, and a panel's first frame
-- is pixel-identical to the bar it grows out of. Hyprland fades every layer
-- surface in on map (fadeLayersIn, inherited from `fade`), so without this a
-- panel poured out of the bar translucent: measured in a nested Hyprland
-- 0.56.2, 66-90 % of the network panel's footprint was a blend of panel and
-- wallpaper from 240 to 345 ms into its open, and it snapped opaque at 400 ms.
-- With the rule, the blend is the shell's own content fade at its edges (<= 7 %).
-- The shell's layers all carry Quickshell's default namespace.
hl.layer_rule({
    name  = "rime-shell-draws-its-own-motion",
    match = { namespace = "^quickshell$" },

    no_anim = true,
})
