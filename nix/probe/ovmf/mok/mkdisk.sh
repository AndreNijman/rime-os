#!/usr/bin/env bash
# mkdisk.sh: the MOK lab's kernels, initrd and boot disk. Runs INSIDE rime-ovmf-mok as root (run.sh disks).
#   $LAB/efi/{shimx64,grubx64,mmx64}.efi  Fedora 45's signed shim, GRUB and MokManager (the OVMF probe's extraction)
#   $LAB/in/vmlinuz.bootc                 a published bootc Rime kernel, signed in CI with the REAL Rime MOK
#   $LAB/in/rime-mok.pem                  nix/pkgs/kernel/signing/rime-mok.pem (the real Rime MOK certificate)
# Makes:
#   $LAB/keys/test-mok.{key,crt}          a THROWAWAY TEST MOK (the control: a MOK-signed kernel whose key is NOT
#                                         enrolled must be refused)
#   $LAB/keys/mok-password                the TEST password MokManager asks for (letters and digits only: typed as
#                                         QMP key codes)
#   $LAB/disk.raw                         GPT + ESP: shim (BOOTX64.EFI) -> GRUB -> /stage.cfg, which the driver
#                                         rewrites before every boot (mtools), and the three kernels + one initrd
set -euo pipefail
LAB=${LAB:-/lab}
P=$LAB/probe K=$LAB/keys
mkdir -p "$P" "$K"
rm -rf "$P"/*

# ── kernels ───────────────────────────────────────────────────────────────────────────────────────────────────────
rpm=$(ls /opt/fedora-kernel/kernel-core-*.rpm)
echo "fedora kernel rpm: $(rpm -qp --qf '%{NEVRA}' "$rpm") sha256 $(sha256sum "$rpm" | cut -d' ' -f1)"
mkdir -p "$P/rpm"
(cd "$P/rpm" && rpm2cpio "$rpm" | cpio -idm --quiet './lib/modules/*/vmlinuz')
cp "$P"/rpm/lib/modules/*/vmlinuz "$P/fedora.efi"
rm -rf "$P/rpm"
cp "$LAB/in/vmlinuz.bootc" "$P/bootc.efi"

if [[ ! -f $K/test-mok.key ]]; then
  openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
    -subj "/CN=Rime MOK lab TEST MOK (throwaway, DO NOT TRUST)/" -keyout "$K/test-mok.key" -out "$K/test-mok.crt" 2>/dev/null
  chmod 600 "$K/test-mok.key"
fi
[[ -s $K/mok-password ]] || { printf %s "rimemoklab2026" > "$K/mok-password"; chmod 600 "$K/mok-password"; }
cp "$P/fedora.efi" "$P/unsigned.efi"; sbattach --remove "$P/unsigned.efi"
sbsign --key "$K/test-mok.key" --cert "$K/test-mok.crt" --output "$P/testmok.efi" "$P/unsigned.efi" 2>/dev/null
rm -f "$P/unsigned.efi"

openssl x509 -in "$LAB/in/rime-mok.pem" -outform DER -out "$P/rime-mok.der"
echo "rime-mok.der sha256 $(sha256sum "$P/rime-mok.der" | cut -d' ' -f1)"
openssl x509 -in "$LAB/in/rime-mok.pem" -noout -subject -serial -fingerprint -sha256
for k in fedora bootc testmok; do
  echo "--- $k.efi sha256 $(sha256sum "$P/$k.efi" | cut -d' ' -f1)"
  sbverify --list "$P/$k.efi" | sed 's/^/    /'
  if sbverify --cert "$LAB/in/rime-mok.pem" "$P/$k.efi" >/dev/null 2>&1; then
    echo "    sbverify --cert rime-mok.pem: OK"
  else
    echo "    sbverify --cert rime-mok.pem: REFUSED"
  fi
done
# The lab is only meaningful if exactly the bootc kernel carries the real Rime MOK's signature.
sbverify --cert "$LAB/in/rime-mok.pem" "$P/bootc.efi" >/dev/null 2>&1
! sbverify --cert "$LAB/in/rime-mok.pem" "$P/testmok.efi" >/dev/null 2>&1
! sbverify --cert "$LAB/in/rime-mok.pem" "$P/fedora.efi" >/dev/null 2>&1

# ── initrd: busybox + mokutil ────────────────────────────────────────────────────────────────────────────────────
I=$P/initrd
mkdir -p "$I"/{bin,dev,proc,sys,run,tmp,lib64,etc}
cp "$(command -v busybox)" "$I/bin/busybox"
cp "$(command -v mokutil)" "$I/bin/mokutil"
# mokutil's shared libraries (and the loader) at the paths its ELF names
ldd "$(command -v mokutil)" | grep -oE '/[^ ]+' | sort -u | while read -r so; do
  mkdir -p "$I$(dirname "$so")"; cp -L "$so" "$I$so"
done
cp "$P/rime-mok.der" "$I/rime-mok.der"
# mokutil --generate-hash refuses without efivarfs (this container has none), so the guest makes the hash itself.
cp "$K/mok-password" "$I/mok-password"
cat > "$I/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
exec </dev/console >/dev/console 2>&1   # no device node in the archive: the container cannot mknod
mount -t securityfs securityfs /sys/kernel/security
mount -t efivarfs efivarfs /sys/firmware/efi/efivars
echo 1 > /proc/sys/kernel/printk
p() { echo "RIME-MOK: $*"; }
p "reached: kernel=$(uname -r) cmdline=$(cat /proc/cmdline)"
v=/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c
p "secureboot-var=$(od -An -tu1 "$v" | tr -s ' ' | sed 's/^ //')"
p "mokutil --sb-state: $(mokutil --sb-state 2>&1 | tr '\n' ' ')"
if [ -r /sys/kernel/security/lockdown ]; then
  p "lockdown: $(cat /sys/kernel/security/lockdown)"
else
  p "lockdown: no /sys/kernel/security/lockdown (the lockdown LSM is not loaded)"
fi
dmesg | grep -iE "secure ?boot|lockdown|locked down|X\.509|MokListRT|platform keyring|microcode" | sed 's/^/RIME-MOK: dmesg| /'
# The microcode files' NAMES in the bytes GRUB handed the kernel (needs retain_initrd). Not rootfs: nixpkgs' cpios
# hold the one file and no directory entries, and the kernel's initramfs unpacker creates a file only inside a
# directory that already exists (init/initramfs.c), so kernel/x86/microcode/*.bin never reaches rootfs from them.
# The early loader reads the raw image (find_cpio_data), not rootfs (docs/nixos/boot-matrix.md, "Early microcode").
if [ -r /sys/firmware/initrd ]; then
  p "ucode-names: $(tr -c 'A-Za-z0-9/._' '\n' < /sys/firmware/initrd | grep -o 'kernel/x86/microcode/[A-Za-z]*[.]bin' | sort -u | tr '\n' ' ')"
fi
p "ucode-rootfs: $(ls /kernel/x86/microcode 2>/dev/null | tr '\n' ' ')"
p "cpu: hypervisor-flag=$(grep -m1 '^flags' /proc/cpuinfo | grep -c -w hypervisor) $(grep -m5 -E '^(cpu family|model|stepping|microcode)[[:space:]]' /proc/cpuinfo | tr -d '\t' | tr '\n' ' ')"
p "ucode-sysfs: $(cat /sys/devices/system/cpu/cpu0/microcode/version 2>/dev/null || echo none)"
# With `retain_initrd` the kernel keeps the image GRUB handed it at /sys/firmware/initrd: its size and the sha256 of
# each piece say exactly which files GRUB concatenated, in which order. rime.pieces=<len>,<len>,... names the pieces.
if [ -r /sys/firmware/initrd ]; then
  p "initrd-image: size=$(wc -c < /sys/firmware/initrd)"
  pieces=$(sed -n 's/.*rime\.pieces=\([0-9,]*\).*/\1/p' /proc/cmdline)
  off=0
  for len in $(echo "$pieces" | tr ',' ' '); do
    p "initrd-piece: offset=$off len=$len sha256=$(tail -c +$((off + 1)) /sys/firmware/initrd | head -c "$len" | sha256sum | cut -d' ' -f1)"
    off=$(((off + len + 3) / 4 * 4))   # GRUB pads every initrd file to 4 bytes
  done
