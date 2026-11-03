#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rimed-ui-boost.sh — the UI boost and the one-writer facade, end to end,
#  through the REAL rimed and rime binaries, on a private bus and a fake sysfs.
#
#  What runs for real: rimed (profile selection, the tier plan, the writer, the
#  Ui interface, the caller policy, the peer watch, the power-profiles facade),
#  `rime ui boost` and `rime ui hypr-boost`, dbus-daemon, and, when installed,
#  power-profiles-daemon and powerprofilesctl.
#
#  What is fake: logind + polkit (rimed/rimed/examples/fake-session-bus.rs,
#  facts from files so the test can switch the active user), a Hyprland event
#  socket (python3), and the sysfs rimed reads and writes. That last one is
#  bind-mounted over the real paths inside `unshare -Urmn` (a user, mount and
#  network namespace): /sys/devices/system/cpu/cpufreq, the DMI ids (a ThinkPad
#  L16, so the L16 profile is selected), power_supply (on battery), the ACPI
#  platform profile, hwmon, thermal, powercap, intel_pstate and msi-ec. The
#  binds are asserted before rimed starts. Even a missed bind cannot reach the
#  host: uid 0 in a user namespace may not write files the real root owns.
#  The real system bus is hidden under an empty /run/dbus.
#
#  Without a user namespace this SKIPs; RIME_TEST_REQUIRE_UI_BOOST=1 (CI) turns
#  every skip into a failure, including a missing power-profiles-daemon.
# ─────────────────────────────────────────────────────────────────────────────
# ok() and bad() always return 0, so `test && ok || bad` is a real if/else.
# shellcheck disable=SC2015
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2
REPO="$(cd .. && pwd)"
REQUIRE=${RIME_TEST_REQUIRE_UI_BOOST:-0}

skip() {
    if [ "$REQUIRE" = 1 ]; then echo "FAIL  required: $1"; exit 1; fi
    echo "SKIP: $1"; exit 0
}

if [ "${RIME_UIBOOST_IN_NS:-0}" != 1 ]; then
    TGT=${CARGO_TARGET_DIR:-$REPO/rimed/target}
    RIMED_BIN=${RIMED_BIN:-$TGT/debug/rimed}
    RIME_BIN=${RIME_BIN:-$TGT/debug/rime}
    FAKE_BIN=${FAKE_BIN:-$TGT/debug/examples/fake-session-bus}
    if [ ! -x "$RIMED_BIN" ] || [ ! -x "$RIME_BIN" ] || [ ! -x "$FAKE_BIN" ]; then
        command -v cargo >/dev/null || skip "no cargo and no prebuilt binaries"
        ( cd "$REPO/rimed" && cargo build --locked -p rimed -p rime --bins --examples ) \
            || { echo "FATAL: build failed"; exit 2; }
    fi
    command -v unshare >/dev/null || skip "no unshare"
    command -v dbus-daemon >/dev/null || skip "no dbus-daemon"
    command -v python3 >/dev/null || skip "no python3"
    unshare -Urmn true 2>/dev/null || skip "this kernel/runner refuses an unprivileged user namespace"
    exec env RIME_UIBOOST_IN_NS=1 RIMED_BIN="$RIMED_BIN" RIME_BIN="$RIME_BIN" FAKE_BIN="$FAKE_BIN" \
        unshare -Urmn "$(pwd)/$(basename "$0")" "$@"
fi

# ── inside the namespace ─────────────────────────────────────────────────────
pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

T=$(mktemp -d /tmp/rimed-uiboost.XXXXXX)
PIDS=()
cleanup() {
    for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
    wait 2>/dev/null
    [ "${RIME_TEST_KEEP:-0}" = 1 ] && echo "kept $T" || rm -rf "$T"
}
trap cleanup EXIT

