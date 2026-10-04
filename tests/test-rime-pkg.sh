#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-pkg.sh — assertions against the SHIPPED package engine,
#  files/system/libexec/rime-pkg. Nothing here re-implements it: every case
#  either runs the script as a process or sources it and calls the very function
#  the image runs.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  `rime install /path/to/some.rpm` added a second kind of argument to a command
#  that previously took only package names, and the classifier that tells them
#  apart is a policy decision with two sharp edges:
#
#    1. The Flatpak rule matches `org.foo.Bar.rpm`. If the file test does not run
#       FIRST, a file the user pointed at is silently sent to Flathub, which then
#       fails with a message about an application id the user never typed.
#    2. The package NAME read out of an untrusted RPM header is used to build a
#       path under /var/lib/rime/pkg/local, by a script running as root. A name
#       like `../../…` must be refused, not concatenated.
#
#  On top of that sits the signature policy. Rime refuses any RPM it cannot
#  verify; the ONLY way past that is `--allow-unsigned` for a named file, and the
#  refusal has to tell the user exactly that. A regression there is not a crash —
#  it is a machine that installs unverified software quietly, or one that refuses
#  a legitimate vendor RPM with no way forward. Both are silent until someone
#  reads the source, so they are pinned here instead.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  No root, no network, no writes outside a temp directory, no extension is ever
#  built or merged. Every case below is reached BEFORE the engine's root gate or
#  is a pure function. Cases that need `rpm` to read a header are skipped, out
#  loud, where rpm is absent, so the suite is meaningful on a plain CI runner and
#  more thorough on a Fedora one.
#
#  PASS = every case prints the exact refusal it should, with a non-zero exit
#         where one is expected, and no case reports an unexpected shell error.
#
#  Run from anywhere: ./tests/test-rime-pkg.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# `set +e`, for the same reason as every other suite here: this one COUNTS
# failures instead of aborting, and many assertions run commands that exit
# non-zero on purpose. GitHub Actions invokes a script as `bash -e {0}`, and
# under `-e` the first such command ends the script — silently truncating the
# run rather than reporting anything, which is worse than a failure.
set +e
cd "$(dirname "$0")" || exit 2

ENGINE=../files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }

WORK=$(mktemp -d /tmp/rime-pkg-test.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0

ok()      { printf 'PASS  %-42s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-42s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-42s %s\n' "$1" "$2"; skip=$((skip+1)); }

# Run the shipped engine as a process. $1 = case name, $2 = expected substring
# anywhere in its output, rest = argv. A refusal must also exit non-zero: a
# message with exit 0 would let `rime update` carry on as if nothing was wrong.
refuses() {
    local name=$1 want=$2; shift 2
    local out rc
    out=$(bash "$ENGINE" "$@" 2>&1 </dev/null); rc=$?
    if [ "$rc" = 0 ]; then
        bad "$name" "exited 0; expected a refusal"
        return
    fi
    if grep -qF -- "$want" <<<"$out"; then ok "$name"
    else bad "$name" "expected $(printf '%q' "$want"), got: $(head -1 <<<"$out")"; fi
}

# Same, but the expected text must NOT appear. Used where the danger is the
# engine taking a different route rather than failing.
not_refuses_with() {
    local name=$1 unwanted=$2; shift 2
    local out
    out=$(bash "$ENGINE" "$@" 2>&1 </dev/null)
    if grep -qF -- "$unwanted" <<<"$out"; then
        bad "$name" "took the wrong route: $(head -1 <<<"$out")"
    else ok "$name"; fi
}

# Source the shipped engine and call one of its functions. Sourcing runs its
# main() with NO arguments on purpose — that prints usage and returns 0, leaving
# every function and constant defined exactly as the image has them.
call() {
    # WORK is forwarded because it is the engine's own scratch directory, set
    # inside main() and therefore unset when a function is called directly.
    WORK="${WORK:-}" bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        "$f" "${a[@]}"
    ' _ "$ENGINE" "$@"
}
predicate() {  # prints true/false for a shipped predicate
    bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        if "$f" "${a[@]}"; then echo true; else echo false; fi
    ' _ "$ENGINE" "$@"
}
is() {
    local name=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then ok "$name"
    else bad "$name" "expected $(printf '%q' "$want"), got $(printf '%q' "$got")"; fi
}

# ── fixtures ────────────────────────────────────────────────────────────────
printf 'PK\003\004 this is a zip, not an rpm' > "$WORK/notanrpm.rpm"
mkdir -p "$WORK/adirectory.rpm"
: > "$WORK/empty.rpm"
printf '\355\253\356\333garbage after a valid lead' > "$WORK/badheader.rpm"
printf 'PK\003\004' > "$WORK/a file with spaces.rpm"
printf 'PK\003\004' > "$WORK/unreadable.rpm"; chmod 000 "$WORK/unreadable.rpm"

echo "── argument routing: a file must beat the Flatpak rule ────────────────"
# is_flatpak_id matches org.foo.Bar.rpm. Testing it before the file rule would
# send a file the user pointed at to Flathub.
is "org.foo.Bar.rpm is a file"      true  "$(predicate is_local_rpm_arg org.foo.Bar.rpm)"
is "…and also matches the flatpak rule" true "$(predicate is_flatpak_id org.foo.Bar.rpm)"
refuses "so the engine treats it as a file" "no such file: org.foo.Bar.rpm" \
        install org.foo.Bar.rpm
is "a bare reverse-DNS id is not a file" false "$(predicate is_local_rpm_arg org.gimp.GIMP)"
if [ "$(id -u)" = 0 ]; then
    # Running this as root would reach flatpak_install and could really install
    # something, which a test must never do.
    skipped "org.gimp.GIMP stays a Flatpak" "(would install as root)"
else
    not_refuses_with "org.gimp.GIMP stays a Flatpak" "no such file" \
            install org.gimp.GIMP
    refuses "…and stops at the root gate instead" "this needs root" \
            install org.gimp.GIMP
fi

# A bare package name must not become a file just because something with that
# name happens to sit in the working directory.
is "plain name is not a file"       false "$(predicate is_local_rpm_arg htop)"
is "python3.12 is not a file"       false "$(predicate is_local_rpm_arg python3.12)"
is "python3.12 is not a Flatpak"    false "$(predicate is_flatpak_id python3.12)"
is "a path is always a file"        true  "$(predicate is_local_rpm_arg /media/usb/x)"
is "a relative path is a file"      true  "$(predicate is_local_rpm_arg ./x.rpm)"
is "a requested-list id is neither" false "$(predicate is_local_rpm_arg local:chrome)"
is "…and not a Flatpak either"      false "$(predicate is_flatpak_id local:chrome)"

echo "── file validation: every refusal names the file and the reason ───────"
refuses "missing file"          "no such file: /nonexistent/rime-test.rpm" \
        install /nonexistent/rime-test.rpm
refuses "directory named *.rpm" "not a regular file: ${WORK}/adirectory.rpm" \
        install "${WORK}/adirectory.rpm"
refuses "not an RPM at all"     "is not an RPM package" \
        install "${WORK}/notanrpm.rpm"
refuses "empty file"            "is not an RPM package" \
        install "${WORK}/empty.rpm"
# The path is echoed back verbatim, which is what proves it survived word
# splitting on its way through the engine.
refuses "a path containing spaces" "${WORK}/a file with spaces.rpm is not an RPM package" \
        install "${WORK}/a file with spaces.rpm"

if [ "$(id -u)" = 0 ]; then
    skipped "unreadable file" "(root can read anything)"
else
    refuses "unreadable file" "permission denied" install "${WORK}/unreadable.rpm"
fi

if command -v rpm >/dev/null 2>&1; then
    # Correct lead magic, nothing behind it: the magic check passes and rpm's own
    # header read is what must reject it.
    refuses "RPM magic but no header" "cannot read the RPM header" \
            install "${WORK}/badheader.rpm"
else
    skipped "RPM magic but no header" "(no rpm on this machine)"
fi

echo "── package names out of an untrusted header build root-owned paths ────"
is "ordinary name"          true  "$(predicate valid_pkg_name google-chrome-stable)"
is "name with . + _ and +"  true  "$(predicate valid_pkg_name ok_1.2+x)"
is "traversal"              false "$(predicate valid_pkg_name ../../etc/passwd)"
is "embedded slash"         false "$(predicate valid_pkg_name a/b)"
is "leading dash"           false "$(predicate valid_pkg_name -rf)"
is "empty"                  false "$(predicate valid_pkg_name '')"
is "newline"                false "$(predicate valid_pkg_name 'a
b')"

echo "── cache identifiers must round-trip ──────────────────────────────────"
# The requested list is the only thing that survives a reboot, an OS upgrade and
# an `rime update`. If `local:<NAME>` does not map back to exactly one cached
# file, a rebuild either loses the package or reads the wrong one.
is "local_id"              "local:chrome" "$(call local_id chrome)"
is "local_name inverts it" "chrome"       "$(call local_name local:chrome)"
is "id survives two trips" "local:chrome" "$(call local_id "$(call local_name local:chrome)")"
is "is_local_id on an id"  true           "$(predicate is_local_id local:chrome)"
is "is_local_id on a name" false          "$(predicate is_local_id chrome)"
is "is_local_id on 'local:'" false        "$(predicate is_local_id 'local:')"
is "cache path"  "/var/lib/rime/pkg/local/chrome.rpm"   "$(call local_cache local:chrome)"
is "marker path" "/var/lib/rime/pkg/local/chrome.trust" "$(call local_marker local:chrome)"
# The cache path must be derivable from the bare name too — that is the form
# `rime remove chrome` and the state file carry.
is "cache path from a bare name" "/var/lib/rime/pkg/local/chrome.rpm" \
   "$(call local_cache chrome)"

echo "── remove matches by package name, not by the path used to install ────"
is "name matches its local: entry" true  "$(predicate requested_matches local:chrome chrome)"
is "id matches itself"             true  "$(predicate requested_matches local:chrome local:chrome)"
is "repo entry matches its name"   true  "$(predicate requested_matches htop htop)"
is "no cross-matching"             false "$(predicate requested_matches htop chrome)"
is "a prefix is not a match"       false "$(predicate requested_matches local:chromium chrome)"
is "the path is not the entry"     false "$(predicate requested_matches local:chrome ./chrome.rpm)"

# Round-trip through a file shaped exactly like the requested list, including a
# comment and a blank line, which load_requested has to drop.
{ printf '# rime requested packages\n\n'; printf 'htop\nlocal:chrome\n'; } > "$WORK/requested"
mapfile -t entries < <(grep -vE '^\s*(#|$)' "$WORK/requested")
matched=""
for e in "${entries[@]}"; do
    [ "$(predicate requested_matches "$e" chrome)" = true ] && matched="$e"
done
is "list round-trip finds the entry" "local:chrome" "$matched"

echo "── signature policy: refusal is closed, and it tells the user what to do"
is "no marker means not trusted" false \
   "$(predicate trusted_unsigned chrome "$WORK/notanrpm.rpm")"

unsigned_msg=$(bash -c '
    e=$1; set --
    source "$e" >/dev/null 2>&1
    RPM_SIGSTATUS="foo.rpm: DIGESTS SIGNATURES NOT OK"
    refuse_unsigned "/media/usb/foo.rpm"
' _ "$ENGINE" 2>&1); unsigned_rc=$?
is "refusal exits non-zero" 1 "$unsigned_rc"
for want in 'cannot verify /media/usb/foo.rpm' \
            'DIGESTS SIGNATURES NOT OK' \
            'sudo rime install --allow-unsigned /media/usb/foo.rpm' \
            'rime repo enable-copr'; do
    if grep -qF -- "$want" <<<"$unsigned_msg"; then ok "refusal mentions: $want"
    else bad "refusal mentions: $want" "not in the message"; fi
done

# The opt-in must be off on every run. It is a per-file decision recorded under
# LOCAL_DIR, never a mode the engine remembers.
is "--allow-unsigned defaults to off" 0 \
   "$(bash -c 'e=$1; set --; source "$e" >/dev/null 2>&1; echo "$ALLOW_UNSIGNED"' _ "$ENGINE")"
# …and it must be consumed as a flag, not mistaken for a package to install.
refuses "the flag is not a package" "no such file: /nonexistent/rime-test.rpm" \
        install --allow-unsigned /nonexistent/rime-test.rpm

echo "── the engine advertises the local-file form ──────────────────────────"
help=$(bash "$ENGINE" --help 2>&1)
for want in 'file.rpm' '--allow-unsigned' '/var/lib/rime/pkg/local'; do
    if grep -qF -- "$want" <<<"$help"; then ok "--help mentions: $want"
    else bad "--help mentions: $want" "absent"; fi
done

echo "── state we may not read is not state that is absent ──────────────────"
# `[ -f "$STATE" ]` is false for a missing file, a directory we may not search,
# a symlink cycle and a non-directory parent alike. /var/lib/rime/pkg is 0700
# root:root and `list`, `info`, `status` and `verify` all run unprivileged, so
# every one of them told users with packages installed that they had none — and
# `verify` exited 0 while doing it, which reads as "checked, and fine".
#
# The matrix below is the reason the fix reads the file rather than testing the
# permissions that might have let it: three of these cases have a readable
# parent and still cannot be read.
SK=$WORK/statekind
mkdir -p "$SK"

mkdir -p "$SK/present" && echo '{}' > "$SK/present/state.json"
is "a readable state file is ok" ok "$(call state_kind "$SK/present/state.json")"

mkdir -p "$SK/empty"
is "a missing file under a readable dir is absent" absent "$(call state_kind "$SK/empty/state.json")"

is "a missing parent is absent" absent "$(call state_kind "$SK/nothing/here/state.json")"

# ENOTDIR: the parent is a regular file, so the read fails with a readable path.
echo not-a-dir > "$SK/afile"
is "a non-directory parent is unreadable" unreadable "$(call state_kind "$SK/afile/state.json")"

# ELOOP: the name exists, the read cannot complete.
ln -s "$SK/loop" "$SK/loop" 2>/dev/null
is "a symlink cycle is unreadable" unreadable "$(call state_kind "$SK/loop")"

# A dangling symlink is a damaged pointer, not an empty machine.
ln -s "$SK/nowhere" "$SK/dangling"
is "a dangling symlink is unreadable" unreadable "$(call state_kind "$SK/dangling")"

# EACCES, the case that shipped. Root walks through 0000, so the assertion is
# skipped rather than silently passing where the mode bit proves nothing.
mkdir -p "$SK/sealed" && echo '{}' > "$SK/sealed/state.json"
chmod 000 "$SK/sealed"
if cat "$SK/sealed/state.json" >/dev/null 2>&1; then
    skipped "an unsearchable parent is unreadable" "this user overrides the mode bit"
else
    is "an unsearchable parent is unreadable" unreadable "$(call state_kind "$SK/sealed/state.json")"
fi
chmod 755 "$SK/sealed"

# EACCES on the file itself, with a searchable parent.
mkdir -p "$SK/filesealed" && echo '{}' > "$SK/filesealed/state.json"
chmod 000 "$SK/filesealed/state.json"
if cat "$SK/filesealed/state.json" >/dev/null 2>&1; then
    skipped "an unreadable file is unreadable" "this user overrides the mode bit"
else
    is "an unreadable file is unreadable" unreadable "$(call state_kind "$SK/filesealed/state.json")"
fi
chmod 644 "$SK/filesealed/state.json"

echo "── 32-bit: Steam could not be installed through the documented path ───"
# Four separate defects each made `rime install steam` fail, and none of them
# had a test. Steam is a multilib application: RPM Fusion's package declares
# 32-bit libraries as requires, and the client cannot load its own UI modules
# without them ("Could not load module 'vgui2_s.so'").

# 1. Resolution never asked for the 32-bit closure, so dnf fetched none of it.
DNFARGS=$WORK/dnf-args
mkdir -p "$WORK/stub"
cat > "$WORK/stub/dnf5" <<STUB
#!/bin/sh
printf '%s\n' "\$@" > "$DNFARGS"
exit 1
STUB
chmod +x "$WORK/stub/dnf5"
PATH="$WORK/stub:$PATH" call download_rpms "$WORK/dl" steam >/dev/null 2>&1
if [ "$(uname -m)" = x86_64 ]; then
    if grep -qx -- '--arch=i686' "$DNFARGS" 2>/dev/null; then
        ok "resolution asks for the 32-bit closure"
    else
        bad "resolution asks for the 32-bit closure" "no --arch=i686 in: $(tr '\n' ' ' < "$DNFARGS" 2>/dev/null)"
    fi
    grep -qx -- '--arch=x86_64' "$DNFARGS" 2>/dev/null \
        && ok "and still asks for the host architecture" \
        || bad "and still asks for the host architecture" "missing --arch=x86_64"
else
    skipped "resolution asks for the 32-bit closure" "only meaningful on x86_64"
    skipped "and still asks for the host architecture" "only meaningful on x86_64"
fi

# 2. rpm ships 32-bit libraries at /lib even on merged-usr Fedora
#    (libgcc.i686 carries /lib/libgcc_s.so.1). A sysext merges /usr and /opt
#    only, so anything left at the tree root never reaches the running system
#    and the library is missing at runtime with no error at install time.
LP=$WORK/legacy
mkdir -p "$LP/root/lib" "$LP/root/lib64" "$LP/root/bin" "$LP/root/sbin" "$LP/root/usr/lib"
echo lib32 > "$LP/root/lib/libgcc_s.so.1"
echo lib64 > "$LP/root/lib64/libfoo.so"
echo abin  > "$LP/root/bin/thing"
echo asbin > "$LP/root/sbin/daemon"
echo kept  > "$LP/root/usr/lib/already-there"
# $WORK is the engine's own scratch dir, set inside main(); exported here so the
# function under test can write its refused-paths list.
mkdir -p "$LP/work"
WORK="$LP/work" call split_out_of_image "$LP/root" "$LP/etc" >/dev/null 2>&1
is "a 32-bit library at /lib is folded into /usr/lib" "lib32" "$(cat "$LP/root/usr/lib/libgcc_s.so.1" 2>/dev/null)"
is "/lib64 is folded into /usr/lib64"                "lib64" "$(cat "$LP/root/usr/lib64/libfoo.so" 2>/dev/null)"
is "/bin is folded into /usr/bin"                    "abin"  "$(cat "$LP/root/usr/bin/thing" 2>/dev/null)"
is "/sbin is folded into /usr/bin"                   "asbin" "$(cat "$LP/root/usr/bin/daemon" 2>/dev/null)"
is "folding does not clobber what was already there" "kept"  "$(cat "$LP/root/usr/lib/already-there" 2>/dev/null)"
[ -d "$LP/root/lib" ] && bad "the legacy directory is removed after folding" "/lib survived" \
                      || ok  "the legacy directory is removed after folding"

# 3. rpm ships /etc symlinks pointing back into /usr by relative path
#    (fontconfig's conf.d entries). split_out_of_image has already moved the
#    tree, so the link dangles where it now sits; `install` dereferences and
#    dies on it, taking the whole install down. `cp -a` copies the link itself.
ET=$WORK/etcsym
mkdir -p "$ET/etcroot/fonts/conf.d"
ln -s ../../../usr/share/fontconfig/conf.avail/10-x.conf "$ET/etcroot/fonts/conf.d/10-x.conf"
if [ -L "$ET/etcroot/fonts/conf.d/10-x.conf" ] && [ ! -e "$ET/etcroot/fonts/conf.d/10-x.conf" ]; then
    ok "the fixture reproduces a dangling relative symlink"
else
    bad "the fixture reproduces a dangling relative symlink" "fixture is wrong"
fi
if grep -q 'cp -a -- "${etcroot}/${rel}" "${target}.rimenew"' "$ENGINE"; then
    ok "install_etc copies a symlink rather than dereferencing it"
else
    bad "install_etc copies a symlink rather than dereferencing it" "still uses install(1), which dies on a dangling link"
fi

# 4. The guard asked `rpm -q <name>` with no architecture. On multilib that
#    answers with the x86_64 build, so every i686 library was deleted as
#    "already provided by Rime OS" and the application failed to launch.
if grep -q 'rpm -q --qf .%{EVR}. -- "${name}.${arch}"' "$ENGINE"; then
    ok "the installed-check is architecture-qualified"
else
    bad "the installed-check is architecture-qualified" "rpm -q by bare name answers for the host arch only"
fi
if grep -q 'multilib sibling of a core package' "$ENGINE"; then
    ok "a 32-bit sibling of a protected package is allowed"
else
    bad "a 32-bit sibling of a protected package is allowed" "REFUSE_RE still rejects i686 siblings the image never ships"
fi

# 5. A 32-bit package is not a 32-bit application; it is the libraries one
#    needs. fontconfig.i686 carries /usr/bin/fc-list at the same path as the
#    x86_64 build, so a single --replacefiles pass over both arches let i686
#    win: /usr/bin/fc-list became an ELF 32-bit i386 binary shadowing the
#    image's, fc-list returned zero fonts, and Steam drew no text at all.
#
#    The fix was a second pass with a hand-written --excludepath list, and the
#    list was wrong in BOTH directions on katana on 2026-09-19 — it let i686
#    helpers under /usr/libexec shadow 14 image binaries (§3), and it threw away
#    every *_icd.i686.json so 32-bit Vulkan had no driver at all (§6.5). Two of
#    the assertions that used to live here PINNED that list, which is why the
#    second defect could not be seen from this suite: they demanded
#    `--excludepath /usr/share`, the very flag that broke Steam.
#
#    So the argv no longer decides what is carried — merge_multilib does, file
#    by file, and what it may see is what this block asserts. Which files
#    actually cross is measured against real packages in
#    tests/test-rime-multilib-extract.sh; that needs a container and a
#    repository, and this suite needs neither.
XR=$WORK/extract
mkdir -p "$XR/rpms" "$XR/stub" "$XR/blind"
: > "$XR/rpms/native.rpm"; : > "$XR/rpms/lib32.rpm"; : > "$XR/rpms/any.rpm"
cat > "$XR/stub/rpm" <<STUB
#!/bin/sh
# Answer %{ARCH} by file name so extract_rpms can sort the set, answer -qal
# with a path so the image-ownership index is non-empty, and record the argv of
# every install pass so the assertions can read it back.
case "\$*" in
  *--qf*ARCH*)
     case "\$*" in
       *native.rpm*) echo "$(uname -m)" ;;
       *lib32.rpm*)  echo i686 ;;
       *)            echo noarch ;;
     esac
     exit 0 ;;
  *-qal*)     echo /usr/bin/true; exit 0 ;;
  *--initdb*) exit 0 ;;
