#!/usr/bin/env bash
#
# lab.sh: the OVMF Secure Boot probe (PLAN.md PR 5, D3 gate P0). Runs INSIDE the
# rime-ovmf-probe container (see Containerfile and run-all.sh), never on a host:
# every disk, varstore and key is a file under $ART.
#
# Proves (or refutes) that Fedora 45's Microsoft-signed shim and Fedora-signed
# GRUB boot a NixOS kernel signed with a (throwaway) MOK plus its NixOS initrd,
#   (a) from a BLS entry on the ESP (what a fresh install writes, D9), and
#   (b) from a custom.cfg menu entry on a bootc-shaped btrfs /boot (what a
#       migrated machine gets, D3),
# and that an unsigned kernel is refused with Secure Boot on.
#
# Inputs ($ART/in, made by build-nixos.sh): bzImage, initrd, rootfs.img, entry.json
# Outputs: $ART/{fedora-rpms,keys,work,logs}; verdicts in $ART/logs/results.tsv
#
# Usage: lab.sh all | rpms | keys | vars | sign | disks | cases | case NAME
set -euo pipefail

ART=${ART:-/art}
IN=$ART/in
RPMS=$ART/fedora-rpms
KEYS=$ART/keys
W=$ART/work
LOGS=$ART/logs
OVMF=/usr/share/edk2/ovmf
STATIC=$ART/bootc-ref/grub2-static   # copied from quay.io/fedora/fedora-bootc:45 by run-all.sh

# Fixed filesystem ids so the generated configs are reproducible.
ESP_VOLID=1234ABCD                    # FAT volume id -> GRUB/blkid UUID "1234-ABCD"
ESP_UUID=1234-ABCD
BTRFS_UUID=6f0e3b1c-5a1d-4c3e-9e1f-2b7d0c8a9e01
ESP_TYPE=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
LINUX_TYPE=0FC63DAF-8483-4772-8E79-3D69D8477DE4

