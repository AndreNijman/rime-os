#!/usr/bin/env bash
#
# rime-compositor-reload and rime-session-logout: which compositor they decide
# they are in, and the command they send it.
#
# Two defects this pins, both found by the NixOS hypr-lua VM check:
#
#   * On NixOS bin/Hyprland is a wrapper that execs .Hyprland-wrapped, so the
#     running process is ".Hyprland-wrapp" to `pgrep -x` (15-character comm)
#     and `pgrep -x Hyprland` never matches. A caller without
#     XDG_CURRENT_DESKTOP (rime-input-apply started from a plain shell) got
#     "no known compositor is running" and nothing was reloaded.
#     HYPRLAND_INSTANCE_SIGNATURE, which Hyprland sets in everything it
#     starts, now decides before the process name.
#   * Under Hyprland's Lua config `hyprctl dispatch X` is `return
#     hl.dispatch(X)` (0.57 src/ipc/s1/Commands.cpp), so the hyprlang
#     `hyprctl dispatch exit` is refused and Log Out did nothing. The command
#     is now `hl.dsp.exit()`.
#
# And rime-input-apply no longer reports "reloaded" when the reload helper
# failed.
#
# Everything runs against fakes on PATH (hyprctl, labwc, niri, pgrep record
# their argv); nothing reaches a real compositor.
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
RELOAD="$root/files/system/libexec/rime-compositor-reload"
LOGOUT="$root/files/system/libexec/rime-session-logout"
INPUT="$root/files/system/libexec/rime-input-apply"
DISPLAY_APPLY="$root/files/system/libexec/rime-display-apply"
FOCUS="$root/files/system/libexec/rime-agent-focus"
PROJWIN="$root/files/system/libexec/rime-project-windows"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
FAKE="$WORK/bin"; mkdir -p "$FAKE"
CALLS="$WORK/calls"
for tool in hyprctl labwc niri; do
    printf '#!/bin/sh\nprintf "%%s" "%s" >> "%s"; for a in "$@"; do printf " %%s" "$a" >> "%s"; done; echo >> "%s"\n' \
        "$tool" "$CALLS" "$CALLS" "$CALLS" > "$FAKE/$tool"
done
# Like real pgrep: `-x NAME` matches a process of ANY user ($FAKE_PROCS are
# this user's, $FAKE_OTHER another user's, one name per line); `-u UID -x NAME`
# matches only that user's. A helper that forgets -u then picks up a greeter's
# or another seat's compositor, which these fakes make visible.
cat > "$FAKE/pgrep" <<'EOF'
#!/bin/sh
me="$(id -u)"
case "$1" in
    -u) [ "$3" = "-x" ] || exit 2
        [ "$2" = "$me" ] || exit 1
        printf '%s\n' "${FAKE_PROCS:-}" | grep -qxF -- "$4" ;;
    -x) printf '%s\n%s\n' "${FAKE_PROCS:-}" "${FAKE_OTHER:-}" | grep -qxF -- "$2" ;;
    *)  exit 2 ;;
