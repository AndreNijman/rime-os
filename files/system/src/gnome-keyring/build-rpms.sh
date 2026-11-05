#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  build-rpms.sh OUTDIR
#  build-rpms.sh --classify SOURCE_ROOT
#
#  Follows Fedora. The gnome-keyring installed here (Containerfile.base runs
#  this in a stage FROM core, so: whatever core ships) names the Fedora source
#  package. That SRPM is fetched and accepted only when rpmkeys verifies it
#  against Fedora's own release key for that Fedora version: from the source
#  repositories first (`dnf5 download --source`), then from Fedora's build
#  system (Koji), which keeps every build after the repositories move on.
#  No checksum of ours is pinned, so a Fedora update never stops a release.
#
#  Fedora's prepared source (%prep, Fedora's own patches included) then decides:
#
#    patched   gnome-keyring-property-read-no-client.patch (beside this script)
#              applies: it is added to the spec, the release gets `.rime1`,
#              the fix's markers are checked in the prepared source, and
#              gnome-keyring + gnome-keyring-pam are rebuilt into OUTDIR. When
#              the version is not the one the race was last proven on, says so
#              as a GitHub warning: the patch was rebased onto a new Fedora.
#    upstream  Fedora's source already has the fix (the patch reverse-applies,
#              or the session lookup creates the client and no longer returns
#              NULL without an error): Fedora's own gnome-keyring-pam, signature
#              verified the same way, goes to OUTDIR, and a GitHub warning says
#              the carried patch can be dropped. The release goes on.
#    fail      neither: the patch does not apply and the fix is not there. The
#              ONLY case a human has to look at; the build stops.
#
#  OUTDIR/result records MODE, NVR (Fedora's), BUILT (what gets installed) and
#  PROVEN (the last version the race test was run on). Containerfile.base's
#  install step reads it, and so does the PR race test.
#
#  --classify SOURCE_ROOT prints patched, upstream or fail for an unpacked
#  gnome-keyring source tree (exit 0, 0, 1) and touches nothing: the decision
#  above, on its own, for tests.
#
#  Needs: rpm-build, git-core, curl, dnf5's builddep and download commands, and
#  root (builddep).
# ─────────────────────────────────────────────────────────────────────────────
set -Eeuo pipefail
trap 'echo "FATAL: $0 stopped at line $LINENO (exit $?)" >&2' ERR

HERE="$(cd "$(dirname "$0")" && pwd)"
PATCH=gnome-keyring-property-read-no-client.patch
# The version the stock-dies / patched-survives race was last measured on.
PROVEN=gnome-keyring-51.1-1.fc45
KOJI=https://kojipkgs.fedoraproject.org/packages
KEY_DIRS=(/usr/share/pki/rpm-gpg /etc/pki/rpm-gpg)
die() { echo "FATAL: $*" >&2; exit 1; }

# ── The decision ─────────────────────────────────────────────────────────────
# Does SOURCE_ROOT already carry the fix without Rime's patch? Either the patch
# reverse-applies, or both halves are there in Fedora's own words: the session
# lookup creates the client first, and the object lookup no longer returns
# NULL through g_return_val_if_fail when there is no session.
fix_upstream() {
    local root="$1" svc obj
    git_apply "$root" -R --check && return 0
    svc="$root/daemon/dbus/gkd-secret-service.c"
    obj="$root/daemon/dbus/gkd-secret-objects.c"
    awk '/^gkd_secret_service_get_pkcs11_session[[:space:]]*\(/ { f = 1 }
         f && /g_hash_table_lookup[[:space:]]*\(self->clients/ { exit }
         f && /gkd_secret_service_ensure_client[[:space:]]*\(/ { found = 1; exit }
         f && /^}/ { exit }
         END { exit !found }' "$svc" || return 1
    awk '/^secret_objects_lookup_gck_object_for_path[[:space:]]*\(/ { f = 1 }
         f && /g_return_val_if_fail[[:space:]]*\(session[[:space:],)]/ { bad = 1; exit }
         f && /^}/ { exit }
         END { exit bad }' "$obj"
}

# git apply PATCH in $1, never in a repository above it: from a subdirectory of
# one, git skips the paths outside it and a --check would pass on nothing.
git_apply() {
    local root="$1"; shift
    ( cd "$root" && GIT_CEILING_DIRECTORIES="$(dirname "$root")" \
        git apply "$@" "$HERE/$PATCH" ) >/dev/null 2>&1
}

