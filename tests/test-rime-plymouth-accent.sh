#!/usr/bin/env bash
# The boot and shutdown splash follow the owner's matugen accent, from the FIRST
# frame: the theme is chosen before plymouthd starts, never switched mid-way.
#
#   writer  files/system/libexec/rime-plymouth-accent (real root): accent file →
#           hue bucket → /etc/plymouth/plymouthd.conf (shutdown) and
#           loader/credentials/rime.accent.cred on the ESP (boot)
#   reader  files/dracut/rime-plymouth-accent/rime-plymouth-theme (initramfs,
#           before plymouth-start): the credential → the initramfs's
#           plymouthd.conf
#
# Both run here for real, against directories standing in for the ESP.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WRITER="$ROOT/files/system/libexec/rime-plymouth-accent"
READER="$ROOT/files/dracut/rime-plymouth-accent/rime-plymouth-theme"
THEMES="$ROOT/files/branding/plymouth"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
pass=0 fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }

# write <accent or ABSENT> -> sets up a machine, runs the writer; echoes its dir
write() {
    local m="$WORK/m-$RANDOM"
    mkdir -p "$m/greet/accents" "$m/esp" "$m/etc"
    printf 'andre' > "$m/greet/last-user"
    [ "$1" = ABSENT ] || printf '%s\n' "$1" > "$m/greet/accents/andre"
    printf '[Daemon]\nTheme=rime-os-chartreuse\nShowDelay=0\n' > "$m/etc/plymouthd.conf"
    RIME_GREET_VAR="$m/greet" RIME_PLYMOUTH_CONF="$m/etc/plymouthd.conf" \
        RIME_PLYMOUTH_THEMES="$THEMES" RIME_ESP_DIRS="$m/esp" sh "$WRITER"
    printf '%s' "$m"
}
# read <machine dir> -> runs the reader against that ESP; echoes the theme chosen
boot() {
    local c="$1/initrd-plymouthd.conf"
    printf '[Daemon]\nTheme=rime-os-chartreuse\n' > "$c"
    RIME_PLYMOUTH_CONF="$c" RIME_PLYMOUTH_THEMES="$THEMES" RIME_ESP_DIRS="$1/esp" sh "$READER"
    sed -n 's/^Theme=//p' "$c"
}

m="$(write '#fab898')"
[ "$(cat "$m/esp/loader/credentials/rime.accent.cred" 2>/dev/null)" = "01" ] \
    && ok "#fab898 (peach) writes bucket 01 to the ESP credential" \
    || bad "#fab898 (peach) writes bucket 01 to the ESP credential"
grep -qx 'Theme=rime-os-accent-01' "$m/etc/plymouthd.conf" && grep -qx 'ShowDelay=0' "$m/etc/plymouthd.conf" \
    && ok "shutdown's plymouthd.conf names the accent theme, other settings kept" \
    || bad "shutdown's plymouthd.conf names the accent theme, other settings kept"
[ "$(boot "$m")" = "rime-os-accent-01" ] \
    && ok "the initramfs reader starts plymouth in rime-os-accent-01" \
    || bad "the initramfs reader starts plymouth in rime-os-accent-01 (got '$(boot "$m")')"

for pair in '#d9f99d 05' '#a6d0f7 14' '#fa98a0 00'; do
    set -- $pair
    m="$(write "$1")"
    [ "$(boot "$m")" = "rime-os-accent-$2" ] && ok "$1 boots in rime-os-accent-$2" \
        || bad "$1 boots in rime-os-accent-$2 (got '$(boot "$m")')"
done

m="$(write '#fab898')"; mv "$m/greet/accents/andre" "$m/x"; printf '#c8c8c8\n' > "$m/greet/accents/andre"
RIME_GREET_VAR="$m/greet" RIME_PLYMOUTH_CONF="$m/etc/plymouthd.conf" RIME_PLYMOUTH_THEMES="$THEMES" \
    RIME_ESP_DIRS="$m/esp" sh "$WRITER"
[ ! -e "$m/esp/loader/credentials/rime.accent.cred" ] && grep -qx 'Theme=rime-os-chartreuse' "$m/etc/plymouthd.conf" \
    && ok "a grey accent removes the credential and restores the default theme" \
    || bad "a grey accent removes the credential and restores the default theme"
[ "$(boot "$m")" = "rime-os-chartreuse" ] && ok "...and the next boot is the default" \
    || bad "...and the next boot is the default (got '$(boot "$m")')"

m="$(write ABSENT)"
[ "$(boot "$m")" = "rime-os-chartreuse" ] && ok "no accent published boots the default" || bad "no accent published boots the default"

