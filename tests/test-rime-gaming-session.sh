#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-gaming-session.sh — what `rime-gaming-session` actually hands to
#  gamescope, measured by RUNNING it.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  Before it, the only thing standing behind this script was `bash -n` in
#  Containerfile.rime. That is a parser, not a test: it would have accepted
#  every version of the script that produced the 2026-09-19 katana failure
#  (`ROADMAP/evidence/katana-qualification-20260919.md` §6.1 and §6.2), because
#  nothing there is a syntax error. The two defects were
#
#    * gamescope was given no device preference at all, took its default — the
#      first DRM node — and on a hybrid laptop opened the Intel iGPU, whose
#      only connector is the laptop panel. The RTX 3070 with the user's only
#      monitor on it was never touched.
#    * `--rt` was passed whenever RLIMIT_RTPRIO was non-zero. gamescope gates
#      realtime on CAP_SYS_NICE instead, logged "No CAP_SYS_NICE", and ran at
#      ordinary priority — so the flag was a claim the session could not keep.
#
#  Both are now decisions this script makes from things it can read, so both
#  can be measured on a machine with no GPU at all. That is the point: the
#  selection rule is exercised here against katana's exact sysfs shape without
#  katana, and a regression is caught in CI rather than on hardware.
#
#  ── How it cannot touch the machine ─────────────────────────────────────────
#  * `gamescope`, `steam`, `mangoapp`, `getcap` and `rime` are FAKES first on
#    PATH. The gamescope fake records its argv and exits; nothing starts.
#  * A negative control proves the fakes are really in front, because without
#    it every assertion below could pass by never running anything at all.
#  * The real `rime` binary IS used for the selection — through `RIME_ROOT`, at
#    a fixture tree — because the alternative is a fake that agrees with the
#    test rather than with the code. `$RIME_BIN`, or built with cargo.
#  * No compositor, no window, no display. `RIME_GAMING_NO_RIMED=1` keeps
#    `rime game start` out of it as well.
#
#  PASS = the flags gamescope receives are the ones the evidence measured as
#         correct, on the fixtures that reproduce each machine shape.
#
#  Run from anywhere:  ./tests/test-rime-gaming-session.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION="${ROOT}/files/system/libexec/rime-gaming-session"

