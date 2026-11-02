# shellcheck shell=bash
# ─────────────────────────────────────────────────────────────────────────────
#  rime-install-nixos.sh — the NixOS install path of the Rime OS installer
#  (PLAN.md D9). Sourced by rime-install; never run on its own.
#
#  D9 keeps the signed Fedora live ISO, its GUI and this engine, and replaces
#  ONE step: `bootc install` becomes "copy a prebuilt system closure from a
#  signed binary cache, then `nixos-install --system <it>`". Nothing is
#  evaluated on the target. The live stick stays Fedora's because its shim and
#  GRUB are the ones a Secure Boot machine with only Microsoft's keys trusts;
#  a NixOS live ISO's GRUB is unsigned and would not boot there.
#
#  ACTIVE WHEN the ISO was built with NIXOS_SYSTEM=… (build-live-iso.sh), which
#  stamps $NIXOS_STAMPS/nixos-system. On a bootc ISO nothing here runs.
#
#  WHAT IT WRITES (whole disk only; partition mode is refused by name):
#    GPT 1  EFI System Partition, FAT32 "RIME-ESP", 2 GiB, mounted at /boot.
#           Fedora's shim + GRUB (from this live env) at EFI/fedora and the
#           removable path EFI/BOOT, bootupd's static GRUB config, and the BLS
#           entries + kernels the system's boot hook (rime-boot, layout bls)
#           writes during nixos-install.
#    GPT 2  btrfs "rime-root" — plain, or inside LUKS2 "RIMEROOT" (argon2id,
#           recovery key from rime-luks-enroll, both keys proven before the
#           system is copied). Subvolumes @nixos (/), @nix (/nix), @home
#           (/var/home), mounted compress-force=zstd:3.
#    /var/lib/rime/host.toml  hostname, timezone (required), locale, keyboard,
#           and [boot] layout=bls + microcode (the CPU's vendor) + kernel_params (rd.luks.uuid=…). The boot
#           hook reads [boot]; rime-hostlayer applies the rest at activation.
#    the user, created imperatively (users.mutableUsers), in wheel.
#
#  STAMPS ($NIXOS_STAMPS, default /usr/lib/rime-installer; written at ISO build):
#    nixos-system      the toplevel store path to install
#    nixos-cache       the binary cache URL, or `iso` = the cache on this ISO
#    nixos-cache.pub   the cache's public key (name:base64); nothing unsigned
#                      by it is copied
#    nixos-mok.der     the certificate the kernel is signed with (optional)
#    kernel-signed     `signed` when that kernel is signed (as for bootc)
#
#  TEST SEAMS (environment only, each one logged when used):
#    RIME_NIXOS_STAMPS       another stamp directory
#    RIME_NIXOS_CACHE        another cache URL
#    RIME_NIXOS_EFI_SRC      where shimx64.efi/grubx64.efi/mmx64.efi/BOOTX64.CSV are
#    RIME_NIXOS_GRUB_STATIC  bootupd's static GRUB config directory
#    RIME_NIXOS_MIN_BYTES    a lower disk floor (a test VM's disk)
#    RIME_EXTRA_KARGS        extra kernel arguments (a lab's serial console)
# ─────────────────────────────────────────────────────────────────────────────

NIXOS_STAMPS="${RIME_NIXOS_STAMPS:-/usr/lib/rime-installer}"
NIXOS=0
[ -r "$NIXOS_STAMPS/nixos-system" ] && NIXOS=1

# The disk floor (closure.md A11): a fresh system, ONE update staged beside it
# and the rollback generation, as `rime update`'s space gate counts them —
# unpacked bytes, not compressed ones. Below it the machine installs but its
# first update is refused, so it is refused here instead, before anything is
# erased. Decimal gigabytes, like the disk sizes printed on the box.
NIXOS_MIN_GB=19
NIXOS_MIN_BYTES="${RIME_NIXOS_MIN_BYTES:-$((NIXOS_MIN_GB * 1000 * 1000 * 1000))}"
NIXOS_ESP_MIB=2048
NIXOS_ESP_LABEL=RIME-ESP
NIXOS_ROOT_LABEL=rime-root
NIXOS_SUBVOLS="@nixos @nix @home"
NIXOS_MOUNT_OPTS="compress-force=zstd:3"
# rime-boot's own rule for a kernel parameter (nix/pkgs/rime-boot SAFE_PARAM), the same character set written
# as a POSIX bracket expression: `]` first and `-` last, because `\]` inside brackets ends them in grep -E (the
# Python spelling, copied verbatim, refused every parameter, `console=tty0` included).
NIXOS_SAFE_PARAM='^[][A-Za-z0-9_./:=,+@%~^!-]+$'