# ── fake sysfs ───────────────────────────────────────────────────────────────
F=$T/sys
CF=$F/cpufreq
for n in 0 1 2 3; do
    d=$CF/policy$n
    mkdir -p "$d"
    echo amd-pstate-epp > "$d/scaling_driver"
    echo "performance powersave" > "$d/scaling_available_governors"
    echo powersave > "$d/scaling_governor"
    echo "default performance balance_performance balance_power power" \
        > "$d/energy_performance_available_preferences"
    echo balance_power > "$d/energy_performance_preference"
    echo 1 > "$d/boost"
    echo 400000 > "$d/cpuinfo_min_freq"
    echo 5100000 > "$d/cpuinfo_max_freq"
    echo $((1500000 + n * 10000)) > "$d/amd_pstate_lowest_nonlinear_freq"
    echo 400000 > "$d/scaling_min_freq"
    echo 5100000 > "$d/scaling_max_freq"
done
mkdir -p "$F/dmi" "$F/ps/AC" "$F/ps/BAT0" "$F/acpi" "$F/empty" "$F/run-dbus"
echo LENOVO > "$F/dmi/sys_vendor"
echo 21SCCTO1WW > "$F/dmi/product_name"
echo "ThinkPad L16 Gen 2" > "$F/dmi/product_version"
echo "ThinkPad L16 Gen 2" > "$F/dmi/product_family"
echo 10 > "$F/dmi/chassis_type"
echo Mains > "$F/ps/AC/type"; echo 0 > "$F/ps/AC/online"
echo Battery > "$F/ps/BAT0/type"; echo Discharging > "$F/ps/BAT0/status"
echo 80 > "$F/ps/BAT0/capacity"
echo 0 > "$F/ps/BAT0/charge_control_start_threshold"
echo 100 > "$F/ps/BAT0/charge_control_end_threshold"
echo balanced > "$F/acpi/platform_profile"
echo "low-power balanced performance" > "$F/acpi/platform_profile_choices"

bindover() { # fake real
    [ -d "$2" ] || return 0
    mount --bind "$1" "$2" || { bad "bind $2" "mount --bind failed"; return 1; }
}
bindover "$CF" /sys/devices/system/cpu/cpufreq
bindover "$F/dmi" /sys/devices/virtual/dmi/id
bindover "$F/ps" /sys/class/power_supply
bindover "$F/acpi" /sys/firmware/acpi
for d in /sys/class/hwmon /sys/class/thermal /sys/class/powercap \
         /sys/devices/system/cpu/intel_pstate /sys/devices/platform/msi-ec \
         /sys/kernel/sched_ext; do
    bindover "$F/empty" "$d"
done
bindover "$F/run-dbus" /run/dbus
# An installed Rime has its own (older) profiles here, and they override the
# set built into rimed: test THIS tree's.
bindover "$REPO/config/sysprofiles" /usr/share/rimeos/sysprofiles

if [ "$(cat /sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference 2>/dev/null)" = balance_power ] \
   && [ "$(cat /sys/devices/virtual/dmi/id/product_version 2>/dev/null)" = "ThinkPad L16 Gen 2" ] \
   && [ "$(cat /sys/class/power_supply/BAT0/status 2>/dev/null)" = Discharging ] \
   && [ ! -e /run/dbus/system_bus_socket ] \
   && { [ ! -d /usr/share/rimeos/sysprofiles ] \
        || cmp -s /usr/share/rimeos/sysprofiles/thinkpad-l16-g2.toml "$REPO/config/sysprofiles/thinkpad-l16-g2.toml"; }; then
    ok "the fake sysfs and this tree's profiles are what rimed sees; the real system bus is hidden"
else
    bad "fake sysfs in place" "a bind did not take; refusing to start rimed"
    echo "passed=$pass failed=$fail"; exit 1
fi

# rimed writes values without a newline; awk prints one line per file anyway.
vals() { awk '{print}' "$CF"/policy*/"$1" | sort -u | tr '\n' ' ' | sed 's/ $//'; }
epp_all() { vals energy_performance_preference; }
wait_epp() { # want timeout_ms
    local i=0
    while [ "$i" -lt "$(( $2 / 20 ))" ]; do
        [ "$(epp_all)" = "$1" ] && return 0
        sleep 0.02; i=$((i+1))
    done
    return 1
}
sysfs_hash() { find "$CF" "$F/acpi" "$F/ps" -type f -print0 | sort -z | xargs -0 cat | sha256sum; }

