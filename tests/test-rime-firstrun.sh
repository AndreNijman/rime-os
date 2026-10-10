#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Assertions for the self-healing blocks of /usr/libexec/rime-shell-firstrun.
#
#  That script runs at every login and edits files that belong to the user, so
#  the interesting question is never "does it write the new value" but "does it
#  leave everything else exactly as it was, every time it runs". The blocks are
#  extracted from the shipped script by their own comment markers and executed
#  verbatim against throwaway HOMEs — nothing here re-implements them, and a
#  renamed or deleted block fails the test instead of silently skipping it.
#
#  Needs neither root nor network. Run from the repository root:
#      ./tests/test-rime-firstrun.sh
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${ROOT}/files/system/libexec/rime-shell-firstrun"
[ -f "$SRC" ] || { printf 'missing %s\n' "$SRC" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

extract() {
    local first="$1" out="$2" must="$3"
    sed -n "/^${first}/,/^fi$/p" "$SRC" > "$out"
    grep -q "$must" "$out" \
        || { printf 'could not extract the block starting %s\n' "$first" >&2; exit 1; }
}

extract '# Repair an already-seeded ~\/.zshrc' "${WORK}/zshrc-block.sh" ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE

# The blocks log through the script's own helper and read HOME/HYPR_CONF, so the
# harness supplies exactly those and nothing else.
run_zshrc()   { HOME="$1" bash -c 'set -euo pipefail; log() { :; }; source "$1"' -- "${WORK}/zshrc-block.sh"; }

section "zsh autosuggestion colour"
# The bug: zle lower-cases a colour spec when it stores the highlight, and
# zsh-autosuggestions removes its highlight by exact string match, so the
# upper-case value Rime used to ship left accepted text grey.
h="${WORK}/zsh-seeded"; mkdir -p "$h"
printf '%s\n' '# user header' \
               "ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE='fg=#4A5162'" \
               "alias ll='eza -l'" > "${h}/.zshrc"
chmod 0640 "${h}/.zshrc"
inode="$(stat -c %i "${h}/.zshrc")"
mode="$(stat -c %a "${h}/.zshrc")"
run_zshrc "$h"
grep -qxF "ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE='fg=#4a5162'" "${h}/.zshrc" \
    && ok "the upper-case colour is rewritten" || bad "the upper-case colour is rewritten"
grep -qxF "alias ll='eza -l'" "${h}/.zshrc" \
    && ok "the user's own lines survive" || bad "the user's own lines survive"
[ "$(stat -c %i "${h}/.zshrc")" = "$inode" ] \
    && ok "the file keeps its inode" || bad "the file keeps its inode"
[ "$(stat -c %a "${h}/.zshrc")" = "$mode" ] \
    && ok "the file keeps its mode" || bad "the file keeps its mode"
[ -z "$(find "$h" -maxdepth 1 -name '.zshrc.rime.*')" ] \
    && ok "no temporary file is left behind" || bad "no temporary file is left behind"
run_zshrc "$h"
[ "$(grep -c ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE "${h}/.zshrc")" = 1 ] \
    && ok "re-running changes nothing" || bad "re-running changes nothing"

h="${WORK}/zsh-custom"; mkdir -p "$h"
printf '%s\n' "ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE='fg=#123456'" > "${h}/.zshrc"
cp "${h}/.zshrc" "${WORK}/zsh-custom.orig"
run_zshrc "$h"
cmp -s "${h}/.zshrc" "${WORK}/zsh-custom.orig" \
    && ok "a colour the user chose is left alone" || bad "a colour the user chose is left alone"

h="${WORK}/zsh-none"; mkdir -p "$h"
run_zshrc "$h"
[ ! -e "${h}/.zshrc" ] \
    && ok "no ~/.zshrc is invented" || bad "no ~/.zshrc is invented"

# ─────────────────────────────────────────────────────────────────────────────
#  labwc config seeding
#
#  labwc has no IPC, so everything the compositor does for the shell is declared
#  in these files up front. A seeding bug is therefore not a cosmetic problem: it
#  is the difference between a working session and a bare grey screen with no way
#  to discover why. And because the files belong to the user afterwards, the
#  interesting property is again that re-running leaves their edits alone.
# ─────────────────────────────────────────────────────────────────────────────
extract '# ── 6b\. labwc config' "${WORK}/labwc-block.sh" 'rime-shell-autostart'

# The Hyprland rule migration, extracted so the assertion below drives the real
# sed rather than a copy of it that can drift.
sed -n '/^# Hyprland 0\.54+ removed syntax/,/^done$/p' "$SRC" > "${WORK}/hypr-mig-block.sh"
grep -q 'suppress_event maximize' "${WORK}/hypr-mig-block.sh" \
    || { printf 'could not extract the Hyprland migration block\n' >&2; exit 1; }

# The block hardcodes the INSTALLED template directory, which does not exist in a
# checkout. Redirect that single path at the repo copies so the real logic runs
# against the real templates. (That the install path itself exists is asserted at
# image build time in Containerfile.base, which is where it belongs.)
TMPL="${ROOT}/files/desktop/labwc"
sed -i "s|LABWC_TMPL_DIR=/usr/share/rime/labwc|LABWC_TMPL_DIR=${TMPL}|" "${WORK}/labwc-block.sh"
grep -q "LABWC_TMPL_DIR=${TMPL}" "${WORK}/labwc-block.sh" \
    || { printf 'could not redirect LABWC_TMPL_DIR in the extracted block\n' >&2; exit 1; }
run_labwc() {
    # Note the two different $1s: the outer one is this function's argument (the
    # throwaway HOME), the inner one is the bash -c script's first positional
    # argument (the extracted block). They do not collide because they are in
    # different scopes.
    HOME="$1" bash -c '
        set -euo pipefail
        log() { :; }
        KB_LAYOUT=us
        KB_VARIANT=
        # The real script derives this from VARIANT_ID before reaching the
        # labwc block (chartreuse on Daily, gold on Gaming). The block only
        # consumes it, so the harness supplies the Daily value.
        RIME_ACCENT="#D9F99D"
        render_hypr_tmpl() {
            sed -e "s|@HOME@|${HOME}|g" \
                -e "s|@KB_LAYOUT@|${KB_LAYOUT}|g" \
                -e "s|@KB_VARIANT@|${KB_VARIANT}|g" "$1"
        }
        # `--` is $0, so the block path is $1. LABWC_TMPL_DIR is set by the
        # block itself, redirected at extraction time.
        source "$1"
    ' -- "${WORK}/labwc-block.sh"
}

section "labwc config seeding"

if ! command -v labwc >/dev/null 2>&1 && [ ! -x /usr/bin/labwc ]; then
    printf 'SKIP  labwc not installed; seeding block is guarded on it\n'
else
    h="${WORK}/labwc-fresh"; mkdir -p "$h"
    run_labwc "$h"
    for f in rc.xml menu.xml autostart environment; do
        [ -s "${h}/.config/labwc/${f}" ] \
            && ok "seeded ${f}" || bad "seeded ${f}"
    done
    grep -q 'rime-shell-autostart' "${h}/.config/labwc/autostart" \
        && ok "autostart starts Rime Shell" || bad "autostart starts Rime Shell"
    # The keybinds must not hardcode the shell's install path: `rime shell`
    # exists so a renamed IPC target is fixed once in the CLI rather than in
    # every seeded config on every machine.
    grep -q 'command="rime shell' "${h}/.config/labwc/rc.xml" \
        && ok "keybinds go through rime shell" || bad "keybinds go through rime shell"
    ! grep -q 'command="qs -p' "${h}/.config/labwc/rc.xml" \
        && ok "no raw qs invocation in keybinds" || bad "no raw qs invocation in keybinds"
    grep -q '<layout>icon:iconify,max,close</layout>' "${h}/.config/labwc/rc.xml" \
        && grep -q '<maximizedDecoration>titlebar</maximizedDecoration>' "${h}/.config/labwc/rc.xml" \
        && ok "native window controls remain available" \
        || bad "native window controls remain available"
    # A session that cannot be detected by the shell is a session with a bar that
    # thinks it is on an unknown compositor.
    grep -q '^XDG_CURRENT_DESKTOP=labwc' "${h}/.config/labwc/environment" \
        && ok "environment identifies the compositor" \
        || bad "environment identifies the compositor"
    # Placeholders must be substituted, not shipped literally.
    ! grep -q '@HOME@\|@KB_LAYOUT@' "${h}/.config/labwc/autostart" "${h}/.config/labwc/environment" \
        && ok "no placeholders survive substitution" || bad "no placeholders survive substitution"
    [ -x "${h}/.config/labwc/autostart" ] \
        && ok "autostart is executable" || bad "autostart is executable"

    # Idempotence: a second login must not duplicate the autostart block.
    run_labwc "$h"
    [ "$(grep -c 'rime-shell-autostart' "${h}/.config/labwc/autostart")" = 1 ] \
        && ok "re-running does not duplicate autostarts" \
        || bad "re-running does not duplicate autostarts"

    # A user's own edits must survive.
    h="${WORK}/labwc-edited"; mkdir -p "${h}/.config/labwc"
    printf '%s\n' '# my own config' > "${h}/.config/labwc/rc.xml"
    cp "${h}/.config/labwc/rc.xml" "${WORK}/labwc-rc.orig"
    run_labwc "$h"
    cmp -s "${h}/.config/labwc/rc.xml" "${WORK}/labwc-rc.orig" \
        && ok "a hand-written rc.xml is left alone" || bad "a hand-written rc.xml is left alone"

    # An autostart predating Rime Shell gets repaired rather than replaced.
    h="${WORK}/labwc-legacy"; mkdir -p "${h}/.config/labwc"
    printf '%s\n' '# pre-existing' 'xterm &' > "${h}/.config/labwc/autostart"
    run_labwc "$h"
    grep -q 'rime-shell-autostart' "${h}/.config/labwc/autostart" \
        && ok "a legacy autostart gains the shell" || bad "a legacy autostart gains the shell"
    grep -qxF 'xterm &' "${h}/.config/labwc/autostart" \
        && ok "the user's own autostart lines survive" || bad "the user's own autostart lines survive"
fi

# The shipped XML must be well-formed: labwc has no --verify-config, and a
# malformed rc.xml makes it fall back to defaults SILENTLY — a session that
# starts with no keybinds and no shell.
section "labwc shipped config validity"
if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout "${TMPL}/rc.xml" 2>/dev/null \
        && ok "rc.xml is well-formed XML" || bad "rc.xml is well-formed XML"
    xmllint --noout "${TMPL}/menu.xml" 2>/dev/null \
        && ok "menu.xml is well-formed XML" || bad "menu.xml is well-formed XML"
else
    printf 'SKIP  xmllint unavailable\n'
fi

# ── the developer's own session must be untouched ────────────────────────────
# Counted before the labwc block below, compared after. A nested compositor
# creates a new wayland-N socket in $XDG_RUNTIME_DIR, so this is a direct
# measurement of the thing that went wrong rather than a proxy for it.
_socks_before=$(find "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" -maxdepth 1 \
                     -name 'wayland-*' -type s 2>/dev/null | wc -l)

# labwc itself is the only authority on whether an action name and its arguments
# are valid; xmllint cannot know that `Focus direction=...` is not a thing.
if command -v labwc >/dev/null 2>&1; then
    d="${WORK}/labwc-parse"; mkdir -p "$d"
    cp "${TMPL}/rc.xml" "${TMPL}/menu.xml" "$d/"
    sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "${TMPL}/environment" > "${d}/environment"
    # Grep for CONFIG diagnostics specifically, not any error: config parsing
    # happens before the backend comes up and is the only thing under test.
    #
    # WLR_BACKENDS=headless, and WAYLAND_DISPLAY and DISPLAY removed, and that
    # is not tidiness — it is a bug fix. The previous version unset only
    # HYPRLAND_INSTANCE_SIGNATURE and reasoned that labwc "will fail to open a
    # backend here (no seat in CI)". True in CI; false on a developer's
    # machine, where there IS a seat and a live session, so labwc succeeded and
    # opened a NESTED COMPOSITOR WINDOW on whatever workspace the developer was
    # using — for six seconds, once per run of this file. It interrupted real
    # work.
    #
    # "Safe because CI has no display" is the mirror image of "works on my
    # machine", and it is the more dangerous of the two: the failure lands on a
    # person rather than on a build.
    parse_out="$(timeout 6 env -u HYPRLAND_INSTANCE_SIGNATURE \
                     -u WAYLAND_DISPLAY -u DISPLAY \
                     WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
                     labwc -C "$d" 2>&1 \
                 | grep -iE 'invalid argument for action|invalid action|unexpected element' \
                 || true)"
    if [ -z "$parse_out" ]; then
        ok "labwc parses rc.xml with no errors"
    else
        printf '%s\n' "$parse_out" | head -5
        bad "labwc parses rc.xml with no errors"
    fi
