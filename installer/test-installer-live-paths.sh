#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-live-paths.sh — the two UNENCRYPTED install paths, run for
#  real against loop-backed disks, and then BOOTED.
#
#  ═══ WHY THIS EXISTS ═══
#
#  installer/test-installer.sh asserts the engine's guards and the argv it
#  would hand bootc, and every one of those assertions is made against the
#  SOURCE. None of them partitions a disk, downloads an image onto it, or
#  watches a machine start. The engine runs inside the installer's own
#  live-environment image here (live-env-engine), because that is the machine
#  it runs on in real life, and bootc on an ostree-booted developer laptop is
#  not.
#
#  So this suite makes the two claims that a unit test cannot:
#
#    1. PARTITION MODE does not damage the other operating system on the disk.
#       Here is the proof: a GPT with a Windows-shaped ESP (\EFI\Microsoft\Boot\
#       bootmgfw.efi, a BCD, a fallback loader), a data partition full of known
#       bytes, and a free partition for Rime. The data partition is hashed RAW,
#       the Microsoft tree is hashed file by file, and `sfdisk -d` is captured
#       — because a rewritten partition table with identical bytes behind it is
#       still a Windows that no longer boots, its BCD naming PARTUUIDs that
#       moved. All three are compared after the install AND AGAIN after the
#       installed Rime has booted once, since the first boot is when Rime
#       mounts that shared ESP read-write for the first time.
#
#    2. A WHOLE-DISK NETWORK INSTALL fits on a 12 GiB disk and boots. Nothing
#       is staged: the engine partitions the disk itself, formats it, and the
#       live env's bootc streams ~6.9 GB of real registry blobs straight into
#       the target while the engine compresses the store as it arrives. The
#       layout is compared against the one `bootc install to-disk` produces,
#       the space the installed system takes is measured, and a sampler running
#       throughout records every tmpfs's usage so that "nothing lands in RAM"
#       is a MEASUREMENT and not a design claim.
#
#  ═══ WHAT "BOOTS" MEANS ═══
#
#  Not "the install command exited 0". The disk image is handed to a qemu
#  inside the rime-bootlab container, under OVMF, and installer/
#  plain-boot-drive.py reads two things off the guest's own serial console:
#  dracut's "Switching root", and then a getty prompt or a systemd target that
#  means a user could log in. Which marker fired is reported, never collapsed.
#
#  ONE DELIBERATE DIFFERENCE between the artefact installed and the artefact
#  booted, and it is announced rather than buried: the installed system's
#  bootloader entry is given serial-console kargs before the boot, because a
#  system installed with `--karg quiet --karg splash` and no console= says
#  NOTHING on a serial line and there would be nothing to read. The engine's
#  own kargs are asserted to be present FIRST and are not removed. The
#  RIME_LUKS_EXTRA_KARGS seam does the equivalent inside the engine for the
#  encrypted path; it is LUKS-only, and inventing a second seam would have
#  meant editing the file under test.
#
#  ═══ ABSOLUTE SAFETY ═══
#
#  Loop-backed image FILES only. The target is asserted to be a /dev/loop*
#  node this script attached itself, on a file under /var/lab-scratch, and the
#  script exits before touching anything if it is not. No real disk is ever
#  named. Every engine invocation — the dry runs included — goes through
#  tests/lab/nvram-guard, which hashes every `Boot*` efivar before and after
#  and fails the run if one moved; `efibootmgr -v` is ALSO captured
#  independently on either side of each install and diffed here, because two
#  measurements that agree are worth more than one that cannot be checked. The
#  engine passes bootc `--generic-image` for a loop-backed target, which is the
#  layer that PREVENTS the write (see installer/rime-install:set_nvram_args_for
#  and BOOT-BREAKAGE-2026-09-20.md); the guard is what detects it if that is
#  ever wrong, and it has caught exactly that twice.
#
#  ═══ WHAT IT NEEDS ═══
#
#  Passwordless root, podman, losetup, sgdisk, /dev/kvm, ~40 GB free on
#  /var/lab-scratch, a Rime OS image in ROOT podman storage for the partition
#  path, a working route to the registry for the disk path, the installer's
#  live-environment image (RIME_LIVE_ENV_IMAGE, built from
#  Containerfile.installer: the engine runs inside it, see live-env-engine),
#  and the rime-bootlab container image (built if absent).
#  A GitHub runner has none of these: this suite is in
#  tests/suites-not-in-ci.txt.
#
#  Usage:
#      test-installer-live-paths.sh [--path partition|disk|both] [--no-boot]
#                                   [--keep] [--boot-timeout SECONDS]
#
#  The two paths are run STRICTLY IN SEQUENCE and must never be run
#  concurrently with each other or with any other install suite: the engine's
#  $TROOT (/run/rime-target) and $LOG (/var/log/rime-install.log) are fixed
#  paths, and its preflight `unmount_target` clears "leftovers" that would in
#  fact be the other run's live target.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 1
REPO=$(cd .. && pwd)

ENGINE=./rime-install
IMAGE="${RIME_LIVE_PATHS_IMAGE:-localhost/rime-os:daily}"
LAB="${RIME_BOOTLAB_IMAGE:-localhost/rime-bootlab}"
NVGUARD="../tests/lab/nvram-guard"
# The installer's live-environment image the engine runs inside (see
# live-env-engine), built from Containerfile.installer.
LIVE_ENV_IMAGE="${RIME_LIVE_ENV_IMAGE:-localhost/rime-installer:latest}"

# The registry ref the DISK path downloads from. Left at the engine's own
# default on purpose: the point of that path is the real netinstall, and
# swapping in a local image would test a different program.
NETINSTALL_REF="${RIME_LIVE_PATHS_REF:-ghcr.io/andrenijman/rime-os:daily}"

# TEST-ONLY kargs appended to the installed bootloader entry so the guest can
# be read at all.
#
# THE ORDER OF THE TWO console= ARGUMENTS IS LOAD-BEARING, and it is the
# opposite of installer/test-installer-luks-boot.sh's. Measured 2026-09-22 on a
# partition-mode install that booted perfectly and was reported as a failure:
# the LAST console= on the kernel command line becomes /dev/console, and
# systemd-getty-generator puts the serial getty there. With
# "console=ttyS0 console=tty1" — the luks-boot spelling — /dev/console is tty1,
# NO serial getty is ever generated, and "login:" cannot appear on the serial
# line no matter how well the machine booted. That suite only ever looks for
# "Switching root", so it never had to care; this one reads a login prompt, so
# it does.
#
# ttyS0 last => /dev/console is the serial line => serial-getty@ttyS0 => a real
# "login:". Kernel messages still reach both consoles either way.
BOOT_KARGS="console=tty1 console=ttyS0,115200 systemd.log_target=kmsg systemd.show_status=1 loglevel=7 rd.plymouth=0 plymouth.enable=0 rd.timeout=120"

# 24 GiB is comfortably over the engine's 12 GB whole-disk floor and leaves
# ~21 GiB for Rime beside a 512 MiB ESP and a 2 GiB data partition.
PART_DISK_SIZE=24G
# 12 GiB is the engine's own floor (12 GB, _min_gb) rounded to the nearest
# size a person would buy: the point of this path is that a network install
# fits on a disk that small, with nothing staged anywhere else. Sparse.
DISK_DISK_SIZE=12G

