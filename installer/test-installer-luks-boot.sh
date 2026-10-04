#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-luks-boot.sh — boot a disk THIS INSTALLER PRODUCED, for real,
#  under OVMF, and prove it reaches the real root.
#
#  WHY THIS EXISTS. installer/test-installer-luks-live.sh proves the engine's
#  OWN steps each report success: partitions the right sizes, the right types,
#  a LUKS2 header both keys open, a crypttab and kargs that name the right
#  UUIDs. None of that proves the disk BOOTS. installer/test-installer-
#  keymap-boot.sh boots a real guest and types a real passphrase, but against a
#  bare 256 MB volume with a direct kernel boot — no partition table, no ESP,
#  no firmware, no bootloader. As of 2026-09-21 nothing had booted a disk this
#  installer wrote end to end: firmware -> shim -> GRUB -> kernel -> dracut
#  asking for the passphrase -> the pivot into the real root.
#
#  This suite does exactly that: build the disk with the real engine, boot it
#  under OVMF in the rime-bootlab container, type the passphrase on an
#  emulated keyboard through QMP (installer/luks-boot-drive.py), and read the
#  serial log for dracut's own "Switching root" line.
#
#  WHY A SEPARATE INSTALL PHASE RATHER THAN REUSING test-installer-luks-live.sh.
#  That suite always deletes its 24 GB image on exit — filling /var/lab-scratch
#  once already took the machine's shell down — and it installs with
#  keymap=bg specifically to prove the XKB-to-console conversion. A boot test
#  needs the opposite: keep the disk, and use keymap=us so a passphrase typed
#  as literal QMP qcodes (a-z0-9, no shift) is unambiguous — the keymap
#  CONVERSION is that suite's subject, not this one's. Copying the ~60-line
#  install phase (stub enrolment helper, answers file, nvram-guard-wrapped
#  engine call) costs far less than the coupling of sharing it would.
#
#  WHAT IT NEEDS. Passwordless root, podman, /dev/kvm, ~25 GB of free disk on
#  /var/lab-scratch, a Rime OS image in ROOT podman storage
#  (localhost/rime-os:daily or RIME_LUKS_BOOT_IMAGE=), and the rime-bootlab
#  container image (bootlab/Containerfile; built if absent). It cannot run on
#  a GitHub runner and is listed in tests/suites-not-in-ci.txt.
#
#  WHAT IT REFUSES TO DO. Same as test-installer-luks-live.sh: the install
#  target is a /dev/loop* node this script attached itself, this machine's
#  UEFI boot variables are snapshotted and diffed by tests/lab/nvram-guard
#  around the INSTALL phase only. The BOOT phase that follows needs no such
#  guard: it is an unprivileged qemu process inside the rime-bootlab container
#  reading two pflash FILES and one disk IMAGE FILE in a bind-mounted work
#  directory — there is no host efivarfs anywhere near it, and every other
#  OVMF scenario in this lab (files/scripts/boot-v2/run-scenarios) boots the
#  same way with no guard, for the same reason. See luks-boot-drive.py's own
#  header for why the varstore is pristine (UEFI Setup Mode, not Secure Boot
#  enforcing) and why there is no `-no-reboot` on this first run.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 1
REPO=$(cd .. && pwd)

ENGINE=./rime-install
IMAGE="${RIME_LUKS_BOOT_IMAGE:-localhost/rime-os:daily}"
LAB="${RIME_BOOTLAB_IMAGE:-localhost/rime-bootlab}"
# us: no vconsole.keymap karg is added at all (rime-install omits it for
# exactly `us`), so the passphrase below is typed as literal QMP qcodes with
# no layout translation to reason about — that translation is
# test-installer-keymap-boot.sh's subject, not this suite's.
KEYMAP_XKB="us"
PASSPHRASE="rimebootproof1"
DISK_SIZE=24G