else
    printf 'SKIP  labwc unavailable; cannot validate action names\n'
fi

# The assertion that keeps the fix above from being undone. If a future change
# lets labwc attach to the developer's session again, this fails here instead
# of putting a window on someone's workspace mid-task.
_socks_after=$(find "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" -maxdepth 1 \
                    -name 'wayland-*' -type s 2>/dev/null | wc -l)
if [ "$_socks_before" = "$_socks_after" ]; then
    ok "no compositor was started on the session running the tests"
else
    bad "no compositor was started on the session running the tests"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  labwc window chrome (Rime Floating)
#
#  themerc-override is the first-boot default; matugen overwrites this exact
#  path from the live palette once the user changes wallpaper. labwc IGNORES an
#  unrecognised theme key in silence, so a typo here does not fail a session, it
#  just quietly leaves that element on the built-in grey — which is exactly the
#  Openbox-fallback look the Floating pass exists to remove. Hence a key-name
#  check rather than a parse check.
# ─────────────────────────────────────────────────────────────────────────────
# ─────────────────────────────────────────────────────────────────────────────
#  labwc input settings
#
#  labwc ignores an unrecognised element in SILENCE — no warning, no error, the
#  session starts fine and the setting simply does not exist. That is how
#  `<tapToClick>yes</tapToClick>` shipped: not a labwc element (it is `<tap>`),
#  so the line did nothing, and it looked correct because tap-to-click is on by
#  default anyway.
#
#  So the check is against the NAMES labwc actually implements, taken from the
#  package's own exhaustive reference rather than from memory.
# ─────────────────────────────────────────────────────────────────────────────
# ─────────────────────────────────────────────────────────────────────────────
#  Hyprland window-rule migration
#
#  The migration and the shipped template must agree, because they configure the
#  same compositor. They did not, twice, in opposite directions: the template was
#  once "corrected" to Hyprland 0.51.1 syntax after checking the PUBLISHED core
#  image, which was stale — Containerfile.core builds 0.56.2, where only the
#  `match:` forms parse.
#
#  So this asserts the migration's OUTPUT equals what the template ships. That is
#  the invariant; which syntax is currently right is the Containerfile's
#  `Hyprland --verify-config` assertion to decide.
# ─────────────────────────────────────────────────────────────────────────────
section "Hyprland rule migration agrees with the template"

