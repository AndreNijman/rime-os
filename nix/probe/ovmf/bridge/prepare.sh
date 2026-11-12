#!/usr/bin/env bash
# prepare.sh STEP...: makes every input of the bridge OVMF lab (README.md here) under $LAB, on the HOST, through
# containers only: the rime-nix container (Nix), localhost/rime-ovmf-probe (openssl, sbsigntools, virt-fw-vars) and the
# published bootc image (the rime build, mkbootc.sh). Nothing here touches the host's disks, ESP or NVRAM, and nothing
# it makes is committed: every key is a TEST key under $LAB/keys.
#
#   keys      TEST MOK (signs the lab's Rime kernel) + TEST payload key (rime-bridge-lab-TEST-1) + the real Rime MOK
#             certificate (public, nix/pkgs/kernel/signing/rime-mok.pem: signs the published bootc kernel)
#   vars      fw/vars-sb-bridge.fd: Fedora's enrolled SB varstore + MokList = [Rime MOK, TEST MOK]
#   kernel    pass 1 of system.nix (imageJson), the image signed detached by sign-detached.sh with the TEST MOK
#   toplevel  pass 2 of system.nix: the signed system the payload carries (prints its path into $LAB/toplevel)
#   payload   payload.img: payload.sh with the bridge as the readiness image ships it and the TEST payload key
#   image     the readiness layer's files in $LAB/layer, from `git archive` of the readiness branch (READ only): its
#             `rime`, built inside the published image as Containerfile.base builds it (rust + cargo in a container
#             that is thrown away, so it links against the image's own libraries), its rime-migrate, and the TEST
#             payload key's public line where the release will put rime-bridge-payload-1's
#   oci       the published image as an OCI layout in $LAB/oci + ONE layer with those files (oci-layer.py): the
#             READINESS image mkbootc.sh deploys with ostree
#   disk-A, disk-L   mkbootc.sh A|L inside the published image (L needs dm_crypt on the host: see README)
#
# RIME_LAB_KERNEL=release (topology-e): the NixOS kernel carries the RELEASE signature committed in
# nix/pkgs/kernel/signing/signatures/ (system.nix releaseKernel), not a TEST MOK one: `kernel` signs nothing, `vars`
# enrols ONLY the real Rime MOK certificate, `toplevel` records the image hash and `sbverify` against rime-mok.pem.
set -euo pipefail
RIG=$(cd "$(dirname "$0")" && pwd)
TREE=$(cd "$RIG/../../../.." && pwd)             # the rime-os worktree (= /work/<name> in rime-nix)
LAB=${LAB:-/var/lab/rime-nix/scratch/bridge3}
ART=${ART:-/var/lab/rime-nix/artifacts/bridge3}
READINESS=${READINESS:-/var/lab/rime-nix/wt-readiness}
BASE=${BASE:-ghcr.io/andrenijman/rime-os@sha256:0ace1f6dad51597d79a7d0d84135dd2896fc89d748ec74b9ecfc7c98c92df9bf}
OVMF_WORK=${OVMF_WORK:-/var/lab/rime-nix/artifacts/ovmf/work}
PIN=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
NIXJ=(--max-jobs "${RIME_LAB_JOBS:-2}" --option cores "${RIME_LAB_CORES:-4}")
KERNEL=${RIME_LAB_KERNEL:-test}
[[ $KERNEL == test || $KERNEL == release ]] || { echo "prepare: RIME_LAB_KERNEL is test or release" >&2; exit 2; }
work() { echo "/work/${1#/var/lab/rime-nix/}"; }   # host path under /var/lab/rime-nix -> the rime-nix container's
NIX() { sudo -n podman exec -w "$(work "$TREE")" rime-nix "$@"; }
PROBE() { sudo -n podman run --rm --security-opt label=disable -v "$LAB:/lab" -v "$TREE:/tree:ro" --entrypoint bash \
  localhost/rime-ovmf-probe -c "$1"; }
say() { printf '>> %s\n' "$*"; }
mkdir -p "$LAB/keys" "$LAB/fw" "$LAB/sig" "$ART"
sudo -n chown "$(id -u):$(id -g)" "$LAB" "$LAB/keys" "$LAB/fw" "$LAB/sig" 2>/dev/null || true