classify() {
    local root="$1"
    [ -f "$root/daemon/dbus/gkd-secret-service.c" ] && [ -f "$root/daemon/dbus/gkd-secret-objects.c" ] \
        || die "$root is not a gnome-keyring source tree (no daemon/dbus/gkd-secret-{service,objects}.c)"
    if git_apply "$root" --check; then
        echo patched
    elif fix_upstream "$root"; then
        echo upstream
    else
        echo fail
    fi
}

if [ "${1:-}" = --classify ]; then
    [ $# -eq 2 ] || { echo "usage: $0 --classify SOURCE_ROOT" >&2; exit 2; }
    m="$(classify "$(cd "$2" && pwd)")"
    echo "$m"
    if [ "$m" = fail ]; then exit 1; fi
    exit 0
fi

[ $# -eq 1 ] || { echo "usage: $0 OUTDIR | --classify SOURCE_ROOT" >&2; exit 2; }
OUT=$1

# ── What core ships ─────────────────────────────────────────────────────────
NVR="$(rpm -q --qf '%{NAME}-%{VERSION}-%{RELEASE}' gnome-keyring)" \
    || die "gnome-keyring is not installed here: nothing to follow"
SRPM="$(rpm -q --qf '%{SOURCERPM}' gnome-keyring)"
[ "$SRPM" = "${NVR}.src.rpm" ] || die "gnome-keyring's source package is '$SRPM', not ${NVR}.src.rpm"
V="${NVR#gnome-keyring-}"; VER="${V%-*}"; REL="${V##*-}"
DIST="${REL##*.}"
case "$DIST" in fc[0-9]*) ;; *) die "'$NVR' is not a Fedora build (release $REL)";; esac
case "$REL" in *.rime[0-9]*) die "core already ships a Rime build ($NVR): nothing to follow";; esac
ARCH="$(rpm -E %_arch)"
echo "gnome-keyring: core ships ${NVR}"

W="$(mktemp -d)"; T="$W/topdir"
trap 'rm -rf "$W"' EXIT

# ── Fedora's key for that Fedora version ────────────────────────────────────
KEY=""
for d in "${KEY_DIRS[@]}"; do
    [ -f "$d/RPM-GPG-KEY-fedora-${DIST#fc}-primary" ] && { KEY="$d/RPM-GPG-KEY-fedora-${DIST#fc}-primary"; break; }
done
[ -n "$KEY" ] || die "no Fedora ${DIST#fc} release key in ${KEY_DIRS[*]}"
KR="$W/keyring"; mkdir -p "$KR"
rpmkeys --dbpath "$KR" --import "$KEY"
KID="$(rpm --dbpath "$KR" -q gpg-pubkey --qf '%{VERSION}\n' | head -n 1)"
[[ "$KID" =~ ^[0-9a-fA-F]{8,}$ ]] || die "could not read the key id of $KEY"
KID="${KID,,}"; KID="${KID: -8}"

# The literal words, never the exit code: an unsigned rpm answers "digests OK" and 0.
signed_by_fedora() {
    local out
    out="$(rpmkeys --dbpath "$KR" --checksig -- "$1" 2>&1)" || true
    echo "  $out"
    [[ "$out" = *"digests signatures OK"* ]]
}

# An rpm is a 96-byte lead, the signature header (16 bytes + 16 per entry +
# data, padded to 8), then what the signature covers; Koji's sigcache holds the
# signature header alone. Same splice as rime-pkg's.
splice_sighdr() {
    local unsigned="$1" sig="$2" out="$3" il dl size
    [ "$(od -A n -t x1 -N 4 -- "$sig" | tr -d ' \n')" = 8eade801 ] || return 1
    [ "$(od -A n -t x1 -j 96 -N 4 -- "$unsigned" | tr -d ' \n')" = 8eade801 ] || return 1
    read -r il dl < <(od -A n -t u4 --endian=big -j 104 -N 8 -- "$unsigned") || return 1
    [[ "${il:-x}" =~ ^[0-9]{1,5}$ && "${dl:-x}" =~ ^[0-9]{1,8}$ ]] || return 1
    size=$(( (16 + 16 * il + dl + 7) / 8 * 8 ))
    { head -c 96 -- "$unsigned"; cat -- "$sig"; tail -c +"$(( 96 + size + 1 ))" -- "$unsigned"; } > "$out"
}

