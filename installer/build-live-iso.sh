#!/usr/bin/env bash
# Build the Rime OS installer LIVE ISO from installer/Containerfile.installer.
#
#   installer image -> exported rootfs
#                    + Rime image injected into its /var/lib/containers (host-side skopeo)
#                   -> ext4 rootfs.img -> squashfs (classic dmsquash-live layout)
#                    + dracut dmsquash-live initramfs
#                   -> xorriso hybrid ISO (UEFI incl. Secure Boot + legacy BIOS)
#
# Why the ext4-in-squashfs (dm-snapshot) layout instead of overlayfs live root:
# podman/bootc in the live session need native overlay mounts, and the kernel
# refuses overlay-on-overlayfs. With dm-snapshot the live root is plain ext4,
# so `podman run … bootc install to-disk` behaves exactly like on a normal host.
#
# Boot-test in QEMU BOTH ways (SeaBIOS and OVMF) before flashing. Run from the
# repo's installer/ dir.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:-/var/tmp/rime-iso-build}"
# This script `sudo rm -rf`s directories under WORK. A WORK that resolves to a
# shared directory would make that a recursive delete of somebody else's files.
WORK="$(realpath -m "$WORK")"
case "$WORK" in
  /|/var|/var/tmp|/tmp|/home|/var/home|/root|/var/lab-scratch)
    echo "FATAL: WORK must be a dedicated build directory, not $WORK" >&2; exit 1 ;;
esac
# The list above only names the obvious shared directories; /var/tmp/shared is
# just as shared and not on it. So ownership is proven, not assumed: this script
# works in WORK only if WORK is new, empty, or carries the marker it wrote on an
# earlier run. The rm -rf calls below target fixed names (rootfs, sqroot,
# isoroot, cs-run, grub-i386-pc) that another tool's tree could also contain.
if [ -d "$WORK" ] && [ -n "$(ls -A "$WORK" 2>/dev/null)" ] && [ ! -e "$WORK/.rime-iso-build" ]; then
  echo "FATAL: $WORK already holds files this script did not create (no .rime-iso-build marker)." >&2
  echo "       Point WORK at a new or empty directory; nothing has been touched." >&2
  exit 1
fi
mkdir -p "$WORK" && touch "$WORK/.rime-iso-build"
# Which tag this ISO installs. This is NOT cosmetic: it names the embedded
# storage tag AND is stamped into the live env so rime-install derives its
# --target-imgref from it — the origin the installed machine follows on every
# `bootc upgrade`. The editions converged into one image, `:rime`; the old tags
# are aliases of the same digest, and new media must record the canonical one.
EDITION="${EDITION:-rime}"
OCI="$WORK/rime.oci"                       # produced by: sudo skopeo copy containers-storage:localhost/rime-os:$EDITION oci-archive:$OCI:rime-os-$EDITION
OUT="${OUT:-$WORK/rime-os-installer.iso}"
LABEL="RIME-INSTALL"
# Overridable so a throwaway probe image can be built into a bootable ISO
# without clobbering the real installer tag. The default is the production one.
IMG="${IMG:-localhost/rime-installer:latest}"
ISOROOT="$WORK/isoroot"

# PRODUCTION=1 (default): the flashed-to-USB build. NO unattended install path —
# the marker file is not baked and the unattended boot-menu entry is omitted, so
# `rime.unattended` is inert and nobody can accidentally trigger a disk wipe.
# PRODUCTION=0: test/CI build — bakes the marker + adds the unattended menu entry
# so the QEMU boot-test can drive an end-to-end install headlessly.
PRODUCTION="${PRODUCTION:-1}"
if [ "$PRODUCTION" = 1 ] && [ "$EDITION" != rime ]; then
  echo "FATAL: production media must record the canonical :rime origin, not :$EDITION" >&2; exit 1
fi

# NETINSTALL=1: build the small ISO. It ships the live environment only and the
# installer pulls the OS from the public registry at install time, which takes
# it from ~12 GB to something publishable (GitHub caps release assets at 2 GiB)
# and downloadable by someone who just wants to try this.
#
# NETINSTALL=0 (default) is the fat offline ISO: the OS image is embedded once,
# as an OCI dir on the ISO that the installer's bootc reads directly, so an
# install needs no network whatsoever. That is the one to hand someone on a USB
# stick, and the one to use where the network cannot be trusted.
NETINSTALL="${NETINSTALL:-0}"
if [ "$NETINSTALL" = 1 ]; then echo "build mode: NETWORK INSTALL (small ISO, pulls the OS at install time)"; fi
if [ "$PRODUCTION" = 1 ]; then ALLOW_UNATTENDED=0; else ALLOW_UNATTENDED=1; fi
echo "build mode: PRODUCTION=$PRODUCTION (ALLOW_UNATTENDED=$ALLOW_UNATTENDED)"

mkdir -p "$WORK"
if [ "$NETINSTALL" != 1 ]; then
  [ -f "$OCI" ] || { echo "ERROR: $OCI missing (run the skopeo export first)"; exit 1; }
fi

