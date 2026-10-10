#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-ai-apps.sh — the desktop AI apps install on demand, verified, and
#  bring no update channel of their own.
#
#  ── What this exists for ────────────────────────────────────────────────────
#  From 2026-09-11 to 2026-10-04 ChatGPT, Claude Desktop and Claude Code shipped
#  IN the image (Containerfile.core stages 5a-aiapps and 5a-claude). Andre
#  reversed that on 2026-10-04 — "make the apps optional", and the install as
#  small as possible — so they are `rime install` targets now:
#
#    chatgpt         OpenAI's rpm repo, verified against a pinned key (rime-pkg)
#    claude-desktop  Anthropic's apt repo, verified against a pinned key (rime-pkg)
#    claude-code     npm, per user, into ~/.local, where it updates itself
#
#  Four things can go wrong with that, and this holds each one:
#
#    1. An app creeps back into the image. That is ~2.3 GB on every machine
#       and every core rebuild, so its absence is asserted on the Containerfile
#       (code, not comments: a comment explaining why it left must not count).
#    2. A vendor's update channel gets in through the new route. Both vendors
#       package for mutable distributions — OpenAI's rpm ships an ENABLED
#       /etc/yum.repos.d/chatgpt.repo, Anthropic's deb writes an apt source from
#       its maintainer script — and rime-pkg builds extensions with dnf against
#       the host's repos. So rime-pkg must never write a repo file, never run a
#       maintainer script, and never import a vendor key into a keyring that
#       dnf or rpm would consult afterwards.
#    3. The pins drift. The fingerprints rime-pkg checks are the SAME ones the
#       image build pinned; a changed constant is a changed trust decision and
#       has to be a visible one.
#    4. Claude Code is installed where it cannot update itself, or its updater
#       is still switched off fleet-wide (DISABLE_AUTOUPDATER=1 belonged to the
#       image copy, which could not write /usr).
#
#  Everything here is structural and runs anywhere; tests/test-rime-pkg.sh holds
#  the routes' behaviour, including fixtures signed with the wrong key.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")" || exit 1

REPO=$(cd .. && pwd)
CF="$REPO/Containerfile.core"
PKG="$REPO/files/system/libexec/rime-pkg"

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }
want() { local d="$1"; shift; if "$@"; then ok "$d"; else bad "$d"; fi; }
wont() { local d="$1"; shift; if "$@"; then bad "$d"; else ok "$d"; fi; }

# Code only: comments stripped, backslash-continuations joined. The Containerfile
# explains, in comments, why each app left — and names it while doing so.
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT
strip_code() { grep -v '^[[:space:]]*#' "$1" | sed -e :a -e '/\\$/N; s/\\\n[[:space:]]*//; ta'; }
CODE="$SCRATCH/core"; PKGCODE="$SCRATCH/pkg"
strip_code "$CF" > "$CODE"
strip_code "$PKG" > "$PKGCODE"
in_code() { grep -qE "$1" "$CODE"; }
in_pkg()  { grep -qE "$1" "$PKGCODE"; }

# A check that can never fail proves nothing. Each `wont` below is also run
# against a copy of the real file with the old line put back (the shapes are
# the ones 5a-aiapps and 5a-claude had, continuations included) and must see it.
sees_mutant() {  # FILE PATTERN LINE...
    local file="$1" pat="$2"; shift 2
    cp "$file" "$SCRATCH/mutant"; printf '%s\n' "$@" >> "$SCRATCH/mutant"
    strip_code "$SCRATCH/mutant" | pipe_has -E "$pat"
}

section "the apps are not in the image"

# Stripping must leave the build itself: every stage, and no comment line.
want "the comment strip kept the build's stages"        in_code '^FROM .* AS '
wont "  ...and left no comment behind"                  in_code '^[[:space:]]*#'

P_CHATGPT='chatgpt\.rpm|dnf5 -y install [^;]*chatgpt'
P_DESKTOP='/usr/lib/claude-desktop|claude-desktop\.deb'
P_CODE='@anthropic-ai/claude-code'
P_REPOS='persistent\.oaistatic\.com|downloads\.claude\.ai'
wont "no stage installs the chatgpt rpm"                in_code "$P_CHATGPT"
want "  ...and the check sees one put back"             sees_mutant "$CF" "$P_CHATGPT" \
    'RUN set -eu; \' '    dnf5 -y install --setopt=install_weak_deps=False /tmp/chatgpt.rpm'
