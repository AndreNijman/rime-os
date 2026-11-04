#!/usr/bin/env bash
# run.sh: the boot-hook OVMF lab, from the host, with nothing installed on it and no host boot state touched
# (README.md here). Nix runs in the rime-nix container; keys, firmware and QEMU in rime-ovmf-probe (Fedora 45:
# its shim, GRUB and Secure Boot OVMF).
#   LAB  scratch (default /var/lab/rime-nix/scratch/boothook): TEST keys, varstores, generations, disks
#   ART  evidence (default /var/lab/rime-nix/artifacts/boothook)
#   run.sh [prep|gens|disks|scenarios|all] [SCENARIO...]   SCENARIO = bls-sb-mok, bls-sb-off, custom-sb-mok, custom-sb-off
#   run.sh disks [LAYOUT...]  only these base disks (bls, custom)
#   run.sh sbrecovery         Secure Boot recovery on the bls disk: SB on with the TEST MOK NOT enrolled, the hook's
#                             "Enrol the Rime OS Secure Boot key" entry -> MokManager -> enrol from disk -> boots;
#                             SB off -> no entry (drive.py SbRecovery)
#
# The P2 boot matrix (matrix.nix; docs/nixos/boot-matrix.md), with LAB/ART defaulting to .../bootmatrix:
#   run.sh matrix-prep        firmware, Fedora's shim/GRUB, the TEST MOK, varstores
#   run.sh matrix-sign        the real Rime kernel image(s) of every cell signed by sign-detached.sh (TEST MOK)
#   run.sh matrix-gens        gen1/gen2 of every cell, built with those signatures; the LUKS writer VM
#   run.sh matrix-disks [CELL...]   one base disk per cell (LUKS2 written inside the writer VM, never on the host)
#   run.sh matrix-write CELL...     only the writer VM step of matrix-disks, again (a LUKS cell's disk.raw + payload)
#   run.sh matrix-run [CELL-FW...]  CELL = bls-plain|bls-luks|custom-plain|custom-luks, FW = sb-mok|sb-off
#   run.sh matrix             all of the above
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OVMF_RIG=$(cd "$HERE/.." && pwd)
case ${1:-all} in
  matrix*) DEF=bootmatrix;;
  *) DEF=boothook;;
