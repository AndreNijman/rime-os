#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-deb.sh — assertions against the SHIPPED package engine's .deb
#  route, files/system/libexec/rime-pkg. Nothing here re-implements it: every
#  case either runs the script as a process or sources it and calls the very
#  function the image runs.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  `rime install ./thing.deb` adds a THIRD kind of argument to a command that
#  already took package names and .rpm paths, and it adds a package format the
#  rest of this engine knows nothing about. Five things about it are silent
#  when they go wrong, which is why they are pinned here rather than left to
#  review:
#
#   1. ROUTING. is_local_rpm_arg answers true for ANY argument containing a
#      slash. If the .deb test does not run first, `rime install ./claude.deb`
#      is refused with "is not an RPM package" — a message about a format the
#      user did not name.
#
#   2. MAINTAINER SCRIPTS. dpkg runs preinst/postinst/prerm/postrm as root.
#      Rime never does. Claude Desktop's postinst writes an apt source, which
#      on this machine would be a second update channel the product decision in
#      CLAUDE.md forbids by name. And a package whose entry point EXISTS only
#      because its postinst creates it must be refused, not half-installed:
#      this engine already shipped that defect once with rpm scriptlets, where
#      `rime install wine` produced twelve dangling /usr/bin symlinks, no
#      /usr/bin/wine at all, and reported success.
#
#   3. LAYOUT. Debian puts shared libraries in /usr/lib/x86_64-linux-gnu and
#      Fedora looks in /usr/lib64. Carried verbatim they are inert; translated
#      they shadow the image's own ABI, which is the fontconfig.i686 disaster
#      with no reboot to undo it.
#
#   4. IMAGE OWNERSHIP. rime-pkg decides whether a path belongs to the OS by
#      asking the rpmdb. A .deb's files have no rpm owner — and neither does
#      anything the image places with `cp -a`, as it placed Claude Desktop
#      until 2026-10-04. A guard built on `rpm -qf` would have waved a .deb
#      straight over the image's own copy of the very application this route
#      exists for.
#
#   5. TRUST. A .deb carries no signature Rime can check, because Debian signs
#      the apt index rather than the package. The engine must therefore refuse
#      every one of them without --allow-unsigned, and must not quietly invent
#      a weaker rule than the RPM path's.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  Leg A needs no root, no network, no podman and writes nothing outside a temp
#  directory. Its .deb fixtures are built here with `ar` and `tar`. Leg B runs
#  the engine's extract_debs for real, as root, inside a throwaway container,
#  because PKG_ROOT is a readonly constant pointing at /var/lib/rime — the same
#  reason tests/test-rime-pkg-update.sh reaches for podman. Both skip out loud.
#
#  PASS = every case prints the exact refusal it should, with a non-zero exit
#         where one is expected, and no case reports an unexpected shell error.
#
#  Run from anywhere: ./tests/test-rime-deb.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# `set +e` for the same reason every other suite here does it: this one COUNTS
# failures instead of aborting, and many assertions run commands that exit
# non-zero on purpose. GitHub Actions invokes a script as `bash -e {0}`, and
# under -e the first such command truncates the run silently.
set +e
cd "$(dirname "$0")" || exit 2

ENGINE=../files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }
ENGINE_ABS=$(readlink -f "$ENGINE")
REPO_ROOT=$(readlink -f ..)

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-deb-test.XXXXXX") || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %-58s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-58s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-58s %s\n' "$1" "$2"; skip=$((skip+1)); }

is() {
    local name=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then ok "$name"
    else bad "$name" "expected $(printf '%q' "$want"), got $(printf '%q' "$got")"; fi
}

# Run the shipped engine as a process. A refusal must also exit non-zero: a
# message with exit 0 would let `rime update` carry on as if nothing was wrong.
refuses() {
    local name=$1 want=$2; shift 2
    local out rc
    out=$(bash "$ENGINE" "$@" 2>&1 </dev/null); rc=$?
    if [ "$rc" = 0 ]; then bad "$name" "exited 0; expected a refusal"; return; fi
    if grep -qF -- "$want" <<<"$out"; then ok "$name"
    else bad "$name" "expected $(printf '%q' "$want"), got: $(head -1 <<<"$out")"; fi
}
not_refuses_with() {
    local name=$1 unwanted=$2; shift 2
    local out
    out=$(bash "$ENGINE" "$@" 2>&1 </dev/null)
    if grep -qF -- "$unwanted" <<<"$out"; then
        bad "$name" "took the wrong route: $(head -1 <<<"$out")"
    else ok "$name"; fi
}

# Source the shipped engine and call one of its functions. `set --` before the
# source is not optional: rime-pkg ends in `main "$@"`, and a sourced script
# inherits the caller's positional parameters — without it every one of these
# would run `rime-pkg <first argument>` for real.
call() {
    bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        "$f" ${a[@]+"${a[@]}"}
    ' _ "$ENGINE_ABS" "$@"
}
# Same, but stderr is what is being read (the refusals warn before they die).
callerr() {
    bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        "$f" ${a[@]+"${a[@]}"}
    ' _ "$ENGINE_ABS" "$@" 2>&1
}
predicate() {
    bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        if "$f" ${a[@]+"${a[@]}"}; then echo true; else echo false; fi
    ' _ "$ENGINE_ABS" "$@"
}
# Does calling $1 with the rest of the arguments succeed?
succeeds() {
    bash -c '
        e=$1; f=$2; shift 2; a=("$@"); set --
        source "$e" >/dev/null 2>&1
        set +e
        "$f" ${a[@]+"${a[@]}"} >/dev/null 2>&1
    ' _ "$ENGINE_ABS" "$@"
}

if ! command -v ar >/dev/null 2>&1; then
    echo "SKIP  ar (binutils) is absent; a .deb cannot be built or read without it"
    echo
    printf 'rime-deb: 0 passed, 0 failed, 1 skipped\n'
    exit 0
fi

# ── fixtures ────────────────────────────────────────────────────────────────
# mkdeb <out.deb> <controldir> <datadir> [format] [datacompression]
mkdeb() {
    local out=$1 ctl=$2 data=$3 fmt=${4:-2.0} comp=${5:-gz}
    local t; t=$(mktemp -d "$WORK/mk.XXXXXX") || return 1
    printf '%s\n' "$fmt" > "$t/debian-binary"
    tar -czf "$t/control.tar.gz" -C "$ctl" . 2>/dev/null || return 1
    case "$comp" in
        gz)  tar -czf "$t/data.tar.gz"  -C "$data" . 2>/dev/null ;;
        xz)  tar -cJf "$t/data.tar.xz"  -C "$data" . 2>/dev/null ;;
        zst) tar --zstd -cf "$t/data.tar.zst" -C "$data" . 2>/dev/null ;;
        none) : > "$t/nothing" ;;
    esac || return 1
    rm -f "$out"
    ( cd "$t" && ar rc "$out" debian-binary control.tar.gz data.tar.* 2>/dev/null )
}
# A deb whose data archive is built from an explicit tar, so a member name can
# be something tar would never produce from a directory walk.
mkdeb_rawdata() {
    local out=$1 ctl=$2 datatar=$3
    local t; t=$(mktemp -d "$WORK/mk.XXXXXX") || return 1
    printf '2.0\n' > "$t/debian-binary"
    tar -czf "$t/control.tar.gz" -C "$ctl" . 2>/dev/null || return 1
    cp -f "$datatar" "$t/data.tar"
    rm -f "$out"
    ( cd "$t" && ar rc "$out" debian-binary control.tar.gz data.tar 2>/dev/null )
}
control_file() {   # control_file <dir> <name> <version> <arch> [depends]
    mkdir -p "$1"
    { printf 'Package: %s\n' "$2"
      printf 'Version: %s\n' "$3"
      printf 'Architecture: %s\n' "$4"
      printf 'Maintainer: Rime test <t@example.invalid>\n'
      [ -n "${5:-}" ] && printf 'Depends: %s\n' "$5"
      printf 'Description: a fixture\n'
    } > "$1/control"
}
# The maintainer script every fixture carries. If anything ever runs one, the
# sentinel it writes is what proves it.
SENTINEL="$WORK/MAINTAINER-SCRIPT-RAN"
add_postinst() {
    cat > "$1/postinst" <<POSTEOF
#!/bin/sh
echo ran > "$SENTINEL"
update-alternatives --install /usr/bin/hello hello /opt/App/hello 100
POSTEOF
    chmod 755 "$1/postinst"
}

