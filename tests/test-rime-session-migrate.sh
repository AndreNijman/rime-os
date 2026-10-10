#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  /usr/libexec/rime-session-migrate against an APEX-shaped home.
#
#  The first login of an APEX account on a Rime OS image must lose nothing and
#  break nothing. So the fixture below is a home the way an APEX image left it
#  (the loader an APEX image seeded into hyprland.lua, modules and generated
#  files under hypr/apex, the Remote identity key under ~/.local/state/apex,
#  enabled apex-* user units, the niri/labwc/zsh files firstrun seeded), plus a
#  fake image root with the Rime paths. The script runs against both, and then:
#
#    * every directory moved, the old name a symlink, the identity key intact;
#    * the user's own hyprland.lua, NOT rewritten beyond one line, still
#      resolves every module — checked by running it in a Lua interpreter with
#      a stub `hl` that counts binds, twice, the second pass standing in for
#      `hyprctl reload` — with no bind doubled and no user rebind lost;
#    * Hyprland --verify-config accepts it (when Hyprland is installed);
#    * a second run changes nothing at all;
#    * the conflict, empty-new-directory, dotfiles-symlink and fresh-home cases.
#
#  Then each defect the script exists to prevent is put back, one at a time
#  (a mutant of the script, or of the shipped keybindings.lua), and the check
#  that guards it has to FAIL. A suite whose checks cannot fail proves nothing.
#
#  Needs no root and no network; never touches the real home or user manager
#  (systemctl is a stub that records its argv). Run from the repository root:
#      ./tests/test-rime-session-migrate.sh
# ─────────────────────────────────────────────────────────────────────────────
# Every assertion is a string `check` evals, so shellcheck sees the variables
# those strings read as unused (SC2034), and the `~` in a label a human reads as
# an unexpanded tilde (SC2088). Both are the shape of the harness, not defects.
# shellcheck disable=SC2034,SC2088
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${ROOT}/files/system/libexec/rime-session-migrate"
HYPR_SRC="${ROOT}/files/desktop/hypr"
[ -f "$SCRIPT" ] || { printf 'missing %s\n' "$SCRIPT" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '      %s\n' "$2"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }
check() { if eval "$2"; then ok "$1"; else bad "$1" "${3:-}"; fi; }

LUA="$(command -v lua5.4 || command -v lua || command -v luajit || true)"

# ── the image root ───────────────────────────────────────────────────────────
IMG="${WORK}/img"
mk() { mkdir -p "$(dirname "$1")"; printf '%s\n' "${2:-x}" > "$1"; }
mk "${IMG}/usr/share/rime-shell/shell.qml"
mk "${IMG}/usr/share/rime-shell/src/scripts/screenshot.sh"
mk "${IMG}/usr/share/rime-shell/src/config/hypridle.conf" \
   'general { lock_cmd = qs -c /usr/share/rime-shell ipc call lockscreen lock }'
for b in rime-shell-autostart rime-open-browser rime-screen-reader rime-desktop-menu; do
    mk "${IMG}/usr/libexec/${b}"
done
mk "${IMG}/usr/share/rime/shell/agent.sh"
mk "${IMG}/usr/share/rime/shell/greeting.sh"
mk "${IMG}/usr/bin/rime"
for u in rime-agentd.service rime-remoted.service rime-aid.service \
         rime-storage-notice.service rime-storage-notice.timer; do
    mk "${IMG}/usr/lib/systemd/user/${u}"
done
mkdir -p "${IMG}/usr/share/rime/hypr/rime"
cp "${HYPR_SRC}/rime/"*.lua "${IMG}/usr/share/rime/hypr/rime/"

# systemctl stand-in: records, never reaches a user manager.
STUB="${WORK}/systemctl"
cat > "$STUB" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "${RIME_TEST_SYSTEMCTL_LOG:?}"
EOF
chmod +x "$STUB"

