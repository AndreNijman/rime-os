#!/usr/bin/env bash
# tests/test-rime-secureboot-key.sh — the boot-menu way back when Secure Boot
# refuses the Rime kernel (files/system/libexec/rime-secureboot-key).
#
# Runs unprivileged: `place` works on plain directories, and `ensure` runs
# with RIME_SBK_PREFIX (a fake root) and stubbed findmnt/lsblk/mount/umount/
# mountpoint on PATH, where a "mount" is a symlink to a fake device's files.
# The VM proof (a real shim, a real MokManager, OVMF with Secure Boot on) is
# installer/test-installer-sb-recovery.sh.
#
# RIME_SBK_UNDER_TEST=<path> runs the same cases against another copy of the
# helper, which is how the mutants in the PR were checked.
set -euo pipefail
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SBK="${RIME_SBK_UNDER_TEST:-$REPO/files/system/libexec/rime-secureboot-key}"
UNIT="$REPO/files/system/units/rime-secureboot-key.service"
BASECF="$REPO/Containerfile.base"

PASS=0 FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

[[ -x "$REPO/files/system/libexec/rime-secureboot-key" ]] || { echo "FATAL: helper missing or not executable" >&2; exit 1; }

count() { grep -cxF -- "$1" "$2" 2>/dev/null || true; }

# A fake ESP as bootupd leaves it.
mk_esp() {  # $1 = dir  [$2 = bootuuid written into EFI/fedora/bootuuid.cfg]
  mkdir -p "$1/EFI/fedora" "$1/EFI/BOOT"
  printf 'MokManager' > "$1/EFI/fedora/mmx64.efi"
  printf 'shim' > "$1/EFI/fedora/shimx64.efi"
  printf 'shim' > "$1/EFI/BOOT/BOOTX64.EFI"
  if [[ -n "${2:-}" ]]; then printf 'set BOOT_UUID="%s"\n' "$2" > "$1/EFI/fedora/bootuuid.cfg"; fi
}
CERT="$TMP/rime-mok.der"
head -c 900 /dev/urandom > "$CERT"

# ─────────────────────────────────────────────────────────────────────────────
sec "the GRUB block"
B="$TMP/block.cfg"
"$SBK" block > "$B"
[[ "$(head -1 "$B")" == '### BEGIN rime-secureboot-key ###' ]] && ok "starts with the BEGIN marker" || bad "BEGIN marker"
[[ "$(tail -1 "$B")" == '### END rime-secureboot-key ###' ]] && ok "ends with the END marker" || bad "END marker"
pipe_has -F 'if [ "$shim_lock" = "y" ]; then' "$B" && ok "shown only when shim reports Secure Boot on" || bad "no shim_lock gate"
pipe_has -F 'if [ "$grub_platform" = "efi" ]; then' "$B" && ok "EFI only" || bad "no grub_platform gate"
pipe_has -F -- "--id rime-enrol-key" "$B" && ok "has a stable menu id" || bad "menu id"
pipe_has -F 'search --no-floppy --file --set=rime_esp /EFI/rime/rime-mok.der' "$B" && ok "finds the ESP by the key file" || bad "search line"
pipe_has -F 'chainloader ($rime_esp)/EFI/fedora/mmx64.efi' "$B" && ok "chainloads Fedora-signed MokManager from that ESP" || bad "chainloader line"
pipe_has -F 'sleep --verbose --interruptible 20' "$B" && ok "waits 20 s, Esc goes back" || bad "sleep line"
# `read` is not built into Fedora's signed grubx64.efi (measured: "can't find
# command `read'") and Secure Boot forbids insmod of anything else.
if grep -E '^[[:space:]]*read([[:space:]]|$)' "$B" >/dev/null; then bad "uses GRUB read, which signed GRUB lacks"; else ok "does not use GRUB read"; fi
if grep -E '&&|\|\|' "$B" >/dev/null; then bad "GRUB script has no && / ||"; else ok "no && / || (GRUB script has neither)"; fi
if grep -Ei '[0-9a-f]{8}-[0-9a-f]{4}-|--fs-uuid' "$B" >/dev/null; then bad "machine-specific value in the block"; else ok "no UUID: the same text on every machine"; fi
# The text the user reads must fit an 80-column GRUB console.
long=$(sed -n 's/^[[:space:]]*echo "\(.*\)"$/\1/p' "$B" | sed 's/\\"/"/g' | awk 'length($0) > 78' | head -1)
[[ -z "$long" ]] && ok "every message line fits 80 columns" || bad "message line too long: $long"
pipe_has -F 'Enroll key from disk' "$B" && ok "names MokManager's own menu item" || bad "steps do not name Enroll key from disk"