esac
echo "\$@" >> "$XR/passes"
exit 0
STUB
# The same stub with the rpmdb struck dumb: -qal answers nothing at all.
sed '/-qal/d' "$XR/stub/rpm" > "$XR/blind/rpm"
chmod +x "$XR/stub/rpm" "$XR/blind/rpm"
rm -f "$XR/passes"
PATH="$XR/stub:$PATH" call extract_rpms "$XR/rpms" "$XR/root" >/dev/null 2>&1

native_pass="$(grep -F 'native.rpm' "$XR/passes" 2>/dev/null | head -1)"
lib32_pass="$(grep -F 'lib32.rpm' "$XR/passes" 2>/dev/null | head -1)"

# Read the flags back out of the recorded argv rather than matching substrings,
# so "no exclusions but /boot" can be stated as the whole set and not as the
# absence of the two anyone happened to think of.
opt_values() {  # opt_values <argv line> <flag> -> sorted, space-separated
    awk -v want="$2" '{for (i=1;i<NF;i++) if ($i==want) print $(i+1)}' <<<"$1" \
        | LC_ALL=C sort -u | tr '\n' ' '
}

if [ -n "$native_pass" ] && [ -n "$lib32_pass" ] && [ "$native_pass" != "$lib32_pass" ]; then
    ok "multilib is extracted in a pass of its own"
