#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-editors.sh — the editors Rime offers can actually be launched.
#
#  ── The failure this exists for ─────────────────────────────────────────────
#  Zed shipped on every Rime image, ran fine from a shell, and was invisible in
#  every launcher, menu and "Open With" list: the Containerfile installed the
#  desktop entry from `zed.app/share/applications/zed.desktop`, a name the
#  tarball has never shipped (upstream's file is `dev.zed.Zed.desktop`), and
#  `2>/dev/null || true` on that install swallowed the failure. A binary on PATH
#  with no launcher entry is, to the person using the machine, an editor that
#  does not work.
#
#  Since 2026-10-04 Zed is not in the image at all ("make the apps optional"):
#  `rime install zed` installs its Flathub build, which ships and exports its
#  own entry, and updates with every Zed release instead of every core rebuild.
#  So the Zed half of this suite now holds that arrangement: no stage unpacks a
#  Zed tarball into /usr again (where the entry bug lived), and rime-pkg routes
#  the bare name to Flathub and says why.
#
#  Neovim still ships, and it turned out to be a second live defect of the same
#  shape. Its entry is correct and says `Terminal=true`, which is an instruction
#  to the DESKTOP: supply a terminal. freedesktop's mechanism for that is
#  `xdg-terminal-exec`, and no Rime machine had it — while
#  /etc/xdg/xdg-terminals.list, which is that program's config file and names
#  Alacritty.desktop, has shipped since rime-logs 31. The configuration was on
#  every install and the program that reads it was never packaged. So nvim ran
#  fine from a shell everywhere and could not be started from the desktop
#  anywhere, and `rime install neovim` told the user it was already "provided by
#  Rime OS" — which is true, and is rime-pkg refusing to shadow an image
#  package, and is not the bug.
#
#  The shell half of the repair (routing Terminal=true entries through the
#  helper instead of calling DesktopEntry.execute(), which does not honour the
#  field) is in rime-shell on task/terminal-entries-launchable, measured by its
#  tests/run-terminal-entry-test.sh. THIS file asserts the image's half: that
#  the helper is installed, that the config and the helper and $TERMINAL all
#  name the same terminal, and that the terminal they name is really there.
#
#  Nothing here launches an editor. A suite that opened a window on the
#  developer's session to prove a window opens is not run twice.
#
#  Run from anywhere: ./tests/test-rime-editors.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2
REPO=$(cd .. && pwd)
CF="$REPO/Containerfile.core"
CFB="$REPO/Containerfile.base"
PKG="$REPO/files/system/libexec/rime-pkg"

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip+1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }
want() { local d="$1"; shift; if "$@"; then ok "$d"; else bad "$d"; fi; }

# COMMENTS ARE STRIPPED, THEN backslash-continuations joined. The first version
# of this suite failed on a comment that quoted the wrong filename while
# explaining the bug, and a later one grepped one physical line for a pattern
# that was only ever on the next: an assertion that cannot fail is worse than
# none. Core's comments still explain where Zed went, and name it doing so.
CODE=$(mktemp); trap 'rm -f "$CODE"' EXIT
grep -v '^[[:space:]]*#' "$CF" | sed -e :a -e '/\\$/N; s/\\\n[[:space:]]*//; ta' > "$CODE"

section "Zed installs on demand, from the build that ships its own entry"

want "the comment strip kept the build's stages" \
    grep -qE '^FROM .* AS ' "$CODE"

if grep -qE 'zed\.dev|zed-linux|/usr/lib/zed\.app|/usr/bin/zed' "$CODE"; then
    bad "no stage unpacks a Zed tarball into the image"
else
    ok "no stage unpacks a Zed tarball into the image"
fi

# The bare name has to land on Flathub, not on the repository route: there is no
# Zed rpm in any repository Rime enables, so `rime install zed` with no route
# would end in "no package matches" — the "does not work" this suite is for.
zed_route=$(bash -c 'e=$1; set --; source "$e" >/dev/null 2>&1; curated_source zed' _ "$PKG" 2>/dev/null)
want "rime install zed is routed to Flathub" \
    test "${zed_route%%|*}" = flatpak
