#!/usr/bin/env bash
# rime-desktop-menu: when the Shell does not answer, the helper restarts it and then polls until the menu opens.
# rime-shell-autostart execs quickshell, so it returns only when the Shell exits: the helper must start it detached,
# or the poll never runs while the restarted Shell lives (the right-click handler would hang for the Shell's life).
set -uo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
PROG=$REPO/files/system/libexec/rime-desktop-menu
W=$(mktemp -d); trap 'kill $(cat "$W"/pids 2>/dev/null) 2>/dev/null; rm -rf "$W"' EXIT
pass=0 fail=0
ok() { echo "PASS  $1"; pass=$((pass + 1)); }
no() { echo "FAIL  $1"; fail=$((fail + 1)); }

mkdir -p "$W/bin"
# `rime shell ipc context-menu open`: answers once the fake Shell has claimed its socket (the marker), and records it.
cat > "$W/bin/rime" <<STUB
#!/bin/sh
[ -e "$W/up" ] || exit 1
echo opened >> "$W/menu"
STUB
# The launcher: like the real one, it becomes the Shell (blocks for the Shell's life). It claims the socket after a
# short start-up delay, then lives on.
cat > "$W/autostart" <<STUB
#!/bin/sh
echo \$\$ >> "$W/pids"
echo started >> "$W/starts"
sleep \${FAKE_STARTUP:-0.6}
touch "$W/up"
exec sleep 60
STUB
cat > "$W/bin/notify-send" <<STUB
#!/bin/sh
echo "\$*" >> "$W/notified"
STUB
chmod +x "$W/bin/rime" "$W/autostart" "$W/bin/notify-send"

run() { rm -f "$W/up" "$W/menu" "$W/starts" "$W/notified"
  PATH="$W/bin:$PATH" RIME_SHELL_AUTOSTART="$W/autostart" timeout 20 "$PROG"; echo $? > "$W/rc"; }

# 1. Shell answering: menu opens, no restart.
touch "$W/up"; PATH="$W/bin:$PATH" RIME_SHELL_AUTOSTART="$W/autostart" timeout 5 "$PROG"; rc=$?
[ $rc = 0 ] && [ ! -e "$W/starts" ] && ok "a Shell that answers gets the menu, no restart" || no "answering Shell (rc=$rc)"

# 2. Shell down: restarted detached, the poll opens the menu while the restarted Shell keeps running.
t0=$(date +%s%N); run; rc=$(cat "$W/rc"); ms=$(( ($(date +%s%N) - t0) / 1000000 ))
[ "$rc" = 0 ] && ok "a dead Shell is restarted and the menu opens (rc 0, ${ms} ms)" || no "restart path rc=$rc (124 = the helper waited on the Shell)"
[ "$(wc -l < "$W/starts" 2>/dev/null)" = 1 ] && ok "the launcher ran exactly once" || no "launcher runs: $(cat "$W/starts" 2>/dev/null | wc -l)"
[ -s "$W/menu" ] && ok "the menu was opened after the restart" || no "menu never opened"
[ "$ms" -lt 10000 ] && ok "the helper returned while the restarted Shell lives" || no "the helper took ${ms} ms"
[ ! -e "$W/notified" ] && ok "no failure notice when the restart worked" || no "notice shown: $(cat "$W/notified")"
pid=$(tail -1 "$W/pids"); kill -0 "$pid" 2>/dev/null && ok "the restarted Shell is still running after the helper exits" || no "the restarted Shell died with the helper"

# 3. A Shell that never comes up: the helper gives up within its poll, says so, exits 1.
rm -f "$W/up"
PATH="$W/bin:$PATH" RIME_SHELL_AUTOSTART=/bin/true timeout 30 "$PROG"; rc=$?
[ "$rc" = 1 ] && grep -q "not running" "$W/notified" 2>/dev/null && ok "a Shell that never answers: notice and exit 1" || no "never-up path rc=$rc"

echo
echo "rime-desktop-menu: $pass passed, $fail failed"
[ "$fail" = 0 ]
