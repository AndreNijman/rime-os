#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-pkg-rpm6.sh — the package engine's trust chain against a REAL
#  rpm 6 (Fedora 45), with real OpenPGP signatures.
#
#  ── Why this exists ─────────────────────────────────────────────────────────
#  Every other suite either fakes rpm or never signs anything, so the four
#  places where the engine depends on what rpm itself says were untested:
#
#    * rpm_signature_ok matches the text "digests signatures OK". If rpm ever
#      words it differently, every package is refused (or, worse, a test that
#      fakes rpmkeys keeps passing).
#    * refresh_copr_keyring ran `rpm --dbpath <dir> --initdb`. On Fedora 45
#      (rpm 6.1, SELinux enforcing) that is refused with "can't create
#      transaction lock on <dir>/.rpm.lock (Permission denied)", exit 255, as
#      root, in /var/lib and /var/tmp alike. It only kept working because its
#      callers run it under `if !`, which switches errexit off. It also deleted
#      the keyring BEFORE importing, so one unreadable key file took every
#      COPR package's verification with it.
#    * extract_rpms passes --nosignature, because rpm 6.1 under
#      %_pkgverify_level all refuses to install a package whose key is not in
#      the (empty) throwaway root, even one the system trusts. That is only
#      safe because guard_rpms verified every file first.
#    * the ChatGPT route verifies against a one-file keyring with
#      `--define '_keyring fs'`, which nothing exercised past the fingerprint
#      check.
#
#  The keyring is also held to its mechanism, not just its result: an mv
#  wrapper requires the swap to be exactly one `mv --exchange` (a remove then a
#  rename also changes the inode), an rpm that imports without writing
#  rpmdb.sqlite is refused with the old keyring kept, and `rime repo
#  disable-copr` with another stored key unreadable must fail closed (the
#  disabled COPR's key is no longer trusted, the bad key is named) while
#  `enable-copr` in that state names the bad key, not the new COPR.
#
#  And the rpm cache: dnf5 must really skip a cached file whose checksum
#  matches ("Already downloaded") and really re-fetch a corrupt one, and the
#  --url listing the engine parses must really name the files dnf5 writes.
#  test-rime-pkg.sh drives the same code with a stub dnf5; this is the real one.
#
#  The container's own `rpm --dbpath X --initdb` WORKS (no SELinux domain
#  transition inside it), so the host's refusal is reproduced with a wrapper
#  that refuses exactly that call with exactly the host's words, and the
#  keyring is ALSO judged on what does not depend on SELinux: an import
#  failure must leave the previous keyring verifying.
#
#  Everything happens inside `podman run --rm` with this repository mounted
#  read-only. Keys are throwaway, generated per run.
#
#  Run from anywhere: ./tests/test-rime-pkg-rpm6.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

ENGINE=files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }

if ! command -v podman >/dev/null 2>&1; then
    echo "SKIP  podman is absent; this suite needs a real rpm 6 and dnf5"
    exit 0
fi

IMAGE=${RIME_RPM6_IMAGE:-registry.fedoraproject.org/fedora:45}
PROBE=$(mktemp) || exit 2
trap 'rm -f "$PROBE"' EXIT

cat > "$PROBE" <<'PROBE_EOF'
set -uo pipefail
PKG=/repo/files/system/libexec/rime-pkg
ok()  { echo "OK $1"; }
bad() { echo "BAD $1 :: $2"; }
one() { tr '\n' ' ' < "$1" | cut -c1-300; }

dnf5 -q -y install rpm-build rpm-sign gnupg2 cpio gzip >/dev/null 2>&1 \
    || { echo "PROBE_SKIP no repository reachable to install rpm-build"; exit 0; }

# ── the rpm this suite is about ─────────────────────────────────────────────
ver=$(rpm --version)
case "$ver" in
    "RPM version 6."*) ok "rpm is 6.x (${ver#RPM version })" ;;
    *) bad "rpm is 6.x" "got '$ver': this suite would prove nothing about rpm 6" ;;