want "  ...and says why on the spot" \
    test -n "${zed_route#*|}"

if [ -d /usr/lib/zed.app ]; then
    skp "this machine still has the image's Zed (an image from before 2026-10-04)"
fi

section "neovim ships an entry, and a terminal to honour it"

if [ -s /usr/share/applications/nvim.desktop ]; then
    ok "a launcher entry for Neovim is installed"
    tryexec=$(sed -n 's/^TryExec=//p' /usr/share/applications/nvim.desktop | head -n1)
    if [ -n "$tryexec" ] && command -v "$tryexec" >/dev/null 2>&1; then
        ok "its TryExec ($tryexec) resolves on PATH"
    else
        bad "its TryExec (${tryexec:-none}) resolves on PATH"
    fi
    # Terminal=true is not a defect — it is a fact with a consequence, and the
    # consequence is that SOMETHING has to be there to host it.
    if grep -qx 'Terminal=true' /usr/share/applications/nvim.desktop; then
        ok "the entry declares Terminal=true, so it needs a terminal emulator"
        found=""
        for t in kitty alacritty foot ghostty wezterm xterm; do
            command -v "$t" >/dev/null 2>&1 && { found="$t"; break; }
        done
        if [ -n "$found" ]; then
            ok "the image ships a terminal emulator to host it ($found)"
        else
            bad "the image ships a terminal emulator to host it"
        fi
    else
        skp "the entry does not declare Terminal=true"
    fi
else
    skp "nvim.desktop is not on this machine"
fi

section "the image installs the program that makes Terminal=true mean something"

# Same comment-strip-then-join pipeline as the Zed check above, and for the
# same reason: the package name and the assertion that it arrived are on
# different physical lines of one `\`-continued RUN, so a per-line grep can be
# satisfied by text that is not in the command it claims to check.
DESKTOP=$(mktemp); TERMBLK=$(mktemp)
trap 'rm -f "$CODE" "$DESKTOP" "$TERMBLK"' EXIT
awk '/^RUN set -eux; \\$/{buf=""; f=1} f{buf=buf $0 "\n"} f&&/dnf5 clean all|^$/{if (buf ~ /alacritty/) {printf "%s", buf; exit} f=0}' "$CF" \
    | grep -v '^[[:space:]]*#' | sed -e :a -e '/\\$/N; s/\\\n[[:space:]]*//; ta' > "$DESKTOP"
# The Containerfile.base block that checks the three parts agree. It starts at
# the COPY of the list, so a `xdg-terminal-exec` mentioned anywhere else in the
# file cannot satisfy an assertion about this one.
awk '/^COPY files\/system\/xdg\/xdg-terminals.list/{f=1} f{print} f&&/agrees with TERMINAL/{exit}' "$CFB" \
    | grep -v '^[[:space:]]*#' | sed -e :a -e '/\\$/N; s/\\\n[[:space:]]*//; ta' > "$TERMBLK"

want "the desktop-package stanza was found in Containerfile.core" \
    test -s "$DESKTOP"

# The PACKAGE LIST, isolated, not the stanza. The first version of this
# assertion grepped the whole joined stanza for `xdg-terminal-exec` — and the
# stanza also contains `command -v xdg-terminal-exec` and a FATAL message
# naming it, so deleting the package from the install line left it passing.
# It could not fail against the one mutant it exists for. Everything between
# `dnf5 -y install` and the next `;`, split into words, matched whole.
# Split on `;` FIRST. The stanza is one joined line carrying two installs
# (`dnf5 -y install <the desktop set>` and, further down, `dnf5 -y install
# --skip-unavailable unrar`), and `.*dnf5 -y install ` is greedy — it matched
# the SECOND one and reported a two-package image. Every install in the stanza
# now contributes, and `-` flags are dropped rather than counted as packages.
pkgs=$(tr ';' '\n' < "$DESKTOP" \
        | sed -n 's/^[[:space:]]*dnf5 -y install //p' \
        | tr ' ' '\n' | grep -v '^-' | grep .)
