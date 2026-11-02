#!/usr/bin/env bash
# run.sh: the TPM lab (p2-gates T1-T3; README.md here), from the host, with nothing installed on it and no host boot
# state touched. Nix runs in the rime-nix container; keys, firmware, swtpm and QEMU in rime-ovmf-tpm (Fedora 45: its
# shim, GRUB, Secure Boot OVMF, swtpm, tpm2-tools and signed kernel).
#   LAB  scratch (default /var/lab/rime-nix/scratch/tpmkeymap/lab): TEST keys, varstores, disks, TPM state
#   ART  evidence (default /var/lab/rime-nix/artifacts/tpmkeymap/tpm)
#   run.sh prep|system|disks|probes|run [SCENARIO]|all      SCENARIO = main (default) | mutant-bootc-reenrol | rebind-sb
#                                                           | mutant-rebind-skipped
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OVMF_RIG=$(cd "$HERE/.." && pwd)
LAB=$(realpath -m -- "${LAB:-/var/lab/rime-nix/scratch/tpmkeymap/lab}")
ART=$(realpath -m -- "${ART:-/var/lab/rime-nix/artifacts/tpmkeymap/tpm}")
for d in "$LAB" "$ART"; do   # both end in a recursive chown
  [[ $d == /var/lab/rime-nix/*/* ]] || { echo "run.sh: refusing $d (must be under /var/lab/rime-nix/<dir>/)" >&2; exit 2; }
done
PIN=1c89083e451e891152365b799595cf7790a43325
WT=$(cd "$HERE/../../../.." && pwd)
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
sysexpr() {   # ATTR [SIG]
  local sig=${2:+"signature = $(to_work "$LAB")/in/kernel.sig;"}
  echo "(import $(to_work "$HERE")/systems.nix { flake = builtins.getFlake \"path:$(to_work "$WT")\"; $sig }).$1"
}
nixb() { sudo -n podman exec rime-nix nix build --impure --no-link --print-out-paths --option cores 4 --expr "$1"; }
mkdir -p "$LAB" "$ART"
probe() { sudo -n podman run --rm --device /dev/kvm -v "$LAB:/lab:z" -v "$ART:/out:z" -v "$HERE:/rig:ro,z" \
  -e LAB=/lab -e OUT=/out --entrypoint "$1" rime-ovmf-tpm "${@:2}"; }
UUID=$(sudo -n podman exec rime-nix nix eval --impure --raw --expr "$(sysexpr luksUuid)")

step_prep() {
  sudo -n podman build -q -t rime-ovmf-probe "$OVMF_RIG" >/dev/null
  sudo -n podman build -q -t rime-ovmf-tpm "$HERE" >/dev/null
  # Fedora 45's signed shim/GRUB and bootupd's static GRUB config, as the OVMF probe extracted them (run-all.sh).
  local P=${OVMF_ART:-/var/lab/rime-nix/artifacts/ovmf}
  mkdir -p "$LAB/efi" "$LAB/in"; cp "$P"/work/efi/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$LAB/efi/"
  rm -rf "$LAB/grub2-static"; cp -r "$P/bootc-ref/grub2-static" "$LAB/"
  local k
  k=$(nixb "$(sysexpr unsignedKernel)")
  sudo -n podman exec rime-nix cp "$k/bzImage" "$(to_work "$LAB")/in/bzImage.unsigned"
  probe /bin/bash -euc '
    K=/lab/keys; mkdir -p $K /lab/fw
    if [[ ! -f $K/test-mok.key ]]; then
      openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
        -subj "/CN=Rime TPM lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout $K/test-mok.key -out $K/test-mok.crt 2>/dev/null
      openssl x509 -in $K/test-mok.crt -outform DER -out $K/test-mok.der
      cat /proc/sys/kernel/random/uuid > $K/test-mok.guid; chmod 600 $K/test-mok.key
      printf %s "rime-tpm-lab-TEST-passphrase" > $K/TEST-passphrase; chmod 600 $K/TEST-passphrase
    fi
    openssl x509 -in $K/test-mok.crt -noout -subject -fingerprint -sha256
    sbsign --key $K/test-mok.key --cert $K/test-mok.crt --detached --output /lab/in/kernel.sig /lab/in/bzImage.unsigned
    O=/usr/share/edk2/ovmf
    qemu-img convert -O raw $O/OVMF_CODE_4M.secboot.qcow2 /lab/fw/code-secboot.raw
    qemu-img convert -O raw $O/OVMF_VARS_4M.secboot.qcow2 /lab/fw/vars-secboot.raw
    virt-fw-vars --input /lab/fw/vars-secboot.raw --output /lab/fw/vars-sb-mok.fd \
      --add-mok "$(cat $K/test-mok.guid)" $K/test-mok.der >/dev/null
    virt-fw-vars --input /lab/fw/vars-sb-mok.fd --print 2>/dev/null | grep -E "SecureBoot|MokList|PK|KEK|^ *db|dbx" | head -20 || true
    rpm -q edk2-ovmf qemu-system-x86-core swtpm tpm2-tools
    sha256sum /lab/efi/shimx64.efi /lab/efi/grubx64.efi' | tee "$ART/prep.txt"
}

step_system() {
  local t
  t=$(nixb "$(sysexpr toplevel sig)")
  echo "toplevel $t" | tee "$ART/system.txt"
  printf '{"bls":{"gen1":"%s","gen2":"%s","gen3":"%s"}}\n' "$t" "$t" "$t" > "$LAB/gens.json"
}

step_disks() {
  # the NixOS disk: ../boothook/mkdisk.sh's fresh-install layout (nixos-install runs the Rime boot hook)
  sudo -n podman exec -e LAB="$(to_work "$LAB")" rime-nix nix shell \
    "github:NixOS/nixpkgs/$PIN#"{util-linux,dosfstools,mtools,e2fsprogs,btrfs-progs,nixos-install-tools,python3,gnused,findutils,coreutils} \
    -c bash "$(to_work "$OVMF_RIG")/boothook/mkdisk.sh" bls > "$ART/mkdisk-bls.log" 2>&1 \
    || { tail -30 "$ART/mkdisk-bls.log"; exit 1; }
  grep -v '^copying path' "$ART/mkdisk-bls.log" | tail -n 4
  step_probes
}

step_probes() {   # the "before" probe disks and the LUKS2 data disk (mkprobe.sh)
  probe /bin/bash /rig/mkprobe.sh "$UUID" 2>&1 | tee "$ART/mkprobe.txt"
}

step_run() {
  local s=${1:-main}
  sudo -n rm -rf "${ART:?}/$s"
  probe python3 -I /rig/drive.py "$s" "$UUID"
}

rc=0
case "${1:-all}" in
  prep) step_prep;; system) step_system;; disks) step_disks;; probes) step_probes;;
  run) shift; step_run "$@" || rc=$?;;
  all) step_prep; step_system; step_disks; step_run main || rc=$?;;
  *) echo "usage: run.sh prep|system|disks|probes|run [main|mutant-bootc-reenrol|rebind-sb|mutant-rebind-skipped]|all" >&2; exit 2;;
esac
sudo -n chown -R "$(id -u):$(id -g)" "$LAB" "$ART"
exit "$rc"
