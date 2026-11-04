#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  build-rpms.sh NVR SRPM_SHA256 OUTDIR
#
#  Fedora's gnome-keyring SRPM at exactly NVR (sha256-pinned, from Koji), with
#  gnome-keyring-property-read-no-client.patch (beside this script) added and
#  the release marked `.rime1`, rebuilt into gnome-keyring and gnome-keyring-pam
#  in OUTDIR. The ONE recipe: Containerfile.base's gnome-keyring-builder stage
#  and the PR-validation race test both run this file, so the packages CI
#  measures are built exactly as the image's are.
#
#  Fails, and says why, when:
#    • the SRPM's sha256 is not the pinned one;
#    • the spec changed in a way the edits below would get wrong (its Release
#      line, or Fedora now carrying patches of its own: one of them may BE
#      this fix, and a human has to look);
#    • the patch does not apply, or applies and the prepared source lacks it;
#    • the build does not produce exactly the two `.rime1` packages.
#
#  Needs: rpm-build, dnf5's builddep command, curl, and root (builddep).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 NVR SRPM_SHA256 OUTDIR" >&2; exit 2; }
NVR=$1 SHA=$2 OUT=$3
HERE="$(cd "$(dirname "$0")" && pwd)"
PATCH=gnome-keyring-property-read-no-client.patch
die() { echo "FATAL: $*" >&2; exit 1; }

case "$NVR" in gnome-keyring-*-*.fc[0-9]*) ;; *) die "'$NVR' is not a gnome-keyring NVR";; esac
V="${NVR#gnome-keyring-}"; VER="${V%-*}"; REL="${V##*-}"
DIST="${REL##*.}"             # fc45
BASEREL="${REL%."$DIST"}"     # 1
RIMEREL="${BASEREL}.rime1.${DIST}"

W="$(mktemp -d)"; T="$W/topdir"
trap 'rm -rf "$W"' EXIT

curl -fsSL --retry 3 --proto '=https' -o "$W/$NVR.src.rpm" \
    "https://kojipkgs.fedoraproject.org/packages/gnome-keyring/${VER}/${REL}/src/${NVR}.src.rpm"
echo "${SHA}  $W/$NVR.src.rpm" | sha256sum -c - >/dev/null \
    || die "${NVR}.src.rpm is not the pinned build (sha256 $(sha256sum "$W/$NVR.src.rpm" | cut -d' ' -f1), want ${SHA})"

rpm -i --define "_topdir ${T}" "$W/$NVR.src.rpm"
cp "$HERE/$PATCH" "${T}/SOURCES/"
SPEC="${T}/SPECS/gnome-keyring.spec"

grep -E '^Release:[[:space:]]+%autorelease$' "$SPEC" >/dev/null \
    || die "gnome-keyring.spec's Release line changed; re-check the .rime1 suffix"
# Fedora's 51.1 spec carries no patches. If it ever does, one of them may be
# this very fix (or touch the same lines): stop and let a human decide.
if grep -E '^Patch[0-9]*:' "$SPEC" >/dev/null; then
    grep -E '^Patch[0-9]*:' "$SPEC" >&2
    die "${NVR} now carries patches of its own (above): check whether Fedora fixed the property-read abort before adding Rime's"
fi
grep -E '^Source0:' "$SPEC" >/dev/null || die "gnome-keyring.spec has no Source0 line to put the patch after"
sed -i -E 's/^(Release:[[:space:]]+)%autorelease$/\1%autorelease -e rime1/' "$SPEC"
sed -i "/^Source0:/a Patch0:         ${PATCH}" "$SPEC"
grep -E '^%autosetup ' "$SPEC" >/dev/null || die "gnome-keyring.spec no longer applies its patches with %autosetup"

dnf5 -y builddep "$SPEC" >/dev/null

# %prep alone first: the fix must be in the source that gets compiled, not only
# in SOURCES (rpmbuild -bb removes its build tree when it succeeds).
rpmbuild --define "_topdir ${T}" -bp "$SPEC" >/dev/null \
    || die "rpmbuild -bp failed: does the patch still apply to ${NVR}? (errors above)"
SVC="$(find "${T}/BUILD" -path '*/daemon/dbus/gkd-secret-service.c' | head -n 1)"
OBJ="$(find "${T}/BUILD" -path '*/daemon/dbus/gkd-secret-objects.c' | head -n 1)"
[ -n "$SVC" ] && [ -n "$OBJ" ] || die "the prepared source has no daemon/dbus/gkd-secret-{service,objects}.c"
[ "$(grep -c 'gkd_secret_service_ensure_client (self, caller);' "$SVC")" -ge 2 ] \
    || die "the prepared gkd-secret-service.c does not create the client where it is needed"
grep -F 'No session for the caller to look up' "$OBJ" >/dev/null \
    || die "the prepared gkd-secret-objects.c still returns NULL without an error"

rpmbuild --define "_topdir ${T}" --define "_smp_mflags -j$(nproc)" -bb "$SPEC" \
    || die "rpmbuild -bb of the patched ${NVR} failed (compiler output above)"

mkdir -p "$OUT"
for p in gnome-keyring gnome-keyring-pam; do
    f="${T}/RPMS/x86_64/${p}-${VER}-${RIMEREL}.x86_64.rpm"
    [ -f "$f" ] || { ls -l "${T}/RPMS/x86_64" >&2; die "the build made no ${p}-${VER}-${RIMEREL}.x86_64.rpm"; }
    cp "$f" "$OUT/"
done
ls -l "$OUT"