HOSTARCH=$(uname -m)
case "$HOSTARCH" in
    x86_64)  DEBARCH=amd64;  OTHERARCH=arm64 ;;
    aarch64) DEBARCH=arm64;  OTHERARCH=amd64 ;;
    *)       DEBARCH=$HOSTARCH; OTHERARCH=amd64 ;;
esac

# The well-formed package: a program on PATH, a private library beside it, a
# desktop entry, an icon, and a symlink with a relative ../ target — the exact
# shape claude-desktop's own .deb has.
G=$WORK/good
mkdir -p "$G/ctl" "$G/data/usr/bin" "$G/data/usr/lib/hello" \
         "$G/data/usr/share/applications" "$G/data/usr/share/icons/hicolor/48x48/apps"
control_file "$G/ctl" hello 1.2.3 "$DEBARCH" 'libgtk-3-0, libc6 (>= 2.34)'
add_postinst "$G/ctl"
printf '#!/bin/sh\necho hello\n' > "$G/data/usr/lib/hello/hello"
chmod 755 "$G/data/usr/lib/hello/hello"
printf 'ELF-ish\n' > "$G/data/usr/lib/hello/libprivate.so"
printf 'sandbox\n'  > "$G/data/usr/lib/hello/helper-sandbox"
chmod 4755 "$G/data/usr/lib/hello/helper-sandbox"
ln -s ../lib/hello/hello "$G/data/usr/bin/hello"
printf '[Desktop Entry]\nName=Hello\nExec=/usr/bin/hello %%U\nType=Application\n' \
    > "$G/data/usr/share/applications/hello.desktop"
printf 'PNG\n' > "$G/data/usr/share/icons/hicolor/48x48/apps/hello.png"
mkdeb "$WORK/hello.deb" "$G/ctl" "$G/data" || { echo "cannot build the fixture .deb"; exit 2; }

echo "── routing: a .deb must beat the .rpm rule, which matches every path ──"
is "./x.deb is a .deb"                true  "$(predicate is_local_deb_arg ./x.deb)"
is "a bare name ending .deb is a .deb" true "$(predicate is_local_deb_arg x.deb)"
is "…and the .rpm rule also claims it" true "$(predicate is_local_rpm_arg ./x.deb)"
is "a plain name is not a .deb"       false "$(predicate is_local_deb_arg htop)"
is "a Flatpak id is not a .deb"       false "$(predicate is_local_deb_arg org.gimp.GIMP)"
is "a requested-list id is not a .deb" false "$(predicate is_local_deb_arg deb:hello)"
is "a real .deb under a path is one"  true  "$(predicate is_local_deb_arg "$WORK/hello.deb")"
# The regression pin for the ordering. If the .rpm test runs first the engine
# answers "is not an RPM package", about a format nobody named.
refuses "a missing .deb is reported as a missing file" \
        "no such file: /nonexistent/rime-test.deb" install /nonexistent/rime-test.deb
not_refuses_with "…and never as 'not an RPM package'" "is not an RPM package" \
        install /nonexistent/rime-test.deb
# …and the .rpm path is untouched: this is the assertion that fails if the new
# branch swallowed it.
refuses "a missing .rpm is still an RPM refusal" \
        "no such file: /nonexistent/rime-test.rpm" install /nonexistent/rime-test.rpm

echo "── identifiers must round-trip, and not collide with the .rpm ones ────"
is "deb_id"                  "deb:hello" "$(call deb_id hello)"
is "deb_name inverts it"     "hello"     "$(call deb_name deb:hello)"
is "id survives two trips"   "deb:hello" "$(call deb_id "$(call deb_name deb:hello)")"
is "is_deb_id on an id"      true        "$(predicate is_deb_id deb:hello)"
is "is_deb_id on a bare name" false      "$(predicate is_deb_id hello)"
is "is_deb_id on 'deb:'"     false       "$(predicate is_deb_id 'deb:')"
is "is_deb_id on a local: id" false      "$(predicate is_deb_id local:hello)"
is "cache path"    "/var/lib/rime/pkg/deb/hello.deb"   "$(call deb_cache deb:hello)"
is "marker path"   "/var/lib/rime/pkg/deb/hello.trust" "$(call deb_marker deb:hello)"
is "manifest path" "/var/lib/rime/pkg/deb/hello.files" "$(call deb_manifest deb:hello)"
is "cache path from a bare name" "/var/lib/rime/pkg/deb/hello.deb" "$(call deb_cache hello)"
# The two caches must not be the same directory: every function that walks
# LOCAL_DIR reads an RPM header out of what it finds there.
is "the .deb cache is not the .rpm cache" false \
   "$([ "$(call deb_cache hello)" = "$(call local_cache hello)" ] && echo true || echo false)"

echo "── remove matches by package name, whichever format installed it ──────"
is "a name matches its deb: entry"   true  "$(predicate requested_matches deb:hello hello)"
is "a name still matches local:"     true  "$(predicate requested_matches local:hello hello)"
is "a deb: id matches itself"        true  "$(predicate requested_matches deb:hello deb:hello)"
is "no cross-matching"               false "$(predicate requested_matches deb:hello htop)"
is "a prefix is not a match"         false "$(predicate requested_matches deb:helloworld hello)"

echo "── package names out of an untrusted archive build root-owned paths ───"
is "ordinary name"              true  "$(predicate valid_deb_name claude-desktop)"
is "a vendor's capitals"        true  "$(predicate valid_deb_name PacketTracer)"
is "name with . + _ and +"      true  "$(predicate valid_deb_name ok_1.2+x)"
is "traversal"                  false "$(predicate valid_deb_name ../../etc/passwd)"
is "embedded slash"             false "$(predicate valid_deb_name a/b)"
is "leading dash"               false "$(predicate valid_deb_name -rf)"
is "empty"                      false "$(predicate valid_deb_name '')"
is "newline"                    false "$(predicate valid_deb_name 'a
b')"
is "a shell metacharacter"      false "$(predicate valid_deb_name 'a;rm -rf /')"

echo "── the archive itself: what is refused before anything is unpacked ────"
is "the ar magic is recognised"  true  "$(predicate deb_magic_ok "$WORK/hello.deb")"
printf 'PK\003\004 this is a zip\n' > "$WORK/notadeb.deb"
is "a zip is not a .deb"         false "$(predicate deb_magic_ok "$WORK/notadeb.deb")"
printf '\355\253\356\333' > "$WORK/rpmmagic.deb"
is "an RPM is not a .deb"        false "$(predicate deb_magic_ok "$WORK/rpmmagic.deb")"
refuses "a file named .deb that is a zip is refused as a .deb" \
        "is not a Debian package" install "$WORK/notadeb.deb"
