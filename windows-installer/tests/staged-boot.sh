#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  staged-boot.sh — the Windows app's disk writes, judged by a real firmware.
#
#  Builds a disk image laid out the way Windows Setup lays out a disk (100 MiB
#  ESP holding a Windows boot manager, MSR, an NTFS C:, a recovery partition
#  at the end, first usable LBA 34) with unallocated space in the middle,
#  stages Rime into that space with the SAME code the .exe runs
#  (`rime-windows-installer stage-image`), stores the firmware entry the .exe
#  would store (Boot#### + BootNext, bytes printed by stage-image), and boots
#  it in QEMU under OVMF with Secure Boot ON and Microsoft's keys enrolled.
#
#  Pass criteria, read off the serial console:
#    * shim -> GRUB start from Rime's ESP (GRUB draws our menu on serial),
#    * the kernel finds the live root by PARTUUID and the live system boots,
#    * Windows' ESP and every Windows partition are byte-identical afterwards
#      (hashed before staging and after the boot),
#    * BootNext was consumed (the firmware ran our entry once).
#
#  Usage: staged-boot.sh ISO WORKDIR [EXTRA_KARGS]
#  Needs podman, /dev/kvm and localhost/apex-winlab (windows-installer/lab).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
ISO="$(realpath "$1")"; WORK="$(realpath -m "$2")"; EXTRA="${3:-}"
IMAGE="${RIME_WINLAB_IMAGE:-localhost/apex-winlab:latest}"
mkdir -p "$WORK"
DISK="$WORK/disk.raw"

echo "== build the host binary =="
(cd "$HERE" && cargo build --offline --locked --release -q)
BIN="$HERE/target/release/rime-windows-installer"

echo "== a disk the way Windows Setup makes one =="
rm -f "$DISK"
podman run --rm -v "$WORK":/w:z "$IMAGE" bash -euo pipefail -c '
  truncate -s 64G /w/disk.raw
  last=$(( 64*1024*1024*2 - 1 ))
  rec_start=$(( (last - 33 - 600*2048 + 1) / 2048 * 2048 ))
  printf "label: gpt\nfirst-lba: 34\n%s\n%s\n%s\n%s\n" \
    "start=2048, size=100MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI system partition\"" \
    "size=16MiB, type=E3C9E316-0B5C-4DB8-817D-F92DF00215AE, name=\"Microsoft reserved partition\"" \
    "size=30GiB, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name=\"Basic data partition\"" \
    "start=$rec_start, size=600MiB, type=DE94BBA4-06D1-4D40-A16A-BFD50179D6AC, name=\"\"" \
    | sfdisk -q /w/disk.raw
  mformat -i /w/disk.raw@@1048576 -F -T 204800 -v SYSTEM ::
  mmd -i /w/disk.raw@@1048576 ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot ::/EFI/Boot
  head -c 1500000 /dev/urandom > /tmp/bootmgfw.efi
  mcopy -i /w/disk.raw@@1048576 /tmp/bootmgfw.efi ::/EFI/Microsoft/Boot/bootmgfw.efi
  mcopy -i /w/disk.raw@@1048576 /tmp/bootmgfw.efi ::/EFI/Boot/bootx64.efi
  s=$(sfdisk -J /w/disk.raw | python3 -c "import json,sys; p=json.load(sys.stdin)[\"partitiontable\"][\"partitions\"][2]; print(p[\"start\"], p[\"size\"])")
  set -- $s
  truncate -s $(( $2 * 512 )) /tmp/ntfs.img
  mkntfs --force --quiet --fast --label Windows /tmp/ntfs.img
  dd if=/tmp/ntfs.img of=/w/disk.raw bs=1M seek=$(( $1 / 2048 )) conv=notrunc,sparse status=none
  rm -f /tmp/ntfs.img
  sfdisk -d /w/disk.raw
'