# ── an APEX-shaped home ──────────────────────────────────────────────────────
# The loader every APEX image seeded into ~/.config/hypr/hyprland.lua, code
# lines verbatim (795a1face:files/desktop/hypr/hyprland.lua, comments dropped).
# rime-rename: keep — the block below is APEX-era content on purpose.
old_hyprland_lua() {
    cat <<'EOF'
-- APEX-OS — default Hyprland config (seeded per-user by apex-shell-firstrun).
local failures = {}
for name in pairs(package.loaded) do
    if name:sub(1, 5) == "apex." then package.loaded[name] = nil end
end
local function apex(name)
    local module = "apex." .. name
    if not package.searchpath(module, package.path) then
        return nil          -- generated, not written yet
    end
    local ok, result = pcall(require, module)
    if ok then return result end
    failures[#failures + 1] = ("apex/%s.lua: %s"):format(name, tostring(result))
    return nil
end
apex("appearance")        -- look and feel, animations
apex("session")           -- environment variables, autostart
apex("input-defaults")    -- keyboard layout and input fallbacks
apex("monitors-default")  -- the catch-all monitor rule
apex("rules")             -- window rules
apex("keybindings")       -- APEX keybinds; returns handles the shell can disable
apex("monitors")        -- generated by apex-display-apply   (Settings → Display)
apex("input")           -- generated by apex-input-apply     (Settings → Input)
apex("shell-keybinds")  -- generated by APEX Shell           (Settings → Keybinds)
apex("user-overrides")
-- A bind of the user's own, with an image path in it.
hl.bind("SUPER + F10", hl.dsp.exec_cmd("/usr/libexec/apex-screen-reader toggle"))
if #failures > 0 then
    error("APEX config modules failed to load:\n  " .. table.concat(failures, "\n  "), 0)
end
EOF
}

# The shell's generated module, as the APEX shell wrote it and as Rime Shell
# will. $1 = module prefix ("apex" or "rime").
shell_keybinds_lua() {
    cat <<EOF
local ok, defaults = pcall(require, "$1.keybindings")
if ok and defaults then defaults.disable("SUPER", "W") end
hl.bind("SUPER + W", hl.dsp.exec_cmd("qs -p /usr/share/$1-shell ipc call launcher toggle"))
pcall(require, "$1.shell-keybinds-user")
EOF
}

make_home() {   # make_home <dir>
    local H="$1"
    mkdir -p "$H"
    # Hyprland: the loader, APEX-image modules, generated and user files.
    mkdir -p "$H/.config/hypr/apex" "$H/.config/hypr/shaders"
    old_hyprland_lua > "$H/.config/hypr/hyprland.lua"
    local f
    for f in "${HYPR_SRC}/rime/"*.lua; do
        # An APEX image's copy of each module: the same file in the old names,
        # without the both-names block this image added to keybindings.lua.
        sed -e 's/rime/apex/g; s/Rime/APEX/g' -e '/^package\.loaded\[/d' \
            -e 's/@KB_LAYOUT@/us/; s/@KB_VARIANT@//' "$f" \
            > "$H/.config/hypr/apex/$(basename "$f")"
    done
    shell_keybinds_lua apex > "$H/.config/hypr/apex/shell-keybinds.lua"
    printf 'hl.bind("SUPER + F12", hl.dsp.exec_cmd("my-thing"))\n' \
        > "$H/.config/hypr/apex/shell-keybinds-user.lua"
    cat > "$H/.config/hypr/apex/user-overrides.lua" <<'EOF'
local ok, kb = pcall(require, "apex.keybindings")
if ok and kb then kb.disable("SUPER", "T") end
hl.bind("SUPER + T", hl.dsp.exec_cmd("kitty"))
EOF
    printf 'hl.monitor({ output = "eDP-1", mode = "preferred", position = "0x0", scale = 1.5 })\n' \
        > "$H/.config/hypr/apex/monitors.lua"
    printf 'hl.config({ input = { sensitivity = -0.25 } })\n' > "$H/.config/hypr/apex/input.lua"
    printf -- '-- a backup the user made\n' > "$H/.config/hypr/apex/appearance.lua.bak-mine"
    # A customised hypridle.conf: the user's lines, and a lock_cmd into the shell.
    cat > "$H/.config/hypr/hypridle.conf" <<'EOF'
# mine
general {
    lock_cmd = qs -p ~/.local/share/apex-shell-live ipc call lockscreen lock || qs -c /usr/share/apex-shell ipc call lockscreen lock
}
listener { timeout = 600 }
EOF
    # State, config, data: the OS side.
    mkdir -p "$H/.local/state/apex/remote" "$H/.local/state/apex/agent/sessions"
    head -c 32 /dev/urandom > "$H/.local/state/apex/remote/identity.key"
    printf '[{"id":"phone"}]\n' > "$H/.local/state/apex/remote/devices.json"
    printf '{}\n' > "$H/.local/state/apex/agent/sessions/s1.json"
    printf 'deadbeef\n' > "$H/.local/state/apex/hypridle.conf.sha256"
    mkdir -p "$H/.config/apex" "$H/.local/share/apex/env"
    printf '{"default":"claude"}\n' > "$H/.config/apex/agent.json"
    printf '{"name":"rust"}\n' > "$H/.local/share/apex/env/rust.json"
    # The shell's directories (moved at directory level only).
    mkdir -p "$H/.config/apex-shell/src/user_data" "$H/.cache/apex-shell" "$H/.local/state/apex-shell"
    printf '{"reduceMotion":true}\n' > "$H/.config/apex-shell/src/user_data/settings.json"
    printf 'input {\n    touchpad {\n        tap\n    }\n}\n' > "$H/.config/apex-shell/ApexShellInput.kdl"
    printf 'binds {\n    Mod+L { spawn "qs" "-p" "/usr/share/apex-shell" "ipc" "call" "lockscreen" "lock"; }\n}\n' \
        > "$H/.config/apex-shell/ApexShellKeybinds.kdl"
    printf 'format = "$all"\n' > "$H/.cache/apex-shell/starship.toml"
    ln -s "$H/.cache/apex-shell/starship.toml" "$H/.config/starship.toml"
    # niri, as firstrun left it.
    mkdir -p "$H/.config/niri"
    cat > "$H/.config/niri/config.kdl" <<EOF
input {
    keyboard {
        xkb {
            layout "us"
        }
    }
}
// spawn-at-startup "waybar"   // disabled by APEX: quickshell is this system's bar

// ── APEX Shell autostarts (seeded by apex-shell-firstrun) ──
spawn-at-startup "awww-daemon"
spawn-at-startup "/usr/libexec/apex-shell-autostart"

// ── APEX generated configs (apex-shell-firstrun) ──
// Regenerated by APEX Settings and APEX Shell — do not hand-edit the targets.
// Positional: these come last, so they win over the sections above.
include "$H/.config/apex-shell/ApexShellInput.kdl"
include "$H/.config/apex-shell/ApexShellKeybinds.kdl"
EOF
    # labwc, as firstrun and apex-labwc-keybinds left it.
    mkdir -p "$H/.config/labwc"
    cat > "$H/.config/labwc/rc.xml" <<'EOF'
<?xml version="1.0"?>
<labwc_config>
  <keyboard>
    <!-- APEX-KEYBINDS-BEGIN — generated by apex-labwc-keybinds. Do not edit. -->
    <keybind key="A-space"><action name="Execute" command="apex shell launcher"/></keybind>
    <!-- APEX-KEYBINDS-END -->
    <keybind key="W-l"><action name="Execute" command="qs -p /usr/share/apex-shell ipc call lockscreen lock"/></keybind>
    <keybind key="W-w"><action name="Execute" command="/usr/libexec/apex-open-browser"/></keybind>
    <keybind key="W-p"><action name="Execute" command="bash /home/u/.local/bin/apex-screenshot area"/></keybind>
  </keyboard>
</labwc_config>
EOF
    printf '<openbox_menu><item><action name="Execute" command="apex shell lock"/></item></openbox_menu>\n' \
        > "$H/.config/labwc/menu.xml"
    printf '#!/bin/sh\n# /usr/libexec/apex-shell-autostart is the launcher\n/usr/libexec/apex-shell-autostart &\n' \
        > "$H/.config/labwc/autostart"
    # zsh, as firstrun seeded it, plus a line of the user's.
    cat > "$H/.zshrc" <<'EOF'
#  APEX-OS — default zsh configuration
alias apex-update='sudo bootc upgrade'
[[ -r /usr/share/apex/shell/agent.sh ]] && source /usr/share/apex/shell/agent.sh
[[ -r /usr/share/apex/shell/greeting.sh ]] && source /usr/share/apex/shell/greeting.sh
alias ll='eza -l'
EOF
    # Per-user units: two image units enabled, one timer, one masked, one
    # drop-in, and a unit of the user's own that merely starts with apex-.
    local U="$H/.config/systemd/user"
    mkdir -p "$U/default.target.wants" "$U/timers.target.wants" "$U/apex-agentd.service.d"
    ln -s /usr/lib/systemd/user/apex-agentd.service "$U/default.target.wants/apex-agentd.service"
    ln -s /usr/lib/systemd/user/apex-remoted.service "$U/default.target.wants/apex-remoted.service"
    ln -s /usr/lib/systemd/user/apex-storage-notice.timer "$U/timers.target.wants/apex-storage-notice.timer"
    ln -s /dev/null "$U/apex-aid.service"
    printf '[Service]\nEnvironment=FOO=1\n' > "$U/apex-agentd.service.d/override.conf"
    printf '[Service]\nExecStart=%s/.local/bin/apex-wip-snapshot\n' "$H" > "$U/apex-wip-snapshot.service"
    ln -s "$U/apex-wip-snapshot.service" "$U/default.target.wants/apex-wip-snapshot.service"
    printf '[Timer]\nOnCalendar=*:0/3\n' > "$U/apex-wip-snapshot.timer"
    ln -s "$U/apex-wip-snapshot.timer" "$U/timers.target.wants/apex-wip-snapshot.timer"
    mkdir -p "$U/apex-remoted.service.d"
    printf '[Service]\nEnvironment=RUST_LOG=debug\n' > "$U/apex-remoted.service.d/debug.conf"
    # …and one whose name the image DOES ship a rime- twin of: still the user's.
    printf '[Service]\nExecStart=/bin/true\n' > "$U/apex-storage-notice.service"
    ln -s "$U/apex-storage-notice.service" "$U/default.target.wants/apex-storage-notice.service"
    # Things that merely share the prefix and are not ours.
    mkdir -p "$H/.local/share/apex-shell-live" "$H/.config/apex-migration-backup-2026-07-30"
    printf 'x\n' > "$H/.local/share/apex-shell-live/shell.qml"
}

# run <home> [extra args] — the script, isolated from the real account.
run() {
    local H="$1"; shift
    mkdir -p "$H.run"
    env -i PATH=/usr/bin:/bin HOME="$H" XDG_RUNTIME_DIR="$H.run" \
        RIME_MIGRATE_SYSTEMCTL="$STUB" RIME_TEST_SYSTEMCTL_LOG="$H.systemctl" \
        python3 "${SCRIPT_UNDER_TEST:-$SCRIPT}" --home "$H" --root "$IMG" "$@" > "$H.out" 2>&1
}

# A listing of everything in a home: path, type, link target or checksum.
snapshot() {
    (cd "$1" && find . -printf '%p %y %l\n' | sort | while read -r p t l; do
        if [ "$t" = f ]; then printf '%s f %s\n' "$p" "$(sha256sum < "$p" | cut -c1-16)"
        else printf '%s %s %s\n' "$p" "$t" "$l"; fi
    done)
}

# ── the Lua check: does the migrated config load, and bind what it should? ──
cat > "${WORK}/harness.lua" <<'EOF'
local dir = arg[1]
package.path = dir .. "/?.lua;" .. dir .. "/?/init.lua;" .. package.path
local function any()
    return setmetatable({}, { __index = function() return any() end,
                              __call = function() return any() end })
end
local handles
local hl = any()
rawset(hl, "bind", function(combo, _, _)
    local parts = {}
    for w in combo:upper():gmatch("[^%s+]+") do parts[#parts + 1] = w end
    local key = table.remove(parts)
    table.sort(parts)
    local h = { live = true, combo = table.concat(parts, "+") .. "|" .. key }
    function h:remove() self.live = false end
    function h:set_enabled(v) self.live = v end
    handles[#handles + 1] = h
    return h
end)
rawset(hl, "get_config", function() return nil end)
_G.hl = hl
for pass = 1, 2 do                 -- pass 2 is `hyprctl reload`: same Lua state
    handles = {}
    local ok, err = pcall(dofile, dir .. "/hyprland.lua")
    local live, dups = {}, {}
    for _, h in ipairs(handles) do
        if h.live then
            if live[h.combo] then dups[#dups + 1] = h.combo end
            live[h.combo] = (live[h.combo] or 0) + 1
        end
    end
    local n = 0
    for _ in pairs(live) do n = n + 1 end
    print(("pass%d ok=%s live=%d dups=%d F12=%s F10=%s T=%s W=%s"):format(pass, tostring(ok), n,
        #dups, tostring(live["SUPER|F12"] or 0), tostring(live["SUPER|F10"] or 0),
        tostring(live["SUPER|T"] or 0), tostring(live["SUPER|W"] or 0)))
    if not ok then print("error: " .. tostring(err):gsub("\n", " | ")) end
end
EOF
lua_report() { "$LUA" "${WORK}/harness.lua" "$1/.config/hypr" 2>&1; }
# The shape a healthy config has, on BOTH passes: it loaded, the ~60 image
# binds are live, none twice, and the user's F12 (shell-keybinds-user), F10
# (hyprland.lua), T (user-overrides rebind) and W (shell rebind) each exactly once.
lua_healthy() {
    local r; r="$(lua_report "$1")"
    for p in 1 2; do
        printf '%s\n' "$r" | pipe_has -E "^pass${p} ok=true live=[5-9][0-9] dups=0 F12=1 F10=1 T=1 W=1$" \
            || return 1
    done
}

# ═════════════════════════════════════════════════════════════════════════════
sec "an APEX home, first login"
H="${WORK}/home"
make_home "$H"
key_before="$(sha256sum < "$H/.local/state/apex/remote/identity.key")"
inode_zshrc="$(stat -c %i "$H/.zshrc")"
run "$H" --start-units; rc=$?
check "the migration succeeds" '[ "$rc" = 0 ]' "$(cat "$H.out")"

for pair in ".local/state/apex:rime" ".config/apex:rime" ".local/share/apex:rime" \
            ".config/hypr/apex:rime" ".config/apex-shell:rime-shell" \
            ".cache/apex-shell:rime-shell" ".local/state/apex-shell:rime-shell"; do
    old="${pair%%:*}"; new="$(dirname "$old")/${pair##*:}"
    check "$old moved to $new" '[ -d "$H/$new" ] && [ ! -L "$H/$new" ]'
    check "…and $old is a relative link to it" '[ -L "$H/$old" ] && [ "$(readlink "$H/$old")" = "${pair##*:}" ]'
done
check "the Remote identity key is byte-for-byte the one phones paired with" \
    '[ "$(sha256sum < "$H/.local/state/rime/remote/identity.key")" = "$key_before" ]'
check "paired devices and agent sessions came with it" \
    '[ -f "$H/.local/state/rime/remote/devices.json" ] && [ -f "$H/.local/state/rime/agent/sessions/s1.json" ]'
check "~/.config/rime/agent.json and the capsule records are in place" \
    '[ -f "$H/.config/rime/agent.json" ] && [ -f "$H/.local/share/rime/env/rust.json" ]'
check "the shell's files keep their names inside the moved directory" \
    '[ -f "$H/.config/rime-shell/src/user_data/settings.json" ] && [ -f "$H/.config/rime-shell/ApexShellInput.kdl" ]'
check "~/.local/share/apex-shell-live is not ours and is untouched" \
    '[ -d "$H/.local/share/apex-shell-live" ] && [ ! -L "$H/.local/share/apex-shell-live" ] && [ ! -e "$H/.local/share/rime-shell-live" ]'
check "~/.config/apex-migration-backup-* is not ours and is untouched" \
    '[ -d "$H/.config/apex-migration-backup-2026-07-30" ] && [ ! -L "$H/.config/apex-migration-backup-2026-07-30" ]'

sec "Hyprland"
check "hyprland.lua still loads its modules by the APEX names" \
    'grep -q "^apex(\"keybindings\")" "$H/.config/hypr/hyprland.lua"'
check "…and its cache clear now clears both prefixes" \
    'grep -qF "== \"apex.\" or name:sub(1, 5) == \"rime.\" then" "$H/.config/hypr/hyprland.lua"'
check "the user's own bind follows the moved image path" \
    'grep -qF "/usr/libexec/rime-screen-reader toggle" "$H/.config/hypr/hyprland.lua"'
check "a backup of hyprland.lua was kept" '[ -f "$H/.config/hypr/hyprland.lua.pre-rime.bak" ]'
check "the image modules were refreshed before the first compositor start" \
    'cmp -s "$H/.config/hypr/rime/keybindings.lua" "${HYPR_SRC}/rime/keybindings.lua"'
check "input-defaults.lua keeps the keymap it was rendered with" \
    'grep -q "kb_layout  = \"us\"" "$H/.config/hypr/rime/input-defaults.lua"'
check "no live line under hypr/ still names a moved image path" \
    '! grep -rhvE "^[[:space:]]*--" "$H/.config/hypr/hyprland.lua" "$H/.config/hypr/rime/"*.lua | grep -qE "/usr/(libexec|share)/apex"'
check "the generated and user modules came along" \
    '[ -f "$H/.config/hypr/rime/monitors.lua" ] && [ -f "$H/.config/hypr/rime/input.lua" ] && [ -f "$H/.config/hypr/rime/user-overrides.lua" ] && [ -f "$H/.config/hypr/rime/appearance.lua.bak-mine" ]'
if [ -n "$LUA" ]; then
    check "the config loads, binds every default once and keeps every user bind, across a reload" \
        'lua_healthy "$H"' "$(lua_report "$H")"
    # The window before Rime Shell regenerates its module: the new shell's
    # module asks for rime.keybindings under a loader that loads apex.* names.
    cp -a "$H" "${WORK}/home-newshell"
    shell_keybinds_lua rime > "${WORK}/home-newshell/.config/hypr/rime/shell-keybinds.lua"
    printf 'hl.bind("SUPER + F12", hl.dsp.exec_cmd("my-thing"))\n' \
        > "${WORK}/home-newshell/.config/hypr/rime/shell-keybinds-user.lua"
    check "with Rime Shell's module (rime.* names) under the APEX loader: no double binds, nothing lost on reload" \
        'lua_healthy "${WORK}/home-newshell"' "$(lua_report "${WORK}/home-newshell")"
else
    skip "no Lua interpreter (lua5.4, lua, luajit); the load check cannot run"
fi
if command -v Hyprland >/dev/null 2>&1; then
    rt="${WORK}/rt"; mkdir -p "$rt"; chmod 0700 "$rt"
    got="$(env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$rt" \
        timeout 60 Hyprland --verify-config -c "$H/.config/hypr/hyprland.lua" 2>&1 \
        | sed -n '/Config parsing result/,$p')"
    check "Hyprland --verify-config accepts the migrated config" \
        'grep -qx "config ok" <<<"$got"' "$(grep -v '^$\|====' <<<"$got" | head -3)"
else
    skip "Hyprland is not installed; --verify-config cannot run"
fi

sec "hypridle, niri, labwc, zsh, starship"
check "a customised hypridle.conf keeps the user's lines" \
    'grep -qx "# mine" "$H/.config/hypr/hypridle.conf" && grep -q "timeout = 600" "$H/.config/hypr/hypridle.conf"'
check "…and its lock command reaches the shell this image ships" \
    'grep -qF "|| qs -c /usr/share/rime-shell ipc call lockscreen lock" "$H/.config/hypr/hypridle.conf"'
check "…after APEX's, which stays for a rollback" \
    'grep -qF "|| qs -c /usr/share/apex-shell ipc call lockscreen lock || qs -c /usr/share/rime-shell" "$H/.config/hypr/hypridle.conf"'
check "…while a path of the user's own on the same line is left as it was" \
    'grep -qF "lock_cmd = qs -p ~/.local/share/apex-shell-live ipc call lockscreen lock ||" "$H/.config/hypr/hypridle.conf"'
check "…and the original is kept beside it" \
    'grep -qF "/usr/share/apex-shell ipc call" "$H/.config/hypr/hypridle.conf.pre-rime.bak"'
N="$H/.config/niri/config.kdl"
check "niri includes the new generated files" \
    'grep -qxF "include \"$H/.config/rime-shell/RimeShellInput.kdl\"" "$N" && grep -qxF "include \"$H/.config/rime-shell/RimeShellKeybinds.kdl\"" "$N"'
check "…which exist, with the user's settings in them" \
    'grep -q "tap" "$H/.config/rime-shell/RimeShellInput.kdl" && grep -q "lockscreen" "$H/.config/rime-shell/RimeShellKeybinds.kdl"'
check "the include block carries the marker firstrun looks for, so it is not appended twice" \
    'grep -qF "// ── Rime generated configs (rime-shell-firstrun) ──" "$N" && ! grep -qF "APEX generated configs" "$N"'
check "the autostart spawn follows the moved launcher" \
    'grep -qxF "spawn-at-startup \"/usr/libexec/rime-shell-autostart\"" "$N"'
if command -v niri >/dev/null 2>&1; then
    check "niri validates the migrated config" 'niri validate --config "$N" >/dev/null 2>&1' \
        "$(niri validate --config "$N" 2>&1 | tail -3)"
else
    skip "niri is not installed; cannot validate the migrated config"
fi
R="$H/.config/labwc/rc.xml"
check "labwc's generated keybind region has the markers rime-labwc-keybinds finds" \
    'grep -qF "RIME-KEYBINDS-BEGIN" "$R" && grep -qF "RIME-KEYBINDS-END" "$R" && ! grep -q "APEX-KEYBINDS" "$R"'
check "…the markers are exactly the generator's" \
    'for m in "$(sed -n "s/^BEGIN = \"\(.*\)\"$/\1/p" "${ROOT}/files/system/libexec/rime-labwc-keybinds")" "$(sed -n "s/^END = \"\(.*\)\"$/\1/p" "${ROOT}/files/system/libexec/rime-labwc-keybinds")"; do [ -n "$m" ] && grep -qF "$m" "$R" || exit 1; done'
check "labwc commands call the renamed CLI and image paths" \
    'grep -qF "command=\"rime shell launcher\"" "$R" && grep -qF "qs -p /usr/share/rime-shell" "$R" && grep -qF "/usr/libexec/rime-open-browser" "$R"'
check "…but a script of the user's own in ~/.local/bin is left alone" \
    'grep -qF "/home/u/.local/bin/apex-screenshot area" "$R"'
check "labwc's menu and autostart follow too" \
    'grep -qF "command=\"rime shell lock\"" "$H/.config/labwc/menu.xml" && grep -qx "/usr/libexec/rime-shell-autostart &" "$H/.config/labwc/autostart"'
check "…and the autostart comment is left as the user wrote it" \
    'grep -qx "# /usr/libexec/apex-shell-autostart is the launcher" "$H/.config/labwc/autostart"'
check "~/.zshrc sources the agent and greeting scripts from their new place" \
    'grep -qxF "[[ -r /usr/share/rime/shell/agent.sh ]] && source /usr/share/rime/shell/agent.sh" "$H/.zshrc" && grep -qF "/usr/share/rime/shell/greeting.sh" "$H/.zshrc"'
check "…while the user's lines and aliases are untouched" \
    'grep -qxF "alias ll='"'"'eza -l'"'"'" "$H/.zshrc" && grep -qxF "alias apex-update='"'"'sudo bootc upgrade'"'"'" "$H/.zshrc"'
check "…and it is the same file (inode kept, so a dotfiles symlink would survive)" \
    '[ "$(stat -c %i "$H/.zshrc")" = "$inode_zshrc" ]'
check "~/.config/starship.toml follows the shell cache to its new name" \
    '[ "$(readlink "$H/.config/starship.toml")" = "$H/.cache/rime-shell/starship.toml" ]'

sec "per-user units"
U="$H/.config/systemd/user"
check "rime-agentd is enabled where apex-agentd was" \
    '[ "$(readlink "$U/default.target.wants/rime-agentd.service")" = /usr/lib/systemd/user/rime-agentd.service ]'
check "rime-remoted and the storage timer likewise" \
    '[ -L "$U/default.target.wants/rime-remoted.service" ] && [ -L "$U/timers.target.wants/rime-storage-notice.timer" ]'
check "the old links stay, for a rollback" \
    '[ -L "$U/default.target.wants/apex-agentd.service" ]'
check "a masked unit stays masked under its new name" \
    '[ "$(readlink "$U/rime-aid.service")" = /dev/null ]'
check "the drop-in directory moved, the old name linked" \
    '[ -f "$U/rime-agentd.service.d/override.conf" ] && [ -L "$U/apex-agentd.service.d" ]'
check "every image unit's drop-in directory moved (rime-remoted's too)" \
    '[ -f "$U/rime-remoted.service.d/debug.conf" ] && [ -L "$U/apex-remoted.service.d" ]'
check "the user's own apex-wip-snapshot timer is neither renamed nor disabled" \
    '[ -L "$U/timers.target.wants/apex-wip-snapshot.timer" ] && [ ! -L "$U/timers.target.wants/rime-wip-snapshot.timer" ] && [ -f "$U/apex-wip-snapshot.timer" ] && [ ! -e "$U/rime-wip-snapshot.timer" ]'
check "the user's own apex-wip-snapshot unit is not touched" \
    '[ ! -L "$U/default.target.wants/rime-wip-snapshot.service" ] && [ ! -e "$U/rime-wip-snapshot.service" ] && [ -f "$U/apex-wip-snapshot.service" ]'
check "a user unit named like an image unit is not enabled under the image's new name" \
    '[ ! -L "$U/default.target.wants/rime-storage-notice.service" ] && [ -f "$U/apex-storage-notice.service" ]'
check "…and the user is told it is theirs" 'grep -q "apex-storage-notice.service is your own copy" "$H.out"'
check "the units it enabled were started, and nothing else" \
    '[ "$(sort "$H.systemctl" | tr "\n" ";")" = "--user --no-block start rime-agentd.service;--user --no-block start rime-remoted.service;--user --no-block start rime-storage-notice.timer;" ]' \
    "$(cat "$H.systemctl" 2>/dev/null)"

sec "idempotency"
before="$(snapshot "$H")"
: > "$H.systemctl"
run "$H" --start-units; rc=$?
check "a second run succeeds" '[ "$rc" = 0 ]' "$(cat "$H.out")"
check "…and changes nothing at all" '[ "$(snapshot "$H")" = "$before" ]' \
    "$(diff <(printf '%s\n' "$before") <(snapshot "$H") | head -5)"
check "…and starts nothing" '[ ! -s "$H.systemctl" ]'
check "…and says nothing" '[ ! -s "$H.out" ]' "$(head -3 "$H.out")"

sec "the edges"
# Both names hold data: nothing may be moved or merged.
C="${WORK}/conflict"; make_home "$C"
mkdir -p "$C/.local/state/rime/remote"
head -c 32 /dev/urandom > "$C/.local/state/rime/remote/identity.key"
k_old="$(sha256sum < "$C/.local/state/apex/remote/identity.key")"
k_new="$(sha256sum < "$C/.local/state/rime/remote/identity.key")"
run "$C"; rc=$?
check "both state directories holding data is reported (exit 1)" '[ "$rc" = 1 ]'
check "…and both keys are exactly where they were" \
    '[ ! -L "$C/.local/state/apex" ] && [ "$(sha256sum < "$C/.local/state/apex/remote/identity.key")" = "$k_old" ] && [ "$(sha256sum < "$C/.local/state/rime/remote/identity.key")" = "$k_new" ]'
check "…with the command that finishes it by hand" 'grep -q "ln -sfn rime" "$C.out"'
check "…and every other step still ran" '[ -L "$C/.config/apex" ] && [ -L "$C/.config/hypr/apex" ]'

# A daemon that created the new directory but wrote nothing into it yet.
E="${WORK}/empty-new"; make_home "$E"
mkdir -p "$E/.local/state/rime/agent/sessions"
run "$E"; rc=$?
check "an empty new directory tree is merged into" \
    '[ "$rc" = 0 ] && [ -L "$E/.local/state/apex" ] && [ -f "$E/.local/state/rime/remote/identity.key" ] && [ -f "$E/.local/state/rime/agent/sessions/s1.json" ]'

# Hyprland's module directory, already seeded by firstrun.
M="${WORK}/hypr-merge"; make_home "$M"
mkdir -p "$M/.config/hypr/rime"; cp "${HYPR_SRC}/rime/rules.lua" "$M/.config/hypr/rime/rules.lua"
run "$M"; rc=$?
check "hypr/rime already seeded: the user's files are merged in" \
    '[ "$rc" = 0 ] && [ -L "$M/.config/hypr/apex" ] && [ -f "$M/.config/hypr/rime/user-overrides.lua" ]'
check "…and a name in both keeps the new copy, the old beside it" \
    'cmp -s "$M/.config/hypr/rime/rules.lua" "${HYPR_SRC}/rime/rules.lua" && [ -f "$M/.config/hypr/rime/rules.lua.from-apex" ]'

# A directory the user keeps in a dotfiles repository.
D="${WORK}/dotfiles"; make_home "$D"
mkdir -p "$D/dotfiles"; mv "$D/.config/apex-shell" "$D/dotfiles/apex-shell"
ln -s ../dotfiles/apex-shell "$D/.config/apex-shell"
run "$D"; rc=$?
check "a symlinked directory gets the same link under the new name" \
    '[ "$rc" = 0 ] && [ "$(readlink "$D/.config/rime-shell")" = ../dotfiles/apex-shell ] && [ -f "$D/.config/rime-shell/src/user_data/settings.json" ]'

# The managed hypridle copy is refreshed from the image, not patched.
I="${WORK}/idle"; make_home "$I"
printf '# the image copy APEX shipped\ngeneral { lock_cmd = qs -c /usr/share/apex-shell ipc call lockscreen lock }\n' \
    > "$I/.config/hypr/hypridle.conf"
sha256sum < "$I/.config/hypr/hypridle.conf" | cut -d' ' -f1 > "$I/.local/state/apex/hypridle.conf.sha256"
run "$I"; rc=$?
check "an unmodified hypridle.conf is refreshed from the image" \
    'cmp -s "$I/.config/hypr/hypridle.conf" "${IMG}/usr/share/rime-shell/src/config/hypridle.conf"'
check "…and its stamp updated, so firstrun keeps managing it" \
    '[ "$(tr -d "[:space:]" < "$I/.local/state/rime/hypridle.conf.sha256")" = "$(sha256sum < "$I/.config/hypr/hypridle.conf" | cut -d" " -f1)" ]'

# A Rime OS home that was never APEX: nothing to do, nothing invented.
F="${WORK}/fresh"; mkdir -p "$F/.config/hypr/rime" "$F/.local/state/rime"
before="$(snapshot "$F")"
run "$F"; rc=$?
check "a home that was never APEX is left exactly as it is" \
    '[ "$rc" = 0 ] && [ "$(snapshot "$F")" = "$before" ] && [ ! -s "$F.out" ]'

sec "with Rime Shell's own migration in the image"
# The shell ships src/scripts/rime-shell-migrate.sh, and the shell directories
# are its business: this script runs it (that is what puts it before firstrun's
# first mkdir) instead of renaming them itself. Needs a shell tree to take the
# real script from; the directory-level fallback is what everything above ran.
SHELL_TREE=""
for cand in "${RIME_SHELL_TREE:-}" "${ROOT}/../rime-shell" "${ROOT}/../wt-rime-shell" /usr/share/rime-shell; do
    [ -n "$cand" ] && [ -f "${cand}/src/scripts/rime-shell-migrate.sh" ] && { SHELL_TREE="$cand"; break; }
done
if [ -z "$SHELL_TREE" ]; then
    skip "no rime-shell tree with src/scripts/rime-shell-migrate.sh; the delegation cannot be checked"
else
    IMG2="${WORK}/img-shell"; cp -a "$IMG" "$IMG2"
    cp "${SHELL_TREE}/src/scripts/rime-shell-migrate.sh" "${IMG2}/usr/share/rime-shell/src/scripts/"
    S="${WORK}/with-shell"; make_home "$S"
    IMG_SAVE="$IMG"; IMG="$IMG2"; run "$S"; rc=$?; IMG="$IMG_SAVE"
    check "the migration succeeds with the shell's script doing the shell's part" '[ "$rc" = 0 ]' "$(cat "$S.out")"
    check "the shell's directories moved, the old names linked" \
        '[ -L "$S/.config/apex-shell" ] && [ -d "$S/.config/rime-shell" ] && [ -L "$S/.cache/apex-shell" ] && [ -L "$S/.local/state/apex-shell" ]'
    check "the shell renamed its own files; the old name still resolves for niri" \
        '[ -f "$S/.config/rime-shell/RimeShellInput.kdl" ] && [ ! -L "$S/.config/rime-shell/RimeShellInput.kdl" ] && [ -L "$S/.config/rime-shell/ApexShellInput.kdl" ]'
    check "niri includes the moved files by their new names" \
        'grep -qxF "include \"$S/.config/rime-shell/RimeShellInput.kdl\"" "$S/.config/niri/config.kdl" && grep -q tap "$S/.config/rime-shell/RimeShellInput.kdl"'
    check "…with no conflict along the way" '! grep -qi conflict "$S.out"' "$(grep -i conflict "$S.out")"
    if command -v niri >/dev/null 2>&1; then
        check "niri validates it" 'niri validate --config "$S/.config/niri/config.kdl" >/dev/null 2>&1'
    fi
    before="$(snapshot "$S")"
    IMG="$IMG2"; run "$S"; rc=$?; IMG="$IMG_SAVE"
    check "a second run changes nothing and says nothing" \
        '[ "$rc" = 0 ] && [ "$(snapshot "$S")" = "$before" ] && [ ! -s "$S.out" ]' "$(head -3 "$S.out")"
fi

sec "Release B: the image still answers to the APEX names"
# What install-rename-compat puts in the real image: the old command, the
# /usr/libexec helpers and the /usr/share trees under their APEX names, as
# links to the new ones. A user's line that names them then works as it is,
# here AND after a rollback to APEX, which has none of the new names; only
# what an alias cannot carry is rewritten: a quickshell IPC call, because
# quickshell finds an instance by the path it is given.
IMGB="${WORK}/img-b"; cp -a "$IMG" "$IMGB"
ln -s rime "${IMGB}/usr/bin/apex"   # rime-rename: keep — the image's alias
for b in shell-autostart open-browser screen-reader desktop-menu; do
    ln -s "rime-$b" "${IMGB}/usr/libexec/apex-$b"   # rime-rename: keep — the image's aliases
done
ln -s rime-shell "${IMGB}/usr/share/apex-shell"; ln -s rime "${IMGB}/usr/share/apex"   # rime-rename: keep
B="${WORK}/release-b"; make_home "$B"; BB="${WORK}/release-b.before"; cp -a "$B" "$BB"
IMG_SAVE="$IMG"; IMG="$IMGB"; run "$B"; rc=$?; IMG="$IMG_SAVE"
same() { cmp -s "$BB/$1" "$B/$1"; }
check "the migration succeeds" '[ "$rc" = 0 ]' "$(cat "$B.out")"
check "~/.zshrc is not touched: its paths resolve through the aliases" \
    'same .zshrc && [ ! -e "$B/.zshrc.pre-rime.bak" ]'
check "labwc's menu and autostart are not touched" \
    'same .config/labwc/menu.xml && same .config/labwc/autostart'
check "labwc keybinds keep the apex command and the old helper path" \
    'grep -qF "command=\"apex shell launcher\"" "$B/.config/labwc/rc.xml" && grep -qF "/usr/libexec/apex-open-browser" "$B/.config/labwc/rc.xml"'
check "…but a quickshell IPC call names the new shell" \
    'grep -qF "command=\"qs -p /usr/share/rime-shell ipc call lockscreen lock\"" "$B/.config/labwc/rc.xml"'
check "niri still starts the shell through the old launcher name" \
    'grep -qxF "spawn-at-startup \"/usr/libexec/apex-shell-autostart\"" "$B/.config/niri/config.kdl"'
check "hyprland.lua: only the cache clear changed" \
    'diff <(grep -vF "name:sub(1, 5)" "$BB/.config/hypr/hyprland.lua") <(grep -vF "name:sub(1, 5)" "$B/.config/hypr/hyprland.lua") >/dev/null'
check "the shell's generated module reaches the running shell" \
    'grep -qF "qs -p /usr/share/rime-shell ipc call launcher toggle" "$B/.config/hypr/rime/shell-keybinds.lua"'
HI="$B/.config/hypr/hypridle.conf"
check "hypridle's lock command still reaches APEX's shell" \
    'grep -qF "|| qs -c /usr/share/apex-shell ipc call lockscreen lock ||" "$HI"'
check "…and then this image's" \
    'grep -qF "|| qs -c /usr/share/rime-shell ipc call lockscreen lock" "$HI"'
before="$(snapshot "$B")"
IMG="$IMGB"; run "$B"; rc=$?; IMG="$IMG_SAVE"
check "a second run changes nothing and says nothing" \
    '[ "$rc" = 0 ] && [ "$(snapshot "$B")" = "$before" ] && [ ! -s "$B.out" ]' "$(head -3 "$B.out")"

# A rollback, then Rime again. On the rollback login APEX's firstrun finds no
# block under its own marker and appends one (verbatim: 795a1face
# apex-shell-firstrun, the NIRIINC heredoc); the next Rime login must not turn
# it into a second Rime block, or each round trip adds another.
append_apex_niri_block() {   # rime-rename: keep — the APEX firstrun's own text
    printf '\n// ── APEX generated configs (apex-shell-firstrun) ──\n// Regenerated by APEX Settings and APEX Shell — do not hand-edit the targets.\n// Positional: these come last, so they win over the sections above.\ninclude "%s/.config/apex-shell/ApexShellInput.kdl"\ninclude "%s/.config/apex-shell/ApexShellKeybinds.kdl"\n' \
        "$1" "$1" >> "$1/.config/niri/config.kdl"
}
append_apex_niri_block "$B"
IMG="$IMGB"; run "$B"; rc=$?; IMG="$IMG_SAVE"
N="$B/.config/niri/config.kdl"
check "after a round trip niri still has exactly one include block" \
    '[ "$(grep -c "Rime generated configs" "$N")" = 1 ] && ! grep -q "APEX generated configs" "$N"' \
    "$(grep -n "generated configs\|^include" "$N")"
check "…and includes each generated file once" \
    '[ "$(grep -c "RimeShellInput.kdl\"$" "$N")" = 1 ] && [ "$(grep -c "RimeShellKeybinds.kdl\"$" "$N")" = 1 ]'
if command -v niri >/dev/null 2>&1; then
    check "…and niri validates it" 'niri validate --config "$N" >/dev/null 2>&1' "$(niri validate --config "$N" 2>&1 | tail -3)"
fi
before="$(snapshot "$B")"
IMG="$IMGB"; run "$B"; rc=$?; IMG="$IMG_SAVE"
check "a second run after that changes nothing" '[ "$rc" = 0 ] && [ "$(snapshot "$B")" = "$before" ]'

# A hypridle.conf customised elsewhere, with the lock command APEX shipped.
P="${WORK}/release-b-plain"; make_home "$P"
printf 'general {\n    lock_cmd = qs -c /usr/share/apex-shell ipc call lockscreen lock\n}\nlistener { timeout = 900 }\n' \
    > "$P/.config/hypr/hypridle.conf"
IMG="$IMGB"; run "$P"; IMG="$IMG_SAVE"
# APEX's firstrun postcondition, verbatim (795a1face:files/system/libexec/apex-shell-firstrun):
# after a rollback it fails every login unless the FIRST `qs -c` path of
# lock_cmd holds a shell.qml on the APEX image.
apex_target="$(sed -n 's/^[[:space:]]*lock_cmd[[:space:]]*=[[:space:]]*qs -c \([^ ]*\).*/\1/p' \
    "$P/.config/hypr/hypridle.conf" | head -1)"
check "APEX's firstrun check, read off the migrated file, finds APEX's shell" \
    '[ "$apex_target" = /usr/share/apex-shell ]' "got '$apex_target'"
# And the command itself, through sh as hypridle runs it, with a qs that has
# one running shell and answers "No running instances" (255) for any other.
mkdir -p "${WORK}/qsbin"
cat > "${WORK}/qsbin/qs" <<'EOF'
#!/bin/sh
printf '%s\n' "$2" >> "${QS_LOG:?}"
[ "$2" = "${QS_RUNNING:?}" ] && exit 0
echo "No running instances for \"$2\"" >&2
exit 255
EOF
chmod +x "${WORK}/qsbin/qs"
lock_cmd="$(sed -n 's/^[[:space:]]*lock_cmd[[:space:]]*=[[:space:]]*//p' "$P/.config/hypr/hypridle.conf")"
for running in /usr/share/rime-shell /usr/share/apex-shell; do
    : > "${WORK}/qs.log"
    PATH="${WORK}/qsbin:$PATH" QS_LOG="${WORK}/qs.log" QS_RUNNING="$running" sh -c "$lock_cmd" 2>/dev/null; rc=$?
    check "the lock command locks with $running running" \
        '[ "$rc" = 0 ] && [ "$(tail -1 "${WORK}/qs.log")" = "$running" ]' "rc=$rc, called: $(tr '\n' ' ' < "${WORK}/qs.log")"
done

sec "where it runs"
FR="${ROOT}/files/system/libexec/rime-shell-firstrun"
call_line="$(grep -n '^if \[ -x /usr/libexec/rime-session-migrate \]' "$FR" | head -1 | cut -d: -f1)"
first_mkdir="$(grep -nE '^[^#]*mkdir -p' "$FR" | head -1 | cut -d: -f1)"
check "rime-shell-firstrun calls the migration before it creates any directory" \
    '[ -n "$call_line" ] && [ -n "$first_mkdir" ] && [ "$call_line" -lt "$first_mkdir" ]' \
    "call at ${call_line:-?}, first mkdir at ${first_mkdir:-?}"

# ═════════════════════════════════════════════════════════════════════════════
sec "each guard fails when its defect is put back"
MUT="${WORK}/mutants"; mkdir -p "$MUT"
# mutant <label> <python-expr old> <python-expr new> <check that must now FAIL>
mutant() {
    local label="$1" old="$2" new="$3" guard="$4" m="${MUT}/m$((pass + fail + skipped))"
    python3 - "$SCRIPT" "$m.py" "$old" "$new" <<'EOF' || { bad "mutant '$label' could not be applied"; return; }
import sys
src, dst, old, new = sys.argv[1:5]
s = open(src).read()
if s.count(old) < 1:
    sys.exit(1)
open(dst, "w").write(s.replace(old, new))
EOF
    local MH="$m.home"; make_home "$MH"
    SCRIPT_UNDER_TEST="$m.py" run "$MH" --start-units
    if eval "$guard"; then bad "mutant survives: $label"; else ok "caught: $label"; fi
}
if [ -n "$LUA" ]; then
    mutant "no compat symlink for the old directory name" \
        '    os.symlink(new_name, old)' '    pass' 'lua_healthy "$MH"'
    mutant "the cache-clear line left clearing apex.* only" \
        '"\n" + NEW_CACHE_CLEAR + "\n"' '"\n" + OLD_CACHE_CLEAR + "\n"' \
        'shell_keybinds_lua rime > "$MH/.config/hypr/rime/shell-keybinds.lua"; lua_healthy "$MH"'
    mutant "the APEX-image modules not refreshed before the first start" \
        '    if moved and os.path.isdir(img_mods)' '    if False and os.path.isdir(img_mods)' \
        'shell_keybinds_lua rime > "$MH/.config/hypr/rime/shell-keybinds.lua"; lua_healthy "$MH"'
    # The shipped keybindings.lua without its both-names block.
    cp -a "$IMG" "${WORK}/img-noalias"
    sed -i '/^package\.loaded\[/d' "${WORK}/img-noalias/usr/share/rime/hypr/rime/keybindings.lua"
    MH="${WORK}/noalias"; make_home "$MH"
    IMG_SAVE="$IMG"; IMG="${WORK}/img-noalias"; run "$MH"; IMG="$IMG_SAVE"
    shell_keybinds_lua rime > "$MH/.config/hypr/rime/shell-keybinds.lua"
    if lua_healthy "$MH"; then bad "mutant survives: keybindings.lua answers to one name only"
    else ok "caught: keybindings.lua answers to one name only"; fi
else
    skip "no Lua interpreter; the Hyprland mutants cannot run"
fi
mutant "the state directory not moved" \
    '    for base in (state, config, data, cache):' '    for base in (config, data, cache):' \
    '[ -f "$MH/.local/state/rime/remote/identity.key" ]'
mutant "a user unit followed like an image unit" \
    '            if os.path.dirname(target) not in IMAGE_UNIT_DIRS:' '            if False:' \
    '[ ! -L "$MH/.config/systemd/user/default.target.wants/rime-storage-notice.service" ]'
# The conflict mutant needs its own home: both names holding data.
m="${MUT}/conflict"; python3 - "$SCRIPT" "$m.py" <<'EOF'
import sys
s = open(sys.argv[1]).read()
open(sys.argv[2], "w").write(s.replace("and (merge or is_empty_tree(new))", "and True"))
EOF
MH="$m.home"; make_home "$MH"; mkdir -p "$MH/.local/state/rime/remote"
printf 'fresh\n' > "$MH/.local/state/rime/remote/identity.key"
kh="$(sha256sum < "$MH/.local/state/apex/remote/identity.key")"
SCRIPT_UNDER_TEST="$m.py" run "$MH"
if [ -f "$MH/.local/state/apex/remote/identity.key" ] && [ ! -L "$MH/.local/state/apex" ] \
   && [ "$(sha256sum < "$MH/.local/state/apex/remote/identity.key")" = "$kh" ]; then
    bad "mutant survives: a conflicting new directory merged into"
else
    ok "caught: a conflicting new directory merged into (the paired key would be displaced)"
fi
mutant "niri includes rewritten without creating their targets" \
    '            if (not os.path.lexists(new_target) or placeholder) and not dry_run:' '            if False:' \
    '[ -f "$MH/.config/rime-shell/RimeShellInput.kdl" ]'
mutant "labwc markers left in their APEX spelling" \
    'new = new.replace(OLD_KB_BEGIN, NEW_KB_BEGIN).replace(OLD_KB_END, NEW_KB_END)' 'pass' \
    'grep -qF "RIME-KEYBINDS-BEGIN" "$MH/.config/labwc/rc.xml"'
mutant "comments rewritten as if they were commands" \
    'if comment and re.match(' 'if False and re.match(' \
    'grep -qx "# /usr/libexec/apex-shell-autostart is the launcher" "$MH/.config/labwc/autostart"'
# The managed-copy mutant needs a home whose hypridle.conf matches its stamp.
m="${MUT}/idle"; python3 - "$SCRIPT" "$m.py" <<'EOF'
import sys
s = open(sys.argv[1]).read()
open(sys.argv[2], "w").write(s.replace("if was and was == sha256(conf) and os.path.isfile(img):", "if False:"))
EOF
MH="$m.home"; make_home "$MH"
printf '# the image copy APEX shipped\ngeneral { lock_cmd = qs -c /usr/share/apex-shell ipc call lockscreen lock }\n' \
    > "$MH/.config/hypr/hypridle.conf"
sha256sum < "$MH/.config/hypr/hypridle.conf" | cut -d' ' -f1 > "$MH/.local/state/apex/hypridle.conf.sha256"
SCRIPT_UNDER_TEST="$m.py" run "$MH"
if cmp -s "$MH/.config/hypr/hypridle.conf" "${IMG}/usr/share/rime-shell/src/config/hypridle.conf"; then
    bad "mutant survives: a managed hypridle.conf patched instead of refreshed"
else
    ok "caught: a managed hypridle.conf patched instead of refreshed (firstrun would freeze it)"
fi

# Release B's decisions, judged on the image with the aliases.
IMG_SAVE="$IMG"; IMG="$IMGB"
mutant "an image path rewritten while the image still has the old one" \
    '        if self.exists(token.rstrip("/") or "/"):' '        if False:' \
    'grep -qF "/usr/libexec/apex-open-browser" "$MH/.config/labwc/rc.xml"'
mutant "the apex command rewritten while its wrapper is in the image" \
    ' or image.exists(f"/usr/bin/{OLD}"):' ':' \
    'grep -qF "command=\"apex shell launcher\"" "$MH/.config/labwc/rc.xml"'
mutant "a quickshell IPC call left on the alias" \
    '            new = rewrite_ipc(new, image)' '            pass' \
    'grep -qF "qs -p /usr/share/rime-shell ipc call lockscreen lock" "$MH/.config/labwc/rc.xml"'
mutant "hypridle's IPC call not chained" \
    'else chain_ipc(line, image)' 'else line' \
    'grep -qF "|| qs -c /usr/share/rime-shell ipc call lockscreen lock" "$MH/.config/hypr/hypridle.conf"'
mutant "hypridle's chain with the Rime call first" \
    '{m.group(1)}{OLD_SHELL_SHARE}{m.group(2)} || {m.group(1)}{NEW_SHELL_SHARE}{m.group(2)}' \
    '{m.group(1)}{NEW_SHELL_SHARE}{m.group(2)} || {m.group(1)}{OLD_SHELL_SHARE}{m.group(2)}' \
    'grep -qF "|| qs -c /usr/share/apex-shell ipc call lockscreen lock ||" "$MH/.config/hypr/hypridle.conf"'
mutant "APEX's re-appended niri block renamed into a second Rime block" \
    '    lines = drop_reappended_niri_block(text).split("\n")' '    lines = text.split("\n")' \
    'append_apex_niri_block "$MH"; SCRIPT_UNDER_TEST="$m.py" run "$MH"; [ "$(grep -c "Rime generated configs" "$MH/.config/niri/config.kdl")" = 1 ]'
IMG="$IMG_SAVE"

printf '\nrime-session-migrate: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
[ "$fail" -eq 0 ]