# An .rpm renamed .deb must be refused for what it is not, rather than
# accidentally taken by the RPM branch after the name said otherwise.
refuses "an RPM renamed .deb is refused as a .deb" \
        "is not a Debian package" install "$WORK/rpmmagic.deb"
mkdir -p "$WORK/adirectory.deb"
refuses "a directory named *.deb"  "not a regular file" install "$WORK/adirectory.deb"
: > "$WORK/empty.deb"
refuses "an empty file"            "is not a Debian package" install "$WORK/empty.deb"
printf 'a path with spaces' > "$WORK/a file with spaces.deb"
refuses "a path containing spaces is echoed back verbatim" \
        "$WORK/a file with spaces.deb is not a Debian package" \
        install "$WORK/a file with spaces.deb"

mkdeb "$WORK/fmt3.deb" "$G/ctl" "$G/data" 3.0
refuses "a .deb declaring format 3.0" "Rime reads format 2.0" install "$WORK/fmt3.deb"

# An ar archive with no control member at all.
NC=$WORK/nocontrol; mkdir -p "$NC"
printf '2.0\n' > "$NC/debian-binary"; tar -czf "$NC/data.tar.gz" -C "$G/data" .
( cd "$NC" && ar rc "$WORK/nocontrol.deb" debian-binary data.tar.gz )
refuses "a .deb with no control archive" "has no control archive" install "$WORK/nocontrol.deb"

ND=$WORK/nodata; mkdir -p "$ND"
printf '2.0\n' > "$ND/debian-binary"; tar -czf "$ND/control.tar.gz" -C "$G/ctl" .
( cd "$ND" && ar rc "$WORK/nodata.deb" debian-binary control.tar.gz )
refuses "a .deb with no data archive" "has no data archive" install "$WORK/nodata.deb"

NB=$WORK/nobin; mkdir -p "$NB"
tar -czf "$NB/control.tar.gz" -C "$G/ctl" .; tar -czf "$NB/data.tar.gz" -C "$G/data" .
( cd "$NB" && ar rc "$WORK/nobin.deb" control.tar.gz data.tar.gz )
refuses "an ar archive that is not a .deb at all" "has no debian-binary member" \
        install "$WORK/nobin.deb"

echo "── metadata: the fields the rest of the engine depends on ─────────────"
CF=$WORK/ctlparse; mkdir -p "$CF"
{ printf 'Package: folded\n'
  printf 'Version: 1.0\n'
  printf 'Architecture: %s\n' "$DEBARCH"
  printf 'Depends:coreutils,libc-bin,\n'
  printf ' libfuse2, sudo\n'
  printf 'Description: two lines\n'
  printf ' and a continuation that is not a field\n'
} > "$CF/control"
is "a field with no space after the colon" "coreutils,libc-bin, libfuse2, sudo" \
   "$(call deb_control_field "$CF/control" Depends)"
is "a field is found case-insensitively"   "folded" \
   "$(call deb_control_field "$CF/control" package)"
is "a continuation does not leak into the next field" "1.0" \
   "$(call deb_control_field "$CF/control" Version)"
is "a field that is not there is empty"    "" \
   "$(call deb_control_field "$CF/control" Essential)"

is "deb_name_of reads the Package field" "hello" "$(call deb_name_of "$WORK/hello.deb")"
is "deb_identity is name-version.arch"   "hello-1.2.3.${DEBARCH}" \
   "$(call deb_identity "$WORK/hello.deb")"

BA=$WORK/badarch; mkdir -p "$BA/ctl"
control_file "$BA/ctl" hello 1.0 "$OTHERARCH"
mkdeb "$WORK/badarch.deb" "$BA/ctl" "$G/data"
refuses "a .deb for another architecture" \
        "built for Debian architecture '${OTHERARCH}'" install "$WORK/badarch.deb"

I3=$WORK/i386; mkdir -p "$I3/ctl"
control_file "$I3/ctl" hello 1.0 i386
mkdeb "$WORK/i386.deb" "$I3/ctl" "$G/data"
if [ "$HOSTARCH" = x86_64 ]; then
    # Deliberate: this engine's multilib machinery is rpm's, and half a Debian
    # 32-bit userspace is worse than a refusal.
    refuses "an i386 .deb on x86_64 is refused, not multilib'd" \
            "built for Debian architecture 'i386'" install "$WORK/i386.deb"
else
    skipped "an i386 .deb on x86_64 is refused, not multilib'd" "not x86_64"
fi

BN=$WORK/badname; mkdir -p "$BN/ctl"
control_file "$BN/ctl" '../../etc/passwd' 1.0 "$DEBARCH"
mkdeb "$WORK/badname.deb" "$BN/ctl" "$G/data"
refuses "a package name that would escape the cache directory" \
        "is not a name Rime will turn into a path" install "$WORK/badname.deb"

NN=$WORK/noname; mkdir -p "$NN/ctl"
{ printf 'Version: 1.0\nArchitecture: %s\n' "$DEBARCH"; } > "$NN/ctl/control"
mkdeb "$WORK/noname.deb" "$NN/ctl" "$G/data"
refuses "a control file with no Package field" \
        "has no name/version/architecture" install "$WORK/noname.deb"

KN=$WORK/kernel; mkdir -p "$KN/ctl"
control_file "$KN/ctl" glibc 1.0 "$DEBARCH"
mkdeb "$WORK/glibc.deb" "$KN/ctl" "$G/data"
refuses "a core-system name is refused from a .deb too" \
        "kernel or core-system package" install "$WORK/glibc.deb"

echo "── every compression a .deb may use is actually read ──────────────────"
for comp in gz xz zst; do
    if ! mkdeb "$WORK/comp-${comp}.deb" "$G/ctl" "$G/data" 2.0 "$comp"; then
        skipped "data.tar.${comp} is read" "this machine cannot create one"
        continue
    fi
    d=$WORK/unpack-$comp; rm -rf "$d"; mkdir -p "$d"
    member=$(call deb_members "$WORK/comp-${comp}.deb" | grep '^data\.tar')
    if call deb_tar "$WORK/comp-${comp}.deb" "$member" -xf - -C "$d" >/dev/null 2>&1 \
       && [ -f "$d/usr/lib/hello/hello" ]; then
        ok "data.tar.${comp} is read"
    else
        bad "data.tar.${comp} is read" "nothing came out of the ${comp} member"
    fi
done
# Not a formality: `ar p x.deb data.tar.xz | tar -xf -` does NOT autodetect
# compression on a pipe (GNU tar 1.35: "Archive is compressed. Use -J option"),
# which is why the engine chooses the flag from the member name.
is "an unknown compression is refused rather than guessed" false \
   "$(succeeds deb_tar "$WORK/hello.deb" data.tar.brotli -tf - && echo true || echo false)"