nixos_stamp() { tr -d '\r\n' < "$NIXOS_STAMPS/$1" 2>/dev/null || true; }

nixos_nix() {  # nix with the cache's key as the ONLY trusted one, never a build user
  nix --extra-experimental-features nix-command \
      --option build-users-group '' \
      --option require-sigs true \
      --option trusted-public-keys "$NIXOS_PUBKEY" \
      --option substituters '' \
      "$@"
}

# Run a program of the TARGET system: its store is mounted at /nix in a private
# mount namespace, so the target's own nixos-install / nixos-enter / useradd run
# with the store paths they were built against, and the live env never gets a
# /nix of its own.
nixos_in_target() {
  # bash -c SCRIPT $0 $1 ARGS...: $0 is the target root, $1 the system, then the command.
  unshare --mount --propagation private -- bash -c '
    set -e
    mkdir -p /nix
    mount --rbind "$0/nix" /nix
    export PATH="$1/sw/bin:$PATH"
    shift
    exec "$@"' "$TROOT" "$NIXOS_SYSTEM" "$@"
}

nixos_unmount() {
  [ -d "$TROOT" ] && umount -R "$TROOT" 2>/dev/null
  return 0
}

# ── pre-flight: everything that can refuse, while every disk is intact ──────
nixos_preflight() {
  NIXOS_SYSTEM=$(nixos_stamp nixos-system)
  NIXOS_PUBKEY=$(nixos_stamp nixos-cache.pub)
  NIXOS_CACHE="${RIME_NIXOS_CACHE:-$(nixos_stamp nixos-cache)}"
  [ -n "${RIME_NIXOS_STAMPS:-}" ] && log "nixos: TEST stamps from $RIME_NIXOS_STAMPS"
  [ -n "${RIME_NIXOS_CACHE:-}" ] && log "nixos: TEST cache $RIME_NIXOS_CACHE"
  [ "$NIXOS_CACHE" = iso ] && NIXOS_CACHE="file://${RIME_LIVE_MEDIUM:-/run/initramfs/live}/nix-cache"
  NIXOS_EFI_SRC="${RIME_NIXOS_EFI_SRC:-}"
  if [ -z "$NIXOS_EFI_SRC" ]; then
    NIXOS_EFI_SRC=$(dirname "$(ls /boot/efi/EFI/*/shimx64.efi 2>/dev/null | head -1)" 2>/dev/null || true)
  else
    log "nixos: TEST EFI loaders from $NIXOS_EFI_SRC"
  fi
  NIXOS_GRUB_STATIC="${RIME_NIXOS_GRUB_STATIC:-/usr/lib/bootupd/grub2-static}"

  # Partition mode needs an ESP shared with another OS and a root partition the
  # user picked; the NixOS layout (Rime's own ESP at /boot, three subvolumes)
  # is only written to a whole disk so far.
  [ "$INSTALL_MODE" = disk ] || die "This installer puts Rime OS on a whole disk only: installing it into one partition beside another system is not supported yet.

Nothing has been erased. Go back and choose \"Erase the whole disk\", or install
onto another disk."
  [ "$ROOTFS_TYPE" = btrfs ] || die "Rime OS installs on btrfs only (got '$ROOTFS_TYPE'). Nothing has been erased."

  printf '%s' "$NIXOS_SYSTEM" | grep -Eq '^/nix/store/[0-9a-z]{32}-[^/]+$' \
    || die "This ISO names no valid system to install ('$NIXOS_SYSTEM' in $NIXOS_STAMPS/nixos-system). Nothing has been erased. The ISO was built incorrectly; download it again."
  printf '%s' "$NIXOS_PUBKEY" | grep -Eq '^[A-Za-z0-9._-]+:[A-Za-z0-9+/]+={0,2}$' \
    || die "This ISO carries no valid key for its package cache ($NIXOS_STAMPS/nixos-cache.pub). Nothing has been erased. The ISO was built incorrectly; download it again."
  case "$NIXOS_CACHE" in
    https://*|file:///*) : ;;
    *) die "This ISO names no usable package cache ('$NIXOS_CACHE'). Nothing has been erased." ;;
  esac

  # ── the disk floor, by name ────────────────────────────────────────────────
  local sz
  sz=$(lsblk -bdno SIZE "$DISK" 2>/dev/null | tr -d ' ' || true)
  case "$sz" in
    ''|*[!0-9]*) die "Could not read the size of $DISK. Nothing has been erased." ;;
  esac
  [ "$NIXOS_MIN_BYTES" = "$((NIXOS_MIN_GB * 1000 * 1000 * 1000))" ] \
    || log "nixos: TEST disk floor $NIXOS_MIN_BYTES bytes (RIME_NIXOS_MIN_BYTES)"
  [ "$sz" -ge "$NIXOS_MIN_BYTES" ] \
    || die "$DISK is too small: $((sz / 1000000000)) GB. Rime OS needs a disk of at least $NIXOS_MIN_GB GB (the system, one update beside it and the previous version to go back to). Nothing has been erased."

  # ── timezone: required ─────────────────────────────────────────────────────
  # host.toml must name one: NixOS leaves /etc/localtime to it (base.nix sets
  # time.timeZone = null), so a machine without one runs on UTC by accident.
  NIXOS_TZ="${TIMEZONE:-}"
  if [ -z "$NIXOS_TZ" ] && [ -L /etc/localtime ]; then
    NIXOS_TZ=$(readlink /etc/localtime | sed 's|.*/zoneinfo/||')
    log "nixos: no timezone answered; using the live environment's $NIXOS_TZ"
  fi
  [ -n "$NIXOS_TZ" ] || die "No timezone was chosen, and Rime OS needs one. Nothing has been erased. Go back and pick your timezone."
  printf '%s' "$NIXOS_TZ" | grep -Eq '^[A-Za-z][A-Za-z0-9+_-]*(/[A-Za-z0-9+_-]+)*$' \
    || die "Invalid timezone '$NIXOS_TZ'. Nothing has been erased."
  if [ -d /usr/share/zoneinfo ] && [ ! -f "/usr/share/zoneinfo/$NIXOS_TZ" ]; then
    die "Unknown timezone '$NIXOS_TZ'. Nothing has been erased."
  fi

  # ── locale ─────────────────────────────────────────────────────────────────
  NIXOS_LANG="${LOCALE:-}"
  [ -n "$NIXOS_LANG" ] || NIXOS_LANG=$(sed -n 's/^LANG=//p' /etc/locale.conf 2>/dev/null | tr -d '"' | head -1)
  case "$NIXOS_LANG" in ""|C|C.UTF-8|C.utf8) NIXOS_LANG=en_US.UTF-8 ;; esac
  printf '%s' "$NIXOS_LANG" | grep -Eq '^[A-Za-z]{2,3}(_[A-Za-z]{2})?(\.(UTF-8|utf8))?(@[A-Za-z0-9]+)?$' \
    || die "Invalid locale '$NIXOS_LANG'. Nothing has been erased."

  # ── kernel arguments this machine's boot entries carry ─────────────────────
  NIXOS_KARGS=()
  local k
  for k in ${NIXOS_EXTRA_KARGS:-} ${RIME_EXTRA_KARGS:-}; do
    printf '%s' "$k" | grep -Eq "$NIXOS_SAFE_PARAM" \
      || die "Kernel argument '$k' is not one the boot menu can carry. Nothing has been erased."
    NIXOS_KARGS+=("$k")
  done
  [ -n "${RIME_EXTRA_KARGS:-}" ] && log "nixos: TEST kernel arguments: $RIME_EXTRA_KARGS"

  # ── early microcode: this CPU's vendor only ────────────────────────────────
  # host.toml [boot] microcode: the boot hook loads that vendor's microcode as its own initrd file (one copy on the
  # ESP for every generation). An unknown vendor keeps both (the hook's default).
  case "$(awk -F': *' '/^vendor_id/ { print $2; exit }' "${RIME_CPUINFO:-/proc/cpuinfo}" 2>/dev/null)" in
    GenuineIntel) NIXOS_MICROCODE=intel ;;
    AuthenticAMD) NIXOS_MICROCODE=amd ;;
    *) NIXOS_MICROCODE=both ;;
  esac
  log "nixos: early microcode for this machine: $NIXOS_MICROCODE"

  # ── tools and loaders ──────────────────────────────────────────────────────
  for k in nix unshare sgdisk wipefs partprobe udevadm mkfs.vfat mkfs.btrfs btrfs blkid findmnt; do
    have "$k" || die "$k is missing from this installer, so $DISK cannot be prepared. Nothing has been erased. The ISO was built incorrectly; download it again."
  done
  if [ "$ENCRYPT" = yes ]; then
    have cryptsetup || die "cryptsetup is missing from this installer. Nothing has been erased."
    # D9: "LUKS with a non-us keymap is refused by name until the keymap VM gate passes". The generic toplevel's
    # initrd carries the US console keymap only (rd.vconsole.keymap= names a map it does not have, and the prompt
    # stays US), so a passphrase typed on another layout would not open the disk at first boot. UNLOCK_KEYMAP is
    # rime-install's decision for the prompt (us when the layout cannot type the passphrase at all, as on bootc).
    # Lifted when the initrd ships every console keymap and the boot hook carries host.toml's (nix/tpmkeymap).
    if [ -n "${UNLOCK_KEYMAP:-}" ] && [ "$UNLOCK_KEYMAP" != us ]; then
      die "An encrypted install with the '$UNLOCK_KEYMAP' keyboard layout is not supported yet: the disk-unlock screen of this Rime OS uses the US layout only, so the passphrase you typed might not open the disk.

