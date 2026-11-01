#!/usr/bin/env bash
# run.sh: the boot-hook OVMF lab, from the host, with nothing installed on it and no host boot state touched
# (README.md here). Nix runs in the rime-nix container; keys, firmware and QEMU in rime-ovmf-probe (Fedora 45:
# its shim, GRUB and Secure Boot OVMF).
#   LAB  scratch (default /var/lab/rime-nix/scratch/boothook): TEST keys, varstores, generations, disks
#   ART  evidence (default /var/lab/rime-nix/artifacts/boothook)
#   run.sh [prep|gens|disks|scenarios|all] [SCENARIO...]   SCENARIO = bls-sb-mok, bls-sb-off, custom-sb-mok, custom-sb-off
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OVMF_RIG=$(cd "$HERE/.." && pwd)
LAB=$(realpath -m -- "${LAB:-/var/lab/rime-nix/scratch/boothook}")
ART=$(realpath -m -- "${ART:-/var/lab/rime-nix/artifacts/boothook}")
for d in "$LAB" "$ART"; do   # both end in a recursive chown
  [[ $d == /var/lab/rime-nix/*/* ]] || { echo "run.sh: refusing $d (must be under /var/lab/rime-nix/<dir>/)" >&2; exit 2; }
done
PIN=1c89083e451e891152365b799595cf7790a43325
WT=$(cd "$HERE/../../../.." && pwd)
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
mkdir -p "$LAB" "$ART"
probe() { sudo -n podman run --rm --device /dev/kvm -v "$LAB:/lab:z" -v "$ART:/out:z" -v "$HERE:/rig:ro,z" \
  -e LAB=/lab -e OUT=/out --entrypoint "$1" rime-ovmf-probe "${@:2}"; }

step_prep() {
  sudo -n podman build -q -t rime-ovmf-probe "$OVMF_RIG" >/dev/null
  # Fedora 45's signed shim/GRUB and bootupd's static GRUB config, as the OVMF probe extracted them (run-all.sh).
  local P=${OVMF_ART:-/var/lab/rime-nix/artifacts/ovmf}
  mkdir -p "$LAB/efi"; cp "$P"/work/efi/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$LAB/efi/"
  rm -rf "$LAB/grub2-static"; cp -r "$P/bootc-ref/grub2-static" "$LAB/"
  # The unsigned kernel of every lab generation, and a detached signature by a THROWAWAY TEST MOK.
  mkdir -p "$LAB/in"
  local k
  k=$(sudo -n podman exec rime-nix nix build --impure --no-link --print-out-paths --expr \
    "(import $(to_work "$HERE")/systems.nix { flake = builtins.getFlake \"path:$(to_work "$WT")\"; }).unsignedKernel")
  sudo -n podman exec rime-nix cp "$k/bzImage" "$(to_work "$LAB")/in/bzImage.unsigned"
  probe /bin/bash -euc '
    K=/lab/keys; mkdir -p $K /lab/fw
    if [[ ! -f $K/test-mok.key ]]; then
      openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
        -subj "/CN=Rime boot-hook lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout $K/test-mok.key -out $K/test-mok.crt 2>/dev/null
      openssl x509 -in $K/test-mok.crt -outform DER -out $K/test-mok.der
      cat /proc/sys/kernel/random/uuid > $K/test-mok.guid; chmod 600 $K/test-mok.key
    fi
    openssl x509 -in $K/test-mok.crt -noout -subject -fingerprint -sha256
    sbsign --key $K/test-mok.key --cert $K/test-mok.crt --detached --output /lab/in/kernel.sig /lab/in/bzImage.unsigned
    O=/usr/share/edk2/ovmf
    qemu-img convert -O raw $O/OVMF_CODE_4M.secboot.qcow2 /lab/fw/code-secboot.raw
    qemu-img convert -O raw $O/OVMF_VARS_4M.secboot.qcow2 /lab/fw/vars-secboot.raw
    virt-fw-vars --input /lab/fw/vars-secboot.raw --output /lab/fw/vars-sb-mok.fd \
      --add-mok "$(cat $K/test-mok.guid)" $K/test-mok.der --set-shim-verbose >/dev/null
    qemu-img convert -O raw $O/OVMF_VARS_4M.qcow2 /lab/fw/vars-sb-off.fd
    rpm -q edk2-ovmf qemu-system-x86-core' | tee "$ART/prep.txt"
}

step_gens() { LAB=$LAB WT=$WT "$HERE/gens.sh" | tee "$ART/gens.txt"; cp "$LAB/gens.json" "$ART/"; }

step_disks() {
  local l
  for l in bls custom; do
    sudo -n podman exec -e LAB="$(to_work "$LAB")" rime-nix nix shell \
      "github:NixOS/nixpkgs/$PIN#"{util-linux,dosfstools,mtools,e2fsprogs,btrfs-progs,nixos-install-tools,python3,gnused,findutils,coreutils} \
      -c bash "$(to_work "$HERE")/mkdisk.sh" "$l" > "$ART/mkdisk-$l.log" 2>&1 \
      || { tail -30 "$ART/mkdisk-$l.log"; exit 1; }
    grep -v '^copying path' "$ART/mkdisk-$l.log" | tail -n 4
  done
}

step_scenarios() {
  local s rc=0 list=("$@")
  [[ ${#list[@]} -gt 0 ]] || list=(bls-sb-mok bls-sb-off custom-sb-mok custom-sb-off)
  for s in "${list[@]}"; do
    sudo -n rm -rf "${ART:?}/$s"   # the container's evidence is root-owned until the final chown
    [[ -f $ART/results.tsv ]] && sudo -n sed -i "/\t$s\t/d" "$ART/results.tsv"
    probe python3 -I /rig/drive.py "${s%%-*}" "${s#*-}" || rc=1
  done
  return "$rc"
}

rc=0
case "${1:-all}" in
  prep) step_prep;; gens) step_gens;; disks) step_disks;;
  scenarios) shift; step_scenarios "$@" || rc=$?;;
  all) step_prep; step_gens; step_disks; step_scenarios || rc=$?;;
  *) echo "usage: run.sh prep|gens|disks|scenarios [SCENARIO...]|all" >&2; exit 2;;
esac
sudo -n chown -R "$(id -u):$(id -g)" "$LAB" "$ART"
[[ -f $ART/results.tsv ]] && { echo; sort -k2,2 -k3,3 -s "$ART/results.tsv"; }
exit "$rc"