echo "── trust: a .deb can never be verified, and the refusal says so ───────"
trust_msg=$(bash -c '
    e=$1; set --
    source "$e" >/dev/null 2>&1
    deb_trust_refusal "/media/usb/foo.deb" "foo" "libgtk-3-0, libc6 (>= 2.34)"
' _ "$ENGINE_ABS" 2>&1); trust_rc=$?
is "the refusal exits non-zero" 1 "$trust_rc"
for want in 'cannot verify /media/usb/foo.deb' \
            'Debian signs the apt' \
            'no deb keyring' \
            '_gpgorigin' \
            'resolves NONE of its dependencies' \
            'libgtk-3-0' \
            'libc6 (>= 2.34)' \
            'sudo rime install --allow-unsigned /media/usb/foo.deb'; do
    if grep -qF -- "$want" <<<"$trust_msg"; then ok "the refusal says: $want"
    else bad "the refusal says: $want" "not in the message"; fi
done
# The opt-in is off on every run; it is a per-file decision, never a mode.
is "--allow-unsigned defaults to off" 0 \
   "$(bash -c 'e=$1; set --; source "$e" >/dev/null 2>&1; echo "$ALLOW_UNSIGNED"' _ "$ENGINE_ABS")"
# The marker covers exact bytes, so swapping the cached file revokes it.
mkdir -p "$WORK/markers"
is "no marker means not trusted" false "$(predicate trusted_deb hello "$WORK/hello.deb")"

echo "── the payload: only /usr and /opt, and no Debian library layout ──────"
mklayout() {   # mklayout <name> ; then populate $WORK/layout/<name>
    rm -rf "$WORK/layout/$1"; mkdir -p "$WORK/layout/$1"
}
L=$WORK/layout
# Accepted: an application with its own private directory. This is the shape
# every Electron .deb has, claude-desktop included.
mklayout okpriv
mkdir -p "$L/okpriv/usr/lib/hello" "$L/okpriv/usr/bin"
printf 'x\n' > "$L/okpriv/usr/lib/hello/libEGL.so"
ln -s ../lib/hello/hello "$L/okpriv/usr/bin/hello"
is "a private library beside its application is carried" true \
   "$(succeeds deb_layout_ok "$L/okpriv" hello && echo true || echo false)"
# …and that symlink has a ../ in its target, which must NOT be mistaken for an
# escape: /usr/bin/claude-desktop -> ../lib/claude-desktop/claude-desktop is
# what Anthropic's own .deb ships.
is "a relative ../ symlink target inside /usr is kept" true \
   "$(succeeds deb_layout_ok "$L/okpriv" hello && echo true || echo false)"

mklayout opt
mkdir -p "$L/opt/opt/App"; printf 'x\n' > "$L/opt/opt/App/app"
is "/opt is a hierarchy a system extension merges" true \
   "$(succeeds deb_layout_ok "$L/opt" app && echo true || echo false)"

mklayout etc
mkdir -p "$L/etc/etc"; printf 'x\n' > "$L/etc/etc/foo.conf"
out=$(callerr deb_layout_ok "$L/etc" foo)
if grep -qF "merges only /usr and /opt" <<<"$out" && grep -qF "/etc/foo.conf" <<<"$out"
then ok "a .deb shipping /etc is refused, and the path is named"
else bad "a .deb shipping /etc is refused, and the path is named" "got: $(head -1 <<<"$out")"; fi

mklayout var
mkdir -p "$L/var/var/lib/foo"; printf 'x\n' > "$L/var/var/lib/foo/db"
is "a .deb shipping /var is refused" false \
   "$(succeeds deb_layout_ok "$L/var" foo && echo true || echo false)"

mklayout multiarch
mkdir -p "$L/multiarch/usr/lib/x86_64-linux-gnu"
printf 'x\n' > "$L/multiarch/usr/lib/x86_64-linux-gnu/libfoo.so.1"
out=$(callerr deb_layout_ok "$L/multiarch" foo)
if grep -qF "multiarch" <<<"$out" && grep -qF "/usr/lib64" <<<"$out"; then
    ok "a library in the Debian multiarch directory is refused, with the reason"
else
    bad "a library in the Debian multiarch directory is refused, with the reason" \
        "got: $(head -1 <<<"$out")"
fi
mklayout multiarch2
mkdir -p "$L/multiarch2/usr/lib/aarch64-linux-gnu/pkgconfig"
printf 'x\n' > "$L/multiarch2/usr/lib/aarch64-linux-gnu/pkgconfig/foo.pc"
is "the whole multiarch directory is refused, not only its .so files" false \
   "$(succeeds deb_layout_ok "$L/multiarch2" foo && echo true || echo false)"
mklayout multiarch3
mkdir -p "$L/multiarch3/usr/lib/arm-linux-gnueabihf"
printf 'x\n' > "$L/multiarch3/usr/lib/arm-linux-gnueabihf/libc.so.6"
is "the eabihf spelling is a multiarch directory too" false \
   "$(succeeds deb_layout_ok "$L/multiarch3" foo && echo true || echo false)"
is "deb_is_multiarch_dir on a real triplet"  true  "$(predicate deb_is_multiarch_dir x86_64-linux-gnu)"
is "…and not on an application directory"    false "$(predicate deb_is_multiarch_dir claude-desktop)"

mklayout syslib
mkdir -p "$L/syslib/usr/lib64"; printf 'x\n' > "$L/syslib/usr/lib64/libbar.so.1"
out=$(callerr deb_layout_ok "$L/syslib" bar)
if grep -qF "where the system linker looks" <<<"$out"; then
    ok "a library dropped into a linker search path is refused"
else
    bad "a library dropped into a linker search path is refused" "got: $(head -1 <<<"$out")"
fi
mklayout syslib2
mkdir -p "$L/syslib2/usr/lib"; printf 'x\n' > "$L/syslib2/usr/lib/libbaz.so"
is "…and /usr/lib counts as one" false \
   "$(succeeds deb_layout_ok "$L/syslib2" baz && echo true || echo false)"

mklayout usrlocal
mkdir -p "$L/usrlocal/usr/local/bin"; printf 'x\n' > "$L/usrlocal/usr/local/bin/foo"
out=$(callerr deb_layout_ok "$L/usrlocal" foo)
if grep -qF "symlink into /var" <<<"$out"; then
    ok "/usr/local is refused, because on Rime it is a symlink into /var"
else
    bad "/usr/local is refused, because on Rime it is a symlink into /var" \
        "got: $(head -1 <<<"$out")"
fi

mklayout kmod
mkdir -p "$L/kmod/usr/lib/modules/6.1.0"; printf 'x\n' > "$L/kmod/usr/lib/modules/6.1.0/foo.ko"
is "a kernel module needs an image build, not an extension" false \
   "$(succeeds deb_layout_ok "$L/kmod" foo && echo true || echo false)"

mklayout escape
mkdir -p "$L/escape/usr/bin"; ln -s /etc/shadow "$L/escape/usr/bin/x"
out=$(callerr deb_layout_ok "$L/escape" x)
if grep -qF "outside the hierarchies" <<<"$out"; then
    ok "a symlink pointing out of /usr and /opt is refused"
else
    bad "a symlink pointing out of /usr and /opt is refused" "got: $(head -1 <<<"$out")"
fi
mklayout escape2
mkdir -p "$L/escape2/usr/bin"; ln -s ../../../etc/passwd "$L/escape2/usr/bin/x"
is "…including one that climbs out with ../" false \
   "$(succeeds deb_layout_ok "$L/escape2" x && echo true || echo false)"

echo "── path normalisation, which decides the two cases above ──────────────"
is "a plain path"              "/usr/bin/x"      "$(call deb_norm_path /usr/bin/x)"
is "one ../"                   "/usr/lib/hello"  "$(call deb_norm_path /usr/bin/../lib/hello)"
is "a relative link resolved"  "/usr/lib/hello/hello" \
   "$(call deb_norm_path /usr/bin/../lib/hello/hello)"
is "./ is dropped"             "/usr/bin/x"      "$(call deb_norm_path /usr/./bin/x)"
is "climbing past the root"    "/"               "$(call deb_norm_path /../../..)"
is "a doubled slash"           "/usr/bin"        "$(call deb_norm_path //usr//bin)"

echo "── the member-name pre-scan, before a byte is written ─────────────────"
# GNU tar 1.35 does refuse to write through a symlinked directory — measured.
# This pass is the layer that does not depend on that.
RAW=$WORK/rawtar; mkdir -p "$RAW/usr/bin"; printf 'x\n' > "$RAW/usr/bin/hello"
( cd "$RAW" && tar -cf "$WORK/climb.tar" \
      --transform='s|^\./usr/bin/hello$|../evil|' ./usr/bin/hello ) 2>/dev/null
mkdeb_rawdata "$WORK/climb.deb" "$G/ctl" "$WORK/climb.tar"
out=$(callerr deb_names_ok "$WORK/climb.deb" data.tar climber)
if grep -qF "climbs out of the package" <<<"$out"; then
    ok "a member name with a .. component is refused"
else
    bad "a member name with a .. component is refused" "got: $(head -1 <<<"$out")"
fi
( cd "$RAW" && tar -cPf "$WORK/abs.tar" "$RAW/usr/bin/hello" ) 2>/dev/null
mkdeb_rawdata "$WORK/abs.deb" "$G/ctl" "$WORK/abs.tar"
out=$(callerr deb_names_ok "$WORK/abs.deb" data.tar absolute)
if grep -qF "absolute path" <<<"$out"; then
    ok "an absolute member name is refused"
else
    bad "an absolute member name is refused" "got: $(head -1 <<<"$out")"
fi
member=$(call deb_members "$WORK/hello.deb" | grep '^data\.tar')
is "an ordinary package passes the pre-scan" true \
   "$(succeeds deb_names_ok "$WORK/hello.deb" "$member" hello && echo true || echo false)"
# An empty payload is a package that installs nothing and reports success —
# the shape of every defect this engine has had. tar writes `./` for the top
# directory, so "the archive lists something" is not the question; "does it
# carry a file" is, and only the unpacked tree can answer it.
EM=$WORK/emptydata; mkdir -p "$EM"
mkdeb "$WORK/emptydata.deb" "$G/ctl" "$EM"
ED=$WORK/emptytree; rm -rf "$ED"; mkdir -p "$ED"
call deb_tar "$WORK/emptydata.deb" data.tar.gz -xf - -C "$ED" >/dev/null 2>&1
out=$(callerr deb_layout_ok "$ED" empty)
if grep -qF "no files at all" <<<"$out"; then
    ok "a .deb whose payload is only directories is refused"
else
    bad "a .deb whose payload is only directories is refused" "got: $(head -1 <<<"$out")"
fi

echo "── the package must bring its own way in ──────────────────────────────"
E=$WORK/entry
rm -rf "$E"; mkdir -p "$E/bin/usr/bin"; printf 'x\n' > "$E/bin/usr/bin/hello"
is "an executable on PATH is the entry point" "/usr/bin/hello" "$(call deb_entry_point "$E/bin")"
mkdir -p "$E/link/usr/bin" "$E/link/usr/lib/app"
printf 'x\n' > "$E/link/usr/lib/app/app"; ln -s ../lib/app/app "$E/link/usr/bin/app"
is "a symlink on PATH counts, which is what claude-desktop ships" "/usr/bin/app" \
   "$(call deb_entry_point "$E/link")"
mkdir -p "$E/desktop/opt/App" "$E/desktop/usr/share/applications"
printf 'x\n' > "$E/desktop/opt/App/app"
printf '[Desktop Entry]\nName=App\nExec=/opt/App/app %%U\nType=Application\n' \
    > "$E/desktop/usr/share/applications/app.desktop"
is "a desktop entry whose Exec is a shipped path counts" "/opt/App/app" \
   "$(call deb_entry_point "$E/desktop")"
mkdir -p "$E/ghost/opt/App" "$E/ghost/usr/share/applications"
printf 'x\n' > "$E/ghost/opt/App/data"
printf '[Desktop Entry]\nName=App\nExec=/usr/bin/made-by-postinst %%U\nType=Application\n' \
    > "$E/ghost/usr/share/applications/app.desktop"
is "a desktop entry naming a path the package does NOT ship does not count" false \
   "$(succeeds deb_entry_point "$E/ghost" && echo true || echo false)"
mkdir -p "$E/none/opt/pt"; printf 'x\n' > "$E/none/opt/pt/blob"
is "a payload with no program at all has no entry point" false \
   "$(succeeds deb_entry_point "$E/none" && echo true || echo false)"
# The refusal that case produces has to name the maintainer scripts, because
# that is the only sentence that tells the user why a package that "contains"
# their program will not install.
NE=$WORK/noentry; mkdir -p "$NE/ctl" "$NE/data/opt/pt"
control_file "$NE/ctl" pt 1.0 "$DEBARCH"
add_postinst "$NE/ctl"
printf 'blob\n' > "$NE/data/opt/pt/blob"
mkdeb "$WORK/noentry.deb" "$NE/ctl" "$NE/data"

echo "── image ownership: the guard rpm -qf cannot be ───────────────────────"
S=$WORK/shadow
rm -rf "$S"; mkdir -p "$S/img/usr/bin" "$S/root" "$S/pkg/usr/bin"
printf 'image\n' > "$S/img/usr/bin/hello"
printf 'deb\n'   > "$S/pkg/usr/bin/hello"
out=$(callerr deb_shadow_ok "$S/pkg" "$S/root" "$S/img" hello)
if grep -qF "Rime OS already provides '/usr/bin/hello'" <<<"$out"; then
    ok "a .deb over a file the image provides is refused, and the path is named"
else
    bad "a .deb over a file the image provides is refused, and the path is named" \
        "got: $(head -1 <<<"$out")"
fi
rm -rf "$S/img2"; mkdir -p "$S/img2/usr/bin"
is "…and the same package is allowed where the image has nothing" true \
   "$(succeeds deb_shadow_ok "$S/pkg" "$S/root" "$S/img2" hello && echo true || echo false)"
# `[ -e ]` is false for a dangling symlink. A dangling symlink is still a file
# the image ships, and shadowing it is still shadowing.
rm -rf "$S/img3"; mkdir -p "$S/img3/usr/bin"; ln -s /nowhere "$S/img3/usr/bin/hello"
is "a dangling symlink in the image still counts as owned" false \
   "$(succeeds deb_shadow_ok "$S/pkg" "$S/root" "$S/img3" hello && echo true || echo false)"
# The other half: a path this same transaction's RPMs already placed.
rm -rf "$S/root2"; mkdir -p "$S/root2/usr/bin"; printf 'rpm\n' > "$S/root2/usr/bin/hello"
out=$(callerr deb_shadow_ok "$S/pkg" "$S/root2" "$S/img2" hello)
if grep -qF "also shipped by a package already in this transaction" <<<"$out"; then
    ok "a collision with an rpm in the same transaction is refused"
else
    bad "a collision with an rpm in the same transaction is refused" "got: $(head -1 <<<"$out")"
fi
# Two packages sharing /usr/bin is not a conflict; a package whose DIRECTORY
# lands where the image keeps a file is.
rm -rf "$S/img4" "$S/pkg4"; mkdir -p "$S/img4/usr/bin" "$S/pkg4/usr/share/applications"
printf 'a file where a directory should be\n' > "$S/img4/usr/share"
is "a directory over an image file is refused" false \
   "$(succeeds deb_shadow_ok "$S/pkg4" "$S/root" "$S/img4" x && echo true || echo false)"
rm -rf "$S/img5"; mkdir -p "$S/img5/usr/bin" "$S/img5/usr/share/applications"
is "…but sharing a directory with the image is not" true \
   "$(succeeds deb_shadow_ok "$S/pkg4" "$S/root" "$S/img5" x && echo true || echo false)"

echo "── where the image actually is ────────────────────────────────────────"
# The overlay trap: while an extension is merged, /usr is an overlayfs whose
# upper layers ARE the extension, so reading the running /usr would report
# everything the last build placed as belonging to the image.
IMGROOT=$(call image_hierarchy_root)
if [ -e /run/ostree-booted ]; then
    if [ -n "$IMGROOT" ] && [ -d "${IMGROOT}/usr" ]; then
        ok "the image hierarchy resolves to a real tree with a /usr"
    else
        bad "the image hierarchy resolves to a real tree with a /usr" "got '${IMGROOT}'"
    fi
    if [ "$IMGROOT" = "/" ]; then
        bad "…and it is NOT the running root, which the extension overlays" \
            "it answered / on an ostree system"
    else
        ok "…and it is NOT the running root, which the extension overlays"
    fi
    # The property that makes it the right oracle: the same path read through
    # the deployment and through the merged / can disagree.
    if [ -e "${IMGROOT}/usr/lib/os-release" ]; then
        ok "the deployment tree really is readable"
    else
        bad "the deployment tree really is readable" "no ${IMGROOT}/usr/lib/os-release"
    fi
else
    is "a machine with no ostree deployment uses its own root" "/" "$IMGROOT"
    skipped "…and it is NOT the running root, which the extension overlays" \
            "no ostree deployment here"
    skipped "the deployment tree really is readable" "no ostree deployment here"
fi

echo "── the sequence extract_debs runs, over the well-formed fixture ───────"
# Production order, asserted against production so this cannot drift into
# testing something extract_debs no longer does.
body=$(sed -n '/^extract_debs()/,/^}/p' "$ENGINE")
order=$(grep -oE 'deb_names_ok|deb_layout_ok|deb_shadow_ok|deb_entry_point|cp -a' <<<"$body" | tr '\n' ' ')
is "extract_debs guards before it merges" \
   "deb_names_ok deb_layout_ok deb_shadow_ok deb_entry_point cp -a " "$order"
if grep -q 'LD_DATA_MEMBER' <<<"$body" && ! grep -q 'control\.tar' <<<"$body"; then
    ok "extract_debs unpacks the data member and never the control member"
else
    bad "extract_debs unpacks the data member and never the control member" \
        "it reaches for the control archive"
fi
# …and rebuild_extension runs it after the rpm pass, which is what lets the
# shadow guard see this transaction's own rpm paths.
rb=$(sed -n '/^rebuild_extension()/,/^}/p' "$ENGINE")
if [ "$(grep -n 'extract_rpms "\$rpms" "\$root"' <<<"$rb" | cut -d: -f1)" -lt \
     "$(grep -n 'extract_debs "\$root"' <<<"$rb" | cut -d: -f1)" ]; then
    ok "rebuild_extension extracts rpms before .debs"
else
    bad "rebuild_extension extracts rpms before .debs" "the .deb pass cannot see rpm paths"
fi
if [ "$(grep -n 'extract_debs "\$root"' <<<"$rb" | cut -d: -f1)" -lt \
     "$(grep -n 'fix_caches "\$root"' <<<"$rb" | cut -d: -f1)" ]; then
    ok "…and rebuilds the desktop/MIME caches after them"
else
    bad "…and rebuilds the desktop/MIME caches after them" \
        "the entry the postinst would have registered reaches no cache"
fi

P=$WORK/payload; rm -rf "$P"; mkdir -p "$P/root" "$P/img" "$P/scratch"
member=$(call deb_members "$WORK/hello.deb" | grep '^data\.tar')
call deb_names_ok "$WORK/hello.deb" "$member" hello >/dev/null 2>&1
call deb_tar "$WORK/hello.deb" "$member" -xf - -C "$P/scratch" --no-same-owner -p >/dev/null 2>&1
call deb_layout_ok "$P/scratch" hello >/dev/null 2>&1 \
    && ok "the well-formed fixture passes the layout rules" \
    || bad "the well-formed fixture passes the layout rules" "it was refused"
call deb_shadow_ok "$P/scratch" "$P/root" "$P/img" hello >/dev/null 2>&1 \
    && ok "…and shadows nothing in an empty image" \
    || bad "…and shadows nothing in an empty image" "it was refused"
is "…and its entry point is the program on PATH" "/usr/bin/hello" \
   "$(call deb_entry_point "$P/scratch")"
cp -a "$P/scratch/." "$P/root/" 2>/dev/null

is "the program is in the payload"      "yes" "$([ -L "$P/root/usr/bin/hello" ] && echo yes || echo no)"
is "the private library is in the payload" "yes" \
   "$([ -f "$P/root/usr/lib/hello/libprivate.so" ] && echo yes || echo no)"
is "the desktop entry is in the payload" "yes" \
   "$([ -f "$P/root/usr/share/applications/hello.desktop" ] && echo yes || echo no)"
is "the icon is in the payload"         "yes" \
   "$([ -f "$P/root/usr/share/icons/hicolor/48x48/apps/hello.png" ] && echo yes || echo no)"
is "the symlink is a symlink, not the file it points at" \
   "../lib/hello/hello" "$(readlink "$P/root/usr/bin/hello")"
# claude-desktop ships chrome-sandbox 4755 in its data archive and Chromium
# refuses to start when it finds that helper present but not setuid, so the
# mode is not decoration.
is "a setuid bit in the data archive survives extraction" "4755" \
   "$(stat -c %a "$P/root/usr/lib/hello/helper-sandbox" 2>/dev/null)"
is "an ordinary executable keeps its mode" "755" \
   "$(stat -c %a "$P/root/usr/lib/hello/hello" 2>/dev/null)"

echo "── the maintainer scripts are never run, and never shipped ────────────"
is "the sentinel a postinst would write does not exist" "no" \
   "$([ -e "$SENTINEL" ] && echo yes || echo no)"
is "the postinst itself is not in the payload" "0" \
   "$(find "$P/root" -name 'postinst' -o -name 'preinst' -o -name 'postrm' | wc -l)"
is "the fixture really does carry a postinst" "postinst" \
   "$(bash -c 'e=$1; d=$2; set --; source "$e" >/dev/null 2>&1; inspect_local_deb "$d" >/dev/null 2>&1; echo "$LD_SCRIPTS"' _ "$ENGINE_ABS" "$WORK/hello.deb")"
is "…and its Depends are read, not resolved" "libgtk-3-0, libc6 (>= 2.34)" \
   "$(bash -c 'e=$1; d=$2; set --; source "$e" >/dev/null 2>&1; inspect_local_deb "$d" >/dev/null 2>&1; echo "$LD_DEPENDS"' _ "$ENGINE_ABS" "$WORK/hello.deb")"

echo "── the resolved set is what makes rime update notice a new .deb ───────"
rs=$(call resolved_set "$WORK/nosuchdir" "$WORK/hello.deb")
case "$rs" in
    deb:hello-1.2.3."$DEBARCH"+????????????) ok "a .deb contributes one identity line with its checksum" ;;
    *) bad "a .deb contributes one identity line with its checksum" "got '$rs'" ;;