wont "no stage unpacks Claude Desktop"                  in_code "$P_DESKTOP"
want "  ...and the check sees one put back"             sees_mutant "$CF" "$P_DESKTOP" \
    'RUN ar x /tmp/claude-desktop.deb && \' '    cp -a usr/lib/claude-desktop /usr/lib/'
wont "no stage installs Claude Code"                    in_code "$P_CODE"
want "  ...and the check sees one put back"             sees_mutant "$CF" "$P_CODE" \
    'RUN npm install -g \' '      --prefix /usr \' '      @anthropic-ai/claude-code'
wont "no stage fetches the vendors' repositories"       in_code "$P_REPOS"
want "  ...and the check sees one put back"             sees_mutant "$CF" "$P_REPOS" \
    'RUN curl -fsSL https://downloads.claude.ai/apt/stable/InRelease -o /tmp/InRelease'

section "Claude Code updates itself now"

wont "DISABLE_AUTOUPDATER is not set fleet-wide"        in_code 'DISABLE_AUTOUPDATER'
want "  ...and the check sees it put back"              sees_mutant "$CF" 'DISABLE_AUTOUPDATER' \
    "RUN printf 'DISABLE_AUTOUPDATER=1\\n' >> /etc/environment"
want "Electron apps still get the Wayland hint"         in_code 'ELECTRON_OZONE_PLATFORM_HINT=auto'
# The per-user route lands in ~/.local only because core points npm's global
# prefix there. Without it `npm install -g` as a user dies EACCES on
# /usr/local, and the route is dead.
want "npm's global prefix is the user's own ~/.local"    in_code "sed -i 's\\|\\^prefix=/usr/local\\$\\|prefix=\\\$\\{HOME\\}/\\.local\\|'"
want "rime-pkg installs claude-code as the user, with npm" in_pkg 'npm "\$1" -g "\$CLAUDE_CODE_NPM"'
want "  ...and never with --prefix /usr (where it could not update)" bash -c "! grep -qE 'npm .*--prefix /usr' '$PKGCODE'"

section "the vendor routes bring no update channel"

P_REPOFILE='/etc/yum\.repos\.d|sources\.list'
P_SCRIPT='dpkg (-i|--install)|(sh|bash|\.) +"?\$[a-z_]*/?[a-z_/]*postinst'
P_IMPORT='rpm(keys)? +--import'
wont "rime-pkg writes no repo file"                     in_pkg "$P_REPOFILE"
want "  ...and the check sees one written"              sees_mutant "$PKG" "$P_REPOFILE" \
    '    printf "[chatgpt]\\nbaseurl=%s\\n" "$repo" > /etc/yum.repos.d/chatgpt.repo'
wont "rime-pkg runs no maintainer script"               in_pkg "$P_SCRIPT"
want "  ...and the check sees one run"                  sees_mutant "$PKG" "$P_SCRIPT" \
    '    sh "$dir/ctl/postinst" configure'
# The ChatGPT check is `rpmkeys --define _keyring fs --import` into a one-file
# keyring under the work dir; any OTHER import lands in the system rpmdb.
imports_to_system() { grep -E "$P_IMPORT" "$1" | pipe_has -v "_keyring fs"; }
wont "rime-pkg imports no vendor key into the system rpmdb" imports_to_system "$PKGCODE"
want "  ...and the check sees one imported"             bash -c '
    cp "$1" "$2/mutant"; printf "%s\n" "    rpm --import \"\$dir/key.asc\"" >> "$2/mutant"
    grep -v "^[[:space:]]*#" "$2/mutant" | grep -E "$3" | grep -qv "_keyring fs"' _ "$PKG" "$SCRATCH" "$P_IMPORT"
want "ChatGPT is verified against a one-file keyring"   in_pkg "rpmkeys --define '_keyring fs'"
want "Claude Desktop's InRelease is checked with gpgv"   in_pkg 'gpgv --keyring "\$dir/key\.gpg" "\$dir/InRelease"'

section "the pins are the ones the image build used"

# These are the constants 5a-aiapps carried until 2026-10-04. Changing either is
# a change of whom Rime trusts to sign that app, and belongs in its own commit.
want "the ChatGPT key is pinned at 3BFA0E4A…6C4660E4"    in_pkg '^readonly CHATGPT_KEY_FPR=3BFA0E4AE8B8CC16A2D9BA684A3B4A566C4660E4$'
want "the Claude key is pinned at 31DDDE24…1A7ECACE"   in_pkg '^readonly CLAUDE_KEY_FPR=31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE$'
wont "neither fingerprint can come from the environment" in_pkg 'KEY_FPR="?\$\{'

printf '\nai-apps: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