else
    bad "multilib is extracted in a pass of its own" "one pass carried both arches"
fi

native_root="$(opt_values "$native_pass" --root)"
lib32_root="$(opt_values "$lib32_pass" --root)"
if [ -n "$lib32_root" ] && [ "$lib32_root" != "$native_root" ]; then
    ok "the 32-bit pass unpacks into a root of its own"
else
    bad "the 32-bit pass unpacks into a root of its own" \
        "both passes used --root ${native_root:-<none>}, so rpm decides what shadows what again"
fi

lib32_ex="$(opt_values "$lib32_pass" --excludepath)"
if [ "$lib32_ex" = "/boot " ]; then
    ok "the 32-bit pass hand-excludes nothing but /boot"
else
    bad "the 32-bit pass hand-excludes nothing but /boot" \
        "it excludes: ${lib32_ex:-<nothing>} — a directory list is what broke Steam twice; the merge decides"
fi

native_ex="$(opt_values "$native_pass" --excludepath)"
case " $native_ex " in
    *" /usr/bin "*) bad "the native pass still installs binaries" "excluded /usr/bin from the native set too" ;;
    *) ok "the native pass still installs binaries" ;;
esac

# Fail CLOSED, and in the opposite direction to image_owns(). An empty answer
# from the rpmdb reads as "the image owns nothing", which would carry every
# 32-bit copy of every image path into the extension — the §3 disaster, for
# every directory at once. It must stop the run, not proceed.
blind_out="$(PATH="$XR/blind:$PATH" call extract_rpms "$XR/rpms" "$XR/root-blind" 2>&1)"
if [[ "$blind_out" == *"refusing to merge the 32-bit set blind"* ]]; then
    ok "an rpmdb that answers nothing stops the merge"