esac
cp -f "$WORK/hello.deb" "$WORK/hello-reissued.deb"
printf '\n' >> "$WORK/hello-reissued.deb"
rs2=$(call resolved_set "$WORK/nosuchdir" "$WORK/hello-reissued.deb")
if [ "$rs" != "$rs2" ]; then
    ok "a reissued file at the same version is a different set, so it rebuilds"
else
    bad "a reissued file at the same version is a different set, so it rebuilds" \
        "both read '$rs'"
fi

echo "── the engine advertises the .deb form, and its limits ────────────────"
help=$(bash "$ENGINE" --help 2>&1)
for want in 'file.deb' '/var/lib/rime/pkg/deb' 'maintainer scripts' \
            'apt repositories' '--allow-unsigned'; do
    if grep -qF -- "$want" <<<"$help"; then ok "--help mentions: $want"
    else bad "--help mentions: $want" "absent"; fi
done

echo "── leg B: extract_debs for real, as root, in a throwaway container ────"
# PKG_ROOT is a readonly constant pointing at /var/lib/rime, and the manifest,
# the entry-point record and the dependency record are written there. Nothing
# unprivileged can exercise that, and a test must never write it on the machine
# running the suite — so it happens in a container, the same reason
# tests/test-rime-pkg-update.sh reaches for one.
if ! command -v podman >/dev/null 2>&1; then
    skipped "leg B: extract_debs end to end" "podman is absent"
