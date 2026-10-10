#!/usr/bin/env bash
#
#  rime-debrand-runtime.sh — de-brand an ALREADY-INSTALLED Rime OS system
#
#  The image-side fixes (Containerfile.base) only take effect for deployments
#  created from a NEWLY BUILT image. Systems installed from an older image keep
#  three stale, user-visible "Fedora" strings that live OUTSIDE the ostree
#  deployment and are therefore never replaced by `bootc upgrade`:
#
#    1. /boot/loader/entries/*.conf   `title Fedora Linux 43 (Forty Three) …`
#       Written by ostree at deploy time from the deployment's os-release
#       PRETTY_NAME. ostree carries an existing deployment's parsed bootconfig
#       forward on every re-sync, so a stale title sticks for the life of that
#       deployment. Rewriting the title line fixes it permanently for that
#       deployment; new deployments get the branded title from the new image.
#
#    2. EFI NVRAM boot entry label   `Boot0002* Fedora`
#       Written once by bootupd at install time from get_product_name(), which
#       reads /etc/system-release and strips / *release.*/. NVRAM is not part of
#       any deployment, so only efibootmgr can change it.
#
#    3. /etc/os-release as a LOCAL /etc OVERRIDE (a regular file, not the
#       image's symlink into /usr/lib). ostree 3-way-merges /etc forward into
#       every future deployment, so a hand-written override outlives the fix and
#       keeps shadowing the branded /usr/lib/os-release.
#
#  SAFETY MODEL — this touches the boot path.
#    * Dry-run by DEFAULT. Nothing is written without --apply.
#    * Every BLS entry is validated before it is touched (must have title/linux,
#      and the referenced vmlinuz/initramfs must actually exist).
#    * ONLY the `title` line is rewritten. The rewrite is verified by comparing
#      every non-title line byte-for-byte before it is installed.
#    * Backups go to /var/lib/rime-debrand/<timestamp>/ — deliberately NOT next
#      to the entries, because grub's blscfg globs *.conf and a backup left in
#      that directory would appear as a phantom boot menu entry.
#    * The new EFI entry is created and VERIFIED before the old one is deleted.
#      If anything fails first, the original entry is untouched and the machine
#      still boots.
#
#  USAGE
#    sudo ./rime-debrand-runtime.sh                       # dry run, report only
#    sudo ./rime-debrand-runtime.sh --apply               # fix this install
#    sudo ./rime-debrand-runtime.sh --apply --skip-efi \
#         --boot-dir /mnt/other/boot                      # fix a second install
#
#  A second install on the same disk shares ONE ESP and therefore ONE firmware
#  boot entry — run the EFI relabel once, and use --skip-efi for the second
#  install's BLS entries.
#
set -euo pipefail

BRAND="Rime OS"
BOOT_DIR="/boot"
BOOT_DIR_GIVEN=0
APPLY=0
DO_BLS=1
DO_EFI=1
DO_OSREL=1
STAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP_DIR="${RIME_DEBRAND_BACKUP_DIR:-/var/lib/rime-debrand}/${STAMP}"
CHANGED=0
FAILED=0

usage() {
    sed -n '2,45p' "$0" | sed 's/^#\{1,2\} \{0,1\}//;s/^#$//'
    cat <<'EOF'

Options:
  --apply                 actually write changes (default: dry run)
  --brand NAME            product name to brand with (default: Rime OS)
  --boot-dir DIR          operate on this /boot (default: /boot). Also skips
                          the EFI and /etc/os-release steps, which belong to
                          the running system, not to an offline /boot.
  --skip-bls              do not touch /boot/loader/entries
  --skip-efi              do not touch EFI NVRAM
  --skip-os-release       do not touch /etc/os-release
  -h, --help              this help
EOF
}

say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*" >&2; }
err()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; FAILED=1; }
plan() { printf '  \033[36m→\033[0m %s\n' "$*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --apply)           APPLY=1 ;;
        --brand)           BRAND="${2:?--brand needs a value}"; shift ;;
        --boot-dir)        BOOT_DIR="${2:?--boot-dir needs a value}"; shift
                           BOOT_DIR_GIVEN=1; DO_EFI=0; DO_OSREL=0 ;;
        --skip-bls)        DO_BLS=0 ;;
        --skip-efi)        DO_EFI=0 ;;
        --skip-os-release) DO_OSREL=0 ;;
        -h|--help)         usage; exit 0 ;;
        *)                 printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

