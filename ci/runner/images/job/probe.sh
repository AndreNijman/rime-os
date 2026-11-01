#!/bin/bash
# Isolation probes, run INSIDE a job container in dry-run mode (no registration).
# Prints one PASS/FAIL line per check and exits with the number of failures.
# Never prints the content of anything it manages to read.
set -uo pipefail

fails=0
pass() { printf 'PASS %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*"; fails=$((fails + 1)); }
info() { printf 'INFO %s\n' "$*"; }

# ── 1. Andre's home and keys do not exist in here ───────────────────────────
for p in /var/home /var/home/andre /var/home/andre/.ssh /var/home/andre/.gnupg /home/andre \
	/var/roothome /var/lab/runner /var/lab/scratch /var/lib/apex /etc/rime-runner /etc/apex-runner; do
	if [ -e "$p" ] || [ -L "$p" ]; then
		fail "absent: $p exists inside the job"
	else
		pass "absent: $p"
	fi
done
# Reading must fail too (not only "absent" by name): try the usual key files.
for f in /var/home/andre/.ssh/id_ed25519 /var/home/andre/.ssh/id_rsa /var/home/andre/.ssh/authorized_keys \
	/var/home/andre/.gnupg/pubring.kbx /var/home/andre/.gnupg/private-keys-v1.d /etc/apex-runner/gh-token \
	/etc/rime-runner/gh-token; do
	if cat "$f" >/dev/null 2>&1 || ls "$f" >/dev/null 2>&1; then
		fail "unreadable: $f was readable"
	else
		pass "unreadable: $f"
	fi
done
# Nothing in the job's filesystem belongs to Andre's uid on the host. Inside the
# user namespace an unmapped host uid shows as the overflow uid; a mount of any
# host path he owns would show up here.
# (/nix is skipped: its three mounts are the builder's, owned by its uid block.)
leaks=$(find / -xdev \( -path /proc -o -path /sys -o -path /nix \) -prune -o -uid 65534 -print 2>/dev/null | head -5)
if [ -n "$leaks" ]; then
	fail "no host-owned files on the job's root filesystem (found: $(echo "$leaks" | tr '\n' ' '))"
else
	pass "no host-owned (unmapped) files outside /nix"
fi
mounts=$(awk '{print $5}' /proc/self/mountinfo | grep -E '^/(var/home|home|root|var/roothome|etc/rime-runner|etc/apex-runner|var/lab/runner)(/|$)' || true)
if [ -n "$mounts" ]; then
	fail "no mount of a host secret path (found: $(echo "$mounts" | tr '\n' ' '))"
else
	pass "no mount point under /var/home, /root, /etc/*-runner, /var/lab/runner"
fi

# ── 2. identity: unprivileged, own uid block, no capabilities ───────────────
read -r inner outer len < /proc/self/uid_map
info "uid_map: $inner $outer $len"
if [ "$inner" = 0 ] && [ "$outer" = "${RIME_RUNNER_EXPECT_UID_BASE:-x}" ] && [ "$len" = 65536 ]; then
	pass "user namespace maps container root to host uid $outer (not 0, not 1000)"
else
	fail "user namespace: expected 0 ${RIME_RUNNER_EXPECT_UID_BASE:-?} 65536"
fi
if [ "$(id -u)" = 1000 ] && [ "$(id -un)" = runner ]; then pass "runs as runner (uid 1000 in the namespace)"; else fail "runs as $(id -un)"; fi
capeff=$(awk '/^CapEff/ {print $2}' /proc/self/status)
capbnd=$(awk '/^CapBnd/ {print $2}' /proc/self/status)
if [ "$capeff" = 0000000000000000 ] && [ "$capbnd" = 0000000000000000 ]; then
	pass "no capabilities (CapEff and CapBnd empty)"
else
	fail "capabilities: CapEff=$capeff CapBnd=$capbnd"
fi
if grep -q '^NoNewPrivs:[[:space:]]*1' /proc/self/status; then pass "no_new_privs set"; else fail "no_new_privs not set"; fi
if command -v sudo >/dev/null 2>&1; then fail "sudo present"; else pass "no sudo"; fi

# ── 3. no host devices, no firmware, no kernel memory ───────────────────────
# shellcheck disable=SC2010
devs=$(ls /dev 2>/dev/null | grep -E '^(nvme|sd[a-z]|dm-|mapper|kvm|mem$|kmem$|port$|nvidia|dri$|tpm)' || true)
if [ -z "$devs" ]; then pass "no block, kvm, GPU, TPM or memory devices in /dev"; else fail "devices present: $devs"; fi
# podman masks /proc/kcore with /dev/null: reading it succeeds and yields nothing.
kc=$(head -c 64 /proc/kcore 2>/dev/null | wc -c)
if [ "$kc" = 0 ]; then pass "/proc/kcore yields no kernel memory (masked)"; else fail "/proc/kcore readable ($kc bytes)"; fi
if touch /sys/firmware/efi/efivars/rime-probe 2>/dev/null; then fail "efivars writable"; else pass "efivars not writable"; fi
if [ -S /run/podman/podman.sock ] || [ -S /var/run/docker.sock ] || [ -S /run/docker.sock ]; then
	fail "a container engine socket is reachable"
else
	pass "no podman/docker socket"
fi

# ── 4. the shared Nix store: readable, not writable, builds go via the builder
if ls /nix/store >/dev/null 2>&1 && [ -n "$(ls /nix/store | head -1)" ]; then
	pass "shared Nix store readable ($(ls /nix/store | wc -l) entries)"
else
	fail "shared Nix store not readable"
fi
if touch /nix/store/rime-probe 2>/dev/null; then
	fail "/nix/store writable from the job"
	rm -f /nix/store/rime-probe
else
	pass "/nix/store read-only for the job"
fi
if rm -f /nix/var/nix/daemon-socket/socket 2>/dev/null && [ ! -e /nix/var/nix/daemon-socket/socket ]; then
	fail "job could delete the builder's socket"
else
	pass "builder socket cannot be removed or replaced by the job"
fi
if command -v nix >/dev/null 2>&1; then
	pass "nix client on PATH ($(nix --version 2>/dev/null))"
	if nix store info >/dev/null 2>&1 || nix store ping >/dev/null 2>&1; then
		pass "nix-daemon reachable through the socket"
	else
		fail "nix-daemon not reachable: $(nix store info 2>&1 | tail -1)"
	fi
	sh_path=$(readlink -f /nix/var/nix/profiles/default/bin/sh 2>/dev/null || true)
	[ -n "$sh_path" ] || sh_path=$(readlink -f /nix/var/nix/profiles/default/bin/bash)
	stamp=$(date +%s%N)
	expr="derivation { name = \"rime-runner-probe\"; system = \"x86_64-linux\"; builder = builtins.storePath \"$sh_path\";
	  args = [ \"-c\" \"read m < /proc/self/uid_map; echo \\\"stamp=$stamp pid=\$\$ map=\$m\\\" > \$out\" ]; }"
	if out=$(nix build --impure --no-link --print-out-paths --expr "$expr" 2>/tmp/nix-build.err); then
		content=$(cat "$out" 2>/dev/null || true)
		info "built through the daemon: $out -> $content"
		case $content in
		"stamp=$stamp pid=1 "*) pass "derivation built by the builder in Nix's sandbox (pid 1 in its own namespace) and visible read-only in /nix/store" ;;
		*) fail "unexpected build output: $content" ;;
		esac
	else
		fail "nix build through the daemon failed: $(tail -3 /tmp/nix-build.err | tr '\n' ' ')"
	fi
	info_out=$(nix store info 2>&1 || true)
	if printf '%s\n' "$info_out" | grep -E '^Trusted:[[:space:]]*0' >/dev/null; then
		pass "the builder treats the job as an UNTRUSTED client (cannot add unsigned paths, change substituters or disable the sandbox)"
	else
		fail "job is not an untrusted client: $(printf '%s' "$info_out" | tr '\n' ' ')"
	fi
else
	fail "nix client not on PATH"
fi

# ── 5. runner present, not registered ───────────────────────────────────────
if v=$(/opt/actions-runner/bin/Runner.Listener --version 2>/dev/null); then
	pass "GitHub runner $v present (dry run: not registered)"
else
	fail "GitHub runner binary missing or broken"
fi
if [ -e /opt/actions-runner/.runner ] || [ -e /opt/actions-runner/.credentials ]; then
	fail "runner has registration files in a dry run"
else
	pass "no .runner/.credentials (nothing registered)"
fi

# ── 6. network (dry run uses --network none) ────────────────────────────────
if getent hosts github.com >/dev/null 2>&1; then info "network: github.com resolves"; else info "network: none (dry run)"; fi

echo "RESULT failures=$fails"
exit "$fails"
