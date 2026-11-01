#!/usr/bin/env bash
# run-all.sh: the whole OVMF Secure Boot probe, from the host, with nothing
# installed on it: Nix runs in the rime-nix container, firmware + QEMU in the
# rime-ovmf-probe container (Fedora 45). Touches no host boot state.
#   ART   artifacts dir (default /var/lab/rime-nix/artifacts/ovmf)
#   SKIP_NIX=1  reuse $ART/in from an earlier build-nixos.sh
set -euo pipefail
ART=${ART:-/var/lab/rime-nix/artifacts/ovmf}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$ART/bootc-ref"

sudo -n podman build -q -t rime-ovmf-probe "$HERE" >/dev/null

# bootupd's static GRUB config, from the bootc base image the product uses today.
sudo -n podman run --rm --entrypoint /bin/sh -v "$ART/bootc-ref:/o:z" quay.io/fedora/fedora-bootc:45 \
  -c 'rm -rf /o/grub2-static && cp -r /usr/lib/bootupd/grub2-static /o/ && rpm -q bootupd > /o/bootupd-version.txt'

[[ ${SKIP_NIX:-0} == 1 ]] || "$HERE/build-nixos.sh"

rc=0
sudo -n podman run --rm --device /dev/kvm -e ART=/art \
  -v "$ART:/art:z" -v "$HERE:/rig:ro,z" rime-ovmf-probe /rig/lab.sh "${@:-all}" || rc=$?

# Leave the evidence readable and removable by the invoking user.
sudo -n chown -R "$(id -u):$(id -g)" "$ART"
exit "$rc"
