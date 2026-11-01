#!/usr/bin/env bash
# rime-sysext-rebuild.service must not put the network on the boot critical
# path: no network-online.target ordering in the unit, and rime-pkg waits for
# the network itself only when a rebuild really has to download.
#
# And a rebuild that CANNOT happen must not hold the desktop. A boot that needs
# a rebuild still waits for it (that is the design), but offline after the
# network wait, or a package set that does not fit the image, ends the unit at
# once with a non-zero status and a note the session turns into a notice
# (rime-pkg-notice). Measured 2026-11-01: a refused boot rebuild reported
# "Finished" after 12 min 37 s, 11 minutes of it a second download of the whole
# set at the newest builds, made only to name the package in the way.
#
# The behaviour cases run the engine's own functions (extracted with sed, as
# the network-wait case always did) against stubs: dnf5's two passes are
# download_set, whose second, diagnostic pass STALLS. Then the same cases run
# against three broken copies of the engine and must fail: a refusal that
# stalls, a refusal that reports success, and an offline boot that downloads.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
UNIT=files/system/units/rime-sysext-rebuild.service
ENGINE=files/system/libexec/rime-pkg
NOTICE=files/system/libexec/rime-pkg-notice
NSVC=files/system/units/rime-pkg-notice.service
NTIMER=files/system/units/rime-pkg-notice.timer
pass=0 fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }
for f in "$UNIT" "$ENGINE" "$NOTICE" "$NSVC" "$NTIMER"; do
    [ -f "$f" ] || { echo "FATAL: missing $f" >&2; exit 1; }
done

if grep -E '^(After|Wants|Requires)=.*network-online' "$UNIT" >/dev/null; then
    bad "unit is not ordered after network-online.target" "the 8 s wait-online is back on every boot"
else
    ok "unit is not ordered after network-online.target"
fi
if grep -E '^After=.*systemd-sysext.service' "$UNIT" >/dev/null; then
    ok "unit still runs after systemd-sysext.service"
else
    bad "unit still runs after systemd-sysext.service" "the merge-state check would race the merge"
fi

# The wait sits after the up-to-date return, so a normal boot never reaches it.
fast="$(grep -n 'twins_unchanged "\$STATE"; then' "$ENGINE" | head -1 | cut -d: -f1)"
call="$(grep -n '^        if ! wait_for_boot_network; then$' "$ENGINE" | head -1 | cut -d: -f1)"
if [ -n "$fast" ] && [ -n "$call" ] && [ "$call" -gt "$fast" ]; then
    ok "the network wait comes after the up-to-date early return"
else
    bad "the network wait comes after the up-to-date early return" "fast=$fast call=$call"
fi

# Behaviour of the function itself, with systemctl/timeout/ip stubbed.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
printf '#!/bin/sh\necho "timeout $1" >> "%s/calls"\nshift; exec "$@"\n' "$tmp" > "$tmp/bin/timeout"
printf '#!/bin/sh\necho "$*" >> "%s/calls"\n' "$tmp" > "$tmp/bin/systemctl"
# A default route exists while $tmp/online does.
printf '#!/bin/sh\n[ -e "%s/online" ] && [ "$*" = "-4 route show default" ] && echo "default via 192.0.2.1 dev wlan0"\nexit 0\n' "$tmp" > "$tmp/bin/ip"
chmod +x "$tmp/bin/"*
: > "$tmp/online"
body="$(sed -n '/^wait_for_boot_network() {/,/^}/p;/^boot_network_up() {/,/^}/p' "$ENGINE")"
run() { : > "$tmp/calls"; ( msg() { :; }; eval "$body"; export PATH="$tmp/bin:$PATH"; "$@"; wait_for_boot_network ) ; cat "$tmp/calls"; }
out="$(INVOCATION_ID=x run true)"
want="$(printf 'timeout 90\nstart network-online.target')"
if [ "$out" = "$want" ]; then ok "inside a service it starts network-online.target, bounded at 90 s"; else bad "inside a service it starts network-online.target, bounded at 90 s" "got: $out"; fi
out="$(env -u INVOCATION_ID bash -c "$(declare -f run); tmp=$tmp; body=$(printf %q "$body"); run true")"
if [ -z "$out" ]; then ok "outside a service it starts nothing"; else bad "outside a service it starts nothing" "got: $out"; fi