pass=0
fail=0
skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); [ -n "${2:-}" ] && printf '      %s\n' "$2"; }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip + 1)); }
section() { printf '\n\033[1m── %s ──\033[0m\n' "$1"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-gaming-session-XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# ── the real rime binary, for the real selection rule ───────────────────────
RIME_BIN="${RIME_BIN:-}"
if [ -z "$RIME_BIN" ]; then
    if cargo build --quiet --manifest-path "${ROOT}/rimed/Cargo.toml" -p rime 2>/dev/null; then
        RIME_BIN="${ROOT}/rimed/target/debug/rime"
    fi
fi
if [ ! -x "${RIME_BIN:-/nonexistent}" ]; then
    printf 'rime-gaming-session: cannot build or find the rime binary; nothing to test\n' >&2
    exit 1
fi

# ── the fakes ───────────────────────────────────────────────────────────────
# One bin directory, reused, with the call log beside it. Every fake appends
# its own name so "nothing was spawned" is an assertion and not a hope.
BIN="${WORK}/bin"
CALLS="${WORK}/calls"
mkdir -p "$BIN"
: > "$CALLS"

make_fake() {
    local name="$1" body="${2:-}"
    cat > "${BIN}/${name}" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "${name}" >> "${CALLS}"
printf '%s\n' "\$*" > "${WORK}/argv-${name}"
${body}
exit 0
FAKE
    chmod +x "${BIN}/${name}"
}

make_fake gamescope
make_fake steam
make_fake mangoapp
# The session hands the login keyring to itself; the real daemon must never
# start from a test, so this one only records.
make_fake gnome-keyring-daemon
# getcap that reports no file capability: the shipped state of every Rime
# machine today, and the state §6.2 measured.
make_fake getcap
# Nothing the session does on its way out may reach the real machine: `sudo`
# here would switch the developer's own session. The fakes record argv.
# `sudo -n … --desktop` answers "hyprland", as the real helper would after a
# switch in from Hyprland; everything else is recorded and succeeds.
cat > "${BIN}/sudo" <<FAKE
#!/usr/bin/env bash
printf 'sudo %s\n' "\$*" >> "${WORK}/sudo-calls"
case " \$* " in *" --desktop "*) echo hyprland ;; esac
exit 0
FAKE
# systemctl: only is-system-running is asked; SYSTEM_STATE picks the answer.
cat > "${BIN}/systemctl" <<FAKE
#!/usr/bin/env bash
[ "\$1" = is-system-running ] && echo "\${SYSTEM_STATE:-running}"
exit 0
FAKE
# systemd-inhibit: record the lock asked for, then run the command it wraps.
cat > "${BIN}/systemd-inhibit" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" > "${WORK}/argv-systemd-inhibit"
while [ \$# -gt 0 ]; do case "\$1" in --*) shift ;; *) break ;; esac; done
exec "\$@"
FAKE
# loginctl: only the session's State is asked; SESSION_STATE picks the answer.
cat > "${BIN}/loginctl" <<FAKE
#!/usr/bin/env bash
echo "\${SESSION_STATE:-active}"
FAKE
chmod +x "${BIN}/sudo" "${BIN}/systemctl" "${BIN}/systemd-inhibit" "${BIN}/loginctl"
# `rime` is a wrapper around the REAL binary so the selection rule under test
# is the one that ships, while `rime game start` still cannot reach a bus.
#
# `rime game …` is INTERCEPTED and never reaches the real binary. That is not
# tidiness: the real `rime game start` talks to rimed on the live system bus,
# so a suite that let it through would enter game mode on the machine running
# the tests — p-core cpuset, IRQ steering, `performance`, scx_lavd — which is
# exactly the state these tests exist to keep off a machine.
#
# The fake records its own PPID alongside the argv, which is what lets the
# owner assertions check that `--owner-pid` names the SESSION SCRIPT and not
# some constant that happens to be a number.
cat > "${BIN}/rime" <<RIMEFAKE
#!/usr/bin/env bash
printf 'rime\n' >> "${CALLS}"
if [ "\${1:-}" = "game" ]; then
    printf 'ppid=%s argv=%s\n' "\$PPID" "\$*" >> "${WORK}/rime-game-calls"
    case "\${2:-}" in
        start)
            # An rimed that predates the owner watch refuses --owner-pid.
            if [ "\${RIME_FAKE_NO_OWNER:-0}" = "1" ] && [[ "\$*" == *--owner-pid* ]]; then
                printf 'rime: entering game mode failed: unknown method StartOwnedBy\n' >&2
                exit 1
            fi
            exit 0 ;;
        stop)
            if [ "\${RIME_FAKE_STOP_FAILS:-0}" = "1" ]; then
                printf 'rime: leaving game mode failed: org.freedesktop.DBus.Error.AccessDenied: not authorized for org.rimeos.rimed.manage-power\n' >&2
                exit 1
            fi
            exit 0 ;;
    esac
    exit 0
fi
exec "${RIME_BIN}" "\$@"
RIMEFAKE
chmod +x "${BIN}/rime"

# ── fixtures: a sysfs tree per machine shape ────────────────────────────────
# `RIME_ROOT` is a filesystem root, so the same tree answers the readiness
# probe and the DRM topology. Only the DRM half matters here.
mkfixture() {
    local name="$1"
    local r="${WORK}/root-${name}"
    mkdir -p "${r}/proc/self" "${r}/sys/class/drm"
    printf 'Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000000000\nCapPrm:\t0000000000000000\nCapAmb:\t0000000000000000\n' \
        > "${r}/proc/self/status"
    printf '%s' "$r"
}

card() {  # <root> <cardN> <vendor-hex> <device-hex> <boot_vga>
    mkdir -p "$1/sys/class/drm/$2/device"
    printf '%s\n' "$3" > "$1/sys/class/drm/$2/device/vendor"
    printf '%s\n' "$4" > "$1/sys/class/drm/$2/device/device"
    printf '%s\n' "$5" > "$1/sys/class/drm/$2/device/boot_vga"
}

conn() {  # <root> <cardN> <NAME> <status>
    mkdir -p "$1/sys/class/drm/$2-$3"
    printf '%s\n' "$4" > "$1/sys/class/drm/$2-$3/status"
}

# The MSI Katana GF76, exactly as §5.1/§6.1 measured it.
KATANA="$(mkfixture katana)"
card "$KATANA" card1 0x8086 0x46a6 1
card "$KATANA" card2 0x10de 0x249d 0
conn "$KATANA" card1 eDP-1 connected
# The iGPU's own HDMI port, wired to nothing. DRM connector names are unique
# per CARD, not per machine, so this one and card2's share a name — and it
# sorts first. Anything that resolves a connector by name alone answers about
# a disconnected port on the wrong GPU.
conn "$KATANA" card1 HDMI-A-1 disconnected
conn "$KATANA" card2 HDMI-A-1 connected

# The ThinkPad L16: one AMD card, one panel.
L16="$(mkfixture l16)"
card "$L16" card1 0x1002 0x15bf 1
conn "$L16" card1 eDP-1 connected

