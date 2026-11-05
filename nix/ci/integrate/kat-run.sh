#!/bin/bash
# kat-run.sh — hostlayer-katana on katana's real sanitised snapshot and runtime share, at this checkout's HEAD.
# Runs on the HOST (it drives the rime-nix container with sudo podman exec). Every integration runs it
# (docs/nixos/open-items.md: the H rows cite only this run); the store check alone prints SKIP and proves nothing.
#   nix/ci/integrate/kat-run.sh            log: /var/lab/rime-nix/artifacts/<the checkout's directory name>/hostlayer-katana.log
# The checkout must live under /var/lab/rime-nix (mounted at /work in the container); RIME_TREE (a host path)
# overrides it, RIME_ART the log directory. Nothing from the snapshot is committed or written: its digest is
# printed before and after.
# shellcheck disable=SC2024 # the logs are the host user's files; only podman needs root
set -u
lab=/var/lab/rime-nix
tree=${RIME_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
[ -f "$tree/flake.nix" ] || { echo "kat-run.sh: $tree is not a Rime checkout (no flake.nix)" >&2; exit 2; }
case "$tree" in "$lab"/*) ;; *) echo "kat-run.sh: $tree is not under $lab, so the container cannot see it" >&2; exit 2 ;; esac
ctree=/work/${tree#"$lab"/}
name=$(basename "$tree")
art=${RIME_ART:-$lab/artifacts/$name}; mkdir -p "$art"
W=/work/scratch/$name-kat
log=$art/hostlayer-katana.log
digest() { sudo -n podman exec rime-nix bash -c 'cd /work/artifacts/katana-state && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16'; }
before=$(digest)
sudo -n podman exec rime-nix mkdir -p "$W"
sudo -n podman exec -w "$ctree" rime-nix nix build path:.#checks.x86_64-linux.hostlayer-katana.driver --out-link "$W/kat-driver" > "$art/kat-driver-build.log" 2>&1 || { echo "driver build failed (see $art/kat-driver-build.log)"; exit 1; }
sudo -n podman exec rime-nix bash -c "rm -rf $W/kat-tmp && mkdir -p $W/kat-tmp/out && cd $W/kat-tmp/out && RIME_KATANA_STATE=/work/artifacts/katana-state RIME_KATANA_RUNTIME=/work/artifacts/katana-runtime TMPDIR=$W/kat-tmp $W/kat-driver/bin/nixos-test-driver --output_directory $W/kat-tmp/out" > "$log" 2>&1
echo "EXIT $?" >> "$log"
after=$(digest)
echo "tree $tree at $(git -C "$tree" rev-parse --short HEAD); snapshot digest before $before after $after" >> "$log"
sudo -n chown "$(id -un):$(id -gn)" "$log" 2>/dev/null
sudo -n podman exec rime-nix rm -rf "$W"
tail -3 "$log"