esac
lvl=$(rpm --eval '%{_pkgverify_level}')
[ "$lvl" = all ] && ok "%_pkgverify_level is all" || bad "%_pkgverify_level is all" "got '$lvl'"

# ── throwaway keys and four tiny packages ───────────────────────────────────
#   rimesys      signed by a key imported into the SYSTEM rpmdb
#   rimecopr     signed by a key only the COPR keyring will hold
#   rimecopr2    signed by a second COPR's key
#   rimeother    signed by a key imported nowhere
#   rimeunsigned not signed at all
K=/keys; B=/build; R=/rpms; mkdir -p "$K" "$B" "$R"
mkkey() {
    mkdir -p "$K/$1"; chmod 700 "$K/$1"
    gpg --homedir "$K/$1" --batch --pinentry-mode loopback --passphrase '' \
        --quick-gen-key "Rime $1 <$1@example.invalid>" ed25519 sign never >/dev/null 2>&1
    gpg --homedir "$K/$1" --armor --export > "$K/$1.asc" 2>/dev/null
}
fpr() { gpg --homedir "$K/$1" --with-colons --fingerprint 2>/dev/null | awk -F: '/^fpr:/{print $10; exit}'; }
mkkey sys; mkkey copr; mkkey copr2; mkkey other
cat > "$B/p.spec" <<'SPEC'
Name: %{pname}
Version: 1.0
Release: 1
Summary: rime-pkg rpm6 fixture
License: MIT
BuildArch: noarch
%description
fixture
%install
install -Dm644 %{_sourcedir}/payload %{buildroot}%{ppath}
%files
%{ppath}
SPEC
# $1 name, $2 file shipped, $3 its content source, $4 output dir
mkrpm() {
    mkdir -p "$B/$1-src"; cp "$3" "$B/$1-src/payload"
    rpmbuild -bb --define "_topdir $B/$1" --define "_sourcedir $B/$1-src" \
        --define "_rpmdir $4" --define '_build_name_fmt %%{NAME}-%%{VERSION}.rpm' \
        --define "pname $1" --define "ppath $2" "$B/p.spec" >/dev/null 2>&1
}
sign() {  # $1 key, $2 rpm
    rpmsign --addsign --define "_gpg_path $K/$1" --define "_gpg_name $1@example.invalid" \
        --define '_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback --passphrase ""' \
        "$2" >/dev/null 2>&1
}
echo marker > "$B/marker"
for n in rimesys rimecopr rimecopr2 rimeother rimeunsigned; do
    mkrpm "$n" "/usr/share/$n/marker" "$B/marker" "$R" || bad "fixture $n builds" "rpmbuild failed"
done
sign sys "$R/rimesys-1.0.rpm" && sign copr "$R/rimecopr-1.0.rpm" && sign copr2 "$R/rimecopr2-1.0.rpm" \
    && sign other "$R/rimeother-1.0.rpm" \
    && ok "fixtures signed with rpmsign ($(rpm --eval '%{_openpgp_sign}'))" \
    || bad "fixtures signed with rpmsign" "rpmsign failed"
rpmkeys --import "$K/sys.asc" && ok "the system key imports into the system rpmdb" \
    || bad "the system key imports into the system rpmdb" "rpmkeys --import failed"

# ── the engine, as shipped ──────────────────────────────────────────────────
# shellcheck disable=SC1090
source "$PKG" >/dev/null 2>&1
set +e
E=/tmp/stderr

# Reproduces the Fedora 45 host: `rpm --dbpath <dir> --initdb` refused under
# enforcing SELinux, word for word, exit 255. Every other rpm call is real.
mkdir -p /wrap
cat > /wrap/rpm <<'WRAP'
#!/bin/bash
db=""; init=0; prev=""
for a in "$@"; do
    [ "$prev" = --dbpath ] && db=$a
    [ "$a" = --initdb ] && init=1
    prev=$a
done
if [ -n "$db" ] && [ "$init" = 1 ]; then
    echo "error: can't create transaction lock on ${db}/.rpm.lock (Permission denied)" >&2
    exit 255