# ── private bus, fake logind/polkit, rimed ───────────────────────────────────
cat > "$T/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path=$T/bus.sock</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow receive_sender="*"/>
  </policy>
</busconfig>
EOF
dbus-daemon --config-file="$T/bus.conf" --nofork --nopidfile >"$T/bus.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do [ -S "$T/bus.sock" ] && break; sleep 0.05; done
export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$T/bus.sock"
unset DBUS_SESSION_BUS_ADDRESS

has_owner() {
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner string:"$1" 2>/dev/null \
        | grep "boolean true" >/dev/null
}
owner_of() {
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.GetNameOwner string:"$1" 2>/dev/null \
        | sed -n 's/.*string "\(.*\)"/\1/p'
}

S=$T/session; mkdir -p "$S"; echo 0 > "$S/uid"
FAKE_SESSION_DIR=$S "$FAKE_BIN" >"$T/fake.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do has_owner org.freedesktop.login1 && break; sleep 0.05; done
has_owner org.freedesktop.login1 && ok "fake logind + polkit on the private bus" \
    || bad "fake logind" "$(cat "$T/fake.log")"

mkdir -p "$T/state"
STATE_DIRECTORY=$T/state "$RIMED_BIN" >"$T/rimed.log" 2>&1 &
RIMED_PID=$!; PIDS+=("$RIMED_PID")
for _ in $(seq 200); do has_owner org.freedesktop.UPower.PowerProfiles && break; sleep 0.05; done

if grep -q "profile active=thinkpad-l16-g2" "$T/rimed.log"; then
    ok "rimed selected the L16 profile from the fake DMI"
else
    bad "L16 profile" "$(head -3 "$T/rimed.log")"
fi
if [ "$(epp_all)" = balance_power ] \
   && [ "$(vals scaling_governor)" = powersave ] \
   && [ "$(awk '{print}' "$CF/policy1/scaling_min_freq")" = 1510000 ] \
   && [ "$(vals boost)" = 1 ]; then
    ok "on battery the tier is balanced: powersave, balance_power, boost on, floor = lowest_nonlinear"
else
    bad "battery tier" "epp=$(epp_all) gov=$(cat "$CF/policy0/scaling_governor") min=$(cat "$CF/policy1/scaling_min_freq")"
fi

# ── the boost ────────────────────────────────────────────────────────────────
"$RIME_BIN" ui boost 400 >"$T/b1.out" 2>&1 &
B1=$!
if wait_epp balance_performance 1000; then
    ok "Boost(400) from the active session's user raises EPP to balance_performance on every policy"
else
    bad "boost raised EPP" "epp=$(epp_all) $(cat "$T/b1.out")"
fi
grep -q "boosted for 400 ms" "$T/b1.out" 2>/dev/null || sleep 0.1
wait "$B1"
grep -q "boosted for 400 ms" "$T/b1.out" && ok "the call answers the granted milliseconds" \
    || bad "granted ms" "$(cat "$T/b1.out")"
wait_epp balance_power 1000 && ok "the tier's own EPP is written back when the boost ends" \
    || bad "restore" "epp=$(epp_all)"
[ "$(vals scaling_governor)" = powersave ] \
    && [ "$(awk '{print}' "$CF/policy1/scaling_min_freq")" = 1510000 ] \
    && ok "nothing but EPP moved (governor and floor untouched)" \
    || bad "only EPP" "gov/min changed"

out=$("$RIME_BIN" ui boost 5000 2>&1)
case "$out" in *"boosted for 600 ms"*) ok "one call is capped at 600 ms" ;;
    *) bad "cap" "$out" ;; esac
wait_epp balance_power 1500 >/dev/null

# Release on disconnect: kill the caller mid-window, the restore comes long
# before its 600 ms would have run out.
"$RIME_BIN" ui boost 600 >/dev/null 2>&1 &
B2=$!
if wait_epp balance_performance 1000; then
    kill -9 "$B2"; wait "$B2" 2>/dev/null
    t0=$(date +%s%N)
    if wait_epp balance_power 1000; then
        dt=$(( ($(date +%s%N) - t0) / 1000000 ))
        if [ "$dt" -lt 350 ]; then
            ok "a caller that leaves the bus releases its boost at once (${dt} ms after kill)"
        else
            bad "release on disconnect" "restored only after ${dt} ms (the 600 ms timer?)"
        fi
    else
        bad "release on disconnect" "never restored"
    fi
