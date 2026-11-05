#!/usr/bin/env bash
#
# Rime OS — labwc (Rime Floating) session capability matrix.
#
# The roadmap's labwc test matrix, for the part of it a machine can answer.
# Everything here runs a REAL client against a REAL nested labwc using the
# SHIPPED config, because the question is never "does labwc support this
# protocol" in the abstract — it is "does the session we ship actually let a
# client do this". A protocol that is advertised but unusable looks identical to
# one that works, right up until a user tries it.
#
# The application grid from the same section (Firefox, Chromium, Steam,
# gamescope, VS Code, JetBrains, LibreOffice, Blender, Discord, Wine, XWayland
# games) is NOT here and is not claimed. Those need a real GPU session and a
# human looking at the screen; they live in docs/labwc-verification.md as a
# manual checklist. Fabricating them would be worse than leaving them out.
#
# Skips rather than fails when labwc or a probe client is missing, so this is
# safe to run in CI where there is no compositor at all.
#
# Usage: tests/test-labwc-session.sh

set -uo pipefail
# `set +e` is deliberate and load-bearing. This suite COUNTS failures rather
# than aborting on them, and several assertions run commands that exit non-zero
# on purpose — a refusal, a guard firing, a bad argument. GitHub Actions invokes
# a script as `bash -e {0}`, and under `-e` a `x="$(cmd)"` assignment whose
# command exits non-zero terminates the whole script. That is exactly what
# happened: the suite passed locally, and on CI it died part-way through with
# the remaining assertions reported as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMPL="${ROOT}/files/desktop/labwc"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

pass=0
fail=0
skip=0

ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip + 1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }

# ── preconditions ────────────────────────────────────────────────────────────
section "environment"

if ! command -v labwc >/dev/null 2>&1; then
    skp "labwc is not installed; the whole session matrix is unrunnable here"
    printf '\nlabwc-session: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    exit 0
fi
ok "labwc is installed ($(labwc --version 2>&1 | head -1))"

# ── OPT-IN, because this suite opens windows on your desktop ────────────────
#
# It starts real nested labwc sessions with real clients, which is the only way
# to answer what it asks — and on a live desktop those are visible windows that
# take focus. No CI workflow runs this file (checked: it appears in no
# workflow), and it exits early without a parent display, so the ONLY machine it
# ever executes on is a developer's, in the middle of whatever they were doing.
#
# It did exactly that: a blanket `tests/*.sh` sweep launched nested compositors
# on the developer's active workspace and interrupted their work. So it now
# requires saying so on purpose. A sweep skips it; a person who wants the
# session matrix runs it deliberately.
if [ "${RIME_LABWC_SESSION_TESTS:-0}" != "1" ]; then
    skp "opt-in: this suite opens nested compositor windows on your desktop"
    printf '      run it deliberately with RIME_LABWC_SESSION_TESTS=1 %s\n' "$0"
    printf '\nlabwc-session: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    exit 0
fi

# A compositor needs somewhere to put its output. There are two answers, and
# only one of them existed until now.
#
# NESTED needs a parent display, and that is the developer's own desktop — real
# windows, taking real focus, which is what the opt-in above is for.
#
# HEADLESS needs nothing at all: wlroots renders to a buffer and there is no
# screen involved, which is what makes this suite runnable on a CI runner, and
# runnable safely on a machine somebody is using. It was never wired up, so the
# suite could only ever run the way that interrupts people.
HEADLESS=0
if [ "${WLR_BACKENDS:-}" = headless ]; then
    HEADLESS=1
    ok "the headless backend is selected, so nothing is drawn on anybody's screen"
