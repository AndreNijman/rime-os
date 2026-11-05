#!/usr/bin/env bash
# The bootc image keeps the chartreuse spark; the layered ridge in Rime Blue is NixOS's (AGENTS.md "Branding").
# Containerfile.base copies these shared files as they are, so they must carry the spark, and the NixOS build turns
# them into the ridge with nix/branding/ridge.patch (nix/pkgs/rime-ridge-files.nix). This suite checks both halves:
# the files the bootc image ships still show the spark, and the patch still applies to them with no fuzz.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

pass=0 fail=0
ok()  { pass=$((pass + 1)); echo "ok   $*"; }
bad() { fail=$((fail + 1)); echo "FAIL $*"; }
has() { grep -F -- "$2" "$1" >/dev/null 2>&1; }
check() { if has "$1" "$2"; then ok "$3"; else bad "$3"; fi; }
lacks() { if has "$1" "$2"; then bad "$3"; else ok "$3"; fi; }

g=files/desktop/rime-greet
check $g/shell.qml 'greetCtx.edition === "rime"   ? "#d9f99d"' "greeter: the edition accent is chartreuse"
check $g/shell.qml 'assets/spark-chartreuse.png' "greeter: the logo is the spark"
lacks $g/shell.qml 'ridge-lockup-base.png' "greeter: no ridge lockup"
lacks $g/GreetSurface.qml 'ShapePath' "greeter: no ridge plane polygon"
for c in chartreuse gold white; do
    if [ -s "$g/assets/spark-$c.png" ]; then ok "greeter: assets/spark-$c.png ships"; else bad "greeter: assets/spark-$c.png ships"; fi
done
if [ -e "$g/assets/ridge-lockup-base.png" ]; then bad "greeter: the ridge asset is not in the bootc tree"; else ok "greeter: the ridge asset is not in the bootc tree"; fi

check files/desktop/shell/rime-greeting.sh '--logo-color-1 "${_rime_logo_color}"' "greeting: fastfetch draws its logo (the ASCII spark)"
lacks files/desktop/shell/rime-greeting.sh 'rime-term-graphics' "greeting: no image-protocol symbol"
check files/system/fastfetch/config.jsonc 'rime-logo.txt' "fastfetch: the spark logo"
check files/system/fastfetch/config.jsonc '38;2;161;201;153' "fastfetch: chartreuse keys"

f=files/system/libexec/rime-shell-firstrun
check $f '*)       RIME_ACCENT="#D9F99D" ;;   # chartreuse' "firstrun: chartreuse is the seeded accent"
check $f 'hl.bind("SUPER + Return", hl.dsp.exec_cmd("alacritty"))' "firstrun: the last-resort config opens Alacritty"

# The NixOS half only exists on a tree with the flake.
if [ -f nix/branding/ridge.patch ]; then
    if ! command -v patch >/dev/null; then
        bad "patch(1) is needed to check nix/branding/ridge.patch"
    else
        work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
        mkdir -p "$work/desktop/shell" "$work/system/fastfetch"
        cp -r $g "$work/desktop/rime-greet"
        cp files/desktop/shell/rime-greeting.sh "$work/desktop/shell/"
        cp files/system/fastfetch/config.jsonc "$work/system/fastfetch/"
        if patch --batch --forward --fuzz=0 --no-backup-if-mismatch -s -p1 -d "$work" < nix/branding/ridge.patch \
            && [ -z "$(find "$work" -name '*.rej')" ]; then
            ok "nix/branding/ridge.patch applies to the shared files with no fuzz"
            check "$work/desktop/rime-greet/shell.qml" '"#91c2dc"' "patched greeter: Rime Blue"
            check "$work/system/fastfetch/config.jsonc" '38;2;145;194;220' "patched fastfetch: Rime Blue keys"
        else
            bad "nix/branding/ridge.patch applies to the shared files with no fuzz"
        fi
    fi
    for a in nix/branding/rime-greet/assets/ridge-lockup-base.png nix/branding/rime-greet/make-logo-assets.sh; do
        if [ -s "$a" ]; then ok "$a exists"; else bad "$a exists"; fi
    done
fi

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