pass=0; fail=0
ok()  { printf 'PASS  %-52s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-52s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
die() { printf 'FATAL: %s\n' "$1" >&2; exit 2; }

sudo -n true 2>/dev/null || die "needs passwordless root (sudo -n)."
command -v podman    >/dev/null || die "podman is not installed."
command -v losetup   >/dev/null || die "losetup is not installed."
command -v cryptsetup>/dev/null || die "cryptsetup is not installed."
[ -c /dev/kvm ]                 || die "/dev/kvm is absent; a TCG-only OVMF+GRUB boot is too slow to be useful here."

NVGUARD="../tests/lab/nvram-guard"
# The installer's live-environment image the engine runs inside (see
# live-env-engine), built from Containerfile.installer.
LIVE_ENV_IMAGE="${RIME_LIVE_ENV_IMAGE:-localhost/rime-installer:latest}"
[ -x "$NVGUARD" ] || die "$NVGUARD is missing or not executable.
This suite will not run a privileged loopback install without it — see
BOOT-BREAKAGE-2026-09-20.md and AGENTS.md \"Touching a machine's boot path\"."
sudo -n podman image exists "$IMAGE" 2>/dev/null \
    || die "$IMAGE is not in ROOT podman storage. Build it, or set RIME_LUKS_BOOT_IMAGE."

# /var/lab-scratch, never /tmp: /tmp on this machine is a 15 GB tmpfs, and a
# disk image there is that much RAM. RIME_LUKS_BOOT_SCRATCH overrides it.
SCRATCH_ROOT="${RIME_LUKS_BOOT_SCRATCH:-/var/lab-scratch}"
mkdir -p "$SCRATCH_ROOT" 2>/dev/null
[ -d "$SCRATCH_ROOT" ] && [ -w "$SCRATCH_ROOT" ] || SCRATCH_ROOT=/var/tmp
case "$(df -PT "$SCRATCH_ROOT" 2>/dev/null | awk 'NR==2{print $2}')" in
  tmpfs|ramfs) die "$SCRATCH_ROOT is a RAM filesystem; a 24 GB image there would eat the machine. Set RIME_LUKS_BOOT_SCRATCH to somewhere on a real disk." ;;
esac
WORK=$(mktemp -d "$SCRATCH_ROOT/rime-luks-boot.XXXXXX") || die "no scratch directory"
chmod 755 "$WORK"
IMG="$WORK/target.img"
LOOP=""
MNT="$WORK/mnt"
BOOTMNT="$WORK/bootmnt"

