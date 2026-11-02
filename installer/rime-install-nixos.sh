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
#  WHAT IT WRITES
#   whole disk (mode=disk):
#    GPT 1  EFI System Partition, FAT32 "RIME-ESP", 2 GiB, mounted at /boot.
#    GPT 2  btrfs "rime-root" — plain, or inside LUKS2 "RIMEROOT" (argon2id,
#           recovery key from rime-luks-enroll, both keys proven before the
#           system is copied).
#   one partition (mode=partition, unencrypted; docs/nixos/installer.md):
#    TARGET btrfs "rime-root". Nothing else on the disk is formatted.
#    ESP    Rime's OWN EFI System Partition (the one the Windows app makes, or
#           one the owner made): never Windows' (rime-boot keeps kernels on the
#           ESP at /boot and refuses one holding Windows Boot Manager, R20).
#           Relabelled "RIME-ESP" as the very last step.
#   both:
#    the ESP: Fedora's shim + GRUB (from this live env) at EFI/fedora and the
#           removable path EFI/BOOT (never over another loader there),
#           bootupd's static GRUB config, a Windows entry in grub2/custom.cfg
#           when Windows is beside it, and the BLS entries + kernels the
#           system's boot hook (rime-boot, layout bls) writes during
#           nixos-install. Subvolumes @nixos (/), @nix (/nix), @home
#           (/var/home), mounted compress-force=zstd:3.
#    /var/lib/rime/host.toml  hostname, timezone (required), locale, keyboard,
#           and [boot] layout=bls + microcode (the CPU's vendor) + kernel_params (rd.luks.uuid=…, and
#           rd.vconsole.keymap=us when the unlock prompt cannot use the owner's
#           layout). The boot hook reads [boot]; rime-hostlayer applies the rest.
#    /var/lib/rime/nix/{releases/<release>/,applied.json}  the release this
#           system was installed as, the way `rime update` records one.
#    /var/lib/rime/switch/rime-feature@gaming.target.on  (`steam`) when gaming
#           was chosen: the switch `rime install steam` writes, so the system
#           installed (the release's plain-gaming variant) and the one every
#           update selects (base + switched-on features) are the same.
#    /var/log/journal  created No_COW (chattr +C) before anything writes to it:
#           journald's first files are written before tmpfiles' `h ... +C`
#           runs, and +C on a directory only reaches files created after it.
#    the user, created imperatively (users.mutableUsers), in wheel.
#
#  BEFORE ANYTHING IS ERASED, the system is proven to be a SIGNED RELEASE
#  (docs/nixos/installer.md "Release check"): the release manifest carried on
#  the ISO is verified as `rime update` verifies one (cosign signature, Fulcio
#  chain, the nix-release.yml identity), the stamped system must be its
#  stamped variant, the system INSTALLED is the release's own toplevel of the
#  variant chosen (plain, or plain-gaming), so `rime trust --gate` passes on the
#  first boot, and every closure path is looked up in its cache in parallel and
#  bound to the manifest's NarHash (rime-install-nixos-release), then checked
#  for a signature by the trusted key itself (nix store verify).
#
#  STAMPS ($NIXOS_STAMPS, default /usr/lib/rime-installer; written at ISO build):
#    nixos-system             the toplevel store path to install
#    nixos-cache              the Rime binary cache URL, or `iso` = the cache on this ISO
#    nixos-cache.pub          its public key (name:base64); nothing unsigned by it is copied
#    nixos-upstream-cache     the cache of the paths the release takes from nixpkgs
#    nixos-upstream-cache.pub (cache.nixos.org), `iso`, or absent when one cache holds all
#    nixos-variant            the release variant the system is (default plain): the BASE an install gets
#    nixos-choices.json       the systems an install may choose (the base, and <base>-gaming when the owner
#                             chooses gaming) with the disk floor of each (nix/installer/choices.nix, which reads
#                             nix/release/floors.nix); every one of them must be a variant of the signed release
#    nixos-release/           the signed release record (manifest/, sig/, repo,
#                             fulcio-root.pem, signers, issuer)
#    nixos-mok.der            the certificate the kernel is signed with (optional)
#    kernel-signed            `signed` when that kernel is signed (as for bootc)
#
#  TEST SEAMS (environment only, each one logged when used):
#    RIME_NIXOS_STAMPS          another stamp directory
#    RIME_NIXOS_CACHE           another cache URL (http:// accepted here only)
#    RIME_NIXOS_UPSTREAM_CACHE  another upstream cache URL (likewise)
#    RIME_NIXOS_EFI_SRC         where shimx64.efi/grubx64.efi/mmx64.efi/BOOTX64.CSV are
#    RIME_NIXOS_GRUB_STATIC     bootupd's static GRUB config directory
#    RIME_NIXOS_MIN_BYTES       a lower disk floor (a test VM's disk)
#    RIME_NIXOS_PART_MIN_BYTES  a lower partition floor (likewise)
#    RIME_EXTRA_KARGS           extra kernel arguments (a lab's serial console)
# ─────────────────────────────────────────────────────────────────────────────

NIXOS_STAMPS="${RIME_NIXOS_STAMPS:-/usr/lib/rime-installer}"
NIXOS=0
[ -r "$NIXOS_STAMPS/nixos-system" ] && NIXOS=1
# A NixOS ISO that fetches its system at install time is a network install too
# (build-live-iso.sh writes the netinstall marker, which the GUI reads for its
# network page), but none of the bootc netinstall steps (the pinned image
# digest, skopeo, the registry) apply: they are switched off here and the NixOS
# path checks the network for its own cache (nixos_net_check).
if [ "$NIXOS" = 1 ]; then
  NETINSTALL=0
fi

