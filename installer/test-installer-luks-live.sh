#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-luks-live.sh — a REAL `bootc install` onto a REAL LUKS2
#  volume, and then every claim the installer makes about it, checked against
#  the disk rather than against the source.
#
#  WHY THIS EXISTS. The encrypted path creates a partition table, a LUKS2
#  header, two keyslots, a plain /boot, a btrfs root inside the volume, a
#  crypttab, and four kernel arguments — and every one of those is a thing that
#  can be subtly wrong in a way no amount of reading the script reveals. This
#  repository has repeatedly found that a scenario which cannot fail proves
#  nothing. So this suite does the install for real and then opens the disk two
#  different ways to prove the owner could.
#
#  WHAT IT REFUSES TO DO. It installs onto a LOOPBACK FILE and nothing else:
#  the target must be a /dev/loop* node that this script created itself, and it
#  exits before touching anything if it is not. No real disk is ever named. It
#  also snapshots this machine's UEFI boot entries before and after and fails
#  if they moved, because `bootc install` is capable of adding one and the host
#  running this suite is somebody's working computer.
#
#  WHAT IT NEEDS. Passwordless root, podman, ~20 GB of free disk, and an
#  Rime OS image in ROOT podman storage (localhost/rime-os:daily, or
#  RIME_LIVE_IMAGE=...). A GitHub runner has none of those, which is why this
#  suite is listed in tests/suites-not-in-ci.txt and why its fast half —
#  test-installer-luks.sh, the refusals and the keymap conversion — is a
#  separate file that CI does run.
#
#  THE ENROLMENT HELPER. /usr/libexec/rime-luks-enroll belongs to the boot-v2
#  work and is not in the image yet, so this suite supplies a stand-in through
#  the engine's RIME_LUKS_ENROLL_LOCAL test hook. The stand-in does exactly the
#  part of the contract the installer depends on — `systemd-cryptenroll
#  --recovery-key`, writing the key to --recovery-out — and nothing else. When
#  the real helper lands, delete the stub and point the hook at it; every
#  assertion below is about the installer's half of the contract and none of
#  them cares which implementation produced the key.
#
#  PASS = the install completes, BOTH keys open the volume, /boot is readable
#  with no key at all, the kernel arguments and crypttab name the right UUIDs,
#  the console keymap is the one that was chosen, the recovery key reached a
#  file the user can read and did NOT reach the log — and every one of those
#  assertions fails when run against a deliberately broken copy of what it
#  inspected.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 1

ENGINE=./rime-install
IMAGE="${RIME_LIVE_IMAGE:-localhost/rime-os:daily}"
# `bg` on purpose: it is one of the 36 XKB layout names that is NOT a loadable
# console keymap, so an install that ends up with KEYMAP=bg_bds-utf8 proves the
# conversion ran, and one that ends up with `bg` or `us` proves it did not.
KEYMAP_XKB="bg"
KEYMAP_CONSOLE=bg_bds-utf8
PASSPHRASE='correct horse battery 9'
DISK_SIZE=24G