# A machine with nothing plugged in anywhere.
DARK="$(mkfixture dark)"
card "$DARK" card1 0x8086 0x46a6 1
conn "$DARK" card1 eDP-1 disconnected

# ── running the session ─────────────────────────────────────────────────────
# Captures the log and the argv gamescope saw. Never runs a compositor: the
# gamescope on PATH is the fake, and the session's own `command -v` finds it.
run_session() {  # <fixture-root> [extra env assignments...]
    : > "$CALLS"
    rm -f "${WORK}/argv-gamescope"
    env -i \
        PATH="${BIN}:/usr/bin:/bin" \
        HOME="${WORK}/home" \
        RIME_ROOT="$1" \
        RIME_GAMING_NO_RIMED=1 \
        "${@:2}" \
        bash "$SESSION" > "${WORK}/out" 2> "${WORK}/log"
    printf '%s' "$?"
}

# The same, with rimed NOT stubbed out — so `rime game start` / `rime game
# stop` really are called, against the intercepting fake above. Later `env`
# assignments win, so the caller's own overrides still apply.
run_session_with_rimed() {  # <fixture-root> [extra env assignments...]
    : > "$CALLS"
    rm -f "${WORK}/argv-gamescope" "${WORK}/rime-game-calls"
    env -i \
        PATH="${BIN}:/usr/bin:/bin" \
        HOME="${WORK}/home" \
        RIME_ROOT="$1" \
        RIME_GAMING_NO_RIMED=0 \
        "${@:2}" \
        bash "$SESSION" > "${WORK}/out" 2> "${WORK}/log"
    printf '%s' "$?"
}

game_calls() { cat "${WORK}/rime-game-calls" 2>/dev/null; }

gs_argv() { cat "${WORK}/argv-gamescope" 2>/dev/null; }
session_log() { cat "${WORK}/log" 2>/dev/null; }

# ── negative control ────────────────────────────────────────────────────────
section "the fakes are really in front"

rc="$(run_session "$KATANA")"
if [ -s "${WORK}/argv-gamescope" ]; then
    ok "the session ran and the FAKE gamescope is what it reached"
else
    bad "the session ran and the FAKE gamescope is what it reached" \
        "rc=${rc}; log: $(session_log | tail -3)"
fi
# Without this, every assertion below could be measuring an empty file.
if [ "$(gs_argv)" != "" ]; then
    ok "the recorded argv is non-empty, so the assertions below measure something"
else
    bad "the recorded argv is non-empty, so the assertions below measure something"
fi

# ── §6.1: the GPU and the screen ────────────────────────────────────────────
section "Gaming Mode opens the card the monitor is on"

argv="$(gs_argv)"
# The literal pair measured on katana to move gamescope onto card2, HDMI-A-1
# and 240 Hz. `[[ == * ]]` and not `grep -q`: under pipefail a matching
# `grep -q` kills its writer with SIGPIPE and the pipeline returns 141.
if [[ "$argv" == *"--prefer-vk-device 10de:249d"* ]]; then
    ok "the NVIDIA card's PCI id is passed as --prefer-vk-device"
else
    bad "the NVIDIA card's PCI id is passed as --prefer-vk-device" "argv: ${argv}"
fi
if [[ "$argv" == *"--prefer-output HDMI-A-1"* ]]; then
    ok "the external monitor is asked for by name"
else
    bad "the external monitor is asked for by name" "argv: ${argv}"
fi
if [[ "$argv" != *"eDP-1"* ]]; then
    ok "the laptop panel is not what Gaming Mode asks for"
else
    bad "the laptop panel is not what Gaming Mode asks for" "argv: ${argv}"
fi
# The id must come from sysfs, not from a constant in the script: a machine
# that is not katana has to get its own.
# Comment lines are stripped first. The script's own header QUOTES the katana
# measurement, and a checker that read the sentence explaining the rule as a
# violation of it would train the next person to delete the explanation — the
# safe-graphics suite makes the same allowance for the same reason.
runnable_session="$(grep -vE '^[[:space:]]*#' "$SESSION")"
if [[ "$runnable_session" != *"10de:"* ]]; then
    ok "no PCI id is hardcoded in the session script"
else
    bad "no PCI id is hardcoded in the session script" \
        "$(printf '%s' "$runnable_session" | grep -n '10de:')"
fi

rc="$(run_session "$L16")"
argv="$(gs_argv)"
if [[ "$argv" == *"--prefer-vk-device 1002:15bf"* ]]; then
    ok "a single-GPU AMD machine gets its own card, with no special case"
else
    bad "a single-GPU AMD machine gets its own card, with no special case" "argv: ${argv}"
fi

