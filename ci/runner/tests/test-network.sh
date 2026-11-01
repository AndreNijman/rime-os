#!/bin/bash
# Live network test for the job containers: the firewall template and the
# containers' IPv6 switch, under real traffic. Needs root (sudo) and the images
# test-isolation.sh builds (run that first).
#
#   sudo ci/runner/tests/test-network.sh
#
# Registers nothing with GitHub and starts no systemd unit. It does create, for
# the length of the run, a podman network of its own (rime-runner-nettest,
# 10.89.78.0/24, bridge rr-nettest0) and an nft table of its own
# (inet rime_runner_nettest, rendered from nftables.conf.in, matching only that
# subnet and that bridge); an EXIT trap removes both. The production network,
# table and units are not touched.
#
# Four runs against the same host targets (the gateway's sshd over IPv4, the
# bridge's fe80:: address over IPv6, katana's LAN address, the LAN router):
#   A  no table, no IPv6 switch  -> sshd reachable both ways: the probe can see a hole
#   B  table only                -> both closed: nft alone covers IPv4 and IPv6
#   C  production job_args + table -> everything on katana and the LAN closed;
#                                    public DNS and HTTPS still work
#   D  production job_args only  -> IPv6 closed by the container's own switch
#   E  the builder (production builder_args) + table -> same walls, and it
#                                    still reaches cache.nixos.org (substitutes, fetches)
set -euo pipefail

HERE=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
export RIME_RUNNER_HOME=$HERE
export RIME_RUNNER_CONF=$HERE/runner.conf
# shellcheck source=../lib/common.sh
. "$HERE/lib/common.sh"

[ "$(id -u)" = 0 ] || rr_die "run with sudo"

DRY=/var/lab/rime-runner-dryrun
# shellcheck disable=SC2034 # read by the lib/common.sh functions
{
	RIME_RUNNER_STATE=$DRY
	RIME_RUNNER_STORAGE=$DRY/containers
	RIME_RUNNER_RUNROOT=/run/rime-runner-dryrun/containers
	RIME_RUNNER_NIX=$DRY/nix
}
unset RIME_RUNNER_CGROUP_PARENT
NET=rime-runner-nettest
SUBNET=10.89.78.0/24
GW=10.89.78.1
BRIDGE=rr-nettest0
TABLE=rime_runner_nettest
SLOT=9
C=rime-runner-job-$SLOT

rr_podman_env
podman image exists "$RIME_RUNNER_JOB_IMAGE" || rr_die "no job image: run tests/test-isolation.sh first"
podman network exists "$NET" && rr_die "$NET exists already (a crashed run?): podman network rm $NET"
nft list table inet "$TABLE" >/dev/null 2>&1 && rr_die "table inet $TABLE is loaded (a crashed run?)"