m="$(write '#fab898')"; printf 'zz' > "$m/esp/loader/credentials/rime.accent.cred"
[ "$(boot "$m")" = "rime-os-chartreuse" ] && ok "a credential that is not a bucket is ignored" || bad "a credential that is not a bucket is ignored"
printf '99' > "$m/esp/loader/credentials/rime.accent.cred"
[ "$(boot "$m")" = "rime-os-chartreuse" ] && ok "a bucket out of range is ignored" || bad "a bucket out of range is ignored"

# The UKI path: systemd-stub's copy wins over the ESP read.
grep -q '/.extra/global_credentials/rime.accent.cred' "$READER" \
    && ok "the reader takes the stub-delivered credential first (UKI path)" \
    || bad "the reader takes the stub-delivered credential first (UKI path)"

# Every bucket the writer can name is a complete theme: every image
# rime-os.script loads (make-splash-art.sh draws them, make-accent-themes.sh
# derives the 24).
SPLASH_FILES="spark.png spark-hd.png spark-blur.png spark-soft.png halo.png bullet.png
              bullet-hd.png wordmark.png wordmark-hd.png prompt.png prompt-hd.png"
missing=0
for n in $(seq 0 23); do
    t="rime-os-accent-$(printf '%02d' "$n")"
    for f in "$t.plymouth" rime-os.script $SPLASH_FILES; do
        [ -s "$THEMES/$t/$f" ] || missing=$((missing + 1))
    done
    grep -q "^ImageDir=/usr/share/plymouth/themes/$t$" "$THEMES/$t/$t.plymouth" || missing=$((missing + 1))
done
[ "$missing" -eq 0 ] && ok "all 24 accent themes are complete" || bad "all 24 accent themes are complete ($missing missing)"
# Every word on the splash is an Image.Text(), and Image.Text() draws nothing
# at all without a label plugin: no error, just an empty sprite. Until
# 2026-09-26 no image had one, so an encrypted machine's passphrase prompt was
# invisible. The package must be installed, and the build must refuse an
# initramfs the plugin or its font did not reach.
grep -q 'Image.Text' "$THEMES"/rime-os-chartreuse/rime-os.script \
    && awk '/^COPY --from=plymouth-builder /{on=1} on{print} on && !/\\$/ && !/^COPY/{exit}' "$ROOT/Containerfile.core" \
        | grep -E 'install /tmp/rime-plymouth/|for p in |plymouth-plugin-' | grep -w 'plymouth-plugin-label' >/dev/null \
    && ok "the splash's text has a renderer: core installs plymouth-plugin-label" \
    || bad "the splash's text has a renderer: core installs plymouth-plugin-label"
_want="$(sed -n '/for want in usr\/bin\/rime-plymouth-theme/,/; do/p' "$ROOT/Containerfile.rime")"
printf '%s\n' "$_want" | grep -qF 'usr/lib64/plymouth/label-freetype.so' \
    && printf '%s\n' "$_want" | grep -qF 'usr/share/fonts/Plymouth.ttf' \
    && ok "the build asserts the label plugin and its font are in the initramfs" \
    || bad "the build asserts the label plugin and its font are in the initramfs"
# The initramfs draws with the freetype label, which drew U+00B7 as a
# missing-glyph box. Keep every message sent to the splash plain ASCII.
_nonascii="$(grep -rlE 'plymouth +message' "$ROOT/files" 2>/dev/null | grep -v '\.script$' \
    | xargs -r env LC_ALL=C grep -nP '^[^#]*--text=.*[^\x00-\x7F]' 2>/dev/null)"
[ -z "$_nonascii" ] && ok "every splash message is plain ASCII" \
    || bad "every splash message is plain ASCII: $_nonascii"
# No mid-animation switching: the theme is chosen before plymouth starts.
grep -rq 'SetUpdateStatusFunction' "$THEMES"/rime-os-accent-*/rime-os.script "$THEMES"/rime-os-chartreuse/rime-os.script \
    && bad "no theme switches colour mid-animation" || ok "no theme switches colour mid-animation"

# ── the splash's motion rules ──────────────────────────────────────────────
# The "Convergence" splash was choppy for measurable reasons (see the header
# of rime-os.script): it re-scaled/rotated ~45 images every refresh, counted
# frames instead of time, ran at plymouth's default 50 Hz, and sat on a
# gradient. Each of those is a line of script that could quietly come back.
SCRIPT="$THEMES/rime-os-chartreuse/rime-os.script"
# body_of <function> -> the lines of `fun <function> (...) {` up to its closing `}`
body_of() { awk -v f="$1" '$0 ~ "^fun "f" " {p=1} p {print} p && /^}/ {exit}' "$SCRIPT"; }
_frame="$(body_of refresh; body_of draw_bullets; body_of advance_clock)"
[ "$(printf '%s\n' "$_frame" | grep -c '^fun ')" -eq 3 ] \
    && ! printf '%s\n' "$_frame" | grep -vE '^[[:space:]]*#' | grep -qE '\.Scale\(|\.Rotate\(|Image\(|Image\.Text\(' \
    && ok "no image is loaded, scaled, rotated or drawn as text per frame" \
    || bad "no image is loaded, scaled, rotated or drawn as text per frame"