rc="$(run_session "$DARK")"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" != *"--prefer-vk-device"* ]]; then
    ok "with nothing connected, no device is pinned"
else
    bad "with nothing connected, no device is pinned" "argv: ${argv}"
fi
# The whole point of the fix: a fallback that nobody can see IS the defect.
if [[ "$log" == *"ERROR"* ]] && [[ "$log" == *"default"* ]]; then
    ok "and the session says so loudly instead of falling back in silence"
else
    bad "and the session says so loudly instead of falling back in silence" \
        "log: $(printf '%s' "$log" | tail -5)"
fi

section "the selection can be turned off, and says that too"

rc="$(run_session "$KATANA" RIME_GAMING_NO_DEVICE_SELECT=1)"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" != *"--prefer-vk-device"* ]]; then
    ok "RIME_GAMING_NO_DEVICE_SELECT restores gamescope's own default"
else
    bad "RIME_GAMING_NO_DEVICE_SELECT restores gamescope's own default" "argv: ${argv}"
fi
if [[ "$log" == *"integrated GPU"* ]]; then
    ok "…and names the consequence rather than going quiet"
else
    bad "…and names the consequence rather than going quiet" "log: ${log}"
fi

section "a hand-set override still wins"

rc="$(run_session "$KATANA" RIME_GAMESCOPE_ARGS=--prefer-output=DP-9)"
argv="$(gs_argv)"
# gamescope takes the last occurrence of a repeated option, so the override has
# to be positioned after the selection — asserted on ORDER, because both
# appearing in the argv is exactly what a wrong order also looks like.
if [[ "$argv" == *"--prefer-output HDMI-A-1"*"--prefer-output=DP-9"* ]]; then
    ok "RIME_GAMESCOPE_ARGS comes after the computed selection"
else
    bad "RIME_GAMESCOPE_ARGS comes after the computed selection" "argv: ${argv}"
fi

# ── §6.2: realtime ──────────────────────────────────────────────────────────
section "--rt is only claimed when it can be granted"

rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
log="$(session_log)"
# The fixture's CapEff is all zeroes and the fake getcap prints nothing, which
# is every Rime machine today.
if [[ "$argv" != *"--rt"* ]]; then
    ok "with no CAP_SYS_NICE, --rt is not passed"
else
    bad "with no CAP_SYS_NICE, --rt is not passed" "argv: ${argv}"
fi
if [[ "$log" == *"CAP_SYS_NICE: absent"* ]]; then
    ok "and the log says which capability is missing, not that RT was requested"
else
    bad "and the log says which capability is missing, not that RT was requested" \
        "log: $(printf '%s' "$log" | tail -6)"
fi
# The old bug in one assertion: a non-zero rlimit must no longer be enough.
if [[ "$log" != *"requesting realtime"* ]]; then
    ok "the session no longer claims realtime it cannot get"
else
    bad "the session no longer claims realtime it cannot get" "log: ${log}"
fi

# A login that holds CAP_SYS_NICE in its EFFECTIVE set — bit 23, so the hex
# ends in 800000 — is the other way the flag becomes truthful.
EFF="$(mkfixture eff)"
card "$EFF" card1 0x8086 0x46a6 1
conn "$EFF" card1 eDP-1 connected
printf 'Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000800000\nCapPrm:\t0000000000000000\nCapAmb:\t0000000000000000\n' \
    > "${EFF}/proc/self/status"
rc="$(run_session "$EFF")"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" == *"--rt"* ]]; then
    ok "CAP_SYS_NICE in the effective set turns --rt back on"
else
    bad "CAP_SYS_NICE in the effective set turns --rt back on" "argv: ${argv}"
fi
if [[ "$log" == *"effective set"* ]]; then
    ok "…and the log says which set it came from"
else
    bad "…and the log says which set it came from" "log: ${log}"
fi

# A gamescope that DOES carry the file capability turns the flag back on with
# no change to the script — the forward path if a future RPM sets it.
cat > "${BIN}/getcap" <<'CAPFAKE'
#!/usr/bin/env bash
printf '%s cap_sys_nice=ep\n' "$1"
exit 0
CAPFAKE
chmod +x "${BIN}/getcap"
rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" == *"--rt"* ]]; then
    ok "a gamescope with cap_sys_nice=ep gets --rt again"
else
    bad "a gamescope with cap_sys_nice=ep gets --rt again" "argv: ${argv}"
fi
if [[ "$log" == *"file capability"* ]]; then
    ok "and the log says where the capability came from"
else
    bad "and the log says where the capability came from" "log: ${log}"
fi
make_fake getcap   # back to the shipped reality

# ── §7.2: VRR, asked about the right screen and never silently ─────────────
section "adaptive sync"

rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
log="$(session_log)"
# The katana fixture has no vrr_capable anywhere, which is katana. The old code
# globbed, matched nothing, passed nothing and printed nothing — and on a 240 Hz
# monitor "this machine has no VRR" and "this driver does not publish the
# property" then looked identical.
if [[ "$argv" != *"--adaptive-sync"* ]]; then
    ok "with no vrr_capable anywhere, adaptive sync is not requested"
else
    bad "with no vrr_capable anywhere, adaptive sync is not requested" "argv: ${argv}"
fi
if [[ "$log" == *"does not say"* ]]; then
    ok "…and the log distinguishes 'the driver does not say' from 'no VRR here'"
else
    bad "…and the log distinguishes 'the driver does not say' from 'no VRR here'" \
        "log: $(printf '%s' "$log" | grep -i vrr)"
fi

# The namesake trap, end to end: the iGPU's disconnected HDMI-A-1 says 0 and
# the monitor's says 1. A name-only lookup reads the first and turns VRR off on
# a machine that has it.
printf '0\n' > "${KATANA}/sys/class/drm/card1-HDMI-A-1/vrr_capable"
printf '1\n' > "${KATANA}/sys/class/drm/card2-HDMI-A-1/vrr_capable"
rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
if [[ "$argv" == *"--adaptive-sync"* ]]; then
    ok "adaptive sync is read from the chosen card, not from a same-named connector"
else
    bad "adaptive sync is read from the chosen card, not from a same-named connector" \
        "argv: ${argv}"
fi
rm -f "${KATANA}/sys/class/drm/card1-HDMI-A-1/vrr_capable" \
      "${KATANA}/sys/class/drm/card2-HDMI-A-1/vrr_capable"

# …and the case the old global glob got wrong the other way: VRR on the PANEL,
# on a session that is running on the monitor.
printf '1\n' > "${KATANA}/sys/class/drm/card1-eDP-1/vrr_capable"
rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" != *"--adaptive-sync"* ]]; then
    ok "VRR on a screen this session is NOT using does not turn it on"
else
    bad "VRR on a screen this session is NOT using does not turn it on" "argv: ${argv}"
fi
if [[ "$log" == *"not using"* ]]; then
    ok "…and the log says that is what happened"
else
    bad "…and the log says that is what happened" "log: $(printf '%s' "$log" | grep -i vrr)"
fi
rm -f "${KATANA}/sys/class/drm/card1-eDP-1/vrr_capable"

section "a partial answer is used, not thrown away"

# A DRM node with no PCI device behind it: --prefer-output is a perfectly good
# answer and only --prefer-vk-device is unknowable. `rime` exits non-zero
# because the answer is incomplete; discarding the good half because of that
# would be a second silent regression on top of the one being fixed.
NOPCI="$(mkfixture nopci)"
mkdir -p "${NOPCI}/sys/class/drm/card0-HDMI-A-1"
printf 'connected\n' > "${NOPCI}/sys/class/drm/card0-HDMI-A-1/status"
rc="$(run_session "$NOPCI")"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" == *"--prefer-output HDMI-A-1"* ]]; then
    ok "the half that could be answered still reaches gamescope"
else
    bad "the half that could be answered still reaches gamescope" "argv: ${argv}"
fi
if [[ "$argv" != *"--prefer-vk-device"* ]]; then
    ok "…and no device is invented for the half that could not"
else
    bad "…and no device is invented for the half that could not" "argv: ${argv}"
fi
if [[ "$log" == *"ERROR"* ]] && [[ "$log" == *"incomplete"* ]]; then
    ok "…and the gap is said out loud"
else
    bad "…and the gap is said out loud" "log: $(printf '%s' "$log" | tail -5)"
fi

# ── §6.3: the capability reading that explains Steam's bwrap ────────────────
section "the capability sets are logged whatever they are"

rc="$(run_session "$KATANA")"
log="$(session_log)"
if [[ "$log" == *"capabilities: CapEff="* ]]; then
    ok "CapEff/CapPrm/CapAmb are in the session log"
else
    bad "CapEff/CapPrm/CapAmb are in the session log" "log: ${log}"
fi
# A non-empty permitted set is what makes Steam's own bwrap refuse to start.
# §6.3 recorded that message and could not attribute it; this row means the
# next run can.
PRM="$(mkfixture prm)"
card "$PRM" card1 0x8086 0x46a6 1
conn "$PRM" card1 eDP-1 connected
printf 'Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000000000\nCapPrm:\t0000000000800000\nCapAmb:\t0000000000000000\n' \
    > "${PRM}/proc/self/status"