else
    PROBE=$WORK/probe.sh
    cat > "$PROBE" <<'PROBE_EOF'
set -uo pipefail
dnf5 -y install binutils findutils gawk >/dev/null 2>&1 \
  || dnf -y install binutils findutils gawk >/dev/null 2>&1 \
  || { echo "PROBE_SKIP no repository reachable, so binutils cannot be installed"; exit 0; }
command -v ar >/dev/null 2>&1 || { echo "PROBE_SKIP ar is still absent"; exit 0; }

B=/tmp/b; rm -rf "$B"; mkdir -p "$B/ctl" "$B/data/usr/bin" "$B/data/usr/lib/hello" \
                                "$B/data/usr/share/applications"
cat > "$B/ctl/control" <<CTL
Package: hello
Version: 1.2.3
Architecture: amd64
Maintainer: Rime test <t@example.invalid>
Depends: libgtk-3-0, libc6 (>= 2.34)
Description: a fixture
CTL
cat > "$B/ctl/postinst" <<'PI'
#!/bin/sh
echo ran > /tmp/MAINTAINER-SCRIPT-RAN
mkdir -p /etc/apt/sources.list.d
echo 'deb https://example.invalid/ stable main' > /etc/apt/sources.list.d/hello.list
PI
chmod 755 "$B/ctl/postinst"
printf '#!/bin/sh\necho hello\n' > "$B/data/usr/lib/hello/hello"; chmod 755 "$B/data/usr/lib/hello/hello"
printf 'sandbox\n' > "$B/data/usr/lib/hello/helper-sandbox"; chmod 4755 "$B/data/usr/lib/hello/helper-sandbox"
ln -s ../lib/hello/hello "$B/data/usr/bin/hello"
printf '[Desktop Entry]\nName=Hello\nExec=/usr/bin/hello\nType=Application\n' \
    > "$B/data/usr/share/applications/hello.desktop"