elif [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    skp "no parent display and WLR_BACKENDS is not headless; labwc has nowhere to render"
    printf '\nlabwc-session: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    exit 0
else
    ok "a parent display is available to nest inside"
fi

# ── the config under test ────────────────────────────────────────────────────
# The shipped templates, rendered exactly as the provisioner renders them, so
# this tests what users get rather than a convenient variant of it.
CFG="${WORK}/.config"
mkdir -p "${CFG}/labwc"
cp "${TMPL}/rc.xml" "${TMPL}/menu.xml" "${CFG}/labwc/"
sed 's/@ACCENT@/#D9F99D/g' "${TMPL}/themerc-override" > "${CFG}/labwc/themerc-override"

# Run a probe script inside a nested labwc and return its output.
run_nested() {
    local script="$1" seconds="${2:-25}"
    chmod +x "$script"
    if [ "$HEADLESS" = 1 ]; then
        # WAYLAND_DISPLAY and DISPLAY are STRIPPED, not merely ignored. Setting
        # WLR_BACKENDS=headless while a parent display is still visible leaves
        # wlroots able to pick the wayland backend anyway, and the first thing
        # the operator would know about it is a window appearing on their
        # screen. With no parent in the environment there is nothing to nest
        # into, so "headless" is a property of the run rather than a promise.
        env -u WAYLAND_DISPLAY -u DISPLAY \
            XDG_CONFIG_HOME="${CFG}" WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
            timeout "$seconds" labwc --startup "$script" 2>/dev/null
    else
        XDG_CONFIG_HOME="${CFG}" timeout "$seconds" labwc --startup "$script" 2>/dev/null
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# ── capability probes ────────────────────────────────────────────────────────
section "session capabilities"

cat > "${WORK}/probe.sh" <<PROBE
#!/bin/bash
out="${WORK}/results"
: > "\$out"
say() { printf '%s=%s\n' "\$1" "\$2" >> "\$out"; }

# Wait for the compositor to actually have a configured output before probing.
# --startup fires as soon as labwc is up, which is BEFORE the first frame: a
# capture attempted then fails for lack of a rendered buffer, not for lack of
# protocol support. Without this the capture probe failed roughly one run in
# three, and a flaky check is worse than no check because it teaches people to
# ignore the result.
for _ in \$(seq 1 40); do
    wlr-randr 2>/dev/null | grep -q 'Enabled: yes' && break
    sleep 0.1
done

# wlr-output-management. The display-settings page is built on this, so if it
# does not answer here, monitor arrangement cannot work in this session.
if wlr-randr >/dev/null 2>&1; then say output_management ok; else say output_management fail; fi

# wlr-screencopy: screenshots and the screen recorder. Retried, for the same
# first-frame reason — the question is whether capture works at all, not
# whether it works on the first attempt after startup.
screencopy=fail
for _ in 1 2 3 4 5 6 7 8 9 10; do
    rm -f "${WORK}/shot.png"
    if grim "${WORK}/shot.png" >/dev/null 2>&1 && [ -s "${WORK}/shot.png" ]; then
        screencopy=ok
        break
    fi
    sleep 0.3
done
say screencopy "\$screencopy"

# Clipboard and primary selection are separate mechanisms; a compositor can
# serve one and not the other, and middle-click paste is the one people notice.
if echo rime-clipboard | wl-copy 2>/dev/null && [ "\$(wl-paste 2>/dev/null)" = rime-clipboard ]; then
    say clipboard ok
else
    say clipboard fail
fi
if echo rime-primary | wl-copy --primary 2>/dev/null && [ "\$(wl-paste --primary 2>/dev/null)" = rime-primary ]; then
    say primary ok
else
    say primary fail
fi

# The three probes below are long-running clients: they connect, bind their
# protocol, and then keep running until stopped. So the healthy outcome is
# \`timeout\` killing them (124), and a *quick* non-zero exit is the failure.
#
# Judged on EXIT STATUS, not on stderr text. Matching a list of
# unsupported-protocol phrases meant any other startup failure — no connection,
# bad environment, a permission problem — produced a green capability result,
# which is precisely the "green on nothing" this file is supposed to avoid.
# stderr is still captured, but only to explain a failure.
probe_client() {
    local key="\$1" seconds="\$2"; shift 2
    if ! command -v "\$1" >/dev/null 2>&1; then
        say "\$key" skip
        return
    fi
    local err="${WORK}/\${key}.err" rc=0
    timeout "\$seconds" "\$@" >/dev/null 2>"\$err" || rc=\$?
    case "\$rc" in
        # 124: still running when we stopped it, which is what success looks
        # like for a client that never exits on its own.
        124) say "\$key" ok ;;
        # 0: exited cleanly. swaylock -f daemonises and returns 0 on success.
        0)   say "\$key" ok ;;
        *)   say "\$key" "fail(rc=\$rc)" ;;
    esac
}

# wlr-layer-shell. Rime Shell is a layer-shell client, so this is the single
# protocol the whole desktop depends on; swaybg is the smallest client that
# exercises it.
probe_client layer_shell 3 swaybg -c '#112233'

# ext-session-lock: the lock screen path. -f daemonises after locking.
probe_client session_lock 3 swaylock -f -c 000000

# ext-idle-notify: idle handling without a compositor-specific daemon.
probe_client idle 3 swayidle -w timeout 1 true
PROBE

run_nested "${WORK}/probe.sh" 40 >/dev/null 2>&1

report() {
    local key="$1" label="$2"
    local v
    v="$(sed -n "s/^${key}=//p" "${WORK}/results" 2>/dev/null | head -1)"
    case "${v:-missing}" in
        ok)     ok "$label" ;;
        skip)   skp "$label (probe client not installed)" ;;
        fail*)  bad "$label [$v]" ;;
        *)      bad "$label (probe produced no result)" ;;
    esac
}

