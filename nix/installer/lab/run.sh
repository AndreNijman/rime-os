#!/usr/bin/env bash
# run.sh: the NixOS installer's OVMF lab (p2-gates I1-I3), from the host, with nothing installed on it and no host
# boot state touched. Nix runs in the rime-nix container; qemu, OVMF and keys in rime-installer-lab (the OVMF probe
# image + tesseract); the ISO is built by installer/build-live-iso.sh exactly as for release, in its NixOS mode.
#
#   LAB  scratch (default /var/lab/rime-nix/scratch/installer): TEST keys, varstores, cache, ISO, disks
#   ART  evidence (default /var/lab/rime-nix/artifacts/installer)
#
#   run.sh prep        lab image, a THROWAWAY TEST MOK and a TEST cache key, OVMF varstores
#   run.sh sign        the Rime kernel image of the lab system, signed by the TEST MOK (sign-detached.sh)
#   run.sh system      the lab system (nix/installer/lab/systems.nix pass 2) -> $LAB/system
#   run.sh cache       its closure into a file:// binary cache signed by the TEST cache key -> $LAB/cache
#   run.sh iso         installer/build-live-iso.sh, NixOS mode, PRODUCTION=1, the cache on the ISO
#   run.sh qualify     I1: installer/iso-qualify.py live --firmware uefi-sb --media usb
#   run.sh install plain|luks   install from the ISO onto a blank 20 GiB disk (Secure Boot on, Microsoft keys)
#   run.sh boot plain|luks      I2/I3: plain-boot-drive.py / luks-boot-drive.py, Secure Boot on, TEST MOK enrolled
#   run.sh firstboot plain      iso-qualify.py firstboot (NixOS): log in, read host.toml's effect, the greeter
#   run.sh clean       delete the big scratch (ISO, disks, cache, ISO build tree)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
LAB=$(realpath -m -- "${LAB:-/var/lab/rime-nix/scratch/installer}")
ART=$(realpath -m -- "${ART:-/var/lab/rime-nix/artifacts/installer}")
for d in "$LAB" "$ART"; do
  [[ $d == /var/lab/rime-nix/*/* ]] || { echo "run.sh: refusing $d (must be under /var/lab/rime-nix/<dir>/)" >&2; exit 2; }
done
mkdir -p "$LAB" "$ART"
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
CLAB=$(to_work "$LAB")
nixc() { sudo -n podman exec rime-nix nix "$@"; }
lab() { sudo -n podman run --rm --device /dev/kvm --security-opt label=disable -v "$LAB:/lab" -v "$ART:/out" \
  -v "$WT:/rig:ro" -e LAB=/lab -e OUT=/out --entrypoint "$1" rime-installer-lab "${@:2}"; }
sexpr() {   # sexpr ATTR [signed]
  local args="flake = builtins.getFlake \"path:$(to_work "$WT")\";"
  [[ ${2:-} == signed ]] && args+=" signatureDir = $CLAB/sig; certificate = $CLAB/keys/test-mok.crt;"
  echo "(import $(to_work "$HERE")/systems.nix { $args }).$1"
}
# The lab's kernel arguments, through host.toml [boot] kernel_params (RIME_EXTRA_KARGS): the serial console the
# drivers read (tty0 too, so the keyboard reaches the console passphrase prompt), systemd's info lines
# ("Switching root.", an info line, so loglevel 7), its status lines past the shipped `quiet`, and no Plymouth, so the passphrase is asked on the console, as the bootc lab does.
# systemd.log_target=kmsg: once the NixOS initrd's journald is up, PID 1 logs to it and nothing reaches the serial
# console, so "Switching root." (what the drivers wait for) was never seen although the pivot happened.
KARGS="console=tty0 console=ttyS0,115200 loglevel=7 systemd.show_status=1 systemd.log_target=kmsg plymouth.enable=0 rd.plymouth=0"
LUKSPASS=rimelabpass42
USERPASS=labpass7

step_prep() {
  sudo -n podman image exists rime-ovmf-probe || sudo -n podman build -q -t rime-ovmf-probe "$WT/nix/probe/ovmf" >/dev/null
  sudo -n podman build -q -t rime-installer-lab "$HERE" >/dev/null
  lab /bin/bash -euc '
    K=/lab/keys; mkdir -p $K /lab/fw
    if [[ ! -f $K/test-mok.key ]]; then
      openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
        -subj "/CN=Rime installer lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout $K/test-mok.key -out $K/test-mok.crt 2>/dev/null
      openssl x509 -in $K/test-mok.crt -outform DER -out $K/test-mok.der
      cat /proc/sys/kernel/random/uuid > $K/test-mok.guid; chmod 600 $K/test-mok.key
    fi
    openssl x509 -in $K/test-mok.crt -noout -subject -fingerprint -sha256
    O=/usr/share/edk2/ovmf
    cp $O/OVMF_CODE.secboot.fd /lab/fw/code-secboot.fd
    cp $O/OVMF_VARS.secboot.fd /lab/fw/vars-secboot.fd
    # A machine whose owner already enrolled the Rime key (the TEST MOK standing in for it).
    virt-fw-vars --input /lab/fw/vars-secboot.fd --output /lab/fw/vars-sb-mok.fd \
      --add-mok "$(cat $K/test-mok.guid)" $K/test-mok.der >/dev/null
    virt-fw-vars --input /lab/fw/vars-sb-mok.fd --print 2>/dev/null | grep -E "^ *(MokList|PK|KEK|db)\b|name=" | head -20 || true
    rpm -q edk2-ovmf qemu-system-x86-core tesseract' | tee "$ART/prep.txt"
  sudo -n chown -R "$(id -u):$(id -g)" "$LAB/keys" "$LAB/fw"
  if [[ ! -f $LAB/keys/cache.sec ]]; then
    nixc key generate-secret --key-name rime-installer-lab-TEST-1 > "$LAB/keys/cache.sec"
    chmod 600 "$LAB/keys/cache.sec"
  fi
  sudo -n podman exec -i rime-nix nix --extra-experimental-features nix-command key convert-secret-to-public \
    < "$LAB/keys/cache.sec" > "$LAB/keys/cache.pub"
  echo "TEST cache key: $(cat "$LAB/keys/cache.pub")" | tee -a "$ART/prep.txt"
}

step_sign() {
  local js h img tgt
  js=$(nixc eval --impure --raw --expr "$(sexpr imageJson)")
  echo "$js" | tee "$ART/image.json"
  read -r h img tgt < <(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(d["imageHash"], d["image"], d["target"])' "$js")
  nixc build --no-link "$img"
  mkdir -p "$LAB/in" "$LAB/sig"
  sudo -n podman exec rime-nix cp "$img/$tgt" "$CLAB/in/$h.unsigned"
  cp "$WT/nix/pkgs/kernel/sign-detached.sh" "$LAB/in/"
  lab /bin/bash -euc "bash /lab/in/sign-detached.sh --image /lab/in/$h.unsigned --hash $h \
    --key /lab/keys/test-mok.key --cert /lab/keys/test-mok.crt --pinned /lab/keys/test-mok.crt --out /lab/sig \
    --store-path $img --note 'installer lab, THROWAWAY TEST MOK'" | tee "$ART/sign.txt"
}

step_system() {
  local p
  p=$(nixc build --impure --no-link --print-out-paths --option cores 4 --expr "$(sexpr toplevel signed)" \
    2>"$LAB/system-build.log") || { tail -n 40 "$LAB/system-build.log"; exit 1; }
  echo "$p" | tee "$LAB/system" "$ART/system.txt"
  echo "closure: $(nixc path-info -r "$p" | wc -l) paths, $(nixc path-info -S "$p" | awk '{printf "%.2f GB NAR", $2/1e9}')" \
    | tee -a "$ART/system.txt"
}

step_cache() {
  local p
  p=$(cat "$LAB/system")
  sudo -n rm -rf "$LAB/cache"
  nixc copy --to "file://$CLAB/cache?compression=zstd&parallel-compression=true&secret-key=$CLAB/keys/cache.sec" "$p"
  nixc path-info --store "file://$CLAB/cache" --json "$p" | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [dict(path=k,**v) for k,v in d.items()]; print("cache signatures:", d[0].get("signatures"))' \
    | tee "$ART/cache.txt"
  sudo -n du -sh "$LAB/cache" | tee -a "$ART/cache.txt"
}

step_iso() {
  local w=$LAB/iso-build
  # The mutable live env needs a recovery-key helper; the NixOS ISO carries the repo's (files/system/libexec).
  sudo -n env WORK="$w" PRODUCTION=1 NETINSTALL=0 \
    NIXOS_SYSTEM="$(cat "$LAB/system")" NIXOS_CACHE_DIR="$LAB/cache" NIXOS_CACHE_PUB="$LAB/keys/cache.pub" \
    NIXOS_MOK_CERT="$LAB/keys/test-mok.der" NIXOS_KERNEL_SIGNED=signed IMG=localhost/rime-installer-nix:lab \
    bash "$WT/installer/build-live-iso.sh" > "$ART/build-live-iso.log" 2>&1 \
    || { tail -n 40 "$ART/build-live-iso.log"; exit 1; }
  tail -n 4 "$ART/build-live-iso.log"
  sudo -n chown "$(id -u):$(id -g)" "$w/rime-os-installer.iso" "$w/rime-os-installer.iso.sha256"
  cp "$w/rime-os-installer.iso.sha256" "$ART/"
}

step_qualify() {
  sudo -n rm -rf "$LAB/qualify"; mkdir -p "$LAB/qualify"
  local rc=0
  lab python3 -I /rig/installer/iso-qualify.py live --work /lab/qualify --name i1-uefi-sb-usb \
    --iso /lab/iso-build/rime-os-installer.iso --firmware uefi-sb --media usb --timeout 300 \
    > "$ART/i1-iso-qualify.log" 2>&1 || rc=$?
  cat "$ART/i1-iso-qualify.log"
  cp "$LAB"/qualify/*.json "$LAB"/qualify/*.png "$LAB"/qualify/*.serial.log "$ART/" 2>/dev/null || true
  return "$rc"
}

labdisk() {   # labdisk KIND: a vfat RIMELAB image with go + answers.in
  local kind=$1 d=$LAB/run-$1
  mkdir -p "$d/fs"
  {
    echo "mode=disk"
    echo "username=alex"
    echo "password=$USERPASS"
    echo "hostname=rime-lab-$kind"
    echo "timezone=Australia/Perth"
    echo "locale=en_AU.UTF-8"
    echo "keymap=us"
    echo "mokpw=labmok12"
    if [[ $kind == luks ]]; then echo "encrypt=yes"; echo "lukspass=$LUKSPASS"; else echo "encrypt=no"; fi
    echo "confirmed=ERASE"
  } > "$d/fs/answers.in"
  cat > "$d/fs/go" <<EOF
#!/bin/bash
# The installer lab's VT 2 script (nix/installer/lab/run.sh): runs the engine the GUI runs, on the disk with
# serial "rimetarget", and reports on ttyS0.
exec > /dev/ttyS0 2>&1
echo "RIME-LAB-GO: started \$(date -u +%T)"
T=\$(readlink -f /dev/disk/by-id/virtio-rimetarget)
fp=\$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "\$T" | head -1)
{ cat /media/answers.in; echo "disk=\$T"; echo "confirm_target=\$T"; echo "confirm_disk_id=\$fp"; } > /tmp/answers
echo "RIME-LAB-GO: target \$T (\$fp)"
RIME_EXTRA_KARGS="$KARGS" RIME_LUKS_PBKDF_MEMORY=65536 rime-install --headless /tmp/answers
rc=\$?
echo "RIME-LAB-GO: rc=\$rc"
cp /var/log/rime-install.log /media/rime-install.log 2>/dev/null
cp /run/rime-installer/result /media/result 2>/dev/null
sync; umount /media
poweroff
EOF
  sudo -n rm -f "$d/lab.img"
  lab /bin/bash -euc "truncate -s 32M /lab/run-$kind/lab.img; mkfs.vfat -n RIMELAB /lab/run-$kind/lab.img >/dev/null
    mcopy -i /lab/run-$kind/lab.img /lab/run-$kind/fs/go /lab/run-$kind/fs/answers.in ::/"
}

step_install() {
  local kind=$1 d=$LAB/run-$1 rc=0
  labdisk "$kind"
  sudo -n rm -f "$d/disk.raw"; truncate -s 20G "$d/disk.raw"
  lab python3 -I /rig/nix/installer/lab/install-drive.py --work "/lab/run-$kind" --name "install-$kind" \
    --iso /lab/iso-build/rime-os-installer.iso --disk "/lab/run-$kind/disk.raw" --labdisk "/lab/run-$kind/lab.img" \
    --timeout 5400 > "$ART/install-$kind.log" 2>&1 || rc=$?
  cat "$ART/install-$kind.log"
  cp "$d/install-$kind.serial.log" "$d"/*.png "$ART/" 2>/dev/null || true
  lab /bin/bash -c "mcopy -n -i /lab/run-$kind/lab.img ::/rime-install.log /out/rime-install-$kind.log; mcopy -n -i /lab/run-$kind/lab.img ::/result /out/result-$kind.txt" || true
  return "$rc"
}

step_boot() {
  local kind=$1 rc=0
  if [[ $kind == luks ]]; then
    lab python3 -I /rig/installer/luks-boot-drive.py --work "/lab/run-$kind" --name "boot-$kind" \
      --code /lab/fw/code-secboot.fd --vars-template /lab/fw/vars-sb-mok.fd --disk "/lab/run-$kind/disk.raw" \
      --passphrase "$LUKSPASS" --timeout 600 --accel kvm > "$ART/boot-$kind.log" 2>&1 || rc=$?
  else
    lab python3 -I /rig/installer/plain-boot-drive.py --work "/lab/run-$kind" --name "boot-$kind" \
      --code /lab/fw/code-secboot.fd --vars-template /lab/fw/vars-sb-mok.fd --disk "/lab/run-$kind/disk.raw" \
      --timeout 600 > "$ART/boot-$kind.log" 2>&1 || rc=$?
  fi
  cat "$ART/boot-$kind.log"
  cp "$LAB/run-$kind/serial-boot-$kind.log" "$ART/" 2>/dev/null || true
  # luks-boot-drive.py always exits 0: its verdict line is the result.
  if [[ $kind == luks ]] && ! grep -qx 'verdict=switched-root' "$ART/boot-$kind.log"; then rc=1; fi
  return "$rc"
}

step_firstboot() {
  local kind=$1 rc=0
  lab python3 -I /rig/installer/iso-qualify.py firstboot --work "/lab/run-$kind" --name "firstboot-$kind" \
    --disk "/lab/run-$kind/disk.raw" --nixos --display --vars-template /lab/fw/vars-sb-mok.fd \
    --user alex --password "$USERPASS" --timeout 600 --expect-system "$(cat "$LAB/system")" \
    --expect-host "rime-lab-$kind" --expect-tz Australia/Perth --expect-lang en_AU.UTF-8 \
    > "$ART/firstboot-$kind.log" 2>&1 || rc=$?
  cat "$ART/firstboot-$kind.log"
  cp "$LAB/run-$kind/firstboot-$kind".{json,png,serial.log} "$ART/" 2>/dev/null || true
  return "$rc"
}

step_clean() {
  sudo -n rm -rf "$LAB/iso-build" "$LAB/cache" "$LAB"/run-*/disk.raw "$LAB"/run-*/*.fd "$LAB/in" "$LAB/qualify"/*.fd
  sudo -n du -sh "$LAB"
}

rc=0
case "${1:-}" in
  prep) step_prep;; sign) step_sign;; system) step_system;; cache) step_cache;; iso) step_iso;;
  qualify) step_qualify || rc=$?;;
  install) step_install "${2:?plain|luks}" || rc=$?;;
  boot) step_boot "${2:?plain|luks}" || rc=$?;;
  firstboot) step_firstboot "${2:?plain|luks}" || rc=$?;;
  clean) step_clean;;
  *) echo "usage: run.sh prep|sign|system|cache|iso|qualify|install KIND|boot KIND|firstboot KIND|clean" >&2; exit 2;;
esac
sudo -n find "$LAB" "$ART" -exec chown "$(id -u):$(id -g)" {} + 2>/dev/null || true
exit "$rc"
