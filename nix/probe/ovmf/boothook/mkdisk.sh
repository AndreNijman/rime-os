#!/usr/bin/env bash
# mkdisk.sh LAYOUT: make the lab's base disk for LAYOUT (bls | custom) with generation 1 installed by nixos-install
# (which runs the Rime hook in a chroot with NIXOS_INSTALL_BOOTLOADER=1) and generations 2 and 3 copied into its
# store (not yet generations of the profile: the VM makes them generations with switch-to-configuration boot).
# Runs INSIDE the rime-nix container as root (run.sh calls it under `nix shell` with the tools). Every disk is a file
# under $LAB; the one loop device it attaches is detached by its exact name before it exits.
#   LAB    /work/scratch/boothook   (gens.json, efi/, grub2-static/ made by run.sh)
set -euo pipefail
LAYOUT=$1
LAB=${LAB:-/work/scratch/boothook}
W=$LAB/disks/$LAYOUT
ESP_VOLID=1234ABCD ESP_UUID=1234-ABCD
BTRFS_UUID=6f0e3b1c-5a1d-4c3e-9e1f-2b7d0c8a9e01
ESP_TYPE=C12A7328-F81F-11D2-BA4B-00A0C93EC93B LINUX_TYPE=0FC63DAF-8483-4772-8E79-3D69D8477DE4
gen() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]][sys.argv[3]])' "$LAB/gens.json" "$1" "$2"; }
G1=$(gen "$LAYOUT" gen1) G2=$(gen "$LAYOUT" gen2) G3=$(gen "$LAYOUT" gen3)
log=$W.log
say() { printf '>> %s\n' "$*" | tee -a "$log"; }