# ─────────────────────────────────────────────────────────────────────────────
sec "place: a fresh install"
E="$TMP/p1/esp" BT="$TMP/p1/boot"
mk_esp "$E"; mkdir -p "$BT/grub2"
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>"$TMP/p1.err" && ok "place exits 0" || bad "place failed: $(cat "$TMP/p1.err")"
cmp -s "$CERT" "$E/EFI/rime/rime-mok.der" && ok "certificate at \\EFI\\rime\\rime-mok.der" || bad "certificate not placed"
cmp -s "$B" "$BT/grub2/custom.cfg" && ok "custom.cfg is exactly the block" || bad "custom.cfg differs from the block"
[[ "$(stat -c %a "$BT/grub2/custom.cfg")" == 644 ]] && ok "custom.cfg is 0644" || bad "custom.cfg mode $(stat -c %a "$BT/grub2/custom.cfg")"
ls -A "$BT/grub2" | pipe_has '^\.custom' && bad "temp file left in grub2/" || ok "no temp file left"
cp "$BT/grub2/custom.cfg" "$TMP/p1.before"
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>/dev/null && ok "second run exits 0" || bad "second run failed"
cmp -s "$TMP/p1.before" "$BT/grub2/custom.cfg" && ok "idempotent: second run leaves custom.cfg alone" || bad "second run changed custom.cfg"

# ─────────────────────────────────────────────────────────────────────────────
sec "place: keeps the installer's Windows entry"
E="$TMP/p2/esp" BT="$TMP/p2/boot"
mk_esp "$E"; mkdir -p "$BT/grub2"
cat > "$BT/grub2/custom.cfg" <<'EOF'
# Rime OS: Windows is installed beside Rime on this machine.
menuentry 'Windows' --class windows --id windows {
  insmod part_gpt
  insmod fat
  insmod chain
  search --no-floppy --fs-uuid --set=root 1234-ABCD
  chainloader /EFI/Microsoft/Boot/bootmgfw.efi
}
set timeout=5
EOF
cp "$BT/grub2/custom.cfg" "$TMP/win.cfg"
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>/dev/null && ok "place exits 0" || bad "place failed"
head -n "$(wc -l < "$TMP/win.cfg")" "$BT/grub2/custom.cfg" | cmp -s - "$TMP/win.cfg" && ok "Windows entry kept first, byte for byte" || bad "Windows entry changed"
[[ "$(count "set timeout=5" "$BT/grub2/custom.cfg")" == 1 ]] && ok "Windows timeout kept once" || bad "timeout line count"
[[ "$(count '### BEGIN rime-secureboot-key ###' "$BT/grub2/custom.cfg")" == 1 ]] && ok "one block" || bad "block count"
cp "$BT/grub2/custom.cfg" "$TMP/p2.before"
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>/dev/null
cmp -s "$TMP/p2.before" "$BT/grub2/custom.cfg" && ok "idempotent with the Windows entry" || bad "second run changed custom.cfg"

sec "place: replaces an older block in place"
{ cat "$TMP/win.cfg"; echo; echo '### BEGIN rime-secureboot-key ###'; echo "menuentry 'old wording' { true }"; echo '### END rime-secureboot-key ###'; echo; } > "$BT/grub2/custom.cfg"
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>/dev/null
cmp -s "$TMP/p2.before" "$BT/grub2/custom.cfg" && ok "old block replaced by the current one, Windows kept" || bad "stale block not replaced cleanly"
pipe_has -F 'old wording' "$BT/grub2/custom.cfg" && bad "old block text survived" || ok "old block text gone"