Nothing has been erased. Go back and either choose the US (English) keyboard layout, or install without encryption."
    fi
  fi
  for k in shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV; do
    [ -f "$NIXOS_EFI_SRC/$k" ] || die "This installer carries no $k (looked in ${NIXOS_EFI_SRC:-/boot/efi/EFI/*}). Nothing has been erased."
  done
  for k in grub-static-efi.cfg grub-static-pre.cfg configs.d; do
    [ -e "$NIXOS_GRUB_STATIC/$k" ] || die "This installer carries no static GRUB configuration ($NIXOS_GRUB_STATIC/$k). Nothing has been erased."
  done

  # ── the system is in the cache and signed, before the disk is touched ──────
  note "Checking that $NIXOS_SYSTEM is available …"
  if ! nixos_nix path-info --store "$NIXOS_CACHE" "$NIXOS_SYSTEM" >>"$LOG" 2>&1; then
    die "Rime OS could not be found in its package cache ($NIXOS_CACHE).

Nothing has been erased — your disks are untouched.
Check the network connection (a network install needs it), then try again."
  fi
  # Every path of the closure, signed by the stamped key itself (not merely by a key of the same name), before the
  # disk is touched: `nix copy` checks the same again, but only after the wipe.
  if ! nixos_nix store verify --store "$NIXOS_CACHE" --no-contents --sigs-needed 1 --recursive "$NIXOS_SYSTEM" >>"$LOG" 2>&1; then
    die "Rime OS in $NIXOS_CACHE is not signed by ${NIXOS_PUBKEY%%:*}, or its cache is incomplete. Nothing has been erased."
  fi
  log "nixos: system $NIXOS_SYSTEM in $NIXOS_CACHE, signed by ${NIXOS_PUBKEY%%:*}; tz=$NIXOS_TZ lang=$NIXOS_LANG kargs=${NIXOS_KARGS[*]:-}"
}