esac
EOF
chmod +x "$FAKE"/*
for t in bash sh grep tr printf cat id; do
    p="$(command -v "$t")" && ln -sf "$p" "$FAKE/$t" 2>/dev/null
done

# run <script> [VAR=value ...]: a clean environment, only the fakes on PATH.
run() {
    local script="$1"; shift
    : > "$CALLS"
    env -i PATH="$FAKE" "$@" bash "$script" >/dev/null 2>&1
    echo "rc=$?" >> "$CALLS"
}
called() { grep -qxF -- "$1" "$CALLS"; }

section "rime-compositor-reload"
run "$RELOAD" HYPRLAND_INSTANCE_SIGNATURE=fixture_sig
called "hyprctl reload" \
    && ok "HYPRLAND_INSTANCE_SIGNATURE alone reloads Hyprland" \
    || bad "HYPRLAND_INSTANCE_SIGNATURE alone reloads Hyprland ($(tr '\n' ' ' < "$CALLS"))"
run "$RELOAD" FAKE_PROCS=.Hyprland-wrapp
called "hyprctl reload" \
    && ok "a wrapped Hyprland (.Hyprland-wrapp) is found by its process name" \
    || bad "a wrapped Hyprland (.Hyprland-wrapp) is found by its process name ($(tr '\n' ' ' < "$CALLS"))"
run "$RELOAD" XDG_CURRENT_DESKTOP=niri HYPRLAND_INSTANCE_SIGNATURE=fixture_sig
called "niri validate" \
    && ok "XDG_CURRENT_DESKTOP still wins over a stray signature" \
    || bad "XDG_CURRENT_DESKTOP still wins over a stray signature ($(tr '\n' ' ' < "$CALLS"))"
run "$RELOAD" FAKE_OTHER=labwc
called "rc=1" && ! grep -q '^labwc' "$CALLS" \
    && ok "another user's labwc (a greeter, a second seat) is not reloaded" \
    || bad "another user's labwc (a greeter, a second seat) is not reloaded ($(tr '\n' ' ' < "$CALLS"))"
run "$RELOAD"
called "rc=1" && ! grep -q '^hyprctl' "$CALLS" \
    && ok "with no compositor at all it fails and reloads nothing" \
    || bad "with no compositor at all it fails and reloads nothing ($(tr '\n' ' ' < "$CALLS"))"

section "rime-session-logout"
run "$LOGOUT" XDG_CURRENT_DESKTOP=Hyprland
called "hyprctl dispatch hl.dsp.exit()" \
    && ok "Hyprland is ended with the Lua dispatcher hl.dsp.exit()" \
    || bad "Hyprland is ended with the Lua dispatcher hl.dsp.exit() ($(tr '\n' ' ' < "$CALLS"))"
run "$LOGOUT" HYPRLAND_INSTANCE_SIGNATURE=fixture_sig
called "hyprctl dispatch hl.dsp.exit()" \
    && ok "HYPRLAND_INSTANCE_SIGNATURE alone ends Hyprland" \
    || bad "HYPRLAND_INSTANCE_SIGNATURE alone ends Hyprland ($(tr '\n' ' ' < "$CALLS"))"
run "$LOGOUT" FAKE_PROCS=.Hyprland-wrapp
called "hyprctl dispatch hl.dsp.exit()" \
    && ok "a wrapped Hyprland is found by its process name" \
    || bad "a wrapped Hyprland is found by its process name ($(tr '\n' ' ' < "$CALLS"))"
for other in Hyprland .Hyprland-wrapp; do
    run "$LOGOUT" FAKE_OTHER=$other
    ! grep -q '^hyprctl' "$CALLS" \
        && ok "another user's $other is not asked to exit" \
        || bad "another user's $other is not asked to exit ($(tr '\n' ' ' < "$CALLS"))"
done
if grep -nE "hyprctl dispatch +[a-z]" "$LOGOUT" "$RELOAD" | grep -v '^[^:]*:[0-9]*:[[:space:]]*#' >/dev/null; then
    bad "no hyprlang dispatch word is left in either helper"
else
    ok "no hyprlang dispatch word is left in either helper"
fi

section "the Python helpers look only at this user's compositor"
PY3="$(command -v python3)"
for helper in "$INPUT:running_compositor" "$DISPLAY_APPLY:compositor"; do
    file="${helper%:*}"; fn="${helper##*:}"
    for case_ in "FAKE_OTHER=labwc:none" "FAKE_PROCS=labwc:labwc"; do
        assign="${case_%:*}"; want="${case_##*:}"
        got="$(env -i PATH="$FAKE" "$assign" "$PY3" - "$file" "$fn" <<'PY' 2>&1
import importlib.machinery, importlib.util, sys
spec = importlib.util.spec_from_loader("h", importlib.machinery.SourceFileLoader("h", sys.argv[1]))
h = importlib.util.module_from_spec(spec); spec.loader.exec_module(h)
r = getattr(h, sys.argv[2])()
print("none" if r in (None, "unknown") else r)
PY
)"
        [ "$got" = "$want" ] \
            && ok "$(basename "$file") $fn with $assign -> $want" \
            || bad "$(basename "$file") $fn with $assign -> $want (got: $got)"
    done
done

section "window helpers speak Lua dispatch"
# Hyprland 0.57 evaluates `hyprctl dispatch X` as `return hl.dispatch(X)`, so
# the hyprlang words (focuswindow, movetoworkspacesilent, workspace) are
# refused, and hyprctl still exits 0 on a refusal. The fake answers "ok" only
# to an hl.dsp call and logs every argv; the generated calls are then parsed
# by Hyprland itself when it is installed (the VM test runs them for real).
DBIN="$WORK/dbin"; mkdir -p "$DBIN"
cat > "$DBIN/hyprctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$DCALLS"
case "$2" in hl.dsp.*) echo ok ;; *) echo "invalid dispatcher" ;; esac
EOF
chmod +x "$DBIN/hyprctl"
DCALLS="$WORK/dcalls"; export DCALLS
dispatch_py() {
    : > "$DCALLS"
    PATH="$DBIN:$PATH" "$PY3" - "$@" <<'PY' 2>&1
import importlib.machinery, importlib.util, sys
def load(p):
    spec = importlib.util.spec_from_loader("m", importlib.machinery.SourceFileLoader("m", p))
    m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m); return m
f, pw = load(sys.argv[1]), load(sys.argv[2])
print(f.hyprland_focus("0x55d1"), pw.hyprland_place("0x55d1", "special:a\"b"),
      pw.hyprland_workspace(4))
PY
}
got="$(dispatch_py "$FOCUS" "$PROJWIN")"
[ "$got" = "True True True" ] \
    && ok "focus, place and workspace succeed only through hl.dsp calls" \
    || bad "focus, place and workspace succeed only through hl.dsp calls (got: $got; $(cat "$DCALLS"))"
grep -qxF 'dispatch hl.dsp.focus({ window = "address:0x55d1" })' "$DCALLS" \
    && ok "rime-agent-focus focuses with hl.dsp.focus({ window = ... })" \
    || bad "rime-agent-focus focuses with hl.dsp.focus ($(cat "$DCALLS"))"
grep -qxF 'dispatch hl.dsp.window.move({ workspace = "special:a\034b", follow = false, window = "address:0x55d1" })' "$DCALLS" \
    && ok "rime-project-windows moves silently with follow = false, the name escaped" \
    || bad "rime-project-windows moves silently with follow = false ($(cat "$DCALLS"))"
grep -qxF 'dispatch hl.dsp.focus({ workspace = "4" })' "$DCALLS" \
    && ok "rime-project-windows switches workspace with hl.dsp.focus({ workspace = ... })" \
    || bad "rime-project-windows switches workspace ($(cat "$DCALLS"))"
# A refusal is a failure: the fake refuses anything but hl.dsp, so swap in one
# that refuses everything with exit 0, as Hyprland does.
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "$DCALLS"\necho "[string]:1: no such window"\n' > "$DBIN/hyprctl"
got="$(dispatch_py "$FOCUS" "$PROJWIN")"
[ "$got" = "False False False" ] \
    && ok "a dispatch Hyprland refuses (exit 0, reply not ok) is reported as failed" \
    || bad "a dispatch Hyprland refuses is reported as failed (got: $got)"
if grep -nE '"dispatch", *"(focuswindow|movetoworkspace|movetoworkspacesilent|workspace|exec)"' \
        "$FOCUS" "$PROJWIN" >/dev/null; then
    bad "no hyprlang dispatcher word is left in the window helpers"
else
    ok "no hyprlang dispatcher word is left in the window helpers"
fi

section "rime-input-apply reports a failed reload"
out="$(python3 - "$INPUT" <<'PY' 2>&1
import importlib.machinery, importlib.util, subprocess, sys, types
spec = importlib.util.spec_from_loader("gen", importlib.machinery.SourceFileLoader("gen", sys.argv[1]))
gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)
gen.os.path.exists = lambda p: True
def fake_run(cmd, **kw):
    return types.SimpleNamespace(returncode=1, stdout="",
                                 stderr="rime: no known compositor is running (XDG_CURRENT_DESKTOP=unset)")
gen.subprocess.run = fake_run
print(gen.reload_compositor())
PY
)"
case "$out" in
    *"not reloaded"*"no known compositor"*) ok "a reload helper that fails is reported, with its reason" ;;
    *) bad "a reload helper that fails is reported, with its reason (got: $out)" ;;
esac

echo
echo "rime session-helpers: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