sec "place: a changed certificate is replaced"
head -c 900 /dev/urandom > "$TMP/cert2"
"$SBK" place --boot "$BT" --esp "$E" --cert "$TMP/cert2" 2>/dev/null
cmp -s "$TMP/cert2" "$E/EFI/rime/rime-mok.der" && ok "new certificate in place" || bad "certificate not replaced"

# ─────────────────────────────────────────────────────────────────────────────
sec "place: refusals"
E="$TMP/p3/esp" BT="$TMP/p3/boot"
mkdir -p "$E/EFI/Microsoft/Boot" "$BT/grub2"
set +e
"$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>"$TMP/p3.err"; rc=$?
set -e
[[ "$rc" == 1 ]] && ok "an ESP without MokManager is refused (rc 1)" || bad "rc $rc for an ESP without MokManager"
[[ ! -e "$E/EFI/rime" ]] && ok "nothing written to that ESP" || bad "wrote to an ESP without MokManager"
pipe_has -F 'mmx64.efi' "$TMP/p3.err" && ok "the refusal says why" || bad "refusal text: $(cat "$TMP/p3.err")"
set +e
"$SBK" place --boot "$BT" 2>/dev/null; rc=$?
"$SBK" bogus 2>/dev/null; rc2=$?
set -e
[[ "$rc" == 2 && "$rc2" == 2 ]] && ok "usage errors exit 2" || bad "usage rc $rc/$rc2"
E="$TMP/p4/esp"; mk_esp "$E"; BT="$TMP/p4/boot"; mkdir -p "$BT"
set +e; "$SBK" place --boot "$BT" --esp "$E" --cert "$CERT" 2>/dev/null; rc=$?; set -e
[[ "$rc" == 1 && ! -e "$BT/grub2" ]] && ok "a /boot without grub2/ is refused, not created" || bad "rc $rc for /boot without grub2"

# ─────────────────────────────────────────────────────────────────────────────
# ensure: a fake booted machine. Devices: disk fakedisk with partitions
# fakedisk1 (an ESP left by another OS), fakedisk2 (this install's ESP),
# fakedisk3 (root, holding /boot).
mk_machine() {  # $1 = dir
  local R="$1/root" D="$1/devs" S="$1/stub"
  mkdir -p "$R/boot/grub2" "$R/boot/efi" "$R/sys/firmware/efi/efivars" "$R/usr/share/rime-os/secureboot" "$R/run" "$D" "$S"
  echo '# grub.cfg' > "$R/boot/grub2/grub.cfg"
  cp "$CERT" "$R/usr/share/rime-os/secureboot/rime-mok.der"
  echo signed > "$R/usr/share/rime-os/secureboot/kernel-signed"
  mk_esp "$D/fakedisk1" "11111111-2222-3333-4444-555555555555"
  mk_esp "$D/fakedisk2" "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
  : > "$1/mount.log"
  cat > "$S/findmnt" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"-o SOURCE"*)  echo '/dev/fakedisk3[/root/boot]' ;;
  *"-o UUID"*)    echo 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee' ;;
  *"-o OPTIONS"*) echo 'ro,relatime,seclabel' ;;
esac
EOF
  cat > "$S/lsblk" <<'EOF'
#!/usr/bin/env bash
dev="${*: -1}"
case "$*" in
  *"-ndo TYPE"*) [ "$dev" = /dev/fakedisk ] && echo disk || echo part ;;
  *"-no PKNAME"*) echo fakedisk ;;
  *"-lno NAME"*) printf 'fakedisk\nfakedisk1\nfakedisk2\nfakedisk3\n' ;;
  *"-dno PARTTYPE"*) case "$dev" in /dev/fakedisk1|/dev/fakedisk2) echo C12A7328-F81F-11D2-BA4B-00A0C93EC93B ;; *) echo 0fc63daf-8483-4772-8e79-3d69d8477de4 ;; esac ;;