windows_hash() {  # every Windows partition, hashed
  podman run --rm -v "$WORK":/w:z "$IMAGE" bash -euo pipefail -c '
    sfdisk -J /w/disk.raw | python3 -c "
import json,sys,hashlib
win={\"C12A7328-F81F-11D2-BA4B-00A0C93EC93B\",\"E3C9E316-0B5C-4DB8-817D-F92DF00215AE\",\"EBD0A0A2-B9E5-4433-87C0-68B6B72699C7\",\"DE94BBA4-06D1-4D40-A16A-BFD50179D6AC\"}
f=open(\"/w/disk.raw\",\"rb\")
for p in json.load(sys.stdin)[\"partitiontable\"][\"partitions\"]:
    if p[\"type\"].upper() not in win or p.get(\"name\")==\"Rime OS boot\": continue
    f.seek(p[\"start\"]*512); h=hashlib.sha256(); left=p[\"size\"]*512
    while left:
        b=f.read(min(left,8<<20)); h.update(b); left-=len(b)
    print(p[\"node\"], p[\"type\"], h.hexdigest())
"'
}
windows_hash > "$WORK/windows-before.txt"
cat "$WORK/windows-before.txt"

echo "== stage Rime into the free space, with the installer's own code =="
free=$(sfdisk -J "$DISK" | python3 -c '
import json,sys
t=json.load(sys.stdin)["partitiontable"]; ps=sorted(t["partitions"],key=lambda p:p["start"])
a=ps[2]["start"]+ps[2]["size"]; b=ps[3]["start"]-1
print(f"{a}-{b}")')
"$BIN" stage-image "$DISK" --iso "$ISO" --space "free:$free" | tee "$WORK/stage.txt"
grep -q STAGED-OK "$WORK/stage.txt"
BOOTNUM=$(sed -n 's/^BOOTNUM=//p' "$WORK/stage.txt")
OPT=$(sed -n 's/^LOADOPTION=//p' "$WORK/stage.txt")
ESPOFF=$(sed -n 's/^ESP_OFFSET=//p' "$WORK/stage.txt")
sgdisk -v "$DISK" | tee "$WORK/sgdisk.txt"
grep -q 'No problems found' "$WORK/sgdisk.txt"
windows_hash > "$WORK/windows-staged.txt"
diff -u "$WORK/windows-before.txt" "$WORK/windows-staged.txt"
echo "PASS  every Windows partition is byte-identical after staging"

echo "== firmware: the entry the .exe stores, and BootNext =="
podman run --rm -v "$WORK":/w:z -e BOOTNUM="$BOOTNUM" -e OPT="$OPT" -e ESPOFF="$ESPOFF" -e EXTRA="$EXTRA" "$IMAGE" bash -euo pipefail -c '
  if [ -n "$EXTRA" ]; then
    mtype -i "/w/disk.raw@@$ESPOFF" ::/EFI/rimeinst/grub.cfg > /tmp/grub.cfg
    sed -i "s|console=tty0|console=tty0 $EXTRA|" /tmp/grub.cfg
    mcopy -o -i "/w/disk.raw@@$ESPOFF" /tmp/grub.cfg ::/EFI/rimeinst/grub.cfg
  fi
  cp /usr/share/edk2/ovmf/OVMF_VARS.secboot.fd /w/vars.fd
  bn_le=$(printf "%s" "$BOOTNUM" | sed -E "s/(..)(..)/\2\1/")
  cat > /tmp/v.json <<EOF
{"version": 2, "variables": [
 {"name": "Boot$BOOTNUM", "guid": "8be4df61-93ca-11d2-aa0d-00e098032b8c", "attr": 7, "data": "$OPT"},
 {"name": "BootNext", "guid": "8be4df61-93ca-11d2-aa0d-00e098032b8c", "attr": 7, "data": "$bn_le"}]}
EOF
  virt-fw-vars -i /w/vars.fd -o /w/vars.fd --set-json /tmp/v.json >/dev/null
  virt-fw-vars -i /w/vars.fd --print 2>/dev/null | grep -E "^(Boot[0-9A-F]{4}|BootNext|BootOrder|SecureBoot)" | tee /w/fw-before.txt
'

echo "== boot it: OVMF, Secure Boot on, Microsoft keys =="
rm -f "$WORK/serial.log"
podman run --rm --device /dev/kvm -v "$WORK":/w:z \
  -e BOOT_SECONDS="${BOOT_SECONDS:-420}" -e BOOT_UNTIL="${BOOT_UNTIL:-}" -e SETTLE="${SETTLE:-20}" \
  -e HOLD="${HOLD:-}" -e QEMU_EXTRA="${QEMU_EXTRA:-}" "$IMAGE" bash -uo pipefail -c '
  timeout "${BOOT_SECONDS:-420}" qemu-system-x86_64 -name staged \
    -machine q35,accel=kvm,smm=on -cpu host -smp 4 -m 6144 \
    -global driver=cfi.pflash01,property=secure,value=on \
    -drive if=pflash,format=raw,unit=0,readonly=on,file=/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd \
    -drive if=pflash,format=raw,unit=1,file=/w/vars.fd \
    -device ich9-ahci,id=ahci \
    -drive id=d0,file=/w/disk.raw,format=raw,if=none,cache=unsafe \
    -device ide-hd,drive=d0,bus=ahci.0,model=RIME-STAGED-DISK,serial=STG0000001 \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
    -chardev socket,id=s1,path=/w/shell.sock,server=on,wait=off -serial file:/w/serial.log -serial chardev:s1 \
    -display none -vga std -monitor unix:/w/mon.sock,server,nowait -no-reboot ${QEMU_EXTRA:-} &
  q=$!
  for i in $(seq 1 "${BOOT_SECONDS:-420}"); do
    sleep 1
    if grep -qE "${BOOT_UNTIL:-EXT4-fs \\(dm-0\\): mounted}" /w/serial.log 2>/dev/null; then sleep "${SETTLE:-20}"; break; fi
    kill -0 $q 2>/dev/null || break
  done
  printf "screendump /w/screen.ppm\n" | socat - unix-connect:/w/mon.sock >/dev/null 2>&1; sleep 2
  if [ -n "${HOLD:-}" ]; then wait $q; else kill $q 2>/dev/null; wait $q 2>/dev/null; fi
  virt-fw-vars -i /w/vars.fd --print 2>/dev/null | grep -E "^(Boot[0-9A-F]{4}|BootNext|BootOrder)" > /w/fw-after.txt
  true
'
set +e +o pipefail
tr -d '\r' < "$WORK/serial.log" | grep -aE 'Install Rime OS|Back to Windows|dmsquash|live|Linux version|rime-installer|Secure boot|Reached target' | head -30
check() { if grep -aqE "$2" "$WORK/serial.log"; then echo "PASS  $1"; else echo "FAIL  $1"; FAILED=1; fi; }
FAILED=0
check "GRUB drew Rime's setup menu from Rime's ESP" 'Install Rime OS'
check "the kernel started" 'Linux version'
check "the live system came up" "${BOOT_UNTIL:-EXT4-fs \\(dm-0\\): mounted}"
if grep -q '^BootNext' "$WORK/fw-after.txt"; then echo "FAIL  BootNext was not consumed"; FAILED=1; else echo "PASS  BootNext was consumed by the firmware"; fi
windows_hash > "$WORK/windows-after.txt"
if diff -u "$WORK/windows-before.txt" "$WORK/windows-after.txt"; then echo "PASS  Windows partitions byte-identical after the boot"; else echo "FAIL  a Windows partition changed"; FAILED=1; fi
exit "$FAILED"