else
    bad "release on disconnect" "the boost never started"
fi

# ── the caller policy ────────────────────────────────────────────────────────
before=$(sysfs_hash)
echo 4242 > "$S/uid"; sleep 2.3    # past the 2 s session cache
out=$("$RIME_BIN" ui boost 400 2>&1); rc=$?
if [ "$rc" != 0 ] && echo "$out" | grep "AccessDenied\|not the user of the active session" >/dev/null; then
    ok "a caller who is not the active session's user is refused (AccessDenied)"
else
    bad "refuse other uid" "rc=$rc $out"
fi
echo 0 > "$S/uid"; echo 1 > "$S/remote"; sleep 2.3
out=$("$RIME_BIN" ui boost 400 2>&1); rc=$?
[ "$rc" != 0 ] && echo "$out" | grep "remote" >/dev/null \
    && ok "the right user in a REMOTE session is refused" || bad "refuse remote" "rc=$rc $out"
[ "$(sysfs_hash)" = "$before" ] && ok "a refused call writes nothing" || bad "refused wrote" "sysfs changed"
rm -f "$S/remote"; sleep 2.3

# ── the Hyprland watcher ─────────────────────────────────────────────────────
export XDG_RUNTIME_DIR=$T/xdg HYPRLAND_INSTANCE_SIGNATURE=test
mkdir -p "$XDG_RUNTIME_DIR/hypr/test"
cat > "$T/hypr.py" <<'EOF'
import socket, sys, os, time
p = sys.argv[1]
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(1)
c, _ = s.accept()
fifo = sys.argv[2]
while True:
    with open(fifo) as f:
        for line in f:
            c.sendall(line.encode())
EOF
mkfifo "$T/hypr.fifo"
python3 "$T/hypr.py" "$XDG_RUNTIME_DIR/hypr/test/.socket2.sock" "$T/hypr.fifo" &
PIDS+=($!)
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/hypr/test/.socket2.sock" ] && break; sleep 0.05; done
"$RIME_BIN" ui hypr-boost >"$T/hypr.out" 2>&1 &
HB=$!; PIDS+=("$HB")
sleep 0.3
echo "activewindow>>kitty,~" > "$T/hypr.fifo"
sleep 0.3
[ "$(epp_all)" = balance_power ] && ok "a non-animation Hyprland event asks for nothing" \
    || bad "hypr ignore" "epp=$(epp_all)"
echo "openwindow>>80a6f50,2,kitty,kitty" > "$T/hypr.fifo"
if wait_epp balance_performance 1000; then
    ok "rime ui hypr-boost boosts on openwindow"
    # The watcher stays connected: this restore is the window's own timer.
    if wait_epp balance_power 1500 && kill -0 "$HB" 2>/dev/null; then
        ok "the boost ends on its timer while the caller is still connected"
    else
        bad "timer restore" "epp=$(epp_all)"
    fi
else
    bad "hypr boost" "epp=$(epp_all) $(cat "$T/hypr.out")"
fi

# ── one writer: power-profiles-daemon cannot take over ───────────────────────
PPD=""
for c in /usr/libexec/power-profiles-daemon /usr/lib/power-profiles-daemon/power-profiles-daemon \
         "$(command -v power-profiles-daemon 2>/dev/null)"; do
    [ -n "$c" ] && [ -x "$c" ] && PPD=$c && break
done
# The image side of "one writer": the service is masked, the conf lets rimed
# own PPD's names.
grep -F 'systemctl mask power-profiles-daemon.service' "$REPO/Containerfile.base" >/dev/null \
    && grep -F '<allow own="org.freedesktop.UPower.PowerProfiles"/>' \
        "$REPO/files/system/dbus-1/system.d/org.rimeos.Rimed1.conf" >/dev/null \
    && ok "the image masks power-profiles-daemon.service and lets rimed own its names" \
    || bad "image one-writer" "Containerfile.base mask or Rimed1.conf own rule missing"