fi
mokutil --list-enrolled 2>&1 | grep -E "Subject:|SHA1 Fingerprint|MokListRT is empty|is empty" | sed 's/^/RIME-MOK: enrolled| /'
case " $(cat /proc/cmdline) " in
  *" rime.mok=import "*)
    p "importing rime-mok.der"
    mokutil --generate-hash="$(cat /mok-password)" > /tmp/mok.hash
    if mokutil --import /rime-mok.der --hash-file /tmp/mok.hash; then p "import rc=0"; else p "import rc=$?"; fi
    mokutil --list-new 2>&1 | grep -E "Subject:|SHA1 Fingerprint|Issuer:" | sed 's/^/RIME-MOK: pending| /'
    p "DONE import, rebooting"
    sync
    reboot -f
    ;;
esac
p "DONE probe, powering off"
sync
poweroff -f
INIT
chmod 755 "$I/init"
(cd "$I" && find . | sort | cpio -o -H newc --quiet | gzip -9) > "$P/initrd.img"
rm -rf "$I"
echo "initrd $(du -h "$P/initrd.img" | cut -f1)"

# ── disk ─────────────────────────────────────────────────────────────────────────────────────────────────────────
ESP_TYPE=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
w=$LAB/esp.d
rm -rf "$w"; mkdir -p "$w/EFI/BOOT" "$w/EFI/fedora" "$w/probe"
cp "$LAB/efi/shimx64.efi" "$w/EFI/BOOT/BOOTX64.EFI"
cp "$LAB"/efi/{grubx64.efi,mmx64.efi} "$w/EFI/BOOT/"
cp "$LAB"/efi/{shimx64.efi,grubx64.efi,mmx64.efi} "$w/EFI/fedora/"
cp "$P"/{fedora,bootc,testmok}.efi "$P/initrd.img" "$w/probe/"
# Early microcode as SEPARATE initrd files (rime-boot's split layout; nixpkgs' own cpios, unchanged), if prep found them.
for u in intel-ucode amd-ucode; do [[ -s $LAB/in/$u.img ]] && cp "$LAB/in/$u.img" "$w/probe/"; done
mkdir -p "$w/loader/entries"
for x in BOOT fedora; do
  cat > "$w/EFI/$x/grub.cfg" <<'CFG'
set timeout=0
search --no-floppy --fs-uuid --set=root 4D4F-4B4C
source ($root)/stage.cfg
CFG
done
printf 'echo "RIME-MOK: no stage"\n' > "$w/stage.cfg"
rm -f "$LAB/esp.img"; truncate -s 160M "$LAB/esp.img"
mkfs.vfat -F 32 -i 4D4F4B4C -n MOKLAB "$LAB/esp.img" >/dev/null
mcopy -s -i "$LAB/esp.img" "$w"/* ::/
rm -f "$LAB/disk.raw"; truncate -s 162M "$LAB/disk.raw"
printf 'label: gpt\nstart=2048, size=327680, type=%s, name="EFI System Partition"\n' "$ESP_TYPE" | sfdisk -q "$LAB/disk.raw"
dd if="$LAB/esp.img" of="$LAB/disk.raw" bs=1M seek=1 conv=notrunc,sparse status=none
rm -rf "$w" "$LAB/esp.img"
echo "made $LAB/disk.raw"
