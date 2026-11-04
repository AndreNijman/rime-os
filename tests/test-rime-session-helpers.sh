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
# pgrep -x NAME succeeds only for a name listed in $FAKE_PROCS (one per line).
cat > "$FAKE/pgrep" <<'EOF'
#!/bin/sh
[ "$1" = "-x" ] || exit 2
printf '%s\n' "${FAKE_PROCS:-}" | grep -qxF -- "$2"
EOF
chmod +x "$FAKE"/*
for t in bash sh grep tr printf cat; do
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
if grep -nE "hyprctl dispatch +[a-z]" "$LOGOUT" "$RELOAD" | grep -v '^[^:]*:[0-9]*:[[:space:]]*#' >/dev/null; then
    bad "no hyprlang dispatch word is left in either helper"
else
    ok "no hyprlang dispatch word is left in either helper"
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
