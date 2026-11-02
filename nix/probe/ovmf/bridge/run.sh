#!/usr/bin/env bash
# run.sh KIND [driver args]: the bridge OVMF proof for topology KIND (A or E) on this host, in KVM guests only.
# Needs: disks/KIND from mkbootc.sh, payload.img from payload.sh (README.md here), the rime-ovmf-probe image and
# the OVMF rig varstores in $OVMF_WORK (nix/probe/ovmf/lab.sh made them). Logs and screenshots go to $ART/KIND.
set -euo pipefail
KIND=$1; shift
LAB=${LAB:-/var/lab/rime-nix/scratch/bridge}
ART=${ART:-/var/lab/rime-nix/artifacts/bridge}
OVMF_WORK=${OVMF_WORK:-/var/lab/rime-nix/artifacts/ovmf/work}
RIG=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$ART/$KIND"
exec sudo -n podman run --rm --device /dev/kvm --security-opt label=disable \
  -v "$LAB:/lab" -v "$ART/$KIND:/art" -v "$OVMF_WORK:/ovmf:ro" -v "$RIG:/rig:ro" \
  --entrypoint python3 localhost/rime-ovmf-probe -u /rig/drive-bridge.py "$KIND" "$@"
