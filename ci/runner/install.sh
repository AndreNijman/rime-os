#!/bin/bash
# Install the isolated katana CI runners. Andre runs this; nothing else does.
#
#   sudo ci/runner/install.sh                    install next to today's runner (default)
#   sudo ci/runner/install.sh --replace-legacy   also stop + disable apex-github-runner.service
#   sudo ci/runner/install.sh --slots 2          two concurrent jobs
#   sudo ci/runner/install.sh --plan             print what it would do, change nothing
#
# Default = COEXIST: today's apex-github-runner.service (label `katana`) keeps
# serving kernel-build.yml and katana-probe.yml, because those jobs run rootless
# podman inside the runner, which a job container here does not provide. The new
# runners answer the NEW label `katana-nix`, so no existing job moves. PLAN.md
# 6.2 row 6: "Today's apex-github-runner.service stays until step 3 replaces it."
#
# What it touches: /usr/local/libexec/rime-runner, /etc/rime-runner,
# /etc/systemd/system/rime-runner*, /var/lab/rime-runner, one podman network
# (rime-runner), one nftables table (inet rime_runner), SELinux file contexts
# for /var/lab/rime-runner/nix. What it never touches: the boot chain, ESP,
# GRUB, MOK, NVRAM, subvolumes, /etc/apex-runner, the ghrunner user, and
# apex-github-runner.service unless --replace-legacy is given.
set -euo pipefail

SRC=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
PREFIX=/usr/local/libexec/rime-runner
ETC=/etc/rime-runner
UNITDIR=/etc/systemd/system
LEGACY=apex-github-runner.service

replace_legacy=0
plan=0
slots=
while [ $# -gt 0 ]; do
	case $1 in
	--replace-legacy) replace_legacy=1 ;;
	--plan) plan=1 ;;
	--slots) slots=${2:?}; shift ;;
	-h | --help) sed -n '2,22p' "$0"; exit 0 ;;
	*) echo "unknown option: $1" >&2; exit 64 ;;
	esac
	shift
done

run() {
	if [ "$plan" = 1 ]; then printf 'would run: %s\n' "$*"; else "$@"; fi
}
say() { printf 'install: %s\n' "$*"; }