else
    bad "an rpmdb that answers nothing stops the merge" \
        "it carried on: $(printf '%s' "$blind_out" | tr '\n' ' ' | cut -c1-120)"
fi

# 6. `--` IS NOT A UNIVERSAL END-OF-OPTIONS SEPARATOR. dnf5 rejects it on every
#    subcommand, writes the complaint to stderr and prints NOTHING to stdout:
#
#      $ dnf5 repoquery --quiet --queryformat '%{name}\n' -- chromium; echo $?
#      Unknown argument "--" for command "repoquery". …      (stderr)
#      2                                                      (and no stdout)
#
#    Two invocations in this engine carried one and both were silently dead.
#    probe_rpm's meant `rime resolve` returned no repository candidate for ANY
#    package, so it told a user to install the Chromium flatpak while `rime
#    install chromium` installed the Fedora rpm (evidence §11.1). The other was
#    in newer_satisfied_by_image, below.
#
#    Asserted as a CLASS, over the whole engine, because the defect is a
#    property of dnf5 rather than of the two places anyone has noticed. Comment
#    lines are stripped first: the comments explaining this quote the broken
#    invocation verbatim, and a checker that trips over its own explanation is
#    one somebody deletes.
# ── a flatpak-only name must not enter the dnf transaction ──────────────────
#
# Found 2026-09-22 on katana by `rime install obs-studio discord code
# libreoffice blender wine chromium`. `code` has no rpm anywhere; the routing
# kept every non-curated name for dnf, dnf answered `No package "code"
# available`, and the WHOLE transaction died — obs-studio, libreoffice,
# blender, wine and chromium did not install either. One unavailable name took
# five good ones with it.
#
# It is the same disagreement the section below records in the other direction:
# resolve said one thing and install did another. Asserted on the routing
# function both now share, so they cannot drift apart again.
ROUTE_STUB="$WORK/routestub"; mkdir -p "$ROUTE_STUB"
cat > "$ROUTE_STUB/dnf5" <<'STUB'
#!/usr/bin/env bash
# An rpm exists for everything EXCEPT the name the real defect was about.
for a in "$@"; do
    case "$a" in
        code) exit 0 ;;                       # no output = no candidate
        obs-studio|chromium) printf '%s|1-1|fedora|a real package