fi
# An rpm that imports fine but keeps its keys somewhere other than
# rpmdb.sqlite: what refresh_copr_keyring's database check is there for.
if [ -n "$db" ] && [ "${RPM_WRAP_NODB:-0}" = 1 ]; then
    /usr/bin/rpm "$@" || exit
    rm -f -- "$db/rpmdb.sqlite"
    exit 0
fi
exec /usr/bin/rpm "$@"
WRAP
chmod +x /wrap/rpm
# Every mv the keyring code makes, so the swap's mechanism is asserted rather
# than inferred from a changed inode (a remove-then-rename changes it too).
cat > /wrap/mv <<'WRAP'
#!/bin/bash
printf '%s\n' "$*" >> /tmp/mv.log
exec /usr/bin/mv "$@"
WRAP
# `rime repo enable-copr/disable-copr` call `dnf5 copr …` (no repository is
# touched here) and curl for the key, which this serves from $CURL_KEY.
cat > /wrap/dnf5 <<'WRAP'
#!/bin/bash
[ "$2" = copr ] && exit 0
exec /usr/bin/dnf5 "$@"
WRAP
cat > /wrap/curl <<'WRAP'
#!/bin/bash
out=""; prev=""
for a in "$@"; do [ "$prev" = -o ] && out=$a; prev=$a; done
cp -- "$CURL_KEY" "$out"
WRAP
chmod +x /wrap/mv /wrap/dnf5 /wrap/curl
/wrap/rpm --dbpath /tmp/probe-db --initdb 2>/dev/null
[ "$?" = 255 ] && /wrap/rpm --root /tmp/probe-root --initdb \
    && ok "control: the wrapper refuses only a --dbpath initdb" \
    || bad "control: the wrapper refuses only a --dbpath initdb" "it does not behave like the host"

# Static: nothing in the engine runs `rpm --dbpath … --initdb` any more.
hits=$(grep -vE '^[[:space:]]*#' "$PKG" | grep -E '\brpm\b.*--dbpath.*--initdb' | wc -l)
[ "$hits" = 0 ] && ok "no engine line runs rpm --dbpath … --initdb" \
    || bad "no engine line runs rpm --dbpath … --initdb" "$hits line(s) still do"

sig() {  # $1 rpm -> "rc|status"
    rpm_signature_ok "$1"; local rc=$?
    printf '%s|%s' "$rc" "${RPM_SIGSTATUS:-}"
}
f_sys=$R/rimesys-1.0.rpm f_copr=$R/rimecopr-1.0.rpm f_copr2=$R/rimecopr2-1.0.rpm f_other=$R/rimeother-1.0.rpm f_uns=$R/rimeunsigned-1.0.rpm

# ── rpm_signature_ok against the system keyring ─────────────────────────────
rm -rf "$COPR_KEY_DB" "$COPR_KEYS"
got=$(sig "$f_sys")
[[ "$got" == "0|"*"digests signatures OK"* ]] && ok "signed by a system key: accepted" \
    || bad "signed by a system key: accepted" "$got"
for f in "$f_uns" "$f_other"; do
    want=$(rpmkeys --checksig -- "$f" 2>&1)
    got=$(sig "$f")
    [ "$got" = "1|$want" ] && ok "refused, quoting rpmkeys: $(basename "$f") ($want)" \
        || bad "refused, quoting rpmkeys: $(basename "$f")" "got '$got', rpmkeys says '$want'"
done
got=$(sig "$f_copr")
[ "${got%%|*}" = 1 ] && ok "a COPR-signed package is refused before its COPR is enabled" \
    || bad "a COPR-signed package is refused before its COPR is enabled" "$got"

# ── refresh_copr_keyring ────────────────────────────────────────────────────
# No keys: no keyring, quietly.
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" = 0 ] && [ ! -s "$E" ] && [ ! -e "$COPR_KEY_DB" ] \
    && ok "no COPR keys: no keyring, no error" \
    || bad "no COPR keys: no keyring, no error" "rc=$rc keyring=$([ -e "$COPR_KEY_DB" ] && echo present || echo absent) stderr: $(one "$E")"