# The disk floors (closure.md A11) are NOT written here: they come from the ISO's nixos-choices.json, generated from
# nix/release/floors.nix, the one place `rime update`'s space gate and `rime install steam` read them from too
# (nixos_choose). A floor is a fresh system and ONE mass-rebuild update staged beside it plus the gate's reserve, as
# that gate counts them: below it the machine installs but its first update is refused, so it is refused here
# instead, before anything is erased. Decimal gigabytes, like the disk sizes printed on the box.
NIXOS_ESP_MIB=2048
NIXOS_ESP_LABEL=RIME-ESP
NIXOS_ROOT_LABEL=rime-root
NIXOS_SUBVOLS="@nixos @nix @home"
NIXOS_MOUNT_OPTS="compress-force=zstd:3"
# rime-boot's own rule for a kernel parameter (nix/pkgs/rime-boot SAFE_PARAM), the same character set written
# as a POSIX bracket expression: `]` first and `-` last, because `\]` inside brackets ends them in a POSIX ERE (the
# Python spelling, copied verbatim, refused every parameter, `console=tty0` included). Matched with bash's [[ =~ ]],
# which compiles the same POSIX ERE.
NIXOS_SAFE_PARAM='^[][A-Za-z0-9_./:=,+@%~^!-]+$'
# The release check's bounds (docs/nixos/installer.md "Release check": a 4,000-path closure is 15-40 s of narinfo
# lookups on cache.nixos.org, measured 2026-11-02): the whole lookup, then each `nix store verify` group.
NIXOS_CHECK_DEADLINE=300
NIXOS_VERIFY_DEADLINE=600
NIXOS_WORK=/run/rime-installer/nixos
# rime-boot's reserve on /boot (nix/pkgs/rime-boot reserveMiB) plus room for shim, GRUB and MokManager.
NIXOS_ESP_SLACK=$(( (32 + 16) * 1024 * 1024 ))

# An absent stamp is empty (the redirection's own error is silenced too: `cmd <f 2>/dev/null` opens f first).
nixos_stamp() { { tr -d '\r\n' < "$NIXOS_STAMPS/$1"; } 2>/dev/null || true; }

nixos_nix() {  # nix with the release's keys as the ONLY trusted ones, never a build user
  nix --extra-experimental-features nix-command \
      --option build-users-group '' \
      --option require-sigs true \
      --option trusted-public-keys "$NIXOS_TRUSTED" \
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

nixos_cache_url() {  # $1=stamped value  $2=seam value  $3=what (for the log) — prints the URL to use
  local v="$1"
  if [ -n "$2" ]; then v="$2"; log "nixos: TEST $3 $2"; fi
  [ "$v" = iso ] && v="file://${RIME_LIVE_MEDIUM:-/run/initramfs/live}/nix-cache"
  printf '%s' "$v"
}

nixos_cache_ok() {  # $1=URL  $2=seam value — https and file:///, and http:// only through a TEST seam
  case "$1" in
    https://*|file:///*) return 0 ;;
    http://*) [ -n "$2" ] ;;
    *) return 1 ;;
  esac
}

# Which layer is missing, before saying "the cache could not be reached" (the
# bootc netinstall's net_diagnose, for the cache's own host).
nixos_net_diagnose() {  # $1=URL
  local host="${1#*://}"
  host="${host%%/*}"; host="${host%%:*}"
  if [ -z "$(ip route show default 2>/dev/null)" ]; then echo no-route; return; fi
  if ! getent hosts "$host" >/dev/null 2>&1; then echo "no-dns $host"; return; fi
  echo ok
}

nixos_net_check() {  # $1=URL — waits for a just-connected link, then refuses by the layer that is missing
  case "$1" in file://*) return 0 ;; esac
  local v="" w
  for w in 1 2 3 4 5 6 7 8 9 10; do
    v=$(nixos_net_diagnose "$1")
    [ "$v" = ok ] && break
    sleep 2
  done
  case "$v" in
    ok) log "nixos: network: a route and a name for ${1%%/nix*}" ;;
    no-route) die "This machine is not connected to any network, and this installer downloads Rime OS while it installs.

Nothing has been erased — your disks are untouched.
Connect to Wi-Fi or plug in a network cable, then try again." ;;
    *) die "This machine is connected to a network, but cannot look up ${v#no-dns } (no DNS), so Rime OS cannot be downloaded.

Nothing has been erased — your disks are untouched.
Check the network (a captive portal, a DNS setting), then try again." ;;
  esac
}

nixos_checker() {  # the release checker this installer carries (never one from the cache)
  local c
  for c in /usr/lib/rime-installer/rime-install-nixos-release.py "$(dirname "$0")/rime-install-nixos-release.py"; do
    [ -r "$c" ] && { printf '%s' "$c"; return 0; }
  done
  return 1
}

# Which device of DISK's would be wiped (whole-disk mode), or which one is the
# target (partition mode), may carry these labels. Any OTHER device with them
# would win /dev/disk/by-label at boot, and the installed system would mount
# its root or /boot from the wrong disk.
nixos_label_clash() {  # $1=label  $2=the device allowed to carry it in partition mode
  local dev parent
  while read -r dev; do
    [ -n "$dev" ] || continue
    if [ "$INSTALL_MODE" = partition ]; then
      [ "$dev" = "$2" ] && continue
    else
      parent=$(lsblk -dno PKNAME "$dev" 2>/dev/null | head -1 | tr -d ' ')
      [ "$dev" = "$DISK" ] || [ "/dev/$parent" = "$DISK" ] && continue
    fi
    die "$dev already carries the label \"$1\", which Rime OS uses to find its own partitions at boot: with two of them the installed system could start from the wrong one.

Nothing has been erased — your disks are untouched.
Erase or relabel $dev (an earlier Rime OS install?), or disconnect it, then try again."
  done < <(lsblk -rpno NAME,LABEL 2>/dev/null | awk -v l="$1" '$2 == l { print $1 }')
}

# The ESP partition mode installs to, read-only: Windows Boot Manager on it?
# How much room? Another loader at the removable path?
nixos_esp_probe() {
  local m own=0
  NIXOS_ESP_WINDOWS=0; NIXOS_ESP_FREE=0
  if [ "${ESP_IS_LIVE:-0}" = 1 ]; then
    m=/run/initramfs/live
  else
    m=$(mktemp -d) || return 1
    own=1
    mount -o ro "$ESP_DEV" "$m" >>"$LOG" 2>&1 || { rmdir "$m"; return 1; }
  fi
  # FAT lookups ignore case, so this finds EFI/Microsoft/Boot/bootmgfw.efi however it is spelt.
  [ -f "$m/EFI/Microsoft/Boot/bootmgfw.efi" ] && NIXOS_ESP_WINDOWS=1
  NIXOS_ESP_FREE=$(df -B1 --output=avail "$m" 2>/dev/null | tail -1 | tr -d ' ')
  if [ "$own" = 1 ]; then umount "$m" 2>/dev/null; rmdir "$m" 2>/dev/null; fi
  case "$NIXOS_ESP_FREE" in ''|*[!0-9]*) return 1 ;; esac
  return 0
}