step_keys() {
  PROBE 'set -e; K=/lab/keys; cd $K
    if [ ! -f test-mok.key ]; then
      openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
        -subj "/CN=Rime bridge lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout test-mok.key -out test-mok.crt 2>/dev/null
      openssl x509 -in test-mok.crt -outform DER -out test-mok.der
      cat /proc/sys/kernel/random/uuid > test-mok.guid; chmod 600 test-mok.key
    fi
    cp /tree/nix/pkgs/kernel/signing/rime-mok.pem rime-mok.pem
    openssl x509 -in rime-mok.pem -outform DER -out rime-mok.der
    for c in rime-mok.pem test-mok.crt; do openssl x509 -in $c -noout -subject -fingerprint -sha256; done'
  if [[ ! -f $LAB/keys/payload-TEST.sec ]]; then
    NIX bash -c "cd $(work "$LAB")/keys && nix key generate-secret --key-name rime-bridge-lab-TEST-1 > payload-TEST.sec \
      && nix key convert-secret-to-public < payload-TEST.sec > payload-TEST.pub && chmod 600 payload-TEST.sec"
  fi
  echo "payload key: $(cat "$LAB/keys/payload-TEST.pub")"
}

step_vars() {
  cp "$OVMF_WORK/vars-secboot.raw" "$LAB/fw/vars-secboot.raw"
  # shellcheck disable=SC2016 # expanded by the PROBE shell, not here
  local test_mok='--add-mok "$g" keys/test-mok.der'
  [[ $KERNEL == release ]] && test_mok=   # the release-signed kernel: MokList = the Rime MOK and nothing else
  PROBE 'set -e; cd /lab; g=$(cat keys/test-mok.guid)
    virt-fw-vars --input /lab/fw/vars-secboot.raw --output fw/vars-sb-bridge.fd \
      --add-mok "$g" keys/rime-mok.der '"$test_mok"' --set-shim-verbose
    virt-fw-vars --input fw/vars-sb-bridge.fd --print --verbose 2>&1 | grep -E "^name=|subject CN=" | head -40'
}

