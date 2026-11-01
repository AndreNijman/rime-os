#!/bin/bash
# Remove the isolated katana runners and put today's runner back as it was.
#
#   sudo ci/runner/uninstall.sh            remove units, scripts, network, firewall table; keep caches
#   sudo ci/runner/uninstall.sh --purge    also delete /var/lab/rime-runner (images, Nix store)
#
# Restores apex-github-runner.service to the state install.sh recorded in
# /etc/rime-runner/legacy-state (enabled + active on katana today). With no
# record it enables and starts it, which is katana's state on 2026-11-01.
# Never touches /etc/apex-runner, the ghrunner user, the boot chain or /var/lab/runner.
set -euo pipefail

LEGACY=apex-github-runner.service
ETC=/etc/rime-runner
PREFIX=/usr/local/libexec/rime-runner
purge=0
[ "${1:-}" = --purge ] && purge=1
say() { printf 'uninstall: %s\n' "$*"; }
[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }

conf=$ETC/runner.conf
[ -r "$conf" ] || conf=$(dirname "$(readlink -f "$0")")/runner.conf
# shellcheck source=runner.conf
. "$conf"

# ── 1. stop everything new ─────────────────────────────────────────────────
mapfile -t slots < <(systemctl list-units --all --plain --no-legend 'rime-runner@*.service' | awk '{print $1}')
for u in "${slots[@]}" rime-runner-builder.service rime-runner-firewall.service; do
	[ -n "$u" ] || continue
	say "stopping $u"
	systemctl disable --now "$u" 2>/dev/null || true
done
for i in $(seq 0 31); do systemctl disable "rime-runner@$i.service" >/dev/null 2>&1 || true; done
nft delete table inet rime_runner 2>/dev/null || true

# ── 2. deregister leftover JIT runners (best effort) ───────────────────────
if [ -s "${RIME_RUNNER_TOKEN_FILE:-/nonexistent}" ]; then
	hdr() { printf 'Authorization: Bearer %s\n' "$(<"$RIME_RUNNER_TOKEN_FILE")"; }
	ids=$(curl -fsS -H @<(hdr) -H "Accept: application/vnd.github+json" \
		"https://api.github.com/repos/$RIME_RUNNER_REPO/actions/runners?per_page=100" 2>/dev/null |
		python3 -c 'import json,sys
for r in json.load(sys.stdin).get("runners", []):
    if r["name"].startswith("katana-nix-"): print(r["id"])' 2>/dev/null || true)
	for id in $ids; do
		say "deregistering runner $id"
		curl -fsS -X DELETE -H @<(hdr) -H "Accept: application/vnd.github+json" \
			"https://api.github.com/repos/$RIME_RUNNER_REPO/actions/runners/$id" >/dev/null 2>&1 || true
	done
fi

# ── 3. remove units, scripts, network ──────────────────────────────────────
rm -f /etc/systemd/system/rime-runner.slice /etc/systemd/system/rime-runner-builder.service \
	/etc/systemd/system/rime-runner@.service /etc/systemd/system/rime-runner-firewall.service
rm -rf /etc/systemd/system/rime-runner.slice.d /etc/systemd/system.control/rime-runner.slice.d
systemctl daemon-reload
if [ -d "${RIME_RUNNER_STATE:-/nonexistent}/podman" ]; then
	env CONTAINERS_STORAGE_CONF="$RIME_RUNNER_STATE/podman/storage.conf" \
		CONTAINERS_CONF_OVERRIDE="$RIME_RUNNER_STATE/podman/containers.conf" \
		podman rm -fa >/dev/null 2>&1 || true
fi
podman network rm "${RIME_RUNNER_NETWORK:-rime-runner}" >/dev/null 2>&1 || true
rm -f /usr/local/sbin/rime-runner-ctl /run/rime-runner/paused
rm -rf "$PREFIX" /run/rime-runner
if command -v semanage >/dev/null; then
	semanage fcontext -d "$RIME_RUNNER_NIX/var/nix/daemon-socket(/.*)?" 2>/dev/null || true
	semanage fcontext -d "$RIME_RUNNER_NIX(/.*)?" 2>/dev/null || true
fi
if [ "$purge" = 1 ]; then
	say "purging $RIME_RUNNER_STATE"
	rm -rf --one-file-system "$RIME_RUNNER_STATE"
fi

# ── 4. today's runner, as it was ───────────────────────────────────────────
en=enabled
ac=active
if [ -r "$ETC/legacy-state" ]; then
	en=$(sed -n 's/^enabled=//p' "$ETC/legacy-state")
	ac=$(sed -n 's/^active=//p' "$ETC/legacy-state")
fi
if [ -e /etc/systemd/system/$LEGACY ]; then
	case $en in enabled) systemctl enable "$LEGACY" ;; esac
	case $ac in active) systemctl start "$LEGACY" ;; esac
	say "$LEGACY: enabled=$(systemctl is-enabled $LEGACY 2>/dev/null || true) active=$(systemctl is-active $LEGACY 2>/dev/null || true) (recorded: enabled=$en active=$ac)"
else
	say "WARNING: /etc/systemd/system/$LEGACY does not exist; nothing to restore"
fi

# The token and the recorded state go last, once the legacy runner is back.
rm -f "$ETC/legacy-state" "$ETC/nftables.conf" "$ETC/runner.conf"
if [ "$purge" = 1 ]; then rm -rf "$ETC"; else say "kept $ETC/gh-token (if any); --purge removes it"; fi
say "done"
