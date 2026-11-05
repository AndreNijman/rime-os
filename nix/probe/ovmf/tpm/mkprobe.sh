#!/usr/bin/env bash
# mkprobe.sh: the TPM lab's "before" boot disks and its LUKS2 data disk. Runs INSIDE rime-ovmf-tpm as root (run.sh).
#   $LAB/keys/test-mok.{key,crt}       THROWAWAY TEST MOK (run.sh prep)
#   $LAB/efi/{shimx64,grubx64,mmx64}.efi  Fedora 45's signed shim and GRUB (the OVMF probe's extraction)
# Makes:
#   $LAB/probe/vmlinuz.fedora    Fedora 45's kernel-core vmlinuz, as Fedora signed it
#   $LAB/probe/vmlinuz.mok       the same image with Fedora's signature removed and the TEST MOK's attached: the shape
#                                of a bootc-Rime kernel (verified by shim through MokList, not shim's built-in cert)
#   $LAB/probe/initrd.img        busybox: prints SB state, PCR 7 (sysfs) and the firmware event log, powers off
#   $LAB/disks/probe-{fedora,mok}.raw  GPT + ESP: shim (BOOTX64.EFI) -> GRUB -> that kernel + initrd
#   $LAB/disks/data.raw          LUKS2 (fixed UUID): the TEST passphrase + a TEST recovery key (systemd-cryptenroll)
set -euo pipefail
LAB=${LAB:-/lab}
UUID=$1
P=$LAB/probe D=$LAB/disks K=$LAB/keys
mkdir -p "$P" "$D"
rm -rf "$P/rpm" "$P/initrd"
mkdir -p "$P/rpm"
rpm=$(ls /opt/fedora-kernel/kernel-core-*.rpm)
echo "kernel rpm: $(rpm -qp --qf '%{NEVRA}' "$rpm") sha256 $(sha256sum "$rpm" | cut -d' ' -f1)"
(cd "$P/rpm" && rpm2cpio "$rpm" | cpio -idm --quiet './lib/modules/*/vmlinuz')
cp "$P"/rpm/lib/modules/*/vmlinuz "$P/vmlinuz.fedora"
rm -rf "$P/rpm"
echo "--- vmlinuz.fedora signatures"; sbverify --list "$P/vmlinuz.fedora"
cp "$P/vmlinuz.fedora" "$P/vmlinuz.unsigned"; sbattach --remove "$P/vmlinuz.unsigned"
sbsign --key "$K/test-mok.key" --cert "$K/test-mok.crt" --output "$P/vmlinuz.mok" "$P/vmlinuz.unsigned" 2>/dev/null
rm -f "$P/vmlinuz.unsigned"
echo "--- vmlinuz.mok signatures"; sbverify --list "$P/vmlinuz.mok"

# busybox initrd
I=$P/initrd
mkdir -p "$I"/{bin,dev,proc,sys,run}
cp "$(command -v busybox)" "$I/bin/busybox"
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
echo 1 > /proc/sys/kernel/printk   # keep kernel messages out of the base64 below
p() { echo "RIME-TPM: $*"; }
p "probe reached: kernel=$(uname -r) cmdline=$(cat /proc/cmdline)"
v=/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c
p "secureboot-var=$(od -An -tu1 "$v" | tr -s ' ' | sed 's/^ //')"
i=0
while [ ! -r /sys/class/tpm/tpm0/pcr-sha256/7 ] && [ $i -lt 50 ]; do sleep 0.2; i=$((i + 1)); done
p "pcr7-sysfs: $(cat /sys/class/tpm/tpm0/pcr-sha256/7)"
p "eventlog-begin"
base64 -w 0 /sys/kernel/security/tpm0/binary_bios_measurements; echo
p "eventlog-end"
dmesg | grep -iE "secure ?boot|lockdown|X\.509|tpm" | sed 's/^/RIME-TPM: dmesg| /'
p "DONE, powering off"
sync
poweroff -f
INIT
chmod 755 "$I/init"
(cd "$I" && find . | sort | cpio -o -H newc --quiet | gzip -9) > "$P/initrd.img"
rm -rf "$I"

ESP_TYPE=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
esp_disk() {   # NAME VOLID KERNEL
  local name=$1 volid=$2 kernel=$3 w=$D/$1.d
  rm -rf "$w"; mkdir -p "$w/EFI/BOOT" "$w/EFI/fedora" "$w/probe"
  cp "$LAB/efi/shimx64.efi" "$w/EFI/BOOT/BOOTX64.EFI"
  cp "$LAB"/efi/{grubx64.efi,mmx64.efi} "$w/EFI/BOOT/"
  cp "$LAB"/efi/{shimx64.efi,grubx64.efi,mmx64.efi} "$w/EFI/fedora/"
  cp "$kernel" "$w/probe/vmlinuz"; cp "$P/initrd.img" "$w/probe/initrd.img"
  for x in BOOT fedora; do
    cat > "$w/EFI/$x/grub.cfg" <<CFG
set timeout=0
search --no-floppy --fs-uuid --set=root ${volid:0:4}-${volid:4:4}
linux /probe/vmlinuz console=ttyS0,115200 rdinit=/init panic=-1 rime.probe=$name
initrd /probe/initrd.img
boot
CFG
  done
  rm -f "$w.img"; truncate -s 64M "$w.img"
  mkfs.vfat -F 32 -i "$volid" -n PROBE "$w.img" >/dev/null
  mcopy -s -i "$w.img" "$w"/* ::/
  rm -f "$D/$name.raw"; truncate -s 66M "$D/$name.raw"
  printf 'label: gpt\nstart=2048, size=131072, type=%s, name="EFI System Partition"\n' "$ESP_TYPE" | sfdisk -q "$D/$name.raw"
  dd if="$w.img" of="$D/$name.raw" bs=1M seek=1 conv=notrunc,sparse status=none
  rm -rf "$w" "$w.img"
  echo "made $D/$name.raw ($(sha256sum "$kernel" | cut -c1-16) kernel)"
}
esp_disk probe-fedora 1111AAAA "$P/vmlinuz.fedora"
esp_disk probe-mok 2222BBBB "$P/vmlinuz.mok"

# LUKS2 data disk: TEST passphrase (pbkdf2, 1000 iterations: lab speed), then a TEST recovery key, the way the
# installer enrols one (systemd-cryptenroll --recovery-key).
rm -f "$D/data.raw"; truncate -s 64M "$D/data.raw"
cryptsetup luksFormat --type luks2 -q --uuid "$UUID" --pbkdf pbkdf2 --pbkdf-force-iterations 1000 \
  --key-file "$K/TEST-passphrase" "$D/data.raw"
PASSWORD=$(cat "$K/TEST-passphrase") systemd-cryptenroll --recovery-key "$D/data.raw" 2>/dev/null \
  | tr -d '[:space:]' > "$K/TEST-recovery-key"
chmod 600 "$K/TEST-recovery-key"
[[ $(wc -c < "$K/TEST-recovery-key") -ge 60 ]] || { echo "mkprobe: no recovery key" >&2; exit 1; }
cryptsetup luksDump "$D/data.raw" | grep -E '^(UUID|Keyslots|Tokens)|^  [0-9]+: '
echo "TEST recovery key enrolled (in $K/TEST-recovery-key, never committed)"
