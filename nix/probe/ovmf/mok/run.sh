#!/usr/bin/env bash
# run.sh: the MOK enrolment lab (README.md here), from the host, with nothing installed on it and no host boot state
# touched: every disk and varstore is a file, QEMU runs in the rime-ovmf-mok container.
#   LAB  scratch (default /var/lab/rime-nix/scratch/secureboot/mok): TEST keys, varstore, disk
#   ART  evidence (default /var/lab/rime-nix/artifacts/secureboot/mok)
#   BOOTC_KERNEL  a published bootc Rime kernel image (default: the running bootc system's own vmlinuz)
#   BOOTC_SYSMAP  that kernel's System.map (default: the one beside it); `split` breakpoints its microcode loader
#
#   run.sh prep      build the image; Fedora's shim/GRUB/MokManager; the bootc kernel; the Microsoft-only varstore
#   run.sh disks     mkdisk.sh: kernels, initrd, ESP
#   run.sh run       drive.py: control refusal, mokutil --import, MokManager by QMP keys, the Rime-signed boot,
#                    the TEST-MOK control, and the varstore's MokList read back
#   run.sh realsig DIR   PLAN.md D7 / p2-gates "release MOK": DIR holds nix-kernel-sign.yml's artefact
#                    (<hash>.sig + <hash>.json). Checks it names THIS commit's `rime` image, attaches it exactly as
#                    nix/pkgs/kernel/sign.nix does, sbverify against rime-mok.pem, and boots that NixOS kernel on the
#                    enrolled (Rime-MOK-only) varstore. Needs `run` first.
#   run.sh split     after `run`: early microcode as separate initrd files on the enrolled varstore, through
#                    Fedora GRUB's blscfg (one `initrd` line per file) and through a plain menuentry (one `initrd`
#                    line naming all three): rime-boot's split layout (docs/nixos/boot-matrix.md, "Early microcode");
#                    then the kernel's early loader itself over QEMU's gdbstub, guest CPU without the hypervisor flag
#   run.sh installer ILAB   the NixOS installer's OWN enrolment path. ILAB = the installer lab's LAB
#                    (nix/installer/lab/run.sh) after `install plain`: its ISO ran rime-install-nixos.sh on a
#                    Microsoft-keys-only varstore with mokpw set, so nixos_mok() ran `mokutil --import` of the lab's
#                    TEST MOK. drive.py installer boots that installed disk with the install's own varstore: shim
#                    starts MokManager (MI1, driven by QMP keys, screenshots), MokList then holds exactly the TEST MOK
#                    (V); iso-qualify.py firstboot then boots the TEST-signed NixOS kernel on THAT varstore with
#                    Secure Boot on and logs in (MI2). RIME_MOK_MUTANT=wrong-password: MokManager gets the wrong
#                    password; must end FAIL with nothing enrolled (MI2 is not run).
#   run.sh all       prep, disks, run
#   RIME_MOK_MUTANT=wrong-password run.sh run   the mutant: MokManager gets the wrong password; must end FAIL
#                    (nothing enrolled, the Rime-signed kernel still refused), evidence in $ART/run-mutant
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OVMF_RIG=$(cd "$HERE/.." && pwd)
WT=$(cd "$HERE/../../../.." && pwd)
LAB=$(realpath -m -- "${LAB:-/var/lab/rime-nix/scratch/secureboot/mok}")
ART=$(realpath -m -- "${ART:-/var/lab/rime-nix/artifacts/secureboot/mok}")
for d in "$LAB" "$ART"; do   # both end in a recursive chown
  [[ $d == /var/lab/rime-nix/*/* ]] || { echo "run.sh: refusing $d (must be under /var/lab/rime-nix/<dir>/)" >&2; exit 2; }
done
mkdir -p "$LAB" "$ART"
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
lab() { sudo -n podman run --rm --device /dev/kvm --security-opt label=disable \
  -v "$LAB:/lab" -v "$ART:/out" -v "$HERE:/rig:ro" -e LAB=/lab -e OUT=/out -e RIME_MOK_MUTANT="${RIME_MOK_MUTANT:-}" \
  --entrypoint "$1" rime-ovmf-mok "${@:2}"; }

step_prep() {
  sudo -n podman image exists rime-ovmf-probe || sudo -n podman build -q -t rime-ovmf-probe "$OVMF_RIG" >/dev/null
  sudo -n podman image exists rime-ovmf-tpm || sudo -n podman build -q -t rime-ovmf-tpm "$OVMF_RIG/tpm" >/dev/null
  sudo -n podman build -q -t rime-ovmf-mok "$HERE" >/dev/null
  local P=${OVMF_ART:-/var/lab/rime-nix/artifacts/ovmf}
  mkdir -p "$LAB/efi" "$LAB/in" "$LAB/fw"
  cp "$P"/work/efi/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$LAB/efi/"
  local k=${BOOTC_KERNEL:-/usr/lib/modules/$(uname -r)/vmlinuz}
  [[ -r $k ]] || { echo "run.sh: no bootc kernel at $k (set BOOTC_KERNEL)" >&2; exit 2; }
  cp "$k" "$LAB/in/vmlinuz.bootc"
  local m=${BOOTC_SYSMAP:-$(dirname "$k")/System.map}
  [[ -r $m ]] || { echo "run.sh: no System.map at $m (set BOOTC_SYSMAP)" >&2; exit 2; }
  cp "$m" "$LAB/in/System.map.bootc"
  cp "$WT/nix/pkgs/kernel/signing/rime-mok.pem" "$LAB/in/rime-mok.pem"
  # nixpkgs' early-microcode cpios, the ones the Rime toplevel names (for the split-initrd boots).
  local v u
  for v in microcode-intel:intel-ucode microcode-amd:amd-ucode; do
    u=$(sudo -n podman exec -w "$(to_work "$WT")" rime-nix nix build --no-link --print-out-paths \
      "path:.#nixosConfigurations.rime.pkgs.${v%%:*}")
    cp "/var/lab/rime-nix$u/${v#*:}.img" "$LAB/in/${v#*:}.img"
    echo "${v#*:}.img: $u ($(stat -c %s "$LAB/in/${v#*:}.img") bytes)"
  done
  lab /bin/bash -euc '
    O=/usr/share/edk2/ovmf
    qemu-img convert -O raw $O/OVMF_CODE_4M.secboot.qcow2 /lab/fw/code-secboot.raw
    # Microsoft keys only (Red Hat PK, Microsoft KEK + db): what a fresh PC ships with. No MOK of any kind.
    qemu-img convert -O raw $O/OVMF_VARS_4M.secboot.qcow2 /lab/fw/vars-ms-only.raw
    virt-fw-vars --input /lab/fw/vars-ms-only.raw --print 2>/dev/null | grep -iE "^ *(name|guid)|Mok|SecureBoot" | head -30 || true
    rpm -q edk2-ovmf qemu-system-x86-core shim-x64 mokutil 2>/dev/null || true
    sha256sum /lab/efi/*.efi /lab/in/vmlinuz.bootc /lab/in/System.map.bootc' | tee "$ART/prep.txt"
}

step_disks() { lab /bin/bash /rig/mkdisk.sh 2>&1 | tee "$ART/mkdisk.txt"; }

step_run() {
  sudo -n rm -rf "${ART:?}/run${RIME_MOK_MUTANT:+-mutant}"
  lab python3 -I /rig/drive.py run
}

step_realsig() {
  local dir=${1:?usage: run.sh realsig DIR}
  dir=$(realpath -- "$dir")
  local json=(/dev/null)
  shopt -s nullglob; json=("$dir"/*.json); shopt -u nullglob
  [[ ${#json[@]} -eq 1 ]] || { echo "run.sh realsig: $dir must hold exactly one <hash>.json (nix-kernel-sign.yml's artefact)" >&2; exit 2; }
  local h; h=$(basename "${json[0]}" .json)
  [[ -s $dir/$h.sig ]] || { echo "run.sh realsig: no $dir/$h.sig" >&2; exit 2; }
  local want; want=$(sudo -n podman exec -w "$(to_work "$WT")" rime-nix nix eval --raw \
    path:.#nixosConfigurations.rime.config.system.build.rimeKernelSigning.imageHash)
  echo "this commit's rime image hash: $want; artefact: $h" | tee "$ART/realsig.txt"
  [[ $h == "$want" ]] || { echo "run.sh realsig: the artefact signs $h, this commit's image is $want: refused" >&2; exit 1; }
  local sigdir="$WT/nix/pkgs/kernel/signing/signatures"
  cp "$dir/$h.sig" "$dir/$h.json" "$sigdir/"
  # sign.nix: sbattach + `sbverify --cert rime-mok.pem` inside the build; require = true makes a missing or wrong
  # signature an evaluation/build failure, never an unsigned kernel.
  local img
  img=$(sudo -n podman exec -w "$(to_work "$WT")" rime-nix nix build --no-link --print-out-paths \
    path:.#nixosConfigurations.rime.config.system.build.rimeKernelBootImage)
  echo "signed image $img" | tee -a "$ART/realsig.txt"
  sudo -n podman exec rime-nix cp "$img/bzImage" "$(to_work "$LAB")/in/nixos-real-signed.efi"
  lab /bin/bash -euc '
    sbverify --cert /lab/in/rime-mok.pem /lab/in/nixos-real-signed.efi
    mcopy -o -i /lab/disk.raw@@1M /lab/in/nixos-real-signed.efi ::/probe/nixos.efi' | tee -a "$ART/realsig.txt"
  lab python3 -I /rig/drive.py realsig
}

step_installer() {
  local il=${1:?usage: run.sh installer INSTALLER_LAB}
  il=$(realpath -- "$il")
  local d=$il/run-plain
  [[ -s $d/disk.raw && -s $d/install-plain.vars.fd && -s $il/keys/test-mok.der && -s $il/system ]] || {
    echo "run.sh installer: $il has no finished \`install plain\` (run-plain/disk.raw, install-plain.vars.fd, keys, system)" >&2
    exit 2; }
  local pw sha out=$ART/installer${RIME_MOK_MUTANT:+-mutant} r=0
  pw=$(sed -n 's/^mokpw=//p' "$d/fs/answers.in")
  sha=$(sha256sum < "$il/keys/test-mok.der" | cut -d' ' -f1)
  [[ -n $pw ]] || { echo "run.sh installer: the install's answers have no mokpw" >&2; exit 2; }
  sudo -n rm -rf "${out:?}"
  # The installed disk is used as it is (MokManager writes only the varstore copy, never the disk).
  sudo -n podman run --rm --device /dev/kvm --security-opt label=disable \
    -v "$LAB:/lab" -v "$ART:/out" -v "$HERE:/rig:ro" -v "$il:/inst" -e LAB=/lab -e OUT=/out \
    -e RIME_MOK_MUTANT="${RIME_MOK_MUTANT:-}" -e MOK_CODE=/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd \
    -e MOK_DISK=/inst/run-plain/disk.raw -e MOK_VARS=/inst/run-plain/install-plain.vars.fd \
    -e MOK_PASSWORD="$pw" -e MOK_CERT_SHA256="$sha" \
    --entrypoint python3 rime-ovmf-mok -I /rig/drive.py installer || r=$?
  sudo -n chown -R "$(id -u):$(id -g)" "$out"
  [[ $r -eq 0 && -z ${RIME_MOK_MUTANT:-} ]] || return "$r"
  # MI2: the installed NixOS (kernel signed by the TEST MOK) boots on the varstore MokManager just wrote.
  sudo -n podman run --rm --device /dev/kvm --security-opt label=disable \
    -v "$il:/lab" -v "$out:/res" -v "$WT:/rig:ro" -e LAB=/lab --entrypoint python3 rime-installer-lab \
    -I /rig/installer/iso-qualify.py firstboot --work /res --name MI2-firstboot --disk /lab/run-plain/disk.raw \
    --nixos --display --vars-template /res/enrolled.vars.fd --user alex \
    --password "$(sed -n 's/^password=//p' "$d/fs/answers.in")" --timeout 600 --expect-system "$(cat "$il/system")" \
    --expect-host rime-lab-plain --expect-tz Australia/Perth --expect-lang en_AU.UTF-8 \
    > "$out/MI2-firstboot.log" 2>&1 || r=$?
  cat "$out/MI2-firstboot.log"
  echo "MI2-firstboot rc=$r" | tee -a "$out/results.tsv"
  return "$r"
}

rc=0
case "${1:-all}" in
  prep) step_prep;; disks) step_disks;; run) step_run || rc=$?;;
  realsig) shift; step_realsig "$@" || rc=$?;;
  installer) shift; step_installer "$@" || rc=$?;;
  split) sudo -n rm -rf "${ART:?}/split"; lab python3 -I /rig/drive.py split || rc=$?;;
  all) step_prep; step_disks; step_run || rc=$?;;
  *) echo "usage: run.sh prep|disks|run|realsig DIR|split|installer ILAB|all" >&2; exit 2;;
esac
sudo -n chown -R "$(id -u):$(id -g)" "$LAB" "$ART"
exit "$rc"