WANT_PATHS="both"
# ── WHY THE DEFAULT FIXTURE CARRIES A BIOS BOOT PARTITION ────────────────────
# Measured on 2026-09-22, not reasoned: a Windows-EXACT fixture (ESP + data +
# free space, no bios_grub — which is what a GPT disk prepared by Windows Setup
# actually looks like) makes the install FAIL here, and the reason is the lab,
# not the installer.
#
#   /usr/sbin/grub2-install: error: filesystem `btrfs' doesn't support blocklists.
#   error: boot data installation failed: installing component BIOS to device
#          /dev/loop1: installing GRUB on /dev/loop1
#
# The chain, read out of the tools rather than guessed:
#   * every loopback install in this lab MUST pass bootc `--generic-image` — it
#     is the one layer that prevents the 2026-09-20 host-NVRAM incident, and
#     rime-install adds it from set_nvram_args_for for a loop-backed target;
#   * bootc's own help for that flag: "All bootloader types will be installed";
#   * so bootc does NOT hand bootupd `--auto`, and bootupd installs the BIOS
#     component as well as the ESP one;
#   * `grub2-install --target i386-pc` on a GPT disk with no bios_grub partition
#     has nowhere to embed core.img and falls back to blocklists, which btrfs
#     refuses. Hard failure, and the whole install fails with it.
#
# On real hardware the engine passes NO `--generic-image` (that branch is only
# taken for a loop-backed disk), bootc hands bootupd `--auto`, and on a
# UEFI-booted machine bootupd installs the ESP component ONLY — the BIOS step
# never runs and this failure cannot occur. `--auto` is present exactly once in
# the bootc binary; `--generic-image` is documented above.
#
# A 1 MiB bios_grub partition compensates for the lab-only flag and changes
# nothing about what this path actually measures: whether the neighbouring OS's
# partitions, its ESP files and the partition table survive, and whether the
# result boots. `--fixture windows` reproduces the red above on demand.
FIXTURE="windows+biosboot"
DO_BOOT=1
KEEP=0
BOOT_TIMEOUT=600

while [ $# -gt 0 ]; do
  case "$1" in
    --path)         WANT_PATHS="${2:?--path needs partition|disk|both}"; shift 2 ;;
    --fixture)      FIXTURE="${2:?--fixture needs windows|windows+biosboot}"; shift 2 ;;
    --no-boot)      DO_BOOT=0; shift ;;
    --keep)         KEEP=1; shift ;;
    --boot-timeout) BOOT_TIMEOUT="${2:?--boot-timeout needs seconds}"; shift 2 ;;
    -h|--help)      sed -n '2,100p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)              printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
case "$WANT_PATHS" in partition|disk|both) : ;; *) echo "--path must be partition, disk or both" >&2; exit 2 ;; esac
case "$FIXTURE" in
  windows|windows+biosboot) : ;;
  *) echo "--fixture must be windows or windows+biosboot" >&2; exit 2 ;;
esac

pass=0; fail=0
ok()   { printf 'PASS  %-58s %s\n' "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %-58s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
info() { printf 'note  %s\n' "$*"; }
hdr()  { printf '\n══ %s\n' "$*"; }
die()  { printf 'FATAL: %s\n' "$1" >&2; exit 2; }

# ── prerequisites. A missing one is a FATAL, never a skipped assertion ───────
sudo -n true 2>/dev/null || die "needs passwordless root (sudo -n)."
for t in podman losetup sgdisk mkfs.vfat sfdisk partprobe udevadm blkid; do
  command -v "$t" >/dev/null || die "$t is not installed."
done
[ -x "$NVGUARD" ] || die "$NVGUARD is missing or not executable.
This suite will not run a privileged loopback install without it — see
BOOT-BREAKAGE-2026-09-20.md and AGENTS.md \"Touching a machine's boot path\"."
if [ "$DO_BOOT" = 1 ]; then
  [ -c /dev/kvm ] || die "/dev/kvm is absent; a TCG-only OVMF boot of a full
desktop image is too slow to be useful. Re-run with --no-boot to do the install
half only, and say so in the result."
fi
case "$WANT_PATHS" in
  partition|both)
    sudo -n podman image exists "$IMAGE" 2>/dev/null \
      || die "$IMAGE is not in ROOT podman storage (the partition path installs from it).
Build it, or set RIME_LIVE_PATHS_IMAGE." ;;
esac

# ── nothing else may be installing ──────────────────────────────────────────
# $TROOT (/run/rime-target) and $LOG are FIXED paths in the engine, and its
# preflight `unmount_target` clears "leftovers" that would in fact be another
# run's live target. Two of these at once destroy each other's install, and the
# damage reads as a defect in the installer.
if pgrep -f '[a]pex-install --headless' >/dev/null 2>&1; then
  die "another rime-install is already running:
$(pgrep -af '[a]pex-install --headless')
This suite and that run share /run/rime-target and /var/log/rime-install.log."
fi
if findmnt -rno TARGET /run/rime-target >/dev/null 2>&1; then
  die "/run/rime-target is mounted — another install is live, or one was interrupted:
$(findmnt -rno TARGET,SOURCE /run/rime-target)
Release it before running this suite."
fi

# ── scratch: a real disk, never RAM ─────────────────────────────────────────
SCRATCH_ROOT="${RIME_LIVE_PATHS_SCRATCH:-/var/lab-scratch}"
sudo -n mkdir -p "$SCRATCH_ROOT" 2>/dev/null
[ -d "$SCRATCH_ROOT" ] || die "$SCRATCH_ROOT does not exist and could not be created."
case "$(df -PT "$SCRATCH_ROOT" 2>/dev/null | awk 'NR==2{print $2}')" in
  tmpfs|ramfs) die "$SCRATCH_ROOT is a RAM filesystem; a 40 GiB image there would eat the machine." ;;
esac
WORK=$(sudo -n mktemp -d "$SCRATCH_ROOT/rime-live-paths.XXXXXX") || die "no scratch directory"
# OWNED, not just readable. Nearly every capture in this file is
# `sudo -n <cmd> > "$WORK/..."`, and a redirect is performed by the SHELL, which
# is unprivileged — into a root-owned 0755 directory that is EACCES for it. At
# 0755 this suite would lose every artefact it exists to produce and the
# assertions reading them back would go red for a reason having nothing to do
# with the installer.
sudo -n chown "$(id -u):$(id -g)" "$WORK" || die "could not take ownership of $WORK"
sudo -n chmod 755 "$WORK"
: > "$WORK/.writable" || die "$WORK is not writable by $(id -un); every capture below would be lost."
rm -f "$WORK/.writable"
info "work directory: $WORK"

# Per-run state, reset by rig_*; cleanup asks the kernel rather than
# remembering, for the reason test-installer-luks-live.sh records.
LOOP=""
IMG=""
MNT="$WORK/mnt"
ESPMNT="$WORK/espmnt"
SAMPLER_PID=""