[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
mountpoint -q /var/lab || { echo "/var/lab is not mounted" >&2; exit 1; }
for t in podman nft systemctl curl python3; do
	command -v "$t" >/dev/null || { echo "missing: $t" >&2; exit 1; }
done
[ -c /dev/kvm ] || say "warning: no /dev/kvm; VM tests (runNixOSTest) will not run"

# ── 1. files ───────────────────────────────────────────────────────────────
say "installing scripts to $PREFIX"
run install -d -m 0755 "$PREFIX" "$PREFIX/bin" "$PREFIX/lib" "$PREFIX/builder" "$PREFIX/images/job"
run install -m 0755 "$SRC"/bin/rime-runner-job "$SRC"/bin/rime-runner-builder "$SRC"/bin/rime-runner-ctl "$PREFIX/bin/"
run install -m 0644 "$SRC/lib/common.sh" "$PREFIX/lib/"
run install -m 0644 "$SRC/builder/nix.conf" "$PREFIX/builder/"
run install -m 0644 "$SRC"/images/job/Containerfile "$SRC"/images/job/nix.conf "$PREFIX/images/job/"
run install -m 0755 "$SRC"/images/job/entrypoint.sh "$SRC"/images/job/probe.sh "$PREFIX/images/job/"
run install -m 0644 "$SRC/README.md" "$PREFIX/README.md"
run install -m 0755 "$SRC/uninstall.sh" "$PREFIX/uninstall.sh"
run ln -sf "$PREFIX/bin/rime-runner-ctl" /usr/local/sbin/rime-runner-ctl

run install -d -m 0700 "$ETC"
if [ ! -e "$ETC/runner.conf" ]; then
	run install -m 0644 "$SRC/runner.conf" "$ETC/runner.conf"
else
	say "keeping existing $ETC/runner.conf"
fi
if [ -n "$slots" ] && [ "$plan" = 0 ]; then
	sed -i "s/^RIME_RUNNER_SLOTS=.*/RIME_RUNNER_SLOTS=$slots/" "$ETC/runner.conf"
fi
# shellcheck source=runner.conf
if [ -e "$ETC/runner.conf" ]; then . "$ETC/runner.conf"; else . "$SRC/runner.conf"; fi
[ -n "$slots" ] && RIME_RUNNER_SLOTS=$slots

# What the legacy runner looked like before we did anything: uninstall.sh puts it back.
if [ ! -e "$ETC/legacy-state" ]; then
	en=$(systemctl is-enabled "$LEGACY" 2>/dev/null || true)
	ac=$(systemctl is-active "$LEGACY" 2>/dev/null || true)
	say "recording $LEGACY state: enabled=$en active=$ac"
	if [ "$plan" = 0 ]; then printf 'enabled=%s\nactive=%s\nrecorded=%s\n' "$en" "$ac" "$(date -Is)" >"$ETC/legacy-state"; fi
fi

# ── 2. firewall ────────────────────────────────────────────────────────────
say "rendering $ETC/nftables.conf for $RIME_RUNNER_SUBNET"
if [ "$plan" = 0 ]; then
	sed -e "s#@SUBNET@#$RIME_RUNNER_SUBNET#g" -e "s#@GATEWAY@#$RIME_RUNNER_GATEWAY#g" \
		"$SRC/nftables.conf.in" >"$ETC/nftables.conf"
	chmod 0600 "$ETC/nftables.conf"
	nft -c -f "$ETC/nftables.conf"
fi

# ── 3. units ───────────────────────────────────────────────────────────────
say "installing units"
if [ "$plan" = 0 ]; then
	sed -e "s#@CPU_WEIGHT@#$RIME_RUNNER_CPU_WEIGHT#" -e "s#@IO_WEIGHT@#$RIME_RUNNER_IO_WEIGHT#" \
		-e "s#@MEMORY_HIGH@#$RIME_RUNNER_MEMORY_HIGH#" -e "s#@MEMORY_MAX@#$RIME_RUNNER_MEMORY_MAX#" \
		"$SRC/systemd/rime-runner.slice" >"$UNITDIR/rime-runner.slice"
	install -m 0644 "$SRC/systemd/rime-runner-builder.service" "$SRC/systemd/rime-runner@.service" \
		"$SRC/systemd/rime-runner-firewall.service" "$UNITDIR/"
	systemctl daemon-reload
else
	echo "would install rime-runner.slice, rime-runner-builder.service, rime-runner@.service, rime-runner-firewall.service"
fi

# ── 4. state on /var/lab, SELinux, network, images, store ──────────────────
run install -d -m 0711 "$RIME_RUNNER_STATE"
if command -v semanage >/dev/null && [ "$(getenforce 2>/dev/null || echo Disabled)" != Disabled ]; then
	say "SELinux file contexts for $RIME_RUNNER_NIX"
	run semanage fcontext -a -t container_ro_file_t -r s0 "$RIME_RUNNER_NIX(/.*)?" 2>/dev/null ||
		run semanage fcontext -m -t container_ro_file_t -r s0 "$RIME_RUNNER_NIX(/.*)?"
	run semanage fcontext -a -t container_file_t -r s0 "$RIME_RUNNER_NIX/var/nix/daemon-socket(/.*)?" 2>/dev/null ||
		run semanage fcontext -m -t container_file_t -r s0 "$RIME_RUNNER_NIX/var/nix/daemon-socket(/.*)?"
fi
if ! podman network exists "$RIME_RUNNER_NETWORK" 2>/dev/null; then
	say "creating podman network $RIME_RUNNER_NETWORK ($RIME_RUNNER_SUBNET)"
	run podman network create --subnet "$RIME_RUNNER_SUBNET" --gateway "$RIME_RUNNER_GATEWAY" "$RIME_RUNNER_NETWORK"
fi
say "building images (job image: dnf + the sha256-pinned runner tarball)"
run "$PREFIX/bin/rime-runner-ctl" build-images
run "$PREFIX/bin/rime-runner-ctl" seed
if [ "$plan" = 0 ] && command -v restorecon >/dev/null; then restorecon -R "$RIME_RUNNER_NIX" || true; fi

# ── 5. start ───────────────────────────────────────────────────────────────
run systemctl enable --now rime-runner-firewall.service rime-runner-builder.service
if [ -s "$RIME_RUNNER_TOKEN_FILE" ]; then
	for i in $(seq 0 $((RIME_RUNNER_SLOTS - 1))); do
		run systemctl enable --now "rime-runner@$i.service"
	done
else
	say "NOT starting job slots: $RIME_RUNNER_TOKEN_FILE is missing."
	say "Create a fine-grained PAT for $RIME_RUNNER_REPO only, permission 'Administration: Read and write', then:"
	say "  sudo install -m 0600 /dev/stdin $RIME_RUNNER_TOKEN_FILE   # paste, Ctrl-D"
	say "  sudo systemctl enable --now rime-runner@0.service"
fi

if [ "$replace_legacy" = 1 ]; then
	say "--replace-legacy: stopping and disabling $LEGACY (uninstall.sh restores it)"
	run systemctl disable --now "$LEGACY"
else
	say "$LEGACY left as it is (coexist). Use --replace-legacy only once nothing needs label 'katana'."
fi

say "done. Status: sudo rime-runner-ctl status. Test without registering: sudo $SRC/tests/test-isolation.sh"