# Which system this machine gets and the floors it must meet (A4: the ISO's base variant, plain, unless the owner
# chose gaming: then <base>-gaming), from the ISO's nixos-choices.json (nix/installer/choices.nix, generated from
# nix/release/floors.nix). Sets NIXOS_GAMING, NIXOS_INSTALL_VARIANT, NIXOS_MIN_GB, NIXOS_PART_MIN_GB (and their
# bytes), NIXOS_BASE_MIN_GB / NIXOS_BASE_PART_MIN_GB (for the refusal's hint) and NIXOS_SWITCHES ("FILE NAME,…" per
# feature the chosen system carries: the switch `rime install` writes for it).
nixos_choose() {
  local f="$NIXOS_STAMPS/nixos-choices.json" out line re
  case "${GAMING:-no}" in
    yes|no) NIXOS_GAMING="${GAMING:-no}" ;;
    *) die "answers: bad gaming '$GAMING' (expected yes or no). Nothing has been erased." ;;
  esac
  [ -r "$f" ] || die "This ISO carries no list of the systems it installs and their disk floors ($f). Nothing has been erased. The ISO was built incorrectly; download it again."
  out=$(python3 -I - "$f" "$NIXOS_VARIANT" "$NIXOS_GAMING" 2>>"$LOG" <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))
base, gaming = sys.argv[2], sys.argv[3] == "yes"
vs = c["variants"]
if base not in vs:
    print("nobase " + base)
    sys.exit(0)
name = base + "-gaming" if gaming else base
if name not in vs:
    print("none " + name)
    sys.exit(0)
print("base", int(vs[base]["disk_gb"]), int(vs[base]["partition_gb"]))
print("variant", name, int(vs[name]["disk_gb"]), int(vs[name]["partition_gb"]))
for feat in vs[name]["features"]:
    s = c["features"][feat]
    print("switch", s["switch"], ",".join(s["names"]))
PY
  ) || die "This ISO's list of systems ($f) could not be read. Nothing has been erased. The ISO was built incorrectly; download it again."
  NIXOS_SWITCHES=(); NIXOS_INSTALL_VARIANT=""
  NIXOS_MIN_GB=""; NIXOS_PART_MIN_GB=""; NIXOS_BASE_MIN_GB=""; NIXOS_BASE_PART_MIN_GB=""
  re='^/var/lib/rime/switch/rime-feature@[a-z0-9-]+\.target\.on [a-z0-9][a-z0-9.+-]*(,[a-z0-9][a-z0-9.+-]*)*$'
  while read -r line; do
    case "$line" in
      "nobase "*) die "This ISO's list of systems does not name its own system ${line#nobase }. Nothing has been erased. The ISO was built incorrectly; download it again." ;;
      "none "*) die "This ISO does not offer Rime OS with gaming (its release has no ${line#none } system). Nothing has been erased.