' "$a"; exit 0 ;;
    esac
done
exit 0
STUB
cat > "$ROUTE_STUB/flatpak" <<'STUB'
#!/usr/bin/env bash
case "$*" in
    *search*code*)     printf 'com.visualstudio.code	Visual Studio Code	flathub
' ;;
    *search*chromium*) printf 'org.chromium.Chromium	Chromium	flathub
' ;;
    *remotes*)         printf 'flathub
' ;;
esac
exit 0
STUB
chmod +x "$ROUTE_STUB/dnf5" "$ROUTE_STUB/flatpak"

got="$(PATH="$ROUTE_STUB:$PATH" call bare_name_route code 2>/dev/null)"
case "$got" in
    "flatpak com.visualstudio.code")
        ok "a name with no RPM anywhere routes to its Flatpak" ;;
    *)  bad "a name with no RPM anywhere routes to its Flatpak"             "got '${got:-<nothing>}' — it would enter the dnf set and fail the whole transaction" ;;
esac

got="$(PATH="$ROUTE_STUB:$PATH" call bare_name_route obs-studio 2>/dev/null)"
[ "$got" = rpm ]     && ok "a name with an RPM stays on the repository route"     || bad "a name with an RPM stays on the repository route" "got '${got:-<nothing>}'"

# The both-ways half: a name Flatpak also carries must STILL take the rpm when
# one exists, or this fix would quietly move every dual-published package off
# the repositories.
got="$(PATH="$ROUTE_STUB:$PATH" call bare_name_route chromium 2>/dev/null)"
[ "$got" = rpm ]     && ok "an RPM wins over a Flatpak when both exist"     || bad "an RPM wins over a Flatpak when both exist" "got '${got:-<nothing>}' — the rpm route was abandoned"

# And a name neither source has stays on the rpm route, so dnf produces the
# error that names it rather than this function inventing one.
got="$(PATH="$ROUTE_STUB:$PATH" call bare_name_route nosuchpkg 2>/dev/null)"
[ "$got" = rpm ]     && ok "a name nothing carries stays on the rpm route, so dnf names it"     || bad "a name nothing carries stays on the rpm route, so dnf names it" "got '${got:-<nothing>}'"

dd_hits=$(grep -vE '^[[:space:]]*#' "$ENGINE" | grep -cE '\bdnf5\b[^|]* -- ' || true)
if [ "${dd_hits:-1}" = 0 ]; then
    ok "no dnf5 invocation passes '--', which dnf5 refuses"
else
    bad "no dnf5 invocation passes '--', which dnf5 refuses" \
        "$dd_hits invocation(s) still do, and each returns nothing for every input"
fi

# 7. newer_satisfied_by_image is the function that decides whether a package the
#    repositories ship NEWER than the image's build is OMITTED (keep the image's
#    copy, install proceeds) or REFUSED (that needs an OS update). It asked
#    libdnf5 with a `--`, so it answered "nothing provides this" for every
#    versioned requirement it was ever asked about, returned 1 every time, and
#    guard_rpms refused packages it was written to omit.
#
#    Driven directly here with both tools stubbed, because the container suite
#    that runs guard_rpms against a real repository cannot construct a
#    newer-than-installed set on demand. BOTH directions are asserted, so the
#    fix cannot be "always return 0", and the stub dnf5 refuses `--` exactly as
#    the real one does, so the separator coming back fails this case too.
NS=$WORK/newer
mkdir -p "$NS/rpms" "$NS/yes" "$NS/no"
: > "$NS/rpms/other.rpm"
cat > "$NS/yes/rpm" <<'STUB'
#!/bin/sh
case "$*" in
  *--requires*) echo "libfoo >= 2.0"; exit 0 ;;
esac
exit 0
STUB
cat > "$NS/yes/dnf5" <<'STUB'
#!/bin/sh
for a in "$@"; do
    if [ "$a" = "--" ]; then
        echo "Unknown argument \"--\" for command \"$1\"." >&2
        exit 2
    fi
done
echo libfoo
exit 0
STUB
cp "$NS/yes/rpm" "$NS/no/rpm"
sed 's/^echo libfoo$/exit 1/' "$NS/yes/dnf5" > "$NS/no/dnf5"
chmod +x "$NS/yes/rpm" "$NS/yes/dnf5" "$NS/no/rpm" "$NS/no/dnf5"