kget() { curl -fsSL --retry 3 --proto '=https' --max-time 300 -o "$2" -- "$1" 2>/dev/null; }

# fetch NEVRA-SPEC FILE ARCHDIR DEST: the package Fedora signed, verified, or fail.
fetch_signed() {
    local spec="$1" file="$2" adir="$3" dest="$4" base="${KOJI}/gnome-keyring/${VER}/${REL}"
    local d="$W/fetch.$RANDOM" src=(); mkdir -p "$d"
    [ "$adir" = src ] && src=(--source)
    if dnf5 -q download "${src[@]}" --destdir "$d" "$spec" >/dev/null 2>&1 \
       && [ -f "$d/$file" ]; then
        echo "gnome-keyring: $file from Fedora's repositories"
        signed_by_fedora "$d/$file" && { mv "$d/$file" "$dest"; return 0; }
        echo "gnome-keyring: the repositories' copy is not signed by Fedora ${DIST#fc}'s key; trying Koji"
    fi
    rm -f "$d"/*
    if kget "${base}/data/signed/${KID}/${adir}/${file}" "$d/$file"; then
        echo "gnome-keyring: $file from Koji (signed copy)"
    elif kget "${base}/${adir}/${file}" "$d/unsigned" \
         && kget "${base}/data/sigcache/${KID}/${adir}/${file}.sig" "$d/sig" \
         && splice_sighdr "$d/unsigned" "$d/sig" "$d/$file"; then
        echo "gnome-keyring: $file from Koji (signature header spliced from its sigcache)"
    else
        return 1
    fi
    signed_by_fedora "$d/$file" && mv "$d/$file" "$dest"
}

fetch_signed "$NVR" "${NVR}.src.rpm" src "$W/${NVR}.src.rpm" \
    || die "could not get ${NVR}.src.rpm signed by Fedora ${DIST#fc}'s key (repositories and Koji both tried)"
[ "$(rpm -qp --qf '%{NAME}-%{VERSION}-%{RELEASE}' "$W/${NVR}.src.rpm" 2>/dev/null)" = "$NVR" ] \
    || die "the fetched source package is not ${NVR}"

rpm -i --define "_topdir ${T}" "$W/${NVR}.src.rpm"
SPEC="${T}/SPECS/gnome-keyring.spec"
[ -f "$SPEC" ] || die "${NVR}.src.rpm has no gnome-keyring.spec"

# Fedora's prepared source, its own patches applied, decides. (--nodeps: %prep
# needs only the unpacker and git, not the build dependencies.)
rpmbuild --define "_topdir ${T}" --nodeps -bp "$SPEC" >/dev/null \
    || die "rpmbuild -bp of Fedora's own ${NVR} failed"
SVC="$(find "${T}/BUILD" -path '*/daemon/dbus/gkd-secret-service.c' -print -quit)"
[ -n "$SVC" ] || die "Fedora's prepared ${NVR} has no daemon/dbus/gkd-secret-service.c"
ROOT="${SVC%/daemon/dbus/gkd-secret-service.c}"
MODE="$(classify "$ROOT")"
rm -rf "${T}/BUILD" "${T}/BUILDROOT"
mkdir -p "$OUT"

case "$MODE" in
upstream)
    echo "::warning title=gnome-keyring fix is upstream::Fedora's ${NVR} already contains the property-read fix. Rime installs Fedora's build unchanged; the carried patch (files/system/src/gnome-keyring/) can be dropped."
    PAM="gnome-keyring-pam-${V}.${ARCH}.rpm"
    fetch_signed "gnome-keyring-pam-${V}.${ARCH}" "$PAM" "$ARCH" "$OUT/$PAM" \
        || die "could not get ${PAM} signed by Fedora ${DIST#fc}'s key"
    BUILT="$NVR"
    ;;
