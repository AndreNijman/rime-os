#!/usr/bin/env bash
# mkbootc.sh KIND: a bootc-shaped Rime OS disk from the PUBLISHED image (or prepare.sh's readiness image built on it,
# named in info.json by RIME_LAB_IMAGE), for the bridge lab (README.md here).
#   KIND = A  installer default: GPT [1M BIOS boot][512M ESP EFI-SYSTEM][btrfs rime-root], shim + GRUB in
#             \EFI\fedora on Rime's own ESP, /boot a directory of the btrfs top level (as installer/rime-install
#             install_plain_disk lays it out)
#   KIND = L  the installer's DEFAULT, encrypted layout (installer/rime-install, encryption on): GPT
#             [1G ESP RIMEESP][1G ext4 rime-boot = /boot][1M BIOS boot (partition 4)][LUKS2 RIMEROOT -> btrfs
#             rime-root], kernel arguments rd.luks.uuid= + rd.luks.name=, /etc/crypttab, /boot in /etc/fstab. The
#             passphrase is a TEST one ($RIME_LAB_LUKS) and its key derivation is weakened exactly as the installer's
#             own test builds weaken it (RIME_LUKS_PBKDF_MEMORY: argon2id 32 MiB, 4 iterations), so a boot is quick.
#   KIND = E  katana's shape: the A disk labelled apex-root, plus the failed boot-v2 trial's leftovers
#             (\EFI\systemd\, \EFI\Linux\, \loader\ on Rime's ESP, /composefs and /state in the top level,
#             /var/lib/rime/boot-migrate with phase=failed, /var/lib/apex -> rime), and a second "Windows" disk whose
#             ESP holds \EFI\Microsoft\ and the legacy \EFI\APEX\ shim + GRUB that boots the machine (topology D).
#
# Runs INSIDE the published Rime image (podman run --privileged, no --pid=host, an empty tmpfs over /sys/firmware,
# so nothing here can reach the host's NVRAM). Neither bootc install nor bootupctl runs: ostree deploys the image
# (from an OCI export of it) into a fresh sysroot, and the ESP gets the image's own shim, GRUB and bootupd static
# config, the files bootupd places. Every disk is a file under /lab/disks; the loop devices are detached by name.
# The NVRAM entries of a real machine are made INSIDE the VM (drive-bridge.py prep), never here.
set -euo pipefail
KIND=$1
L=/lab
W=$L/disks/$KIND
IMGREF_TARGET=ostree-unverified-registry:ghcr.io/andrenijman/rime-os:rime
case $KIND in
  A) ROOT_UUID=3a7d2c10-7b4e-4a51-8d1c-a1a1a1a1a1a1 ;;
  L) ROOT_UUID=0c0c0c0c-7b4e-4a51-8d1c-c1c1c1c1c1c1 LUKS_UUID=0d0d0d0d-7b4e-4a51-8d1c-d1d1d1d1d1d1
     BOOT_UUID=0e0e0e0e-7b4e-4a51-8d1c-e1e1e1e1e1e1 LUKS_PASS=${RIME_LAB_LUKS:?the TEST LUKS passphrase} ;;
  E) ROOT_UUID=0b0b0b0b-5e1f-4c1a-9b77-0e0e0e0e0e0e ;;
  *) echo "mkbootc: KIND is A, L or E" >&2; exit 2 ;;
esac
LABEL=$( [[ $KIND == E ]] && echo apex-root || echo rime-root )
USER_NAME=alex
USER_PASS=${RIME_LAB_PASS:?the TEST password of the lab account}
say() { printf '>> %s\n' "$*"; }

# The host's firmware variables must be out of reach: an empty tmpfs over /sys/firmware in this container's own mount
# namespace (podman's --mount tmpfs at /sys/firmware was measured NOT to hide efi/ on a --privileged container).
mount -t tmpfs -o ro,size=1k rime-no-firmware /sys/firmware
[[ ! -e /sys/firmware/efi ]] || { echo "mkbootc: /sys/firmware/efi is still visible; refusing" >&2; exit 2; }
echo ">> /sys/firmware: $(findmnt -no SOURCE,FSTYPE,OPTIONS /sys/firmware | tail -1); efi/ hidden"
[[ -f /usr/lib/bootupd/grub2-static/grub-static-efi.cfg ]] || { echo "mkbootc: not running in the Rime image" >&2; exit 2; }