have_pkg() { printf '%s\n' "$pkgs" | grep -qx "$1"; }

want "xdg-terminal-exec is in the package list, not merely mentioned nearby" \
    have_pkg xdg-terminal-exec

# The control on the assertion above: if the word-split ever stopped matching
# anything, `have_pkg` would report every package missing and the check would
# be dead in the same silent way. A package that has been in this list for
# years proves the extraction still works.
want "  ...and the extraction that proves it still finds a known package" \
    have_pkg alacritty

# Installing it and never checking it arrived is how the shipped config file
# ended up with no reader for months.
want "the build fails if xdg-terminal-exec did not arrive" \
    grep -q 'FATAL: xdg-terminal-exec is not executable' "$DESKTOP"

want "the agreement block was found in Containerfile.base" \
    test -s "$TERMBLK"

want "the build fails if the list ships with no program to read it" \
    grep -q 'FATAL: /etc/xdg/xdg-terminals.list ships but xdg-terminal-exec does not' "$TERMBLK"

want "the build fails if the list names an entry that is not installed" \
    grep -q 'FATAL: xdg-terminals.list names .* not installed' "$TERMBLK"

# The two settings are written in different files by different stages, and
# nothing but this check stops them drifting apart.
want "the build fails if the list and \$TERMINAL name different terminals" \
    grep -q 'FATAL: xdg-terminals.list opens .* but /etc/environment sets TERMINAL=' "$TERMBLK"

section "the terminal chain on this machine, where it is installed"

# Gated on being a Rime machine, not on the files being there. Gating on the
# files would turn "the image stopped shipping the list" into a silent SKIP,
# which is the same shape as the defect this section exists for: a missing
# piece that nothing complained about. On anything that is not Rime — a CI
# runner, a developer's Ubuntu box — none of it applies and the whole section
# skips.
if grep -q '^PRETTY_NAME="Rime OS"' /usr/lib/os-release 2>/dev/null; then
    if [ -s /etc/xdg/xdg-terminals.list ]; then
        ok "/etc/xdg/xdg-terminals.list is installed"
        xte_entry=$(grep -m1 -E -v '^[[:space:]]*([#/-]|$)' /etc/xdg/xdg-terminals.list || true)
        if [ -n "$xte_entry" ]; then
            ok "it names a terminal ($xte_entry)"
            if [ -s "/usr/share/applications/$xte_entry" ]; then
                ok "the entry it names is installed"
                xte_bin=$(sed -n 's/^TryExec=//p;s/^Exec=\([^ ]*\).*/\1/p' \
                    "/usr/share/applications/$xte_entry" | head -n1)
                env_bin=$(sed -n 's/^TERMINAL=//p' /etc/environment 2>/dev/null | head -n1)
                if [ -n "$env_bin" ] && [ "${xte_bin##*/}" = "${env_bin##*/}" ]; then
                    ok "it agrees with TERMINAL in /etc/environment (${env_bin##*/})"
                else
                    bad "it agrees with TERMINAL in /etc/environment (list says ${xte_bin:-none}, environment says ${env_bin:-none})"
                fi
            else
                bad "the entry it names is installed"
            fi
        else
            bad "it names a terminal"
        fi
    else
        bad "/etc/xdg/xdg-terminals.list is installed"
    fi

    # The live half of the defect itself. On a machine still running an image
    # built before this branch this FAILS, and the failure IS the report: it is
    # exactly what made clicking Neovim do nothing.
    if command -v xdg-terminal-exec >/dev/null 2>&1; then
        ok "xdg-terminal-exec is on PATH, so a Terminal=true entry has something to go through"
    else
        bad "xdg-terminal-exec is on PATH, so a Terminal=true entry has something to go through"
        echo "        This machine predates the fix: nvim.desktop says Terminal=true"
        echo "        and nothing installed here can honour it. Expected until the"
        echo "        branch reaches an image build; not a reason to hand-install it."
    fi
else
    skp "not a Rime image (the live terminal chain does not apply here)"
fi

printf '\nrime-editors: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
