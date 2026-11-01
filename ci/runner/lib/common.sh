# shellcheck shell=bash
# Shared by every rime-runner script. Sourced, never executed.
#
# The one place that knows what a builder or a job container is allowed to see.
# The units, install.sh and the dry-run test all call builder_args/job_args, so
# the test proves the same `podman run` line that production uses.

RIME_RUNNER_HOME=${RIME_RUNNER_HOME:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}

# Config: an explicit file, else the installed one, else the repo default.
if [ -n "${RIME_RUNNER_CONF:-}" ]; then
	:
elif [ -r /etc/rime-runner/runner.conf ]; then
	RIME_RUNNER_CONF=/etc/rime-runner/runner.conf
else
	RIME_RUNNER_CONF=$RIME_RUNNER_HOME/runner.conf
fi
# shellcheck source=../runner.conf
. "$RIME_RUNNER_CONF"

rr_log() { printf 'rime-runner: %s\n' "$*" >&2; }
rr_die() { printf 'rime-runner: FATAL: %s\n' "$*" >&2; exit 1; }

# ── podman with a storage root, lock and tmp dir of its own ─────────────────
# Images and job layers land on /var/lab, and nothing here can lock up or list
# Andre's own rootful containers (rime-nix and friends).
rr_podman_env() {
	local cfg=$RIME_RUNNER_STATE/podman
	mkdir -p "$cfg" "$RIME_RUNNER_STORAGE" "$RIME_RUNNER_RUNROOT"
	cat >"$cfg/storage.conf" <<EOF
[storage]
driver = "overlay"
graphroot = "$RIME_RUNNER_STORAGE"
runroot = "$RIME_RUNNER_RUNROOT"
EOF
	cat >"$cfg/containers.conf" <<EOF
[engine]
lock_type = "file"
tmp_dir = "$RIME_RUNNER_RUNROOT/libpod"
volume_path = "$RIME_RUNNER_STORAGE/volumes"
cgroup_manager = "systemd"
EOF
	export CONTAINERS_STORAGE_CONF=$cfg/storage.conf
	export CONTAINERS_CONF_OVERRIDE=$cfg/containers.conf
}

rr_podman() { podman "$@"; }

# uid/gid block for a role: builder = block 0, job slot N = block N+1.
rr_uid_base() {
	local block=$1
	echo $((RIME_RUNNER_UID_BASE + block * RIME_RUNNER_UID_BLOCK))
}

# ── the builder: the only writer of the shared Nix store ────────────────────
# Runs nix-daemon with a REAL build sandbox. Measured on katana (2026-11-01,
# podman 6.1.3, SELinux enforcing): inside a user namespace Nix's sandbox needs
# CAP_SYS_ADMIN (in that namespace only), /proc without podman's masks, and no
# SELinux container label; with any of the three missing Nix refuses ("this
# system does not support the kernel namespaces that are required for
# sandboxing"). That is why the builder, not the job, holds those: job code
# (shell steps, actions, the GitHub token) never runs here, only derivation
# builders, which Nix's own sandbox confines (no network except fixed-output,
# private mount/pid/ipc/uts namespaces, nixbld uids).
#
# $1: network name, or "none" (the dry run).
builder_args() {
	local net=${1:-$RIME_RUNNER_NETWORK} base
	base=$(rr_uid_base 0)
	# shellcheck disable=SC2054 # commas belong to --tmpfs options
	BUILDER_ARGS=(
		--name rime-runner-builder --replace --rm
		--uidmap "0:$base:$RIME_RUNNER_UID_BLOCK" --gidmap "0:$base:$RIME_RUNNER_UID_BLOCK"
		--cap-add SYS_ADMIN
		--security-opt label=disable
		--security-opt 'unmask=/proc/*'
		--security-opt no-new-privileges
		--device /dev/kvm
		--read-only --tmpfs /tmp:rw,mode=1777,size=1g
		--pids-limit 65536
		--network "$net"
		-v "$RIME_RUNNER_NIX:/nix"
		-v "$RIME_RUNNER_HOME/builder/nix.conf:/etc/nix/nix.conf:ro"
	)
	if [ -n "${RIME_RUNNER_CGROUP_PARENT:-}" ]; then
		BUILDER_ARGS+=(--cgroup-parent "$RIME_RUNNER_CGROUP_PARENT")
	fi
}