rc="$(run_session "$PRM")"
log="$(session_log)"
if [[ "$log" == *"permitted set"* ]] && [[ "$log" == *"bwrap"* ]]; then
    ok "a non-empty permitted set is called out, with what it breaks"
else
    bad "a non-empty permitted set is called out, with what it breaks" \
        "log: $(printf '%s' "$log" | tail -6)"
fi

# ── the fail-safe that already worked, and must keep working ───────────────
section "the fail-safe"

: > "$CALLS"
rm -f "${BIN}/gamescope"
rc="$(env -i PATH="${BIN}:/usr/bin:/bin" HOME="${WORK}/home" RIME_ROOT="$KATANA" \
      RIME_GAMING_NO_RIMED=1 bash "$SESSION" >/dev/null 2>"${WORK}/log"; printf '%s' "$?")"
if [ "$rc" != "0" ]; then
    ok "a missing gamescope exits non-zero, so greetd re-displays the greeter"
else
    bad "a missing gamescope exits non-zero, so greetd re-displays the greeter" "rc=${rc}"
fi
if [[ "$(session_log)" == *"sudo rime install"* ]]; then
    ok "…and names the command that installs it"
else
    bad "…and names the command that installs it" "log: $(session_log)"
fi
make_fake gamescope

# ── the MangoHud overlay, and the flag it cannot coexist with ──────────────
#
# MEASURED ON KATANA (evidence §3.4 and the 2026-09-20 A/B in the session
# script's own header): `--expose-wayland` puts WAYLAND_DISPLAY into mangoapp's
# environment, GLFW takes its Wayland backend, mangoapp dereferences the NULL
# X11 display it gets back, and gamescopereaper respawns the segfault ~2 Hz for
# the whole session — 15 376 core dumps and 4.0 GB in one boot, with no overlay
# ever drawn. Both directions are asserted, because a gate that only ever
# answers one way is the defect family this repo keeps finding.
section "MangoHud is not passed into a crash loop"

rc="$(run_session "$KATANA")"
argv="$(gs_argv)"
if [[ "$argv" == *"--expose-wayland"* ]]; then
    ok "--expose-wayland is still passed by default (native Wayland games)"
else
    bad "--expose-wayland is still passed by default (native Wayland games)" "argv: ${argv}"
fi
if [[ "$argv" != *"--mangoapp"* ]]; then
    ok "…and --mangoapp is NOT, even though mangoapp is on PATH"
else
    bad "…and --mangoapp is NOT, even though mangoapp is on PATH" "argv: ${argv}"
fi
log="$(session_log)"
if [[ "$log" == *"NOT passing --mangoapp"* ]] && [[ "$log" == *"WAYLAND_DISPLAY"* ]]; then
    ok "…and the log says which flag rules it out, and why"
else
    bad "…and the log says which flag rules it out, and why" "log: $(printf '%s' "$log" | tail -6)"
fi

# The other direction. Without this row the gate above passes just as well for
# a script that has simply deleted --mangoapp.
rc="$(run_session "$KATANA" RIME_GAMING_EXPOSE_WAYLAND=0)"
argv="$(gs_argv)"
if [[ "$argv" == *"--mangoapp"* ]] && [[ "$argv" != *"--expose-wayland"* ]]; then
    ok "with RIME_GAMING_EXPOSE_WAYLAND=0 the overlay comes back and exposure goes"
else
    bad "with RIME_GAMING_EXPOSE_WAYLAND=0 the overlay comes back and exposure goes" \
        "argv: ${argv}"
fi

# And a machine with no mangohud installed must get neither the flag nor the
# explanation — the message is about a choice, not about an absent package.
mv "${BIN}/mangoapp" "${WORK}/mangoapp.hidden"
rc="$(run_session "$KATANA" RIME_GAMING_EXPOSE_WAYLAND=0)"
argv="$(gs_argv)"
log="$(session_log)"
if [[ "$argv" != *"--mangoapp"* ]] && [[ "$log" != *"NOT passing --mangoapp"* ]]; then
    ok "no mangoapp on PATH: no flag and no explanation of a choice nobody made"
else
    bad "no mangoapp on PATH: no flag and no explanation of a choice nobody made" \
        "argv: ${argv}"
fi
mv "${WORK}/mangoapp.hidden" "${BIN}/mangoapp"

# ── releasing game mode when the session is destroyed (evidence §3.4) ──────
#
# The defect: `cleanup()` runs `rime game stop`, which is polkit action
# `org.rimeos.rimed.manage-power` — `allow_active=yes`, `auth_admin` otherwise.
# The instant logind deactivates the session the call is REFUSED, and on katana
# the machine sat for 75 minutes on a p-core cpuset with steered IRQs, the
# `performance` tier and scx_lavd, with nothing able to undo it and NOTHING IN
# ITS OWN LOG. These rows hold the two halves of the fix: the session hands
# rimed an owner to watch, and the trap can no longer fail in silence.
section "Steam starts through the HDR and composition guard"

