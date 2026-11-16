#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-wifi-resume.sh — the MT7925 resume repair
#  (files/system/system-sleep/rime-wifi-resume) picks the card by its driver
#  and judges that card's own link.
#
#  Regression: the hook took the FIRST wl* name. A USB Wi-Fi adapter
#  (wlp197s0f3u1, rtl8xxxu) sorts before the built-in card (wlp3s0, mt7925e),
#  so the hook saw a driver it does not repair and exited: no repair after any
#  lid-open for as long as the adapter was plugged in.
#
#  Fixture sysfs trees (RIME_SYSFS); systemd-run, ip, ping, nmcli, modprobe,
#  logger and sleep are stubs on PATH that record what they were asked.
#
#  Run from anywhere:  ./tests/test-rime-wifi-resume.sh
# ─────────────────────────────────────────────────────────────────────────────
# shellcheck disable=SC2015  # ok && pass || fail: ok never fails
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOK="${ROOT}/files/system/system-sleep/rime-wifi-resume"

pass=0
fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); [ -n "${2:-}" ] && printf '      %s\n' "$2"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-wifi-resume-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# ── Fixture sysfs: interface -> device/driver symlink, as the kernel lays it out
iface() {  # root name driver
    mkdir -p "$1/bus/drivers/$3" "$1/devices/$2" "$1/class/net/$2"
    ln -s "$1/bus/drivers/$3" "$1/devices/$2/driver"
    ln -s "$1/devices/$2" "$1/class/net/$2/device"
}
BOTH="${WORK}/sys-both";    iface "$BOTH" wlp197s0f3u1 rtl8xxxu; iface "$BOTH" wlp3s0 mt7925e
ONLY="${WORK}/sys-l16";     iface "$ONLY" wlp3s0 mt7925e
DONGLE="${WORK}/sys-dongle"; iface "$DONGLE" wlp197s0f3u1 rtl8xxxu
NONE="${WORK}/sys-none";    mkdir -p "$NONE/class/net"

# ── Stubs. ROUTES = "dev=gw ..." (the default route each interface has);
#    PING_RC = what ping returns.
BIN="${WORK}/bin"; mkdir -p "$BIN"
LOG="${WORK}/calls"
for t in systemd-run nmcli modprobe logger sleep; do
    printf '#!/usr/bin/env bash\nprintf "%%s %%s\\n" "%s" "$*" >> "%s"\n' "$t" "$LOG" > "$BIN/$t"
done
cat > "$BIN/ip" <<EOF
#!/usr/bin/env bash
printf 'ip %s\n' "\$*" >> "$LOG"
dev=""; prev=""
for a in "\$@"; do [ "\$prev" = dev ] && dev="\$a"; prev="\$a"; done
for r in \${ROUTES:-}; do
    d="\${r%%=*}"; g="\${r#*=}"
    if [ -z "\$dev" ] || [ "\$dev" = "\$d" ]; then
        echo "default via \$g dev \$d proto dhcp metric 600"
        [ -n "\$dev" ] && exit 0
    fi
done
EOF
cat > "$BIN/ping" <<EOF
#!/usr/bin/env bash
printf 'ping %s\n' "\$*" >> "$LOG"
exit "\${PING_RC:-0}"
EOF
chmod +x "$BIN"/*

run() {  # sysfs phase [VAR=value...]
    local sys="$1" phase="$2"; shift 2
    : > "$LOG"
    env PATH="$BIN:$PATH" RIME_SYSFS="$sys" "$@" bash "$HOOK" "$phase" >/dev/null 2>&1
}
called() { grep -q -- "$1" "$LOG"; }

# ── post: which card starts the probe ───────────────────────────────────────
run "$BOTH" post
called "^systemd-run .*--unit=rime-wifi-resume-probe .* probe$" \
    && ok "USB adapter sorted first: the MT7925 card still gets its resume probe" \
    || bad "USB adapter sorted first" "calls: $(tr '\n' '|' < "$LOG")"

run "$ONLY" post
called "^systemd-run .* probe$" && ok "MT7925 alone: probe started" \
    || bad "MT7925 alone" "calls: $(tr '\n' '|' < "$LOG")"

run "$DONGLE" post
! called "^systemd-run" && ok "only a USB adapter: nothing to repair, no probe" \
    || bad "only a USB adapter" "calls: $(tr '\n' '|' < "$LOG")"

run "$NONE" post
! called "^systemd-run" && ok "no Wi-Fi at all: no probe" \
    || bad "no Wi-Fi" "calls: $(tr '\n' '|' < "$LOG")"

run "$BOTH" pre
[ ! -s "$LOG" ] && ok "pre (going to sleep): does nothing" \
    || bad "pre phase" "calls: $(tr '\n' '|' < "$LOG")"

# ── probe: the card's own route and link ────────────────────────────────────
run "$BOTH" probe ROUTES="wlp197s0f3u1=10.9.9.1" PING_RC=1
! called "^ping" && ! called "^modprobe" \
    && ok "default route only through the USB adapter: the MT7925 is not judged by it, no reload" \
    || bad "route through the adapter" "calls: $(tr '\n' '|' < "$LOG")"
called "^logger .*no default route on wlp3s0" && ok "  and the journal says which card had no route" \
    || bad "  journal names the card" "calls: $(tr '\n' '|' < "$LOG")"

run "$BOTH" probe ROUTES="wlp197s0f3u1=10.9.9.1 wlp3s0=192.168.1.1" PING_RC=1
called "^ping -I wlp3s0 .*192.168.1.1$" && ok "failing MT7925 link: pinged through wlp3s0 to its own gateway" \
    || bad "ping through the card" "calls: $(tr '\n' '|' < "$LOG")"
called "^modprobe -r mt7925e" && called "^modprobe mt7925e$" && ok "  and the driver is reloaded" \
    || bad "  driver reloaded" "calls: $(tr '\n' '|' < "$LOG")"

run "$BOTH" probe ROUTES="wlp197s0f3u1=10.9.9.1 wlp3s0=192.168.1.1" PING_RC=0
called "^ping -I wlp3s0 " && ! called "^modprobe" && ok "healthy MT7925 link: no reload" \
    || bad "healthy link" "calls: $(tr '\n' '|' < "$LOG")"

# ── the shipped copy is the same script ─────────────────────────────────────
bash -n "$HOOK" && ok "bash -n" || bad "bash -n"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