Leave gaming off: Steam can be added after the install with \`rime install steam\`." ;;
      "base "*) read -r NIXOS_BASE_MIN_GB NIXOS_BASE_PART_MIN_GB <<<"${line#base }" ;;
      "variant "*) read -r NIXOS_INSTALL_VARIANT NIXOS_MIN_GB NIXOS_PART_MIN_GB <<<"${line#variant }" ;;
      "switch "*)
        [[ ${line#switch } =~ $re ]] || die "This ISO's list of systems names a feature switch Rime OS does not use ('${line#switch }'). Nothing has been erased."
        NIXOS_SWITCHES+=("${line#switch }") ;;
    esac
  done <<<"$out"
  re='^[a-z][a-z0-9-]{0,63}$'
  [[ $NIXOS_INSTALL_VARIANT =~ $re ]] && [[ $NIXOS_MIN_GB =~ ^[0-9]+$ ]] && [[ $NIXOS_PART_MIN_GB =~ ^[0-9]+$ ]] \
    && [[ $NIXOS_BASE_MIN_GB =~ ^[0-9]+$ ]] && [[ $NIXOS_BASE_PART_MIN_GB =~ ^[0-9]+$ ]] \
    || die "This ISO's list of systems ($f) names no floor for ${NIXOS_INSTALL_VARIANT:-its system}. Nothing has been erased. The ISO was built incorrectly; download it again."
  NIXOS_MIN_BYTES="${RIME_NIXOS_MIN_BYTES:-$((NIXOS_MIN_GB * 1000 * 1000 * 1000))}"
  NIXOS_PART_MIN_BYTES="${RIME_NIXOS_PART_MIN_BYTES:-$((NIXOS_PART_MIN_GB * 1000 * 1000 * 1000))}"
  [ -z "${RIME_NIXOS_MIN_BYTES:-}" ] || log "nixos: TEST disk floor $NIXOS_MIN_BYTES bytes (RIME_NIXOS_MIN_BYTES)"
  [ -z "${RIME_NIXOS_PART_MIN_BYTES:-}" ] || log "nixos: TEST partition floor $NIXOS_PART_MIN_BYTES bytes (RIME_NIXOS_PART_MIN_BYTES)"
  log "nixos: system $NIXOS_INSTALL_VARIANT (gaming=$NIXOS_GAMING): floors ${NIXOS_MIN_GB} GB disk, ${NIXOS_PART_MIN_GB} GB partition; switches: ${NIXOS_SWITCHES[*]:-none}"
}

nixos_floor_hint() {  # $1=disk|partition — what to do when only the gaming system does not fit
  [ "$NIXOS_GAMING" = yes ] || return 0
  local gb="$NIXOS_BASE_MIN_GB"; [ "$1" = partition ] && gb="$NIXOS_BASE_PART_MIN_GB"
  printf ' Without gaming Rime OS needs %s GB: go back and leave gaming off (Steam can be added later with `rime install steam` on a bigger disk), or choose a bigger %s.' "$gb" "$1"
}

# ── pre-flight: everything that can refuse, while every disk is intact ──────
nixos_preflight() {
  NIXOS_SYSTEM=$(nixos_stamp nixos-system)
  NIXOS_PUBKEY=$(nixos_stamp nixos-cache.pub)
  NIXOS_UP_PUBKEY=$(nixos_stamp nixos-upstream-cache.pub)
  NIXOS_VARIANT=$(nixos_stamp nixos-variant); : "${NIXOS_VARIANT:=plain}"
  NIXOS_RELEASE_DIR="$NIXOS_STAMPS/nixos-release"
  [ -n "${RIME_NIXOS_STAMPS:-}" ] && log "nixos: TEST stamps from $RIME_NIXOS_STAMPS"
  NIXOS_CACHE=$(nixos_cache_url "$(nixos_stamp nixos-cache)" "${RIME_NIXOS_CACHE:-}" cache)
  NIXOS_UP_CACHE=""
  if [ -n "$NIXOS_UP_PUBKEY" ] || [ -n "${RIME_NIXOS_UPSTREAM_CACHE:-}" ]; then
    NIXOS_UP_CACHE=$(nixos_cache_url "$(nixos_stamp nixos-upstream-cache)" "${RIME_NIXOS_UPSTREAM_CACHE:-}" "upstream cache")
  fi
  NIXOS_EFI_SRC="${RIME_NIXOS_EFI_SRC:-}"
  if [ -z "$NIXOS_EFI_SRC" ]; then
    NIXOS_EFI_SRC=$(dirname "$(ls /boot/efi/EFI/*/shimx64.efi 2>/dev/null | head -1)" 2>/dev/null || true)
  else
    log "nixos: TEST EFI loaders from $NIXOS_EFI_SRC"
  fi
  NIXOS_GRUB_STATIC="${RIME_NIXOS_GRUB_STATIC:-/usr/lib/bootupd/grub2-static}"

  case "$INSTALL_MODE" in
    disk|partition) : ;;
    *) die "Unknown install mode '$INSTALL_MODE'. Nothing has been erased." ;;
  esac
  [ "$ROOTFS_TYPE" = btrfs ] || die "Rime OS installs on btrfs only (got '$ROOTFS_TYPE'). Nothing has been erased."

  # Every value checked below is matched whole with [[ =~ ]], the pattern in a variable (unquoted on the right, so
  # it is a regex), never piped into grep: under pipefail a grep that stops reading early fails its producer.
  local re k
  re='^/nix/store/[0-9a-z]{32}-[^/]+$'
  [[ $NIXOS_SYSTEM =~ $re ]] \
    || die "This ISO names no valid system to install ('$NIXOS_SYSTEM' in $NIXOS_STAMPS/nixos-system). Nothing has been erased. The ISO was built incorrectly; download it again."
  re='^[A-Za-z0-9._-]+:[A-Za-z0-9+/]+={0,2}$'
  for k in "$NIXOS_PUBKEY" ${NIXOS_UP_CACHE:+"$NIXOS_UP_PUBKEY"}; do
    [[ $k =~ $re ]] \
      || die "This ISO carries no valid key for its package cache ($NIXOS_STAMPS/nixos-cache.pub or nixos-upstream-cache.pub). Nothing has been erased. The ISO was built incorrectly; download it again."
  done
  nixos_cache_ok "$NIXOS_CACHE" "${RIME_NIXOS_CACHE:-}" \
    || die "This ISO names no usable package cache ('$NIXOS_CACHE'). Nothing has been erased."
  if [ -n "$NIXOS_UP_CACHE" ]; then
    nixos_cache_ok "$NIXOS_UP_CACHE" "${RIME_NIXOS_UPSTREAM_CACHE:-}" \
      || die "This ISO names no usable upstream package cache ('$NIXOS_UP_CACHE'). Nothing has been erased."
  fi
  NIXOS_TRUSTED="$NIXOS_PUBKEY${NIXOS_UP_CACHE:+ $NIXOS_UP_PUBKEY}"
  re='^[a-z][a-z0-9-]{0,31}$'
  [[ $NIXOS_VARIANT =~ $re ]] \
    || die "This ISO names no valid system variant ('$NIXOS_VARIANT'). Nothing has been erased."
  [ -d "$NIXOS_RELEASE_DIR" ] \
    || die "This ISO carries no signed release record for the system it installs ($NIXOS_RELEASE_DIR), so it cannot prove that system is a Rime OS release.

Nothing has been erased. The ISO was built incorrectly; download it again."

  # ── which system, and its disk floor, by name ──────────────────────────────
  nixos_choose
  local sz what
  if [ "$NIXOS_GAMING" = yes ]; then what="Rime OS with gaming (Steam)"; else what="Rime OS"; fi
  sz=$(lsblk -bdno SIZE "$DISK" 2>/dev/null | tr -d ' ' || true)
  case "$sz" in
    ''|*[!0-9]*) die "Could not read the size of $DISK. Nothing has been erased." ;;
  esac
  if [ "$INSTALL_MODE" = disk ]; then
    [ "$sz" -ge "$NIXOS_MIN_BYTES" ] \
      || die "$DISK is too small for $what: $((sz / 1000000000)) GB. $what needs a disk of at least $NIXOS_MIN_GB GB (the system and one update beside it).$(nixos_floor_hint disk) Nothing has been erased."
  else
    # rime-install's generic partition guards ran first: TARGET is a partition
    # of DISK, ESP_DEV an EFI System Partition on DISK, encryption refused.
    sz=$(lsblk -bdno SIZE "$TARGET" 2>/dev/null | tr -d ' ' || true)
    case "$sz" in ''|*[!0-9]*) die "Could not read the size of $TARGET. Nothing has been erased." ;; esac
    [ "$sz" -ge "$NIXOS_PART_MIN_BYTES" ] \
      || die "$TARGET is too small for $what: $((sz / 1000000000)) GB. $what needs a partition of at least $NIXOS_PART_MIN_GB GB (the system and one update beside it).$(nixos_floor_hint partition) Nothing has been erased."
    nixos_esp_probe || die "Could not read the EFI System Partition $ESP_DEV. Nothing has been erased."
    [ "$NIXOS_ESP_WINDOWS" = 0 ] || die "$ESP_DEV is Windows' EFI System Partition (it holds Windows Boot Manager). Rime OS keeps its kernels on its own EFI System Partition and does not put them beside Windows' boot files.

Nothing has been erased — your disks are untouched.
Install from Windows with the Rime OS installer app (it makes Rime's own EFI partition), or create a separate EFI System Partition of at least 1 GB for Rime and choose that one."
  fi

  # ── timezone: required ─────────────────────────────────────────────────────
  # host.toml must name one: NixOS leaves /etc/localtime to it (base.nix sets
  # time.timeZone = null), so a machine without one runs on UTC by accident.
  NIXOS_TZ="${TIMEZONE:-}"
  if [ -z "$NIXOS_TZ" ] && [ -L /etc/localtime ]; then
    NIXOS_TZ=$(readlink /etc/localtime | sed 's|.*/zoneinfo/||')
    log "nixos: no timezone answered; using the live environment's $NIXOS_TZ"
  fi
  [ -n "$NIXOS_TZ" ] || die "No timezone was chosen, and Rime OS needs one. Nothing has been erased. Go back and pick your timezone."
  re='^[A-Za-z][A-Za-z0-9+_-]*(/[A-Za-z0-9+_-]+)*$'
  [[ $NIXOS_TZ =~ $re ]] || die "Invalid timezone '$NIXOS_TZ'. Nothing has been erased."
  if [ -d /usr/share/zoneinfo ] && [ ! -f "/usr/share/zoneinfo/$NIXOS_TZ" ]; then
    die "Unknown timezone '$NIXOS_TZ'. Nothing has been erased."
  fi

  # ── locale ─────────────────────────────────────────────────────────────────
  NIXOS_LANG="${LOCALE:-}"
  [ -n "$NIXOS_LANG" ] || NIXOS_LANG=$(sed -n 's/^LANG=//p' /etc/locale.conf 2>/dev/null | tr -d '"' | head -1)
  case "$NIXOS_LANG" in ""|C|C.UTF-8|C.utf8) NIXOS_LANG=en_US.UTF-8 ;; esac
  re='^[A-Za-z]{2,3}(_[A-Za-z]{2})?(\.(UTF-8|utf8))?(@[A-Za-z0-9]+)?$'
  [[ $NIXOS_LANG =~ $re ]] || die "Invalid locale '$NIXOS_LANG'. Nothing has been erased."

  # ── kernel arguments this machine's boot entries carry ─────────────────────
  NIXOS_KARGS=()
  for k in ${NIXOS_EXTRA_KARGS:-} ${RIME_EXTRA_KARGS:-}; do
    [[ $k =~ $NIXOS_SAFE_PARAM ]] \
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
  for k in nix python3 openssl timeout xargs unshare sgdisk wipefs partprobe udevadm mkfs.vfat mkfs.btrfs btrfs chattr blkid findmnt lsblk; do
    have "$k" || die "$k is missing from this installer, so $DISK cannot be prepared. Nothing has been erased. The ISO was built incorrectly; download it again."
  done
  if [ "$INSTALL_MODE" = partition ]; then
    have fatlabel || have mlabel \
      || die "fatlabel is missing from this installer, so the EFI partition cannot be named for Rime OS. Nothing has been erased."
  fi
  NIXOS_CHECKER=$(nixos_checker) \
    || die "This installer carries no release checker (rime-install-nixos-release.py). Nothing has been erased. The ISO was built incorrectly; download it again."
  if [ "$ENCRYPT" = yes ]; then
    have cryptsetup || die "cryptsetup is missing from this installer. Nothing has been erased."
    # The initrd carries every console keymap (nix/modules/keymap.nix) and the
    # boot hook puts host.toml's [keyboard] console on every entry
    # (rd.vconsole.keymap=, rime-boot keymap_params). When rime-install decided
    # the unlock prompt needs another layout (UNLOCK_KEYMAP: us, because the
    # owner's layout cannot type the passphrase), that one is named in [boot]
    # kernel_params below, and a kernel_params keymap wins over [keyboard].
    log "nixos: unlock keymap ${UNLOCK_KEYMAP:-us} (console keymap ${CONSOLE_KEYMAP:-us})"
  fi
  for k in shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV; do
    [ -f "$NIXOS_EFI_SRC/$k" ] || die "This installer carries no $k (looked in ${NIXOS_EFI_SRC:-/boot/efi/EFI/*}). Nothing has been erased."
  done
  for k in grub-static-efi.cfg grub-static-pre.cfg configs.d; do
    [ -e "$NIXOS_GRUB_STATIC/$k" ] || die "This installer carries no static GRUB configuration ($NIXOS_GRUB_STATIC/$k). Nothing has been erased."
  done

  # ── no other disk may answer to Rime's labels ──────────────────────────────
  nixos_label_clash "$NIXOS_ESP_LABEL" "${ESP_DEV:-}"
  nixos_label_clash "$NIXOS_ROOT_LABEL" "${TARGET:-}"

  # ── the network, for a cache that is fetched from ──────────────────────────
  nixos_net_check "$NIXOS_CACHE"
  [ -z "$NIXOS_UP_CACHE" ] || nixos_net_check "$NIXOS_UP_CACHE"

  # ── the system is a signed release, in its caches, before a disk is touched ─
  nixos_release_check
  if [ "$INSTALL_MODE" = partition ]; then
    local need=$(( NIXOS_BOOT_BYTES + NIXOS_ESP_SLACK ))
    [ "$NIXOS_ESP_FREE" -ge "$need" ] \
      || die "The EFI System Partition $ESP_DEV has $((NIXOS_ESP_FREE / 1048576)) MB free, and Rime OS needs $((need / 1048576)) MB there for its kernel and boot loader. Nothing has been erased."
  fi
  log "nixos: system $NIXOS_SYSTEM = release $NIXOS_RELEASE $NIXOS_INSTALL_VARIANT ($NIXOS_DIGEST); mode=$INSTALL_MODE tz=$NIXOS_TZ lang=$NIXOS_LANG kargs=${NIXOS_KARGS[*]:-}"
}

# The release check (docs/nixos/installer.md "Release check"). Sets
# NIXOS_RELEASE, NIXOS_DIGEST, NIXOS_BOOT_BYTES and leaves the verified
# manifest and the per-cache path lists in $NIXOS_WORK/release.
nixos_release_check() {
  local out rc=0 sum
  note "Checking that $NIXOS_SYSTEM is a signed Rime OS release …"
  rm -rf "$NIXOS_WORK/release"; mkdir -p "$NIXOS_WORK"
  out=$(python3 -I "$NIXOS_CHECKER" check --release-dir "$NIXOS_RELEASE_DIR" --system "$NIXOS_SYSTEM" \
          --variant "$NIXOS_VARIANT" --install-variant "$NIXOS_INSTALL_VARIANT" \
          --out "$NIXOS_WORK/release" --cache "$NIXOS_CACHE" --key "$NIXOS_PUBKEY" \
          ${NIXOS_UP_CACHE:+--upstream-cache "$NIXOS_UP_CACHE" --upstream-key "$NIXOS_UP_PUBKEY"} \
          --deadline "$NIXOS_CHECK_DEADLINE" 2>>"$LOG") || rc=$?
  log "nixos: release check rc=$rc: $out"
  case "$rc" in
    0) : ;;
    2) die "The package cache did not answer while Rime OS was being checked (${out#unreachable: }).

Nothing has been erased — your disks are untouched.
Check the network connection, then try again." ;;
    *) die "This ISO's copy of Rime OS failed its release check: ${out#refused: }.

Nothing has been erased — your disks are untouched.
The ISO or its package cache is not a Rime OS release; download the ISO again from rimeos.com." ;;
  esac
  sum="$NIXOS_WORK/release/summary.json"
  NIXOS_RELEASE=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["release"])' "$sum")
  NIXOS_DIGEST=$(cat "$NIXOS_WORK/release/digest")
  NIXOS_BOOT_BYTES=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["boot_bytes"])' "$sum")
  # From here on the system is the one INSTALLED: the signed release's own toplevel of the chosen variant (the
  # stamped one, or its gaming sibling), never a path the ISO names by itself. `rime trust --gate` accepts a running
  # system only when it is a variant toplevel of the release recorded in applied.json, which is this one.
  local top re='^/nix/store/[0-9a-z]{32}-[^/]+$'
  top=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["toplevel"])' "$sum")
  [[ $top =~ $re ]] || die "The release check named no system to install ('$top'). Nothing has been erased."
  [ "$top" = "$NIXOS_SYSTEM" ] || log "nixos: installing release $NIXOS_RELEASE's $NIXOS_INSTALL_VARIANT system $top (the ISO's $NIXOS_VARIANT system is $NIXOS_SYSTEM)"
  NIXOS_SYSTEM="$top"
  # Every path, signed by the trusted key ITSELF (not merely by a key of the same name, which is all a narinfo's
  # Sig line names), per cache, both caches at once and bounded: `nix copy` checks the same again, but only after
  # the wipe.
  local pr="" pu="" fail="" t0 t1
  t0=$(date +%s%N)
  nixos_verify "$NIXOS_CACHE" "$NIXOS_PUBKEY" "$NIXOS_WORK/release/rime.paths" & pr=$!
  if [ -s "$NIXOS_WORK/release/upstream.paths" ]; then
    nixos_verify "$NIXOS_UP_CACHE" "$NIXOS_UP_PUBKEY" "$NIXOS_WORK/release/upstream.paths" & pu=$!
  fi
  wait "$pr" || fail="${NIXOS_PUBKEY%%:*} ($NIXOS_CACHE)"
  if [ -n "$pu" ]; then wait "$pu" || fail="${fail:+$fail and }${NIXOS_UP_PUBKEY%%:*} ($NIXOS_UP_CACHE)"; fi
  [ -z "$fail" ] || die "Rime OS release $NIXOS_RELEASE in its package cache is not signed by $fail, or the cache did not answer in time. Nothing has been erased."
  t1=$(date +%s%N)
  log "nixos: release $NIXOS_RELEASE verified: $(tr -d '\n' < "$sum") store-verify-seconds=$(( (t1 - t0) / 1000000 / 1000 )).$(printf '%03d' $(( (t1 - t0) / 1000000 % 1000 )))"
}

nixos_verify() {  # $1=cache  $2=its key  $3=path list
  timeout "$NIXOS_VERIFY_DEADLINE" xargs -a "$3" \
    nix --extra-experimental-features nix-command --option build-users-group '' --option require-sigs true \
        --option trusted-public-keys "$2" --option substituters '' \
        store verify --store "$1" --no-contents --sigs-needed 1 >>"$LOG" 2>&1
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
  # A Fedora-family neighbour's \EFI\fedora is rewritten below: its originals go to ESP_BACKUP_DIR first, as on
  # the bootc path (cp -n keeps the TRUE originals an earlier install backed up).
  if [ "${ESP_SHARED_FEDORA:-0}" = 1 ] && [ -d "$d/EFI/fedora" ]; then
    mkdir -p "$d/$ESP_BACKUP_DIR"
    for f in grub.cfg bootuuid.cfg shimx64.efi grubx64.efi; do
      [ -f "$d/EFI/fedora/$f" ] && cp -n "$d/EFI/fedora/$f" "$d/$ESP_BACKUP_DIR/$f"
    done
    log "esp: backed up the neighbour's EFI/fedora to $ESP_BACKUP_DIR: $(ls "$d/$ESP_BACKUP_DIR" | tr '\n' ' ')"
  fi
  mkdir -p "$d/EFI/fedora" "$d/grub2" || return 1
  cp "$NIXOS_EFI_SRC"/{shimx64.efi,grubx64.efi,mmx64.efi,BOOTX64.CSV} "$d/EFI/fedora/" || return 1
  cp "$NIXOS_GRUB_STATIC/grub-static-efi.cfg" "$d/EFI/fedora/grub.cfg" || return 1
  printf 'set BOOT_UUID="%s"\n' "$u" > "$d/EFI/fedora/bootuuid.cfg" || return 1
  # The removable-media path, for firmware that keeps no boot entry. No
  # fbx64.efi: shim as BOOTX64.EFI would otherwise run fallback. Never over
  # ANOTHER loader there (an ESP Rime shares in partition mode): only when the
  # path is free or already holds this shim.
  if [ ! -e "$d/EFI/BOOT/BOOTX64.EFI" ] || cmp -s "$d/EFI/BOOT/BOOTX64.EFI" "$NIXOS_EFI_SRC/shimx64.efi"; then
    mkdir -p "$d/EFI/BOOT" || return 1
    cp "$NIXOS_EFI_SRC/shimx64.efi" "$d/EFI/BOOT/BOOTX64.EFI" || return 1
    cp "$NIXOS_EFI_SRC"/{grubx64.efi,mmx64.efi} "$d/EFI/BOOT/" || return 1
    cp "$NIXOS_GRUB_STATIC/grub-static-efi.cfg" "$d/EFI/BOOT/grub.cfg" || return 1
    printf 'set BOOT_UUID="%s"\n' "$u" > "$d/EFI/BOOT/bootuuid.cfg" || return 1
  else
    log "esp: EFI/BOOT/BOOTX64.EFI is another loader; left alone (Rime boots from its firmware entry)"
  fi
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

nixos_copy() {  # $1=cache  $2=path list — exactly these paths, their references already on the target
  [ -s "$2" ] || return 0
  nixos_nix copy --from "$1" --to "local?root=$TROOT" --no-recursive --stdin < "$2" >>"$LOG" 2>&1
}

# The release this system was installed as, the way `rime update` records an
# applied one (rimed/rime/src/nixos: releases/<release>/{manifest.json,digest},
# applied.json), so the first update compares against it.
nixos_release_state() {  # $1=generation number
  local d="$TROOT/var/lib/rime/nix" gen="$1"
  mkdir -p "$d/releases/$NIXOS_RELEASE" || return 1
  install -m0644 "$NIXOS_WORK/release/manifest.json" "$d/releases/$NIXOS_RELEASE/manifest.json" || return 1
  printf '%s' "$NIXOS_DIGEST" > "$d/releases/$NIXOS_RELEASE/digest" && chmod 0644 "$d/releases/$NIXOS_RELEASE/digest" || return 1
  python3 -I -c 'import json,sys,time
rel, dig, top, gen, out = sys.argv[1:6]
doc = {"releases": [{"release": rel, "digest": dig, "toplevel": top, "generation": int(gen) if gen.isdigit() else None,
                     "at": int(time.time())}]}
open(out, "w").write(json.dumps(doc, sort_keys=True) + "\n")' \
    "$NIXOS_RELEASE" "$NIXOS_DIGEST" "$NIXOS_SYSTEM" "$gen" "$d/applied.json" && chmod 0644 "$d/applied.json"
}

# The features of the chosen system, switched on the way `rime install steam` switches one on (rimed
# rime/src/nixos/pkg.rs write_list: one requesting name per line, 0644): the update client then keeps selecting the
# same variant (base + switched-on features), and rime-feature@<feature>.target starts at boot.
nixos_switches() {
  local s file names n
  for s in "${NIXOS_SWITCHES[@]}"; do
    file="$TROOT${s%% *}"; names="${s#* }"
    mkdir -p "$(dirname "$file")" || return 1
    { for n in ${names//,/ }; do printf '%s\n' "$n"; done; } > "$file.tmp" || return 1
    chmod 0644 "$file.tmp" && mv -f "$file.tmp" "$file" || return 1
    log "nixos: switched on ${s%% *} ($names)"
  done
}

# ── the install: everything from here on destroys $DISK (or $TARGET) ────────
nixos_install() {
  local esp root dev esp_uuid groups g have_groups espnum
  TARGET_DESC="${TARGET_DESC:-$DISK}"
  note "Installing Rime OS to $TARGET_DESC … (full log: $LOG)"
  WROTE_DISK=1
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  if [ "$INSTALL_MODE" = partition ]; then
    esp="$ESP_DEV"; root="$TARGET"
    wipefs -a "$root" >>"$LOG" 2>&1 || nixos_fail "Could not clear $root"
  else
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
  fi
  espnum=$(cat "/sys/class/block/$(basename "$esp")/partition" 2>/dev/null || echo 1)

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
    NIXOS_KARGS=("rd.luks.uuid=$LUKS_UUID" "${NIXOS_KARGS[@]}")
    # The unlock prompt's layout: rime-boot carries host.toml's console keymap
    # on every entry; when rime-install chose another one for the prompt (us,
    # because the owner's layout cannot type this passphrase), it is named here
    # and wins (rime-boot keymap_params: a kernel_params keymap is kept as is).
    if [ -n "${UNLOCK_KEYMAP:-}" ] && [ "$UNLOCK_KEYMAP" != "${CONSOLE_KEYMAP:-}" ]; then
      NIXOS_KARGS+=("rd.vconsole.keymap=$UNLOCK_KEYMAP")
    fi
    log "luks: $root uuid=$LUKS_UUID mapper=$LUKS_MAPPER unlock-keymap=${UNLOCK_KEYMAP:-us}"
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
  # journald's files No_COW on btrfs (systemd's journal-nocow.conf `h /var/log/journal - - - - +C`, which NixOS
  # links too, comes too late: systemd-journal-flush writes the first files before systemd-tmpfiles-setup runs, and
  # +C on a directory reaches only files created after it). Set while the directory is empty, which is the only time
  # it takes. chattr (FS_NOCOW_FL), never `btrfs property`: that one is an xattr.
  mkdir -p "$TROOT/var/log/journal"
  chattr +C "$TROOT/var/log/journal" >>"$LOG" 2>&1 || nixos_fail "Could not make /var/log/journal No_COW"
  log "nixos: $(lsattr -d "$TROOT/var/log/journal" 2>&1)"
  mount -o "subvol=@nix,$NIXOS_MOUNT_OPTS" "$dev" "$TROOT/nix" >>"$LOG" 2>&1 || nixos_fail "Could not mount @nix"
  mount -o "subvol=@home,$NIXOS_MOUNT_OPTS" "$dev" "$TROOT/var/home" >>"$LOG" 2>&1 || nixos_fail "Could not mount @home"
  if [ "${ESP_IS_LIVE:-0}" = 1 ]; then
    # The live medium IS the ESP (a staged Windows install): writable for as
    # long as this takes, then read-only again (live_esp_readonly).
    mount -o remount,rw /run/initramfs/live >>"$LOG" 2>&1 || nixos_fail "Could not make the EFI partition $esp writable"
    mount --bind /run/initramfs/live "$TROOT/boot" >>"$LOG" 2>&1 || nixos_fail "Could not mount the EFI partition"
  else
    mount -o umask=0077 "$esp" "$TROOT/boot" >>"$LOG" 2>&1 || nixos_fail "Could not mount the EFI partition"
  fi
  ln -sfn /var/home "$TROOT/home"
  log "nixos: $(findmnt -R -no TARGET,SOURCE,FSTYPE,OPTIONS "$TROOT" | tr '\n' ';')"

  esp_uuid=$(blkid -s UUID -o value "$esp" 2>>"$LOG" || true)
  [ -n "$esp_uuid" ] || nixos_fail "The EFI partition has no UUID"
  nixos_esp "$TROOT/boot" "$esp_uuid" || nixos_fail "Could not write the boot loader to the EFI partition"
  # Windows beside Rime: a GRUB entry in custom.cfg, which bootupd's static
  # config sources and rime-boot keeps byte for byte (with_include_block).
  # Skipped where Windows uses BitLocker (add_windows_entry says why).
  [ "$INSTALL_MODE" = partition ] && add_windows_entry "$TROOT"
  nixos_host_toml "$TROOT/var/lib/rime/host.toml" || nixos_fail "Could not write host.toml"
  log "nixos: host.toml: $(tr '\n' ' ' < "$TROOT/var/lib/rime/host.toml")"

  note "Copying Rime OS onto the disk … (this is the long part)"
  # The release's own path lists, per cache: nixpkgs' paths from the upstream cache first (they reference no Rime
  # path), then Rime's. Each copy names exactly the paths the verified manifest lists (rime update's order).
  nixos_copy "$NIXOS_UP_CACHE" "$NIXOS_WORK/release/upstream.paths" \
    || nixos_fail "Copying the system from $NIXOS_UP_CACHE failed"
  nixos_copy "$NIXOS_CACHE" "$NIXOS_WORK/release/rime.paths" \
    || nixos_fail "Copying the system from $NIXOS_CACHE failed"
  nixos_nix path-info --store "local?root=$TROOT" --recursive "$NIXOS_SYSTEM" >/dev/null 2>>"$LOG" \
    || nixos_fail "The copied system is incomplete"
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
  gen=${gen#system-}; gen=${gen%-link}
  nixos_release_state "$gen" || nixos_fail "Could not record the installed release"
  nixos_switches || nixos_fail "Could not switch on the features of the $NIXOS_INSTALL_VARIANT system"
  INSTALLED_OK=1
  log "nixos: entries: $(ls "$TROOT/boot/loader/entries" | tr '\n' ' '); release $NIXOS_RELEASE generation $gen"

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
    efibootmgr -q --create --disk "$DISK" --part "$espnum" --label "Rime OS" --loader '\EFI\fedora\shimx64.efi' >>"$LOG" 2>&1 \
      && log "nvram: Rime OS -> $DISK part $espnum \\EFI\\fedora\\shimx64.efi" \
      || log "nvram: efibootmgr failed; the removable path EFI/BOOT/BOOTX64.EFI still boots"
  fi

  sync
  nixos_unmount
  if [ -n "${LUKS_MAPPER:-}" ]; then cryptsetup close "$LUKS_MAPPER" >>"$LOG" 2>&1 || true; fi
  if [ "$INSTALL_MODE" = partition ]; then
    live_esp_readonly
    # The installed system finds /boot by this label (target.nix). Last, so a
    # failed install leaves the ESP as it was found (the Windows app's live boot
    # names its partition by PARTUUID, not by label, so a retry still starts).
    nixos_relabel_esp "$esp" || nixos_fail "Could not name the EFI partition $esp \"$NIXOS_ESP_LABEL\""
  fi
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  return 0
}

nixos_relabel_esp() {  # $1=ESP device — FAT label RIME-ESP (the live medium is read-only here; the label is in the boot sector)
  if have fatlabel; then
    fatlabel "$1" "$NIXOS_ESP_LABEL" >>"$LOG" 2>&1 || return 1
  else
    mlabel -i "$1" "::$NIXOS_ESP_LABEL" >>"$LOG" 2>&1 || return 1
  fi
  udevadm trigger --action=change "$1" >>"$LOG" 2>&1 || true
  udevadm settle --timeout=30 >>"$LOG" 2>&1 || true
  log "esp: $1 is now \"$(blkid -p -s LABEL -o value "$1" 2>/dev/null)\""
  [ "$(blkid -p -s LABEL -o value "$1" 2>/dev/null)" = "$NIXOS_ESP_LABEL" ]
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
  local crypt="" where
  [ "$ENCRYPT" = yes ] && crypt="

This disk is ENCRYPTED: Rime OS asks for the disk passphrase every time it
starts. The RECOVERY KEY shown earlier works at the same prompt."
  if [ "$INSTALL_MODE" = partition ] && [ "${WINDOWS_ENTRY:-0}" = 1 ]; then
    where="Installed into $TARGET, beside Windows; Windows' own partitions were not touched.
To start Windows, choose Windows in the menu that shows for five seconds at power-on."
  elif [ "$INSTALL_MODE" = partition ]; then
    where="Installed into $TARGET; the other partitions on $DISK were not touched.
To start another system on this disk, use your computer's boot menu (usually F12, F11, F9 or Esc)."
  else
    where="Installed on $DISK (whole disk)."
  fi
  # A staged Windows install: the one-shot "Rime OS Setup" firmware entry goes
  # now; the staged files go on the installed system's first boot.
  remove_setup_entry
  note "Rime OS is installed.

$where$crypt

Log in as: $USERNAME
Hostname:  $HOSTNAME
System:    ${NIXOS_INSTALL_VARIANT:-plain}$( [ "${NIXOS_GAMING:-no}" = yes ] && echo " (Steam and Gaming Mode included)")

$( [ "${ESP_IS_LIVE:-0}" = 1 ] && echo "Restart when you are ready." || echo "Remove the USB stick and reboot." )

Full log: $LOG"
  log "install complete (nixos $NIXOS_SYSTEM, ${NIXOS_INSTALL_VARIANT:-?}, release ${NIXOS_RELEASE:-?})"
  write_result ok ""
  echo "RIME-INSTALL-OK"
}
