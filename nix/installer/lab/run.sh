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
#   run.sh system      the lab systems (nix/installer/lab/systems.nix pass 2): the `plain` variant -> $LAB/system,
#                      `plain-gaming` -> $LAB/system-gaming (the same signed kernel image: checked)
#   run.sh cache       both closures into a file:// binary cache signed by the TEST cache key -> $LAB/cache
#   run.sh release     the release record of both, signed by the THROWAWAY Fulcio CA and identity of
#                      nix/release/fixture-keys.nix (mint-test-release.py) -> $LAB/release
#   run.sh iso         installer/build-live-iso.sh, NixOS mode, PRODUCTION=1, the cache and the release on the ISO,
#                      offering plain and plain-gaming (NIXOS_CHOICES = nix/installer/choices.nix)
#   run.sh qualify     I1: installer/iso-qualify.py live --firmware uefi-sb --media usb
#   run.sh install plain|luks|gb|part|gaming
#                      install from the ISO (Secure Boot on, Microsoft keys) onto a blank 20 GiB disk; gb = LUKS with
#                      the gb keymap and a passphrase holding `"` (Shift+2 on gb, Shift+' on us); part = partition
#                      mode onto a Windows-shaped 32 GiB disk (Windows' ESP, MSR, NTFS, Rime's own ESP with a staged
#                      installer, free space), on a machine whose varstore already has the TEST MOK; gaming = plain
#                      with gaming=yes (the plain-gaming system and the gaming switch)
#   run.sh boot plain|luks      I2/I3: plain-boot-drive.py / luks-boot-drive.py, Secure Boot on, TEST MOK enrolled
#   run.sh firstboot plain|gb|part|gaming
#                      iso-qualify.py firstboot (NixOS): log in, read host.toml's effect, the greeter; gb types the
#                      passphrase at the unlock prompt on a gb keyboard first; part boots the install's own varstore
#                      (its NVRAM entry) and checks the Windows entry and the staged installer's removal
#   run.sh firstboot-us-control gb   the gb disk with the same passphrase typed on a US keyboard: must NOT unlock
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
sexpr() {   # sexpr ATTR [signed|unsigned [VARIANT]]
  local args="flake = builtins.getFlake \"path:$(to_work "$WT")\"; variant = \"${3:-plain}\";"
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
# gb: `"` is Shift+2 there and Shift+' on us, so only the gb console keymap turns these key presses into this text.
GBPASS='rime"lab42'
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
  # One signature serves both systems only if gaming does not change the kernel image.
  [[ $(nixc eval --impure --raw --expr "$(sexpr imageJson unsigned plain-gaming)") == "$js" ]] \
    || { echo "run.sh sign: plain-gaming's kernel image differs from plain's" >&2; exit 1; }
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
  local p v f
  : > "$ART/system.txt"
  for v in plain plain-gaming; do
    f=$LAB/system; [[ $v == plain ]] || f=$LAB/system-gaming
    p=$(nixc build --impure --no-link --print-out-paths --max-jobs 2 --option cores 4 --expr "$(sexpr toplevel signed "$v")" \
      2>"$LAB/system-build-$v.log") || { tail -n 40 "$LAB/system-build-$v.log"; exit 1; }
    echo "$p" > "$f"
    echo "$v: $p" | tee -a "$ART/system.txt"
    echo "closure: $(nixc path-info -r "$p" | wc -l) paths, $(nixc path-info -S "$p" | awk '{printf "%.2f GB NAR", $2/1e9}')" \
      | tee -a "$ART/system.txt"
  done
}

step_cache() {
  local p
  p=$(cat "$LAB/system")
  sudo -n rm -rf "$LAB/cache"
  nixc copy --to "file://$CLAB/cache?compression=zstd&parallel-compression=true&secret-key=$CLAB/keys/cache.sec" "$p" \
    "$(cat "$LAB/system-gaming")"
  nixc path-info --store "file://$CLAB/cache" --json "$p" | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [dict(path=k,**v) for k,v in d.items()]; print("cache signatures:", d[0].get("signatures"))' \
    | tee "$ART/cache.txt"
  sudo -n du -sh "$LAB/cache" | tee -a "$ART/cache.txt"
}