BOOT_DIR="${BOOT_DIR%/}"
[ -n "$BOOT_DIR" ] || BOOT_DIR=/

if [ "$APPLY" = 1 ] && [ "$(id -u)" != 0 ]; then
    printf 'rime-debrand-runtime: --apply needs root.\n' >&2
    exit 2
fi

case "$BRAND" in
    *[Ff][Ee][Dd][Oo][Rr][Aa]*|*[Cc][Aa][Cc][Hh][Yy]*)
        printf 'rime-debrand-runtime: refusing to brand with "%s".\n' "$BRAND" >&2
        exit 2 ;;
esac

if [ "$APPLY" = 1 ]; then
    say "Rime de-brand — APPLYING changes (backups in $BACKUP_DIR)"
else
    say "Rime de-brand — DRY RUN (nothing will be written; re-run with --apply)"
fi
say ""

backup() {
    # backup <file> <subdir>
    [ "$APPLY" = 1 ] || return 0
    mkdir -p "$BACKUP_DIR/$2"
    cp -a "$1" "$BACKUP_DIR/$2/"
}

# ─────────────────────────────────────────────────────────────────────────────
# 1. BLS boot entry titles
# ─────────────────────────────────────────────────────────────────────────────
BOOT_REMOUNTED=0
restore_boot_ro() {
    if [ "$BOOT_REMOUNTED" = 1 ]; then
        mount -o remount,ro "$BOOT_MNT" 2>/dev/null \
            && info "restored $BOOT_MNT read-only" \
            || warn "could NOT restore $BOOT_MNT read-only — do it by hand"
        BOOT_REMOUNTED=0
    fi
}
trap restore_boot_ro EXIT

# new_title <old title text>  →  branded title text on stdout
new_title() {
    local t="$1"
    # ostree's own format is "<PRETTY_NAME> [<version>] (ostree:N)". Preserve the
    # (ostree:N) suffix exactly and replace only the product part, so the result
    # matches what ostree regenerates from the branded image. (The optional
    # <version> comes from ostree commit metadata; the current image carries
    # none, which is why today's titles have no version segment either.)
    if [[ "$t" =~ ^(.*)\ (\(ostree:[0-9]+\))$ ]]; then
        printf '%s %s\n' "$BRAND" "${BASH_REMATCH[2]}"
        return
    fi
    # Anything else (rescue entries, hand-written entries): swap the product
    # name in place and keep the rest of the label.
    printf '%s\n' "$t" \
        | sed -E "s/Fedora Linux [0-9]+ \([^)]*\)/${BRAND}/g; \
                  s/Fedora Linux [0-9]+/${BRAND}/g; \
                  s/Fedora Linux/${BRAND}/g; \
                  s/Fedora/${BRAND}/g"
}