# ── the refusal path: offline and refused end at once, non-zero, with a note ──
# The unit's failure must be visible and the note must survive it.
if grep -E '^SuccessExitStatus=' "$UNIT" >/dev/null; then
    bad "the unit does not turn a failed rebuild into success" "SuccessExitStatus= is set"
else
    ok "the unit does not turn a failed rebuild into success"
fi
if grep -x 'RuntimeDirectory=rime-pkg' "$UNIT" >/dev/null && grep -x 'RuntimeDirectoryMode=0755' "$UNIT" >/dev/null \
   && grep -x 'RuntimeDirectoryPreserve=yes' "$UNIT" >/dev/null; then
    ok "the note's directory is world-readable and outlives the failed unit"
else
    bad "the note's directory is world-readable and outlives the failed unit" "RuntimeDirectory=rime-pkg, Mode=0755, Preserve=yes"
fi
# Every successful exit of the one build function takes the note back. Only a
# line that IS the call counts: `: rebuild_notice_clear` or a comment naming it
# clears nothing.
clears="$(sed -n '/^rebuild_extension() {/,/^}/p' "$ENGINE" | grep -cx '[[:space:]]*rebuild_notice_clear')"
if [ "$clears" -ge 3 ]; then
    ok "every successful build removes the note ($clears exits)"
else
    bad "every successful build removes the note" "rebuild_notice_clear appears $clears times in rebuild_extension, want 3"
fi

# One case, against one copy of the engine. Prints
#   rc=<n> secs=<n> calls=<pinned,unpinned,...> rebuilt=<yes|no> waited=<yes|no> note=<reason|none>
cat > "$tmp/case.sh" <<'CASE_EOF'
set -uo pipefail
engine="$1" case="$2" tmp="$3"
w="$tmp/w.$case"; rm -rf "$w"; mkdir -p "$w"
# The functions under test, from the engine as it is. Everything they call that
# is not under test is stubbed below.
eval "$(sed -n '/^cmd_rebuild() {/,/^}/p;/^wait_for_boot_network() {/,/^}/p;/^boot_network_up() {/,/^}/p;/^rebuild_notice_set() {/,/^}/p;/^rebuild_notice_clear() {/,/^}/p;/^download_rpms() {/,/^}/p' "$engine")"
msg()  { :; }
warn() { :; }
die()  { exit 1; }
need_root() { :; }
take_lock() { :; }
os_version() { printf 45; }
merged() { return 0; }
twins_unchanged() { return 0; }
load_requested() { echo steam; }
image_pin_excludes() { echo 'git-2.56.0-1.fc45.x86_64'; }
rpm_cache_open() { return 1; }
# dnf5's resolving download. The pinned pass fails (nothing fits the image);
# the unpinned pass is the diagnostic one, and it stalls the way a whole-set
# download plus a guard pass over every package does.
download_set() {
    case " $* " in
        *" --exclude="*) echo pinned >> "$w/dl"; return 1 ;;
        *) echo unpinned >> "$w/dl"; sleep "${STALL:-0}"; return 1 ;;
    esac
}
rebuild_extension() {
    if [ "$case" = ok ]; then : > "$w/rebuilt"; return 0; fi
    : > "$w/rebuilt"
    download_rpms "$w/rpms" "$@"
}
PKG_COMPAT_LEVEL=3
BOOT_FAST_FAIL=0
RPM_SET_REFUSED=""
NO_WEAK_DEPS=0
ENABLE_REPOS=""
STATE="$w/state.json"
REBUILD_NOTICE="$w/run/rime-pkg/rebuild"
mkdir -p "$w/rpms"
# Built one compat level ago: the boot NEEDS a rebuild.
level=2; [ "$case" = current ] && level=3
printf '{"os_version_id":"45","pkg_compat_level":%s}\n' "$level" > "$STATE"
mkdir -p "$w/bin"
printf '#!/bin/sh\necho "$*" >> "%s/systemctl"\n' "$w" > "$w/bin/systemctl"
printf '#!/bin/sh\nshift; exec "$@"\n' > "$w/bin/timeout"
if [ "$case" = offline ]; then
    printf '#!/bin/sh\nexit 0\n' > "$w/bin/ip"