release_loop() {
  sudo -n umount -R "$MNT/boot/efi" 2>/dev/null
  sudo -n umount -R "$MNT"    2>/dev/null
  sudo -n umount -R "$ESPMNT" 2>/dev/null
  if [ -n "$LOOP" ]; then
    for _h in /sys/class/block/"$(basename "$LOOP")"*/holders/*; do
      [ -e "$_h" ] || continue
      _dm=$(sudo -n cat "$_h/dm/name" 2>/dev/null) || continue
      [ -n "$_dm" ] && sudo -n cryptsetup close "$_dm" 2>/dev/null
    done
    sudo -n losetup -d "$LOOP" 2>/dev/null
  fi
  LOOP=""
}

cleanup() {
  stop_sampler
  release_loop
  # The disk images ALWAYS go: keeping a 40 GiB file once filled a filesystem
  # and took the machine's shell with it. The small artefacts — logs, hashes,
  # serial consoles, the nvram snapshot pairs — are what a failure needs to be
  # acted on, and they are kept whenever anything failed.
  # The disk images normally ALWAYS go: keeping a 40 GiB file once filled a
  # filesystem and took the machine's shell with it. --keep is the deliberate
  # exception, because a failed boot with no disk left to mount cannot be acted
  # on, and that is precisely the run worth keeping.
  if [ "$KEEP" = 1 ]; then
    printf '\n--keep: the disk images are being LEFT in %s — delete them when done:\n' "$WORK" >&2
    sudo -n du -sh "$WORK"/*.img 2>/dev/null | sed 's/^/    /' >&2
  else
    sudo -n rm -f "$WORK"/*.img 2>/dev/null
  fi
  if [ "${fail:-1}" = 0 ] && [ "$KEEP" != 1 ]; then
    sudo -n rm -rf "$WORK" 2>/dev/null
  else
    printf '\nartefacts kept in %s\n' "$WORK" >&2
  fi
  return 0
}
trap cleanup EXIT

# ═════════════════════════════════════════════════════════════════════════════
#  Helpers
# ═════════════════════════════════════════════════════════════════════════════

# The partition node for a loop device, asked rather than pasted: losetup -P
# gives /dev/loop0p1 here and a different spelling elsewhere.
part_node() {  # $1 = loop device  $2 = index
  local c
  for c in "$1p$2" "$1$2"; do [ -b "$c" ] && { printf '%s' "$c"; return 0; }; done
  return 1
}

attach() {  # $1 = image file — sets LOOP, and proves it is a loop device
  local l
  l=$(sudo -n losetup -fP --show "$1") || die "losetup failed on $1"
  case "$l" in
    /dev/loop[0-9]*) : ;;
    *) die "refusing to continue: losetup returned '$l', which is not a loop device." ;;
  esac
  [ -e "/sys/class/block/$(basename "$l")/loop/backing_file" ] \
    || die "refusing to continue: the kernel does not consider $l loop-backed."
  LOOP="$l"
}

# The partition table, with the LOOP DEVICE NAME normalised out of it.
# `sfdisk -d` names the device on its `device:` line and at the head of every
# partition line. This suite detaches the loop and re-attaches it between the
# install check and the after-first-boot check, and `losetup -f` hands out
# whatever is free THEN — on a box where other sessions attach loops too, an
# unchanged table can come back spelled /dev/loop2 and read as a rewritten one.
# What Windows' BCD depends on is start=, size= and uuid=, and those are what
# stay in the comparison.
# The first 440 bytes of sector 0 — the MBR BOOT CODE region, before the disk
# signature and the partition entries. `sfdisk -d` dumps the partition table and
# is blind to it, so a bootloader that rewrites sector 0 and nothing else reads
# as "the partition table is unchanged".
#
# It MATTERS on a dual-boot disk and it is measured rather than assumed: with a
# bios_grub partition present, bootupd's BIOS component succeeds, and
# `grub2-install --target i386-pc` writes GRUB's boot.img here. sgdisk leaves
# this region zeroed when it writes a protective MBR, so "all zero" and "not all
# zero" distinguish the two cases with no before-snapshot needed.
mbr_bootcode_sha() {  # $1 = block device
  sudo -n dd if="$1" bs=440 count=1 status=none 2>/dev/null | sha256sum | awk '{print $1}'
}
# sha256 of 440 zero bytes — what an untouched protective MBR's boot code is.
MBR_ZERO_SHA=$(head -c 440 /dev/zero | sha256sum | awk '{print $1}')

sfdisk_norm() {  # $1 = output file
  sudo -n sfdisk -d "$LOOP" 2>/dev/null | sed "s|${LOOP}|LOOPDEV|g" > "$1"
  chmod 644 "$1" 2>/dev/null
}

raw_sha() {   # $1 = block device — sha256 of every byte of it
  sudo -n sh -c "sha256sum < '$1'" 2>/dev/null | awk '{print $1}'
}

# A manifest, not a single hash: when one file changes, the diff must name it.
tree_manifest() {  # $1 = directory
  sudo -n sh -c "cd '$1' 2>/dev/null && find . -type f -printf '%p ' -exec sha256sum {} \; " 2>/dev/null \
    | awk '{print $2"  "$1}' | sort
}

start_sampler() {  # $1 = output file
  # setsid + the process group, NEVER `pkill -f "sleep 10"`: command lines on
  # this box are byte-identical across sessions by construction, so a pattern
  # kill matches every other agent's shell too. The sampler writes its own pgid
  # where stop_sampler can read it.
  sudo -n rm -f "$WORK/sampler.pgid"
  sudo -n setsid sh -c "
    echo \$\$ > '$WORK/sampler.pgid'
    while :; do
      printf '=== %s\n' \"\$(date +%T)\"
      losetup -a 2>/dev/null | sed 's/^/loop: /'
      findmnt -t tmpfs,ramfs -no TARGET,SOURCE,USED,AVAIL 2>/dev/null | sed 's/^/tmpfs: /'
      free -m 2>/dev/null | sed -n '2p' | sed 's/^/mem: /'
      sleep 10
    done > '$1' 2>&1" &
  SAMPLER_PID=$!
  # Give the shell a moment to record its pgid before anyone may stop it.
  for _i in 1 2 3 4 5 6 7 8 9 10; do
    [ -s "$WORK/sampler.pgid" ] && break
    sleep 0.2
  done
}
stop_sampler() {
  local pgid
  [ -n "$SAMPLER_PID" ] || return 0
  pgid=$(sudo -n cat "$WORK/sampler.pgid" 2>/dev/null | tr -dc 0-9)
  if [ -n "$pgid" ] && [ "$pgid" -gt 1 ] 2>/dev/null; then
    sudo -n kill -TERM -- "-$pgid" 2>/dev/null
  fi
  kill "$SAMPLER_PID" 2>/dev/null
  SAMPLER_PID=""
}

# ── the engine, always through the guard, always with a before/after pair ────
# The identity string the GUI's confirm page records and the engine re-reads —
# the same lsblk call on both sides, so the suite writes what the GUI would.
fingerprint() { lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE "$1" 2>/dev/null | head -1; }

run_engine() {  # $1 = label  $2 = answers file  $3 = stdout file  rest = env assignments
  local label="$1" ans="$2" out="$3"; shift 3
  local before="$WORK/efi-before-$label.txt" after="$WORK/efi-after-$label.txt"
  # shellcheck disable=SC2024
  sudo -n efibootmgr -v 2>/dev/null > "$before" || echo "(no efibootmgr)" > "$before"
  local start; start=$(date +%s)
  # shellcheck disable=SC2024
  sudo -n "$NVGUARD" --label "live-paths-$label" --out "$WORK/nvram-$label" -- \
    ./live-env-engine RIME_LIVE_ENV_IMAGE="$LIVE_ENV_IMAGE" "$@" -- "$ENGINE" --headless "$ans" > "$out" 2>&1 </dev/null
  ENGINE_RC=$?
  ENGINE_SECS=$(( $(date +%s) - start ))
  # shellcheck disable=SC2024
  sudo -n efibootmgr -v 2>/dev/null > "$after" || echo "(no efibootmgr)" > "$after"
  # The WHOLE file, deliberately. rime-install:50 is `: > "$LOG"` — the engine
  # TRUNCATES /var/log/rime-install.log as its second statement, so the file only
  # ever holds the run that is finishing and there is nothing from an earlier run
  # to exclude.
  #
  # This line was briefly a byte-offset slice, on the assumption that a fixed log
  # path must accumulate. It does not, and the slice cost a REAL FALSE RED: the
  # offset recorded before the run was the size of the PREVIOUS run's log, the
  # engine then truncated, and `tail -c +N` cut the first N bytes off a fresh
  # file — exactly the early lines, which is where the staging DECISION is
  # logged. "[disk] the engine logged the fallback decision" went red against an
  # engine that had logged it perfectly. Verified rather than assumed this time:
  # `grep -n 'LOG=' installer/rime-install` and the `: > "$LOG"` on the next line.
  sudo -n cp /var/log/rime-install.log "$WORK/engine-log-$label.txt" 2>/dev/null \
    || sudo -n sh -c ": > '$WORK/engine-log-$label.txt'"
  sudo -n chmod 644 "$WORK/engine-log-$label.txt" "$before" "$after" 2>/dev/null
  if diff -u "$before" "$after" > "$WORK/efi-diff-$label.txt" 2>&1; then
    ok "[$label] this machine's UEFI boot entries did not move" "efibootmgr -v identical"
  else
    bad "[$label] this machine's UEFI boot entries did not move" \
        "STOP — do not reboot this machine. See $WORK/efi-diff-$label.txt"
    sed 's/^/    /' "$WORK/efi-diff-$label.txt"
  fi
  if grep -q 'verdict: verified' "$out"; then
    ok "[$label] nvram-guard verdict" "verified (efivarfs digests identical)"
  else
    bad "[$label] nvram-guard verdict" "$(grep 'verdict:' "$out" | tail -1)"
  fi
  # DETECTION is what the three assertions above measure. This one is about
  # PREVENTION, and they are not the same claim: "no boot entry moved" is also
  # true of a run in which bootc was never going to write one. `--generic-image`
  # is the only layer that actually skips the firmware step, and the engine adds
  # it from set_nvram_args_for when — and only when — it recognises the target as
  # loop-backed. If that recognition ever fails, everything here still reads
  # green while the machine is one bootupd away from the 2026-09-20 incident.
  # Note the dry run stops BEFORE set_nvram_args_for is ever called, so this is
  # only asserted on a run that actually installs.
  case "$label" in
    *-dry) info "[$label] (dry run stops before set_nvram_args_for; nothing to assert about --generic-image)" ;;
    *)
      if grep -q 'nvram: .* is loop-backed' "$WORK/engine-log-$label.txt"; then
        ok "[$label] PREVENTION was active" \
           "$(grep -m1 -o 'nvram: [^(]*is loop-backed' "$WORK/engine-log-$label.txt") -> bootc --generic-image"
      else
        bad "[$label] PREVENTION was active" \
            "the engine did NOT recognise the target as loop-backed, so bootc ran WITHOUT --generic-image"
      fi ;;
  esac
}

engine_protocol_ok() {  # $1 = label  $2 = stdout file
  local lastproto
  if [ "$ENGINE_RC" = 0 ]; then ok "[$1] engine exit status" "0 after ${ENGINE_SECS}s"
  else bad "[$1] engine exit status" "$ENGINE_RC after ${ENGINE_SECS}s — see $2"; fi
  lastproto=$(grep -v 'nvram-guard\[' "$2" | grep -v '^[[:space:]]*$' | tail -1)
  if [ "$lastproto" = "RIME-INSTALL-OK" ]; then ok "[$1] final protocol line is RIME-INSTALL-OK"
  else bad "[$1] final protocol line is RIME-INSTALL-OK" "got: $lastproto"; fi
  if grep -q 'Unexpected error on line' "$2"; then bad "[$1] the ERR trap did not fire" "it did"
  else ok "[$1] the ERR trap did not fire"; fi
}

# ── serial console kargs, added AFTER the install and never instead of it ────
# Returns 0 if the entry was found and rewritten. Asserts the engine's own
# kargs are there first: this must not be able to paper over an entry the
# installer failed to write.
add_boot_kargs() {  # $1 = label  $2 = root partition device
  local ent opts
  sudo -n mkdir -p "$MNT"
  if ! sudo -n mount "$2" "$MNT" 2>"$WORK/rootmount-$1.err"; then
    bad "[$1] the installed root filesystem mounts" "$(tr '\n' ' ' < "$WORK/rootmount-$1.err")"
    return 1
  fi
  # -L, and it is load-bearing: in an ostree deployment /boot/loader is a
  # SYMLINK to loader.N, and GNU find does not descend a starting point that is
  # a symlink unless told to. Without it this reports "nothing under
  # $MNT/boot/loader" on a perfectly installed disk and the boot phase is
  # skipped after the install succeeded — a false red on the wrong assertion.
  ent=$(sudo -n find -L "$MNT/boot/loader" -name '*.conf' -type f 2>/dev/null | sort | head -1)
  if [ -z "$ent" ]; then
    bad "[$1] the installer wrote a bootloader entry" "nothing under $MNT/boot/loader"
    sudo -n find "$MNT/boot" -maxdepth 2 2>/dev/null | sed 's/^/    boot: /' | head -30
    sudo -n umount -R "$MNT" 2>/dev/null
    return 1
  fi
  ok "[$1] the installer wrote a bootloader entry" "${ent#"$MNT"}"
  opts=$(sudo -n grep -m1 '^options ' "$ent" 2>/dev/null)
  printf '%s\n' "$opts" > "$WORK/kargs-before-$1.txt"
  case "$opts" in
    *quiet*splash*) ok "[$1] the entry carries the engine's own kargs" "quiet splash" ;;
    *) bad "[$1] the entry carries the engine's own kargs" "options line: $opts" ;;
  esac
  case "$opts" in
    *ostree=*) ok "[$1] the entry names an ostree deployment" ;;
    *) bad "[$1] the entry names an ostree deployment" "options line: $opts" ;;
  esac
  sudo -n sed -i "s|^options .*|& $BOOT_KARGS|" "$ent"
  # shellcheck disable=SC2024  # the redirect is the shell's; $WORK is chown'd to it
  sudo -n grep -m1 '^options ' "$ent" > "$WORK/kargs-after-$1.txt" 2>/dev/null
  sudo -n chmod 644 "$WORK/kargs-before-$1.txt" "$WORK/kargs-after-$1.txt" 2>/dev/null
  info "[$1] appended test-only serial kargs to ${ent#"$MNT"} (before/after in $WORK/kargs-*-$1.txt)"
  sudo -n umount -R "$MNT" 2>/dev/null
  return 0
}

# ── the boot ────────────────────────────────────────────────────────────────
boot_the_disk() {  # $1 = label  $2 = image file
  local bootcmd bootout bootrc verdict switched reached
  if ! sudo -n podman image exists "$LAB" 2>/dev/null; then
    info "building $LAB — qemu/OVMF are build-time tooling, deliberately not on Rime machines"
    # shellcheck disable=SC2024
    sudo -n podman build -t "$LAB" -f "$REPO/bootlab/Containerfile" "$REPO" \
      > "$WORK/bootlab-build.log" 2>&1 \
      || { bad "[$1] the rime-bootlab image builds" "see $WORK/bootlab-build.log"; return 1; }
  fi
  sudo -n install -m 0644 ./plain-boot-drive.py "$WORK/plain-boot-drive.py" \
    || { bad "[$1] plain-boot-drive.py is available"; return 1; }
  sudo -n chmod 644 "$2"

  # The firmware pair is converted INSIDE the container: edk2-ovmf lives there
  # and never on a Rime host. The varstore is Fedora's pristine one — no PK
  # enrolled, so this is UEFI Setup Mode rather than Secure Boot enforcing, and
  # the only loader the firmware can find is the removable-media fallback.
  bootcmd='set -e
qemu-img convert -O raw /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 /w/OVMF_CODE.fd
qemu-img convert -O raw /usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2 /w/OVMF_VARS_template.fd
# NOT `strings`: binutils is not in the bootlab image, and the `|| true` on
# that pipeline made the miss invisible. grep -a reads the same bytes.
# This matters for more than tidiness — the file is named .secboot.qcow2 and
# a filename is not provenance; the revision is read out of the binary.
echo "edk2-revision: $(grep -a -o -m1 -E "edk2-[0-9][0-9.a-z-]*" /w/OVMF_CODE.fd || echo unknown)"
echo "ovmf-source: /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + pristine OVMF_VARS_4M (no PK enrolled => UEFI Setup Mode, NOT Secure Boot enforcing)"
python3 /w/plain-boot-drive.py --work /w --name '"$1"' \
    --code /w/OVMF_CODE.fd --vars-template /w/OVMF_VARS_template.fd \
    --disk /w/'"$(basename "$2")"' --timeout '"$BOOT_TIMEOUT"
  bootout="$WORK/boot-stdout-$1.txt"
  # shellcheck disable=SC2024
  sudo -n podman run --rm --device /dev/kvm -v "$WORK":/w:z -w /w "$LAB" -c "$bootcmd" \
    > "$bootout" 2>&1
  bootrc=$?
  sed 's/^/    /' "$bootout"
  info "[$1] boot driver exit=$bootrc"

  local edk2rev
  edk2rev=$(grep -m1 '^edk2-revision: ' "$bootout" | cut -d' ' -f2-)
  info "[$1] guest firmware: ${edk2rev:-unread} (read out of the binary, not off the filename)"
  verdict=$(grep '^verdict=' "$bootout" | tail -1 | cut -d= -f2-)
  switched=$(grep '^switched-root=' "$bootout" | tail -1 | cut -d= -f2-)
  reached=$(grep '^reached-by=' "$bootout" | tail -1 | cut -d= -f2-)
  # The weak marker, surfaced HERE so a reader of this log does not have to open
  # the driver's output to learn what was actually seen. It is deliberately not
  # an assertion: getty.target with nothing wanting it is reached instantly and
  # proves nothing about a login (see WEAK_MARKERS in plain-boot-drive.py).
  local weak
  weak=$(grep -m1 '^weak-marker=' "$bootout" | cut -d= -f2-)
  [ -n "$weak" ] && [ "$weak" != none ] \
    && info "[$1] weak marker seen: $weak (recorded, NOT counted as reaching a login)"
  if [ "$switched" = yes ]; then
    ok "[$1] dracut switched root — the disk this installer wrote BOOTS"
  else
    bad "[$1] dracut switched root — the disk this installer wrote BOOTS" \
        "verdict=$verdict — see $WORK/serial-$1.log"
  fi
  if [ -n "$reached" ] && [ "$reached" != none ]; then
    ok "[$1] the installed system reached a login" "$reached"
  else
    bad "[$1] the installed system reached a login" "verdict=$verdict — see $WORK/serial-$1.log"
  fi
  # A greeter that cannot open a compositor in a GPU-less guest exits cleanly
  # and is restarted for as long as the guest runs. That is a property of
  # running Rime in this lab, not of the install — but it is ALSO the reason
  # multi-user.target and graphical.target are never announced, so it is
  # reported rather than left for the next reader to rediscover.
  local loops units
  loops=$(grep -m1 '^scheduled-restart-jobs=' "$bootout" | cut -d= -f2-)
  units=$(grep -m1 '^restart-looping-units=' "$bootout" | cut -d= -f2-)
  if [ -n "$loops" ] && [ "$loops" != 0 ]; then
    info "[$1] FINDING: $loops restart(s) of ${units:-unknown} during the boot window."
    info "[$1]   systemd does not announce a target while a unit in it is cycling, which"
    info "[$1]   is why graphical.target/multi-user.target may be absent on a booted guest."
  fi
  sudo -n chmod 644 "$WORK/serial-$1.log" 2>/dev/null
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════
#  PATH 1 — partition mode, on a disk that already has another OS on it
# ═════════════════════════════════════════════════════════════════════════════
run_partition_path() {
  hdr "PATH 1 — partition mode on a multi-OS disk (the dual-boot case)"
  IMG="$WORK/multiboot.img"
  sudo -n truncate -s "$PART_DISK_SIZE" "$IMG" || die "could not create $IMG"
  attach "$IMG"
  info "target: $LOOP ($IMG, $PART_DISK_SIZE)"

  # ── the fake Windows layout ───────────────────────────────────────────────
  # p1 ESP, p2 a Microsoft basic data partition, p3 free space for Rime. The
  # type GUIDs are the real ones: the engine reads p1's to decide whether it is
  # an ESP at all, and a "close enough" GUID would make this suite prove that a
  # guard it never reached was satisfied.
  sudo -n sgdisk --zap-all "$LOOP" >/dev/null 2>&1
  # The type GUIDs are the real ones: the engine reads p_esp's to decide whether
  # it is an ESP at all, and a "close enough" GUID would make this suite prove
  # that a guard it never reached was satisfied.
  if [ "$FIXTURE" = "windows+biosboot" ]; then
    # See the FIXTURE comment at the top for why the 1 MiB bios_grub is here and
    # what it compensates for. It is 21686148-…, the same type the engine's own
    # whole-disk path creates.
    sudo -n sgdisk -n1:0:+1M   -t1:21686148-6449-6E6F-744E-656564454649 -c1:BIOS-BOOT \
                   -n2:0:+512M -t2:C12A7328-F81F-11D2-BA4B-00A0C93EC93B -c2:EFI-SYSTEM \
                   -n3:0:+2G   -t3:EBD0A0A2-B9E5-4433-87C0-68B6B72699C7 -c3:WINDATA \
                   -n4:0:0     -t4:0FC63DAF-8483-4772-8E79-3D69D8477DE4 -c4:RIMEROOT \
                   "$LOOP" >/dev/null 2>&1 || die "sgdisk could not rig the multi-OS disk"
    I_ESP=2; I_DATA=3; I_ROOT=4
  else
    sudo -n sgdisk -n1:0:+512M -t1:C12A7328-F81F-11D2-BA4B-00A0C93EC93B -c1:EFI-SYSTEM \
                   -n2:0:+2G   -t2:EBD0A0A2-B9E5-4433-87C0-68B6B72699C7 -c2:WINDATA \
                   -n3:0:0     -t3:0FC63DAF-8483-4772-8E79-3D69D8477DE4 -c3:RIMEROOT \
                   "$LOOP" >/dev/null 2>&1 || die "sgdisk could not rig the multi-OS disk"
    I_ESP=1; I_DATA=2; I_ROOT=3
    info "fixture=windows (Windows-EXACT: no bios_grub). The install is EXPECTED to fail"
    info "  at bootupd's BIOS component for a LAB reason — see the FIXTURE comment."
  fi
  sudo -n partprobe "$LOOP" >/dev/null 2>&1
  sudo -n udevadm settle --timeout=30 >/dev/null 2>&1
  P_ESP=$(part_node "$LOOP" "$I_ESP")   || die "no ESP node appeared on $LOOP"
  P_DATA=$(part_node "$LOOP" "$I_DATA") || die "no data node appeared on $LOOP"
  P_ROOT=$(part_node "$LOOP" "$I_ROOT") || die "no root node appeared on $LOOP"

  sudo -n mkfs.vfat -F32 -n EFISYS "$P_ESP" >/dev/null 2>&1 || die "mkfs.vfat failed on $P_ESP"
  sudo -n mkfs.ext4 -F -L WINDATA "$P_DATA" >/dev/null 2>&1 || die "mkfs.ext4 failed on $P_DATA"

  sudo -n mkdir -p "$ESPMNT"
  sudo -n mount "$P_ESP" "$ESPMNT" || die "could not mount the rigged ESP"
  sudo -n mkdir -p "$ESPMNT/EFI/Microsoft/Boot" "$ESPMNT/EFI/Microsoft/Recovery" "$ESPMNT/EFI/Boot"
  # Known bytes, deterministic size, no randomness: a hash that changes must
  # mean the file changed and never that the fixture did.
  sudo -n sh -c "head -c 1500000 /dev/zero | tr '\\0' 'M' > '$ESPMNT/EFI/Microsoft/Boot/bootmgfw.efi'"
  sudo -n sh -c "head -c 65536   /dev/zero | tr '\\0' 'B' > '$ESPMNT/EFI/Microsoft/Boot/BCD'"
  sudo -n sh -c "head -c 4096    /dev/zero | tr '\\0' 'R' > '$ESPMNT/EFI/Microsoft/Recovery/BCD'"
  # The removable-media fallback. Windows Setup writes one, and bootupd writes
  # its own over the top — see the assertion below, which EXPECTS this to move.
  sudo -n sh -c "head -c 1000000 /dev/zero | tr '\\0' 'F' > '$ESPMNT/EFI/Boot/bootx64.efi'"
  sudo -n sync
  ESP_MS_BEFORE=$(tree_manifest "$ESPMNT/EFI/Microsoft")
  ESP_ALL_BEFORE=$(sudo -n find "$ESPMNT" -mindepth 1 | sed "s|^$ESPMNT||" | sort)
  FALLBACK_BEFORE=$(sudo -n sh -c "sha256sum < '$ESPMNT/EFI/Boot/bootx64.efi'" | awk '{print $1}')
  sudo -n umount "$ESPMNT"

  sudo -n mkdir -p "$MNT"
  sudo -n mount "$P_DATA" "$MNT" || die "could not mount the rigged data partition"
  sudo -n sh -c "head -c 40000000 /dev/zero | tr '\\0' 'D' > '$MNT/user-documents.bin'"
  sudo -n sh -c "printf 'this belongs to the other operating system\n' > '$MNT/README.txt'"
  sudo -n sync
  sudo -n umount "$MNT"

  DATA_SHA_BEFORE=$(raw_sha "$P_DATA")
  sfdisk_norm "$WORK/sfdisk-before.txt"
  printf '%s\n' "$ESP_MS_BEFORE" > "$WORK/esp-microsoft-before.txt"
  info "rigged: ESP=$P_ESP data=$P_DATA (sha ${DATA_SHA_BEFORE:0:16}…) free=$P_ROOT"

  # ── the answers ───────────────────────────────────────────────────────────
  local ans="$WORK/answers-partition"
  sudo -n sh -c "cat > '$ans' <<EOF
mode=partition
disk=$LOOP
target=$P_ROOT
esp=$P_ESP
username=tester
password=loginpw123
hostname=rimepart
encrypt=no
keymap=us
timezone=Australia/Perth
EOF"
  # The typed ERASE bound to all three identities, as the GUI's confirm page
  # writes it. Appended separately: the values carry double quotes, which the
  # sh -c heredoc above would mangle.
  printf 'confirmed=ERASE\nconfirm_target=%s\nconfirm_disk_id=%s\nconfirm_target_id=%s\nconfirm_esp_id=%s\n' \
    "$P_ROOT" "$(fingerprint "$LOOP")" "$(fingerprint "$P_ROOT")" "$(fingerprint "$P_ESP")" \
    | sudo -n tee -a "$ans" >/dev/null
  sudo -n chmod 600 "$ans"

  # ── the dry run: every guard, on the real nodes, writing nothing ──────────
  hdr "PATH 1 — dry run (all guards, no writes)"
  run_engine partition-dry "$ans" "$WORK/engine-partition-dry.txt" \
    RIME_IMAGE="$IMAGE" RIME_NETINSTALL=0 RIME_DRY_RUN=1
  if grep -q '^RIME-INSTALL-DRYRUN-OK' "$WORK/engine-partition-dry.txt"; then
    ok "[partition] every guard passes against the real loop nodes" "RIME-INSTALL-DRYRUN-OK"
  else
    bad "[partition] every guard passes against the real loop nodes" \
        "$(grep -v 'nvram-guard\[' "$WORK/engine-partition-dry.txt" | tail -3 | tr '\n' ' ')"
    return 1
  fi
  if [ "$(raw_sha "$P_DATA")" = "$DATA_SHA_BEFORE" ]; then
    ok "[partition] the dry run wrote nothing to the neighbour's partition"
  else
    bad "[partition] the dry run wrote nothing to the neighbour's partition"
  fi

  # ── the install ───────────────────────────────────────────────────────────
  hdr "PATH 1 — the real install (this is the slow part)"
  run_engine partition "$ans" "$WORK/engine-partition.txt" \
    RIME_IMAGE="$IMAGE" RIME_NETINSTALL=0
  engine_protocol_ok partition "$WORK/engine-partition.txt"
  if [ "$ENGINE_RC" != 0 ]; then
    tail -40 "$WORK/engine-partition.txt" | sed 's/^/    /'
    return 1
  fi

  # ── what survived ─────────────────────────────────────────────────────────
  hdr "PATH 1 — what the other operating system looks like afterwards"
  assert_neighbour_intact "after-install"

  sudo -n mkdir -p "$ESPMNT"
  if sudo -n mount -o ro "$P_ESP" "$ESPMNT" 2>/dev/null; then
    if sudo -n test -d "$ESPMNT/EFI/fedora"; then
      ok "[partition] Rime was added under EFI/fedora alongside Microsoft"
    else
      bad "[partition] Rime was added under EFI/fedora alongside Microsoft" "no EFI/fedora"
    fi
    if sudo -n test -f "$ESPMNT/EFI/BOOT/BOOTX64.EFI" || sudo -n test -f "$ESPMNT/EFI/Boot/bootx64.efi"; then
      ok "[partition] the ESP carries a removable-media fallback loader"
    else
      bad "[partition] the ESP carries a removable-media fallback loader" "OVMF will find nothing"
    fi
    # EXPECTED TO MOVE, and reported as a finding rather than asserted either
    # way: bootupd writes \EFI\BOOT\BOOTX64.EFI on every install, which on a
    # real dual-boot disk replaces the one Windows Setup left. Nothing here
    # can stop that; what this suite owes the reader is that it SAYS so.
    local fb_after
    fb_after=$(sudo -n sh -c "sha256sum < '$ESPMNT/EFI/Boot/bootx64.efi' 2>/dev/null || sha256sum < '$ESPMNT/EFI/BOOT/BOOTX64.EFI' 2>/dev/null" | awk '{print $1}')
    if [ "$fb_after" = "$FALLBACK_BEFORE" ]; then
      info "[partition] FINDING: \\EFI\\BOOT\\BOOTX64.EFI was NOT replaced (sha unchanged)"
    else
      info "[partition] FINDING: \\EFI\\BOOT\\BOOTX64.EFI was replaced by bootupd — ${FALLBACK_BEFORE:0:12}… -> ${fb_after:0:12}…"
      info "[partition]   On a real dual-boot disk this overwrites the fallback loader Windows"
      info "[partition]   Setup leaves behind. Windows' own NVRAM entry names \\EFI\\Microsoft\\Boot\\"
      info "[partition]   bootmgfw.efi, which is intact, so Windows still boots from firmware."
    fi
    sudo -n find "$ESPMNT" -mindepth 1 | sed "s|^$ESPMNT||" | sort > "$WORK/esp-tree-after.txt"
    sudo -n chmod 644 "$WORK/esp-tree-after.txt"
    sudo -n umount "$ESPMNT"
  else
    bad "[partition] the ESP still mounts after the install" "mount failed"
  fi

  # ── boot it ───────────────────────────────────────────────────────────────
  if [ "$DO_BOOT" = 1 ]; then
    hdr "PATH 1 — booting the disk under OVMF"
    add_boot_kargs partition "$P_ROOT" || return 1
    local holders
    holders=$(sudo -n findmnt -n -o TARGET,SOURCE,OPTIONS -S "$P_ESP" 2>/dev/null || true)
    if [ -n "$holders" ]; then
      bad "[partition] nothing still holds the target ESP mounted" "$(printf '%s' "$holders" | tr '\n' ';')"
    else
      ok "[partition] nothing still holds the target ESP mounted"
    fi
    release_loop
    boot_the_disk partition "$IMG"
    # "survived the install" and "survived the first boot" are different
    # claims, and the second is the one a dual-booting owner means: the
    # installed Rime mounts that shared ESP read-write at every boot.
    hdr "PATH 1 — what the other operating system looks like after Rime has BOOTED once"
    attach "$IMG"
    P_ESP=$(part_node "$LOOP" "$I_ESP"); P_DATA=$(part_node "$LOOP" "$I_DATA"); P_ROOT=$(part_node "$LOOP" "$I_ROOT")
    assert_neighbour_intact "after-first-boot"
  fi
  release_loop
  [ "$KEEP" = 1 ] || sudo -n rm -f "$IMG"
  return 0
}

assert_neighbour_intact() {  # $1 = when
  local when="$1" now
  now=$(raw_sha "$P_DATA")
  if [ -n "$now" ] && [ "$now" = "$DATA_SHA_BEFORE" ]; then
    ok "[partition/$when] the neighbour's data partition is byte-identical" "sha256 ${now:0:16}…"
  else
    bad "[partition/$when] the neighbour's data partition is byte-identical" \
        "${DATA_SHA_BEFORE:0:16}… -> ${now:0:16}…"
  fi
  sfdisk_norm "$WORK/sfdisk-$when.txt"
  if diff -u "$WORK/sfdisk-before.txt" "$WORK/sfdisk-$when.txt" > "$WORK/sfdisk-diff-$when.txt" 2>&1; then
    ok "[partition/$when] the partition table is unchanged" "PARTUUIDs intact — Windows' BCD still resolves"
  else
    bad "[partition/$when] the partition table is unchanged" "see $WORK/sfdisk-diff-$when.txt"
    sed 's/^/    /' "$WORK/sfdisk-diff-$when.txt"
  fi
  # Sector 0, which sfdisk -d cannot see. Reported as a FINDING, not asserted
  # either way: which answer is correct depends on whether bootupd's BIOS
  # component ran, and that is decided by the lab-only --generic-image.
  local mbr_now
  mbr_now=$(mbr_bootcode_sha "$LOOP")
  if [ "$mbr_now" = "$MBR_ZERO_SHA" ]; then
    info "[partition/$when] FINDING: the MBR boot code (sector 0, first 440 B) is still all zero"
    info "[partition/$when]   bootupd's BIOS component wrote nothing there."
  else
    info "[partition/$when] FINDING: the MBR boot code (sector 0, first 440 B) is NOT zero — ${mbr_now:0:12}…"
    info "[partition/$when]   grub2-install --target i386-pc wrote boot.img into it. On a real"
    info "[partition/$when]   Windows disk this replaces the protective-MBR boot code. UEFI"
    info "[partition/$when]   firmware never executes it, so Windows still boots; and on real"
    info "[partition/$when]   hardware the BIOS component does not run at all (no --generic-image)."
  fi
  sudo -n mkdir -p "$ESPMNT"
  if sudo -n mount -o ro "$P_ESP" "$ESPMNT" 2>/dev/null; then
    local ms_now
    ms_now=$(tree_manifest "$ESPMNT/EFI/Microsoft")
    printf '%s\n' "$ms_now" > "$WORK/esp-microsoft-$when.txt"
    sudo -n chmod 644 "$WORK/esp-microsoft-$when.txt" 2>/dev/null
    if [ -n "$ms_now" ] && [ "$ms_now" = "$ESP_MS_BEFORE" ]; then
      ok "[partition/$when] EFI/Microsoft is byte-identical, file for file" \
         "$(printf '%s\n' "$ms_now" | grep -c .) files"
    else
      bad "[partition/$when] EFI/Microsoft is byte-identical, file for file"
      diff -u "$WORK/esp-microsoft-before.txt" "$WORK/esp-microsoft-$when.txt" | sed 's/^/    /'
    fi
    # A separate claim from the hashes above, and the one a dual-booter means by
    # "my other OS is still there": Rime may ADD paths to a shared ESP and must
    # DELETE none. Hashing EFI/Microsoft cannot see a deletion outside that
    # subtree — an \EFI\Boot or a vendor directory quietly removed reads green.
    local gone
    gone=$(sudo -n find "$ESPMNT" -mindepth 1 | sed "s|^$ESPMNT||" | sort \
           | comm -23 <(printf '%s\n' "$ESP_ALL_BEFORE") - )
    if [ -z "$gone" ]; then
      ok "[partition/$when] nothing that was on the ESP was removed" \
         "$(printf '%s\n' "$ESP_ALL_BEFORE" | grep -c .) paths all still present"
    else
      bad "[partition/$when] nothing that was on the ESP was removed"
      printf '%s\n' "$gone" | sed 's/^/    missing: /'
    fi
    sudo -n umount "$ESPMNT"
  else
    bad "[partition/$when] the ESP mounts so EFI/Microsoft can be compared" "mount failed"
  fi
}

# ═════════════════════════════════════════════════════════════════════════════
#  PATH 2 — unencrypted whole disk, network install, on the smallest disk
# ═════════════════════════════════════════════════════════════════════════════
run_disk_path() {
  hdr "PATH 2 — whole disk, network install, ${DISK_DISK_SIZE} disk, nothing staged"
  IMG="$WORK/wholedisk.img"
  sudo -n truncate -s "$DISK_DISK_SIZE" "$IMG" || die "could not create $IMG"
  attach "$IMG"
  info "target: $LOOP ($IMG, $DISK_DISK_SIZE), download ref $NETINSTALL_REF"

  local ans="$WORK/answers-disk"
  sudo -n sh -c "cat > '$ans' <<EOF
mode=disk
disk=$LOOP
username=tester
password=loginpw123
hostname=rimedisk
encrypt=no
keymap=us
timezone=Australia/Perth
EOF"
  printf 'confirmed=ERASE\nconfirm_target=%s\nconfirm_disk_id=%s\n' \
    "$LOOP" "$(fingerprint "$LOOP")" | sudo -n tee -a "$ans" >/dev/null
  sudo -n chmod 600 "$ans"

  hdr "PATH 2 — dry run (all guards, the reachability probe, no writes)"
  run_engine disk-dry "$ans" "$WORK/engine-disk-dry.txt" \
    RIME_IMAGE="$IMAGE" RIME_NETINSTALL=1 RIME_TARGET_IMAGE="$NETINSTALL_REF" RIME_DRY_RUN=1
  if grep -q '^RIME-INSTALL-DRYRUN-OK' "$WORK/engine-disk-dry.txt"; then
    ok "[disk] every guard passes against the real loop node" "RIME-INSTALL-DRYRUN-OK"
  else
    bad "[disk] every guard passes against the real loop node" \
        "$(grep -v 'nvram-guard\[' "$WORK/engine-disk-dry.txt" | tail -3 | tr '\n' ' ')"
    return 1
  fi
  if grep -q 'downloads onto it as it goes' "$WORK/engine-disk-dry.txt"; then
    ok "[disk] the engine says the download and the wipe are one step"
  else
    bad "[disk] the engine says the download and the wipe are one step" "no such note"
  fi

  hdr "PATH 2 — the real install: partition, format, stream ~6.9 GB from the registry, compress"
  start_sampler "$WORK/sampler-disk.txt"
  run_engine disk "$ans" "$WORK/engine-disk.txt" \
    RIME_IMAGE="$IMAGE" RIME_NETINSTALL=1 RIME_TARGET_IMAGE="$NETINSTALL_REF"
  stop_sampler
  engine_protocol_ok disk "$WORK/engine-disk.txt"
  sudo -n chmod 644 "$WORK/sampler-disk.txt" 2>/dev/null

  # ── what bootc was handed, from the engine's own log ──────────────────────
  hdr "PATH 2 — how bootc was run"
  local log="$WORK/engine-log-disk.txt" argv
  argv=$(grep -m1 'bootc: .*install to-filesystem' "$log" || true)
  for want in "--source-imgref registry:" "--bootloader grub" "--skip-finalize" "--generic-image"; do
    case "$argv" in
      *"$want"*) ok "[disk] bootc got $want" ;;
      *) bad "[disk] bootc got $want" "${argv:-no bootc line in $log}" ;;
    esac
  done
  if grep -q '^.* compressed: [0-9]* objects; target now uses [0-9]* MB' "$log"; then
    ok "[disk] the store was compressed as it arrived" "$(grep -m1 -o 'compressed: .*' "$log")"
  else
    bad "[disk] the store was compressed as it arrived" "no 'compressed:' line in $log"
  fi
  # No tmpfs grew. Baseline is the first sample; 300 MB of headroom covers the
  # ordinary churn of a live machine without covering a 6.9 GB download.
  local grew
  grew=$(awk '
    /^tmpfs: /{
      tgt=$2; used=$4
      # USED comes as e.g. 9.9M / 1.2G / 512K
      u=used; unit=substr(u,length(u),1); v=substr(u,1,length(u)-1)+0
      if (unit=="G") v*=1024; else if (unit=="K") v/=1024; else if (unit=="T") v*=1048576
      if (!(tgt in base)) base[tgt]=v
      if (v-base[tgt] > peak[tgt]) peak[tgt]=v-base[tgt]
    }
    END{ for (t in peak) if (peak[t] > 300) printf "%s +%.0fMB ", t, peak[t] }
  ' "$WORK/sampler-disk.txt")
  if [ -z "$grew" ]; then
    ok "[disk] no tmpfs grew by more than 300 MB during the install" \
       "$(grep -c '^=== ' "$WORK/sampler-disk.txt") samples"
  else
    bad "[disk] no tmpfs grew by more than 300 MB during the install" "$grew"
  fi

  if [ "$ENGINE_RC" != 0 ]; then
    tail -60 "$WORK/engine-disk.txt" | sed 's/^/    /'
    return 1
  fi

  # ── the layout, compared against the one bootc install to-disk makes ──────
  hdr "PATH 2 — the layout the engine built"
  sudo -n partprobe "$LOOP" >/dev/null 2>&1
  sudo -n udevadm settle --timeout=30 >/dev/null 2>&1
  local nparts
  nparts=$(sudo -n lsblk -rno NAME,TYPE "$LOOP" 2>/dev/null | awk '$2=="part"' | wc -l)
  if [ "$nparts" = 3 ]; then ok "[disk] three partitions"; else bad "[disk] three partitions" "got $nparts"; fi
  local p1 p2 p3 t1 t2 t3 s1 s2
  p1=$(part_node "$LOOP" 1); p2=$(part_node "$LOOP" 2); p3=$(part_node "$LOOP" 3)
  t1=$(sudo -n lsblk -dno PARTTYPE "$p1" 2>/dev/null | tr 'A-Z' 'a-z' | tr -d ' ')
  t2=$(sudo -n lsblk -dno PARTTYPE "$p2" 2>/dev/null | tr 'A-Z' 'a-z' | tr -d ' ')
  t3=$(sudo -n lsblk -dno PARTTYPE "$p3" 2>/dev/null | tr 'A-Z' 'a-z' | tr -d ' ')
  [ "$t1" = 21686148-6449-6e6f-744e-656564454649 ] \
    && ok "[disk] p1 is a BIOS boot partition" || bad "[disk] p1 is a BIOS boot partition" "type $t1"
  [ "$t2" = c12a7328-f81f-11d2-ba4b-00a0c93ec93b ] \
    && ok "[disk] p2 is an EFI System Partition" || bad "[disk] p2 is an EFI System Partition" "type $t2"
  [ "$t3" = 4f68bce3-e8cd-4db1-96e7-fbcaf984b709 ] \
    && ok "[disk] p3 is typed x86-64 root" || bad "[disk] p3 is typed x86-64 root" "type $t3"
  s1=$(sudo -n lsblk -bdno SIZE "$p1" 2>/dev/null | tr -d ' ')
  s2=$(sudo -n lsblk -bdno SIZE "$p2" 2>/dev/null | tr -d ' ')
  [ "$s1" = $((1024*1024)) ] && ok "[disk] p1 is 1 MiB" || bad "[disk] p1 is 1 MiB" "$s1 bytes"
  [ "$s2" = $((512*1024*1024)) ] && ok "[disk] p2 is 512 MiB" || bad "[disk] p2 is 512 MiB" "$s2 bytes"
  local rootfs espfs
  rootfs=$(sudo -n blkid -s TYPE -o value "$p3" 2>/dev/null)
  espfs=$(sudo -n blkid -s TYPE -o value "$p2" 2>/dev/null)
  [ "$rootfs" = btrfs ] && ok "[disk] the root filesystem is btrfs" || bad "[disk] the root filesystem is btrfs" "'$rootfs'"
  [ "$espfs" = vfat ] && ok "[disk] the ESP is vfat" || bad "[disk] the ESP is vfat" "'$espfs'"

  # How much of the disk the installed system takes. MEASURED 2026-10-04 at
  # 8.1 GB for release 2026.09.30.3; 9.5 GB is that plus room for a release
  # to grow a little, and well under what the uncompressed store took (14.95
  # GB), so a store that stopped being compressed fails here.
  sudo -n mkdir -p "$MNT"
  if sudo -n mount "$p3" "$MNT" 2>/dev/null; then
    local used_mb
    used_mb=$(sudo -n df -BM --output=used "$MNT" | tail -1 | tr -dc '0-9')
    if [ -n "$used_mb" ] && [ "$used_mb" -le 9500 ]; then
      ok "[disk] the installed system is compressed" "${used_mb} MB used on the root filesystem"
    else
      bad "[disk] the installed system is compressed" "${used_mb:-?} MB used (want <= 9500)"
    fi
    if [ "$(sudo -n btrfs property get "$MNT" compression 2>/dev/null)" = "compression=zstd" ]; then
      ok "[disk] the root carries the compression property for later writes"
    else
      bad "[disk] the root carries the compression property for later writes" \
          "$(sudo -n btrfs property get "$MNT" compression 2>&1)"
    fi
    sudo -n df -Pk "$MNT" | sed 's/^/    /'
    sudo -n umount -R "$MNT" 2>/dev/null
  else
    bad "[disk] the installed root filesystem mounts"
  fi
  sudo -n mkdir -p "$ESPMNT"
  if sudo -n mount -o ro "$p2" "$ESPMNT" 2>/dev/null; then
    sudo -n test -f "$ESPMNT/EFI/BOOT/BOOTX64.EFI" \
      && ok "[disk] the ESP carries the removable-media fallback loader" \
      || bad "[disk] the ESP carries the removable-media fallback loader" "OVMF has no Boot#### entry to use instead"
    sudo -n find "$ESPMNT" -maxdepth 3 | sed "s|^$ESPMNT||" | sort > "$WORK/esp-tree-disk.txt"
    sudo -n chmod 644 "$WORK/esp-tree-disk.txt"
    sudo -n umount "$ESPMNT"
  else
    bad "[disk] the ESP mounts after the install"
  fi

  if [ "$DO_BOOT" = 1 ]; then
    hdr "PATH 2 — booting the disk under OVMF"
    add_boot_kargs disk "$p3" || return 1
    local holders
    holders=$(sudo -n findmnt -n -o TARGET,SOURCE,OPTIONS -S "$p2" 2>/dev/null || true)
    [ -n "$holders" ] && bad "[disk] nothing still holds the target ESP mounted" "$(printf '%s' "$holders" | tr '\n' ';')" \
                      || ok "[disk] nothing still holds the target ESP mounted"
    release_loop
    boot_the_disk disk "$IMG"
  fi
  release_loop
  [ "$KEEP" = 1 ] || sudo -n rm -f "$IMG"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════
#  Run. Strictly sequential — see the header.
# ═════════════════════════════════════════════════════════════════════════════
printf 'image (partition path): %s\n' "$IMAGE"
printf 'registry ref (disk path): %s\n' "$NETINSTALL_REF"
printf 'partition fixture: %s\n' "$FIXTURE"
printf 'boot phase: %s\n' "$([ "$DO_BOOT" = 1 ] && echo "yes, ${BOOT_TIMEOUT}s timeout" || echo "SKIPPED (--no-boot)")"

case "$WANT_PATHS" in
  partition|both) run_partition_path ;;
esac
case "$WANT_PATHS" in
  disk|both) run_disk_path ;;
esac

hdr "result"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