RIMED_UNIQUE=$(owner_of org.rimeos.Rimed1)
case "$RIMED_UNIQUE" in :*) ;; *) bad "rimed's unique name" "GetNameOwner answered '$RIMED_UNIQUE'"; RIMED_UNIQUE=unknown ;; esac
if [ "$(owner_of org.freedesktop.UPower.PowerProfiles)" = "$RIMED_UNIQUE" ] \
   && [ "$(owner_of net.hadess.PowerProfiles)" = "$RIMED_UNIQUE" ]; then
    ok "rimed owns both power-profiles names"
else
    bad "facade names" "upower=$(owner_of org.freedesktop.UPower.PowerProfiles) rimed=$RIMED_UNIQUE"
fi
if [ -n "$PPD" ]; then
    before=$(sysfs_hash)
    for args in "" "--replace"; do
        # shellcheck disable=SC2086
        "$PPD" $args >"$T/ppd.log" 2>&1 &
        P=$!
        sleep 3
        kill "$P" 2>/dev/null; wait "$P" 2>/dev/null
        [ "$(owner_of org.freedesktop.UPower.PowerProfiles)" = "$RIMED_UNIQUE" ] \
            && [ "$(owner_of net.hadess.PowerProfiles)" = "$RIMED_UNIQUE" ] \
            && ok "power-profiles-daemon ${args:-(plain)} never gets the names" \
            || bad "ppd ${args}" "names moved: $(cat "$T/ppd.log")"
    done
    [ "$(sysfs_hash)" = "$before" ] && ok "and wrote nothing (fake sysfs unchanged)" \
        || bad "ppd wrote" "sysfs changed"
    kill -0 "$RIMED_PID" 2>/dev/null && ok "rimed is still running" || bad "rimed alive" "$(tail -3 "$T/rimed.log")"
else
    [ "$REQUIRE" = 1 ] && bad "power-profiles-daemon" "not installed" || echo "SKIP  power-profiles-daemon not installed"
fi

# ── powerprofilesctl talks to rimed ──────────────────────────────────────────
if command -v powerprofilesctl >/dev/null && python3 -c 'import gi' 2>/dev/null; then
    [ "$(powerprofilesctl get 2>&1)" = balanced ] && ok "powerprofilesctl get = balanced (rimed's tier)" \
        || bad "ppctl get" "$(powerprofilesctl get 2>&1)"
    powerprofilesctl list 2>&1 | grep '^\* balanced:' >/dev/null && ok "powerprofilesctl list marks balanced" \
        || bad "ppctl list" "$(powerprofilesctl list 2>&1)"
    out=$(powerprofilesctl set performance 2>&1); rc=$?
    [ "$rc" != 0 ] && [ "$(epp_all)" = balance_power ] && ok "powerprofilesctl set is refused and writes nothing" \
        || bad "ppctl set" "rc=$rc epp=$(epp_all) $out"
    powerprofilesctl launch -p power-saver -r test -i rime-test sleep 1.5 >/dev/null 2>&1 &
    L=$!
    if wait_epp power 1500; then
        ok "a power-saver hold applies the power-saver tier"
        out=$("$RIME_BIN" ui boost 400 2>&1)
        [ "$(epp_all)" = power ] && ok "no boost in power-saver ($out)" || bad "boost under hold" "epp=$(epp_all)"
    else
        bad "hold" "epp=$(epp_all)"
    fi
    wait "$L"
    wait_epp balance_power 1500 && ok "releasing the hold puts the battery tier back" \
        || bad "hold release" "epp=$(epp_all)"
else
    [ "$REQUIRE" = 1 ] && bad "powerprofilesctl" "not installed (or no python3-gi)" \
        || echo "SKIP  powerprofilesctl / python3-gi not installed"
fi

grep -q "panicked" "$T/rimed.log" && bad "rimed log" "$(grep -m3 panicked "$T/rimed.log")"
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
