#!/usr/bin/env bash
#
# Rime OS — a rebound key actually fires after the config is reloaded.
#
# ── The bug this exists for ──────────────────────────────────────────────────
# Quoted at the top of tests/test-labwc-keybinds.sh:
#
#     rebind the launcher, watch the UI confirm it, press the key, nothing
#     happens
#
# Until this file, nothing in either repo had ever PRESSED A KEY. The split was
# exact and left the interesting half uncovered:
#
#   test-labwc-keybinds.sh   generates and splices the XML, and passes
#                            `--no-reload` to every `apply` on purpose so it can
#                            never touch a live session. It never starts labwc.
#   test-labwc-session.sh    starts a real labwc and calls `--reconfigure`, and
#                            asserts the session SURVIVES a broken rc.xml. It
#                            never presses a key.
#
# So "the XML is correct" and "the compositor is still alive" were both proven,
# and "the key the user just bound now does something" was proven by neither.
# That is the whole of the reported bug.
#
# ── Why this is a new file and not a section in either of those ──────────────
# test-labwc-session.sh nests inside the PARENT display and says so: it is
# gated behind RIME_LABWC_SESSION_TESTS=1 because it opens visible windows on
# the developer's desktop, and it is in no workflow, so anything added there
# runs on no machine unless a human asks for it.
#
# This suite instead runs under WLR_BACKENDS=headless in a private
# XDG_RUNTIME_DIR with WAYLAND_DISPLAY and DISPLAY unset, so it has no parent
# display to open a window on and needs no opt-in. It is safe in a blanket
# `tests/*.sh` sweep and safe in CI.
#
# THIS MATTERS MORE THAN USUAL HERE, because this suite synthesises keystrokes.
# A keypress sent to the wrong display is typed into whatever the developer had
# focused. The isolation is therefore ASSERTED, as three hard failures, before
# the first key is pressed — never skipped past.
#
# `labwc --reconfigure` sends SIGHUP to `$LABWC_PID` (labwc(1)), which is
# inherited. An inherited LABWC_PID would reload the developer's own session,
# so it is unset on entry and then set explicitly to the pid this file started.
#
# ── What it proves ───────────────────────────────────────────────────────────
#   A  the shipped rc.xml's generated binding fires at all         (control)
#   B  an unbound key fires nothing, waited exactly as long        (negative)
#   C  a rebind written with --no-reload does NOT take effect —    (the bug,
#      the new key is dead and the old one still works              reproduced)
#   D  after --reconfigure the new key fires and the old one is dead
#   E  the shipped tool's OWN reload (no --no-reload) does the same
#
# C is the load-bearing one. Without it, D would pass on a tree where the
# reload does nothing and the binding was live from the start, which is exactly
# the failure mode being tested for.
#
# Requires labwc, wtype and python3; SKIPs cleanly without any of them.
#
# Usage: tests/test-labwc-keybind-reload.sh