if [ "$NETINSTALL" = 1 ]; then
  echo "== 0. pin and verify the image this ISO installs =="
  # A netinstall ISO and the image it downloads are one release. Resolving
  # `:rime` at install time would make every ISO install whatever main pushed
  # last — an image nobody has booted from this ISO. So the digest is resolved
  # ONCE, here, and stamped into the live env; rime-install downloads exactly
  # that digest and still records `:rime` as the origin, so the machine updates
  # normally afterwards.
  #
  # RELEASE_DIGEST is overridable so the PRODUCTION=0 build that gets
  # boot-tested and the PRODUCTION=1 build that gets published pin the SAME
  # image even if main pushes in between. Pass the digest the test build printed.
  RELEASE_REPO="ghcr.io/andrenijman/rime-os"
  RELEASE_DIGEST="${RELEASE_DIGEST:-$(sudo skopeo inspect --format '{{.Digest}}' "docker://$RELEASE_REPO:$EDITION")}"
  [[ "$RELEASE_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || { echo "FATAL: could not resolve $RELEASE_REPO:$EDITION to a digest (got '$RELEASE_DIGEST')" >&2; exit 1; }
  RELEASE_IMAGE="$RELEASE_REPO@$RELEASE_DIGEST"
  echo "image pinned: $RELEASE_IMAGE"
  # Signed by main's build-image workflow, or it does not go on an ISO. Default
  # TUF trust root; --network host because the default podman network on this
  # build host does not resolve the Sigstore CDN.
  # This repository under either name: a build signed before the GitHub rename
  # names apex-os, one signed after it rime-os. Anchored both ends. (rime-rename: keep)
  signer_re='^https://github\.com/AndreNijman/(apex|rime)-os/\.github/workflows/build-image\.yml@refs/heads/main$'  # rime-rename: keep (accepts the pre-rename repository)
  sudo podman run --rm --network host ghcr.io/sigstore/cosign/cosign:v3.1.3 verify \
    --certificate-identity-regexp "$signer_re" \
    --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
    "$RELEASE_IMAGE" >/dev/null \
    || { echo "FATAL: $RELEASE_IMAGE is not signed by main's build-image workflow" >&2; exit 1; }
  echo "signature verified: build-image.yml@refs/heads/main"
  # The encrypted install refuses without the recovery-key helper, and it is
  # the default path. An image without it would turn the default into a refusal.
  sudo podman run --rm "$RELEASE_IMAGE" test -x /usr/libexec/rime-luks-enroll \
    || { echo "FATAL: $RELEASE_IMAGE has no /usr/libexec/rime-luks-enroll; the default encrypted install would refuse" >&2; exit 1; }
fi

echo "== 1. build the installer live-env image =="
# --network host: dnf's metalink fetch hung for minutes on the default podman
# network on this build host (DNS), and a hung build looks exactly like a slow one.
sudo podman build --network host --build-arg "ALLOW_UNATTENDED=$ALLOW_UNATTENDED" \
  -f "$HERE/Containerfile.installer" -t "$IMG" "$HERE"

echo "== 2. export the rootfs =="
# A previous run may have left overlay/subvol mounts under rootfs (step 3's
# containers-storage embed). Detach them deepest-first or `rm -rf` fails
# "device busy" and set -e aborts the build.
mount | awk -v d="$WORK/rootfs" 'index($3,d)==1 {print $3}' | sort -r \
  | while read -r m; do sudo umount -l "$m" 2>/dev/null || true; done
sudo rm -rf "$WORK/rootfs"; mkdir -p "$WORK/rootfs"
cid=$(sudo podman create "$IMG")
sudo podman export "$cid" | sudo tar -x -C "$WORK/rootfs"
sudo podman rm "$cid" >/dev/null
KVER=$(sudo ls "$WORK/rootfs/usr/lib/modules" | head -1)
echo "kernel: $KVER"

echo "== 3. the installed image's own bootc inputs, carried into the live env =="
# rime-install runs THIS live env's bootc (`bootc install to-filesystem
# --source-imgref …`, see "Where the OS comes from" in it), not bootc inside a
# container of the image. That is what removed the tens of GB a network
# install used to stage, and it is also why the live env no longer needs the
# image in its container storage at all: an offline ISO installs from the OCI
# directory on the ISO (step 5b), a netinstall from the registry.
#
# bootc does take three things from the machine it runs on, so each one is
# copied out of the EXACT image this ISO installs — never taken from Fedora:
#   /etc/selinux                       bootc labels the deployment with the
#                                      running machine's policy, not the
#                                      image's (bootc-dev/bootc#1438)
#   /usr/lib/ostree/prepare-root.conf  whether the deployment gets its composefs
#                                      image (bootc-dev/bootc#1400)
#   /usr/libexec/rime-luks-enroll      the recovery-key helper the encrypted
#                                      install runs
if [ "$NETINSTALL" = 1 ]; then
  SRC_IMAGE="$RELEASE_IMAGE"
  sudo install -Dm644 /dev/null "$WORK/rootfs/usr/lib/rime-installer/netinstall"
  printf '%s\n' "$RELEASE_DIGEST" | sudo tee "$WORK/rootfs/usr/lib/rime-installer/image-digest" >/dev/null
  grep -qx "$RELEASE_DIGEST" "$WORK/rootfs/usr/lib/rime-installer/image-digest" \
    || { echo "FATAL: image digest stamp not written"; exit 1; }
  echo "image digest stamped: $RELEASE_DIGEST"
else
  SRC_IMAGE="localhost/rime-os:${EDITION}"
fi
CARRIED="etc/selinux usr/lib/ostree/prepare-root.conf usr/libexec/rime-luks-enroll"
cid=$(sudo podman create "$SRC_IMAGE")
sudo rm -rf "$WORK/rootfs/etc/selinux"
sudo mkdir -p "$WORK/rootfs/usr/lib/ostree" "$WORK/rootfs/usr/libexec"
for f in $CARRIED; do
  sudo podman cp "$cid:/$f" "$WORK/rootfs/$f"
done
sudo podman rm "$cid" >/dev/null
# Read back from both sides rather than trusting `podman cp`'s exit status: a
# policy that differs from the image's is a machine labelled wrongly, and it
# would boot looking fine until enforcement denied something.
# shellcheck disable=SC2086  # CARRIED is a list of fixed relative paths
want=$(sudo podman run --rm "$SRC_IMAGE" sh -c "cd / && find $CARRIED -type f -print0 | sort -z | xargs -0 sha256sum" | sha256sum)
# shellcheck disable=SC2086
have=$(cd "$WORK/rootfs" && sudo find $CARRIED -type f -print0 | sort -z | sudo xargs -0 sha256sum | sha256sum)
[ "$want" = "$have" ] || { echo "FATAL: the live env's copies of $CARRIED differ from $SRC_IMAGE" >&2; exit 1; }
sudo test -s "$WORK/rootfs/etc/selinux/targeted/contexts/files/file_contexts" \
  || { echo "FATAL: the policy carried from $SRC_IMAGE has no file_contexts" >&2; exit 1; }
sudo test -x "$WORK/rootfs/usr/libexec/rime-luks-enroll" \
  || { echo "FATAL: rime-luks-enroll is not executable in the live env" >&2; exit 1; }
echo "carried from $SRC_IMAGE: $CARRIED (sha256 of the set matches the image)"
# The live env's bootc must not be OLDER than the one the image ships: an
# older bootc installing a newer image is the combination nobody has tested.
img_bootc=$(sudo podman run --rm "$SRC_IMAGE" rpm -q --qf '%{VERSION}-%{RELEASE}' bootc)
live_bootc=$(sudo podman run --rm "$IMG" rpm -q --qf '%{VERSION}-%{RELEASE}' bootc)
[[ "$img_bootc" =~ ^[0-9] ]] && [[ "$live_bootc" =~ ^[0-9] ]] \
  || { echo "FATAL: could not read bootc versions (image '$img_bootc', live env '$live_bootc')" >&2; exit 1; }
[ "$(printf '%s\n%s\n' "$img_bootc" "$live_bootc" | sort -V | tail -1)" = "$live_bootc" ] \
  || { echo "FATAL: the live env's bootc $live_bootc is older than the image's $img_bootc" >&2; exit 1; }
echo "bootc: live env $live_bootc, image $img_bootc"

# Stamp the edition so rime-install derives IMAGE and --target-imgref from it
# rather than assuming daily. Asserted below, because a wrong or missing stamp
# is silent at install time and only bites on the first `bootc upgrade`.
sudo install -Dm644 /dev/null "$WORK/rootfs/usr/lib/rime-installer/edition"
printf '%s\n' "$EDITION" | sudo tee "$WORK/rootfs/usr/lib/rime-installer/edition" >/dev/null
grep -qx "$EDITION" "$WORK/rootfs/usr/lib/rime-installer/edition" \
  || { echo "FATAL: edition stamp not written"; exit 1; }
echo "edition stamped: $EDITION"

# Stamp whether the image we are about to install carries a kernel signed with
# the Rime MOK. The installer needs this BEFORE it installs anything: it decides
# whether to offer Secure Boot enrolment, and the marker it would otherwise read
# (/usr/share/rime-os/secureboot/kernel-signed) only exists inside the image,
# which is not mounted yet when the question has to be asked.
#
# Enrolment is offered from the LIVE environment on purpose. `mokutil --import`
# writes MokNew/MokAuth to UEFI NVRAM — firmware state, not filesystem state —
# so a key queued here is picked up by shim on the next boot and enrolled for the
# machine, whichever OS is on the disk. Doing it during the install is the whole
# point: the alternative was installing, booting, running mokutil by hand and
# rebooting again, which is a lot to ask of someone who just wanted an OS.
if [ "$NETINSTALL" = 1 ]; then
  # A netinstall used to stamp "unknown" here, because it could not know which
  # image the registry would hand it — and "unknown" meant it never offered
  # Secure Boot enrolment at all. It knows now: step 0 pinned the exact digest
  # the installer will download, so the answer is read from THAT image.
  KSIGNED=$(sudo podman run --rm "$RELEASE_IMAGE" \
              cat /usr/share/rime-os/secureboot/kernel-signed 2>/dev/null | tr -d '\n' || true)
  [ -n "$KSIGNED" ] || KSIGNED=unknown
else
  KSIGNED=$(sudo podman run --rm "localhost/rime-os:${EDITION}" \
              cat /usr/share/rime-os/secureboot/kernel-signed 2>/dev/null | tr -d '\n' || true)
  [ -n "$KSIGNED" ] || KSIGNED=unknown
fi
printf '%s\n' "$KSIGNED" | sudo tee "$WORK/rootfs/usr/lib/rime-installer/kernel-signed" >/dev/null
echo "kernel-signed stamped: $KSIGNED"

echo "== 4. dracut live initramfs (dmsquash-live) =="
# Built inside the installer image (same kernel/modules as the live rootfs).
# No 'livenet' (needs dracut-network, and we don't netboot).
# label=disable: the SELinux-enforcing host would otherwise deny writes to /w.
sudo podman run --rm --security-opt label=disable -v "$WORK":/w "$IMG" \
  dracut --force --no-hostonly --nomdadmconf --nolvmconf \
    --add "dmsquash-live pollcdrom" \
    --add-drivers "squashfs iso9660 sr_mod cdrom loop ext4 dm-snapshot" \
    /w/initrd.img "$KVER"
sudo cp "$WORK/rootfs/usr/lib/modules/$KVER/vmlinuz" "$WORK/vmlinuz"

echo "== 5. ext4 rootfs.img inside squashfs (classic LiveOS layout) =="
# dmsquash-live default (dm-snapshot) mode expects squashfs.img containing
# LiveOS/rootfs.img (an ext4 fs image). Size it to the rootfs + 15% + slack.
bytes=$(sudo du -sb --apparent-size "$WORK/rootfs" | cut -f1)
# +40% and a 1.5G floor of slack. `du --apparent-size` undercounts real ext4 cost
# (metadata, block rounding), and the previous +15%/768M left the live root 96%
# full (~525MB free) — zero margin for logs, /tmp or a container scratch dir.
imgsz=$(( bytes + bytes * 2 / 5 + 1536*1024*1024 ))
sudo rm -rf "$WORK/sqroot"; sudo mkdir -p "$WORK/sqroot/LiveOS" "$WORK/mnt"
sudo truncate -s "$imgsz" "$WORK/sqroot/LiveOS/rootfs.img"
sudo mkfs.ext4 -q -F -L "RIME-LIVE-ROOT" "$WORK/sqroot/LiveOS/rootfs.img"
sudo mount -o loop "$WORK/sqroot/LiveOS/rootfs.img" "$WORK/mnt"
sudo cp -a "$WORK/rootfs/." "$WORK/mnt/"
sudo umount "$WORK/mnt"; sudo rmdir "$WORK/mnt"

# sudo: a previous run's step 5b leaves $ISOROOT/container root-owned.
sudo rm -rf "$ISOROOT"; mkdir -p "$ISOROOT/LiveOS" "$ISOROOT/images/pxeboot" "$ISOROOT/EFI/BOOT"
sudo mksquashfs "$WORK/sqroot" "$ISOROOT/LiveOS/squashfs.img" \
  -comp zstd -b 1M -noappend -no-progress
sudo rm -rf "$WORK/sqroot"
sudo cp "$WORK/vmlinuz"    "$ISOROOT/images/pxeboot/vmlinuz"
sudo cp "$WORK/initrd.img" "$ISOROOT/images/pxeboot/initrd.img"

echo "== 5b. OCI dir on the ISO (bootc install source) =="
# rime-install passes --source-imgref oci:… pointing here: the oci transport
# streams blobs directly off the ISO, so nothing is unpacked into the live
# env's RAM-backed /var/tmp on the way to the disk.
sudo rm -rf "$ISOROOT/container"
if [ "$NETINSTALL" = 1 ]; then
  echo "  (netinstall: no OCI dir on the ISO)"
else
  sudo skopeo copy "oci-archive:$OCI" "oci:$ISOROOT/container"
fi

echo "== 6. bootloader (UEFI grub2) =="
# selinux=0: the live session itself runs without SELinux. The policy files in
# its /etc/selinux are the IMAGE's (step 3), present so bootc can label the
# installed system with them; LOADING them into the live kernel would confine
# a session that was never labelled for it, and bootc then aborts with "Failed
# to enter install_t (running as kernel)". Affects only the live session, not
# the installed OS.
CMDLINE="root=live:CDLABEL=$LABEL rd.live.image selinux=0"
# Menu config lives ON THE ISO (editable without regenerating BOOTX64.EFI).
# serial+console terminals so headless QEMU (and real serial rigs) get the menu.
cat > "$WORK/grub.cfg" <<EOF
# Serial is CONDITIONAL (rime-logs 48). Unconditionally running \`serial\` then
# \`terminal_output serial console\` is fine under QEMU but hostile on real
# laptops with no UART: the command can fail and take the console terminal down
# with it, and a floating RS-232 line can inject phantom keypresses into the
# menu. Guard it so headless/serial rigs still work while real hardware is never
# put at risk by hardware it does not have.
if serial --unit=0 --speed=115200; then
    terminal_input serial console
    terminal_output serial console
fi

# LEGACY-BIOS-ONLY video handoff. On BIOS there is no GOP: something must
# program a VESA linear framebuffer before userspace starts, or sysfb has
# nothing to register, no DRM device node ever appears, and the graphical
# installer cannot start -- the launcher paints its DRM-nodes-absent bug
# screen on a text console. QEMU hides this failure (its bochs GPU has a
# native kernel driver that needs no firmware framebuffer), so it was only
# caught by booting BIOS with nomodeset, which is exactly what every real
# driverless legacy machine looks like.
#
# DO NOT "simplify" this back to gfxpayload. Fedora's i386-pc grub cannot do
# the upstream video handoff: its linux command is the 16-bit-entry loader
# (the module imports grub_relocator16_boot and resets the card to text mode
# right before jumping) and contains no gfxpayload handling at all -- measured
# on grub2-pc-modules 2.12-43.fc43 by dumping the ELF symbols and strings of
# linux.mod, and confirmed in QEMU: with gfxpayload set, the kernel still came
# up on the 80x25 VGA text console. What the 16-bit boot protocol DOES offer
# is vga=791 (VESA mode 0x317, 1024x768 16bpp linear): grub parses it into the
# boot header, the kernel sets the mode itself in real mode via the video
# BIOS, sysfb registers it, simpledrm binds it, and the GUI paints -- verified
# in QEMU with nomodeset (simple-framebuffer + simpledrm in the boot log, GUI
# painted at 1024x768, bochs driver absent).
# Worst case on a pre-VBE-2.0 card without that mode: the kernel prints
# Undefined video mode number, waits 30 seconds for a key, then boots in text
# mode -- the pre-fix behaviour, only slower. It cannot hang the boot. The
# troubleshoot entry below deliberately omits it as the escape hatch.
# On UEFI, biosfb expands to nothing: the UEFI cmdline stays byte-identical
# to the UEFI-only builds and the kernel takes the GOP framebuffer as before.
if [ "\$grub_platform" = "pc" ]; then
    set biosfb=vga=791
else
    set biosfb=
fi

# shim/grub are loaded from the ESP, but the kernel + initrd live on the ISO9660
# filesystem — point \$root at it by volume label before referencing those paths.
# On the BIOS path both grub and the kernel live on the ISO9660 fs and this
# search works identically, so the same file serves both firmwares unmodified.
search --no-floppy --set=root --label $LABEL

set default=0
set timeout=10

# NO \`quiet\` on the default entry. This is an INSTALLER on unknown hardware —
# there is no splash to protect, and \`quiet\` turns every possible failure
# (KMS bringing up no display, dmsquash-live not finding the ISO, the installer
# unit dying) into an identical featureless black screen. An Acer Aspire hit
# exactly that: menu appeared, then nothing, with no way to tell which stage
# failed. Text boot costs nothing here and makes the failure legible.
#
# console=tty0 LAST so the screen is the primary console; ttyS0 first keeps
# QEMU/CI serial observability.
menuentry "Install Rime OS" {
    linux /images/pxeboot/vmlinuz $CMDLINE console=ttyS0,115200 console=tty0 \$biosfb
    initrd /images/pxeboot/initrd.img
}
# For machines whose GPU the kernel cannot mode-set. "Menu, then black" is the
# signature symptom, and this is the standard escape: no KMS, firmware
# framebuffer only.
#
# The graphical installer still works here, and that is not luck. nomodeset
# only stops NATIVE DRM drivers; the live kernel has CONFIG_DRM_SIMPLEDRM=y and
# CONFIG_SYSFB_SIMPLEFB=y (both built in, not modules), so simpledrm binds the
# boot-time framebuffer (the GOP one on UEFI, the vga=791 VESA one on BIOS)
# and a /dev/dri card node exists regardless -- do not assume it is card0,
# the number floats. cage runs on it with WLR_RENDERER=pixman (dumb buffers,
# no render node — simpledrm has none) and GTK renders with GSK_RENDERER=cairo
# (shm, no GL). Nothing in the installer's display path needs a real GPU
# driver.
#
# NOTE for anyone editing this heredoc: it is UNQUOTED (<<EOF), so backticks and
# dollar-variables in these comments are interpreted by the shell. Both bit this
# comment block during editing: a backtick-quoted word ran as a command, and a
# dollar-word tripped the unbound-variable check. Keep prose free of both.
#
# This is worth stating because the launcher used to treat "no native KMS" as a
# reason to give up on the GUI, which was wrong and is what stranded users in
# the old text installer.
menuentry "Install Rime OS (safe graphics — try this if the screen goes black)" {
    linux /images/pxeboot/vmlinuz $CMDLINE console=ttyS0,115200 console=tty0 nomodeset \$biosfb
    initrd /images/pxeboot/initrd.img
}
# Drops to a dracut shell if the live root is not found, instead of hanging
# black. Use when the USB enumerates slowly or the ISO label is not matched.
# Deliberately does NOT carry the biosfb vga mode: this entry doubles as the
# escape hatch for a machine whose video BIOS misbehaves on the VESA mode set,
# so it must stay bootable with the firmware console untouched.
menuentry "Install Rime OS (troubleshoot — dracut shell on failure)" {
    linux /images/pxeboot/vmlinuz $CMDLINE console=ttyS0,115200 console=tty0 rd.shell rd.debug
    initrd /images/pxeboot/initrd.img
}
EOF
# TEST/CI builds only: the unattended-install menu entry (auto-wipes /dev/vda).
# NEVER included in a PRODUCTION build — and even if its cmdline is added by hand,
# rime-install ignores rime.unattended without the (production-absent) marker.
if [ "$PRODUCTION" != 1 ]; then
cat >> "$WORK/grub.cfg" <<EOF
menuentry "Unattended install to /dev/vda -- WIPES /dev/vda (QEMU/CI only)" {
    linux /images/pxeboot/vmlinuz $CMDLINE console=ttyS0,115200 rime.unattended rime.disk=/dev/vda rime.user=andre rime.pass=testpass rime.host=rime rime.karg=console=ttyS0,115200 rime.poweroff \$biosfb
    initrd /images/pxeboot/initrd.img
}
EOF
fi
# ── SECURE BOOT: use Fedora's SIGNED shim chain, not a self-built binary ─────
# A `grub2-mkstandalone` BOOTX64.EFI is unsigned, so SB firmware refuses it
# ("Access Denied -- rejected probably by Secure Boot", reproduced under OVMF).
# Instead ship the standard, already-signed chain:
#   BOOTX64.EFI  = shimx64.efi  (signed by the Microsoft UEFI CA → firmware trusts it)
#   grubx64.efi  = Fedora's signed grub2 (shim verifies it against its embedded Fedora cert)
#   mmx64.efi    = MokManager, for enrolling our own key later (Stage B)
# The live kernel is Fedora's, which is already signed, so the whole chain
# validates with SB ON and no user action.
#
# Fedora's signed grub is built with prefix /EFI/fedora, and when loaded from
# /EFI/BOOT it looks for its config next to itself; ship grub.cfg in BOTH places
# so either resolution order finds it.
# `|| true` on every find: this script runs under `set -e`, and `find` exits
# non-zero when any listed path is missing (/usr/share/shim does not exist on a
# stock Fedora rootfs) — which silently killed the build at this step.
SHIM=$(sudo find "$WORK/rootfs/boot/efi" -name 'shimx64.efi' 2>/dev/null | head -1 || true)
GRUBEFI=$(sudo find "$WORK/rootfs/boot/efi" -name 'grubx64.efi' 2>/dev/null | head -1 || true)
MMEFI=$(sudo find "$WORK/rootfs/boot/efi" -name 'mmx64.efi' 2>/dev/null | head -1 || true)
[ -n "$SHIM" ] && [ -n "$GRUBEFI" ] \
  || { echo "BUILD ASSERT FAILED: signed shimx64.efi/grubx64.efi not found in the rootfs (shim-x64 + grub2-efi-x64 installed?)"; exit 1; }
echo "shim:    $SHIM"
echo "grubefi: $GRUBEFI"

# efiboot.img: FAT image holding the whole signed chain (El Torito UEFI image).
# install -m 0644, not cp: Fedora ships these EFI binaries mode 700 root:root and
# `cp` preserves that, so an unprivileged mcopy could not read them
# ("Permission denied") and set -e killed the build.
sudo install -m 0644 "$SHIM"    "$WORK/BOOTX64.EFI"
sudo install -m 0644 "$GRUBEFI" "$WORK/grubx64.efi"
sudo rm -f "$WORK/mmx64.efi"
[ -z "$MMEFI" ] || sudo install -m 0644 "$MMEFI" "$WORK/mmx64.efi"
# mtools runs from the installer image, like xorriso, grub2-mkimage and dracut:
# an ostree build host (a Rime machine) does not ship mmd/mcopy, and a missing
# one used to surface only here, twenty minutes into the build.
sudo rm -f "$WORK/efiboot.img"
sudo podman run --rm --security-opt label=disable -v "$WORK":"$WORK" --entrypoint bash "$IMG" -c '
  set -euo pipefail
  W="$1"
  mkfs.fat -C -n RIMEEFI "$W/efiboot.img" 20480
  mmd   -i "$W/efiboot.img" ::/EFI ::/EFI/BOOT ::/EFI/fedora
  mcopy -i "$W/efiboot.img" "$W/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
  mcopy -i "$W/efiboot.img" "$W/grubx64.efi" ::/EFI/BOOT/grubx64.efi
  mcopy -i "$W/efiboot.img" "$W/grub.cfg"    ::/EFI/BOOT/grub.cfg
  mcopy -i "$W/efiboot.img" "$W/grub.cfg"    ::/EFI/fedora/grub.cfg
  if [ -f "$W/mmx64.efi" ]; then mcopy -i "$W/efiboot.img" "$W/mmx64.efi" ::/EFI/BOOT/mmx64.efi; fi
' _ "$WORK"
sudo test -s "$WORK/efiboot.img" \
  || { echo "BUILD ASSERT FAILED: efiboot.img was not written"; exit 1; }

sudo mkdir -p "$ISOROOT/EFI/BOOT" "$ISOROOT/EFI/fedora" "$ISOROOT/images"
sudo cp "$WORK/BOOTX64.EFI" "$ISOROOT/EFI/BOOT/BOOTX64.EFI"
sudo cp "$WORK/grubx64.efi" "$ISOROOT/EFI/BOOT/grubx64.efi"
sudo cp "$WORK/grub.cfg"    "$ISOROOT/EFI/BOOT/grub.cfg"
sudo cp "$WORK/grub.cfg"    "$ISOROOT/EFI/fedora/grub.cfg"
[ -n "$MMEFI" ] && sudo cp "$WORK/mmx64.efi" "$ISOROOT/EFI/BOOT/mmx64.efi"
sudo cp "$WORK/efiboot.img" "$ISOROOT/images/efiboot.img"

echo "== 6a. bootloader (legacy BIOS grub2: El Torito core + isohybrid MBR) =="
# grub2 for BIOS too, NOT isolinux/syslinux: one bootloader means ONE menu file
# — the exact grub.cfg written above is read unmodified by the BIOS core image
# (its embedded prefix is /boot/grub2), so entries, cmdlines and the safety
# reasoning in the comments can never drift apart between firmwares. Secure
# Boot is untouched: this core image is only ever executed by legacy BIOS
# firmware; UEFI still loads the signed shim chain from step 6. The layout
# (grub2 El Torito entry + grub2-mbr boot code + appended GPT ESP) is the same
# one every shipping Ubuntu hybrid ISO uses, so the xorriso combination in
# step 7 is field-proven rather than invented here.
#
# Built INSIDE the installer image like the dracut step: the host may lack
# grub2-mkimage/the i386-pc module set (grub2-pc-modules is in the image for
# exactly this). The embedded module list covers everything grub.cfg executes
# (search_label for the root hunt, serial+terminal+test+echo for the guarded
# console setup, linux+boot to start the kernel, part_msdos+part_gpt+biosdisk+
# iso9660 so a dd'd USB enumerates). all_video stays embedded only as a
# videoinfo debugging aid at the grub prompt — the BIOS framebuffer is set by
# the KERNEL via vga=791 because Fedora's BIOS grub cannot set it (see the
# grub.cfg heredoc comment). The full i386-pc tree is ALSO shipped on the ISO
# at /boot/grub2/i386-pc so any future grub.cfg edit that needs one more
# module autoloads it from the medium instead of dying with "command not
# found" only on BIOS machines.
sudo rm -rf "$WORK/grub-i386-pc"
sudo podman run --rm --security-opt label=disable -v "$WORK":/w "$IMG" \
  bash -c "grub2-mkimage -O i386-pc-eltorito -d /usr/lib/grub/i386-pc -p /boot/grub2 \
      -o /w/eltorito.img \
      biosdisk iso9660 part_msdos part_gpt normal search search_label configfile \
      linux echo test serial terminal all_video boot \
    && cp /usr/lib/grub/i386-pc/boot_hybrid.img /w/boot_hybrid.img \
    && mkdir -p /w/grub-i386-pc \
    && cp /usr/lib/grub/i386-pc/*.mod /usr/lib/grub/i386-pc/*.lst /w/grub-i386-pc/"
sudo mkdir -p "$ISOROOT/boot/grub2/i386-pc"
sudo cp "$WORK/eltorito.img"      "$ISOROOT/images/eltorito.img"
sudo cp "$WORK/grub.cfg"          "$ISOROOT/boot/grub2/grub.cfg"
sudo cp -a "$WORK/grub-i386-pc/." "$ISOROOT/boot/grub2/i386-pc/"

echo "== 6b. build-time invariants (fail loudly rather than ship a broken ISO) =="
# CRITICAL-1 shipped because nothing asserted the live env could actually run the
# installer: `clear` (ncurses) was missing, so every install died the moment the
# user confirmed. Assert the things the installer depends on, in the ROOTFS.
_need_bin() { sudo test -x "$WORK/rootfs/usr/bin/$1" || sudo test -x "$WORK/rootfs/usr/sbin/$1" \
    || { echo "BUILD ASSERT FAILED: /usr/bin/$1 missing from the live rootfs"; exit 1; }; }
# skopeo is not optional on a netinstall ISO: it is what stages the image to
# disk instead of the RAM overlay. df likewise — the scratch chooser uses it.
for b in clear podman skopeo lsblk useradd chpasswd mount umount blkid udevadm partprobe awk sed \
         mkfs.btrfs findmnt tput mktemp basename dirname chroot tee find df grep \
         mokutil efibootmgr; do
  _need_bin "$b"
done
sudo test -x "$WORK/rootfs/usr/bin/rime-install" \
  || { echo "BUILD ASSERT FAILED: rime-install missing"; exit 1; }
sudo bash -n "$WORK/rootfs/usr/bin/rime-install" \
  || { echo "BUILD ASSERT FAILED: rime-install has a syntax error"; exit 1; }

# ── The GUI is now the ONLY front end — assert it can actually come up ───────
# whiptail is deliberately NOT in the list above any more: the text installer is
# gone. That removes the safety net this script used to lean on, so everything
# the graphical installer needs has to be proven HERE, in the rootfs that is
# about to be sealed into a squashfs, not just in the container image it came
# from. Every failure in this area so far has been silent — a missing typelib,
# a missing seat backend, absent firmware — and each one shipped an ISO that
# booted to a black screen. If any of these is missing there is no fallback UI
# left to rescue the user, so the build must stop instead.
for b in cage seatd Xwayland rime-installer-gui rime-installer-launch \
         rime-installer-session; do
  _need_bin "$b"
done
sudo chroot "$WORK/rootfs" python3 -c \
  'import gi; gi.require_version("Gtk","4.0"); gi.require_version("Adw","1"); from gi.repository import Gtk, Adw, Gdk, GLib, Gio, Pango' \
  || { echo "BUILD ASSERT FAILED: the live rootfs cannot import GTK4/libadwaita — the GUI would not start (cairo typelib / gobject-introspection missing again?)"; exit 1; }
sudo chroot "$WORK/rootfs" python3 -m py_compile /usr/bin/rime-installer-gui \
  || { echo "BUILD ASSERT FAILED: rime-installer-gui has a Python syntax error"; exit 1; }
sudo rm -rf "$WORK/rootfs/usr/bin/__pycache__"
sudo bash -n "$WORK/rootfs/usr/bin/rime-installer-launch" \
  || { echo "BUILD ASSERT FAILED: rime-installer-launch has a syntax error"; exit 1; }
sudo bash -n "$WORK/rootfs/usr/bin/rime-installer-session" \
  || { echo "BUILD ASSERT FAILED: rime-installer-session has a syntax error"; exit 1; }
# The launcher EXECS the session script. A rootfs without it has no installer at
# all — cage never starts — and that is exactly the class of silent failure this
# block exists to catch, so resolve the target rather than trusting the list.
_sess_target=$(grep -oE '^GUI_CMD=\(([^ )]+)' "$WORK/rootfs/usr/bin/rime-installer-launch" | cut -d'(' -f2)
sudo test -x "$WORK/rootfs$_sess_target" \
  || { echo "BUILD ASSERT FAILED: rime-installer-launch execs $_sess_target, absent from the live rootfs"; exit 1; }
# The keyboard page reads its layout list from here. Without it the page falls
# back to a short built-in list, which is a quietly worse installer.
sudo test -r "$WORK/rootfs/usr/share/X11/xkb/rules/base.lst" \
  || { echo "BUILD ASSERT FAILED: xkeyboard-config's base.lst missing from the live rootfs"; exit 1; }
# Enablement, not just presence. An installed-but-not-enabled seatd is exactly
# the kind of thing that looks fine in `rpm -q` and leaves cage unable to take
# a seat on tty1 at boot, which is a black screen with no diagnosis.
#
# -L, not -e. These are symlinks whose targets are ABSOLUTE paths inside the
# live rootfs (/usr/lib/systemd/system/…), and `test -e` FOLLOWS them — which
# resolves against the build host's root, where those units do not exist. The
# first version of this assert failed on a rootfs that was in fact correct.
for u in rime-installer.service seatd.service; do
  sudo test -L "$WORK/rootfs/etc/systemd/system/multi-user.target.wants/$u" \
    || { echo "BUILD ASSERT FAILED: $u is not enabled in the live rootfs"; exit 1; }
done
sudo test -L "$WORK/rootfs/etc/systemd/system/getty@tty1.service" \
  || { echo "BUILD ASSERT FAILED: getty@tty1 is not masked — it would fight cage for tty1"; exit 1; }
echo "asserts OK: GUI stack present, importable, and enabled"

# ── BIOS boot artifacts: prove the legacy path exists before xorriso runs ────
# The ISO is dual-firmware now. A missing or truncated BIOS core image would
# still produce an ISO that boots fine on every UEFI machine we test on, and
# the regression would only surface in the field on exactly the machines this
# path exists for — so a build that cannot prove the BIOS artifacts must stop.
sudo test -s "$WORK/eltorito.img" \
  || { echo "BUILD ASSERT FAILED: eltorito.img (BIOS grub core) missing or empty"; exit 1; }
_sz=$(sudo stat -c%s "$WORK/eltorito.img")
[ "$_sz" -ge 100000 ] \
  || { echo "BUILD ASSERT FAILED: eltorito.img is only $_sz bytes — grub2-mkimage embedded too little (module list wrong?)"; exit 1; }
_sz=$(sudo stat -c%s "$WORK/boot_hybrid.img")
[ "$_sz" -gt 0 ] && [ "$_sz" -le 512 ] \
  || { echo "BUILD ASSERT FAILED: boot_hybrid.img is $_sz bytes — must be 1..512 to fit the MBR boot-code area"; exit 1; }
sudo cmp -s "$WORK/grub.cfg" "$ISOROOT/boot/grub2/grub.cfg" \
  || { echo "BUILD ASSERT FAILED: /boot/grub2/grub.cfg missing or differs from the UEFI menu — the one-menu-for-both-firmwares invariant is broken"; exit 1; }
sudo test -f "$ISOROOT/boot/grub2/i386-pc/normal.mod" \
  || { echo "BUILD ASSERT FAILED: i386-pc module tree missing from the ISO (BIOS grub could not autoload anything)"; exit 1; }
# The config must actually get a framebuffer set on BIOS, or driverless legacy
# machines boot to the DRM-nodes-absent bug screen instead of the GUI (the
# single most likely way to break this path — see the vga=791 comment in the
# grub.cfg heredoc: gfxpayload is a NO-OP on Fedora's BIOS grub, the kernel
# has to set the mode itself). Check both the guard that sets biosfb and that
# at least the default + safe-graphics entries actually reference it.
grep -q 'biosfb=vga=791' "$WORK/grub.cfg" \
  || { echo "BUILD ASSERT FAILED: grub.cfg lost its BIOS vga=791 framebuffer handoff — driverless legacy machines would get no GUI"; exit 1; }
[ "$(grep -c '\$biosfb' "$WORK/grub.cfg")" -ge 2 ] \
  || { echo "BUILD ASSERT FAILED: fewer than 2 menu entries reference biosfb — the vga=791 handoff is set but unused"; exit 1; }
echo "asserts OK: BIOS grub core, isohybrid MBR, shared menu, module tree, vga=791 handoff"
# Production must NOT carry the unattended marker.
if [ "$PRODUCTION" = 1 ]; then
  if sudo test -e "$WORK/rootfs/usr/share/rime-installer/allow-unattended"; then
    echo "BUILD ASSERT FAILED: production build contains the unattended marker"; exit 1; fi
  if grep -qi 'rime.unattended' "$WORK/grub.cfg"; then
    echo "BUILD ASSERT FAILED: production grub.cfg contains an unattended entry"; exit 1; fi
  echo "asserts OK: no unattended marker, no unattended menu entry"
else
  echo "asserts OK (test build: unattended intentionally present)"
fi

echo "== 7. xorriso: hybrid BIOS+UEFI ISO (El Torito for CD/QEMU + MBR/GPT for dd'd USB) =="
# -appended_part_as_gpt + -partition_offset 16: without these the image carries an
# MBR-only table whose partition 1 starts at LBA 0, which some UEFI firmwares
# dislike when booting from USB. Produces a valid GPT with the ESP intact and the
# RIME-INSTALL label still resolvable from both whole-disk and partition views.
#
# BIOS side (everything before -eltorito-alt-boot): -b makes the grub2 core the
# FIRST El Torito entry, which BIOS firmware picks when booting the ISO as a
# CD; --grub2-boot-info patches the core with its own LBA so the SAME core can
# also be entered from the MBR path; --grub2-mbr installs grub's boot_hybrid
# code in the system area so a dd'd USB stick boots on legacy BIOS;
# --mbr-force-bootable sets the active flag some BIOSes insist on before they
# will boot a disk at all. The UEFI side is UNCHANGED from the UEFI-only build:
# same efiboot.img as an alt El Torito entry, same appended GPT ESP — a UEFI
# machine (Secure Boot included) sees exactly what it saw before. This exact
# combination is what Ubuntu's shipping hybrid ISOs use.
# xorriso runs from the installer image rather than the host: it is not in the
# Rime image, and an ostree host cannot just dnf install it. Same binary the
# live env carries.
sudo podman run --rm --security-opt label=disable -v "$WORK":"$WORK" \
    --entrypoint xorriso "$IMG" -as mkisofs \
    -iso-level 3 -rational-rock -joliet -joliet-long \
    -V "$LABEL" \
    --grub2-mbr "$WORK/boot_hybrid.img" \
    --mbr-force-bootable \
    -b images/eltorito.img -no-emul-boot -boot-load-size 4 \
    -boot-info-table --grub2-boot-info \
    -eltorito-alt-boot \
    -e images/efiboot.img -no-emul-boot \
    -append_partition 2 0xef "$WORK/efiboot.img" \
    -appended_part_as_gpt -partition_offset 16 \
    -o "$OUT" "$ISOROOT"

# Post-build proof that the EMITTED image advertises both firmware paths.
# xorriso reads back its own boot records; the four properties below are
# precisely what each firmware needs (BIOS: x86 El Torito entry + MBR boot
# code; UEFI: EFI El Torito entry + GPT ESP). If any is absent the image
# cannot boot somewhere we claim it does, so it must not ship.
_rep=$(sudo podman run --rm --security-opt label=disable -v "$WORK":"$WORK" \
    --entrypoint xorriso "$IMG" -indev "$OUT" -report_el_torito plain -report_system_area plain 2>/dev/null)
echo "$_rep" | grep -q 'El Torito boot img :   1  BIOS' \
  || { echo "BUILD ASSERT FAILED: emitted ISO has no BIOS El Torito boot entry"; exit 1; }
echo "$_rep" | grep -q 'El Torito boot img :   2  UEFI' \
  || { echo "BUILD ASSERT FAILED: emitted ISO has no UEFI El Torito boot entry"; exit 1; }
echo "$_rep" | grep -q 'grub2-mbr' \
  || { echo "BUILD ASSERT FAILED: emitted ISO system area lacks the grub2 isohybrid MBR (dd'd USB would not BIOS-boot)"; exit 1; }
echo "$_rep" | grep -q '28732ac11ff8d211ba4b00a0c93ec93b' \
  || { echo "BUILD ASSERT FAILED: emitted ISO has no ESP-typed GPT partition (UEFI-from-USB regression)"; exit 1; }
echo "asserts OK: emitted ISO carries BIOS+UEFI El Torito entries, isohybrid MBR, GPT ESP"

# Checksum the ISO we just built (it used to record the PREVIOUS build's hash).
sudo sha256sum "$OUT" | sudo tee "$OUT.sha256" >/dev/null
echo "== DONE: $OUT =="
ls -lh "$OUT"; cat "$OUT.sha256"

# A netinstall ISO is only as durable as the digest it pins: once :rime moves
# on, that digest is untagged, and a registry cleanup of untagged versions
# would break this ISO for everyone who downloads it. Pinning is a release
# step, run from CI where the token can write packages.
if [ "$NETINSTALL" = 1 ] && [ "$PRODUCTION" = 1 ]; then
  echo "== BEFORE PUBLISHING =="
  echo "This ISO downloads $RELEASE_IMAGE."
  echo "Give that digest a durable tag, so no GHCR cleanup can ever remove it:"
  # RELEASE=v2.1.0 makes this line runnable as printed. Without it the release
  # is a placeholder, and it is labelled as one instead of looking finished.
  if printf '%s' "${RELEASE:-}" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "  gh workflow run pin-netinstall-image.yml -f digest=$RELEASE_DIGEST -f release=$RELEASE"
  else
    echo "  gh workflow run pin-netinstall-image.yml -f digest=$RELEASE_DIGEST -f release=<RELEASE>"
    echo "  (replace <RELEASE> with the release this ISO ships in, e.g. v2.1.0, or build with RELEASE=v2.1.0)"
  fi
fi