mk() {  # mk <out> <ctldir> <datadir>
    local t; t=$(mktemp -d)
    printf '2.0\n' > "$t/debian-binary"
    tar -czf "$t/control.tar.gz" -C "$2" .
    tar -czf "$t/data.tar.gz" -C "$3" .
    ( cd "$t" && ar rc "$1" debian-binary control.tar.gz data.tar.gz )
}
mk /tmp/hello.deb "$B/ctl" "$B/data"

# A second package that ships a path this container's own /usr really has.
S=/tmp/s; rm -rf "$S"; mkdir -p "$S/ctl" "$S/data/usr/bin"
sed 's/^Package: hello/Package: shadower/' "$B/ctl/control" > "$S/ctl/control"
printf 'not really ls\n' > "$S/data/usr/bin/ls"; chmod 755 "$S/data/usr/bin/ls"
mk /tmp/shadower.deb "$S/ctl" "$S/data"

# A data package: no program, no maintainer script. A font or an icon theme
# has nothing to start, and nothing that was ever going to create one.
D=/tmp/d; rm -rf "$D"; mkdir -p "$D/ctl" "$D/data/usr/share/fonts/truetype/fx"
sed -e 's/^Package: hello/Package: fixture-fonts/' "$B/ctl/control" > "$D/ctl/control"
printf 'TTF\n' > "$D/data/usr/share/fonts/truetype/fx/Fixture.ttf"
mk /tmp/fonts.deb "$D/ctl" "$D/data"
# The same payload, but now a maintainer script is what would have made it
# usable — that is the refusal.
cp -a "$D" /tmp/d2
cp "$B/ctl/postinst" /tmp/d2/ctl/postinst
sed -i 's/^Package: fixture-fonts/Package: fixture-fonts-scripted/' /tmp/d2/ctl/control
mk /tmp/fonts-scripted.deb /tmp/d2/ctl /tmp/d2/data

set --
source /repo/files/system/libexec/rime-pkg >/dev/null 2>&1
set +e
WORK=/tmp/w; rm -rf "$WORK"; mkdir -p "$WORK/root"; export WORK

# A subshell, because a refusal is `die`, which is `exit 1` — in this sourced
# shell that would end the probe and every assertion after it would read as a
# podman failure rather than as the refusal it is.
( extract_debs "$WORK/root" /tmp/hello.deb ) >/tmp/out.log 2>&1
echo "PROBE_RC_GOOD $?"
sed 's/^/PROBE_LOG /' /tmp/out.log
echo "PROBE_ENTRY $(cat /var/lib/rime/pkg/deb/hello.entry 2>/dev/null)"
echo "PROBE_MANIFEST $(wc -l < /var/lib/rime/pkg/deb/hello.files 2>/dev/null || echo 0)"
echo "PROBE_MANIFEST_HAS $(grep -c '^/usr/bin/hello$' /var/lib/rime/pkg/deb/hello.files 2>/dev/null || echo 0)"
echo "PROBE_DEPENDS $(cat /var/lib/rime/pkg/deb/hello.depends 2>/dev/null)"
echo "PROBE_PAYLOAD_LINK $(readlink "$WORK/root/usr/bin/hello" 2>/dev/null)"
echo "PROBE_PAYLOAD_MODE $(stat -c %a "$WORK/root/usr/lib/hello/helper-sandbox" 2>/dev/null)"
echo "PROBE_SENTINEL $([ -e /tmp/MAINTAINER-SCRIPT-RAN ] && echo yes || echo no)"
echo "PROBE_APTSOURCE $([ -e /etc/apt/sources.list.d/hello.list ] && echo yes || echo no)"
echo "PROBE_SCRIPT_IN_PAYLOAD $(find "$WORK/root" -name postinst | wc -l)"

rm -rf "$WORK/root2"; mkdir -p "$WORK/root2"
( extract_debs "$WORK/root2" /tmp/shadower.deb ) >/tmp/out2.log 2>&1
echo "PROBE_RC_SHADOW $?"