HYPR_TMPL="${ROOT}/files/desktop/hypr/hyprland.lua"
HYPR_MODULES="${ROOT}/files/desktop/hypr/rime"
if [ ! -f "$HYPR_TMPL" ]; then
    bad "the Hyprland template is present"
else
    mig="${WORK}/mig.conf"
    # The pre-0.54 spellings an upgrading user would still have on disk.
    {
        printf 'windowrule = suppressevent maximize, class:.*\n'
        printf 'windowrule = nofocus, class:^$, title:^$, xwayland:1, floating:1, fullscreen:0, pinned:0\n'
    } > "$mig"

    # Run the real block against it, not a copy of the sed.
    # HYPR_LEGACY_CONF, not HYPR_CONF: since P0-025 the seeded config is
    # hyprland.lua and HYPR_CONF names it, while this block is precisely the one
    # that still has to reach a leftover hyprlang hyprland.conf — it runs before
    # rime-hypr-migrate converts it.
    HOME="${WORK}/mighome" bash -c '
        set -euo pipefail
        log() { :; }
        KB_LAYOUT=us; KB_VARIANT=; RIME_ACCENT="#D9F99D"
        render_hypr_tmpl() { cat "$1"; }
        HYPR_LEGACY_CONF="$2"
        mkdir -p "$(dirname "$2")"
        source "$1"
    ' -- "${WORK}/hypr-mig-block.sh" "$mig" >/dev/null 2>&1 || true

    for want in 'suppress_event maximize, match:class' 'no_focus on'; do
        grep -qF "$want" "$mig" \
            && ok "migration produces: ${want}" || bad "migration produces: ${want}"
    done
    # And the shipped tree expresses the same two rules. It cannot ship the same
    # SPELLING any more — the rules are hl.window_rule calls in rime/rules.lua
    # since P0-025 — so the invariant is checked semantically: both rules are
    # present, and the pre-0.54 spellings the sed above removes are absent from
    # the tree the sed can no longer reach.
    for want in 'suppress_event = "maximize"' 'no_focus = true'; do
        grep -qF "$want" "${HYPR_MODULES}/rules.lua" \
            && ok "the shipped rules module expresses: ${want}" \
            || bad "the shipped rules module expresses: ${want}"
    done
    grep -qE 'suppressevent|nofocus,' "${HYPR_MODULES}/rules.lua" \
        && bad "the shipped rules module carries no pre-0.54 spelling" \
        || ok "the shipped rules module carries no pre-0.54 spelling"
    # Nothing may still carry the pre-0.54 spelling after migrating.
    grep -qE 'suppressevent|nofocus,' "$mig" \
        && bad "no pre-0.54 spelling survives migration" \
        || ok "no pre-0.54 spelling survives migration"

    # The togglesplit rewrite, every login: once, not once per login. It used
    # to match its own output and grow a `layoutmsg, ` per login (180 of them,
    # measured on a real config). A grown line is collapsed back.
    run_mig() {
        HOME="${WORK}/mighome" bash -c '
            set -euo pipefail
            log() { :; }
            HYPR_LEGACY_CONF="$2"
            source "$1"
        ' -- "${WORK}/hypr-mig-block.sh" "$1" >/dev/null 2>&1 || true
    }
    ts="${WORK}/togglesplit.conf"
    printf 'bind = $mainMod, J, togglesplit\nbind = SUPER, K, layoutmsg, layoutmsg, layoutmsg, togglesplit\n' > "$ts"
    run_mig "$ts"; run_mig "$ts"; run_mig "$ts"
    [ "$(grep -c 'layoutmsg, togglesplit$' "$ts")" = 2 ] && ! grep -q 'layoutmsg, layoutmsg' "$ts" \
        && ok "the togglesplit rewrite happens once, however many logins run it" \
        || bad "the togglesplit rewrite happens once, however many logins run it ($(tr '\n' '|' < "$ts"))"
    # The shell's migration leaves the old fragment name as a symlink.
    mkdir -p "${WORK}/tslink"
    printf 'bind = SUPER, J, togglesplit\n' > "${WORK}/tslink/RimeShellKeybinds.conf"
    ln -s RimeShellKeybinds.conf "${WORK}/tslink/ApexShellKeybinds.conf"
    run_mig "${WORK}/tslink/ApexShellKeybinds.conf"
    [ -L "${WORK}/tslink/ApexShellKeybinds.conf" ] \
        && grep -q 'layoutmsg, togglesplit' "${WORK}/tslink/RimeShellKeybinds.conf" \
        && ok "a symlinked fragment is migrated through the link, which stays a link" \
        || bad "a symlinked fragment is migrated through the link, which stays a link"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  Every generated module the template requires must be SAFE TO BE MISSING.
