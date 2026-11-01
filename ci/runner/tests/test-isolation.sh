#!/bin/bash
# Dry-run isolation test for the katana job containers. SAFE TO RUN NOW: it
# registers nothing with GitHub, starts no systemd unit, changes no host config
# and opens no network (--network none). Needs root (sudo).
#
#   sudo ci/runner/tests/test-isolation.sh          run (images cached between runs)
#   sudo ci/runner/tests/test-isolation.sh --clean  remove everything it created
#
# What it does, all under /var/lab/rime-runner-dryrun:
#   1. builds the job image and pulls the pinned builder image into a podman
#      storage root of its own (Andre's /var/lib/containers is not touched);
#   2. seeds a shared Nix store and starts the builder (nix-daemon) with the
#      SAME arguments production uses (lib/common.sh builder_args), network none;
#   3. starts one job container with the SAME arguments production uses
#      (job_args), in dry-run mode: the entrypoint runs probe.sh instead of the
#      GitHub runner. The probes must all pass: no /var/home, ~/.ssh, ~/.gnupg,
#      no capabilities, own uid block, the shared store readable but not
#      writable, a derivation built through the builder's sandbox;
#   4. MUTANT: the same job with a canary directory mounted at
#      /var/home/andre/.ssh must FAIL the probes (proves the probe can see a leak);
#   5. checks the firewall rules parse (`nft -c`, never applied);
#   6. stops the builder and proves no container is left behind.
set -euo pipefail

HERE=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
export RIME_RUNNER_HOME=$HERE
export RIME_RUNNER_CONF=$HERE/runner.conf
# shellcheck source=../lib/common.sh
. "$HERE/lib/common.sh"

[ "$(id -u)" = 0 ] || rr_die "run with sudo"

DRY=/var/lab/rime-runner-dryrun
# shellcheck disable=SC2034 # all four are read by the lib/common.sh functions
{
	RIME_RUNNER_STATE=$DRY
	RIME_RUNNER_STORAGE=$DRY/containers
	RIME_RUNNER_RUNROOT=/run/rime-runner-dryrun/containers
	RIME_RUNNER_NIX=$DRY/nix
}
unset RIME_RUNNER_CGROUP_PARENT

if [ "${1:-}" = --clean ]; then
	rr_podman_env
	podman stop -t 5 rime-runner-builder >/dev/null 2>&1 || true
	podman rm -fa >/dev/null 2>&1 || true
	podman rmi -fa >/dev/null 2>&1 || true
	rm -rf --one-file-system "$DRY" /run/rime-runner-dryrun
	echo "removed $DRY"
	exit 0
fi

mountpoint -q /var/lab || rr_die "/var/lab is not mounted"
host_podman() { env -u CONTAINERS_STORAGE_CONF -u CONTAINERS_CONF_OVERRIDE podman "$@"; }
host_before=$(host_podman ps -a --format '{{.Names}}' | sort | tr '\n' ' ')
rr_podman_env