set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` for the same reason as the other two labwc suites: CI invokes a
# script as `bash -e {0}`, under which an assignment from a command that exits
# non-zero kills the run part-way and reports the remaining assertions as
# failures. This suite counts failures instead of aborting on them.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="${ROOT}/files/system/libexec/rime-labwc-keybinds"
RC_TMPL="${ROOT}/files/desktop/labwc/rc.xml"

pass=0
fail=0
skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip + 1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }

finish() {
    printf '\nlabwc-keybind-reload: %d passed, %d failed, %d skipped\n' \
        "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

# ── what this needs ──────────────────────────────────────────────────────────
section "environment"

# The shell's keybind model lives in rime-shell, which is a SEPARATE REPO. The
# tool reads it with --shell-dir. Without a checkout beside this one there is
# no model to generate from, and inventing one would test this file's idea of a
# keybind rather than the product's.
SHELL_DIR="${RIME_SHELL_DIR:-}"
if [ -z "$SHELL_DIR" ]; then
    for cand in "${ROOT}/../wt-base-shell" "${ROOT}/../rime-shell" \
                "${ROOT}/../../rime-shell" "${HOME}/Projects/rime-shell"; do
        [ -f "${cand}/src/services/config_tab/KeybindService.qml" ] && {
            SHELL_DIR="$(cd "$cand" && pwd)"; break; }
    done
fi

missing=""
for t in labwc wtype python3; do
    command -v "$t" >/dev/null 2>&1 || missing="${missing} $t"
done
if [ -n "$missing" ]; then
    skp "not installed:${missing}; a rebound key cannot be pressed here"
    finish; exit 0
fi
if [ -z "$SHELL_DIR" ]; then
    skp "no rime-shell checkout found; set RIME_SHELL_DIR to the shell repo"
    finish; exit 0
fi
ok "labwc, wtype and python3 are all present"
ok "the shell's keybind model is readable ($(basename "$SHELL_DIR"))"

# ── isolation, established before anything is pressed ────────────────────────
section "isolation"

# Saved BEFORE they are unset, so the assertions below can compare against the
# real ambient values rather than against emptiness.
AMBIENT_WL="${WAYLAND_DISPLAY:-}"
AMBIENT_RT="${XDG_RUNTIME_DIR:-}"

# Inherited from the developer's own session if they run labwc. `--reconfigure`
# SIGHUPs whatever this names, so it is cleared before it can be used by
# accident and set explicitly further down.
unset LABWC_PID
unset WAYLAND_DISPLAY
unset DISPLAY

WORK="$(mktemp -d)"
COMP_PID=""
cleanup() {
    # By pid, always. `pkill labwc` would kill the developer's session.
    [ -n "$COMP_PID" ] && kill "$COMP_PID" 2>/dev/null
    [ -n "$COMP_PID" ] && wait "$COMP_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

export XDG_RUNTIME_DIR="${WORK}/rt"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export XDG_CONFIG_HOME="${WORK}/cfg"
export HOME="${WORK}/home"
mkdir -p "${XDG_CONFIG_HOME}/labwc" "$HOME" "${WORK}/bin"

# The generated keybind commands are PATH-resolved program names — the shipped
# model binds SUPER+T to the terminal, which generates `alacritty`. Stubbing it
# is what makes a keypress OBSERVABLE: the real question is not whether labwc
# accepted the XML, it is whether pressing the key runs the command. The stub
# records every invocation; the assertions read that log.
FIRED="${WORK}/fired.log"
cat > "${WORK}/bin/alacritty" <<STUB
#!/bin/sh
echo "fired" >> "${FIRED}"
STUB
chmod +x "${WORK}/bin/alacritty"
export PATH="${WORK}/bin:${PATH}"

cp "$RC_TMPL" "${XDG_CONFIG_HOME}/labwc/rc.xml"
RC="${XDG_CONFIG_HOME}/labwc/rc.xml"

export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_HEADLESS_OUTPUTS=1
labwc > "${WORK}/comp.log" 2>&1 &
COMP_PID=$!

sock=""
for _ in $(seq 1 80); do
    # A glob and a socket test, not `ls | grep`: the lock file sitting beside
    # the socket is named wayland-N.lock and matches the same pattern, so the
    # -S test is doing real work here rather than satisfying shellcheck.
    for cand in "$XDG_RUNTIME_DIR"/wayland-[0-9]*; do
        [ -S "$cand" ] && { sock="${cand##*/}"; break; }
    done
    [ -n "$sock" ] && break
    sleep 0.2
done
if [ -z "$sock" ]; then
    skp "labwc did not come up headless here"
    tail -5 "${WORK}/comp.log" 2>/dev/null | sed 's/^/      /'
    finish; exit 0
fi
export WAYLAND_DISPLAY="$sock"

# Three hard failures, not skips. Each one is a way a synthesised keystroke
# could land on the developer's desktop instead of in this compositor.
[ -S "${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}" ] \
    && ok "the wayland socket is inside this suite's own runtime dir" \
    || bad "the wayland socket is inside this suite's own runtime dir"
if [ -n "$AMBIENT_WL" ] && [ "$WAYLAND_DISPLAY" = "$AMBIENT_WL" ]; then
    bad "this display is not the ambient one [$AMBIENT_WL]"
else
    ok "this display ($WAYLAND_DISPLAY) is not the ambient one (${AMBIENT_WL:-none})"
fi
if [ -n "$AMBIENT_RT" ] && [ "$XDG_RUNTIME_DIR" = "$AMBIENT_RT" ]; then
    bad "this runtime dir is not the ambient one [$AMBIENT_RT]"
else
    ok "this runtime dir is not the ambient one"
fi

# Set only now, and only to the pid started above. Everything after this point
# may call --reconfigure.
export LABWC_PID="$COMP_PID"
[ "$LABWC_PID" = "$COMP_PID" ] && kill -0 "$COMP_PID" 2>/dev/null \
    && ok "--reconfigure is aimed at this suite's labwc (pid $COMP_PID)" \
    || bad "--reconfigure is aimed at this suite's labwc"

if [ "$fail" -ne 0 ]; then
    bad "isolation is not established; refusing to synthesise any keystroke"
    finish; exit 1
fi

# The first-frame race the neighbouring suite documents: --startup and the
# socket both appear before the output is configured, and a key pressed then is
# delivered to a seat with no focus. A flaky control is worse than no control.
for _ in $(seq 1 40); do
    wlr-randr 2>/dev/null | pipe_has 'Enabled: yes' && break
    sleep 0.1
done

# ── pressing keys ────────────────────────────────────────────────────────────
section "a keybind fires"

press() { wtype -M logo -k "$1" -m logo 2>/dev/null; }

# Bounded wait for the stub to record a run. Returns 0 as soon as it does.
fired_within() {
    local tries="$1" _
    for _ in $(seq 1 "$tries"); do
        [ -s "$FIRED" ] && return 0
        sleep 0.2
    done
    [ -s "$FIRED" ]
}

# Press repeatedly while waiting. --reconfigure is asynchronous, so a single
# press can land in the window between the SIGHUP and the new bindings being
# installed. The stub only appends, so a repeat is harmless.
press_until_fired() {
    local key="$1" tries="$2" _
    for _ in $(seq 1 "$tries"); do
        press "$key"
        sleep 0.3
        [ -s "$FIRED" ] && return 0
    done
    [ -s "$FIRED" ]
}

arm() { : > "$FIRED"; }

# A — the control. If the shipped binding does not fire, every assertion below
# is vacuous and the suite has to say so rather than keep counting.
arm
press t
if fired_within 25; then
    ok "the shipped rc.xml's SUPER+T binding runs its command"
else
    bad "the shipped rc.xml's SUPER+T binding runs its command"
    bad "wtype does not reach this compositor's seat; everything below would be vacuous"
    tail -5 "${WORK}/comp.log" 2>/dev/null | sed 's/^/      /'
    finish; exit 1
fi

# B — the negative control, waited EXACTLY as long as the positive. A short
# "it did not appear" against a long "it appeared" is not a control, it is a
# race that happens to resolve the way the author hoped.
arm
press F10
if fired_within 25; then
    bad "an unbound SUPER+F10 runs nothing"
else
    ok "an unbound SUPER+F10 runs nothing (waited as long as the control)"
fi

# ── the rebind ───────────────────────────────────────────────────────────────
section "a rebind, without and then with a reload"

echo '{"app-terminal": {"mods":"SUPER","key":"F10"}}' > "${WORK}/overrides.json"

apply() {
    python3 "$TOOL" apply \
        --rc "$RC" \
        --overrides "${WORK}/overrides.json" \
        --shell-dir "$SHELL_DIR" \
        --shell-path "$SHELL_DIR" \
        "$@" >"${WORK}/apply.out" 2>&1
}

# C — the bug itself. The file is rewritten and the session is NOT told, which
# is precisely "watch the UI confirm it, press the key, nothing happens".
if apply --no-reload; then
    ok "the shipped tool rewrites the binding to SUPER+F10"
else
    bad "the shipped tool rewrites the binding to SUPER+F10"
    sed 's/^/      /' "${WORK}/apply.out"
fi
grep -q '<keybind key="W-F10">' "$RC" \
    && ok "rc.xml on disk now carries W-F10" \
    || bad "rc.xml on disk now carries W-F10"

arm
press F10
if fired_within 25; then
    bad "a rebind written with --no-reload has NOT taken effect yet"
else
    ok "a rebind written with --no-reload has NOT taken effect yet"
fi
arm
press t
if fired_within 25; then
    ok "…and the OLD key still works, because the session never reloaded"
else
    bad "…and the OLD key still works, because the session never reloaded"
fi

# D — the criterion. Same file, same binding; the only thing that changed is
# that the compositor was told.
section "after labwc --reconfigure"

labwc --reconfigure >/dev/null 2>&1 \
    && ok "labwc --reconfigure is accepted" \
    || bad "labwc --reconfigure is accepted"

arm
if press_until_fired F10 20; then
    ok "THE REBOUND KEY FIRES AFTER --reconfigure"
else
    bad "THE REBOUND KEY FIRES AFTER --reconfigure"
fi

arm
press t
if fired_within 25; then
    bad "the key it was rebound FROM no longer fires"
else
    ok "the key it was rebound FROM no longer fires"
fi

# E — the same thing again through the tool's own reload, which is the path a
# user's rebind actually takes. `apply` without --no-reload calls
# `labwc --reconfigure` itself (reload_labwc), so this covers the call site
# rather than only the compositor's half.
section "the shipped tool's own reload"

echo '{"app-terminal": {"mods":"SUPER","key":"F9"}}' > "${WORK}/overrides.json"
if apply; then
    ok "the shipped tool applies a rebind and reloads without being asked twice"
else
    bad "the shipped tool applies a rebind and reloads without being asked twice"
    sed 's/^/      /' "${WORK}/apply.out"
fi

arm
if press_until_fired F9 20; then
    ok "THE REBOUND KEY FIRES AFTER THE TOOL'S OWN RELOAD"
else
    bad "THE REBOUND KEY FIRES AFTER THE TOOL'S OWN RELOAD"
fi

arm
press F10
if fired_within 25; then
    bad "the previous binding is gone after the second rebind"
else
    ok "the previous binding is gone after the second rebind"
fi

# ── what this cannot answer ──────────────────────────────────────────────────
section "not covered here"
cat <<'NOTE'
      This presses keys into a headless seat. It proves the binding is live and
      the command is spawned; it does not prove what the command then draws,
      because a headless output has nothing to look at.

      The Settings UI half of the reported bug — that the panel shows the new
      key after the rebind — is a shell concern and is covered there. This
      file starts at the written rc.xml and ends at the spawned process.
NOTE

finish