# A keyring directory with no database: not verified, quietly, and not written to.
mkdir -p "$COPR_KEY_DB"
got=$(sig "$f_copr" 2>"$E")
[ "${got%%|*}" = 1 ] && [ ! -s "$E" ] && [ -z "$(ls -A "$COPR_KEY_DB")" ] \
    && ok "a keyring directory with no database: refused quietly, nothing created in it" \
    || bad "a keyring directory with no database: refused quietly, nothing created in it" "got '$got', created: $(ls -A "$COPR_KEY_DB" | tr '\n' ' ') stderr: $(one "$E")"
rmdir "$COPR_KEY_DB"

install -Dm0600 "$K/copr.asc" "$COPR_KEYS/owner/project.gpg"
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" = 0 ] && ok "the keyring builds where rpm --initdb is refused (rc 0)" \
    || bad "the keyring builds where rpm --initdb is refused (rc 0)" "rc=$rc stderr: $(one "$E")"
[ ! -s "$E" ] && ok "…and prints nothing (no 'can't create transaction lock')" \
    || bad "…and prints nothing (no 'can't create transaction lock')" "stderr: $(one "$E")"
[ -s "$COPR_KEY_DB/rpmdb.sqlite" ] && [ "$(stat -c %a "$COPR_KEY_DB")" = 700 ] \
    && ok "…into a 0700 directory holding rpmdb.sqlite" \
    || bad "…into a 0700 directory holding rpmdb.sqlite" "mode $(stat -c %a "$COPR_KEY_DB" 2>&1), files: $(ls -A "$COPR_KEY_DB" 2>&1 | tr '\n' ' ')"
got=$(sig "$f_copr")
[[ "$got" == "0|"*"digests signatures OK"* ]] && ok "signed by an enabled COPR's key: accepted" \
    || bad "signed by an enabled COPR's key: accepted" "$got"
got=$(sig "$f_other")
[ "${got%%|*}" = 1 ] && ok "the COPR keyring does not accept a key it was never given" \
    || bad "the COPR keyring does not accept a key it was never given" "$got"
leftover=$(find "$(dirname "$COPR_KEY_DB")" -maxdepth 1 -name 'copr-keyring.*' | wc -l)
[ "$leftover" = 0 ] && ok "no build directory is left beside the keyring" \
    || bad "no build directory is left beside the keyring" "$leftover left"

# A second refresh replaces the keyring with a new directory.
ino=$(stat -c %i "$COPR_KEY_DB")
: > /tmp/mv.log
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" = 0 ] && [ "$(stat -c %i "$COPR_KEY_DB")" != "$ino" ] && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 0 ] \
    && ok "a refresh swaps in a new keyring that still verifies" \
    || bad "a refresh swaps in a new keyring that still verifies" "rc=$rc stderr: $(one "$E")"
# …in ONE rename (renameat2 RENAME_EXCHANGE): no moment without a keyring.
mvs=$(cat /tmp/mv.log)
[ "$(wc -l < /tmp/mv.log)" = 1 ] && [[ "$mvs" == "--exchange -T -- $COPR_KEY_DB.new."*" $COPR_KEY_DB" ]] \
    && ok "…with one mv --exchange, never a remove and a rename" \
    || bad "…with one mv --exchange, never a remove and a rename" "mv calls: $(one /tmp/mv.log)"

# An rpm that imports but writes no rpmdb.sqlite: refused, old keyring kept.
ino=$(stat -c %i "$COPR_KEY_DB")
RPM_WRAP_NODB=1 PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" != 0 ] && grep -F "wrote no" "$E" >/dev/null && [ "$(stat -c %i "$COPR_KEY_DB")" = "$ino" ] \
    && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 0 ] \
    && ok "a keyring built without rpmdb.sqlite is refused; the old one stays" \
    || bad "a keyring built without rpmdb.sqlite is refused; the old one stays" "rc=$rc stderr: $(one "$E")"