# Measured on katana: with HDR off, gamescope's direct scan-out on NVIDIA drew
# the picture three times over until composition was forced. The guard has to
# be gamescope's command (the properties live on gamescope's own X server),
# and Steam's own arguments must still reach Steam after it.
WRAP="${ROOT}/files/system/libexec/rime-gamescope-steam"
rc="$(run_session "$KATANA" RIME_GAMESCOPE_STEAM="${WRAP}")"
argv="$(gs_argv)"
if [[ "$argv" == *" -- ${WRAP} steam -gamepadui -steamos3" ]]; then
    ok "gamescope runs the guard, and the guard is handed steam with its arguments"
else
    bad "gamescope runs the guard, and the guard is handed steam with its arguments" "argv: ${argv}"
fi
# An image without the guard still starts Steam, and says what it gave up.
rc="$(run_session "$KATANA" RIME_GAMESCOPE_STEAM="${WORK}/no-such-guard")"
argv="$(gs_argv)"
if [[ "$argv" == *" -- steam -gamepadui -steamos3" ]] && [[ "$(session_log)" == *"no ${WORK}/no-such-guard"* ]]; then
    ok "without the guard Steam still starts directly, and the log says so"
else
    bad "without the guard Steam still starts directly, and the log says so" \
        "argv: ${argv}; log: $(session_log | grep -i guard | tail -2)"
fi

section "a session that is torn down can still be released"

rc="$(run_session_with_rimed "$KATANA")"
calls="$(game_calls)"
start_line="$(printf '%s\n' "$calls" | grep -m1 'argv=game start' || true)"
if [[ "$start_line" == *"--owner-pid"* ]]; then
    ok "the session hands rimed an owner pid at start"
else
    bad "the session hands rimed an owner pid at start" "game calls: ${calls}"
fi
# The pid must be THE SESSION SCRIPT's, not a constant that happens to parse.
# The fake records its own PPID, which is the script that invoked it.
owner_pid="$(printf '%s' "$start_line" | sed -n 's/.*--owner-pid \([0-9]*\).*/\1/p')"
caller_pid="$(printf '%s' "$start_line" | sed -n 's/^ppid=\([0-9]*\).*/\1/p')"
if [ -n "$owner_pid" ] && [ "$owner_pid" = "$caller_pid" ]; then
    ok "…and it is the session script's own pid (${owner_pid}), not a literal"
else
    bad "…and it is the session script's own pid, not a literal" \
        "owner=${owner_pid} caller=${caller_pid}; line: ${start_line}"
fi
if [[ "$(session_log)" == *"release is owned by rimed"* ]]; then
    ok "…and the log says the release no longer depends on this session's privileges"
else
    bad "…and the log says the release no longer depends on this session's privileges" \
        "log: $(session_log | tail -5)"
fi
# The trap still runs on the clean path.
if printf '%s\n' "$calls" | pipe_has 'argv=game stop'; then
    ok "the EXIT trap still calls game stop on the clean path"
else
    bad "the EXIT trap still calls game stop on the clean path" "game calls: ${calls}"
fi
if [[ "$(session_log)" == *"rimed game mode released"* ]]; then
    ok "…and says so when it worked"
else
    bad "…and says so when it worked" "log: $(session_log | tail -5)"
fi

# A REFUSED stop is the measured failure mode, and it must be loud. The old
# script's `rime game stop >/dev/null 2>&1 && log …` printed nothing at all.
rc="$(run_session_with_rimed "$KATANA" RIME_FAKE_STOP_FAILS=1)"
log="$(session_log)"
if [[ "$log" == *"could not release game mode"* ]]; then
    ok "a refused release is REPORTED, not swallowed"
else
    bad "a refused release is REPORTED, not swallowed" "log: $(printf '%s' "$log" | tail -6)"
fi
if [[ "$log" == *"not authorized for org.rimeos.rimed.manage-power"* ]]; then
    ok "…with polkit's own message, so the cause is in the log a user can read"
else
    bad "…with polkit's own message, so the cause is in the log a user can read" \
        "log: $(printf '%s' "$log" | tail -6)"
fi
if [[ "$log" == *"rimed is"*"watching pid"* ]]; then
    ok "…and names what will release it instead"
else
    bad "…and names what will release it instead" "log: $(printf '%s' "$log" | tail -6)"
fi