#
#  This section used to assert the opposite, because hyprlang needed it:
#  Hyprland treats a `source =` with no matching file as a FATAL config error
#  and refuses the ENTIRE config, so on boot #1 — before the shell and the
#  settings generators have written anything — every sourced file had to be
#  pre-created or the user got no keybinds and no window rules at all.
#
#  Lua removed the need and replaced it with a sharper failure. An uncaught
#  `require` of a missing module does not just fail: it aborts the config AND
#  skips every hl.* call below it. hyprland.lua's loader therefore asks
#  package.searchpath first and treats absent as "not generated yet".
#
#  So the invariant is now the reverse one, and it is checked the only way that
#  proves anything: build the seeded tree with the generated modules genuinely
#  absent and hand it to the Hyprland in this image.
# ─────────────────────────────────────────────────────────────────────────────
section "a generated module that does not exist yet is survivable"

if [ ! -f "$HYPR_TMPL" ]; then
    bad "the Hyprland template is present"
else
    # The loader has to look before it leaps. Asserted separately from the parse
    # below because a template that dropped the guard would still parse here —
    # the modules are absent, so a bare require would abort, but a template that
    # required nothing at all would pass a parse check while doing nothing.
    grep -q 'package.searchpath' "$HYPR_TMPL" \
        && ok "the loader checks package.searchpath before requiring" \
        || bad "the loader checks package.searchpath before requiring"

    # Every generated module is named, and none of them is shipped.
    for gen in monitors input shell-keybinds user-overrides; do
        if grep -q "^rime(\"${gen}\")" "$HYPR_TMPL"; then
            ok "the template requires the generated module: ${gen}"
        else
            bad "the template requires the generated module: ${gen}"
        fi
        [ -e "${HYPR_MODULES}/${gen}.lua" ] \
            && bad "${gen}.lua is generated, so the image must not ship one" \
            || ok "${gen}.lua is generated, so the image must not ship one"
    done

    if ! command -v Hyprland >/dev/null 2>&1; then
        skip "Hyprland is not installed; cannot parse the seeded tree"
    else
        CH="${WORK}/cfghome"
        mkdir -p "${CH}/rime"
        sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$HYPR_TMPL" > "${CH}/hyprland.lua"
        for f in "${HYPR_MODULES}"/*.lua; do
            sed -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g' "$f" \
                > "${CH}/rime/$(basename "$f")"
        done
        rt="$(mktemp -d)"; chmod 0700 "$rt"
        out="$(XDG_RUNTIME_DIR="$rt" timeout 60 env -u WAYLAND_DISPLAY \
                 -u HYPRLAND_INSTANCE_SIGNATURE Hyprland --i-am-really-stupid \
                 --verify-config --config "${CH}/hyprland.lua" 2>&1 || true)"
        rm -rf "$rt"
        case "$out" in
            *"config ok"*) ok "the seeded tree parses with every generated module absent" ;;
            *) bad "the seeded tree parses with every generated module absent: ${out##*Config parsing result:}" ;;
        esac

        # ...and a module that is present but BROKEN must not take the rest with
        # it. This is the half that pays for the loader's pcall: the defaults
        # are already applied by the time a bad generated file is reached, so a
        # settings page writing one wrong line costs that page, not the desktop.
        printf 'this is not lua(((\n' > "${CH}/rime/monitors.lua"
        rt="$(mktemp -d)"; chmod 0700 "$rt"
        out="$(XDG_RUNTIME_DIR="$rt" timeout 60 env -u WAYLAND_DISPLAY \
                 -u HYPRLAND_INSTANCE_SIGNATURE Hyprland --i-am-really-stupid \
                 --verify-config --config "${CH}/hyprland.lua" 2>&1 || true)"
        rm -rf "$rt"
        case "$out" in
            *"config ok"*) bad "a broken generated module is REPORTED, not swallowed" ;;
            *monitors*)    ok "a broken generated module is reported by name" ;;
            *)             bad "a broken generated module is reported by name: ${out##*Config parsing result:}" ;;
        esac
    fi
fi

section "labwc input settings"

# labwc ships rc.xml.all as its complete annotated reference. Preferring it over
# a hardcoded list means the check tracks the installed labwc rather than
# whatever was true when this test was written.
LABWC_REF=""
for cand in /usr/share/doc/labwc/rc.xml.all /usr/share/doc/labwc-*/rc.xml.all; do
    [ -f "$cand" ] && { LABWC_REF="$cand"; break; }