# ── a job: one GitHub job, then destroyed ───────────────────────────────────
# No capabilities, no new privileges, the default SELinux container label, its
# own uid block, no host path except the Nix store (read-only), the store's
# profiles (read-only) and the builder's socket. In particular nothing under
# /var/home, /root, /var/roothome, /etc/rime-runner, /etc/apex-runner,
# /var/lab/runner or /var/lab/scratch exists inside it.
#
# $1: slot number, $2: network name or "none".
job_args() {
	local slot=$1 net=${2:-$RIME_RUNNER_NETWORK} base
	base=$(rr_uid_base $((slot + 1)))
	JOB_ARGS=(
		--name "rime-runner-job-$slot" --replace --rm -i
		--uidmap "0:$base:$RIME_RUNNER_UID_BLOCK" --gidmap "0:$base:$RIME_RUNNER_UID_BLOCK"
		--cap-drop all
		--security-opt no-new-privileges
		--pids-limit 32768
		--network "$net"
		-v "$RIME_RUNNER_NIX/store:/nix/store:ro"
		-v "$RIME_RUNNER_NIX/var/nix/profiles:/nix/var/nix/profiles:ro"
		-v "$RIME_RUNNER_NIX/var/nix/daemon-socket:/nix/var/nix/daemon-socket:ro"
	)
	if [ -n "${RIME_RUNNER_CGROUP_PARENT:-}" ]; then
		JOB_ARGS+=(--cgroup-parent "$RIME_RUNNER_CGROUP_PARENT")
	fi
}

# ── the shared store ────────────────────────────────────────────────────────
# First run: copy the builder image's own /nix into the store directory, so the
# daemon (and every job, through the read-only mount) finds Nix itself there.
rr_seed_store() {
	if [ -e "$RIME_RUNNER_NIX/var/nix/db/db.sqlite" ]; then
		return 0
	fi
	rr_log "seeding the shared Nix store at $RIME_RUNNER_NIX"
	local base
	base=$(rr_uid_base 0)
	install -d -o "$base" -g "$base" -m 0755 "$RIME_RUNNER_NIX"
	rr_podman run --rm --network none \
		--uidmap "0:$base:$RIME_RUNNER_UID_BLOCK" --gidmap "0:$base:$RIME_RUNNER_UID_BLOCK" \
		--security-opt label=disable \
		-v "$RIME_RUNNER_NIX:/seed" \
		"$RIME_RUNNER_BUILDER_IMAGE" \
		sh -c 'cp -a /nix/. /seed/ && mkdir -p /seed/var/nix/daemon-socket /seed/var/nix/builds'
	# The builder (uid block 0) owns it; jobs read it as "other".
	chmod 0755 "$RIME_RUNNER_NIX"
}

# SELinux: jobs keep the container label (container_t with random MCS
# categories), so the store must carry a type they may read at level s0 (no
# categories; a copied store can inherit stray ones). container_ro_file_t is
# read-only to container_t by policy, a second lock behind the :ro mount. Only
# the socket directory is container_file_t, because connecting to a socket
# needs write on the socket file. New files the builder (spc_t, s0) creates
# inherit their parent directory's type, so the labels hold as the store grows.
# install.sh records the same rules with semanage, so a relabel keeps them.
rr_label_store() {
	command -v chcon >/dev/null 2>&1 || return 0
	[ "$(getenforce 2>/dev/null || echo Disabled)" = Disabled ] && return 0
	chcon -R -t container_ro_file_t -l s0 "$RIME_RUNNER_NIX"
	chcon -R -t container_file_t -l s0 "$RIME_RUNNER_NIX/var/nix/daemon-socket"
}