LOOPS=() MOUNTS=()
cleanup() {
  local i
  for ((i=${#MOUNTS[@]}-1; i>=0; i--)); do umount "${MOUNTS[i]}" 2>/dev/null || umount -l "${MOUNTS[i]}" || true; done
  [[ -z ${OPENED:-} ]] || cryptsetup close "$OPENED" 2>/dev/null || true
  for l in "${LOOPS[@]}"; do losetup -d "$l" 2>/dev/null || true; done
}
trap cleanup EXIT
attach() {   # attach IMG OFFSET SIZE: one loop device over one partition of a disk file
  local l
  l=$(losetup -f); l=${l%% *}
  [[ -b $l ]] || mknod "$l" b 7 "${l#/dev/loop}"
  losetup --offset "$2" --sizelimit "$3" "$l" "$1"
  LOOPS+=("$l"); echo "$l"
}
mnt() { mount "$@"; MOUNTS+=("${@: -1}"); }
part() {   # part DISK N -> "offset size" in bytes of partition NUMBER N (L's partition 4 sits between 2 and 3)
  sfdisk -J "$1" | python3 -c 'import json,re,sys; p=[p for p in json.load(sys.stdin)["partitiontable"]["partitions"] if re.search(r"\D%s$" % sys.argv[1], p["node"])][0]; print(p["start"]*512, p["size"]*512)' "$2"
}
static_inner() {
  cat /usr/lib/bootupd/grub2-static/grub-static-pre.cfg
  for f in /usr/lib/bootupd/grub2-static/configs.d/*.cfg; do echo "### BEGIN $(basename "$f") ###"; cat "$f"; done
}
loaders() {   # loaders DIR NAME [UPPER]: shim + GRUB + MokManager + bootupd's EFI grub.cfg into DIR/EFI/NAME
  local d=$1/EFI/$2 shim grub
  shim=$(ls -d /usr/lib/efi/shim/*/EFI | head -1) grub=$(ls -d /usr/lib/efi/grub2/*/EFI | head -1)
  mkdir -p "$d"
  if [[ ${3:-} == upper ]]; then cp "$shim/fedora/shimx64.efi" "$d/SHIMX64.EFI"; else cp "$shim/fedora/shimx64.efi" "$d/"; fi
  cp "$shim/fedora/mmx64.efi" "$grub/fedora/grubx64.efi" "$d/"
  cp /usr/lib/bootupd/grub2-static/grub-static-efi.cfg "$d/grub.cfg"
  printf 'set BOOT_UUID="%s"\n' "${BOOT_UUID:-$ROOT_UUID}" > "$d/bootuuid.cfg"
}

rm -rf "$W"; mkdir -p "$W"
if [[ $KIND == L ]]; then
  say "disk: GPT [ESP 1G RIMEESP][ext4 1G rime-boot $BOOT_UUID][BIOS boot 1M, #4][LUKS2 RIMEROOT $LUKS_UUID] (56G sparse)"
  truncate -s 56G "$W/disk.raw"
  sfdisk -q "$W/disk.raw" <<GPT
label: gpt
$W/disk.raw1 : start=2048, size=2097152, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI System Partition"
$W/disk.raw2 : start=2099200, size=2097152, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="rime-boot"
$W/disk.raw4 : start=4196352, size=2048, type=21686148-6449-6E6F-744E-656564454649, name="BIOS boot partition"
$W/disk.raw3 : start=4198400, type=CA7D7CCB-63ED-4C53-861C-1742536059CC, name="rime-luks"
GPT
  read -r eo es < <(part "$W/disk.raw" 1)
  read -r bo bs < <(part "$W/disk.raw" 2)
  read -r lo ls_ < <(part "$W/disk.raw" 3)
  ESP=$(attach "$W/disk.raw" "$eo" "$es")
  BOOTDEV=$(attach "$W/disk.raw" "$bo" "$bs")
  LUKSDEV=$(attach "$W/disk.raw" "$lo" "$ls_")
  mkfs.vfat -F 32 -n RIMEESP "$ESP" >/dev/null
  mkfs.ext4 -q -F -L rime-boot -U "$BOOT_UUID" "$BOOTDEV"
  # installer/rime-install's LUKS_FMT, with its test-build weakening (RIME_LUKS_PBKDF_MEMORY) and a fixed UUID.
  printf '%s' "$LUKS_PASS" | cryptsetup luksFormat --type luks2 --pbkdf argon2id --label RIMEROOT --batch-mode \
    --pbkdf-memory 32768 --pbkdf-parallel 1 --pbkdf-force-iterations 4 --uuid "$LUKS_UUID" --key-file - "$LUKSDEV"
  printf '%s' "$LUKS_PASS" | cryptsetup open --key-file - "$LUKSDEV" "luks-$LUKS_UUID"
  OPENED=luks-$LUKS_UUID
  ROOT=/dev/mapper/$OPENED
  cryptsetup luksDump "$LUKSDEV" | grep -E '^(UUID|Label|Version)|argon2id|Memory|Time cost'
else
  say "disk: GPT [BIOS boot 1M][ESP 512M EFI-SYSTEM][btrfs $LABEL $ROOT_UUID] (56G sparse)"
  truncate -s 56G "$W/disk.raw"
  sfdisk -q "$W/disk.raw" <<'GPT'
label: gpt
start=2048, size=2048, type=21686148-6449-6E6F-744E-656564454649, name="BIOS-BOOT"
start=4096, size=1048576, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI-SYSTEM"
start=1052672, type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, name="rime-root"
GPT
  read -r eo es < <(part "$W/disk.raw" 2)
  read -r ro rs < <(part "$W/disk.raw" 3)
  ESP=$(attach "$W/disk.raw" "$eo" "$es")
  ROOT=$(attach "$W/disk.raw" "$ro" "$rs")
  mkfs.vfat -F 32 -n EFI-SYSTEM "$ESP" >/dev/null
fi
mkfs.btrfs -q -L "$LABEL" -U "$ROOT_UUID" "$ROOT"
T=$W/mnt; mkdir -p "$T"
mnt -o compress-force=zstd:3 "$ROOT" "$T"

say "ostree: sysroot, stateroot default, deploy the published image"
ostree admin init-fs --modern "$T"
mkdir -p "$T/boot"
[[ $KIND != L ]] || mnt "$BOOTDEV" "$T/boot"   # L: /boot is the ext4 partition from here on
ostree --repo="$T/ostree/repo" config set sysroot.bootloader none
ostree --repo="$T/ostree/repo" config set sysroot.bootprefix true
ostree --repo="$T/ostree/repo" config set sysroot.readonly true
ostree admin stateroot-init --sysroot="$T" default
ostree container image deploy --sysroot "$T" --stateroot default \
  --imgref ostree-unverified-image:oci:/lab/oci --target-imgref "$IMGREF_TARGET" \
  --karg "root=UUID=$ROOT_UUID" --karg rw --karg rootflags=subvol=/ \
  --karg rd.driver.blacklist=nouveau --karg modprobe.blacklist=nouveau \
  --karg console=tty0 --karg console=ttyS0,115200 \
  ${LUKS_UUID:+--karg rd.luks.uuid=$LUKS_UUID --karg rd.luks.name=$LUKS_UUID=luks-$LUKS_UUID --karg plymouth.enable=0 --karg rd.plymouth=0}
D=$(ls -d "$T"/ostree/deploy/default/deploy/*.0 | head -1)
SR=$T/ostree/deploy/default
say "deployment: ${D#"$T"}"
ls "$T/boot/loader/entries/"; cat "$T"/boot/loader/entries/*.conf
if [[ $KIND == L ]]; then
  # sysroot.bootprefix=true (bootc sets it for every install) writes /boot/... paths into the entries; on a separate
  # /boot GRUB resolves them through a "boot -> ." symlink in the partition's root. Recorded, made only if absent.
  ls -la "$T/boot"
  if [[ -L $T/boot/boot ]]; then say "L: /boot/boot -> $(readlink "$T/boot/boot") (ostree made it)"
  else say "L: ostree made no /boot/boot symlink: adding boot -> ."; ln -s . "$T/boot/boot"; fi
fi

say "GRUB: bootupd's static config in /boot/grub2, shim + GRUB in \\EFI\\fedora and \\EFI\\BOOT"
mkdir -p "$T/boot/grub2"
static_inner > "$T/boot/grub2/grub.cfg"
printf 'set BOOT_UUID="%s"\n' "${BOOT_UUID:-$ROOT_UUID}" > "$T/boot/grub2/bootuuid.cfg"
cp -r /boot/grub2/fonts "$T/boot/grub2/"; cp /boot/grub2/grubenv "$T/boot/grub2/grubenv"
E=$W/esp; mkdir -p "$E"; mnt -o umask=0077 "$ESP" "$E"
loaders "$E" fedora
cp /usr/lib/efi/shim/*/EFI/fedora/BOOTX64.CSV "$E/EFI/fedora/"
mkdir -p "$E/EFI/BOOT"; cp /usr/lib/efi/shim/*/EFI/BOOT/{BOOTX64.EFI,fbx64.efi} "$E/EFI/BOOT/"

if [[ $KIND == L ]]; then
  say "L: /etc/crypttab (write_crypttab_in) and /etc/fstab's /boot line (bootc --boot-mount-spec)"
  printf '# Written by rime-install.\n# <name>  <device>  <key file>  <options>\n%s  UUID=%s  none  %s\n' \
    "luks-$LUKS_UUID" "$LUKS_UUID" "luks,discard,x-initrd.attach" > "$D/etc/crypttab"
  chmod 0600 "$D/etc/crypttab"
  printf 'UUID=%s /boot ext4 defaults 1 2\n' "$BOOT_UUID" >> "$D/etc/fstab"
  cat "$D/etc/crypttab" "$D/etc/fstab"; ls -la "$T/boot"
fi

say "lab instrumentation: unmask serial-getty@ttyS0 (the image masks it; the lab logs in over ttyS0)"
rm -f "$D/etc/systemd/system/serial-getty@ttyS0.service"

say "account $USER_NAME (TEST password) with a seeded home and its checksums"
chroot "$D" useradd -M -d "/var/home/$USER_NAME" -G wheel -u 1000 -s /bin/bash "$USER_NAME"
echo "$USER_NAME:$USER_PASS" | chroot "$D" chpasswd
H=$SR/var/home/$USER_NAME
mkdir -p "$H"/{Documents,Pictures,.config/rime-lab,.rime-lab}
echo "written on bootc before the bridge ($KIND)" > "$H/Documents/note.txt"
head -c 4194304 /dev/urandom > "$H/Pictures/blob.bin"
printf '[lab]\nkind = "%s"\n' "$KIND" > "$H/.config/rime-lab/settings.toml"
ln -s Documents/note.txt "$H/note-link"
(cd "$H" && sha256sum Documents/note.txt Pictures/blob.bin .config/rime-lab/settings.toml > .rime-lab/sums)
chown -R 1000:1000 "$H"; chmod 700 "$H"
cp "$H/.rime-lab/sums" "$W/home.sums"
# What a machine whose owner has logged in through the greeter has: rime-greet's last user (the carry moves it).
mkdir -p "$SR/var/lib/rime-greet"; echo "$USER_NAME" > "$SR/var/lib/rime-greet/last-user"
FC=$D/etc/selinux/targeted/contexts/files/file_contexts
if command -v setfiles >/dev/null && [[ -f $FC ]]; then
  setfiles -F -r "$SR" "$FC" "$SR/var/home" "$SR/var/lib/rime-greet"
  setfiles -F -r "$D" "$FC" "$D/etc"
  say "SELinux labels set (setfiles)"
else
  say "no setfiles or file_contexts: home and /etc left unlabelled"
fi

if [[ $KIND == E ]]; then
  say "E: the failed boot-v2 trial's leftovers (rime-boot-migrate's own files) on Rime's ESP and in /var"
  mkdir -p "$E/EFI/systemd" "$E/EFI/Linux" "$E/loader/entries"
  if [[ -f /usr/lib/systemd/boot/efi/systemd-bootx64.efi ]]; then
    cp /usr/lib/systemd/boot/efi/systemd-bootx64.efi "$E/EFI/systemd/"
  else
    echo "lab stand-in for systemd-bootx64.efi (the image ships none)" > "$E/EFI/systemd/systemd-bootx64.efi"
  fi
  echo "lab stand-in for the trial's UKI" > "$E/EFI/Linux/rime-$(sha256sum "$D/usr/lib/os-release" | cut -c1-16).efi"
  printf 'timeout 3\neditor no\n' > "$E/loader/loader.conf"
  mkdir -p "$T/composefs/images" "$T/state/deploy"
  B=$SR/var/lib/rime/boot-migrate
  mkdir -p "$B"
  echo failed > "$B/phase"; echo 2026-09-26T09:08:02+08:00 > "$B/failed-at"; echo 0006 > "$B/bootnum"
  echo 0000,0001 > "$B/bootorder.before"
  cp "$E/EFI/BOOT/BOOTX64.EFI" "$B/BOOTX64.EFI.orig"; sha256sum "$B/BOOTX64.EFI.orig" | cut -d' ' -f1 > "$B/BOOTX64.EFI.sha"
  (cd "$E" && find . -type f | sort) > "$B/boot-before.list"
  sha256sum "$E/EFI/fedora/grubx64.efi" | cut -d' ' -f1 > "$B/grub-esp.sha"
  ln -s rime "$SR/var/lib/apex"
  command -v setfiles >/dev/null && [[ -f $FC ]] && setfiles -F -r "$SR" "$FC" "$SR/var/lib/rime"
  say "E: the Windows disk: ESP with \\EFI\\Microsoft\\ and the legacy \\EFI\\APEX\\ loader"
  truncate -s 1G "$W/win.raw"
  sfdisk -q "$W/win.raw" <<'GPT'
label: gpt
start=2048, size=409600, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI system partition"
start=411648, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="Basic data partition"
GPT
  read -r wo ws < <(part "$W/win.raw" 1)
  WE=$(attach "$W/win.raw" "$wo" "$ws")
  mkfs.vfat -F 32 -n SYSTEM "$WE" >/dev/null
  mkdir -p "$W/wesp"; mnt "$WE" "$W/wesp"
  mkdir -p "$W/wesp/EFI/Microsoft/Boot"
  echo "lab stand-in: not a Windows boot manager" > "$W/wesp/EFI/Microsoft/Boot/bootmgfw.efi"
  loaders "$W/wesp" APEX upper
  (cd "$W/wesp" && find . -type f | sort | xargs sha256sum) > "$W/win-esp.before.sha256"
fi

say "readback"
(cd "$E" && find . -type f | sort | xargs sha256sum) > "$W/esp.before.sha256"
(cd "$T/boot" && find . -path ./efi -prune -o -type f -print | sort | xargs sha256sum) > "$W/boot.before.sha256"
python3 - "$W/info.json" <<PY
import json, sys
json.dump({"kind": "$KIND", "root_uuid": "$ROOT_UUID", "label": "$LABEL", "user": "$USER_NAME",
           "luks_uuid": "${LUKS_UUID:-}", "boot_uuid": "${BOOT_UUID:-}",
           "deployment": "${D#"$T"}", "image": "${RIME_LAB_IMAGE:-ghcr.io/andrenijman/rime-os@sha256:0ace1f6dad51597d79a7d0d84135dd2896fc89d748ec74b9ecfc7c98c92df9bf}"},
          open(sys.argv[1], "w"), indent=1)
PY
sync
for ((i=${#MOUNTS[@]}-1; i>=0; i--)); do umount "${MOUNTS[i]}"; done; MOUNTS=()
[[ -z ${OPENED:-} ]] || { cryptsetup close "$OPENED"; OPENED=; }
for l in "${LOOPS[@]}"; do losetup -d "$l"; done; LOOPS=()
du -h --apparent-size "$W"/*.raw; du -h "$W"/*.raw
say "done: $W"