if [ -s "${WORK}/results" ]; then
    report output_management "wlr-output-management answers (display settings depend on it)"
    report screencopy        "screencopy works (screenshots, recording)"
    report clipboard         "clipboard round-trips"
    report primary           "primary selection round-trips"
    report layer_shell       "layer-shell works (Rime Shell depends on it)"
    report session_lock      "ext-session-lock works (lock screen)"
    report idle              "ext-idle-notify works"
else
    bad "the nested session produced no probe results at all"
fi

# ── config reload and recovery ───────────────────────────────────────────────
# The matrix asks for both. Recovery is the one that matters: labwc falls back
# to defaults SILENTLY on a broken config, which means a bad edit costs the user
# their keybinds and their shell with no error anywhere.
section "config reload and invalid-config recovery"

cat > "${WORK}/reload.sh" <<RELOAD
#!/bin/bash
out="${WORK}/reload-results"
: > "\$out"

# A valid reconfigure must be accepted.
if labwc --reconfigure >/dev/null 2>&1; then echo "reload=ok" >> "\$out"; else echo "reload=fail" >> "\$out"; fi
sleep 1

# Now corrupt the config and reconfigure again. The compositor must SURVIVE:
# an invalid file may not take the session down with it.
printf '<?xml version="1.0"?>\n<labwc_config><core><gap>8</gap>\n' > "${CFG}/labwc/rc.xml"
labwc --reconfigure >/dev/null 2>&1
sleep 1

# Still alive? Ask it to do something that needs a live compositor.
if wlr-randr >/dev/null 2>&1; then echo "survives_bad_config=ok" >> "\$out"; else echo "survives_bad_config=fail" >> "\$out"; fi
RELOAD

run_nested "${WORK}/reload.sh" 30 >/dev/null 2>&1

if [ -s "${WORK}/reload-results" ]; then
    grep -q '^reload=ok' "${WORK}/reload-results" \
        && ok "labwc --reconfigure is accepted" || bad "labwc --reconfigure is accepted"
    grep -q '^survives_bad_config=ok' "${WORK}/reload-results" \
        && ok "the session survives an invalid rc.xml" || bad "the session survives an invalid rc.xml"
else
    bad "the reload probe produced no results"
fi

# ── portal configuration ─────────────────────────────────────────────────────
# Static, but it belongs with the session matrix: screen sharing is the most
# common thing to be quietly broken, and the failure is invisible until someone
# joins a call.
section "portal backends"

PORTAL="${ROOT}/files/system/xdg-desktop-portal/labwc-portals.conf"
if [ ! -f "$PORTAL" ]; then
    bad "a labwc portal config is shipped"
else
    ok "a labwc portal config is shipped"
    grep -qE '^default=gtk$' "$PORTAL" \
        && ok "desktop interfaces are pinned to gtk" || bad "desktop interfaces are pinned to gtk"
    grep -qE '^org\.freedesktop\.impl\.portal\.ScreenCast=wlr$' "$PORTAL" \
        && ok "ScreenCast is pinned to the wlroots backend" || bad "ScreenCast is pinned to the wlroots backend"
    grep -qE '^org\.freedesktop\.impl\.portal\.Screenshot=wlr$' "$PORTAL" \
        && ok "Screenshot is pinned to the wlroots backend" || bad "Screenshot is pinned to the wlroots backend"
    # The packaged labwc config uses `default=wlr;*`, which leaves FileChooser
    # to whichever backend resolves first. Reintroducing that is the regression
    # this guards.
    grep -qE '^\s*default=.*\*' "$PORTAL" \
        && bad "no wildcard backend remains" || ok "no wildcard backend remains"

    if [ -d /usr/share/xdg-desktop-portal/portals ]; then
        [ -f /usr/share/xdg-desktop-portal/portals/wlr.portal ] \
            && ok "the wlroots portal backend is installed" || bad "the wlroots portal backend is installed"
        [ -f /usr/share/xdg-desktop-portal/portals/gtk.portal ] \
            && ok "the gtk portal backend is installed" || bad "the gtk portal backend is installed"
    else
        skp "no portal backends installed here; cannot check the config names real ones"
    fi
fi

# ── what this cannot answer ──────────────────────────────────────────────────
section "not covered here"
cat <<'NOTE'
      Gamma control (night light) cannot be probed nested: a nested backend has
      no gamma-capable output, so gammastep reports "Zero outputs support gamma
      adjustment" regardless of whether labwc implements the protocol. Needs a
      real session.

      Multi-monitor hotplug, scale, refresh and rotation need real outputs.

      Fullscreen/maximise/minimise behaviour, workspace switching and window
      focus need real windows and a human.

      The application grid (Firefox, Chromium, Steam, gamescope, VS Code,
      JetBrains, LibreOffice, Blender, Discord, Qt/GTK/Electron, Wine, XWayland
      games) is a manual checklist in docs/labwc-verification.md.
NOTE

printf '\nlabwc-session: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