do_bls() {
    local entries_dir="$BOOT_DIR/loader/entries"
    say "[1/3] BLS boot entry titles — $entries_dir"

    if [ ! -d "$entries_dir" ]; then
        err "no such directory: $entries_dir"
        return
    fi

    shopt -s nullglob
    local confs=( "$entries_dir"/*.conf )
    shopt -u nullglob
    if [ "${#confs[@]}" -eq 0 ]; then
        err "no *.conf entries found in $entries_dir"
        return
    fi

    # Remount /boot rw only if it is a mountpoint AND currently ro.
    BOOT_MNT="$(findmnt -no TARGET --target "$BOOT_DIR" 2>/dev/null || true)"
    if [ "$APPLY" = 1 ] && [ -n "$BOOT_MNT" ] \
       && findmnt -no OPTIONS --target "$BOOT_DIR" 2>/dev/null | tr ',' '\n' | grep -x ro >/dev/null; then
        info "$BOOT_MNT is mounted read-only — remounting rw for the rewrite"
        mount -o remount,rw "$BOOT_MNT" || { err "could not remount $BOOT_MNT rw"; return; }
        BOOT_REMOUNTED=1
    fi

    local f base title new linux initrd tmp
    for f in "${confs[@]}"; do
        base="$(basename "$f")"

        # ---- validate before touching anything -----------------------------
        if [ "$(grep -c '^title ' "$f")" != 1 ]; then
            err "$base: expected exactly one 'title' line — SKIPPED, not editing"
            continue
        fi
        linux="$(sed -n 's/^linux \(.*\)$/\1/p' "$f" | head -1)"
        if [ -z "$linux" ]; then
            err "$base: no 'linux' line — this does not look like a BLS entry, SKIPPED"
            continue
        fi
        if [ ! -e "$BOOT_DIR/${linux#/}" ]; then
            err "$base: kernel $linux missing under $BOOT_DIR — SKIPPED (entry already broken)"
            continue
        fi
        initrd="$(sed -n 's/^initrd \(.*\)$/\1/p' "$f" | head -1)"
        if [ -n "$initrd" ] && [ ! -e "$BOOT_DIR/${initrd#/}" ]; then
            err "$base: initrd $initrd missing under $BOOT_DIR — SKIPPED (entry already broken)"
            continue
        fi
        [ -n "$initrd" ] || warn "$base: no 'initrd' line (unusual, but not fatal)"

        title="$(sed -n 's/^title \(.*\)$/\1/p' "$f" | head -1)"
        new="$(new_title "$title")"

        if [ "$new" = "$title" ]; then
            ok "$base: already '$title'"
            continue
        fi

        say "  $base"
        info "    before: title $title"
        info "    after:  title $new"

        if [ "$APPLY" != 1 ]; then
            plan "would rewrite the title line only"
            CHANGED=$((CHANGED + 1))
            continue
        fi

        tmp="$(mktemp)"
        awk -v nt="title $new" '{ if ($0 ~ /^title /) print nt; else print }' "$f" > "$tmp"

        # ---- verify the rewrite before installing it -----------------------
        if ! diff -q <(grep -v '^title ' "$f") <(grep -v '^title ' "$tmp") >/dev/null; then
            rm -f "$tmp"
            err "$base: rewrite changed a non-title line — ABORTED, file untouched"
            continue
        fi
        if [ "$(grep -c '^title ' "$tmp")" != 1 ] \
           || [ "$(sed -n 's/^title \(.*\)$/\1/p' "$tmp" | head -1)" != "$new" ]; then
            rm -f "$tmp"
            err "$base: rewritten title did not verify — ABORTED, file untouched"
            continue
        fi

        backup "$f" "loader-entries"
        # `cat >` (not mv) so inode, mode, owner and SELinux label are preserved.
        cat "$tmp" > "$f"
        rm -f "$tmp"
        sync -f "$f" 2>/dev/null || sync
        ok "$base: retitled"
        CHANGED=$((CHANGED + 1))
    done

    # ---- grubenv guard: a saved default that names a title would go stale ---
    local genv="$BOOT_DIR/grub2/grubenv"
    if [ -f "$genv" ] && grep -aq '^saved_entry=' "$genv" 2>/dev/null; then
        local sv; sv="$(grep -a '^saved_entry=' "$genv" | head -1 | cut -d= -f2-)"
        case "$sv" in
            *[Ff][Ee][Dd][Oo][Rr][Aa]*)
                warn "grubenv saved_entry='$sv' still names Fedora."
                warn "  fix with: grub2-editenv $genv set saved_entry='$(new_title "$sv")'" ;;
            *)  info "grubenv saved_entry='$sv' (no Fedora string, left alone)" ;;
        esac
    else
        ok "grubenv has no saved_entry — default selection is by index, nothing to update"
    fi
    restore_boot_ro
    say ""
}

# ─────────────────────────────────────────────────────────────────────────────
# 2. EFI NVRAM boot entry label
# ─────────────────────────────────────────────────────────────────────────────
do_efi() {
    say "[2/3] EFI firmware boot entry label"

    if [ ! -d /sys/firmware/efi ]; then
        warn "not booted via UEFI — nothing to relabel"; say ""; return
    fi
    command -v efibootmgr >/dev/null || { err "efibootmgr not installed"; say ""; return; }

    local esp_src esp_mnt esp_disk esp_part esp_partuuid
    esp_mnt="$(findmnt -no TARGET /boot/efi 2>/dev/null || true)"
    [ -n "$esp_mnt" ] || esp_mnt="$(findmnt -fno TARGET -t vfat 2>/dev/null | head -1)"
    if [ -z "$esp_mnt" ]; then
        err "could not find a mounted ESP (expected /boot/efi)"; say ""; return
    fi
    esp_src="$(findmnt -no SOURCE "$esp_mnt")"
    esp_disk="/dev/$(lsblk -no PKNAME "$esp_src")"
    esp_part="$(cat "/sys/class/block/$(basename "$esp_src")/partition" 2>/dev/null || true)"
    esp_partuuid="$(lsblk -no PARTUUID "$esp_src" 2>/dev/null || true)"
    if [ ! -b "$esp_disk" ] || [ -z "$esp_part" ] || [ -z "$esp_partuuid" ]; then
        err "could not resolve ESP disk/partition from $esp_src"; say ""; return
    fi
    info "ESP: $esp_src  (disk $esp_disk, partition $esp_part, PARTUUID $esp_partuuid)"

    # The loader path is FIXED. \EFI\fedora is compiled into Fedora's signed
    # shim/grub2 — renaming that directory breaks Secure Boot, so it stays.
    # Only the NVRAM *label* changes.
    local loader='\EFI\fedora\shimx64.efi'
    if [ ! -e "$esp_mnt/EFI/fedora/shimx64.efi" ]; then
        err "$esp_mnt/EFI/fedora/shimx64.efi missing — refusing to create a boot entry"
        say ""; return
    fi
    ok "loader present: $esp_mnt/EFI/fedora/shimx64.efi (path stays \\EFI\\fedora — signed shim)"

    local dump; dump="$(efibootmgr -v)"
    local order; order="$(printf '%s\n' "$dump" | sed -n 's/^BootOrder: //p' | head -1)"

    # Entries on OUR ESP pointing at OUR loader.
    local ours; ours="$(printf '%s\n' "$dump" \
        | grep -iF "$esp_partuuid" | grep -iF 'EFI\fedora\shimx64.efi' || true)"
    if [ -z "$ours" ]; then
        err "no existing NVRAM entry points at $esp_partuuid + \\EFI\\fedora\\shimx64.efi"
        info "nothing safe to relabel; bootupd will create a branded entry on the next update"
        say ""; return
    fi

    # Already branded?
    if printf '%s\n' "$ours" | grep -E "^Boot[0-9A-Fa-f]{4}\*?[[:space:]]+${BRAND}[[:space:]]" >/dev/null; then
        ok "an NVRAM entry is already labelled '$BRAND' — nothing to do"
        printf '%s\n' "$ours" | sed 's/^/    /'
        say ""; return
    fi

    local stale_nums; stale_nums="$(printf '%s\n' "$ours" \
        | sed -n 's/^Boot\([0-9A-Fa-f]\{4\}\)\*\{0,1\}[[:space:]].*/\1/p')"
    info "stale entries to replace:"
    printf '%s\n' "$ours" | sed 's/^/    /'
    info "BootOrder before: $order"
    plan "create Boot#### '$BRAND' → $esp_disk part $esp_part $loader"
    plan "put it at the stale entry's position in BootOrder, then delete the stale entry"

    if [ "$APPLY" != 1 ]; then
        CHANGED=$((CHANGED + 1)); say ""; return
    fi

    mkdir -p "$BACKUP_DIR"
    printf '%s\n' "$dump" > "$BACKUP_DIR/efibootmgr-before.txt"

    # --- create FIRST; the old entry stays bootable until this verifies ------
    if ! efibootmgr --quiet --create --disk "$esp_disk" --part "$esp_part" \
                    --loader "$loader" --label "$BRAND" >/dev/null 2>&1; then
        err "efibootmgr --create failed — original entry untouched, nothing changed"
        say ""; return
    fi

    local after new_num
    after="$(efibootmgr -v)"
    new_num="$(printf '%s\n' "$after" \
        | grep -iF "$esp_partuuid" | grep -iF 'EFI\fedora\shimx64.efi' \
        | grep -E "^Boot[0-9A-Fa-f]{4}\*?[[:space:]]+${BRAND}[[:space:]]" \
        | sed -n 's/^Boot\([0-9A-Fa-f]\{4\}\).*/\1/p' | head -1)"
    if [ -z "$new_num" ]; then
        err "created entry did not verify (wrong ESP or label). NOT deleting anything."
        err "inspect with: efibootmgr -v   (backup: $BACKUP_DIR/efibootmgr-before.txt)"
        say ""; return
    fi
    ok "created Boot$new_num '$BRAND' (verified: same ESP, same loader)"

    # --- BootOrder: take the stale entry's slot, do not just sit at the front -
    local first_stale; first_stale="$(printf '%s\n' "$stale_nums" | head -1)"
    local new_order=""
    local n
    for n in ${order//,/ }; do
        if [ "${n^^}" = "${first_stale^^}" ]; then
            new_order="${new_order:+$new_order,}$new_num"
        elif printf '%s\n' "$stale_nums" | grep -ix "$n" >/dev/null; then
            continue                       # drop the other stale ones
        elif [ "${n^^}" = "${new_num^^}" ]; then
            continue                       # efibootmgr already prepended it
        else
            new_order="${new_order:+$new_order,}$n"
        fi
    done
    [ -n "$new_order" ] || new_order="$new_num"
    if efibootmgr --quiet -o "$new_order" >/dev/null 2>&1; then
        ok "BootOrder set to $new_order"
    else
        warn "could not set BootOrder; the new entry is first (efibootmgr default)."
        warn "  original order was: $order"
    fi

    # --- only NOW delete the stale entries -----------------------------------
    for n in $stale_nums; do
        if efibootmgr --quiet -b "$n" -B >/dev/null 2>&1; then
            ok "deleted stale Boot$n"
        else
            warn "could not delete Boot$n — harmless, it points at the same loader"
        fi
    done

    say ""
    info "result:"
    efibootmgr | sed 's/^/    /'
    CHANGED=$((CHANGED + 1))
    say ""
}

# ─────────────────────────────────────────────────────────────────────────────
# 3. /etc/os-release local override
# ─────────────────────────────────────────────────────────────────────────────
brand_os_release() {
    # stdin: an os-release; stdout: the branded version. Same rules as
    # Containerfile.base — ID and VERSION_ID are deliberately left alone.
    sed -E \
      -e "s|^NAME=.*|NAME=\"${BRAND}\"|" \
      -e "s|^PRETTY_NAME=.*|PRETTY_NAME=\"${BRAND}\"|" \
      -e 's|^VERSION=.*|VERSION="43"|' \
      -e 's|^VERSION_CODENAME=.*|VERSION_CODENAME=""|' \
      -e 's|^ANSI_COLOR=.*|ANSI_COLOR="0;38;2;161;201;153"|' \
      -e 's|^LOGO=.*|LOGO=rime-os-logo|' \
      -e 's|^CPE_NAME=.*|CPE_NAME="cpe:/o:rimeos:rime_os:43"|' \
      -e 's|^HOME_URL=.*|HOME_URL="https://github.com/AndreNijman/rime-os"|' \
      -e 's|^DOCUMENTATION_URL=.*|DOCUMENTATION_URL="https://github.com/AndreNijman/rime-os#readme"|' \
      -e 's|^SUPPORT_URL=.*|SUPPORT_URL="https://github.com/AndreNijman/rime-os/discussions"|' \
      -e 's|^BUG_REPORT_URL=.*|BUG_REPORT_URL="https://github.com/AndreNijman/rime-os/issues"|' \
      -e 's|^DEFAULT_HOSTNAME=.*|DEFAULT_HOSTNAME="rime"|' \
      -e '/^REDHAT_/d' \
      -e '/^# /d'
}

do_osrelease() {
    say "[3/3] /etc/os-release local override"

    if [ -L /etc/os-release ]; then
        ok "/etc/os-release is a symlink → $(readlink /etc/os-release) (image-owned, correct)"
        if grep -q '^PRETTY_NAME=' /usr/lib/os-release \
           && grep -qi 'fedora' <(grep -v '^ID=' /usr/lib/os-release); then
            warn "…but /usr/lib/os-release still has Fedora strings — this deployment"
            warn "  predates the image fix. It clears itself on the next 'bootc upgrade'."
        fi
        say ""; return
    fi
    if [ ! -e /etc/os-release ]; then
        err "/etc/os-release does not exist"; say ""; return
    fi

    info "/etc/os-release is a REGULAR FILE — a local /etc modification."
    info "ostree merges /etc forward, so this shadows the image on every future deployment."

    local usr_pretty; usr_pretty="$(sed -n 's/^PRETTY_NAME=//p' /usr/lib/os-release | tr -d '"')"
    if [ -n "$usr_pretty" ] && [[ "$usr_pretty" != *[Ff]edora* ]]; then
        info "the image's /usr/lib/os-release is already branded ('$usr_pretty')"
        plan "remove the override and restore the symlink → ../usr/lib/os-release"
        if [ "$APPLY" = 1 ]; then
            backup /etc/os-release "etc"
            rm -f /etc/os-release
            ln -s ../usr/lib/os-release /etc/os-release
            ok "/etc/os-release is now the image symlink again"
        fi
        CHANGED=$((CHANGED + 1)); say ""; return
    fi

    info "the running deployment's /usr/lib/os-release still says '$usr_pretty'"
    info "→ the override is still needed; refreshing it from the image, fully branded"
    local tmp; tmp="$(mktemp)"
    {
        printf '%s\n' \
          '# Rime OS branding override — TRANSITIONAL, written by rime-debrand-runtime.sh.' \
          '# The running deployment predates the image-side branding, so this shadows its' \
          '# /usr/lib/os-release. DELETE THIS FILE after the next `bootc upgrade` to a' \
          '# branded image (`rm /etc/os-release && ln -s ../usr/lib/os-release /etc/os-release`)' \
          '# — otherwise it sticks forever via ostree /etc merging and will pin VERSION_ID.' \
          '# ID stays "fedora" on purpose: dnf copr chroots and $releasever key off it.'
        brand_os_release < /usr/lib/os-release
        # Carry the flavor stamp over only if the image did not already set it
        # (Containerfile.daily/.gaming append it to /usr/lib/os-release).
        if ! grep -q '^VARIANT_ID=' /usr/lib/os-release; then
            grep -E '^VARIANT(_ID)?=' /etc/os-release 2>/dev/null || true
        fi
    } > "$tmp"

    if diff -q <(grep -v '^#' /etc/os-release) <(grep -v '^#' "$tmp") >/dev/null 2>&1; then
        rm -f "$tmp"
        ok "override already fully branded"
        say ""; return
    fi
    info "diff (current → new):"
    diff -u /etc/os-release "$tmp" 2>/dev/null | tail -n +3 | sed 's/^/    /' || true
    if [ "$APPLY" = 1 ]; then
        backup /etc/os-release "etc"
        cat "$tmp" > /etc/os-release
        ok "/etc/os-release refreshed"
    else
        plan "would refresh /etc/os-release"
    fi
    rm -f "$tmp"
    CHANGED=$((CHANGED + 1))
    say ""
}

if [ "$DO_BLS" = 1 ]; then do_bls; else say "[1/3] BLS titles — skipped"; say ""; fi
if [ "$DO_EFI" = 1 ]; then do_efi; else say "[2/3] EFI NVRAM label — skipped"; say ""; fi
if [ "$DO_OSREL" = 1 ]; then do_osrelease; else say "[3/3] /etc/os-release — skipped"; say ""; fi

say "──────────────────────────────────────────────────────────────"
if [ "$BOOT_DIR_GIVEN" = 1 ]; then
    say "boot dir: $BOOT_DIR (EFI + os-release skipped by default for --boot-dir)"
fi
if [ "$APPLY" = 1 ]; then
    say "applied; $CHANGED item(s) changed. Backups: $BACKUP_DIR"
    say ""
    say "NOT changed, and NOT changeable — see docs/branding.md:"
else
    say "dry run; $CHANGED item(s) would change. Re-run with --apply."
    say ""
    say "Will still say Fedora/CachyOS afterwards (unfixable):"
fi
say "  * uname -r  →  $(uname -r)   (compiled into the CachyOS kernel package)"
say "  * the ESP directory EFI/fedora   (path is compiled into the signed"
say "    shim/grub2 — renaming it breaks Secure Boot; only the NVRAM label moves)"
say "  * ID=fedora in os-release   (machine-facing; dnf copr chroots and"
say "    \$releasever derive from it — verified breakage if changed)"

[ "$FAILED" = 0 ] || exit 1
exit 0