step_release() {
  local keys
  keys=$(nixc build --impure --no-link --print-out-paths --expr "let f = builtins.getFlake \"path:$(to_work "$WT")\"; in
    f.inputs.nixpkgs.legacyPackages.x86_64-linux.callPackage $(to_work "$WT")/nix/release/fixture-keys.nix { }")
  sudo -n rm -rf "$LAB/release"
  sudo -n podman exec rime-nix nix shell \
    github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#python3 \
    github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#openssl -c \
    python3 -I "$(to_work "$WT")/nix/installer/mint-test-release.py" --lib "$(to_work "$WT")/nix/release/rime_nix_release.py" \
      --system "$(cat "$LAB/system")" --also-variant "plain-gaming=$(cat "$LAB/system-gaming")" --cache "file://$CLAB/cache" --key "$(cat "$LAB/keys/cache.pub")" \
      --sign-key "$keys/good.key" --sign-cert "$keys/good.pem" --ca "$keys/ca.pem" --out "$CLAB/release" \
    | tee "$ART/release.json"
}

step_iso() {
  local w=$LAB/iso-build
  nixc eval --json --file "$(to_work "$WT")/nix/installer/choices.nix" > "$LAB/choices.json"
  cp "$LAB/choices.json" "$ART/choices.json"
  # The mutable live env needs a recovery-key helper; the NixOS ISO carries the repo's (files/system/libexec).
  sudo -n env WORK="$w" PRODUCTION=1 NETINSTALL=0 \
    NIXOS_SYSTEM="$(cat "$LAB/system")" NIXOS_CACHE_DIR="$LAB/cache" NIXOS_CACHE_PUB="$LAB/keys/cache.pub" \
    NIXOS_RELEASE_DIR="$LAB/release" NIXOS_CHOICES="$LAB/choices.json" \
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
    if [[ $kind == part ]]; then echo "mode=partition"; else echo "mode=disk"; fi
    echo "username=alex"
    echo "password=$USERPASS"
    echo "hostname=rime-lab-$kind"
    echo "timezone=Australia/Perth"
    echo "locale=en_AU.UTF-8"
    if [[ $kind == gb ]]; then echo "keymap=gb"; else echo "keymap=us"; fi
    echo "mokpw=labmok12"
    case $kind in
      luks) echo "encrypt=yes"; echo "lukspass=$LUKSPASS" ;;
      gb) echo "encrypt=yes"; echo "lukspass=$GBPASS" ;;
      *) echo "encrypt=no" ;;
    esac
    if [[ $kind == gaming ]]; then echo "gaming=yes"; fi
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
{ cat /media/answers.in; echo "disk=\$T"; echo "confirm_disk_id=\$fp"
  if grep -qx mode=partition /media/answers.in; then
    # The Windows-shaped disk: Rime's own ESP is partition 4, the free space partition 5.
    echo "target=\${T}5"; echo "esp=\${T}4"; echo "confirm_target=\${T}5"
    echo "confirm_target_id=\$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "\${T}5" | head -1)"
    echo "confirm_esp_id=\$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "\${T}4" | head -1)"
  else
    echo "confirm_target=\$T"
  fi; } > /tmp/answers
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