# One unreadable key among good ones: a refusal that names it and quotes rpm.
echo 'not a key' > /tmp/broken
install -Dm0600 /tmp/broken "$COPR_KEYS/aaa/broken.gpg"
ino=$(stat -c %i "$COPR_KEY_DB")
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" != 0 ] && ok "an unreadable key fails the refresh (rc $rc)" \
    || bad "an unreadable key fails the refresh" "rc 0: the keyring silently lost a key"
grep -F "aaa/broken.gpg" "$E" | grep -F "import read failed" >/dev/null \
    && ok "…naming the key and quoting rpm" || bad "…naming the key and quoting rpm" "stderr: $(one "$E")"
[ "$(stat -c %i "$COPR_KEY_DB")" = "$ino" ] && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 0 ] \
    && ok "…and the previous keyring is untouched and still verifies" \
    || bad "…and the previous keyring is untouched and still verifies" "replaced or broken"
rm -rf "$COPR_KEYS/aaa"

# The ONLY key file goes bad (a truncated download, a disk error): COPR
# packages must keep verifying against the keyring that was built from it.
cp /tmp/broken "$COPR_KEYS/owner/project.gpg"
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
got=$(sig "$f_copr")
[ "$rc" != 0 ] && [[ "$got" == "0|"* ]] \
    && ok "a corrupted key file does not cost the working keyring" \
    || bad "a corrupted key file does not cost the working keyring" "rc=$rc, $(basename "$f_copr") now: ${got#*|}"
install -Dm0600 "$K/copr.asc" "$COPR_KEYS/owner/project.gpg"
PATH=/wrap:$PATH refresh_copr_keyring 2>/dev/null

# ── revoking a COPR while another stored key is unreadable ──────────────────
# The old engine ignored a failed import, so a machine may carry a key file
# that never imported. A refresh then fails as a whole and keeps the previous
# keyring, which still trusts the key `disable-copr` just removed: the
# disabled COPR's packages would keep verifying. Disabling must fail closed.
install -Dm0600 "$K/copr2.asc" "$COPR_KEYS/owner2/project2.gpg"
PATH=/wrap:$PATH refresh_copr_keyring 2>"$E"; rc=$?
[ "$rc" = 0 ] && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 0 ] && [ "$(sig "$f_copr2" | cut -d'|' -f1)" = 0 ] \
    && ok "control: two COPR keys, both verify" \
    || bad "control: two COPR keys, both verify" "rc=$rc stderr: $(one "$E")"
install -Dm0600 /tmp/broken "$COPR_KEYS/aaa/broken.gpg"
( PATH=/wrap:$PATH cmd_repo_disable_copr owner/project ) 2>"$E"; rc=$?
[ "$rc" != 0 ] && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 1 ] && [ ! -e "$COPR_KEYS/owner/project.gpg" ] \
    && ok "disable-copr with an unreadable key elsewhere: the disabled key is refused" \
    || bad "disable-copr with an unreadable key elsewhere: the disabled key is refused" "rc=$rc rimecopr: $(sig "$f_copr") stderr: $(one "$E")"
[ ! -e "$COPR_KEY_DB" ] && [ "$(sig "$f_copr2" | cut -d'|' -f1)" = 1 ] \
    && ok "…by removing the keyring: every COPR package refused (fail closed)" \
    || bad "…by removing the keyring: every COPR package refused (fail closed)" "keyring $([ -e "$COPR_KEY_DB" ] && echo present || echo absent)"
grep -F "$COPR_KEYS/aaa/broken.gpg" "$E" | grep -F "disable-copr aaa/broken" >/dev/null \
    && ok "…naming the key file and how to clear it" || bad "…naming the key file and how to clear it" "stderr: $(one "$E")"