step_kernel() {
  local j img hash expr
  if [[ $KERNEL == release ]]; then echo "kernel: RIME_LAB_KERNEL=release, nothing to sign (the committed signature)"; return; fi
  expr="(import ./nix/probe/ovmf/bridge/system.nix { flake = builtins.getFlake \"path:$(work "$TREE")\"; })"
  j=$(NIX nix eval --raw --impure --expr "$expr.imageJson")
  echo "$j" > "$LAB/sig/image.json"
  img=$(NIX nix build --no-link --print-out-paths --impure "${NIXJ[@]}" --expr "$expr.image")
  [[ $img == "$(echo "$j" | python3 -c 'import json,sys; print(json.load(sys.stdin)["image"])')" ]] \
    || { echo "prepare: built image $img is not imageJson's" >&2; exit 1; }
  hash=$(echo "$j" | python3 -c 'import json,sys; print(json.load(sys.stdin)["imageHash"])')
  target=$(echo "$j" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target"])')
  sudo -n cp "/var/lab/rime-nix/nix/store/${img#/nix/store/}/$target" "$LAB/sig/$hash.unsigned"
  sudo -n chown "$(id -u):$(id -g)" "$LAB/sig/$hash.unsigned"; chmod 644 "$LAB/sig/$hash.unsigned"
  PROBE "set -e; bash /tree/nix/pkgs/kernel/sign-detached.sh --image /lab/sig/$hash.unsigned --hash $hash \
    --key /lab/keys/test-mok.key --cert /lab/keys/test-mok.crt --pinned /lab/keys/test-mok.crt --out /lab/sig \
    --store-path $img/$target --note 'bridge OVMF lab, throwaway TEST MOK'" | tee "$ART/sign.txt"
  rm -f "$LAB/sig/$hash.unsigned"
}

step_toplevel() {
  local t args expr
  if [[ $KERNEL == release ]]; then
    args="releaseKernel = true;"
  else
    args="signatureDir = $(work "$LAB")/sig; certificate = $(work "$LAB")/keys/test-mok.crt;"
  fi
  expr="(import ./nix/probe/ovmf/bridge/system.nix { flake = builtins.getFlake \"path:$(work "$TREE")\"; $args })"
  t=$(NIX nix build --no-link --print-out-paths --impure "${NIXJ[@]}" --expr "$expr.toplevel")
  echo "$t" | tee "$LAB/toplevel"
  if [[ $KERNEL == release ]]; then
    # The booted kernel is the release-signed one: its image hash names a committed .sig, and sbverify accepts the
    # attached signature against the Rime MOK certificate (sign.nix already refused to build otherwise).
    { echo "kernel: release signature (RIME_LAB_KERNEL=release)"
      NIX nix eval --raw --impure --expr "$expr.imageJson"; echo
      ls -la "$TREE/nix/pkgs/kernel/signing/signatures/"
      # resolved in the container: on the host /nix/store does not exist, so readlink -f there answers nothing
      echo "toplevel kernel: $(NIX nix shell $PIN#coreutils -c readlink -f "$t/kernel")"
    } | tee "$ART/kernel-release.txt"
    local k; k=$(NIX nix shell $PIN#coreutils -c readlink -f "$t/kernel")
    [[ $k == /nix/store/* ]] || { echo "prepare: the toplevel's kernel did not resolve ($k)"; exit 1; }
    sudo -n cp "/var/lab/rime-nix/nix/store/${k#/nix/store/}" "$LAB/sig/nixos-kernel.efi"
    sudo -n chown "$(id -u):$(id -g)" "$LAB/sig/nixos-kernel.efi"
    PROBE 'set -e; sbverify --cert /lab/keys/rime-mok.pem /lab/sig/nixos-kernel.efi
      sbverify --cert /lab/keys/test-mok.crt /lab/sig/nixos-kernel.efi 2>&1 && echo "UNEXPECTED: the TEST MOK verifies it" || echo "the TEST MOK does not verify it (as wanted)"
      sbverify --list /lab/sig/nixos-kernel.efi' 2>&1 | tee -a "$ART/kernel-release.txt"
    rm -f "$LAB/sig/nixos-kernel.efi"
  fi
}

step_payload() {
  local b
  b=$(NIX nix build --no-link --print-out-paths "${NIXJ[@]}" "path:$(work "$TREE")#rime-bridge")
  NIX nix shell $PIN#{zstd,e2fsprogs,python3,gnutar,coreutils,gnused,gnugrep,bash,openssl,findutils,diffutils} -c \
    env LAB="$(work "$LAB")" bash nix/probe/ovmf/bridge/payload.sh "$(cat "$LAB/toplevel")" "$b" \
    "$(work "$LAB")/keys/payload-TEST.sec" 2>&1 | tee "$ART/payload.log"
}

step_image() {
  local c=$LAB/ctx l=$LAB/layer
  sudo -n rm -rf "$c" "$l"; mkdir -p "$c" "$l/usr/bin" "$l/usr/libexec" "$l/usr/share/rime-os/trust"
  # Every file #137 changes in the image (`git diff main...` on the branch: Containerfile.base COPYs rime-migrate and
  # only tests it; rime-update-check gains the bridge-record predicate; `rime` gains `migrate` and the guard).
  git -C "$READINESS" archive HEAD rimed config files/system/libexec/rime-migrate files/system/libexec/rime-update-check \
    | tar -C "$c" -xf -
  { echo "readiness branch: $(git -C "$READINESS" rev-parse HEAD)"
    echo "rime-migrate minus line 1: $(tail -n +2 "$c/files/system/libexec/rime-migrate" | sha256sum | cut -d' ' -f1)"
    echo "rime-bridge.py minus line 1: $(tail -n +2 "$TREE/nix/pkgs/rime-bridge/rime-bridge.py" | sha256sum | cut -d' ' -f1)"
    echo "base image: $BASE"
  } | tee "$ART/readiness-image.txt"
  local m1 m2
  m1=$(tail -n +2 "$c/files/system/libexec/rime-migrate" | sha256sum | cut -d' ' -f1)
  m2=$(tail -n +2 "$TREE/nix/pkgs/rime-bridge/rime-bridge.py" | sha256sum | cut -d' ' -f1)
  if [[ $m1 != "$m2" ]]; then
    echo "prepare: NOTE the readiness branch's rime-migrate is not this tree's rime-bridge.py (minus line 1)" | tee -a "$ART/readiness-image.txt"
    [[ ${RIME_LAB_ALLOW_BRIDGE_DRIFT:-} == 1 ]] || { echo "prepare: refusing (RIME_LAB_ALLOW_BRIDGE_DRIFT=1 to test #137's copy anyway)" >&2; exit 1; }
  fi
  # As Containerfile.base's rime-builder stage: FROM the image, rust + cargo, CARGO_HOME in the build tree.
  sudo -n podman run --rm --security-opt label=disable -v "$c:/build" -e CARGO_HOME=/build/.cargo \
    -e CARGO_TARGET_DIR=/build/target --entrypoint bash "$BASE" -c 'set -eux
      dnf5 -y install --setopt=install_weak_deps=False rust cargo >/dev/null
      cd /build/rimed && cargo build --release --locked --bin rime
      /build/target/release/rime --help 2>&1 | grep -w migrate
      # Containerfile.base`s own checks of the bridge, in the image it lands in
      B=/build/files/system/libexec/rime-migrate
      test "$(head -n1 $B)" = "#!/usr/bin/python3 -I"
      python3 -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" $B
      python3 -I $B --help >/dev/null
      command -v btrfs lsblk findmnt mount umount unshare chroot tar zstd cp systemctl ostree cryptsetup
      bash -n /build/files/system/libexec/rime-update-check' 2>&1 | tail -n 40 | tee "$ART/readiness-rime-build.log"
  sudo -n chown -R "$(id -u):$(id -g)" "$c"
  install -m755 "$c/target/release/rime" "$l/usr/bin/rime"
  install -m755 "$c/files/system/libexec/rime-migrate" "$l/usr/libexec/rime-migrate"
  install -m755 "$c/files/system/libexec/rime-update-check" "$l/usr/libexec/rime-update-check"
  printf '%s\n' "$(cat "$LAB/keys/payload-TEST.pub")" > "$l/usr/share/rime-os/trust/nixos-bridge-payload.pub"
  chmod 644 "$l/usr/share/rime-os/trust/nixos-bridge-payload.pub"
  [[ $(head -n 1 "$l/usr/libexec/rime-migrate") == '#!/usr/bin/python3 -I' ]] || { echo "prepare: rime-migrate shebang" >&2; exit 1; }
  grep -Ex 'rime-bridge-lab-TEST-[0-9]+:[A-Za-z0-9+/]{43}=' "$l/usr/share/rime-os/trust/nixos-bridge-payload.pub"
  (cd "$l" && find . -type f | sort | xargs sha256sum) | tee -a "$ART/readiness-image.txt"
  rm -rf "$c/target" "$c/.cargo"
}

step_oci() {
  sudo -n rm -rf "$LAB/oci"
  sudo -n podman save --format oci-dir -o "$LAB/oci" "$BASE"
  sudo -n chown -R "$(id -u):$(id -g)" "$LAB/oci"
  python3 "$RIG/oci-layer.py" "$LAB/oci" "$LAB/layer" \
    "readiness $(git -C "$READINESS" rev-parse --short HEAD): rime, rime-migrate, rime-update-check, TEST nixos-bridge-payload.pub" \
    | tee -a "$ART/readiness-image.txt"
  du -sh "$LAB/oci"
}

step_disk() {   # step_disk KIND: the published image's tools deploy $LAB/oci (the readiness image)
  sudo -n podman run --rm --privileged --security-opt label=disable -e RIME_LAB_PASS=rime-lab-OLD-1 \
    -e RIME_LAB_LUKS=rimelabluks1 -e DM_DISABLE_UDEV=1 \
    -e RIME_LAB_IMAGE="$BASE + readiness layer ($(git -C "$READINESS" rev-parse --short HEAD))" \
    -v "$LAB:/lab" -v "$RIG:/rig:ro" --entrypoint bash "$BASE" /rig/mkbootc.sh "$1" 2>&1 | tee "$ART/mkbootc-$1.log"
}

for s in "$@"; do
  say "step $s"
  case $s in
    keys|vars|kernel|toplevel|payload|image|oci) "step_$s" ;;
    disk-A|disk-L|disk-E) step_disk "${s#disk-}" ;;
    *) echo "prepare: unknown step $s" >&2; exit 2 ;;
  esac
done