# A disk shaped like a Windows laptop's after the Rime Windows app ran: 1 Windows' ESP (Windows Boot Manager, a
# fixture), 2 MSR, 3 NTFS "Windows", 4 Rime's own ESP (RIME-EFI, the staged installer and its handoff file, as the
# app leaves it), 5 the free space for Rime (8304). Built in the rime-nix container from files (no loop device).
windows_disk() {   # windows_disk IMAGE
  local w; w=$(to_work "$1")
  sudo -n podman exec rime-nix nix shell github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#{gptfdisk,dosfstools,mtools,ntfs3g,coreutils,gnused,util-linux} -c bash -euc "
    img=$w; rm -f \$img; truncate -s 32G \$img
    sgdisk -n 1:2048:+260M -t 1:ef00 -c 1:'EFI system partition' -n 2:0:+16M -t 2:0c01 -c 2:'Microsoft reserved partition' \
      -n 3:0:+4G -t 3:0700 -c 3:'Basic data partition' -n 4:0:+2G -t 4:ef00 -c 4:RIME-EFI -n 5:0:0 -t 5:8304 \$img >/dev/null
    part() { sgdisk -i \$1 \$img | sed -n \"s/^\$2 sector: \([0-9]*\).*/\1/p\"; }
    put() {   # put N FILE: write FILE at partition N's start
      local at; at=\$(part \$1 First); [ -n \"\$at\" ]
      dd if=\$2 of=\$img bs=512 seek=\$at conv=notrunc,sparse status=none; rm -f \$2; }
    size() { echo \$(( \$(part \$1 Last) - \$(part \$1 First) + 1 )); }
    t=\$(dirname \$img)
    truncate -s \$(( \$(size 1) * 512 )) \$t/p1; mkfs.vfat -F 32 -n SYSTEM \$t/p1 >/dev/null
    echo fixture-bootmgfw > \$t/bootmgfw.efi
    mmd -i \$t/p1 ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot; mcopy -i \$t/p1 \$t/bootmgfw.efi ::/EFI/Microsoft/Boot/bootmgfw.efi
    put 1 \$t/p1
    truncate -s \$(( \$(size 3) * 512 )) \$t/p3; mkntfs -F -Q -q -L Windows -p \$(part 3 First) -H 255 -S 63 \$t/p3; put 3 \$t/p3
    truncate -s \$(( \$(size 4) * 512 )) \$t/p4; mkfs.vfat -F 32 -n RIME-EFI \$t/p4 >/dev/null
    mmd -i \$t/p4 ::/rimeinst ::/EFI ::/EFI/rimeinst
    printf 'bootnum=0009\\n' > \$t/handoff.cfg; mcopy -i \$t/p4 \$t/handoff.cfg ::/rimeinst/handoff.cfg
    echo fixture-staged-squashfs > \$t/sq; mcopy -i \$t/p4 \$t/sq ::/rimeinst/squashfs.img
    put 4 \$t/p4
    rm -f \$t/bootmgfw.efi \$t/handoff.cfg \$t/sq
    # Each filesystem must really be there: the rig once ran without sed, wrote none of them and said nothing.
    for pt in 1:vfat 3:ntfs 4:vfat; do
      got=\$(blkid -p -s TYPE -o value -O \$(( \$(part \${pt%%:*} First) * 512 )) \$img || true)
      [ \"\$got\" = \"\${pt#*:}\" ] || { echo \"windows_disk: partition \${pt%%:*} holds '\$got', not \${pt#*:}\" >&2; exit 1; }
    done
    sgdisk -p \$img"
  sudo -n chown "$(id -u):$(id -g)" "$1"
}

step_install() {
  local kind=$1 d=$LAB/run-$1 rc=0 vt=()
  labdisk "$kind"
  sudo -n rm -f "$d/disk.raw"
  if [[ $kind == part ]]; then
    windows_disk "$d/disk.raw" > "$ART/part-disk-before.txt" || { cat "$ART/part-disk-before.txt"; return 1; }
    cat "$ART/part-disk-before.txt"
    sha256sum <(dd if="$d/disk.raw" bs=1M count=$((2048 * 512 / 1048576 + 276)) status=none) > /dev/null
    vt=(--vars-template /lab/fw/vars-sb-mok.fd)
  else
    truncate -s 20G "$d/disk.raw"
  fi
  lab python3 -I /rig/nix/installer/lab/install-drive.py --work "/lab/run-$kind" --name "install-$kind" \
    --iso /lab/iso-build/rime-os-installer.iso --disk "/lab/run-$kind/disk.raw" --labdisk "/lab/run-$kind/lab.img" \
    "${vt[@]}" --timeout 5400 > "$ART/install-$kind.log" 2>&1 || rc=$?
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

step_firstboot() {   # step_firstboot KIND [NAME [LAYOUT]]
  local kind=$1 name=${2:-firstboot-$1} layout=${3:-gb} rc=0 extra=(--vars-template /lab/fw/vars-sb-mok.fd)
  local sys=$LAB/system gaming=no
  if [[ $kind == gaming ]]; then sys=$LAB/system-gaming; gaming=yes; fi
  case $kind in
    gb) extra+=(--luks-passphrase "$GBPASS" --luks-layout "$layout") ;;
    # The install's own varstore: the TEST MOK it started with, plus the NVRAM entry the installer wrote.
    part) extra=(--vars-template "/lab/run-part/install-part.vars.fd" --expect-no-staged --expect-windows-entry) ;;
  esac
  lab python3 -I /rig/installer/iso-qualify.py firstboot --work "/lab/run-$kind" --name "$name" \
    --disk "/lab/run-$kind/disk.raw" --nixos --display "${extra[@]}" \
    --user alex --password "$USERPASS" --timeout 600 --expect-system "$(cat "$sys")" \
    --expect-gaming "$gaming" --expect-nocow \
    --expect-host "rime-lab-$kind" --expect-tz Australia/Perth --expect-lang en_AU.UTF-8 \
    > "$ART/$name.log" 2>&1 || rc=$?
  cat "$ART/$name.log"
  cp "$LAB/run-$kind/$name".{json,png,serial.log} "$ART/" 2>/dev/null || true
  return "$rc"
}

step_clean() {
  sudo -n rm -rf "$LAB/iso-build" "$LAB/cache" "$LAB"/run-*/disk.raw "$LAB"/run-*/*.fd "$LAB/in" "$LAB/qualify"/*.fd
  sudo -n du -sh "$LAB"
}

rc=0
case "${1:-}" in
  prep) step_prep;; sign) step_sign;; system) step_system;; cache) step_cache;; release) step_release;; iso) step_iso;;
  qualify) step_qualify || rc=$?;;
  install) step_install "${2:?plain|luks|gb|part|gaming}" || rc=$?;;
  boot) step_boot "${2:?plain|luks}" || rc=$?;;
  firstboot) step_firstboot "${2:?plain|gb|part|gaming}" || rc=$?;;
  firstboot-us-control) step_firstboot gb firstboot-gb-us-control us || rc=$?;;
  clean) step_clean;;
  *) echo "usage: run.sh prep|sign|system|cache|release|iso|qualify|install KIND|boot KIND|firstboot KIND|firstboot-us-control|clean" >&2; exit 2;;
esac
sudo -n find "$LAB" "$ART" -exec chown "$(id -u):$(id -g)" {} + 2>/dev/null || true
exit "$rc"