nixos_host_toml() {  # $1=file — host.toml for this machine (rime-hostlayer's schema)
  local f="$1" k list="" sep=""
  for k in "${NIXOS_KARGS[@]}"; do list="$list$sep\"$k\""; sep=", "; done
  mkdir -p "$(dirname "$f")" || return 1
  {
    printf '# Written by the Rime OS installer. The machine-specific part of the system\n'
    printf '# (PLAN.md D2): rime-hostlayer applies it at every activation, and the boot\n'
    printf '# hook reads [boot].\n\n'
    printf '[host]\nhostname = "%s"\ntimezone = "%s"\n\n' "$HOSTNAME" "$NIXOS_TZ"
    printf '[locale]\nLANG = "%s"\n\n' "$NIXOS_LANG"
    printf '[keyboard]\n'
    [ -n "${CONSOLE_KEYMAP:-}" ] && printf 'console = "%s"\n' "$CONSOLE_KEYMAP"
    printf 'xkb_layout = "%s"\n' "${KEYMAP:-us}"
    [ -n "${KEYVARIANT:-}" ] && printf 'xkb_variant = "%s"\n' "$KEYVARIANT"
    printf '\n[boot]\nlayout = "bls"\nmicrocode = "%s"\nkernel_params = [%s]\n' "${NIXOS_MICROCODE:-both}" "$list"
  } > "$f.tmp" && chmod 0644 "$f.tmp" && mv -f "$f.tmp" "$f"
}