is "a newer build the image already satisfies is omitted" true \
   "$(PATH="$NS/yes:$PATH" predicate newer_satisfied_by_image "$NS/rpms" libfoo)"
is "a newer build nothing satisfies is not" false \
   "$(PATH="$NS/no:$PATH" predicate newer_satisfied_by_image "$NS/rpms" libfoo)"

echo
echo "── apps that left the image: vendor routes, per-user tools, Flathub ───"
# ChatGPT, Claude Desktop, Claude Code and Zed shipped in the image until
# 2026-10-04 and install on demand since. Each name has exactly one route, and
# these hold the routing table, the per-user tool, and the verification that
# makes a vendor download "verified" rather than "accepted".
is "chatgpt is a vendor rpm"         rpm   "$(call vendor_app chatgpt)"
is "claude-desktop is a vendor deb"  deb   "$(call vendor_app claude-desktop)"
is "htop is not a vendor app"        false "$(predicate vendor_app htop)"
is "claude-code is a per-user tool"  true  "$(predicate user_tool claude-code)"
is "a bare 'claude' is neither"      false "$(predicate user_tool claude)"
is "zed is routed to Flathub"        flatpak "$(call curated_source zed | cut -d'|' -f1)"
is "zen is routed to Flathub"        flatpak "$(call curated_source zen | cut -d'|' -f1)"
# The repository locations are seams for this suite; the fingerprints must not
# be, or a seam could install anything as verified.
is "the ChatGPT fingerprint is not a seam" 3BFA0E4AE8B8CC16A2D9BA684A3B4A566C4660E4 \
   "$(bash -c 'e=$1; set --; export CHATGPT_KEY_FPR=0000 RIME_PKG_CHATGPT_KEY_FPR=0000; source "$e" >/dev/null 2>&1; echo "$CHATGPT_KEY_FPR"' _ "$ENGINE")"
is "the Claude fingerprint is not a seam"  31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE \
   "$(bash -c 'e=$1; set --; export CLAUDE_KEY_FPR=0000 RIME_PKG_CLAUDE_KEY_FPR=0000; source "$e" >/dev/null 2>&1; echo "$CLAUDE_KEY_FPR"' _ "$ENGINE")"

# Claude Code: npm as the user it is for, no root. npm is stubbed and records
# what it was asked; a real npm would reach the network.
VS="$WORK/vendor-stubs"; mkdir -p "$VS/bin" "$VS/root"
cat > "$VS/bin/npm" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$VS/npm.argv"
exit 0
STUB
chmod +x "$VS/bin/npm"
if [ "$(id -u)" != 0 ]; then
    : > "$VS/npm.argv"
    out=$(PATH="$VS/bin:$PATH" bash "$ENGINE" install claude-code 2>&1 </dev/null); rc=$?
    is "claude-code installs without root" 0 "$rc"
    is "…as npm install -g @anthropic-ai/claude-code" "install -g @anthropic-ai/claude-code" "$(tail -1 "$VS/npm.argv")"
    out=$(PATH="$VS/bin:$PATH" bash "$ENGINE" remove claude-code 2>&1 </dev/null); rc=$?
    is "claude-code removes without root" 0 "$rc"
    is "…as npm uninstall -g @anthropic-ai/claude-code" "uninstall -g @anthropic-ai/claude-code" "$(tail -1 "$VS/npm.argv")"
    refuses "a vendor app still needs root" "this needs root" install chatgpt
else
    skipped "claude-code installs without root" "(running as root)"
fi
# Root with nobody behind sudo: an install into /root is not what anyone typing
# this wants, so it is refused. `id` is stubbed to answer uid 0.
cat > "$VS/root/id" <<'STUB'
#!/bin/sh
[ "$1" = -u ] && { echo 0; exit 0; }
exec /usr/bin/id "$@"
STUB
chmod +x "$VS/root/id"
out=$(PATH="$VS/root:$VS/bin:$PATH" SUDO_USER= call user_tool_run install 2>&1)
if grep -qF "installs for one user" <<<"$out"; then ok "root with no user behind sudo is refused"
else bad "root with no user behind sudo is refused" "got: $(head -1 <<<"$out")"; fi