# Enabling another COPR blames the stored key that failed, not the new COPR.
( CURL_KEY="$K/other.asc" PATH=/wrap:$PATH cmd_repo_enable_copr newowner/newproj ) 2>"$E"; rc=$?
[ "$rc" != 0 ] && grep -F "the signing key stored for COPR 'aaa/broken'" "$E" >/dev/null \
    && ! grep -F "could not import the signing key for COPR 'newowner/newproj'" "$E" >/dev/null \
    && [ ! -e "$COPR_KEYS/newowner/newproj.gpg" ] \
    && ok "enable-copr names the stored key that failed, not the new COPR" \
    || bad "enable-copr names the stored key that failed, not the new COPR" "rc=$rc stderr: $(one "$E")"
# Clearing the bad key is the way out it named.
( PATH=/wrap:$PATH cmd_repo_disable_copr aaa/broken ) 2>"$E"; rc=$?
[ "$rc" = 0 ] && [ "$(sig "$f_copr2" | cut -d'|' -f1)" = 0 ] && [ "$(sig "$f_copr" | cut -d'|' -f1)" = 1 ] \
    && ok "…and disabling that COPR rebuilds the keyring without either removed key" \
    || bad "…and disabling that COPR rebuilds the keyring without either removed key" "rc=$rc stderr: $(one "$E")"
rm -rf "$COPR_KEYS/owner2"
install -Dm0600 "$K/copr.asc" "$COPR_KEYS/owner/project.gpg"
PATH=/wrap:$PATH refresh_copr_keyring 2>/dev/null

# ── guard_rpms, then extract_rpms, under %_pkgverify_level all ──────────────
D=/dl; mkdir -p "$D"; cp "$f_sys" "$f_copr" "$D/"
( guard_rpms "$D" ) >/tmp/guard.out 2>&1; rc=$?
[ "$rc" = 0 ] && grep -F "2 package(s) verified" /tmp/guard.out >/dev/null \
    && ok "guard_rpms verifies a system-signed and a COPR-signed package" \
    || bad "guard_rpms verifies a system-signed and a COPR-signed package" "rc=$rc $(one /tmp/guard.out)"
D2=/dl2; mkdir -p "$D2"; cp "$f_sys" "$f_uns" "$D2/"
( guard_rpms "$D2" ) >/tmp/guard.out 2>&1; rc=$?
[ "$rc" != 0 ] && grep -F "unsigned or untrusted package" /tmp/guard.out | grep -F "SIGNATURES NOT OK" >/dev/null \
    && ok "guard_rpms refuses an unsigned package, quoting rpmkeys" \
    || bad "guard_rpms refuses an unsigned package, quoting rpmkeys" "rc=$rc $(one /tmp/guard.out)"
rpm --root /ctl --initdb
rpm --root /ctl -Uvh --nodeps --noscripts --notriggers --noplugins "$f_sys" >/tmp/ctl.out 2>&1; rc=$?
[ "$rc" != 0 ] && grep -F "does not verify" /tmp/ctl.out >/dev/null \
    && ok "control: rpm 6 refuses even a system-signed package into an empty root without --nosignature" \
    || bad "control: rpm 6 refuses even a system-signed package into an empty root without --nosignature" "rc=$rc: --nosignature may no longer be needed; $(one /tmp/ctl.out)"
( extract_rpms "$D" /root-x ) >/tmp/extract.out 2>&1; rc=$?
[ "$rc" = 0 ] && [ -f /root-x/usr/share/rimesys/marker ] && [ -f /root-x/usr/share/rimecopr/marker ] \
    && ok "extract_rpms unpacks the verified set into an empty root" \
    || bad "extract_rpms unpacks the verified set into an empty root" "rc=$rc $(one /tmp/extract.out)"
# extract_rpms is reached from one place, after guard_rpms on the same set.
calls=$(grep -nE '^[[:space:]]*(extract_rpms|guard_rpms) "\$rpms"' "$PKG")
order=$(printf '%s\n' "$calls" | sed -E 's/.*(extract_rpms|guard_rpms).*/\1/' | paste -sd' ' -)
nx=$(grep -vE '^[[:space:]]*#' "$PKG" | grep -cE '(^|[^_[:alnum:]])extract_rpms[[:space:]]+"')
[ "$order" = "guard_rpms extract_rpms" ] && [ "$nx" = 1 ] \
    && ok "extract_rpms has one caller, and it runs after guard_rpms" \
    || bad "extract_rpms has one caller, and it runs after guard_rpms" "order '$order', $nx call(s)"