nixos_esp() {  # $1=mounted ESP  $2=its FAT UUID — Fedora's shim + GRUB with bootupd's static config
  local d="$1" u="$2" x f
  mkdir -p "$d/EFI/fedora" "$d/EFI/BOOT" "$d/grub2" || return 1
  cp "$NIXOS_EFI_SRC"/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$d/EFI/fedora/" || return 1
  # The removable-media path, for firmware that keeps no boot entry. No
  # fbx64.efi: shim as BOOTX64.EFI would otherwise run fallback.
  cp "$NIXOS_EFI_SRC/shimx64.efi" "$d/EFI/BOOT/BOOTX64.EFI" || return 1
  cp "$NIXOS_EFI_SRC"/{grubx64.efi,mmx64.efi} "$d/EFI/BOOT/" || return 1
  for x in fedora BOOT; do
    cp "$NIXOS_GRUB_STATIC/grub-static-efi.cfg" "$d/EFI/$x/grub.cfg" || return 1
    printf 'set BOOT_UUID="%s"\n' "$u" > "$d/EFI/$x/bootuuid.cfg" || return 1
  done
  { cat "$NIXOS_GRUB_STATIC/grub-static-pre.cfg"
    for f in "$NIXOS_GRUB_STATIC"/configs.d/*.cfg; do
      echo "### BEGIN $(basename "$f") ###"; cat "$f"; echo "### END $(basename "$f") ###"
    done
  } > "$d/grub2/grub.cfg" || return 1
  printf 'set BOOT_UUID="%s"\n' "$u" > "$d/grub2/bootuuid.cfg"
}

nixos_fail() {  # $1=what failed — the disk has been written from here on
  log "nixos: FAILED: $1"
  die "$1 (see $LOG)."
}

# ── the install: everything from here on destroys $DISK ────────────────────
nixos_install() {
  local esp root dev esp_uuid groups g have_groups
  TARGET_DESC="${TARGET_DESC:-$DISK}"
  note "Installing Rime OS to $DISK … (full log: $LOG)"
  WROTE_DISK=1
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  wipefs -a "$DISK" >>"$LOG" 2>&1 || nixos_fail "Could not clear $DISK"
  sgdisk --zap-all "$DISK" >>"$LOG" 2>&1 || nixos_fail "Could not clear the partition table of $DISK"
  local rootname=rime-root
  [ "$ENCRYPT" = yes ] && rootname=rime-luks
  sgdisk -n "1:0:+${NIXOS_ESP_MIB}M" -t 1:ef00 -c "1:EFI System Partition" \
         -n 2:0:0 -t 2:8304 -c "2:$rootname" "$DISK" >>"$LOG" 2>&1 \
    || nixos_fail "Could not partition $DISK"
  partprobe "$DISK" >>"$LOG" 2>&1 || true
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  esp=$(part_dev "$DISK" 1); root=$(part_dev "$DISK" 2)
  [ -n "$esp" ] && [ -n "$root" ] || nixos_fail "The new partitions on $DISK did not appear"
  mkfs.vfat -F 32 -n "$NIXOS_ESP_LABEL" "$esp" >>"$LOG" 2>&1 || nixos_fail "Could not format the EFI partition $esp"

  dev="$root"
  if [ "$ENCRYPT" = yes ]; then
    note "Creating the encrypted volume on $root …"
    local fmt=(--type luks2 --pbkdf argon2id --label RIMEROOT --batch-mode --key-file -)
    if [ -n "${RIME_LUKS_PBKDF_MEMORY:-}" ]; then
      fmt+=(--pbkdf-memory "$RIME_LUKS_PBKDF_MEMORY" --pbkdf-parallel 1 --pbkdf-force-iterations 4)
      log "luks: WEAKENED key derivation in use (RIME_LUKS_PBKDF_MEMORY=$RIME_LUKS_PBKDF_MEMORY) — test builds only"
    fi
    printf '%s' "$LUKSPASS" | cryptsetup luksFormat "${fmt[@]}" "$root" >>"$LOG" 2>&1 \
      || nixos_fail "cryptsetup luksFormat failed on $root"
    LUKS_UUID=$(cryptsetup luksUUID "$root" 2>>"$LOG" || true)
    [ -n "$LUKS_UUID" ] || nixos_fail "The new LUKS2 header on $root has no UUID"
    LUKS_MAPPER="luks-$LUKS_UUID"
    printf '%s' "$LUKSPASS" | cryptsetup open --key-file - "$root" "$LUKS_MAPPER" >>"$LOG" 2>&1 \
      || nixos_fail "The new encrypted volume would not open with the passphrase just set"
    note "Creating the recovery key …"
    mkdir -p /run/rime-luks && chmod 700 /run/rime-luks
    local rfile=/run/rime-luks/recovery.key helper="${RIME_LUKS_ENROLL_LOCAL:-$LUKS_ENROLL_PATH}"
    rm -f "$rfile"
    printf '%s' "$LUKSPASS" | PASSWORD="$LUKSPASS" "$helper" --device "$root" --recovery-out "$rfile" >>"$LOG" 2>&1 \
      || nixos_fail "The recovery-key helper failed"
    [ -s "$rfile" ] || nixos_fail "The recovery-key helper produced no recovery key"
    local rkey
    rkey=$(tr -d '\r\n' < "$rfile")
    note "Checking that the passphrase and the recovery key both open the disk …"
    printf '%s' "$LUKSPASS" | cryptsetup open --test-passphrase --key-file - "$root" >>"$LOG" 2>&1 \
      || nixos_fail "The passphrase does not open the encrypted volume"
    printf '%s' "$rkey" | cryptsetup open --test-passphrase --key-file - "$root" >>"$LOG" 2>&1 \
      || nixos_fail "The recovery key does not open the encrypted volume"
    surface_recovery_key "$rkey"
    shred -u "$rfile" 2>/dev/null || rm -f "$rfile"
    rkey=""
    dev="/dev/mapper/$LUKS_MAPPER"
    # The generic system opens the volume the boot hook names from host.toml.
    # (A non-US unlock keymap was refused in nixos_preflight.)
    NIXOS_KARGS=("rd.luks.uuid=$LUKS_UUID" "${NIXOS_KARGS[@]}")
    log "luks: $root uuid=$LUKS_UUID mapper=$LUKS_MAPPER"
  fi

  mkfs.btrfs -f -L "$NIXOS_ROOT_LABEL" "$dev" >>"$LOG" 2>&1 || nixos_fail "mkfs.btrfs failed on $dev"
  mkdir -p "$TROOT"
  mount -o subvolid=5 "$dev" "$TROOT" >>"$LOG" 2>&1 || nixos_fail "Could not mount the new root filesystem"
  local s
  for s in $NIXOS_SUBVOLS; do
    btrfs subvolume create "$TROOT/$s" >>"$LOG" 2>&1 || nixos_fail "Could not create the subvolume $s"
  done
  umount "$TROOT" || nixos_fail "Could not unmount the new root filesystem"
  mount -o "subvol=@nixos,$NIXOS_MOUNT_OPTS" "$dev" "$TROOT" >>"$LOG" 2>&1 || nixos_fail "Could not mount @nixos"
  chmod 0755 "$TROOT"
  mkdir -p "$TROOT/nix" "$TROOT/var/home" "$TROOT/boot"
  mount -o "subvol=@nix,$NIXOS_MOUNT_OPTS" "$dev" "$TROOT/nix" >>"$LOG" 2>&1 || nixos_fail "Could not mount @nix"
  mount -o "subvol=@home,$NIXOS_MOUNT_OPTS" "$dev" "$TROOT/var/home" >>"$LOG" 2>&1 || nixos_fail "Could not mount @home"
  mount -o umask=0077 "$esp" "$TROOT/boot" >>"$LOG" 2>&1 || nixos_fail "Could not mount the EFI partition"
  ln -sfn /var/home "$TROOT/home"
  log "nixos: $(findmnt -R -no TARGET,SOURCE,FSTYPE,OPTIONS "$TROOT" | tr '\n' ';')"

  esp_uuid=$(blkid -s UUID -o value "$esp" 2>>"$LOG" || true)
  [ -n "$esp_uuid" ] || nixos_fail "The EFI partition has no UUID"
  nixos_esp "$TROOT/boot" "$esp_uuid" || nixos_fail "Could not write the boot loader to the EFI partition"
  nixos_host_toml "$TROOT/var/lib/rime/host.toml" || nixos_fail "Could not write host.toml"
  log "nixos: host.toml: $(tr '\n' ' ' < "$TROOT/var/lib/rime/host.toml")"

  note "Copying Rime OS onto the disk … (this is the long part)"
  nixos_nix copy --from "$NIXOS_CACHE" --to "local?root=$TROOT" "$NIXOS_SYSTEM" >>"$LOG" 2>&1 \
    || nixos_fail "Copying the system from $NIXOS_CACHE failed"
  log "nixos: copied; store on disk: $(du -sh --apparent-size "$TROOT/nix/store" 2>/dev/null | cut -f1) apparent, $(df -h --output=used "$TROOT/nix" | tail -1) used"

  note "Installing the boot menu …"
  nixos_in_target nixos-install --root "$TROOT" --system "$NIXOS_SYSTEM" \
      --no-channel-copy --no-root-passwd >>"$LOG" 2>&1 \
    || nixos_fail "nixos-install failed"
  [ -e "$TROOT/etc/NIXOS" ] || nixos_fail "nixos-install left no /etc/NIXOS"
  local gen
  gen=$(readlink "$TROOT/nix/var/nix/profiles/system" 2>/dev/null || true)
  [ -n "$gen" ] && [ "$(readlink "$TROOT/nix/var/nix/profiles/$gen")" = "$NIXOS_SYSTEM" ] \
    || nixos_fail "The system profile does not point at $NIXOS_SYSTEM"
  ls "$TROOT"/boot/loader/entries/*.conf >/dev/null 2>&1 || nixos_fail "The boot hook wrote no boot entry"
  INSTALLED_OK=1
  log "nixos: entries: $(ls "$TROOT/boot/loader/entries" | tr '\n' ' ')"

  note "Creating your account …"
  have_groups=$(cut -d: -f1 "$TROOT/etc/group" 2>/dev/null | tr '\n' ' ')
  groups=""
  for g in wheel networkmanager video audio input; do
    case " $have_groups " in *" $g "*) groups="$groups${groups:+,}$g" ;; esac
  done
  nixos_in_target nixos-enter --root "$TROOT" -- useradd -m -G "$groups" "$USERNAME" >>"$LOG" 2>&1 \
    || nixos_fail "Could not create the account $USERNAME"
  printf '%s:%s\n' "$USERNAME" "$P1" | nixos_in_target nixos-enter --root "$TROOT" -- chpasswd >>"$LOG" 2>&1 \
    || nixos_fail "Could not set the password of $USERNAME"
  grep -q "^$USERNAME:[^!*:]" "$TROOT/etc/shadow" || nixos_fail "The account $USERNAME has no password"
  log "nixos: account $USERNAME in $groups, home $(grep "^$USERNAME:" "$TROOT/etc/passwd" | cut -d: -f6)"
  copy_network_to "$TROOT"

  # The certificate the kernel is signed with, for a later `mokutil --import`.
  if [ -f "$NIXOS_STAMPS/nixos-mok.der" ]; then
    install -Dm0644 "$NIXOS_STAMPS/nixos-mok.der" "$TROOT/var/lib/rime/secureboot/rime-mok.der"
  fi

  # A firmware boot entry for shim, as bootupd makes for a bootc install, but
  # never for a loop-backed disk (BOOT-BREAKAGE-2026-09-20: that entry would
  # land in the firmware of the machine RUNNING the installer).
  if disk_is_loopback "$DISK"; then
    log "nvram: $DISK is loop-backed — no firmware entry"
  elif have efibootmgr && [ -d /sys/firmware/efi/efivars ]; then
    efibootmgr -q --create --disk "$DISK" --part 1 --label "Rime OS" --loader '\EFI\fedora\shimx64.efi' >>"$LOG" 2>&1 \
      && log "nvram: Rime OS -> $DISK part 1 \\EFI\\fedora\\shimx64.efi" \
      || log "nvram: efibootmgr failed; the removable path EFI/BOOT/BOOTX64.EFI still boots"
  fi

  sync
  nixos_unmount
  if [ -n "${LUKS_MAPPER:-}" ]; then cryptsetup close "$LUKS_MAPPER" >>"$LOG" 2>&1 || true; fi
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  return 0
}

# ── Secure Boot: the same choice the bootc path offers ─────────────────────
nixos_mok() {
  local cert="$NIXOS_STAMPS/nixos-mok.der" kstate sb="unknown" test
  kstate=$(nixos_stamp kernel-signed); [ -n "$kstate" ] || kstate=unknown
  have mokutil && sb=$(mokutil --sb-state 2>/dev/null | tr -d '\n' || true)
  if [ "$kstate" != signed ] || [ ! -f "$cert" ] || ! have mokutil; then
    note "Secure Boot: nothing to enrol (kernel: $kstate). Secure Boot is ${sb:-unknown}."
    return 0
  fi
  # Already enrolled (a second install on the same machine): nothing to queue,
  # and no blue screen on the next boot.
  test=$(mokutil --test-key "$cert" 2>&1 || true)
  case "$test" in
    *"already enrolled"*)
      log "secureboot: $cert is already enrolled ($test)"
      note "Secure Boot: the Rime key is already enrolled on this machine."
      return 0 ;;
  esac
  if [ -z "${MOKPW:-}" ]; then
    note "No MOK enrolment password was supplied — the Rime key was NOT enrolled. (Secure Boot is ${sb:-unknown}.)
To enrol it later: mokutil --import /var/lib/rime/secureboot/rime-mok.der"
    return 0
  fi
  if printf '%s\n%s\n' "$MOKPW" "$MOKPW" | mokutil --import "$cert" >>"$LOG" 2>&1; then
    log "secureboot: MOK enrolment queued"
    note "Secure Boot: enrolment queued. On the next boot, a blue 'MOK management' screen appears:

  Enroll MOK  →  Continue  →  Yes  →  enter that password  →  Reboot"
  else
    note "Secure Boot: mokutil could not queue the key (see $LOG). To enrol it later:
  mokutil --import /var/lib/rime/secureboot/rime-mok.der"
  fi
}

nixos_finish() {
  local crypt=""
  [ "$ENCRYPT" = yes ] && crypt="

This disk is ENCRYPTED: Rime OS asks for the disk passphrase every time it
starts. The RECOVERY KEY shown earlier works at the same prompt."
  note "Rime OS is installed.

Installed on $DISK (whole disk).$crypt

Log in as: $USERNAME
Hostname:  $HOSTNAME

Remove the USB stick and reboot.

Full log: $LOG"
  log "install complete (nixos $NIXOS_SYSTEM)"
  write_result ok ""
  echo "RIME-INSTALL-OK"
}
