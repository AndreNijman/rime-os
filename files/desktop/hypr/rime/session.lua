-- Rime session: environment variables and autostart.
-- Loaded by ~/.config/hypr/hyprland.lua.
--
-- Rime-owned: `rime update` replaces this file.

-- ── Environment ──────────────────────────────────────────────────────────────
-- The greetd-launched session PATH is /usr/local/bin:/usr/bin:/usr/local/sbin
-- and does NOT include ~/.local/bin — which is where the awww/awww-daemon
-- wallpaper shims live. Without this the wallpaper daemon is not found at boot
-- and the shell's `awww img` calls fail.
--
-- The hyprlang config had @HOME@ substituted in here at seed time. Lua can read
-- the environment itself, so the seeded file is now identical for every user and
-- keeps working if the home directory is ever moved.
local home = os.getenv("HOME") or ""
hl.env("PATH", home .. "/.local/bin:/usr/local/bin:/usr/bin:/usr/local/sbin")

hl.env("XCURSOR_SIZE", "24")
hl.env("HYPRCURSOR_SIZE", "24")
-- Hyprland loads the XCursor theme named by XCURSOR_THEME, or one called
-- "default" when it is unset (CursorManager.cpp). Nothing provides an
-- icons/default theme on NixOS, so every shape fell back to Hyprland's built-in
-- arrow with a warning each (951 in one VM session). Adwaita ships in the image
-- (adwaita-icon-theme); a theme the user set before login is kept. hl.env runs
-- while the config loads, which is before the cursor manager is created.
if not os.getenv("XCURSOR_THEME") then
    hl.env("XCURSOR_THEME", "Adwaita")
end

-- ── Autostart ────────────────────────────────────────────────────────────────
-- There is no `exec-once` keyword in the Lua config. The replacement is a
-- handler on the `hyprland.start` event, which fires once per compositor start
-- and — like exec-once before it — does NOT re-fire on `hyprctl reload`.
hl.on("hyprland.start", function()
    -- awww-daemon by absolute name so the wallpaper is restored at boot
    -- regardless of PATH resolution timing.
    hl.exec_cmd("awww-daemon")

    -- hypridle + Rime Shell are started by one launcher rather than by two
    -- lines of their own. It resolves hypridle's config (image copy by default,
    -- ~/.config/hypr/hypridle.conf as an explicit override) and is idempotent,
    -- so it is also safe to run by hand to repair a live session.
    --
    -- The shell ships INSIDE the image at /usr/share/rime-shell (rime-logs 56).
    -- It used to be cloned per-user on first login, which meant these autostarts
    -- fired 2-5s before the clone finished and died — a first login with no bar,
    -- no popups and no idle management (rime-logs 51). Vendoring makes that race
    -- structurally impossible: the shell is present before the compositor is
    -- ever exec'd, and the UI and the OS move together on one update channel.
    hl.exec_cmd("/usr/libexec/rime-shell-autostart")

    hl.exec_cmd("/usr/libexec/polkit-mate-authentication-agent-1")
    hl.exec_cmd("wl-paste --type text  --watch cliphist store")
    hl.exec_cmd("wl-paste --type image --watch cliphist store")

    -- Input method (fcitx5, baked into the image). greetd execs Hyprland
    -- directly, so nothing here processes /etc/xdg/autostart — the IME has to be
    -- started by the compositor or CJK/Indic/Cyrillic users cannot type at all.
    -- Guarded so it is a silent no-op if fcitx5 is ever absent. `-d` daemonises,
    -- `-r` replaces a stale instance.
    hl.exec_cmd("command -v fcitx5 >/dev/null && fcitx5 -d -r")

    -- Smooth window and workspace animations on battery without changing the
    -- tier: on every open/close/workspace event this asks rimed for a short UI
    -- boost (org.rimeos.Rimed1.Ui.Boost, capped at 600 ms, rate limited; see
    -- docs/rimed-dbus.md). It asks without waiting for an answer and exits with
    -- the compositor.
    hl.exec_cmd("/usr/bin/rime ui hypr-boost")
end)