elif [ "$case" = v6 ]; then
    # An IPv6-only network: no IPv4 default route at all.
    printf '#!/bin/sh\n[ "$*" = "-6 route show default" ] && echo "default via fe80::1 dev wlan0 proto ra"\nexit 0\n' > "$w/bin/ip"
else
    printf '#!/bin/sh\n[ "$*" = "-4 route show default" ] && echo "default via 192.0.2.1 dev wlan0"\nexit 0\n' > "$w/bin/ip"
fi
chmod +x "$w/bin/"*
export PATH="$w/bin:$PATH"
if [ "$case" = interactive ]; then unset INVOCATION_ID; else export INVOCATION_ID=test; fi
# A note left from before a soft-reboot (/run survives one): an up-to-date boot
# must take it back.
if [ "$case" = current ]; then mkdir -p "$w/run/rime-pkg"; printf 'state=failed\nreason=refused\nboot=old\n' > "$REBUILD_NOTICE"; fi
start=$SECONDS
( cmd_rebuild --if-needed ) >/dev/null 2>&1; rc=$?
secs=$((SECONDS - start))
calls="$(paste -sd, "$w/dl" 2>/dev/null)"
note=none
[ -f "$REBUILD_NOTICE" ] && note="$(sed -n 's/^reason=//p' "$REBUILD_NOTICE")"
rebuilt=no; [ -e "$w/rebuilt" ] && rebuilt=yes
waited=no; [ -s "$w/systemctl" ] && waited=yes
echo "rc=$rc secs=$secs calls=${calls:-none} rebuilt=$rebuilt waited=$waited note=$note"
CASE_EOF

STALL=6
one() { STALL=$STALL bash "$tmp/case.sh" "$1" "$2" "$tmp"; }
field() { local k; for k in $1; do case "$k" in "$2="*) printf '%s' "${k#*=}"; return;; esac; done; }

# check <engine> <label prefix>: runs every case, prints PASS/FAIL lines via the
# callback given, so the same assertions judge the real engine and the mutants.
judge() {  # $1 engine; prints one "ok|bad <name> <why>" line per assertion
    local e=$1 r
    r="$(one "$e" refused)"
    [ "$(field "$r" rc)" != 0 ] && echo "ok refused-nonzero" || echo "bad refused-nonzero $r"
    [ "$(field "$r" secs)" -lt 3 ] && echo "ok refused-at-once" || echo "bad refused-at-once $r"
    [ "$(field "$r" calls)" = pinned ] && echo "ok refused-no-second-download" || echo "bad refused-no-second-download $r"
    [ "$(field "$r" note)" = refused ] && echo "ok refused-note" || echo "bad refused-note $r"
    [ "$(field "$r" waited)" = yes ] && echo "ok refused-waited-for-network" || echo "bad refused-waited-for-network $r"
    r="$(one "$e" offline)"
    [ "$(field "$r" rc)" != 0 ] && echo "ok offline-nonzero" || echo "bad offline-nonzero $r"
    [ "$(field "$r" rebuilt)" = no ] && [ "$(field "$r" calls)" = none ] && echo "ok offline-downloads-nothing" || echo "bad offline-downloads-nothing $r"
    [ "$(field "$r" note)" = offline ] && echo "ok offline-note" || echo "bad offline-note $r"
    # Only an IPv6 default route is still online: the build starts (and is
    # refused by the stub), it is not reported as offline.
    r="$(one "$e" v6)"
    [ "$(field "$r" calls)" = pinned ] && [ "$(field "$r" note)" = refused ] \
        && echo "ok ipv6-only-is-online" || echo "bad ipv6-only-is-online $r"
    r="$(one "$e" ok)"
    [ "$(field "$r" rc)" = 0 ] && [ "$(field "$r" rebuilt)" = yes ] && echo "ok needed-rebuild-succeeds" || echo "bad needed-rebuild-succeeds $r"
    [ "$(field "$r" waited)" = yes ] && echo "ok needed-rebuild-waits-for-network" || echo "bad needed-rebuild-waits-for-network $r"
    [ "$(field "$r" note)" = none ] && echo "ok needed-rebuild-no-note" || echo "bad needed-rebuild-no-note $r"
    r="$(one "$e" current)"
    [ "$(field "$r" rc)" = 0 ] && [ "$(field "$r" waited)" = no ] && [ "$(field "$r" rebuilt)" = no ] \
        && echo "ok up-to-date-boot-does-nothing" || echo "bad up-to-date-boot-does-nothing $r"
    [ "$(field "$r" note)" = none ] && echo "ok up-to-date-boot-clears-note" || echo "bad up-to-date-boot-clears-note $r"
    # A person running it gets the long diagnosis: the gate is the boot's only.
    r="$(STALL=0 one "$e" interactive)"
    [ "$(field "$r" calls)" = pinned,unpinned ] && echo "ok interactive-keeps-diagnosis" || echo "bad interactive-keeps-diagnosis $r"
    [ "$(field "$r" rc)" != 0 ] && echo "ok interactive-refusal-nonzero" || echo "bad interactive-refusal-nonzero $r"
}