cleanup() {
    sudo -n umount "$MNT/boot/efi" 2>/dev/null
    sudo -n umount "$MNT/boot"     2>/dev/null
    sudo -n umount "$MNT"          2>/dev/null
    sudo -n umount "$BOOTMNT"      2>/dev/null
    if [ -n "$LOOP" ]; then
        for _h in /sys/class/block/"$(basename "$LOOP")"*/holders/*; do
            [ -e "$_h" ] || continue
            _dm=$(cat "$_h/dm/name" 2>/dev/null) || continue
            [ -n "$_dm" ] && sudo -n cryptsetup close "$_dm" 2>/dev/null
        done
    fi
    [ -n "$LOOP" ] && sudo -n losetup -d "$LOOP" 2>/dev/null
    if [ "${fail:-1}" = 0 ] && [ "${RIME_LUKS_BOOT_KEEP:-0}" != 1 ]; then
        sudo -n rm -rf "$WORK" 2>/dev/null
    else
        printf 'artefacts kept in %s\n' "$WORK" >&2
    fi
    return 0
}
trap cleanup EXIT

# ── this machine's own firmware must come out of the INSTALL phase untouched ─
EFI_BEFORE="$WORK/efi-before.txt"
EFI_AFTER="$WORK/efi-after.txt"
# shellcheck disable=SC2024
sudo -n efibootmgr -v 2>/dev/null > "$EFI_BEFORE" || echo "(no efibootmgr)" > "$EFI_BEFORE"

# ── the target: a loopback file, and the script proves it is one ────────────
truncate -s "$DISK_SIZE" "$IMG" || die "could not create $IMG"
LOOP=$(sudo -n losetup -fP --show "$IMG") || die "losetup failed"
case "$LOOP" in
  /dev/loop[0-9]*) : ;;
  *) die "refusing to continue: losetup returned '$LOOP', which is not a loop device." ;;
esac
printf 'target: %s (%s, %s)\n\n' "$LOOP" "$IMG" "$DISK_SIZE"

# ── the stand-in enrolment helper — same stub test-installer-luks-live.sh
#    uses, the recovery-key half of the contract and nothing else ───────────
HELPER="$WORK/rime-luks-enroll"
cat > "$HELPER" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
dev=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --device)       dev="$2"; shift 2 ;;
    --recovery-out) out="$2"; shift 2 ;;
    *)              shift ;;
  esac
done
[ -n "$dev" ] && [ -n "$out" ] || { echo "stub: --device and --recovery-out are required" >&2; exit 2; }
umask 077
systemd-cryptenroll --recovery-key "$dev" > "$out" || exit 1
[ -s "$out" ] || exit 1
exit 0
STUB
chmod 755 "$HELPER"

RECOVERY_DIR="$WORK/recovery"
mkdir -p "$RECOVERY_DIR"

ANS="$WORK/answers"
{
  printf 'mode=disk\n'
  printf 'disk=%s\n' "$LOOP"
  printf 'username=tester\n'
  printf 'password=loginpw123\n'
  printf 'hostname=rimebootlab\n'
  printf 'encrypt=yes\n'
  printf 'lukspass=%s\n' "$PASSPHRASE"
  printf 'keymap=%s\n' "$KEYMAP_XKB"
  printf 'timezone=Australia/Perth\n'
  # The typed ERASE and the identity it was typed against, as the GUI writes
  # them; the engine refuses to write without both (see its confirmation
  # binding). Read after the loop node exists, which is when the GUI reads it.
  printf 'confirmed=ERASE\n'
  printf 'confirm_target=%s\n' "$LOOP"
  printf 'confirm_disk_id=%s\n' "$(lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$LOOP" | head -1)"
} > "$ANS"
chmod 600 "$ANS"

OUT="$WORK/engine-stdout.txt"
echo "── running the installer (this is the slow part) ──────────────────────"
start=$(date +%s)
# TEST-ONLY: makes the INSTALLED kernel and systemd visible on a serial
# console — this is the entire reason this suite can read a verdict off the
# guest at all. See rime-install's own comment on RIME_LUKS_EXTRA_KARGS.
EXTRA_KARGS="console=ttyS0,115200 console=tty1 systemd.log_target=kmsg systemd.show_status=1 loglevel=7 rd.plymouth=0 plymouth.enable=0 rd.timeout=120"
# shellcheck disable=SC2024
sudo -n "$NVGUARD" --label "luks-boot-install" --out "$WORK/nvram" -- \
  ./live-env-engine \
    RIME_LIVE_ENV_IMAGE="$LIVE_ENV_IMAGE" \
    RIME_IMAGE="$IMAGE" \
    RIME_LUKS_ENROLL_LOCAL="$HELPER" \
    RIME_RECOVERY_DIR="$RECOVERY_DIR" \
    RIME_LUKS_PBKDF_MEMORY=65536 \
    RIME_LUKS_EXTRA_KARGS="$EXTRA_KARGS" \
    -- "$ENGINE" --headless "$ANS" > "$OUT" 2>&1 </dev/null
rc=$?
echo "engine exit=$rc after $(( $(date +%s) - start ))s"

echo
echo "── the install itself ─────────────────────────────────────────────────"
if [ "$rc" = 0 ]; then ok "engine exit status" "0"
else bad "engine exit status" "$rc — see $OUT"; fi
lastproto=$(grep -v 'nvram-guard\[' "$OUT" | grep -v '^[[:space:]]*$' | tail -1)
if [[ "$lastproto" == "RIME-INSTALL-OK" ]]; then ok "final protocol line is RIME-INSTALL-OK"
else bad "final protocol line is RIME-INSTALL-OK" "got: $lastproto"; fi

if [ "$rc" != 0 ]; then
    echo
    echo "── the install failed; the boot cannot be attempted ───────────────────"
    tail -60 "$OUT"
    printf '\n%s passed, %s failed\n' "$pass" "$((fail + 1))"
    exit 1
fi

# shellcheck disable=SC2024
sudo -n efibootmgr -v 2>/dev/null > "$EFI_AFTER" || echo "(no efibootmgr)" > "$EFI_AFTER"
if diff -q "$EFI_BEFORE" "$EFI_AFTER" >/dev/null 2>&1; then
    ok "this machine's own UEFI boot entries did not move"
else
    bad "this machine's own UEFI boot entries did not move" "see $EFI_BEFORE / $EFI_AFTER — STOP, do not reboot this machine until this is understood"
fi

ESP_PART=$(sudo -n bash -c "for c in ${LOOP}p1 ${LOOP}1; do [ -b \$c ] && echo \$c && break; done")
BOOT_PART=$(sudo -n bash -c "for c in ${LOOP}p2 ${LOOP}2; do [ -b \$c ] && echo \$c && break; done")
LUKS_PART=$(sudo -n bash -c "for c in ${LOOP}p3 ${LOOP}3; do [ -b \$c ] && echo \$c && break; done")
[ -n "$ESP_PART" ] && [ -n "$BOOT_PART" ] && [ -n "$LUKS_PART" ] \
    || die "could not identify ESP/boot/LUKS partitions on $LOOP"

echo
echo "── whether the removable-media fallback the firmware needs is there ───"
# A real machine gets an NVRAM Boot#### entry pointing straight at
# \EFI\fedora\shimx64.efi, added by bootupd via efibootmgr. This install
# deliberately skipped that step (--generic-image, so this machine's own
# firmware is never touched), and this boot's varstore is pristine — no
# Boot#### entries at all. So the ONLY path OVMF can find this loader by is
# the UEFI-mandated removable-media fallback, \EFI\BOOT\BOOTX64.EFI. This is
# CHECKED, not assumed: if it is absent, the boot below is expected to fail at
# firmware and that must not be misread as a LUKS or dracut defect.
mkdir -p "$BOOTMNT"
FALLBACK_PRESENT=0

# WHO ELSE HAS THIS PARTITION MOUNTED, asked before mounting it and reported
# either way.
#
# Round 38's run failed here with the kernel's `loop1p1: Can't mount, would
# change RO state`, and the check it guards never ran at all. That message has
# exactly one meaning: get_tree_bdev() found a LIVE superblock for this block
# device whose SB_RDONLY flag differs from the one being asked for — so the ESP
# was still mounted READ-WRITE somewhere at that moment. Mount namespaces do
# not enter into it; a superblock is global to the kernel.
#
# That is worse than a check that did not run, which is why this reports rather
# than retries. rime-install mounts the ESP at $TROOT/boot/efi and unmounts it
# with `umount -R … 2>/dev/null || true` — a failure there is silent by
# construction. Twenty lines below, this file detaches the loop device and
# hands the IMAGE FILE to a qemu in a different container, on the stated
# grounds that a boot must see what an independent reader of the file would
# see. A live rw vfat mount on the host breaks exactly that: the loop detach
# fails too (also `2>/dev/null`), and the guest reads bytes the host's page
# cache has not finished writing.
ESP_HOLDERS="$(sudo -n findmnt -n -o TARGET,SOURCE,OPTIONS -S "$ESP_PART" 2>/dev/null || true)"
if [ -n "$ESP_HOLDERS" ]; then
    bad "nothing still holds the target ESP mounted before it is handed on" "$(printf '%s' "$ESP_HOLDERS" | tr '\n' ';')"
    printf '%s\n' "$ESP_HOLDERS" | sed 's/^/    holder: /'
    sudo -n fuser -vm "$ESP_PART" 2>&1 | sed 's/^/    fuser: /' || true
    # Only OUR OWN leftovers are cleared, and only so the check below can still
    # answer its question. A mount somewhere else on this machine is reported
    # and left alone: this suite does not get to unmount things it did not
    # create.
    printf '%s\n' "$ESP_HOLDERS" | awk '{print $1}' | while read -r t; do
        case "$t" in
            "$WORK"/*) sudo -n umount -R "$t" 2>&1 | sed 's/^/    umount: /' || true ;;
            *) echo "    holder $t is not under $WORK — left alone" ;;
        esac
    done
else
    ok "nothing still holds the target ESP mounted before it is handed on"
fi

# The mount's own stderr is KEPT, not sent to /dev/null: "would change RO
# state", "wrong fs type" and "no such device" are three different defects and
# the first of them cost a round to diagnose from a one-line summary.
if sudo -n mount -o ro "$ESP_PART" "$BOOTMNT" 2>"$WORK/esp-mount.err"; then
    if sudo -n test -f "$BOOTMNT/EFI/BOOT/BOOTX64.EFI"; then
        FALLBACK_PRESENT=1
        ok "the ESP carries the removable-media fallback loader" "EFI/BOOT/BOOTX64.EFI"
    else
        bad "the ESP carries the removable-media fallback loader" "no EFI/BOOT/BOOTX64.EFI — OVMF has no Boot#### entry and no fallback to find; this boot will not reach the firmware's next stage"
        sudo -n find "$BOOTMNT/EFI" -maxdepth 2 2>/dev/null | sed 's/^/    esp: /'
    fi
    sudo -n umount "$BOOTMNT"
else
    bad "the ESP mounts so the fallback loader can be checked" "mount of $ESP_PART failed: $(tr '\n' ' ' < "$WORK/esp-mount.err" 2>/dev/null)"
    sed 's/^/    mount: /' "$WORK/esp-mount.err" 2>/dev/null || true
fi

# Detach the loop device before handing the file to a qemu process in a
# DIFFERENT container: nothing needs it open any more, and a boot must see
# exactly what a completely independent reader of this file would see, not
# whatever this shell's loop-device handle happens to be caching.
sudo -n losetup -d "$LOOP" 2>/dev/null
LOOP=""

echo
echo "── building the rime-bootlab image if it is not already there ─────────"
if ! sudo -n podman image exists "$LAB" 2>/dev/null; then
    echo "note: building $LAB — qemu/OVMF are build-time tooling, deliberately not on Rime machines"
    # shellcheck disable=SC2024
    sudo -n podman build -t "$LAB" -f "$REPO/bootlab/Containerfile" "$REPO" >"$WORK/bootlab-build.log" 2>&1 \
        || die "could not build $LAB (see $WORK/bootlab-build.log)"
fi

echo
echo "── the boot: OVMF, shim, GRUB, the shipped kernel, dracut, the prompt ──"
install -m 0644 ./luks-boot-drive.py "$WORK/luks-boot-drive.py" \
    || die "luks-boot-drive.py is missing"
sudo -n chmod 644 "$IMG"

# The firmware pair, converted from the qcow2 Fedora ships to the raw format
# qemu's pflash wants. OVMF_VARS_4M.qcow2 is PRISTINE on this lab image (no
# PK/db enrolled — see luks-boot-drive.py's header for why that is the right
# choice here, and why it means UEFI Setup Mode rather than Secure Boot
# enforcing). Done inside the container: edk2-ovmf lives there, never on an
# Rime host, by AGENTS.md "Touching a machine's boot path".
BOOTCMD='set -e
qemu-img convert -O raw /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 /w/OVMF_CODE.fd
qemu-img convert -O raw /usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2 /w/OVMF_VARS_template.fd
python3 /w/luks-boot-drive.py --work /w --name luksboot \
    --code /w/OVMF_CODE.fd --vars-template /w/OVMF_VARS_template.fd \
    --disk /w/target.img --passphrase "'"$PASSPHRASE"'" --timeout 300'
BOOTOUT="$WORK/boot-drive-stdout.txt"
# shellcheck disable=SC2024
sudo -n podman run --rm --device /dev/kvm -v "$WORK":/w:z -w /w "$LAB" -c "$BOOTCMD" \
    > "$BOOTOUT" 2>&1
bootrc=$?
cat "$BOOTOUT" | sed 's/^/    /'
echo "boot driver exit=$bootrc"

verdict=$(grep '^verdict=' "$BOOTOUT" | tail -1 | cut -d= -f2-)
prompt_seen=$(grep '^prompt-seen=' "$BOOTOUT" | tail -1 | cut -d= -f2-)
typed=$(grep '^typed=' "$BOOTOUT" | tail -1 | cut -d= -f2-)
switched=$(grep '^switched-root=' "$BOOTOUT" | tail -1 | cut -d= -f2-)

if [ "$prompt_seen" = yes ]; then ok "the guest reached the passphrase prompt" "verdict=$verdict"
else bad "the guest reached the passphrase prompt" "verdict=$verdict (fallback-present=$FALLBACK_PRESENT) — see $WORK/serial-luksboot.log"; fi
if [ "$typed" = yes ]; then ok "the passphrase was typed on the emulated keyboard"
else bad "the passphrase was typed on the emulated keyboard" "verdict=$verdict"; fi
if [ "$switched" = yes ]; then
    ok "dracut switched root — the disk this installer wrote BOOTS" "first time this has been measured"
else
    bad "dracut switched root — the disk this installer wrote BOOTS" "verdict=$verdict — see $WORK/serial-luksboot.log"
fi

# `welcome-seen` IS NOT A CRITERION OF THIS SUITE, decided here rather than left
# as an unexplained field with no verdict beside it.
#
# This suite's question is the one in its name: does the encrypted disk this
# installer wrote BOOT. That question is fully answered by the three assertions
# above — the prompt was drawn, the passphrase was accepted, and dracut handed
# off to the real root. Everything after `Switching root` belongs to the
# INSTALLED SYSTEM, not to the installer: first-boot welcome is rime-shell's,
# it needs a graphical session this headless serial guest never starts, and
# gating a LUKS boot test on it would make a shell regression read as an
# encryption defect. The suite that owns first-boot welcome should assert it.
#
# It is still REPORTED, because the boot driver measured it and a measurement
# that is taken and then hidden is how "nobody ever checked" becomes "somebody
# checked and it was fine".
welcome=$(grep '^welcome-seen=' "$BOOTOUT" | tail -1 | cut -d= -f2-)
echo "note: welcome-seen=${welcome:-<not reported>} — informational; first-boot welcome is not a criterion of this suite (see the comment above)"

echo
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