! body_of refresh | grep -vE '^[[:space:]]*#' | grep -qE '\+\+|\+= *1\b' \
    && grep -qE '^Plymouth\.SetBootProgressFunction\(on_progress\);' "$SCRIPT" \
    && body_of refresh | grep -q 'advance_clock();' \
    && ok "the timeline is real elapsed time (progress clock), not a frame count" \
    || bad "the timeline is real elapsed time (progress clock), not a frame count"
_rate="$(sed -n 's/^RATE = \([0-9][0-9]*\);.*/\1/p' "$SCRIPT")"
[ -n "$_rate" ] && [ "$_rate" -ge 60 ] && grep -q '^Plymouth.SetRefreshRate(RATE);' "$SCRIPT" \
    && ok "refreshes at ${_rate:-?} Hz, not plymouth's default 50" \
    || bad "refreshes at 60 Hz or more, not plymouth's default 50 (RATE='$_rate')"
grep -q '^Window.SetBackgroundTopColor(0, 0, 0);' "$SCRIPT" \
    && grep -q '^Window.SetBackgroundBottomColor(0, 0, 0);' "$SCRIPT" \
    && [ "$(grep -c 'Window.SetBackground' "$SCRIPT")" -eq 2 ] \
    && ok "the background is plain black: no gradient" \
    || bad "the background is plain black: no gradient"
grep -q '^if (mode == "boot") { intro = 1; }' "$SCRIPT" \
    && ok "only a boot plays the intro; shutdown and reboot show the settled splash" \
    || bad "only a boot plays the intro; shutdown and reboot show the settled splash"
# every image the script loads is in every theme it can boot in
_imgs="$(grep -oE 'Image\("[^"]+"\)' "$SCRIPT" | sed -E 's/Image\("(.*)"\)/\1/' | sort -u)"
_gone=""
for t in "$THEMES"/rime-os-accent-* "$THEMES"/rime-os-chartreuse "$THEMES"/rime-os-gold; do
    for f in $_imgs; do [ -s "$t/$f" ] || _gone="$_gone ${t##*/}/$f"; done
done
[ -n "$_imgs" ] && [ -z "$_gone" ] \
    && ok "every image the script loads ($(printf '%s\n' "$_imgs" | wc -l)) is in all 26 themes" \
    || bad "every image the script loads is in all 26 themes, missing:$_gone"
# The 24 accents and gold are generated from the chartreuse script: they may
# differ from it in the highlight line and nowhere else.
_drift=""
for t in "$THEMES"/rime-os-accent-* "$THEMES"/rime-os-gold; do
    cmp -s <(grep -v '^HI_R = ' "$t/rime-os.script") <(grep -v '^HI_R = ' "$SCRIPT") || _drift="$_drift ${t##*/}"
done
[ -z "$_drift" ] && ok "every theme runs the chartreuse script (only the highlight differs)" \
    || bad "every theme runs the chartreuse script, drifted:$_drift (rerun make-accent-themes.sh)"
# The offline preview ports the script's curves; it lists the lines it ports,
# and a retune that leaves the preview behind must fail here, not in review.
_ported="$(python3 - "$THEMES/render-preview.py" "$SCRIPT" <<'PY'
import ast, sys
tree = ast.parse(open(sys.argv[1]).read())
lines = next(ast.literal_eval(n.value) for n in tree.body
             if isinstance(n, ast.Assign) and getattr(n.targets[0], "id", "") == "PORTED")
src = open(sys.argv[2]).read()
print("\n".join(l for l in lines if l not in src))
print(f"#{len(lines)}")
PY
)"
_nported="$(printf '%s\n' "$_ported" | sed -n 's/^#//p')"
[ "${_nported:-0}" -gt 10 ] && [ "$(printf '%s\n' "$_ported" | grep -v '^#' | grep -c .)" -eq 0 ] \
    && ok "render-preview.py ports the script's current curves (${_nported} lines checked)" \
    || bad "render-preview.py ports the script's current curves; stale: $(printf '%s\n' "$_ported" | grep -v '^#' | grep . | head -3)"

printf '\nrime plymouth-accent: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