pass=0; fail=0
ok()  { printf 'PASS  %-52s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-52s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
die() { printf 'FATAL: %s\n' "$1" >&2; exit 2; }

sudo -n true 2>/dev/null || die "needs passwordless root (sudo -n)."
command -v podman    >/dev/null || die "podman is not installed."
command -v losetup   >/dev/null || die "losetup is not installed."

# ── the engine runs inside tests/lab/nvram-guard, and this is not optional ──
#
# This suite points a `--privileged --pid=host` container at a loopback file on
# a developer's own machine. That is the exact shape of the run that deleted a
# laptop's `Rime OS` boot entry on 2026-09-20 and left it unbootable until it
# was repaired from a live USB (BOOT-BREAKAGE-2026-09-20.md).
#
# There are now two independent layers and this suite uses both:
#
#   1. PREVENTION, in the engine. `rime-install` passes bootc
#      `--generic-image` whenever the install target is loop-backed, which
#      skips the firmware step while still installing every bootloader type,
#      and additionally masks /sys/firmware/efi/efivars in the container.
#      The FLAG is the guard; the mask is defence in depth. The first version
#      of this shipped the mask alone and a run on 2026-09-20 at 22:12 proved
#      it insufficient: bootc takes --pid=host and re-enters the host mount
#      namespace for the bootloader step. Measured by test-installer-luks.sh,
#      which also fails if a new privileged call site appears unguarded.
#   2. DETECTION, here. Every byte of the host's `Boot*` variables is hashed
#      before and after the engine runs, by the same guard the bootc lab uses.
#      If layer 1 is ever wrong, this says so when the command returns instead
#      of at the next power-on.
#
# A missing guard is a hard stop. Running this suite without layer 2 because
# the file moved is how the incident happened in the first place: a discipline
# followed most of the time.
NVGUARD="../tests/lab/nvram-guard"
# The installer's live-environment image the engine runs inside (see
# live-env-engine), built from Containerfile.installer.
LIVE_ENV_IMAGE="${RIME_LIVE_ENV_IMAGE:-localhost/rime-installer:latest}"
[ -x "$NVGUARD" ] || die "$NVGUARD is missing or not executable.
This suite will not run a privileged loopback install without it — see
BOOT-BREAKAGE-2026-09-20.md and AGENTS.md \"Touching a machine's boot path\"."
command -v cryptsetup>/dev/null || die "cryptsetup is not installed."
sudo -n podman image exists "$IMAGE" 2>/dev/null \
    || die "$IMAGE is not in ROOT podman storage. Build it, or set RIME_LIVE_IMAGE."

# /var/lab-scratch, not /tmp and not /var/tmp. /tmp on the build machine is a
# tmpfs — a 30 GB disk image there is 30 GB of RAM, and it took the whole
# machine's shell down once already. RIME_LUKS_SCRATCH overrides it.
SCRATCH_ROOT="${RIME_LUKS_SCRATCH:-/var/lab-scratch}"
mkdir -p "$SCRATCH_ROOT" 2>/dev/null
[ -d "$SCRATCH_ROOT" ] && [ -w "$SCRATCH_ROOT" ] || SCRATCH_ROOT=/var/tmp
case "$(df -PT "$SCRATCH_ROOT" 2>/dev/null | awk 'NR==2{print $2}')" in
  tmpfs|ramfs) die "$SCRATCH_ROOT is a RAM filesystem; a 30 GB image there would eat the machine. Set RIME_LUKS_SCRATCH to somewhere on a real disk." ;;
esac
WORK=$(mktemp -d "$SCRATCH_ROOT/rime-luks-live.XXXXXX") || die "no scratch directory"
chmod 755 "$WORK"
IMG="$WORK/target.img"
LOOP=""
MAPPER=""
MNT="$WORK/mnt"
BOOTMNT="$WORK/bootmnt"