# ── the ChatGPT route: a one-file `_keyring fs` keyring ─────────────────────
# Its fingerprint is pinned, so the engine is copied with the pin pointed at
# the throwaway COPR key; nothing else in it changes.
FPR=$(fpr copr)
sed "s/^readonly CHATGPT_KEY_FPR=.*/readonly CHATGPT_KEY_FPR=${FPR}/" "$PKG" > /tmp/rime-pkg-pinned
mkcg() {  # $1 repo dir, $2 signing key
    mkdir -p "$1/repodata"
    mkrpm chatgpt "/etc/pki/rpm-gpg/RPM-GPG-KEY-chatgpt-${FPR}.asc" "$K/copr.asc" "$1" || return 1
    sign "$2" "$1/chatgpt-1.0.rpm" || return 1
    printf '<metadata><package><location href="chatgpt-1.0.rpm"/></package></metadata>\n' \
        | gzip > "$1/repodata/0123abcd-primary.xml.gz"
    printf '<repomd><data type="primary"><location href="repodata/0123abcd-primary.xml.gz"/></data></repomd>\n' \
        > "$1/repodata/repomd.xml"
    rm -rf "$B/chatgpt" "$B/chatgpt-src"
}
cgfetch() {
    local d; d=$(mktemp -d)
    RIME_PKG_CHATGPT_REPO="file://$1" bash -c 'd=$1; set --; source /tmp/rime-pkg-pinned >/dev/null 2>&1; set +e; vendor_fetch chatgpt "$d"' _ "$d" 2>&1
}
if mkcg /cg-good copr && mkcg /cg-bad other; then
    out=$(cgfetch /cg-good); rc=$?
    [ "$rc" = 0 ] && [[ "$out" == *"/chatgpt.rpm" ]] \
        && ok "chatgpt signed by the pinned key verifies through '_keyring fs' on rpm 6" \
        || bad "chatgpt signed by the pinned key verifies through '_keyring fs' on rpm 6" "rc=$rc $out"
    out=$(cgfetch /cg-bad); rc=$?
    [ "$rc" != 0 ] && [[ "$out" == *"does not verify against the pinned key (rpmkeys says: "*"SIGNATURES NOT OK"* ]] \
        && ok "chatgpt signed by another key is refused, quoting rpmkeys" \
        || bad "chatgpt signed by another key is refused, quoting rpmkeys" "rc=$rc $out"
else
    bad "chatgpt fixtures build" "rpmbuild or rpmsign failed"
fi

# ── the rpm cache, against the real dnf5 ────────────────────────────────────
# htop pulls hwloc-libs: two files, both signed by Fedora's key.
if ! declare -F rpm_cache_keep >/dev/null || [ -z "${RPM_CACHE:-}" ]; then
    bad "the engine keeps an rpm cache" "no rpm_cache_keep / RPM_CACHE in this engine"
    echo PROBE_DONE
    exit 0