done

if [ -z "$LABWC_REF" ]; then
    printf 'SKIP  labwc rc.xml.all unavailable; cannot check libinput element names\n'
else
    ok "labwc's own element reference is available"

    # Every element name labwc documents inside <libinput>, commented or not.
    sed -n '/<libinput>/,/<\/libinput>/p' "$LABWC_REF" \
        | grep -oE '<[a-zA-Z]+>' | tr -d '<>' | sort -u > "${WORK}/labwc-input-known"

    # Every element name the shipped config actually uses.
    sed -n '/<libinput>/,/<\/libinput>/p' "${TMPL}/rc.xml" \
        | grep -vE '^\s*<!--' \
        | grep -oE '<[a-zA-Z]+>' | tr -d '<>' | sort -u > "${WORK}/labwc-input-used"

    unknown=""
    while IFS= read -r el; do
        case "$el" in libinput|device) continue ;; esac
        grep -qxF "$el" "${WORK}/labwc-input-known" || unknown="${unknown} ${el}"
    done < "${WORK}/labwc-input-used"

    if [ -z "$unknown" ]; then
        ok "every <libinput> element is one labwc implements"
    else
        printf '  not implemented by labwc:%s\n' "$unknown"
        printf '  (labwc ignores these silently, so the setting does nothing)\n'
        bad "every <libinput> element is one labwc implements"
    fi

    # The specific regression, named, so it cannot come back quietly.
    #
    # Non-comment lines only: the comment above the block explains what went
    # wrong and therefore contains the string "<tapToClick>". Grepping the whole
    # file made the documentation trip the check on the fixed config.
    live_libinput() {
        sed -n '/<libinput>/,/<\/libinput>/p' "${TMPL}/rc.xml" | grep -vE '^\s*<!--|^\s*[a-zA-Z]'
    }
    live_libinput | pipe_has '<tapToClick>' \
        && bad "tap-to-click uses labwc's own element name" \
        || ok "tap-to-click uses labwc's own element name"
    live_libinput | pipe_has '<tap>yes</tap>' \
        && ok "tap-to-click is enabled with <tap>" \
        || bad "tap-to-click is enabled with <tap>"
