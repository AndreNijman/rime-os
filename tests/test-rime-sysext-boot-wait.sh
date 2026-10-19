#!/usr/bin/env bash
# rime-sysext-rebuild.service must not put the network on the boot critical
# path: no network-online.target ordering in the unit, and rime-pkg waits for
# the network itself only when a rebuild really has to download.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
UNIT=files/system/units/rime-sysext-rebuild.service
ENGINE=files/system/libexec/rime-pkg
pass=0 fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

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
call="$(grep -n '^        wait_for_boot_network$' "$ENGINE" | head -1 | cut -d: -f1)"
if [ -n "$fast" ] && [ -n "$call" ] && [ "$call" -gt "$fast" ]; then
    ok "the network wait comes after the up-to-date early return"
else
    bad "the network wait comes after the up-to-date early return" "fast=$fast call=$call"
fi

# Behaviour of the function itself, with systemctl/timeout stubbed.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
printf '#!/bin/sh\nshift; exec "$@"\n' > "$tmp/bin/timeout"
printf '#!/bin/sh\necho "$*" >> "%s/calls"\n' "$tmp" > "$tmp/bin/systemctl"
chmod +x "$tmp/bin/"*
body="$(sed -n '/^wait_for_boot_network() {/,/^}/p' "$ENGINE")"
run() { : > "$tmp/calls"; ( msg() { :; }; eval "$body"; export PATH="$tmp/bin:$PATH"; "$@"; wait_for_boot_network ) ; cat "$tmp/calls"; }
out="$(INVOCATION_ID=x run true)"
if [ "$out" = "start network-online.target" ]; then ok "inside a service it starts network-online.target"; else bad "inside a service it starts network-online.target" "got: $out"; fi
out="$(env -u INVOCATION_ID bash -c "$(declare -f run); tmp=$tmp; body=$(printf %q "$body"); run true")"
if [ -z "$out" ]; then ok "outside a service it starts nothing"; else bad "outside a service it starts nothing" "got: $out"; fi

printf 'rime-sysext-boot-wait: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