fails=0
ok() { printf 'ok   %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; fails=$((fails + 1)); }

# ── 1. images ──────────────────────────────────────────────────────────────
if ! podman image exists "$RIME_RUNNER_BUILDER_IMAGE"; then
	podman pull -q "$RIME_RUNNER_BUILDER_IMAGE" >/dev/null
fi
if ! podman image exists "$RIME_RUNNER_JOB_IMAGE"; then
	echo "building $RIME_RUNNER_JOB_IMAGE (first run only)"
	podman build -q --pull=missing --network host -t "$RIME_RUNNER_JOB_IMAGE" \
		-f "$HERE/images/job/Containerfile" "$HERE/images/job" >/dev/null
fi
# Rebuild when the probe or entrypoint changed since the image was built.
want=$(cat "$HERE/images/job/Containerfile" "$HERE/images/job/entrypoint.sh" "$HERE/images/job/probe.sh" "$HERE/images/job/nix.conf" | sha256sum | cut -c1-16)
have=$(podman image inspect "$RIME_RUNNER_JOB_IMAGE" --format '{{index .Labels "rime.runner.inputs"}}' 2>/dev/null || true)
if [ "$want" != "$have" ]; then
	podman build -q --pull=missing --network host --label "rime.runner.inputs=$want" -t "$RIME_RUNNER_JOB_IMAGE" \
		-f "$HERE/images/job/Containerfile" "$HERE/images/job" >/dev/null
fi
ok "images: builder $(podman image inspect "$RIME_RUNNER_BUILDER_IMAGE" --format '{{.Id}}' | cut -c1-12), job $(podman image inspect "$RIME_RUNNER_JOB_IMAGE" --format '{{.Id}}' | cut -c1-12)"

# ── 2. store + builder ─────────────────────────────────────────────────────
rr_seed_store
rr_label_store
builder_args none
rm -f "$RIME_RUNNER_NIX/var/nix/daemon-socket/socket"
podman run -d "${BUILDER_ARGS[@]}" "$RIME_RUNNER_BUILDER_IMAGE" nix-daemon >/dev/null
cleanup() { podman stop -t 5 rime-runner-builder >/dev/null 2>&1 || true; }
trap cleanup EXIT
for _ in $(seq 1 60); do
	[ -S "$RIME_RUNNER_NIX/var/nix/daemon-socket/socket" ] && break
	sleep 0.5
done
if [ -S "$RIME_RUNNER_NIX/var/nix/daemon-socket/socket" ]; then ok "builder: nix-daemon listening"; else bad "builder: no socket"; podman logs rime-runner-builder 2>&1 | tail -5; fi

# ── 3. the job, dry run ────────────────────────────────────────────────────
job_args 0 none
expect_base=$(rr_uid_base 1)
echo "--- probes (job slot 0, production job_args, network none) ---"
set +e
podman run "${JOB_ARGS[@]}" -e RIME_RUNNER_DRY_RUN=1 -e RIME_RUNNER_EXPECT_UID_BASE="$expect_base" \
	"$RIME_RUNNER_JOB_IMAGE" </dev/null | tee "$DRY/probe.log"
rc=${PIPESTATUS[0]}
set -e
if [ "$rc" = 0 ]; then ok "job probes: all passed"; else bad "job probes: $rc failed"; fi

# ── 4. mutant: a leaked mount must be caught ───────────────────────────────
canary=$DRY/canary/.ssh
install -d -o 1000 -g 1000 -m 0700 "$canary"
printf 'rime-runner canary, not a key\n' >"$canary/id_ed25519"
chown 1000:1000 "$canary/id_ed25519"
chmod 0600 "$canary/id_ed25519"
set +e
podman run "${JOB_ARGS[@]}" -v "$canary:/var/home/andre/.ssh:ro" \
	-e RIME_RUNNER_DRY_RUN=1 -e RIME_RUNNER_EXPECT_UID_BASE="$expect_base" \
	"$RIME_RUNNER_JOB_IMAGE" </dev/null >"$DRY/mutant.log" 2>&1
mrc=$?
set -e
if [ "$mrc" != 0 ] && grep -E '^FAIL absent: /var/home/andre/.ssh exists' "$DRY/mutant.log" >/dev/null; then
	ok "mutant (canary mounted at /var/home/andre/.ssh) is caught: $mrc probe(s) fail"
else
	bad "mutant not caught (rc=$mrc)"
fi
if grep -E '^PASS unreadable: /var/home/andre/.ssh/id_ed25519' "$DRY/mutant.log" >/dev/null; then
	ok "even when mounted, a uid-1000 0600 file stays unreadable to the job (unmapped owner + SELinux)"
fi

# ── 4b. benchmark freeze: rime-runner-ctl pause --freeze uses podman pause ──
if podman pause rime-runner-builder >/dev/null 2>&1 &&
	[ "$(podman inspect rime-runner-builder --format '{{.State.Status}}')" = paused ] &&
	podman unpause rime-runner-builder >/dev/null 2>&1 &&
	[ "$(podman inspect rime-runner-builder --format '{{.State.Status}}')" = running ]; then
	ok "freeze/thaw (podman pause/unpause) works on a runner container"
else
	bad "podman pause/unpause did not work"
fi

# ── 5. firewall rules parse (check mode, nothing applied) ──────────────────
rendered=$DRY/nftables.conf
sed -e "s#@SUBNET@#$RIME_RUNNER_SUBNET#g" -e "s#@GATEWAY@#$RIME_RUNNER_GATEWAY#g" \
	-e "s#@BRIDGE@#$RIME_RUNNER_BRIDGE#g" "$HERE/nftables.conf.in" >"$rendered"
if nft -c -f "$rendered"; then ok "nftables rules parse (nft -c, not loaded)"; else bad "nftables rules do not parse"; fi
if nft list tables 2>/dev/null | grep -E 'rime_runner' >/dev/null; then bad "rime_runner table is LOADED on the host (should not be)"; else ok "rime_runner table not loaded on the host"; fi

# ── 6. nothing left behind ─────────────────────────────────────────────────
cleanup
trap - EXIT
for _ in $(seq 1 20); do
	[ -z "$(podman ps -aq)" ] && break
	sleep 0.5
done
if [ -z "$(podman ps -aq)" ]; then ok "no runner container left"; else bad "containers left: $(podman ps -a --format '{{.Names}}')"; fi
host_after=$(host_podman ps -a --format '{{.Names}}' | sort | tr '\n' ' ')
# Other sessions start and stop their own containers meanwhile; what matters is
# that none of ours landed there and the shared dev container is still up.
if printf '%s' "$host_after" | tr ' ' '\n' | grep -E '^rime-runner' >/dev/null; then
	bad "a rime-runner container is in the host's own podman storage"
else
	ok "host podman storage has no runner container (before: ${host_before:-none}; after: ${host_after:-none})"
fi
if systemctl is-active --quiet apex-github-runner.service; then ok "apex-github-runner.service still active (not touched)"; else echo "note: apex-github-runner.service is not active (this test never touches it)"; fi

echo "RESULT failures=$fails"
exit "$fails"