# The trust gate lives inside cmd_install, past the root check, so nothing
# unprivileged can reach it. Run the engine as a process, as root, with and
# without the flag.
bash /repo/files/system/libexec/rime-pkg install /tmp/hello.deb >/tmp/out3.log 2>&1
echo "PROBE_RC_NOFLAG $?"
echo "PROBE_NOFLAG_VERIFY $(grep -c 'cannot verify' /tmp/out3.log)"
echo "PROBE_NOFLAG_CACHED $([ -e /var/lib/rime/pkg/deb/hello.deb ] && echo yes || echo no)"
bash /repo/files/system/libexec/rime-pkg install --allow-unsigned /tmp/hello.deb >/tmp/out4.log 2>&1
echo "PROBE_FLAG_ACCEPTED $(grep -c 'accepting /tmp/hello.deb unverified' /tmp/out4.log)"

rm -rf "$WORK/root3"; mkdir -p "$WORK/root3"
( extract_debs "$WORK/root3" /tmp/fonts.deb ) >/tmp/out5.log 2>&1
echo "PROBE_RC_DATA $?"
echo "PROBE_DATA_SAID $(grep -c 'a data package' /tmp/out5.log)"
echo "PROBE_DATA_FILE $([ -e "$WORK/root3/usr/share/fonts/truetype/fx/Fixture.ttf" ] && echo yes || echo no)"
echo "PROBE_DATA_ENTRY $(cat /var/lib/rime/pkg/deb/fixture-fonts.entry 2>/dev/null)"
rm -rf "$WORK/root4"; mkdir -p "$WORK/root4"
( extract_debs "$WORK/root4" /tmp/fonts-scripted.deb ) >/tmp/out6.log 2>&1
echo "PROBE_RC_DATA_SCRIPTED $?"
echo "PROBE_DATA_SCRIPTED_MSG $(grep -c 'created by its maintainer script' /tmp/out6.log)"
grep -o "already provides '/usr/bin/ls'" /tmp/out2.log | head -1 | sed 's/^/PROBE_SHADOW_MSG /'
sed 's/^/PROBE_LOG2 /' /tmp/out2.log
PROBE_EOF
    out=$(podman run --rm \
            -v "$REPO_ROOT":/repo:ro,Z -v "$PROBE":/probe.sh:ro,Z \
            "${RIME_DEB_IMAGE:-registry.fedoraproject.org/fedora:43}" \
            bash /probe.sh 2>&1); prc=$?
    if [ "$prc" != 0 ] && ! grep -q PROBE_RC_GOOD <<<"$out"; then
        skipped "leg B: extract_debs end to end" \
                "podman exited ${prc}: $(tail -2 <<<"$out" | tr '\n' ' ')"
    elif grep -q PROBE_SKIP <<<"$out"; then
        skipped "leg B: extract_debs end to end" \
                "$(grep PROBE_SKIP <<<"$out" | sed 's/PROBE_SKIP //')"
    else
        probe() { grep "^$1 " <<<"$out" | head -1 | sed "s/^$1 //"; }
        is "leg B: extract_debs succeeds on a well-formed .deb" "0" "$(probe PROBE_RC_GOOD)"
        is "leg B: the entry point is recorded" "/usr/bin/hello" "$(probe PROBE_ENTRY)"
        is "leg B: the manifest names the program" "1" "$(probe PROBE_MANIFEST_HAS)"
        is "leg B: the Debian dependencies are recorded verbatim" \
           "libgtk-3-0, libc6 (>= 2.34)" "$(probe PROBE_DEPENDS)"
        is "leg B: the symlink reached the payload as a symlink" \
           "../lib/hello/hello" "$(probe PROBE_PAYLOAD_LINK)"
        is "leg B: the setuid bit reached the payload" "4755" "$(probe PROBE_PAYLOAD_MODE)"
        # The whole point. dpkg would have run this postinst as root.
        is "leg B: the postinst did NOT run" "no" "$(probe PROBE_SENTINEL)"
        is "leg B: …so no apt source was written" "no" "$(probe PROBE_APTSOURCE)"
        is "leg B: the maintainer script is not in the payload either" "0" \
           "$(probe PROBE_SCRIPT_IN_PAYLOAD)"
        if [ "$(probe PROBE_RC_SHADOW)" = "0" ]; then
            bad "leg B: a .deb over a real /usr/bin file is refused" "it was installed"
        else
            ok "leg B: a .deb over a real /usr/bin file is refused"
        fi
        is "leg B: …and the refusal names the path" "already provides '/usr/bin/ls'" \
           "$(probe PROBE_SHADOW_MSG)"
        # The trust gate: every .deb needs --allow-unsigned, and the refusal
        # must happen BEFORE the file is copied into the cache — a refused
        # package that left a cached copy behind would sit there until the next
        # successful operation retired it.
        if [ "$(probe PROBE_RC_NOFLAG)" = "0" ]; then
            bad "leg B: a .deb without --allow-unsigned is refused" "it was installed"
        else
            ok "leg B: a .deb without --allow-unsigned is refused"
        fi
        if [ "$(probe PROBE_NOFLAG_VERIFY)" = "0" ]; then
            bad "leg B: …and the refusal is the trust refusal" "some other error stopped it"
        else
            ok "leg B: …and the refusal is the trust refusal"
        fi
        is "leg B: …and nothing was cached on the way to refusing" "no" \
           "$(probe PROBE_NOFLAG_CACHED)"
        if [ "$(probe PROBE_FLAG_ACCEPTED)" = "0" ]; then
            bad "leg B: --allow-unsigned gets past the gate, saying so" "no acceptance line"
        else
            ok "leg B: --allow-unsigned gets past the gate, saying so"
        fi

        # A package with no program AND no maintainer script is data, not a
        # broken install. Refusing it would be refusing it for a reason that is
        # not true — the rule is "useless without its postinst", and a package
        # with no postinst cannot be.
        is "leg B: a data package with no maintainer script installs" "0" \
           "$(probe PROBE_RC_DATA)"
        is "leg B: …and says that is what it is" "1" "$(probe PROBE_DATA_SAID)"
        is "leg B: …and its files reach the payload" "yes" "$(probe PROBE_DATA_FILE)"
        is "leg B: …and a witness path is recorded for later rebuilds" \
           "/usr/share/fonts/truetype/fx/Fixture.ttf" "$(probe PROBE_DATA_ENTRY)"
        if [ "$(probe PROBE_RC_DATA_SCRIPTED)" = "0" ]; then
            bad "leg B: the same payload WITH a maintainer script is refused" "it installed"
        else
            ok "leg B: the same payload WITH a maintainer script is refused"
        fi
        is "leg B: …and the refusal names the script as the reason" "1" \
           "$(probe PROBE_DATA_SCRIPTED_MSG)"

        # Every warning the user must see is in the log, not only in the source.
        if grep -q "PROBE_LOG.*were NOT run" <<<"$out"; then
            ok "leg B: the install says the maintainer scripts were not run"
        else
            bad "leg B: the install says the maintainer scripts were not run" \
                "no such warning in the output"
        fi
        if grep -q "PROBE_LOG.*resolved none of its Debian dependencies" <<<"$out"; then
            ok "leg B: …and that it resolved none of the dependencies"
        else
            bad "leg B: …and that it resolved none of the dependencies" \
                "no such warning in the output"
        fi
    fi
fi

echo
printf 'rime-deb: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
