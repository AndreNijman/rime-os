#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-plymouth-keyboard.sh — the plymouthd crash fix stays in the image.
#
#  Fedora's plymouthd crashed (SIGSEGV, plymouth-start.service "failed
#  (core-dump)") when the splash was hidden on a machine whose initramfs drew
#  on simpledrm and whose real display driver came up later without a usable
#  display: free_keyboards_for_renderer() freed the simpledrm keyboard without
#  telling the daemon, and the script plugin (Rime's theme) still held it.
#  files/system/src/plymouth/ carries the fix; Containerfile.core rebuilds
#  Fedora's plymouth with it. The proof that it works is a VM boot (README.md
#  beside the patch); this file keeps the pieces from being lost:
#    • the patch tells the daemon BEFORE the keyboard is freed, the splash
#      forgets the keyboard it unset, and the daemon hands it another one;
#    • the core build fetches the pinned SRPM, adds the patch, checks the
#      prepared source has it, and installs ONLY those local RPMs;
#    • both images refuse a plymouth that is not Rime's build (the rime
#      tier's initramfs is what actually runs plymouthd);
#    • the core image rebuilds when the patch changes.
#  Static: no network, no build, no root. Run from anywhere.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")/.." || exit 2

PATCH=files/system/src/plymouth/plymouth-keyboard-removed-handler.patch
CORE=Containerfile.core
RIME=Containerfile.rime
WF=.github/workflows/build-image.yml
for f in "$PATCH" "$CORE" "$RIME" "$WF"; do [ -f "$f" ] || { echo "cannot find $f"; exit 2; }; done

pass=0; fail=0
check() { # name, then a command that must succeed
    local name="$1"; shift
    if "$@"; then printf 'PASS  %s\n' "$name"; pass=$((pass+1)); else printf 'FAIL  %s\n' "$name"; fail=$((fail+1)); fi
}
has() { grep "$@" >/dev/null; }

# ── the patch ────────────────────────────────────────────────────────────────
# The added lines of one file's diff, in order.
added() { awk -v f="$1" '/^diff --git /{on = index($0, " b/" f) > 0; next} on && /^\+[^+]/{print substr($0, 2)}' "$PATCH"; }
# The handler call must be an ADDED line in ply-device-manager.c, inside
# free_keyboards_for_renderer, and come before that function's ply_keyboard_free.
dm_order() {
    awk '/^diff --git /{on = index($0, "ply-device-manager.c") > 0; next}
         on && /^@@ .*free_keyboards_for_renderer/ {inf = 1}
         on && inf && /^\+.*keyboard_removed_handler \(manager->event_handler_data, keyboard\)/ {h = NR}
         on && inf && /^ .*ply_keyboard_free \(keyboard\)/ && h {ok = 1}
         END {exit !ok}' "$PATCH"
}
check "patch: free_keyboards_for_renderer tells the daemon before freeing" dm_order
check "patch: the splash forgets the keyboard it unset" \
    has -F 'splash->keyboard = NULL;' <(added src/libply-splash-core/ply-boot-splash.c)
check "patch: the daemon hands the splash a remaining keyboard" \
    has -F 'ply_boot_splash_set_keyboard (state->boot_splash, other);' <(added src/main.c)
check "patch: never hands back the keyboard being removed" \
    has -F 'if (other != keyboard) {' <(added src/main.c)
check "patch: no AI attribution" bash -c '! grep -iE "co-authored-by|claude|generated with" "$0" >/dev/null' "$PATCH"

# ── the core build ───────────────────────────────────────────────────────────
stage() { awk '/^FROM .* AS plymouth-builder$/{on=1} on && /^FROM / && !/plymouth-builder/{exit} on' "$CORE"; }
check "core: plymouth-builder stage exists" has -E '^FROM quay.io/fedora/fedora-bootc:45 AS plymouth-builder$' "$CORE"
check "core: the SRPM is pinned by sha256" has -E 'ARG PLYMOUTH_SRPM_SHA256=[0-9a-f]{64}$' <(stage)
check "core: the SRPM hash is checked" has -E '"\$\{PLYMOUTH_SRPM_SHA256\}  /build/\$\{PLYMOUTH_NVR\}\.src\.rpm" \| sha256sum -c -' <(stage)
check "core: the patch is added to the spec" has -F 'a Patch: plymouth-keyboard-removed-handler.patch' <(stage)
check "core: the prepared source is checked for the fix" \
    has -F 'keyboard_removed_handler (manager->event_handler_data, keyboard)' <(stage)
check "core: the release is marked .rime" has -F '%autorelease -e rime1' <(stage)
# The install: local RPMs only, then every package checked.
install_run() { awk '/^COPY --from=plymouth-builder /{on=1} on{print} on && !/\\$/ && !/^COPY/{exit}' "$CORE"; }
check "core: installs the plymouth-builder RPMs" has -F 'dnf5 -y --nogpgcheck install /tmp/rime-plymouth/*.rpm' <(install_run)
check "core: refuses a plymouth that is not Rime's build" has -F 'is not Rime'"'"'s plymouth build' <(install_run)
check "core: no plymouth from the repos" bash -c '! grep -vE "^[[:space:]]*#" "$0" | grep -E "dnf5? .*install[^/]* plymouth( |$)" >/dev/null' "$CORE"

# ── the initramfs (rime tier) ────────────────────────────────────────────────
check "rime: the initramfs refuses a plymouth that is not Rime's build" \
    has -F 'FATAL: plymouth is not Rime'"'"'s build' "$RIME"

# ── rebuild on change ────────────────────────────────────────────────────────
check "build-image: core path filter covers the patch" has -F -e "- 'files/system/src/plymouth/**'" "$WF"
check "build-image: CORE_PATHS covers the patch" has -E 'CORE_PATHS=.* files/system/src/plymouth/' "$WF"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
