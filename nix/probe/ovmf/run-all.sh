#!/usr/bin/env bash
# run-all.sh: the whole OVMF Secure Boot probe, from the host, with nothing
# installed on it: Nix runs in the rime-nix container, firmware + QEMU in the
# rime-ovmf-probe container (Fedora 45). Touches no host boot state.
#   ART   artifacts dir (default /var/lab/rime-nix/artifacts/ovmf)
#   SKIP_NIX=1  reuse $ART/in from an earlier build-nixos.sh
#   KERNEL=rime Rime's real kernel, MOK-signed through rime.kernel.signing
#               (kernsign; use ART=/var/lab/rime-nix/artifacts/kernsign)
#   run-all.sh STEP...  only those lab.sh steps (or `case NAME`), no Nix build
set -euo pipefail
ART=${ART:-/var/lab/rime-nix/artifacts/ovmf}
HERE=$(cd "$(dirname "$0")" && pwd)
# ART ends in a recursive sudo chown: refuse anything but a dedicated, absolute,
# at-least-three-levels-deep directory (ART=/ or ART=$HOME would be a disaster).
ART=$(realpath -m -- "$ART")
[[ $ART == /*/*/* && $ART != "$HOME" && $ART != /var/home/* && $ART != /home/* && $ART != /root* ]] \
  || { echo "run-all.sh: refusing ART=$ART (use a dedicated scratch dir such as /var/lab/rime-nix/artifacts/ovmf)" >&2; exit 2; }
mkdir -p "$ART/bootc-ref"

sudo -n podman build -q -t rime-ovmf-probe "$HERE" >/dev/null

# bootupd's static GRUB config, from the bootc base image the product uses today.
sudo -n podman run --rm --entrypoint /bin/sh -v "$ART/bootc-ref:/o:z" quay.io/fedora/fedora-bootc:45 \
  -c 'rm -rf /o/grub2-static && cp -r /usr/lib/bootupd/grub2-static /o/ && rpm -q bootupd > /o/bootupd-version.txt'

lab() {
  sudo -n podman run --rm --device /dev/kvm -e ART=/art -e KERNEL="${KERNEL:-nixpkgs}" \
    -e BOOT_TIMEOUT="${BOOT_TIMEOUT:-300}" -e REFUSE_TIMEOUT="${REFUSE_TIMEOUT:-60}" \
    -v "$ART:/art:z" -v "$HERE:/rig:ro,z" -v "$HERE/../../pkgs/kernel:/kernel:ro,z" \
    rime-ovmf-probe /rig/lab.sh "$@"
}

rc=0
if [[ ${KERNEL:-nixpkgs} == rime && $# == 0 ]]; then
  # KERNEL=rime: Rime's real kernel, signed through rime.kernel.signing. Two Nix
  # passes around the detached signature (build-nixos.sh, lab.sh step_detach).
  t0=$SECONDS
  [[ ${SKIP_NIX:-0} == 1 ]] || "$HERE/build-nixos.sh" image
  lab rpms keys detach || rc=$?
  if [[ $rc == 0 ]]; then
    [[ ${SKIP_NIX:-0} == 1 ]] || "$HERE/build-nixos.sh" system || rc=$?
  fi
  [[ $rc != 0 ]] || lab vars sign disks cases || rc=$?
  echo "run-all.sh KERNEL=rime: rc=$rc, $(( SECONDS - t0 )) s"
else
  [[ ${SKIP_NIX:-0} == 1 || $# != 0 ]] || "$HERE/build-nixos.sh"
  lab "${@:-all}" || rc=$?
fi

# Leave the evidence readable and removable by the invoking user.
sudo -n chown -R "$(id -u):$(id -g)" "$ART"
exit "$rc"