LOOP=
MOUNTS=()
cleanup() {
  local i
  for ((i=${#MOUNTS[@]}-1; i>=0; i--)); do umount "${MOUNTS[i]}" 2>/dev/null || umount -l "${MOUNTS[i]}" || true; done
  [[ -n $LOOP ]] && losetup -d "$LOOP" 2>/dev/null || true
}
trap cleanup EXIT
release() {   # the main path: strict, flushed; a lazy umount here once lost the paths nix copy had just written
  local i
  sync
  for ((i=${#MOUNTS[@]}-1; i>=0; i--)); do
    umount "${MOUNTS[i]}" || { findmnt -R "${MOUNTS[i]}"; echo "mkdisk: cannot unmount ${MOUNTS[i]}" >&2; exit 1; }
  done
  MOUNTS=()
  [[ -n $LOOP ]] && { blockdev --flushbufs "$LOOP"; losetup -d "$LOOP"; LOOP=; }
  sync
}
verify_store() {   # IMG MOUNT_OPTS: the copied generations are on the image, read back from a fresh mount
  local img=$1 opts=$2 m=$W/verify g
  mkdir -p "$m"; attach "$img"; mnt -o "ro,$opts" "$LOOP" "$m"
  for g in "$G1" "$G2" "$G3"; do
    [[ -x $m$g/bin/switch-to-configuration ]] || { echo "mkdisk: $g missing from $img" >&2; exit 1; }
    say "on disk: $g"
  done
  release; rmdir "$m"
}
attach() {   # attach IMG: one loop device, its node made if the container's /dev lacks it
  LOOP=$(losetup -f)
  LOOP=${LOOP%% *}   # losetup prints "/dev/loopN (lost)" when the container's /dev has no node for it
  [[ -b $LOOP ]] || mknod "$LOOP" b 7 "${LOOP#/dev/loop}"
  losetup "$LOOP" "$1"
  say "attached $1 at $LOOP"
}
mnt() { mount "$@"; MOUNTS+=("${@: -1}"); }

# bootupd's static GRUB config (from fedora-bootc:45, unmodified) and Fedora 45's signed shim + GRUB
inner_grub_cfg() {
  cat "$LAB/grub2-static/grub-static-pre.cfg"
  for f in "$LAB"/grub2-static/configs.d/*.cfg; do echo "### BEGIN $(basename "$f") ###"; cat "$f"; done
}
populate_esp_loaders() {   # DIR BOOT_UUID (fbx64.efi left out: shim as BOOTX64.EFI would run fallback)
  local d=$1 u=$2
  mkdir -p "$d/EFI/fedora" "$d/EFI/BOOT"
  cp "$LAB"/efi/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$d/EFI/fedora/"
  cp "$LAB/efi/shimx64.efi" "$d/EFI/BOOT/BOOTX64.EFI"
  cp "$LAB"/efi/{grubx64.efi,mmx64.efi} "$d/EFI/BOOT/"
  for x in fedora BOOT; do
    cp "$LAB/grub2-static/grub-static-efi.cfg" "$d/EFI/$x/grub.cfg"
    printf 'set BOOT_UUID="%s"\n' "$u" > "$d/EFI/$x/bootuuid.cfg"
  done
}
gpt() {   # gpt IMG  PART_IMG TYPE NAME ...
  local img=$1; shift
  local total=2 start=2048 table=$'label: gpt\n' parts=() a=("$@") i sz
  for ((i=0; i<${#a[@]}; i+=3)); do total=$(( total + $(stat -c %s "${a[i]}") / 1048576 )); done
  rm -f "$img"; truncate -s "$(( total + 2 ))M" "$img"
  for ((i=0; i<${#a[@]}; i+=3)); do
    sz=$(( $(stat -c %s "${a[i]}") / 512 ))
    table+="start=$start, size=$sz, type=${a[i+1]}, name=\"${a[i+2]}\""$'\n'
    parts+=("${a[i]}:$start"); start=$(( start + sz ))
  done
  printf '%s' "$table" | sfdisk -q "$img"
  for p in "${parts[@]}"; do dd if="${p%%:*}" of="$img" bs=1M seek="$(( ${p##*:} / 2048 ))" conv=notrunc,sparse status=none; done
}
install_gens() {   # ROOT: generation 1 through nixos-install, 2 and 3 into the store only
  local r=$1
  say "nixos-install --system $G1 --root $r"
  nixos-install --system "$G1" --root "$r" --no-root-passwd --no-channel-copy 2>&1 | tee -a "$log"
  say "nix copy generations 2 and 3 into $r's store"
  nix copy --no-check-sigs --to "local?root=$r" "$G2" "$G3" 2>&1 | tee -a "$log"
  say "after install: $(readlink "$r/nix/var/nix/profiles/system")"
  (cd "$r/boot" && find . \( -type f -o -type l \) -printf '%p %s\n' | sort) | tee -a "$log"
  for f in "$r"/boot/grub2/rime-nixos.cfg "$r"/boot/grub2/custom.cfg "$r"/boot/loader/entries/*.conf; do
    [[ -f $f ]] && { echo "--- ${f#"$r"}"; cat "$f"; } | tee -a "$log"
  done
}

[[ -d $W/root/var/empty ]] && chattr -i "$W/root/var/empty"
rm -rf "$W" "$log"; mkdir -p "$W"
case $LAYOUT in
bls)
  # Fresh install (D9): GPT [ESP 512 MiB = /boot][ext4 root]. The ESP is loop-mounted at /boot for the install,
  # so the hook sees a real vfat mount (its GRUB path comes from mountinfo: "" = ESP root).
  truncate -s 512M "$W/esp.img"; mkfs.vfat -F 32 -i "$ESP_VOLID" -n RIMEESP "$W/esp.img" >/dev/null
  mkdir -p "$W/root/boot"
  attach "$W/esp.img"; mnt -t vfat -o umask=0077 "$LOOP" "$W/root/boot"
  populate_esp_loaders "$W/root/boot" "$ESP_UUID"
  mkdir -p "$W/root/boot/grub2"
  inner_grub_cfg > "$W/root/boot/grub2/grub.cfg"
  printf 'set BOOT_UUID="%s"\n' "$ESP_UUID" > "$W/root/boot/grub2/bootuuid.cfg"
  install_gens "$W/root"
  release
  say "mkfs.ext4 -d (root tree, /boot empty)"
  truncate -s 6G "$W/root.img"
  mkfs.ext4 -q -L rime-lab-root -d "$W/root" "$W/root.img"
  verify_store "$W/root.img" defaults
  gpt "$W/disk.raw" "$W/esp.img" "$ESP_TYPE" "EFI System Partition" "$W/root.img" "$LINUX_TYPE" "root"
  chattr -i "$W/root/var/empty"   # NixOS activation makes it immutable
  rm -rf "$W/root" "$W/root.img" "$W/esp.img"
  ;;
custom)
  # Migrated bootc machine (D2, topology E shape): GPT [ESP 64 MiB: shim+GRUB+stub][btrfs: /boot as a top-level
  # directory, bootc-shaped (ostree's loader -> loader.1 with its entry, /boot/ostree, bootupd's grub2, a custom.cfg
  # that already holds the bootc installer's Windows entry), NixOS root in @nixos]. /boot is bound from the top
  # level, so the hook's mountinfo root is "/boot".
  truncate -s 8G "$W/btrfs.img"
  mkfs.btrfs -q -L rime-lab-btrfs -U "$BTRFS_UUID" "$W/btrfs.img"
  mkdir -p "$W/top" "$W/root"
  attach "$W/btrfs.img"; mnt "$LOOP" "$W/top"
  btrfs -q subvolume create "$W/top/@nixos"
  b=$W/top/boot
  mkdir -p "$b/grub2" "$b/loader.1/entries" "$b/ostree/default-0123456789abcdef"
  inner_grub_cfg > "$b/grub2/grub.cfg"
  printf 'set BOOT_UUID="%s"\n' "$BTRFS_UUID" > "$b/grub2/bootuuid.cfg"
  ln -s loader.1 "$b/loader"
  cat > "$b/loader.1/entries/ostree-1.conf" <<OSTREE
title Fedora Linux 45 (ostree:0)
version 1
options root=UUID=$BTRFS_UUID rw ostree=/ostree/boot.1/default/0123456789abcdef/0
linux /boot/ostree/default-0123456789abcdef/vmlinuz-7.2.9-cachyos1.apex1.fc45.x86_64
initrd /boot/ostree/default-0123456789abcdef/initramfs-7.2.9-cachyos1.apex1.fc45.x86_64.img
OSTREE
  cat > "$b/grub2/custom.cfg" <<'WIN'
# The bootc installer's Windows entry (installer/rime-install): Rime's hook must keep it byte for byte.
menuentry 'Windows (lab)' --id rime-windows {
  search --no-floppy --fs-uuid --set=root 0000-0000
  chainloader /EFI/Microsoft/Boot/bootmgfw.efi
}
WIN
  (cd "$b" && sha256sum loader.1/entries/ostree-1.conf grub2/custom.cfg grub2/grub.cfg) > "$W/ostree-before.sha256"
  readlink "$b/loader" >> "$W/ostree-before.sha256"
  mnt -o subvol=@nixos "$LOOP" "$W/root"
  mkdir -p "$W/root/boot" "$W/root/var/lib/rime" "$W/root/mnt/rime-top"
  mnt --bind "$b" "$W/root/boot"
  printf '[boot]\nlayout = "custom-cfg"\n' > "$W/root/var/lib/rime/host.toml"
  install_gens "$W/root"
  { echo "--- ostree side after the install (must equal ostree-before.sha256 for the first two)";
    (cd "$b" && sha256sum loader.1/entries/ostree-1.conf grub2/grub.cfg); readlink "$b/loader"; } | tee -a "$log"
  release
  verify_store "$W/btrfs.img" subvol=@nixos
  mkdir -p "$W/esp"; populate_esp_loaders "$W/esp" "$BTRFS_UUID"
  truncate -s 64M "$W/esp.img"; mkfs.vfat -F 32 -i "$ESP_VOLID" -n RIMEESP "$W/esp.img" >/dev/null
  mcopy -s -i "$W/esp.img" "$W/esp"/* ::/
  gpt "$W/disk.raw" "$W/esp.img" "$ESP_TYPE" "EFI System Partition" "$W/btrfs.img" "$LINUX_TYPE" "root"
  rm -rf "$W/esp" "$W/esp.img" "$W/btrfs.img" "$W/top" "$W/root"
  ;;
*) echo "usage: mkdisk.sh bls|custom" >&2; exit 2;;
esac
sfdisk -d "$W/disk.raw" | tee -a "$log"
du -h --apparent-size "$W/disk.raw" | tee -a "$log"; du -h "$W/disk.raw" | tee -a "$log"
