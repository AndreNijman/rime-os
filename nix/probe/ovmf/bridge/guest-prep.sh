#!/bin/bash
# guest-prep.sh KIND: runs as root INSIDE a bridge lab VM (KVM guest, OVMF varstore copy), never on a real machine.
# Makes the firmware boot entries a real machine of KIND has:
#   A  "Fedora" -> \EFI\fedora\shimx64.efi on Rime's ESP (shim's fallback normally made it at first boot)
#   E  katana's: "APEX-OS Primary" -> \EFI\APEX\SHIMX64.EFI on the Windows disk's ESP, first in BootOrder; "Fedora";
#      and the failed trial's "Rime OS" -> \EFI\systemd\systemd-bootx64.efi (Boot0006 on katana), NOT in BootOrder.
set -euo pipefail
KIND=$1
[[ $(systemd-detect-virt) == kvm ]] || { echo "guest-prep: not a KVM guest; refusing"; exit 2; }
rime_disk=/dev/$(lsblk -no PKNAME "$(blkid -L rime-root || blkid -L apex-root)")
efibootmgr | sed 's/^/before| /'
fed=$(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\? Fedora.*/\1/p' | head -1)
if [[ -z $fed ]]; then
  efibootmgr -q -c -d "$rime_disk" -p 2 -L Fedora -l '\EFI\fedora\shimx64.efi'
  fed=$(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\? Fedora.*/\1/p' | head -1)
fi
order=$fed
if [[ $KIND == E ]]; then
  win_disk=/dev/$(lsblk -no PKNAME "$(blkid -L SYSTEM)")
  efibootmgr -q -c -d "$win_disk" -p 1 -L "APEX-OS Primary" -l '\EFI\APEX\SHIMX64.EFI'
  apex=$(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\? APEX-OS Primary.*/\1/p' | head -1)
  # katana's trial entry is Boot0006; here shim's fallback may already hold 0006 (two disks = one more
  # auto-created entry), so the first free number from 0006 up (the bridge finds the entry by its path).
  trial=
  for n in 0006 0007 0008 0009 000A 000B 000C 000D 000E 000F; do
    efibootmgr | grep "^Boot$n" >/dev/null || { trial=$n; break; }
  done
  [[ -n $trial ]] || { echo "guest-prep: no free entry number"; exit 1; }
  efibootmgr -q -b "$trial" -c -d "$rime_disk" -p 2 -L "Rime OS" -l '\EFI\systemd\systemd-bootx64.efi'
  order=$apex,$fed
fi
efibootmgr -q -o "$order"
efibootmgr -v | sed 's/^/after| /'
echo "GUEST-PREP DONE $KIND order=$order"