cleanup() {
    sudo -n umount "$MNT/boot/efi" 2>/dev/null
    sudo -n umount "$MNT/boot"     2>/dev/null
    sudo -n umount "$MNT"          2>/dev/null
    sudo -n umount "$BOOTMNT"      2>/dev/null
    [ -n "$MAPPER" ] && sudo -n cryptsetup close "$MAPPER" 2>/dev/null
    # $MAPPER is the mapper THIS SUITE opened. The ENGINE opens one too, named
    # luks-<uuid>, and when the run dies before the suite has read that uuid —
    # which is exactly what happened when nvram-guard aborted run 5 — nothing
    # closed it, the dm device kept the loop device open, and `losetup -d`
    # failed silently for hours afterwards. So the holders are asked instead of
    # remembered: anything mapped on top of a partition of OUR loop device, and
    # nothing else.
    if [ -n "$LOOP" ]; then
        for _h in /sys/class/block/"$(basename "$LOOP")"*/holders/*; do
            [ -e "$_h" ] || continue
            _dm=$(cat "$_h/dm/name" 2>/dev/null) || continue
            [ -n "$_dm" ] && sudo -n cryptsetup close "$_dm" 2>/dev/null
        done
    fi
    [ -n "$LOOP" ] && sudo -n losetup -d "$LOOP" 2>/dev/null
    # Keep the artefacts when anything failed: a suite that deletes the engine
    # output it just told you to read is a suite you cannot act on.
    # The 24 GB disk image ALWAYS goes, pass or fail: keeping it once filled a
    # filesystem and took the machine's shell with it. The small artefacts —
    # the engine's output and its log — are what a failure needs, and they are
    # kept whenever anything failed.
    sudo -n rm -f "$IMG" 2>/dev/null
    if [ "${fail:-1}" = 0 ] && [ "${RIME_LUKS_KEEP:-0}" != 1 ]; then
        sudo -n rm -rf "$WORK" 2>/dev/null
    else
        printf 'artefacts kept in %s (engine output: %s)\n' "$WORK" "$WORK/engine-stdout.txt" >&2
    fi
    return 0
}
trap cleanup EXIT

# ── the machine's own firmware must come out of this untouched ──────────────
EFI_BEFORE="$WORK/efi-before.txt"
EFI_AFTER="$WORK/efi-after.txt"
# The redirect is the CALLER's, on purpose: this file belongs to the user
# running the suite so the diff at the end can read it without sudo.
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

# ── the stand-in enrolment helper ───────────────────────────────────────────
HELPER="$WORK/rime-luks-enroll"
cat > "$HELPER" <<'STUB'
#!/usr/bin/env bash
# Stand-in for /usr/libexec/rime-luks-enroll: the recovery-key half of the
# contract, and nothing else. No TPM: a loopback file has none, and TPM policy
# is the enrolment agent's subject, not the installer's.
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
# $PASSWORD is how the installer hands over the existing passphrase, and is
# systemd-cryptenroll's own documented variable.
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
  printf 'hostname=rimeluks\n'
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
LOGCOPY="$WORK/engine-log.txt"
echo "── running the installer (this is the slow part) ──────────────────────"
start=$(date +%s)
# The engine's stdout is captured by THIS shell, not by sudo: the file must be
# readable afterwards without root, and the engine writes nothing to it that
# needs privilege.
# shellcheck disable=SC2024
sudo -n "$NVGUARD" --label "luks-live-install" --out "$WORK/nvram" -- \
  ./live-env-engine \
    RIME_LIVE_ENV_IMAGE="$LIVE_ENV_IMAGE" \
    RIME_IMAGE="$IMAGE" \
    RIME_LUKS_ENROLL_LOCAL="$HELPER" \
    RIME_RECOVERY_DIR="$RECOVERY_DIR" \
    RIME_LUKS_PBKDF_MEMORY=65536 \
    -- "$ENGINE" --headless "$ANS" > "$OUT" 2>&1 </dev/null
rc=$?
echo "engine exit=$rc after $(( $(date +%s) - start ))s"
sudo -n cp /var/log/rime-install.log "$LOGCOPY" 2>/dev/null || : > "$LOGCOPY"
sudo -n chmod 644 "$LOGCOPY" 2>/dev/null

echo
echo "── the install itself ─────────────────────────────────────────────────"
if [ "$rc" = 0 ]; then ok "engine exit status" "0"
else bad "engine exit status" "$rc — see $OUT"; fi
# The ENGINE's last word, not the file's. tests/lab/nvram-guard now wraps the
# engine and writes its verdict to stderr, which lands in the same capture, so
# a plain `tail -1` reads the guard's line and reports a clean install as a
# protocol failure. The guard's lines are prefixed and are dropped here.
lastproto=$(grep -v 'nvram-guard\[' "$OUT" | grep -v '^[[:space:]]*$' | tail -1)
if [[ "$lastproto" == "RIME-INSTALL-OK" ]]; then ok "final protocol line is RIME-INSTALL-OK"
else bad "final protocol line is RIME-INSTALL-OK" "got: $lastproto"; fi
if grep -q 'Unexpected error on line' "$OUT"; then bad "the ERR trap did not fire" "it did"
else ok "the ERR trap did not fire"; fi

# Everything below needs the disk to exist; stop early and loudly rather than
# reporting twenty cascading failures about a disk that was never written.
if [ "$rc" != 0 ]; then
    echo
    echo "── the install failed; the remaining assertions cannot run ────────────"
    tail -40 "$OUT"
    printf '\n%s passed, %s failed\n' "$pass" "$((fail + 1))"
    exit 1
fi

ESP_PART=$(sudo -n bash -c "for c in ${LOOP}p1 ${LOOP}1; do [ -b \$c ] && echo \$c && break; done")
BOOT_PART=$(sudo -n bash -c "for c in ${LOOP}p2 ${LOOP}2; do [ -b \$c ] && echo \$c && break; done")
LUKS_PART=$(sudo -n bash -c "for c in ${LOOP}p3 ${LOOP}3; do [ -b \$c ] && echo \$c && break; done")

echo
echo "── the layout ─────────────────────────────────────────────────────────"
nparts=$(sudo -n sgdisk -p "$LOOP" 2>/dev/null | awk '/^ +[0-9]+ /{n++} END{print n+0}')
# FOUR: ESP, /boot, the LUKS volume, and a 1 MiB BIOS boot partition. The last
# is what lets bootupd install the i386-pc GRUB component, which it does on any
# loopback install because that path passes bootc --generic-image to keep the
# building machine's NVRAM out of it. Without it, `grub2-install` refuses to
# use blocklists and the install fails with the volume already encrypted.
if [ "$nparts" = 4 ]; then ok "four partitions"; else bad "four partitions" "got $nparts"; fi
t4=$(sudo -n bash -c "for c in ${LOOP}p4 ${LOOP}4; do [ -b \$c ] && lsblk -dno PARTTYPE \$c && break; done" 2>/dev/null | tr 'A-Z' 'a-z')
if [ "$t4" = "21686148-6449-6e6f-744e-656564454649" ]; then ok "p4 is a BIOS boot partition"
else bad "p4 is a BIOS boot partition" "type $t4"; fi
t1=$(sudo -n lsblk -dno PARTTYPE "$ESP_PART" 2>/dev/null | tr 'A-Z' 'a-z')
t3=$(sudo -n lsblk -dno PARTTYPE "$LUKS_PART" 2>/dev/null | tr 'A-Z' 'a-z')
if [ "$t1" = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" ]; then ok "p1 is an EFI System Partition"
else bad "p1 is an EFI System Partition" "type $t1"; fi
if [ "$t3" = "ca7d7ccb-63ed-4c53-861c-1742536059cc" ]; then ok "p3 is typed Linux LUKS, not 'root'"
else bad "p3 is typed Linux LUKS, not 'root'" "type $t3"; fi

# ── /boot must be readable with NO key at all. That is the whole reason it is
#    a separate partition, and a /boot that turned out to be inside the volume
#    would be a machine GRUB cannot boot.
bootfs=$(sudo -n blkid -s TYPE -o value "$BOOT_PART" 2>/dev/null)
if [ "$bootfs" = ext4 ]; then ok "/boot is a plain ext4 partition"; else bad "/boot is a plain ext4 partition" "type '$bootfs'"; fi
mkdir -p "$BOOTMNT"
if sudo -n mount -o ro "$BOOT_PART" "$BOOTMNT" 2>/dev/null; then
    ok "/boot mounts with no key"
    if sudo -n test -d "$BOOTMNT/loader/entries"; then ok "/boot carries the bootloader entries"
    else bad "/boot carries the bootloader entries" "no loader/entries"; fi
    if [ -n "$(sudo -n find "$BOOTMNT" -maxdepth 3 -name 'vmlinuz*' -print -quit 2>/dev/null)" ]; then
        ok "/boot carries a kernel"
    else bad "/boot carries a kernel" "none found"; fi
    sudo -n cp -r "$BOOTMNT/loader/entries" "$WORK/entries" 2>/dev/null
    sudo -n chmod -R a+rX "$WORK/entries" 2>/dev/null
    sudo -n umount "$BOOTMNT"
else
    bad "/boot mounts with no key" "mount failed"
fi

# ── the ESP credential: the keymap channel that survives a signed UKI ───────
# The kernel argument asserted further down is what this machine boots with
# today, under GRUB. It cannot survive the systemd-boot + UKI pivot, because
# there the command line lives inside the signed PE image. systemd-boot reads
# \loader\credentials\*.cred off the ESP and hands them to the initrd as
# system credentials, so the installer writes the keymap there as well, on
# every encrypted install, and a machine installed today carries its layout
# into a UKI world with nothing to migrate. GRUB ignores the file entirely.
#
# What it CANNOT show is that the credential is then honoured — a .cred is
# inert without files/dracut/rime-unlock-hint's rime-vconsole-credential, which
# is measured by booting a guest in installer/test-installer-keymap-boot.sh.
# This assertion is only that the installer wrote the file with the right name
# and the right content.
if sudo -n mount -o ro "$ESP_PART" "$BOOTMNT" 2>/dev/null; then
    CREDF="$BOOTMNT/loader/credentials/vconsole.keymap.cred"
    if sudo -n test -f "$CREDF"; then
        credval=$(sudo -n cat "$CREDF" 2>/dev/null | tr -d '\r\n')
        if [ "$credval" = "$KEYMAP_CONSOLE" ]; then
            ok "the ESP carries the vconsole.keymap credential" "$credval"
        else
            bad "the ESP carries the vconsole.keymap credential" "content '$credval', expected '$KEYMAP_CONSOLE'"
        fi
    else
        bad "the ESP carries the vconsole.keymap credential" "no loader/credentials/vconsole.keymap.cred"
    fi
    sudo -n umount "$BOOTMNT"
else
    bad "the ESP mounts so the credential can be read" "mount of $ESP_PART failed"
fi

echo
echo "── the LUKS2 volume, and both ways into it ────────────────────────────"
if sudo -n cryptsetup isLuks --type luks2 "$LUKS_PART" 2>/dev/null; then ok "p3 carries a LUKS2 header"
else bad "p3 carries a LUKS2 header" "cryptsetup says no"; fi
LUKS_UUID=$(sudo -n cryptsetup luksUUID "$LUKS_PART" 2>/dev/null)
if [ -n "$LUKS_UUID" ]; then ok "the header has a UUID" "$LUKS_UUID"
else bad "the header has a UUID"; fi

RECOVERY_FILE=$(find "$RECOVERY_DIR" -type f -name 'rime-recovery-key-*.txt' -print -quit 2>/dev/null)
RECOVERY_KEY=""
if [ -n "$RECOVERY_FILE" ]; then
    ok "a recovery-key file was written where the user can find it" "$(basename "$RECOVERY_FILE")"
    mode=$(stat -c %a "$RECOVERY_FILE" 2>/dev/null)
    if [ "$mode" = 600 ]; then ok "the recovery-key file is 0600"
    else bad "the recovery-key file is 0600" "mode $mode"; fi
    # The mode is a number; this is the behaviour. An unprivileged reader must
    # not be able to open it at all.
    if head -c1 "$RECOVERY_FILE" >/dev/null 2>&1; then
        bad "an unprivileged user cannot read the recovery key" "this one could"
    else
        ok "an unprivileged user cannot read the recovery key"
    fi
    # `sudo` to read it, because the engine writes it 0600 root -- which is the
    # point. A suite that could read this file without privilege would be
    # asserting the opposite of what it means to.
    RECOVERY_KEY=$(sudo -n grep -oE '[a-z]{8}(-[a-z]{8})+' "$RECOVERY_FILE" | head -1)
else
    bad "a recovery-key file was written where the user can find it" "nothing in $RECOVERY_DIR"
fi
SHOWN_KEY=$(grep -m1 '^RIME-INSTALL-RECOVERY-KEY: ' "$OUT" | sed 's/^RIME-INSTALL-RECOVERY-KEY: //')
if [ -n "$SHOWN_KEY" ]; then ok "the key was put on screen for the user"
else bad "the key was put on screen for the user" "no protocol line in the engine's output"; fi
if [ -n "$SHOWN_KEY" ] && [ "$SHOWN_KEY" = "$RECOVERY_KEY" ]; then
    ok "the key on screen and the key in the file are the same key"
else
    bad "the key on screen and the key in the file are the same key" "screen='$SHOWN_KEY' file='$RECOVERY_KEY'"
fi
# SECRET HYGIENE. The install log is copied onto the USB stick in the clear on
# every failure, so a recovery key in it is a key nobody chose to write down.
if [ -n "$RECOVERY_KEY" ] && grep -qF "$RECOVERY_KEY" "$LOGCOPY"; then
    bad "the recovery key is NOT in the install log" "it is"
else
    ok "the recovery key is NOT in the install log"
fi
# And the passphrase must not be either.
if grep -qF "$PASSPHRASE" "$LOGCOPY"; then bad "the disk passphrase is NOT in the install log" "it is"
else ok "the disk passphrase is NOT in the install log"; fi

# THE ACCEPTANCE CRITERION, twice.
if printf '%s' "$PASSPHRASE" | sudo -n cryptsetup open --test-passphrase --key-file - "$LUKS_PART" 2>/dev/null; then
    ok "the passphrase opens the volume"
else bad "the passphrase opens the volume"; fi
if [ -n "$RECOVERY_KEY" ] && printf '%s' "$RECOVERY_KEY" | sudo -n cryptsetup open --test-passphrase --key-file - "$LUKS_PART" 2>/dev/null; then
    ok "the recovery key opens the volume"
else bad "the recovery key opens the volume"; fi
# A wrong key must NOT open it — without this, an assertion that everything
# opens the volume would pass on a volume with no encryption at all.
if printf '%s' "definitely not the passphrase" | sudo -n cryptsetup open --test-passphrase --key-file - "$LUKS_PART" 2>/dev/null; then
    bad "a wrong passphrase is refused" "it was accepted"
else ok "a wrong passphrase is refused"; fi

echo
echo "── what is inside, once it is open ────────────────────────────────────"
MAPPER="rimeluks-test-$$"
if printf '%s' "$PASSPHRASE" | sudo -n cryptsetup open --key-file - "$LUKS_PART" "$MAPPER" 2>/dev/null; then
    ok "the volume opens for real, not just --test-passphrase"
else
    bad "the volume opens for real, not just --test-passphrase"
    MAPPER=""
fi
DEPLOY=""
ROOT_UUID=""
if [ -n "$MAPPER" ]; then
    rootfs=$(sudo -n blkid -s TYPE -o value "/dev/mapper/$MAPPER" 2>/dev/null)
    ROOT_UUID=$(sudo -n blkid -s UUID -o value "/dev/mapper/$MAPPER" 2>/dev/null)
    if [ "$rootfs" = btrfs ]; then ok "the root filesystem inside is btrfs"
    else bad "the root filesystem inside is btrfs" "type '$rootfs'"; fi
    mkdir -p "$MNT"
    if sudo -n mount "/dev/mapper/$MAPPER" "$MNT" 2>/dev/null; then
        DEPLOY=$(sudo -n bash -c "ls -d $MNT/ostree/deploy/*/deploy/*.0 2>/dev/null | head -1")
        if [ -n "$DEPLOY" ]; then ok "an ostree deployment is on the encrypted root"
        else bad "an ostree deployment is on the encrypted root" "nothing under ostree/deploy"; fi
    else
        bad "the encrypted root mounts"
    fi
fi

if [ -n "$DEPLOY" ]; then
    sudo -n cp "$DEPLOY/etc/crypttab"      "$WORK/crypttab"      2>/dev/null
    sudo -n cp "$DEPLOY/etc/vconsole.conf" "$WORK/vconsole.conf" 2>/dev/null
    sudo -n cp "$DEPLOY/etc/passwd"        "$WORK/passwd"        2>/dev/null
    sudo -n chmod 644 "$WORK/crypttab" "$WORK/vconsole.conf" "$WORK/passwd" 2>/dev/null
fi

echo
echo "── the installed system knows it is encrypted ─────────────────────────"
assert_crypttab() {  # $1 = a crypttab file
    [ -f "$1" ] || return 1
    grep -q "UUID=$LUKS_UUID" "$1" && grep -q 'x-initrd.attach' "$1" && grep -q "luks-$LUKS_UUID" "$1"
}
if assert_crypttab "$WORK/crypttab"; then ok "/etc/crypttab names the volume, the mapper and x-initrd.attach"
else bad "/etc/crypttab names the volume, the mapper and x-initrd.attach" "$(cat "$WORK/crypttab" 2>/dev/null | tr '\n' ' ')"; fi
# Mutation: the same assertion against a copy with the UUID changed must FAIL.
if [ -f "$WORK/crypttab" ]; then
    sed 's/UUID=[0-9a-f-]*/UUID=00000000-0000-0000-0000-000000000000/' "$WORK/crypttab" > "$WORK/crypttab.mut"
    if assert_crypttab "$WORK/crypttab.mut"; then bad "mutant: a wrong UUID in crypttab is caught" "it passed"
    else ok "mutant: a wrong UUID in crypttab is caught"; fi
fi

echo
echo "── the keyboard the unlock prompt will use ────────────────────────────"
assert_vconsole() { [ -f "$1" ] && grep -q "^KEYMAP=$KEYMAP_CONSOLE\$" "$1"; }
if assert_vconsole "$WORK/vconsole.conf"; then ok "/etc/vconsole.conf carries the CONSOLE keymap" "$KEYMAP_CONSOLE"
else bad "/etc/vconsole.conf carries the CONSOLE keymap" "$(grep '^KEYMAP' "$WORK/vconsole.conf" 2>/dev/null)"; fi
if [ -f "$WORK/vconsole.conf" ]; then
    sed "s/^KEYMAP=.*/KEYMAP=$KEYMAP_XKB/" "$WORK/vconsole.conf" > "$WORK/vconsole.mut"
    if assert_vconsole "$WORK/vconsole.mut"; then bad "mutant: the raw XKB name is caught" "it passed"
    else ok "mutant: the raw XKB name is caught"; fi
fi

echo
echo "── the kernel arguments, which are the only thing the initramfs reads ──"
ENTRY=$(find "$WORK/entries" -name '*.conf' -print -quit 2>/dev/null)
assert_kargs() {  # $1 = a BLS entry file
    local f="$1" o
    [ -f "$f" ] || return 1
    o=$(grep -m1 '^options ' "$f") || return 1
    [[ "$o" == *"rd.luks.uuid=$LUKS_UUID"* ]] || return 1
    [[ "$o" == *"rd.luks.name=$LUKS_UUID=luks-$LUKS_UUID"* ]] || return 1
    [[ "$o" == *"vconsole.keymap=$KEYMAP_CONSOLE"* ]] || return 1
    [[ "$o" == *"root=UUID=$ROOT_UUID"* ]] || return 1
    return 0
}
if [ -n "$ENTRY" ]; then
    if assert_kargs "$ENTRY"; then ok "the boot entry unlocks by UUID, names the mapper, sets the keymap and the root"
    else bad "the boot entry unlocks by UUID, names the mapper, sets the keymap and the root" \
             "$(grep -m1 '^options ' "$ENTRY" 2>/dev/null)"; fi
    # Four mutations, one per clause, so no clause can be silently absent from
    # the assertion. Each must make it fail.
    for m in "rd.luks.uuid=$LUKS_UUID" "rd.luks.name=$LUKS_UUID=luks-$LUKS_UUID" \
             "vconsole.keymap=$KEYMAP_CONSOLE" "root=UUID=$ROOT_UUID"; do
        # GLOBAL, and it took a live run to find out why. The engine writes
        # BOTH `rd.vconsole.keymap=X` and `vconsole.keymap=X`, and the first
        # of those CONTAINS the second as a substring. Without /g, sed removed
        # only the occurrence inside `rd.vconsole.keymap=` and the assertion
        # still found the plain one — so the mutant passed, which is this
        # suite's word for "the case proves nothing".
        sed "s|$m|XX-removed-XX|g" "$ENTRY" > "$WORK/entry.mut"
        if assert_kargs "$WORK/entry.mut"; then
            bad "mutant: kernel argument '${m%%=*}' is actually checked" "it passed without it"
        else
            ok "mutant: kernel argument '${m%%=*}' is actually checked"
        fi
    done
else
    bad "a bootloader entry exists" "none found under /boot/loader/entries"
fi

echo
echo "── the account, and the machine's own firmware ────────────────────────"
if grep -q '^tester:' "$WORK/passwd" 2>/dev/null; then ok "the account was created inside the encrypted root"
else bad "the account was created inside the encrypted root"; fi
# shellcheck disable=SC2024
sudo -n efibootmgr -v 2>/dev/null > "$EFI_AFTER" || echo "(no efibootmgr)" > "$EFI_AFTER"
if diff -q "$EFI_BEFORE" "$EFI_AFTER" >/dev/null 2>&1; then
    ok "this machine's UEFI boot entries are unchanged"
else
    bad "this machine's UEFI boot entries are unchanged" "SEE $EFI_BEFORE vs $EFI_AFTER — DO NOT REBOOT UNTIL CHECKED"
    diff "$EFI_BEFORE" "$EFI_AFTER" | head -20
fi
# The guard's own verdict, which reads efivarfs itself rather than efibootmgr's
# rendering of it. Two sources; a change either one sees is a failure. And a
# verdict that is neither "verified" nor an explained could-not-run is a
# failure too: a guard whose output cannot be found proves nothing.
# And the engine's own decision, plus the absence of the tool that does the
# damage. MEASURED 2026-09-20: the tmpfs mask ALONE did not stop this — bootc
# runs with --pid=host and re-enters the host mount namespace for the
# bootloader step, so `efibootmgr --create --disk /dev/loop1` still reached
# this machine's NVRAM and the guard below is what caught it. The prevention
# is `bootc install --generic-image`, which skips the firmware step, and these
# two assertions are what say it is actually in effect.
if grep -q -- '--generic-image' "$LOGCOPY" 2>/dev/null \
   || grep -q 'loop-backed' "$LOGCOPY" 2>/dev/null; then
    ok "the engine took the loopback path for the firmware" "$(grep -m1 '^..:..:.. nvram:' "$LOGCOPY" 2>/dev/null | sed 's/^[0-9:]* //')"
else
    bad "the engine took the loopback path for the firmware" "no nvram decision in the engine log"
fi
if grep -q 'Executing: "efibootmgr"' "$OUT" "$LOGCOPY" 2>/dev/null; then
    bad "bootupd never ran efibootmgr" "it did — see the log; the firmware step was NOT skipped"
else
    ok "bootupd never ran efibootmgr" "the firmware step was skipped"
fi
nvverdict=$(sed -n 's/^.*nvram-guard\[[^]]*\]: verdict: \([a-z-]*\).*/\1/p' "$OUT" 2>/dev/null | tail -1)
case "$nvverdict" in
    verified)       ok "nvram-guard measured the run" "verdict: verified" ;;
    could-not-run)  ok "nvram-guard measured the run" "verdict: could-not-run (no UEFI on this host)" ;;
    "")             bad "nvram-guard measured the run" "no verdict in $OUT — the guard did not report" ;;
    *)              bad "nvram-guard measured the run" "verdict: $nvverdict — DO NOT REBOOT UNTIL CHECKED" ;;
esac

echo
echo "──────────────────────────────────────────────────────────────────────"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
[ "$pass" -gt 20 ] || { echo "FATAL: only $pass assertions ran — something was skipped"; exit 1; }
exit 0