mkdir -p "$RPMS" "$KEYS" "$W" "$LOGS"
say() { printf '>> %s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

# ── 1. Fedora's signed shim and GRUB, from the fedora:45 repos ────────────────
step_rpms() {
  cd "$RPMS"
  ls shim-x64-*.rpm >/dev/null 2>&1 || dnf -q download --arch x86_64 shim-x64
  ls grub2-efi-x64-[0-9]*.rpm >/dev/null 2>&1 || dnf -q download --arch x86_64 grub2-efi-x64
  {
    for r in shim-x64-*.rpm grub2-efi-x64-[0-9]*.rpm; do
      rpm -qp --qf '%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH} ' "$r"; rpm -K "$r"
    done
    sha256sum shim-x64-*.rpm grub2-efi-x64-[0-9]*.rpm
  } | tee "$LOGS/rpms.txt"
  rm -rf "$W/rpmroot"; mkdir -p "$W/rpmroot"
  for r in shim-x64-*.rpm grub2-efi-x64-[0-9]*.rpm; do
    (cd "$W/rpmroot" && rpm2cpio "$RPMS/$r" | cpio -idm --quiet)
  done
  # Fedora 45 ships the EFI binaries under /usr/lib/efi/<pkg>/<version>/EFI/
  # (bootupd copies them to the ESP); older releases used /boot/efi/EFI/.
  local e=$W/efi
  rm -rf "$e"; mkdir -p "$e"
  for f in shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV; do
    local src
    src=$(find "$W/rpmroot" -path '*/EFI/fedora/*' -name "$f" -type f | head -n1)
    [[ -n $src ]] || die "missing $f in the RPMs"
    cp "$src" "$e/$f"; echo "$f <- ${src#"$W"/rpmroot}"
  done | tee -a "$LOGS/rpms.txt"
  {
    sha256sum "$e"/shimx64.efi "$e"/grubx64.efi "$e"/mmx64.efi
    for f in shimx64.efi grubx64.efi; do echo "--- sbverify --list $f"; sbverify --list "$e/$f" 2>&1; done
  } | tee -a "$LOGS/rpms.txt"
}

# ── 2. A throwaway test MOK (never the Rime MOK) ─────────────────────────────
step_keys() {
  if [[ ! -f $KEYS/test-mok.key ]]; then
    openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
      -subj "/CN=Rime OVMF probe TEST MOK (throwaway, DO NOT TRUST)/" \
      -keyout "$KEYS/test-mok.key" -out "$KEYS/test-mok.crt" 2>/dev/null
    openssl x509 -in "$KEYS/test-mok.crt" -outform DER -out "$KEYS/test-mok.der"
    cat /proc/sys/kernel/random/uuid > "$KEYS/test-mok.guid"
    chmod 600 "$KEYS/test-mok.key"
  fi
  { openssl x509 -in "$KEYS/test-mok.crt" -noout -subject
    openssl x509 -in "$KEYS/test-mok.crt" -noout -fingerprint -sha1
    openssl x509 -in "$KEYS/test-mok.crt" -noout -fingerprint -sha256
    echo "owner GUID $(cat "$KEYS/test-mok.guid")"; } | tee "$LOGS/keys.txt"
}

# ── 3. Firmware: Fedora's SB OVMF; three varstores ──────────────────────────
step_vars() {
  cd "$W"
  qemu-img convert -O raw "$OVMF/OVMF_CODE_4M.secboot.qcow2" code-secboot.raw
  qemu-img convert -O raw "$OVMF/OVMF_VARS_4M.secboot.qcow2" vars-secboot.raw   # SB on, MS keys enrolled
  qemu-img convert -O raw "$OVMF/OVMF_VARS_4M.qcow2"         vars-blank.raw     # no keys: SB off
  # sb-mok: Fedora's enrolled store + the test MOK pre-seeded in MokList (NV|BS),
  # the state shim leaves behind after a MokManager enrolment.
  virt-fw-vars --input vars-secboot.raw --output vars-sb-mok.fd \
    --add-mok "$(cat "$KEYS/test-mok.guid")" "$KEYS/test-mok.der" --set-shim-verbose
  # sb-nomok: the same store without the MOK (the MOK must be what admits the kernel).
  virt-fw-vars --input vars-secboot.raw --output vars-sb-nomok.fd --set-shim-verbose
  cp vars-blank.raw vars-sb-off.fd
  for v in vars-sb-mok.fd vars-sb-nomok.fd vars-sb-off.fd; do
    echo "===== $v"; virt-fw-vars --input "$v" --print --verbose 2>&1 \
      | grep -E '^name=|SecureBoot|SetupMode|MokList|CN=|subject|x509' | head -60 || echo "(no variables: a blank store, Setup Mode, Secure Boot off)"
  done | tee "$LOGS/vars.txt"
}

# ── 4. Sign the NixOS kernel with the test MOK ──────────────────────────────
step_sign() {
  cd "$W"
  cp "$IN/bzImage" bzImage.unsigned
  sbsign --key "$KEYS/test-mok.key" --cert "$KEYS/test-mok.crt" --output bzImage.signed bzImage.unsigned
  { echo "--- sbverify --cert test-mok.crt bzImage.signed"; sbverify --cert "$KEYS/test-mok.crt" bzImage.signed 2>&1
    echo "--- sbverify --list bzImage.unsigned"; sbverify --list bzImage.unsigned 2>&1 || true
    sha256sum bzImage.unsigned bzImage.signed "$IN/initrd" "$IN/rootfs.img"
  } | tee "$LOGS/sign.txt"
}

# ── 5. Disks ─────────────────────────────────────────────────────────────────
# Menu entry data, the way the future boot.loader.external hook gets it (bootspec).
entry() { jq -r "$1" "$IN/entry.json"; }
kname() { basename "$(dirname "$(entry .kernel)")"; }   # <hash>-linux-7.3
iname() { basename "$(dirname "$(entry .initrd)")"; }   # <hash>-initrd-linux-7.3
options() { echo "init=$(entry .init) $(entry '.params | join(" ")')"; }

# bootupd's static GRUB config, assembled exactly as bootupd installs it:
# grub-static-pre.cfg followed by configs.d/* in name order.
inner_grub_cfg() {
  cat "$STATIC/grub-static-pre.cfg"
  for f in "$STATIC"/configs.d/*.cfg; do echo "### BEGIN $(basename "$f") ###"; cat "$f"; done
}

# ESP common part: Fedora's layout plus the removable path. fbx64.efi is left
# out on purpose: shim launched as BOOTX64.EFI would run fallback and reboot.
populate_esp_loaders() {
  local d=$1 boot_uuid=$2 e=$W/efi
  mkdir -p "$d/EFI/fedora" "$d/EFI/BOOT"
  cp "$e"/shimx64.efi "$e"/grubx64.efi "$e"/mmx64.efi "$e"/BOOTX64.CSV "$d/EFI/fedora/"
  cp "$e/shimx64.efi" "$d/EFI/BOOT/BOOTX64.EFI"
  cp "$e/grubx64.efi" "$e/mmx64.efi" "$d/EFI/BOOT/"
  for x in fedora BOOT; do
    cp "$STATIC/grub-static-efi.cfg" "$d/EFI/$x/grub.cfg"
    printf 'set BOOT_UUID="%s"\n' "$boot_uuid" > "$d/EFI/$x/bootuuid.cfg"
  done
}

mkfat() {   # mkfat DIR IMG SIZE_MIB
  rm -f "$2"; truncate -s "$3M" "$2"
  mkfs.vfat -F 32 -i "$ESP_VOLID" -n RIMEESP "$2" >/dev/null
  mcopy -s -i "$2" "$1"/* ::/
}

# gpt IMG  PART_IMG TYPE NAME  [PART_IMG TYPE NAME ...]; 1 MiB aligned.
gpt() {
  local img=$1; shift
  local total=2 parts=() start=2048 table
  local -a args=("$@")
  for ((i=0; i<${#args[@]}; i+=3)); do total=$(( total + $(stat -c %s "${args[i]}") / 1048576 )); done
  rm -f "$img"; truncate -s "$(( total + 2 ))M" "$img"
  table=$'label: gpt\n'
  for ((i=0; i<${#args[@]}; i+=3)); do
    local sz=$(( $(stat -c %s "${args[i]}") / 512 ))
    table+="start=$start, size=$sz, type=${args[i+1]}, name=\"${args[i+2]}\""$'\n'
    parts+=("${args[i]}:$start")
    start=$(( start + sz ))
  done
  printf '%s' "$table" | sfdisk -q "$img"
  for p in "${parts[@]}"; do dd if="${p%%:*}" of="$img" bs=512 seek="${p##*:}" conv=notrunc,sparse status=none; done
}

# (a) fresh install: the ESP is /boot. bootupd-shaped grub2/ on the ESP, a BLS
#     entry in /loader/entries, kernel + initrd under /rime/nixos/.
make_disk_bls() {   # make_disk_bls signed|unsigned
  local k=$1 d=$W/esp-bls-$1
  rm -rf "$d"; mkdir -p "$d/grub2" "$d/loader/entries" "$d/rime/nixos"
  populate_esp_loaders "$d" "$ESP_UUID"
  inner_grub_cfg > "$d/grub2/grub.cfg"
  printf 'set BOOT_UUID="%s"\n' "$ESP_UUID" > "$d/grub2/bootuuid.cfg"
  cp "$W/bzImage.$k" "$d/rime/nixos/$(kname)-bzImage.efi"
  cp "$IN/initrd" "$d/rime/nixos/$(iname)-initrd.efi"
  cat > "$d/loader/entries/rime-nixos-generation-1.conf" <<EOF
title Rime (NixOS $(entry .label)) generation 1, kernel $k
version 1
linux /rime/nixos/$(kname)-bzImage.efi
initrd /rime/nixos/$(iname)-initrd.efi
options $(options)
EOF
  mkfat "$d" "$W/esp-bls-$k.fat" 256
  gpt "$W/disk-bls-$k.img" "$W/esp-bls-$k.fat" "$ESP_TYPE" "EFI System Partition"
  { echo "===== disk-bls-$k: ESP tree"; (cd "$d" && find . -type f | sort)
    echo "--- loader/entries/rime-nixos-generation-1.conf"; cat "$d/loader/entries/rime-nixos-generation-1.conf"
    echo "--- sfdisk"; sfdisk -d "$W/disk-bls-$k.img"; } | tee "$LOGS/disk-bls-$k.txt"
}

# (b) migrated bootc machine: ESP holds shim/GRUB + bootupd's stub grub.cfg that
#     finds /boot by UUID; /boot is a directory on a btrfs filesystem (katana's
#     topology E shape). ostree's /boot/loader -> loader.1 keeps a BLS entry for
#     the bootc deployment; Rime adds only $prefix/custom.cfg -> rime-nixos.cfg.
make_disk_custom() {   # make_disk_custom signed|unsigned
  local k=$1 d=$W/esp-custom-$1 b=$W/btrfs-custom-$1
  rm -rf "$d" "$b"; mkdir -p "$d" "$b/boot/grub2" "$b/boot/loader.1/entries" "$b/boot/rime/nixos" \
    "$b/boot/ostree/default-0123456789abcdef"
  populate_esp_loaders "$d" "$BTRFS_UUID"
  inner_grub_cfg > "$b/boot/grub2/grub.cfg"
  printf 'set BOOT_UUID="%s"\n' "$BTRFS_UUID" > "$b/boot/grub2/bootuuid.cfg"
  ln -s loader.1 "$b/boot/loader"
  # The bootc deployment's entry, as ostree writes it (first in the menu). Its
  # kernel is not on this probe disk: if GRUB ever picked it, the log says so.
  cat > "$b/boot/loader.1/entries/ostree-1.conf" <<EOF
title Fedora Linux 45 (ostree:0)
version 1
options root=UUID=$BTRFS_UUID rw ostree=/ostree/boot.1/default/0123456789abcdef/0
linux /boot/ostree/default-0123456789abcdef/vmlinuz-7.2.9-cachyos1.apex1.fc45.x86_64
initrd /boot/ostree/default-0123456789abcdef/initramfs-7.2.9-cachyos1.apex1.fc45.x86_64.img
EOF
  cp "$W/bzImage.$k" "$b/boot/rime/nixos/$(kname)-bzImage.efi"
  cp "$IN/initrd" "$b/boot/rime/nixos/$(iname)-initrd.efi"
  cat > "$b/boot/grub2/custom.cfg" <<'EOF'
# Rime: the only file Rime adds to bootc's GRUB config. 41_custom.cfg sources it.
if [ -f ${prefix}/rime-nixos.cfg ]; then
  source ${prefix}/rime-nixos.cfg
fi
EOF
  cat > "$b/boot/grub2/rime-nixos.cfg" <<EOF
# Rime-owned NixOS menu (what the boot.loader.external hook writes on a migrated
# machine). Paths are on \$boot, the btrfs filesystem that holds /boot.
menuentry 'Rime (NixOS $(entry .label)) generation 1, kernel $k' --id rime-nixos-generation-1 {
  linux /boot/rime/nixos/$(kname)-bzImage.efi $(options)
  initrd /boot/rime/nixos/$(iname)-initrd.efi
}
set timeout_style=menu
set timeout=3
set default=rime-nixos-generation-1
EOF
  mkfat "$d" "$W/esp-custom-$k.fat" 64
  rm -f "$W/btrfs-custom-$k.img"; truncate -s 512M "$W/btrfs-custom-$k.img"
  mkfs.btrfs -q -U "$BTRFS_UUID" -r "$b" "$W/btrfs-custom-$k.img"
  gpt "$W/disk-custom-$k.img" "$W/esp-custom-$k.fat" "$ESP_TYPE" "EFI System Partition" \
      "$W/btrfs-custom-$k.img" "$LINUX_TYPE" "root"
  { echo "===== disk-custom-$k: ESP tree"; (cd "$d" && find . -type f | sort)
    echo "===== btrfs tree"; (cd "$b" && find . \( -type f -o -type l \) | sort)
    echo "--- boot/grub2/custom.cfg"; cat "$b/boot/grub2/custom.cfg"
    echo "--- boot/grub2/rime-nixos.cfg"; cat "$b/boot/grub2/rime-nixos.cfg"
    echo "--- sfdisk"; sfdisk -d "$W/disk-custom-$k.img"; } | tee "$LOGS/disk-custom-$k.txt"
}

step_disks() {
  [[ -d $STATIC/configs.d ]] || die "missing $STATIC (run-all.sh copies it from fedora-bootc:45)"
  make_disk_bls signed; make_disk_bls unsigned
  make_disk_custom signed; make_disk_custom unsigned
}

# ── 6. Boot cases ────────────────────────────────────────────────────────────
# name | boot disk | varstore | expectation
CASES=(
  "a-bls-sbon-mok-signed      disk-bls-signed.img      vars-sb-mok.fd    boots"
  "b-custom-sbon-mok-signed   disk-custom-signed.img   vars-sb-mok.fd    boots"
  "a-bls-sbon-mok-unsigned    disk-bls-unsigned.img    vars-sb-mok.fd    refused"
  "b-custom-sbon-mok-unsigned disk-custom-unsigned.img vars-sb-mok.fd    refused"
  "a-bls-sbon-nomok-signed    disk-bls-signed.img      vars-sb-nomok.fd  refused"
  "a-bls-sboff-unsigned       disk-bls-unsigned.img    vars-sb-off.fd    boots"
  "b-custom-sboff-unsigned    disk-custom-unsigned.img vars-sb-off.fd    boots"
)

strip_ansi() { sed -e 's/\x1b\[[0-9;?]*[A-Za-z]//g' -e 's/\x1b[()][A-Z0-9]//g' -e 's/\r//g' "$1"; }

run_case() {   # run_case NAME DISK VARS EXPECT
  local name=$1 disk=$2 vars=$3 expect=$4 t serial=$LOGS/$1.serial.log
  cd "$W"
  cp "$vars" "run-$name.vars.fd"
  rm -f "run-$name.root.qcow2"
  qemu-img create -q -f qcow2 -b "$IN/rootfs.img" -F raw "run-$name.root.qcow2"
  t=150; [[ $expect == refused ]] && t=60
  : > "$serial"
  say "case $name: $disk + $vars, expect $expect (timeout ${t}s)"
  local start=$SECONDS rc=0
  timeout --signal=KILL "$t" qemu-system-x86_64 \
    -machine q35,smm=on,accel=kvm -cpu host -m 2048 -smp 2 \
    -global driver=cfi.pflash01,property=secure,value=on \
    -global ICH9-LPC.disable_s3=1 \
    -drive if=pflash,unit=0,format=raw,readonly=on,file="$W/code-secboot.raw" \
    -drive if=pflash,unit=1,format=raw,file="run-$name.vars.fd" \
    -drive if=virtio,format=raw,file="$disk",snapshot=on \
    -drive if=virtio,format=qcow2,file="run-$name.root.qcow2" \
    -debugcon "file:$LOGS/$name.ovmf.log" -global isa-debugcon.iobase=0x402 \
    -serial "file:$serial" -display none -no-reboot -nic none \
    2>"$LOGS/$name.qemu.stderr" || rc=$?
  local secs=$(( SECONDS - start ))
  strip_ansi "$serial" > "$LOGS/$name.serial.txt"
  # boots   = the NixOS stage-2 marker service ran to the end;
  # refused = GRUB's shim_lock verifier refused the kernel and nothing of the
  #           kernel ran (no initrd banner, no marker). A timeout alone is neither.
  local s=$LOGS/$name.serial.txt got sb=- want_sb=enabled
  [[ $vars == vars-sb-off.fd ]] && want_sb=disabled
  if grep -a 'RIME-OVMF-PROBE: DONE' "$s" >/dev/null; then got=boots
  elif grep -a 'bad shim signature' "$s" >/dev/null \
       && ! grep -a -E 'Booting initrd of NixOS|RIME-OVMF-PROBE' "$s" >/dev/null; then got=refused
  else got=unknown; fi
  if grep -a 'mokutil --sb-state: SecureBoot enabled' "$s" >/dev/null; then sb=enabled
  elif grep -a 'mokutil --sb-state: SecureBoot disabled' "$s" >/dev/null; then sb=disabled; fi
  local verdict=FAIL
  if [[ $got == "$expect" ]]; then
    verdict=PASS
    # A boot with SB on must also show the kernel saw SB on and the test MOK in MokListRT.
    if [[ $got == boots ]]; then
      [[ $sb == "$want_sb" ]] || verdict=FAIL
      if [[ $want_sb == enabled ]] && ! grep -a "Loaded X.509 cert 'Rime OVMF probe TEST MOK" "$s" >/dev/null; then verdict=FAIL; fi
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\texpect=%s\tgot=%s\tsb=%s\tqemu_rc=%s\t%ss\n' \
    "$verdict" "$name" "$disk" "$vars" "$(date -u +%FT%TZ)" "$expect" "$got" "$sb" "$rc" "$secs" | tee -a "$LOGS/results.tsv"
  rm -f "run-$name.vars.fd" "run-$name.root.qcow2"
}

# The lines that carry each verdict, de-duplicated (shim's verbose log repeats).
excerpts() {
  local pat='load_image\(\) attempting|AuthenticodeVerify\(\) succeeded|No .sbat section|Binary is not authorized|No signatures found|Security Policy Violation|bad shim signature|you need to load the kernel|Booting initrd of NixOS|RIME-OVMF-PROBE'
  for c in "${CASES[@]}"; do
    local n=${c%% *}
    echo "######## $n"
    grep -a -E "$pat" "$LOGS/$n.serial.txt" | sed 's/^.*RIME-OVMF-PROBE/RIME-OVMF-PROBE/; s/^.*Booting initrd/Booting initrd/' \
      | awk '!seen[$0]++' | cut -c1-260
  done > "$LOGS/excerpts.txt"
}

step_cases() {
  : > "$LOGS/results.tsv"
  for c in "${CASES[@]}"; do
    # shellcheck disable=SC2086
    run_case $c
  done
  excerpts
  echo; cat "$LOGS/results.tsv"
  ! grep -E '^FAIL' "$LOGS/results.tsv" >/dev/null
}

case "${1:-all}" in
  rpms) step_rpms;; keys) step_keys;; vars) step_vars;; sign) step_sign;;
  disks) step_disks;; cases) step_cases;;
  case) shift; for c in "${CASES[@]}"; do [[ ${c%% *} == "$1" ]] && { run_case $c; exit; }; done; die "no case $1";;
  all) step_rpms; step_keys; step_vars; step_sign; step_disks; step_cases;;
  *) die "usage: lab.sh all|rpms|keys|vars|sign|disks|cases|case NAME";;
esac