declare -A what=(
    [refused-nonzero]="a refused boot rebuild exits non-zero"
    [refused-at-once]="a refused boot rebuild exits at once (no stalled second pass)"
    [refused-no-second-download]="a refused boot rebuild does not download the set again to diagnose it"
    [refused-note]="a refused boot rebuild leaves the note reason=refused"
    [refused-waited-for-network]="a needed rebuild still waits for the network before resolving"
    [offline-nonzero]="an offline boot rebuild exits non-zero"
    [offline-downloads-nothing]="an offline boot rebuild never starts the build"
    [offline-note]="an offline boot rebuild leaves the note reason=offline"
    [ipv6-only-is-online]="an IPv6-only default route counts as online (the build starts)"
    [needed-rebuild-succeeds]="a needed rebuild that works exits 0"
    [needed-rebuild-waits-for-network]="a needed rebuild that works waited for the network first"
    [needed-rebuild-no-note]="a needed rebuild that works leaves no note"
    [up-to-date-boot-does-nothing]="an up-to-date boot waits for nothing and builds nothing"
    [up-to-date-boot-clears-note]="an up-to-date boot takes back a note left from before a soft-reboot"
    [interactive-keeps-diagnosis]="outside the boot service a refusal still runs the diagnostic pass"
    [interactive-refusal-nonzero]="outside the boot service a failed --if-needed exits non-zero"
)
while read -r verdict name why; do
    if [ "$verdict" = ok ]; then ok "${what[$name]}"; else bad "${what[$name]}" "$why"; fi
done < <(judge "$ENGINE")

# ── mutants: each breaks one promise and must be caught ─────────────────────
mutant() {  # $1 label, $2 the assertion that must now fail, $3 sed expression
    local label=$1 must=$2 expr=$3 m="$tmp/mutant.$1"
    sed -e "$expr" "$ENGINE" > "$m"
    if cmp -s "$m" "$ENGINE"; then
        bad "mutant '$label' is caught" "the sed did not change the engine; the mutant is stale"
        return
    fi
    if judge "$m" | grep -E "^bad $must( |$)" >/dev/null; then
        ok "mutant '$label' is caught ($must)"
    else
        bad "mutant '$label' is caught" "every assertion still passed against it"
    fi
}
# The boot rebuild takes the diagnostic second download again (the 12-minute stall).
mutant stall refused-at-once 's/if \[ "\${BOOT_FAST_FAIL:-0}" = 1 \]; then/if false; then/'
# A refused rebuild reports success again ("Finished").
mutant finished refused-nonzero '/rebuild_notice_set refused/{n;s/return 1/return 0/}'
# Offline is not measured: the wait returns success and the build starts.
mutant offline-builds offline-downloads-nothing 's/^    boot_network_up$/    true/'
# Only IPv4 routes are looked at: an IPv6-only network is called offline.
mutant ipv4-only ipv6-only-is-online 's/\$(ip -6 route show default 2>\/dev\/null)//'

