#!/usr/bin/env bash
# The starship prompt follows the wallpaper: rime-shell-firstrun makes
# ~/.config/starship.toml a symlink to the matugen-rendered
# ~/.cache/rime-shell/starship.toml, and migrates a static seed an earlier
# release wrote. It must never touch a file or a link the user made.
#
# The block under test is cut out of rime-shell-firstrun itself (from
# STARSHIP_CACHE= to the end of its if/elif), so this suite fails loudly if the
# block moves rather than passing against a copy of it.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIRSTRUN="${ROOT}/files/system/libexec/rime-shell-firstrun"
SEED="${ROOT}/files/desktop/starship/starship.toml"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0 fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
finish() { printf '\nrime starship-link: %d passed, %d failed\n' "$pass" "$fail"; [ "$fail" -eq 0 ]; exit; }

BLOCK="${WORK}/block.sh"
sed -n '/^STARSHIP_CACHE=/,/^elif \[ -f "\${STARSHIP_TMPL}" \]; then$/p' "$FIRSTRUN" | sed '$d' > "$BLOCK"
echo "fi" >> "$BLOCK"
if grep -q 'ln -s "\${STARSHIP_CACHE}"' "$BLOCK" && bash -n "$BLOCK"; then
    ok "the starship block is found in rime-shell-firstrun"
else
    bad "the starship block is found in rime-shell-firstrun"; finish
fi

# A stand-in for the shell's matugen template: the seed with the matugen tag.
SHELL_SRC="${WORK}/rime-shell"
mkdir -p "${SHELL_SRC}/src/config"
sed 's|@ACCENT@|{{colors.primary.default.hex}}|g' "$SEED" > "${SHELL_SRC}/src/config/starship.toml.template"

run() {
    env -i PATH="$PATH" HOME="$1" SRC_DIR="$SHELL_SRC" STARSHIP_TMPL="$SEED" \
        RIME_ACCENT="#D9F99D" RIME_VARIANT=daily \
        bash -c 'log(){ echo "log: $*"; }; . "$1"' _ "$BLOCK"
}
home() { local h; h="$(mktemp -d -p "$WORK")"; mkdir -p "$h/.config"; printf '%s' "$h"; }

# ── nothing there: link, and a cache the link can point at ─────────────────
H="$(home)"; run "$H" >/dev/null
[ "$(readlink "$H/.config/starship.toml")" = "$H/.cache/rime-shell/starship.toml" ] \
    && ok "a fresh account gets a link to the wallpaper-themed prompt" \
    || bad "a fresh account gets a link to the wallpaper-themed prompt"
grep -q 'bold #D9F99D' "$H/.cache/rime-shell/starship.toml" 2>/dev/null \
    && ok "until matugen runs, the cache carries the edition accent" \
    || bad "until matugen runs, the cache carries the edition accent"
grep -q '{{' "$H/.cache/rime-shell/starship.toml" 2>/dev/null \
    && bad "the seeded cache has no unrendered matugen tag" \
    || ok "the seeded cache has no unrendered matugen tag"

# ── an earlier release's untouched seed, either edition: migrated ──────────
for accent in "#91C2DC" "#D9F99D" "#FDE047"; do
    H="$(home)"; sed "s|@ACCENT@|${accent}|g" "$SEED" > "$H/.config/starship.toml"
    run "$H" >/dev/null
    [ -L "$H/.config/starship.toml" ] && [ -f "$H/.config/starship.toml.rime-seed" ] \
        && ok "an untouched ${accent} seed becomes the link, and is kept aside" \
        || bad "an untouched ${accent} seed becomes the link, and is kept aside"
done
out="$(run "$H")"
[ -z "$out" ] && ok "a second login changes nothing" || bad "a second login changes nothing ($out)"

# ── a placeholder a failed run left behind: Rime's, migrated ───────────────
H="$(home)"; cp "$SEED" "$H/.config/starship.toml"; run "$H" >/dev/null
[ -L "$H/.config/starship.toml" ] \
    && ok "a leftover @ACCENT@ file is replaced by the link" \
    || bad "a leftover @ACCENT@ file is replaced by the link"

# ── the user's own file or link: never touched ─────────────────────────────
H="$(home)"; sed 's|@ACCENT@|#123456|g' "$SEED" > "$H/.config/starship.toml"
before="$(sha256sum < "$H/.config/starship.toml")"; run "$H" >/dev/null
[ ! -L "$H/.config/starship.toml" ] && [ "$(sha256sum < "$H/.config/starship.toml")" = "$before" ] \
    && [ ! -e "$H/.config/starship.toml.rime-seed" ] \
    && ok "a customised file is left exactly as it is" \
    || bad "a customised file is left exactly as it is"
H="$(home)"; ln -s /elsewhere/mine.toml "$H/.config/starship.toml"; run "$H" >/dev/null
[ "$(readlink "$H/.config/starship.toml")" = "/elsewhere/mine.toml" ] \
    && ok "a link the user made is left alone" \
    || bad "a link the user made is left alone"

finish