esac
LAB=$(realpath -m -- "${LAB:-/var/lab/rime-nix/scratch/$DEF}")
ART=$(realpath -m -- "${ART:-/var/lab/rime-nix/artifacts/$DEF}")
for d in "$LAB" "$ART"; do   # both end in a recursive chown
  [[ $d == /var/lab/rime-nix/*/* ]] || { echo "run.sh: refusing $d (must be under /var/lab/rime-nix/<dir>/)" >&2; exit 2; }
done
PIN=1c89083e451e891152365b799595cf7790a43325
WT=$(cd "$HERE/../../../.." && pwd)
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
mkdir -p "$LAB" "$ART"
# No SELinux relabel (:z) of the mounts: it walks $LAB recursively, including a disk tree mkdisk.sh is building
# (immutable /var/empty: the relabel fails the run). The probe is a lab container with no host paths but these.
probe() { sudo -n podman run --rm --device /dev/kvm --security-opt label=disable -v "$LAB:/lab" -v "$ART:/out" -v "$HERE:/rig:ro" \
  -e LAB=/lab -e OUT=/out --entrypoint "$1" rime-ovmf-probe "${@:2}"; }

prep_firmware() {
  sudo -n podman build -q -t rime-ovmf-probe "$OVMF_RIG" >/dev/null
  # Fedora 45's signed shim/GRUB and bootupd's static GRUB config, as the OVMF probe extracted them (run-all.sh).
  local P=${OVMF_ART:-/var/lab/rime-nix/artifacts/ovmf}
  mkdir -p "$LAB/efi"; cp "$P"/work/efi/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$LAB/efi/"
  rm -rf "$LAB/grub2-static"; cp -r "$P/bootc-ref/grub2-static" "$LAB/"
  probe /bin/bash -euc '
    K=/lab/keys; mkdir -p $K /lab/fw
    if [[ ! -f $K/test-mok.key ]]; then
      openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
        -subj "/CN=Rime boot-hook lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout $K/test-mok.key -out $K/test-mok.crt 2>/dev/null
      openssl x509 -in $K/test-mok.crt -outform DER -out $K/test-mok.der
      cat /proc/sys/kernel/random/uuid > $K/test-mok.guid; chmod 600 $K/test-mok.key
    fi
    openssl x509 -in $K/test-mok.crt -noout -subject -fingerprint -sha256
    O=/usr/share/edk2/ovmf
    qemu-img convert -O raw $O/OVMF_CODE_4M.secboot.qcow2 /lab/fw/code-secboot.raw
    qemu-img convert -O raw $O/OVMF_VARS_4M.secboot.qcow2 /lab/fw/vars-secboot.raw
    virt-fw-vars --input /lab/fw/vars-secboot.raw --output /lab/fw/vars-sb-mok.fd \
      --add-mok "$(cat $K/test-mok.guid)" $K/test-mok.der --set-shim-verbose >/dev/null
    qemu-img convert -O raw $O/OVMF_VARS_4M.qcow2 /lab/fw/vars-sb-off.fd
    rpm -q edk2-ovmf qemu-system-x86-core'
}

step_prep() {
  prep_firmware | tee "$ART/prep.txt"
  # The unsigned kernel of every lab generation, and a detached signature by a THROWAWAY TEST MOK.
  mkdir -p "$LAB/in"
  local k
  k=$(sudo -n podman exec rime-nix nix build --impure --no-link --print-out-paths --expr \
    "(import $(to_work "$HERE")/systems.nix { flake = builtins.getFlake \"path:$(to_work "$WT")\"; }).unsignedKernel")
  sudo -n podman exec rime-nix cp "$k/bzImage" "$(to_work "$LAB")/in/bzImage.unsigned"
  probe /bin/bash -euc 'K=/lab/keys
    sbsign --key $K/test-mok.key --cert $K/test-mok.crt --detached --output /lab/in/kernel.sig /lab/in/bzImage.unsigned' \
    | tee -a "$ART/prep.txt"
}

# ── the P2 boot matrix ──
CLAB=$(to_work "$LAB")
mexpr() {   # mexpr ATTR [signed]: an attribute of matrix.nix, pass 1 (unsigned) or pass 2 (signatureDir = $LAB/sig)
  local args="flake = builtins.getFlake \"path:$(to_work "$WT")\";"
  [[ ${2:-} == signed ]] && args+=" signatureDir = $CLAB/sig; certificate = $CLAB/keys/test-mok.crt;"
  echo "(import $(to_work "$HERE")/matrix.nix { $args }).$1"
}
nixc() { sudo -n podman exec rime-nix nix "$@"; }

step_matrix_sign() {
  # kernsign's path with a TEST MOK: sign-detached.sh (the script nix-kernel-sign.yml runs) over each unsigned
  # system.build.rimeKernelImage, written as <signatureDir>/<image hash>.sig for rime.kernel.signing to attach.
  local js n i h img tgt
  js=$(nixc eval --impure --raw --expr "$(mexpr imagesJson)")
  echo "$js" > "$ART/images.json"
  n=$(python3 -c 'import json,sys; print(len(json.loads(sys.argv[1])))' "$js")
  mkdir -p "$LAB/in" "$LAB/sig"
  cp "$WT/nix/pkgs/kernel/sign-detached.sh" "$LAB/in/"
  for ((i=0; i<n; i++)); do
    read -r h img tgt < <(python3 -c 'import json,sys; d=json.loads(sys.argv[1])[int(sys.argv[2])]; print(d["imageHash"], d["image"], d["target"])' "$js" "$i")
    nixc build --no-link "$img"
    sudo -n podman exec rime-nix cp "$img/$tgt" "$CLAB/in/$h.unsigned"
    probe /bin/bash -euc "bash /lab/in/sign-detached.sh --image /lab/in/$h.unsigned --hash $h \
      --key /lab/keys/test-mok.key --cert /lab/keys/test-mok.crt --pinned /lab/keys/test-mok.crt --out /lab/sig \
      --store-path $img --note 'bootmatrix lab, THROWAWAY TEST MOK'"
  done
  cp "$LAB"/sig/*.json "$ART/" 2>/dev/null || sudo -n cp "$LAB"/sig/*.json "$ART/"
}

step_matrix_gens() {
  local c g p json='{'
  for c in "${MATRIX_CELLS[@]}"; do
    json+="\"$c\":{"
    for g in gen1 gen2; do
      p=$(nixc build --impure --no-link --print-out-paths --option cores 4 --expr "$(mexpr "$c.$g" signed)" 2>>"$LAB/matrix-gens.log") \
        || { tail -n 30 "$LAB/matrix-gens.log"; exit 1; }
      echo "$c.$g $p"
      json+="\"$g\":\"$p\","
    done
    json="${json%,}},"
  done
  echo "${json%,}}" > "$LAB/matrix-gens.json"
  cp "$LAB/matrix-gens.json" "$ART/"
  p=$(nixc build --impure --no-link --print-out-paths --expr "$(mexpr luksWriter)")
  mkdir -p "$LAB/writer"
  sudo -n podman exec rime-nix sh -c "cp -L $p/kernel $p/initrd $CLAB/writer/"
  echo "luks writer $p"
}

step_matrix_disks() {
  local c list=("$@")
  [[ ${#list[@]} -gt 0 ]] || list=("${MATRIX_CELLS[@]}")
  for c in "${list[@]}"; do
    sudo -n podman exec -e LAB="$CLAB" -e GENS="$CLAB/matrix-gens.json" rime-nix nix shell \
      "github:NixOS/nixpkgs/$PIN#"{util-linux,dosfstools,mtools,e2fsprogs,btrfs-progs,nixos-install-tools,python3,gnused,findutils,coreutils} \
      -c bash "$(to_work "$HERE")/mkdisk.sh" "$c" > "$ART/mkdisk-$c.log" 2>&1 \
      || { tail -30 "$ART/mkdisk-$c.log"; exit 1; }
    grep -v '^copying path' "$ART/mkdisk-$c.log" | tail -n 4
    step_matrix_write "$c"
    sudo -n du -h --apparent-size "$LAB/disks/$c/disk.raw"; sudo -n du -h "$LAB/disks/$c/disk.raw"
  done
}

step_matrix_write() {   # CELL...: a LUKS cell's volume, formatted and filled in the writer VM (no-op for plain)
  local c
  for c in "$@"; do
    if [[ -f $LAB/disks/$c/luks.env ]]; then
      # The LUKS2 volume is formatted and filled inside a throwaway VM (matrix.nix luksWriter).
      probe /bin/bash -euc "source /lab/disks/$c/luks.env
        qemu-system-x86_64 -machine q35,accel=kvm -cpu host -m 2048 -smp 2 -nographic -no-reboot -nic none \
          -kernel /lab/writer/kernel -initrd /lab/writer/initrd \
          -append \"console=ttyS0 panic=-1 rime.part=\$part rime.uuid=\$uuid rime.fs=\$fs\" \
          -fw_cfg name=opt/rime/passphrase,string=\$pass \
          -drive if=virtio,format=raw,file=/lab/disks/$c/disk.raw \
          -drive if=virtio,format=raw,readonly=on,file=/lab/disks/$c/payload.img" > "$ART/luks-writer-$c.log" 2>&1 || true
      grep 'RIME-WRITER' "$ART/luks-writer-$c.log" || true
      grep -q 'RIME-WRITER: DONE rc=0' "$ART/luks-writer-$c.log" || { echo "luks writer failed for $c" >&2; exit 1; }
      sudo -n rm -f "$LAB/disks/$c/payload.img"
    fi
  done
}

step_matrix_run() {
  local s rc=0 list=("$@") c f pids=() n=0
  if [[ ${#list[@]} -eq 0 ]]; then
    for c in "${MATRIX_CELLS[@]}"; do for f in sb-mok sb-off; do list+=("$c-$f"); done; done
  fi
  for s in "${list[@]}"; do
    sudo -n rm -rf "${ART:?}/$s"
    [[ -f $ART/results.tsv ]] && sudo -n sed -i "/\t$s\t/d" "$ART/results.tsv"
  done
  # Two VMs at a time (4 GiB, 4 vCPUs each): katana also runs CI and other labs.
  for s in "${list[@]}"; do
    probe python3 -I /rig/drive.py matrix "${s%-sb-*}" "sb-${s##*-sb-}" > "$ART/drive-$s.log" 2>&1 &
    pids+=($!); n=$((n + 1))
    if (( n % 2 == 0 )); then for p in "${pids[@]}"; do wait "$p" || rc=1; done; pids=(); fi
  done
  for p in "${pids[@]}"; do wait "$p" || rc=1; done
  return "$rc"
}
MATRIX_CELLS=(bls-plain bls-luks custom-plain custom-luks)

step_gens() { LAB=$LAB WT=$WT "$HERE/gens.sh" | tee "$ART/gens.txt"; cp "$LAB/gens.json" "$ART/"; }

step_disks() {
  local l list=("$@")
  [[ ${#list[@]} -gt 0 ]] || list=(bls custom)
  for l in "${list[@]}"; do
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

step_sbrecovery() {
  sudo -n rm -rf "${ART:?}/sbrecovery-bls"
  [[ -f $ART/results.tsv ]] && sudo -n sed -i "/\tsbrecovery-bls\t/d" "$ART/results.tsv"
  probe python3 -I /rig/drive.py sbrecovery bls
}

rc=0
case "${1:-all}" in
  prep) step_prep;; gens) step_gens;; disks) shift; step_disks "$@";;
  sbrecovery) step_sbrecovery || rc=$?;;
  scenarios) shift; step_scenarios "$@" || rc=$?;;
  all) step_prep; step_gens; step_disks; step_scenarios || rc=$?;;
  matrix-prep) prep_firmware | tee "$ART/prep.txt";;
  matrix-sign) step_matrix_sign;;
  matrix-gens) step_matrix_gens;;
  matrix-disks) shift; step_matrix_disks "$@";;
  matrix-write) shift; step_matrix_write "$@";;
  matrix-run) shift; step_matrix_run "$@" || rc=$?;;
  matrix) prep_firmware | tee "$ART/prep.txt"; step_matrix_sign; step_matrix_gens; step_matrix_disks
          step_matrix_run || rc=$?;;
  *) echo "usage: run.sh prep|gens|disks [LAYOUT...]|scenarios|sbrecovery [SCENARIO...]|all|matrix-prep|matrix-sign|matrix-gens|matrix-disks|matrix-run|matrix" >&2; exit 2;;
esac
# Never into $LAB/disks: a disk being built there (another run.sh) holds a root-owned NixOS tree.
sudo -n find "$LAB" "$ART" -path "$LAB/disks" -prune -o -exec chown "$(id -u):$(id -g)" {} +
[[ -f $ART/results.tsv ]] && { echo; sort -k2,2 -k3,3 -s "$ART/results.tsv"; }
exit "$rc"