cleanup() {
	podman rm -f -t 0 "$C" rime-runner-builder >/dev/null 2>&1 || true
	nft delete table inet "$TABLE" 2>/dev/null || true
	podman network rm -f "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fails=0
ok() { printf 'ok   %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; fails=$((fails + 1)); }
note() { printf 'note %s\n' "$*"; }

podman network create --subnet "$SUBNET" --gateway "$GW" --interface-name "$BRIDGE" --disable-dns "$NET" >/dev/null
[ "$(podman network inspect "$NET" --format '{{.NetworkInterface}}')" = "$BRIDGE" ] ||
	rr_die "podman did not name the bridge $BRIDGE"

rendered=$DRY/nettest-nftables.conf
sed -e "s#@SUBNET@#$SUBNET#g" -e "s#@GATEWAY@#$GW#g" -e "s#@BRIDGE@#$BRIDGE#g" \
	-e "s#inet rime_runner\\b#inet $TABLE#g" "$HERE/nftables.conf.in" >"$rendered"
[ "$(grep -c "inet $TABLE" "$rendered")" = 3 ] || rr_die "table rename failed in $rendered"
grep -E 'inet rime_runner([^_]|$)' "$rendered" >/dev/null && rr_die "rendered file still names the production table"
table_on() { nft -f "$rendered"; }
table_off() { nft delete table inet "$TABLE" 2>/dev/null || true; }

lan_ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -1)
router=$(ip -4 route show default 2>/dev/null | awk '{print $3; exit}')
ssh_v4=0
ssh_v6=0
ss -Hlnt 'sport = :22' | awk '{print $4}' | grep -E '^(0\.0\.0\.0|\*):22$' >/dev/null && ssh_v4=1
ss -Hlnt 'sport = :22' | awk '{print $4}' | grep -E '^(\[::\]|\*):22$' >/dev/null && ssh_v6=1
echo "targets: gateway $GW, LAN address ${lan_ip:-none}, router ${router:-none}; sshd v4=$ssh_v4 v6=$ssh_v6"

# Start a job container (sleeping, as uid 1000, no capabilities) and probe from it.
# $1 = "prod" (production job_args) or "raw" (job_args minus the IPv6 switch).
start() {
	job_args "$SLOT" "$NET"
	local args=() skip=0 a
	for a in "${JOB_ARGS[@]}"; do
		if [ "$skip" = 1 ]; then skip=0; continue; fi
		if [ "$1" = raw ] && [ "$a" = --sysctl ]; then skip=1; continue; fi
		args+=("$a")
	done
	podman run -d "${args[@]}" --entrypoint sleep "$RIME_RUNNER_JOB_IMAGE" 300 >/dev/null
	# wait for duplicate address detection, or an fe80:: source cannot be used
	sleep 3
}
tcp() { # tcp HOST PORT -> prints open|closed
	if podman exec "$C" timeout 4 bash -c "exec 3<>/dev/tcp/$1/$2" 2>/dev/null; then echo open; else echo closed; fi
}
v6ll() { # the bridge's link-local address, as the container would dial it
	ip -6 addr show dev "$BRIDGE" scope link 2>/dev/null | sed -n 's#.*inet6 \(fe80::[0-9a-f:]*\)/.*#\1#p' | head -1
}
stop() { podman rm -f -t 0 "$C" >/dev/null 2>&1 || true; }
expect() { # expect LABEL WANT GOT
	if [ "$2" = "$3" ]; then ok "$1: $3"; else bad "$1: want $2, got $3"; fi
}

# ── A: no firewall, IPv6 on: the holes are real and the probe sees them ────
table_off
start raw
ll=$(v6ll)
[ -n "$ll" ] || rr_die "bridge $BRIDGE has no fe80:: address"
echo "bridge link-local: $ll"
if [ "$ssh_v4" = 1 ]; then expect "A (no table) gateway sshd over IPv4" open "$(tcp "$GW" 22)"; fi
if [ "$ssh_v6" = 1 ]; then expect "A (no table, IPv6 on) bridge fe80:: sshd" open "$(tcp "$ll%eth0" 22)"; fi
stop

# ── B: the firewall alone ──────────────────────────────────────────────────
table_on
start raw
ll=$(v6ll)
expect "B (table, IPv6 on) gateway sshd over IPv4" closed "$(tcp "$GW" 22)"
expect "B (table, IPv6 on) bridge fe80:: sshd" closed "$(tcp "$ll%eth0" 22)"
stop

# ── C: production: firewall + job_args ─────────────────────────────────────
start prod
ll=$(v6ll)
expect "C eth0 IPv6 switched off in the job" 1 "$(podman exec "$C" cat /proc/sys/net/ipv6/conf/eth0/disable_ipv6)"
expect "C ::1 still there for local test servers" 0 "$(podman exec "$C" cat /proc/sys/net/ipv6/conf/lo/disable_ipv6)"
expect "C gateway sshd over IPv4" closed "$(tcp "$GW" 22)"
expect "C bridge fe80:: sshd" closed "$(tcp "$ll%eth0" 22)"
if [ -n "$lan_ip" ]; then expect "C katana's LAN address $lan_ip:22" closed "$(tcp "$lan_ip" 22)"; fi
if [ -n "$router" ]; then
	expect "C LAN router $router:80" closed "$(tcp "$router" 80)"
	expect "C LAN router $router:53" closed "$(tcp "$router" 53)"
fi
expect "C Tailscale/CGNAT 100.100.100.100:53" closed "$(tcp 100.100.100.100 53)"
expect "C gateway DNS (tcp 53) closed too" closed "$(tcp "$GW" 53)"
# Positive controls: the job still reaches the internet, DNS included.
if getent hosts github.com >/dev/null 2>&1; then
	if podman exec "$C" getent hosts github.com >/dev/null 2>&1; then
		ok "C DNS works (public resolvers: $RIME_RUNNER_DNS)"
	else
		bad "C github.com does not resolve inside the job"
	fi
	expect "C public internet (github.com:443)" open "$(tcp github.com 443)"
else
	note "katana itself cannot resolve github.com (offline?): positive controls skipped"
fi
stop

# ── D: the job's own IPv6 switch, no firewall ──────────────────────────────
table_off
start prod
ll=$(v6ll)
if [ "$ssh_v6" = 1 ]; then expect "D (no table, production job_args) bridge fe80:: sshd" closed "$(tcp "$ll%eth0" 22)"; fi
stop

# ── E: the builder, with its own args, behind the same table ──────────────
table_on
[ -e "$RIME_RUNNER_NIX/var/nix/db/db.sqlite" ] || rr_die "no seeded store: run tests/test-isolation.sh first"
builder_args "$NET"
rm -f "$RIME_RUNNER_NIX/var/nix/daemon-socket/socket"
podman run -d "${BUILDER_ARGS[@]}" "$RIME_RUNNER_BUILDER_IMAGE" nix-daemon >/dev/null
sleep 3
B=rime-runner-builder
expect "E builder eth0 IPv6 off" 1 "$(podman exec "$B" cat /proc/sys/net/ipv6/conf/eth0/disable_ipv6)"
if podman exec "$B" timeout 4 bash -c "exec 3<>/dev/tcp/$GW/22" 2>/dev/null; then bad "E builder reaches the gateway's sshd"; else ok "E builder: gateway sshd closed"; fi
if [ -n "$lan_ip" ]; then
	if podman exec "$B" timeout 4 bash -c "exec 3<>/dev/tcp/$lan_ip/22" 2>/dev/null; then bad "E builder reaches $lan_ip:22"; else ok "E builder: katana's LAN address closed"; fi
fi
# End to end: a job asks the daemon for a path it does not have, and the
# daemon substitutes it from cache.nixos.org (DNS, HTTPS, its narinfo cache,
# the signature check). A small path with no references: jq 1.8.2's man pages.
SUBST=/nix/store/kyc6a29vbm7k4nh84bj17gxaljnic3a9-jq-1.8.2-man
if getent hosts cache.nixos.org >/dev/null 2>&1; then
	podman exec "$B" nix-store --delete "$SUBST" >/dev/null 2>&1 || true
	start prod
	if podman exec "$C" nix-store -r "$SUBST" >"$DRY/nettest-subst.log" 2>&1 && podman exec "$C" test -d "$SUBST/share"; then
		ok "E a job's nix-store -r is substituted by the builder from cache.nixos.org"
	else
		bad "E substitution through the builder failed: $(tail -2 "$DRY/nettest-subst.log" | tr '\n' ' ')"
	fi
	stop
	podman exec "$B" nix-store --delete "$SUBST" >/dev/null 2>&1 || true
else
	note "katana cannot resolve cache.nixos.org: substitution check skipped"
fi
podman stop -t 5 "$B" >/dev/null 2>&1 || true

cleanup
trap - EXIT
if podman network exists "$NET" || nft list table inet "$TABLE" >/dev/null 2>&1; then
	bad "test network or table left behind"
else
	ok "test network and table removed"
fi
if nft list table inet rime_runner >/dev/null 2>&1; then note "production table inet rime_runner is loaded (not by this test)"; fi
echo "RESULT failures=$fails"
exit "$fails"