fi

section "labwc window chrome"
THEMERC="${TMPL}/themerc-override"
if [ ! -f "$THEMERC" ]; then
    bad "themerc-override is shipped"
else
    ok "themerc-override is shipped"

    # The accent placeholder must be present in the template and gone after
    # seeding: an unsubstituted @ACCENT@ is not a colour, and labwc drops the
    # line, leaving the active border on the default.
    grep -q '@ACCENT@' "$THEMERC" \
        && ok "themerc-override carries the @ACCENT@ placeholder" \
        || bad "themerc-override carries the @ACCENT@ placeholder"

    # The seeding half only has an answer where the block actually ran. The
    # seeding block is guarded on labwc being installed, so on a runner without
    # it there is no seeded file to inspect and asserting one would be checking
    # the guard, not the behaviour.
    if ! command -v labwc >/dev/null 2>&1 && [ ! -x /usr/bin/labwc ]; then
        printf 'SKIP  labwc not installed; nothing was seeded to inspect\n'
    elif [ -f "${h}/.config/labwc/themerc-override" ]; then
        ok "themerc-override is seeded into the user config"
        grep -q '@ACCENT@' "${h}/.config/labwc/themerc-override" \
            && bad "seeded themerc-override has no unsubstituted placeholder" \
            || ok "seeded themerc-override has no unsubstituted placeholder"
        grep -qE '^window\.active\.border\.color: #[0-9A-Fa-f]{6}$' \
             "${h}/.config/labwc/themerc-override" \
            && ok "the seeded accent is a real hex colour" \
            || bad "the seeded accent is a real hex colour"
    else
        bad "themerc-override is seeded into the user config"
    fi

    # Every key must be one labwc actually knows. The man page is the only
    # authority; without it this is unverifiable and skipping is honest.
    if man 5 labwc-theme >/dev/null 2>&1; then
        man 5 labwc-theme 2>/dev/null | col -b \
            | grep -oE "^ {3,7}[a-z][a-z0-9.*-]+" | tr -d ' ' | sort -u \
            > "${WORK}/labwc-theme-keys"
        unknown=""
        while IFS= read -r key; do
            grep -qxF "$key" "${WORK}/labwc-theme-keys" || unknown="${unknown} ${key}"
        done <<EOF