# ── the notice: once per boot, in the session, never for a rebuild that worked
nb="$tmp/notice"; mkdir -p "$nb/bin" "$nb/home"
printf '#!/bin/sh\n[ -e "%s/notify-fails" ] && exit 1\nprintf "%%s\\n" "$*" >> "%s/notified"\n' "$nb" "$nb" > "$nb/bin/notify-send"
chmod +x "$nb/bin/notify-send"
note() { RIME_PKG_REBUILD_NOTICE="$nb/rebuild" HOME="$nb/home" XDG_STATE_HOME="$nb/home/state" PATH="$nb/bin:$PATH" bash "$NOTICE" >/dev/null 2>&1; }
count() { if [ -f "$nb/notified" ]; then wc -l < "$nb/notified"; else echo 0; fi; }
note
[ "$(count)" = 0 ] && ok "notice: no note, nothing said" || bad "notice: no note, nothing said" "$(count)"
printf 'state=failed\nreason=offline\nboot=b1\n' > "$nb/rebuild"; note
if [ "$(count)" = 1 ] && grep -F 'offline' "$nb/notified" >/dev/null && grep -F 'sudo rime pkg rebuild' "$nb/notified" >/dev/null; then
    ok "notice: an offline boot rebuild is said, with the command"
else
    bad "notice: an offline boot rebuild is said, with the command" "$(cat "$nb/notified" 2>/dev/null)"
fi
note
[ "$(count)" = 1 ] && ok "notice: not again in the same boot" || bad "notice: not again in the same boot" "$(count)"
printf 'state=failed\nreason=offline\nboot=b2\n' > "$nb/rebuild"; note
[ "$(count)" = 2 ] && ok "notice: said again on the next boot" || bad "notice: said again on the next boot" "$(count)"
printf 'state=failed\nreason=refused\nboot=b3\n' > "$nb/rebuild"; : > "$nb/notify-fails"; note; rm -f "$nb/notify-fails"
[ "$(count)" = 2 ] || bad "notice: a failed notify-send is not recorded as said" "it was shown anyway: $(count)"
note
if [ "$(count)" = 3 ] && tail -1 "$nb/notified" | grep -F 'which package' >/dev/null; then
    ok "notice: a failed notify-send is not recorded as said; a refusal says how to find the package"
else
    bad "notice: a failed notify-send is not recorded as said; a refusal says how to find the package" "$(tail -1 "$nb/notified")"
fi
printf 'state=failed\nreason=bogus\nboot=b4\n' > "$nb/rebuild"; note
printf 'state=failed\nreason=offline\n' > "$nb/rebuild"; note
[ "$(count)" = 3 ] && ok "notice: an unknown reason or a half-written note says nothing" || bad "notice: an unknown reason or a half-written note says nothing" "$(count)"
# A user unit started by a timer, conditioned on the note, never a system unit.
if grep -x 'ConditionPathExists=/run/rime-pkg/rebuild' "$NSVC" >/dev/null && grep -x 'OnStartupSec=2min' "$NTIMER" >/dev/null \
   && grep -F '/usr/lib/systemd/user/rime-pkg-notice.timer' Containerfile.base >/dev/null \
   && grep -F 'systemctl --global enable rime-pkg-notice.timer' Containerfile.base >/dev/null; then
    ok "notice: a user timer two minutes into the session, only when there is a note"
else
    bad "notice: a user timer two minutes into the session, only when there is a note" "unit or Containerfile.base drifted"
fi
if grep -F 'rebuild' "$ENGINE" >/dev/null && grep -F 'RIME_PKG_REBUILD_NOTICE-/run/rime-pkg/rebuild' "$ENGINE" >/dev/null; then
    ok "notice and engine agree on the note's path"
else
    bad "notice and engine agree on the note's path" "rime-pkg's REBUILD_NOTICE default moved"
fi

# A success exit that no longer takes the note back must fail the count above.
m="$tmp/mutant.no-clear"
sed -e '0,/^    rebuild_notice_clear$/s//    : rebuild_notice_clear/' "$ENGINE" > "$m"
if cmp -s "$m" "$ENGINE"; then
    bad "mutant 'no-clear' is caught" "the sed did not change the engine; the mutant is stale"
elif [ "$(sed -n '/^rebuild_extension() {/,/^}/p' "$m" | grep -cx '[[:space:]]*rebuild_notice_clear')" -lt 3 ]; then
    ok "mutant 'no-clear' is caught (a success exit that keeps the note)"
else
    bad "mutant 'no-clear' is caught" "the count still reached 3"
fi

printf 'rime-sysext-boot-wait: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