patched)
    cp "$HERE/$PATCH" "${T}/SOURCES/"
    if grep -E '^Release:[[:space:]]+%autorelease$' "$SPEC" >/dev/null; then
        sed -i -E 's/^(Release:[[:space:]]+)%autorelease$/\1%autorelease -e rime1/' "$SPEC"
    elif grep -E '^Release:[[:space:]]+[0-9][0-9.]*%\{\?dist\}$' "$SPEC" >/dev/null; then
        sed -i -E 's/^(Release:[[:space:]]+[0-9][0-9.]*)%\{\?dist\}$/\1.rime1%{?dist}/' "$SPEC"
    else
        grep -E '^Release:' "$SPEC" >&2 || true
        die "gnome-keyring.spec's Release line (above) is neither %autorelease nor N%{?dist}: the .rime1 suffix needs a human"
    fi
    grep -E '^%autosetup ' "$SPEC" >/dev/null \
        || die "gnome-keyring.spec no longer applies its patches with %autosetup: Rime's patch would not be applied"
    # After Fedora's own patches (applied in number order), else after Source0.
    last="$(grep -nE '^Patch[0-9]*:' "$SPEC" | tail -n 1 | cut -d: -f1 || true)"
    [ -n "$last" ] || last="$(grep -m1 -nE '^Source0?:' "$SPEC" | cut -d: -f1 || true)"
    [ -n "$last" ] || die "gnome-keyring.spec has no Source0 line to put the patch after"
    sed -i "${last}a Patch9001:      ${PATCH}" "$SPEC"

    dnf5 -y builddep "$SPEC" >/dev/null

    # %prep with the patch: the fix must be in the source that gets compiled,
    # not only in SOURCES (rpmbuild -bb removes its build tree when it succeeds).
    rpmbuild --define "_topdir ${T}" -bp "$SPEC" >/dev/null \
        || die "rpmbuild -bp of ${NVR} with Rime's patch failed (errors above)"
    SVC="$(find "${T}/BUILD" -path '*/daemon/dbus/gkd-secret-service.c' -print -quit)"
    OBJ="$(find "${T}/BUILD" -path '*/daemon/dbus/gkd-secret-objects.c' -print -quit)"
    [ -n "$SVC" ] && [ -n "$OBJ" ] || die "the prepared source has no daemon/dbus/gkd-secret-{service,objects}.c"
    [ "$(grep -c 'gkd_secret_service_ensure_client (self, caller);' "$SVC")" -ge 2 ] \
        || die "the prepared gkd-secret-service.c does not create the client where it is needed"
    grep -F 'No session for the caller to look up' "$OBJ" >/dev/null \
        || die "the prepared gkd-secret-objects.c still returns NULL without an error"

    rpmbuild --define "_topdir ${T}" --define "_smp_mflags -j$(nproc)" -bb "$SPEC" \
        || die "rpmbuild -bb of the patched ${NVR} failed (compiler output above)"
    for p in gnome-keyring gnome-keyring-pam; do
        set -- "${T}/RPMS/${ARCH}/${p}-${VER}-"*".rime1.${DIST}.${ARCH}.rpm"
        [ $# -eq 1 ] && [ -f "$1" ] || { ls -l "${T}/RPMS/${ARCH}" >&2; die "the build made no single ${p}-${VER}-*.rime1.${DIST}.${ARCH}.rpm"; }
        [ "$(rpm -qp --qf '%{NAME}' "$1")" = "$p" ] || die "$1 is not $p"
        cp "$1" "$OUT/"
    done
    BUILT="$(rpm -qp --qf '%{NAME}-%{VERSION}-%{RELEASE}' "$OUT/gnome-keyring-${VER}-"*.rpm)"
    if [ "$NVR" = "$PROVEN" ]; then
        echo "::notice title=gnome-keyring::Rime's property-read fix applied to Fedora's ${NVR}: built ${BUILT}."
    else
        echo "::warning title=gnome-keyring rebased::Rime's property-read fix REBASED onto Fedora's ${NVR} (last proven on ${PROVEN}): it applied and its markers are in the prepared source; built ${BUILT}. Re-run the race test (tests/test-rime-keyring-property-read.sh) and update PROVEN in build-rpms.sh."
    fi
    ;;
*)
    die "Rime's property-read patch does not apply to Fedora's ${NVR}, and that source does not already contain the fix. A human has to rebase files/system/src/gnome-keyring/${PATCH} (or confirm Fedora fixed it another way)."
    ;;
esac

printf 'MODE=%s\nNVR=%s\nBUILT=%s\nPROVEN=%s\n' "$MODE" "$NVR" "$BUILT" "$PROVEN" > "$OUT/result"
echo "gnome-keyring: $(tr '\n' ' ' < "$OUT/result")"
ls -l "$OUT"