# An rimed too old to take an owner must still start game mode — and must say
# what has been lost rather than reading like a normal start.
rc="$(run_session_with_rimed "$KATANA" RIME_FAKE_NO_OWNER=1)"
calls="$(game_calls)"
log="$(session_log)"
if printf '%s\n' "$calls" | pipe_has 'argv=game start$'; then
    ok "an rimed that refuses --owner-pid still gets a plain game start"
else
    bad "an rimed that refuses --owner-pid still gets a plain game start" \
        "game calls: ${calls}"
fi
if [[ "$log" == *"does not accept a session owner"* ]]; then
    ok "…and the fallback says the release now depends on a trap polkit can refuse"
else
    bad "…and the fallback says the release now depends on a trap polkit can refuse" \
        "log: $(printf '%s' "$log" | tail -6)"
fi

# ── Gaming Mode is a toggle, and drops laptop behaviour ─────────────────────
printf '#!/bin/sh\nexit 0\n' > "${WORK}/helper"; chmod +x "${WORK}/helper"
rm -f "${WORK}/sudo-calls" "${WORK}/argv-systemd-inhibit"
rc="$(run_session "$KATANA" RIME_SESSION_HELPER="${WORK}/helper")"
inh="$(cat "${WORK}/argv-systemd-inhibit" 2>/dev/null)"
if [[ "${inh}" == *"--what=handle-lid-switch:idle "* ]] && [[ "${inh}" == *" gamescope "* ]]; then
    ok "gamescope runs under a lid-switch + idle inhibitor"
else
    bad "gamescope runs under a lid-switch + idle inhibitor" "inhibit argv: ${inh}"
fi
if [[ "${inh}" != *sleep* ]]; then ok "...which leaves sleep alone (Steam's Power > Sleep still works)"
else bad "...which leaves sleep alone (Steam's Power > Sleep still works)" "${inh}"; fi
if [ -n "$(gs_argv)" ]; then ok "gamescope still runs through the inhibitor"
else bad "gamescope still runs through the inhibitor" "no gamescope argv"; fi
if grep -qx "sudo -n ${WORK}/helper hyprland --switch" "${WORK}/sudo-calls" 2>/dev/null; then
    ok "leaving Gaming Mode switches back to the desktop it came from"
else
    bad "leaving Gaming Mode switches back to the desktop it came from" "$(cat "${WORK}/sudo-calls" 2>/dev/null)"
fi
rm -f "${WORK}/sudo-calls"
rc="$(run_session "$KATANA" RIME_SESSION_HELPER="${WORK}/helper" SYSTEM_STATE=stopping)"
if ! grep -q -- "--switch" "${WORK}/sudo-calls" 2>/dev/null; then
    ok "...but not while the machine is shutting down"
else
    bad "...but not while the machine is shutting down" "$(cat "${WORK}/sudo-calls")"
fi
rm -f "${WORK}/sudo-calls"
rc="$(run_session "$KATANA" RIME_SESSION_HELPER="${WORK}/helper" RIME_GAMING_NO_RETURN=1)"
if ! grep -q -- "--switch" "${WORK}/sudo-calls" 2>/dev/null; then
    ok "...and not with RIME_GAMING_NO_RETURN=1"
else
    bad "...and not with RIME_GAMING_NO_RETURN=1" "$(cat "${WORK}/sudo-calls")"
fi

rm -f "${WORK}/sudo-calls"
rc="$(run_session "$KATANA" RIME_SESSION_HELPER="${WORK}/helper" SESSION_STATE=closing)"
if ! grep -q -- "--switch" "${WORK}/sudo-calls" 2>/dev/null; then
    ok "...and not when a switch is ending the session (it chose where to go)"
else
    bad "...and not when a switch is ending the session (it chose where to go)" "$(cat "${WORK}/sudo-calls")"
fi

# ── the login keyring stays unlocked in Gaming Mode ────────────────────────
section "the login keyring"
rc="$(run_session "$KATANA")"
if [ "$(cat "${WORK}/argv-gnome-keyring-daemon" 2>/dev/null)" = "--start --components=secrets" ]; then
    ok "the session takes over the keyring PAM unlocked (gnome-keyring-daemon --start)"
else
    bad "the session takes over the keyring PAM unlocked (gnome-keyring-daemon --start)" \
        "argv: $(cat "${WORK}/argv-gnome-keyring-daemon" 2>/dev/null)"
fi
first="$(grep -n -x -e gnome-keyring-daemon -e gamescope "$CALLS" | head -1)"
if [ "${first#*:}" = "gnome-keyring-daemon" ]; then
    ok "...before gamescope and Steam start"
else
    bad "...before gamescope and Steam start" "calls: $(tr '\n' ' ' < "$CALLS")"
fi

printf '\nrime-gaming-session: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