$(grep -vE '^\s*#|^\s*$' "$THEMERC" | sed 's/:.*//' | tr -d ' ' | sort -u)
EOF
        if [ -z "$unknown" ]; then
            ok "every themerc-override key is one labwc documents"
        else
            printf '  unknown keys:%s\n' "$unknown"
            bad "every themerc-override key is one labwc documents"
        fi
    else
        printf 'SKIP  labwc-theme(5) unavailable; cannot validate theme key names\n'
    fi

    # Geometry has to stay in step with rc.xml: labwc derives titlebar height
    # from the font, so the 36-40 px target only holds for this pairing.
    grep -q '<name>Noto Sans</name>' "${TMPL}/rc.xml" \
        && ok "rc.xml uses a proportional face for window chrome" \
        || bad "rc.xml uses a proportional face for window chrome"
    grep -qE '<cornerRadius>1[0-2]</cornerRadius>' "${TMPL}/rc.xml" \
        && ok "rc.xml sets a 10-12 px corner radius" \
        || bad "rc.xml sets a 10-12 px corner radius"
    grep -qE '^window\.titlebar\.padding\.height: 11$' "$THEMERC" \
        && ok "titlebar padding matches the 36-40 px target" \
        || bad "titlebar padding matches the 36-40 px target"
    # Both dimensions, and the hover radius that has to be half of them for the
    # highlight to be a circle. An `(width|height)` alternation here would pass
    # with one of the two wrong.
    if grep -qE '^window\.button\.width: 30$' "$THEMERC" \
       && grep -qE '^window\.button\.height: 30$' "$THEMERC" \
       && grep -qE '^window\.button\.hover\.bg\.corner-radius: 15$' "$THEMERC"; then
        ok "window buttons are 30x30 with a circular hover"
    else
        bad "window buttons are 30x30 with a circular hover"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
#  labwc keybinds vs the shell's own defaults
#
#  Maintained by hand (labwc has no IPC to push bindings over and no include
#  mechanism), so they can drift silently. Only runnable where a shell tree is
#  available; the image build runs the same script against the vendored copy.
# ─────────────────────────────────────────────────────────────────────────────
section "labwc keybinds match the shell defaults"
CHECK="${ROOT}/files/scripts/check-labwc-keybinds"
SHELL_TREE=""
# The CHECKOUT first, then the installed copy. The image build calls the checker
# with an explicit /usr/share/rime-shell path (Containerfile.base), so this
# ordering only affects a local run — and there, the vendored copy on the machine
# is whatever the last image shipped, which lags the tree being tested. Checking
# a change against a stale source of truth fails for a reason that has nothing
# to do with the change.
for cand in "${ROOT}/../rime-shell" /usr/share/rime-shell; do
    [ -f "${cand}/src/services/config_tab/KeybindService.qml" ] && { SHELL_TREE="$cand"; break; }
done

if [ -z "$SHELL_TREE" ]; then
    printf 'SKIP  no rime-shell tree available to compare against\n'
elif ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP  python3 unavailable\n'
else
    if python3 "$CHECK" "$SHELL_TREE" "${TMPL}/rc.xml" >/dev/null 2>&1; then
        ok "every shell popup bind matches KeybindService"
    else
        # `| head -12` here used to END THE SUITE. This file is `set -euo
        # pipefail`; the checker prints 13 lines, head took 12 and closed the
        # pipe, python died of SIGPIPE, pipefail surfaced 141 and errexit
        # exited — before `bad` ran, before the summary, and before every
        # section below this one. So a genuine mismatch reported as a crash
        # with no verdict, and any assertion added after this point silently
        # never ran. `awk` reads its input to the end, so there is no early
        # close and no SIGPIPE; `|| true` is for the checker's own non-zero
        # exit, which is the thing being reported rather than an error.
        python3 "$CHECK" "$SHELL_TREE" "${TMPL}/rc.xml" 2>&1 | awk 'NR <= 12' || true
        bad "every shell popup bind matches KeybindService"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
#  niri: the stock waybar spawn
#
#  MEASURED ON KATANA 2026-09-19 (evidence §5.4): the niri session ran waybar
#  AND quickshell. niri's upstream default-config.kdl — which is what lands in
#  ~/.config/niri/config.kdl whether niri writes it or this script copies it —
#  carries `spawn-at-startup "waybar"` at line 271, and this script only ever
#  APPENDED to that file, so every niri user got two bars.
#
#  The block edits a file that belongs to the user, so the properties that
#  matter are the ones the labwc section above cares about too: it changes one
#  line and only that line, it is idempotent, it leaves a config the user
#  edited alone, and it never leaves niri with something niri refuses.
# ─────────────────────────────────────────────────────────────────────────────
section "niri: one bar, not two"

sed -n '/^    NIRI_BIN=/,/^    fi$/p' "$SRC" > "${WORK}/niri-bar-block.sh"
grep -q 'NIRI_STOCK_WAYBAR' "${WORK}/niri-bar-block.sh" \
    || { printf 'could not extract the niri waybar block\n' >&2; exit 1; }