fi
# The disk is roomy here whatever the runner's is: the free-space bound has its
# own cases in test-rime-pkg.sh, and these are about dnf5.
rpm_cache_fs_space() { echo "1000000000000 500000000000"; }
cachedl() {  # $1 destdir -> stderr of a download + guard + keep, in a subshell
    rm -rf "$1"; mkdir -p "$1"
    ( download_rpms "$1" htop && guard_rpms "$1" && rpm_cache_keep "$1" ) 2>&1
}
rm -rf "$RPM_CACHE"
out=$(cachedl /c1); rc=$?
n1=$(ls /c1/*.rpm 2>/dev/null | wc -l)
nc=$(ls "$RPM_CACHE"/*.rpm 2>/dev/null | wc -l)
if [ "$rc" = 0 ] && [ "$n1" -ge 2 ] && [ "$nc" = "$n1" ] \
   && [ "$(cd /c1 && ls)" = "$(cd "$RPM_CACHE" && ls)" ]; then
    ok "a first build fills the cache with exactly its verified set ($nc files)"
else
    bad "a first build fills the cache with exactly its verified set" "rc=$rc build=$n1 cache=$nc $(printf '%s' "$out" | tail -3 | tr '\n' ' ')"
fi
out=$(cachedl /c2); rc=$?
already=$(printf '%s\n' "$out" | grep -c 'Already downloaded')
if [ "$rc" = 0 ] && [ "$already" = "$n1" ] && [[ "$out" == *"reusing ${n1} of ${n1} package(s)"* ]] \
   && [ "$(cd /c2 && sha256sum -- *.rpm)" = "$(cd /c1 && sha256sum -- *.rpm)" ]; then
    ok "a second build downloads nothing: dnf5 says 'Already downloaded' for all $already"
else
    bad "a second build downloads nothing" "rc=$rc already=$already of $n1: $(printf '%s' "$out" | grep -E 'reusing|Already|%' | tr '\n' ' ' | cut -c1-300)"
fi
victim=$(cd "$RPM_CACHE" && ls -- *.rpm | head -1)
good=$(sha256sum < "/c1/$victim")
sz=$(stat -c %s "$RPM_CACHE/$victim")
printf '\x00' | dd of="$RPM_CACHE/$victim" bs=1 seek=$((sz - 64)) conv=notrunc 2>/dev/null
out=$(cachedl /c3); rc=$?
already=$(printf '%s\n' "$out" | grep -c 'Already downloaded')
if [ "$rc" = 0 ] && [ "$already" = $((n1 - 1)) ] && [ "$(sha256sum < "/c3/$victim")" = "$good" ] \
   && [ "$(sha256sum < "$RPM_CACHE/$victim")" = "$good" ]; then
    ok "a corrupted cache file (same size) is fetched again and the cache healed"
else
    bad "a corrupted cache file (same size) is fetched again and the cache healed" "rc=$rc already=$already of $n1"
fi
out=$(rm -rf /c4; mkdir -p /c4; ( download_rpms /c4 tree && guard_rpms /c4 && rpm_cache_keep /c4 ) 2>&1); rc=$?
if [ "$rc" = 0 ] && [ "$(cd /c4 && ls)" = "$(cd "$RPM_CACHE" && ls -A)" ]; then
    ok "a build of another set prunes the cache to that set"
else
    bad "a build of another set prunes the cache to that set" "rc=$rc cache: $(ls -A "$RPM_CACHE" | tr '\n' ' ')"
fi
echo PROBE_DONE
PROBE_EOF

out=$(podman run --rm --security-opt label=disable \
        -v "$PWD":/repo:ro -v "$PROBE":/probe.sh:ro \
        "$IMAGE" bash /probe.sh 2>&1)

# NOT `printf … | grep -q` under pipefail (SIGPIPE turns a match into 141).
if [[ "$out" == *PROBE_SKIP* ]]; then
    echo "SKIP  $(printf '%s\n' "$out" | grep PROBE_SKIP | sed 's/PROBE_SKIP //')"
    exit 0
fi

pass=0; fail=0
while IFS= read -r line; do
    case "$line" in
        "OK "*)  printf 'PASS  %s\n' "${line#OK }"; pass=$((pass+1)) ;;
        "BAD "*) printf 'FAIL  %s\n' "${line#BAD }"; fail=$((fail+1)) ;;
    esac
done <<<"$out"
if [[ "$out" != *PROBE_DONE* ]]; then
    # set -u or a crash ended the probe: whatever it did not reach is not a pass.
    printf '%s\n' "$out" | tail -20
    echo "FAIL  the probe stopped before its last case"
    fail=$((fail+1))
fi

echo
printf 'rime-pkg-rpm6: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