# ── the vendor checks refuse what they must ──────────────────────────────────
# Fixtures signed with a THROWAWAY key, served from file:// repositories. The
# real vendors' keys cannot sign a fixture, which is exactly the point: a
# package whose key is not the pinned one must be refused, however well-formed.
if command -v gpg >/dev/null && command -v ar >/dev/null; then
    G="$WORK/vendor-gpg"; mkdir -p "$G"; chmod 700 "$G"
    gpg --homedir "$G" --batch --pinentry-mode loopback --passphrase '' \
        --quick-gen-key 'Rime Test <rime-test@example.invalid>' ed25519 sign never >/dev/null 2>&1
    gpg --homedir "$G" --armor --export > "$WORK/vendor-test.asc" 2>/dev/null
    TFPR=$(gpg --homedir "$G" --show-keys --with-colons --with-fingerprint "$WORK/vendor-test.asc" 2>/dev/null | awk -F: '/^fpr:/{print $10; exit}')

    # Claude Desktop: a .deb carrying the throwaway key in its postinst.
    CR="$WORK/claude-repo"; mkdir -p "$CR/dists/stable/main/binary-amd64" "$CR/pool" "$WORK/cd/ctl" "$WORK/cd/data"
    { echo '#!/bin/sh'; cat "$WORK/vendor-test.asc"; } > "$WORK/cd/ctl/postinst"
    printf 'Package: claude-desktop\nVersion: 9.9.9\nArchitecture: amd64\n' > "$WORK/cd/ctl/control"
    ( cd "$WORK/cd/ctl" && tar -czf ../control.tar.gz ./control ./postinst )
    ( cd "$WORK/cd/data" && tar -cJf ../data.tar.xz . )
    printf '2.0\n' > "$WORK/cd/debian-binary"
    ( cd "$WORK/cd" && ar rc "$CR/pool/claude-desktop_9.9.9_amd64.deb" debian-binary control.tar.gz data.tar.xz )
    debsum=$(sha256sum "$CR/pool/claude-desktop_9.9.9_amd64.deb" | cut -d' ' -f1)
    mkcrepo() {  # $1 = Filename  $2 = SHA256
        printf 'Package: claude-desktop\nVersion: 9.9.9\nFilename: %s\nSHA256: %s\n\n' "$1" "$2" \
            > "$CR/dists/stable/main/binary-amd64/Packages"
        printf 'SHA256:\n %s Packages\n' "$(sha256sum "$CR/dists/stable/main/binary-amd64/Packages" | cut -d' ' -f1)" \
            > "$CR/dists/stable/InRelease"
    }
    vfetch() {  # $1 = app, rest = env
        local app=$1; shift
        local d; d=$(mktemp -d "$WORK/vf.XXXXXX")
        env "$@" bash -c 'e=$1; a=$2; d=$3; set --; source "$e" >/dev/null 2>&1; set +e; vendor_fetch "$a" "$d"' _ "$ENGINE" "$app" "$d" 2>&1
    }
    mkcrepo pool/claude-desktop_9.9.9_amd64.deb "$debsum"
    out=$(vfetch claude-desktop RIME_PKG_CLAUDE_REPO="file://$CR")
    if grep -qF "its archive key is ${TFPR}, not the 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE Rime pins" <<<"$out"; then
        ok "claude-desktop with another key is refused"
    else bad "claude-desktop with another key is refused" "got: $(tail -1 <<<"$out")"; fi
    mkcrepo pool/claude-desktop_9.9.9_amd64.deb "$(printf '0%.0s' $(seq 64))"
    out=$(vfetch claude-desktop RIME_PKG_CLAUDE_REPO="file://$CR")
    if grep -qF "does not match the sha256 its index publishes" <<<"$out"; then ok "claude-desktop with the wrong sha256 is refused"
    else bad "claude-desktop with the wrong sha256 is refused" "got: $(tail -1 <<<"$out")"; fi
    mkcrepo ../../etc/passwd "$debsum"
    out=$(vfetch claude-desktop RIME_PKG_CLAUDE_REPO="file://$CR")
    if grep -qF "climbs out of the repository" <<<"$out"; then ok "a package path with .. is refused"
    else bad "a package path with .. is refused" "got: $(tail -1 <<<"$out")"; fi

    # ChatGPT: an rpm carrying the throwaway key under the PINNED key's file
    # name. The name proves nothing; the fingerprint inside is what is checked.
    if command -v rpmbuild >/dev/null; then
        GR="$WORK/chatgpt-repo"; mkdir -p "$GR/repodata" "$WORK/cg/SOURCES"
        cp "$WORK/vendor-test.asc" "$WORK/cg/SOURCES/key.asc"
        cat > "$WORK/cg/chatgpt.spec" <<SPEC
Name: chatgpt
Version: 1.0
Release: 1
Summary: rime-pkg test fixture
License: MIT
BuildArch: noarch
%description
fixture
%install
install -Dm644 %{_sourcedir}/key.asc %{buildroot}/etc/pki/rpm-gpg/RPM-GPG-KEY-chatgpt-3BFA0E4AE8B8CC16A2D9BA684A3B4A566C4660E4.asc
%files
/etc/pki/rpm-gpg/RPM-GPG-KEY-chatgpt-3BFA0E4AE8B8CC16A2D9BA684A3B4A566C4660E4.asc
SPEC
        if rpmbuild -bb --define "_topdir $WORK/cg" --define "_sourcedir $WORK/cg/SOURCES" \
               --define "_rpmdir $GR" --define '_build_name_fmt %%{NAME}-%%{VERSION}.rpm' \
               "$WORK/cg/chatgpt.spec" >/dev/null 2>&1; then
            printf '<metadata><package><location href="chatgpt-1.0.rpm"/></package></metadata>\n' \
                | gzip > "$GR/repodata/0123abcd-primary.xml.gz"
            printf '<repomd><data type="primary"><location href="repodata/0123abcd-primary.xml.gz"/></data></repomd>\n' \
                > "$GR/repodata/repomd.xml"
            out=$(vfetch chatgpt RIME_PKG_CHATGPT_REPO="file://$GR")
            if grep -qF "its signing key is ${TFPR}, not the 3BFA0E4AE8B8CC16A2D9BA684A3B4A566C4660E4 Rime pins" <<<"$out"; then
                ok "chatgpt with another key is refused"
            else bad "chatgpt with another key is refused" "got: $(tail -1 <<<"$out")"; fi
        else
            skipped "chatgpt with another key is refused" "(rpmbuild could not build the fixture)"
        fi
    else
        skipped "chatgpt with another key is refused" "(no rpmbuild)"
    fi
else
    skipped "the vendor checks refuse what they must" "(no gpg or ar)"
fi

echo
printf 'rime-pkg: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" = 0 ]