esac
EOF
  cat > "$S/mount" <<EOF
#!/usr/bin/env bash
echo "mount \$*" >> "$1/mount.log"
case "\$*" in *remount*) exit 0 ;; esac
dev="\${*: -2:1}" dir="\${*: -1}"
src="$D/\${dev#/dev/}"
[ -d "\$src" ] || exit 32
rmdir "\$dir" && ln -s "\$src" "\$dir"
EOF
  cat > "$S/umount" <<EOF
#!/usr/bin/env bash
echo "umount \$*" >> "$1/mount.log"
dir="\${*: -1}"
[ -L "\$dir" ] && rm "\$dir" && mkdir "\$dir"
exit 0
EOF
  printf '#!/usr/bin/env bash\nexit 1\n' > "$S/mountpoint"
  chmod +x "$S"/*
}
run_ensure() {  # $1 = machine dir
  RIME_SBK_PREFIX="$1/root" RIME_SBK_NO_UNSHARE=1 PATH="$1/stub:$PATH" "$SBK" ensure
}

sec "ensure: picks this install's ESP by bootuuid.cfg, not the first ESP"
M="$TMP/m1"; mk_machine "$M"
run_ensure "$M" 2>"$TMP/m1.err" && ok "ensure exits 0" || bad "ensure failed: $(cat "$TMP/m1.err")"
cmp -s "$CERT" "$M/devs/fakedisk2/EFI/rime/rime-mok.der" && ok "certificate on the ESP whose bootuuid.cfg names /boot" || bad "certificate not on fakedisk2"
[[ ! -e "$M/devs/fakedisk1/EFI/rime" ]] && ok "the other OS's ESP untouched" || bad "wrote to the first ESP"
pipe_has -F 'mount -o remount,rw' "$M/mount.log" && ok "/boot remounted read-write (it is ro on bootc)" || bad "no remount rw"
pipe_has -F 'mount -o remount,ro' "$M/mount.log" && ok "and back to read-only" || bad "no remount ro"
cmp -s "$B" "$M/root/boot/grub2/custom.cfg" && ok "custom.cfg holds the block" || bad "custom.cfg wrong"
[[ -s "$M/root/var/lib/rime/secureboot-key/stamp" ]] && ok "stamp written" || bad "no stamp"
[[ -d "$M/root/run/rime-secureboot-key/esp" && ! -L "$M/root/run/rime-secureboot-key/esp" ]] && ok "ESP unmounted afterwards" || bad "ESP left mounted"
: > "$M/mount.log"
run_ensure "$M" 2>/dev/null && ok "second boot exits 0" || bad "second boot failed"
[[ ! -s "$M/mount.log" ]] && ok "second boot mounts nothing (stamp + custom.cfg match)" || bad "second boot mounted: $(cat "$M/mount.log")"

sec "ensure: custom.cfg lost its block -> put back"
: > "$M/root/boot/grub2/custom.cfg"
run_ensure "$M" 2>/dev/null
cmp -s "$B" "$M/root/boot/grub2/custom.cfg" && ok "block restored" || bad "block not restored"

sec "ensure: two ESPs with MokManager, neither names /boot -> no guess"
M="$TMP/m2"; mk_machine "$M"
rm "$M/devs/fakedisk2/EFI/fedora/bootuuid.cfg"
set +e; run_ensure "$M" 2>"$TMP/m2.err"; rc=$?; set -e
[[ "$rc" == 1 ]] && ok "exits 1" || bad "rc $rc"
[[ ! -e "$M/devs/fakedisk1/EFI/rime" && ! -e "$M/devs/fakedisk2/EFI/rime" ]] && ok "no certificate written anywhere" || bad "guessed an ESP"
[[ ! -e "$M/root/var/lib/rime/secureboot-key/stamp" ]] && ok "no stamp, so the next boot tries again" || bad "stamp written after a failure"

sec "ensure: one ESP with MokManager and no bootuuid.cfg -> that one"
M="$TMP/m3"; mk_machine "$M"
rm "$M/devs/fakedisk2/EFI/fedora/bootuuid.cfg" "$M/devs/fakedisk1/EFI/fedora/mmx64.efi"
run_ensure "$M" 2>/dev/null && ok "exits 0" || bad "failed"
cmp -s "$CERT" "$M/devs/fakedisk2/EFI/rime/rime-mok.der" && ok "certificate on the only ESP with MokManager" || bad "not placed"

sec "ensure: /boot/efi is a mount point -> used as it is"
M="$TMP/m4"; mk_machine "$M"
mk_esp "$M/root/boot/efi"
printf '#!/usr/bin/env bash\n[ "${*: -1}" = "%s" ]\n' "$M/root/boot/efi" > "$M/stub/mountpoint"
run_ensure "$M" 2>/dev/null && ok "exits 0" || bad "failed"
cmp -s "$CERT" "$M/root/boot/efi/EFI/rime/rime-mok.der" && ok "certificate on /boot/efi" || bad "not on /boot/efi"
if pipe_has -E 'mount -o (ro|rw) /dev/' "$M/mount.log"; then bad "mounted a device although /boot/efi was mounted"; else ok "no device mounted"; fi

sec "ensure: machines it must leave alone"
for case in no-efi sdboot no-grub unsigned; do
  M="$TMP/m-$case"; mk_machine "$M"
  case "$case" in
    no-efi)   rm -rf "$M/root/sys/firmware/efi" ;;
    sdboot)   : > "$M/root/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f" ;;
    no-grub)  rm "$M/root/boot/grub2/grub.cfg" ;;
    unsigned) echo unsigned > "$M/root/usr/share/rime-os/secureboot/kernel-signed" ;;
  esac
  set +e; run_ensure "$M" 2>/dev/null; rc=$?; set -e
  if [[ "$rc" == 0 && ! -s "$M/mount.log" && ! -e "$M/root/boot/grub2/custom.cfg" && ! -e "$M/devs/fakedisk2/EFI/rime" ]]; then
    ok "$case: exit 0, nothing mounted, nothing written"
  else
    bad "$case: rc $rc, mounts: $(tr '\n' ' ' < "$M/mount.log")"
  fi
done

# ─────────────────────────────────────────────────────────────────────────────
sec "the unit and the image"
pipe_has -x 'ExecStart=/usr/libexec/rime-secureboot-key ensure' "$UNIT" && ok "unit runs ensure" || bad "unit ExecStart"
pipe_has -x 'WantedBy=multi-user.target' "$UNIT" && ok "WantedBy, never blocks a boot" || bad "unit install section"
pipe_has -x 'ConditionPathExists=/boot/grub2/grub.cfg' "$UNIT" && ok "skipped on non-GRUB machines" || bad "no grub.cfg condition"
pipe_has -x 'Type=oneshot' "$UNIT" && ok "oneshot" || bad "not oneshot"
if pipe_has '^SELinuxContext=' "$UNIT"; then bad "unit forces an SELinux context"; else ok "no SELinuxContext (unconfined_service_t writes boot_t and dosfs_t)"; fi
pipe_has -F 'COPY files/system/libexec/rime-secureboot-key /usr/libexec/rime-secureboot-key' "$BASECF" && ok "image ships the helper" || bad "helper not in Containerfile.base"
pipe_has -F 'systemctl enable rime-secureboot-key.service' "$BASECF" && ok "image enables the unit" || bad "unit not enabled"
if grep -nE '^[^#]*\b(mokutil|efibootmgr|bootupctl|grub2-install|grub2-mkconfig)\b' "$SBK" >/dev/null; then
  bad "the helper runs an NVRAM or loader tool"
else
  ok "the helper never runs mokutil, efibootmgr or a loader installer"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" == 0 ]]