NIRI_BIN_T="$(command -v niri 2>/dev/null || echo /usr/bin/niri)"
run_niri_bar() {  # <config path>
    NIRI_CONF="$1" bash -c '
        set -uo pipefail
        log() { :; }
        source "$1"
    ' -- "${WORK}/niri-bar-block.sh"
}

# The real upstream default, not a hand-typed excerpt: a two-line paraphrase
# would pass a test that the actual file fails.
UPSTREAM=""
for cand in /usr/share/doc/niri/default-config.kdl /usr/share/niri/default-config.kdl; do
    [ -f "$cand" ] && { UPSTREAM="$cand"; break; }
done

if [ ! -x "$NIRI_BIN_T" ]; then
    skip "niri is not installed; the waybar block is not exercised"
elif [ -z "$UPSTREAM" ]; then
    skip "niri's default-config.kdl is not on this machine; nothing to transform"
else
    c="${WORK}/niri-stock.kdl"
    cp "$UPSTREAM" "$c"
    before_lines="$(wc -l < "$c")"
    # Proves the fixture really is the broken shape. Without it every
    # assertion below could be measuring a file that never had the line.
    grep -qxF 'spawn-at-startup "waybar"' "$c" \
        && ok "the upstream default really does start waybar" \
        || bad "the upstream default really does start waybar"
    line="$(grep -nxF 'spawn-at-startup "waybar"' "$c" | cut -d: -f1)"

    run_niri_bar "$c"

    if grep -qxF 'spawn-at-startup "waybar"' "$c"; then
        bad "the stock waybar spawn is disabled"
    else
        ok "the stock waybar spawn is disabled"
    fi
    [ "$(wc -l < "$c")" = "$before_lines" ] \
        && ok "the file has exactly as many lines as before" \
        || bad "the file has exactly as many lines as before"
    # The whole file, minus the one line, byte for byte. A substitution
    # anywhere else could not survive this.
    a="$(sed "${line}d" "$UPSTREAM" | sha256sum)"
    b="$(sed "${line}d" "$c" | sha256sum)"
    [ "$a" = "$b" ] \
        && ok "not one other byte of the user's config changed" \
        || bad "not one other byte of the user's config changed"
    "$NIRI_BIN_T" validate --config "$c" >/dev/null 2>&1 \
        && ok "niri still accepts the config" \
        || bad "niri still accepts the config" \
               "$("$NIRI_BIN_T" validate --config "$c" 2>&1 | head -5)"
    [ -f "${c}.pre-rime-bar.bak" ] \
        && ok "a backup of the original is kept beside it" \
        || bad "a backup of the original is kept beside it"
    [ -z "$(find "${WORK}" -maxdepth 1 -name 'niri-stock.kdl.rimenew.*')" ] \
        && ok "no temporary file is left behind" \
        || bad "no temporary file is left behind"

    # Idempotence: a per-login unit runs this every single login.
    sum_once="$(sha256sum < "$c")"
    run_niri_bar "$c"
    run_niri_bar "$c"
    [ "$(sha256sum < "$c")" = "$sum_once" ] \
        && ok "running it again changes nothing" \
        || bad "running it again changes nothing"

    # A user who edited that line meant it. Only upstream's exact spelling at
    # column 0 is touched.
    c2="${WORK}/niri-user.kdl"
    sed 's|^spawn-at-startup "waybar"$|spawn-at-startup "waybar" // I want this|' \
        "$UPSTREAM" > "$c2"
    cp "$c2" "${WORK}/niri-user.orig"
    run_niri_bar "$c2"
    cmp -s "$c2" "${WORK}/niri-user.orig" \
        && ok "a waybar line the user edited is left alone" \
        || bad "a waybar line the user edited is left alone"

    # A config that is already broken is not this block's to make worse.
    c3="${WORK}/niri-broken.kdl"
    { cat "$UPSTREAM"; printf 'this-is-not-a-niri-node {\n'; } > "$c3"
    cp "$c3" "${WORK}/niri-broken.orig"
    run_niri_bar "$c3"
    cmp -s "$c3" "${WORK}/niri-broken.orig" \
        && ok "a config niri already rejects is not edited" \
        || bad "a config niri already rejects is not edited"

    # And a config that never had the line is not invented into one.
    c4="${WORK}/niri-nobar.kdl"
    grep -vxF 'spawn-at-startup "waybar"' "$UPSTREAM" > "$c4"
    cp "$c4" "${WORK}/niri-nobar.orig"
    run_niri_bar "$c4"
    cmp -s "$c4" "${WORK}/niri-nobar.orig" \
        && ok "a config with no stock waybar spawn is untouched" \
        || bad "a config with no stock waybar spawn is untouched"
fi

printf '\nrime-shell-firstrun: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
[ "$fail" -eq 0 ]
