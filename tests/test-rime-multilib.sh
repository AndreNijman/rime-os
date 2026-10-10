#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-multilib.sh — ask the shipped package engine to judge a REAL
#  multilib package set, and assert the decisions it makes.
#
#  ── Why this exists next to test-rime-pkg.sh ────────────────────────────────
#  test-rime-pkg.sh asserts the argv the engine builds. That is the right level
#  for most of it, and it is not enough here: `rime install steam` was broken by
#  six defects, and the one that reached a user's screen produced correct argv
#  and a wrong result. So this one runs the engine against packages Fedora
#  actually ships and reads back what it decided.
#
#  ── Why it decides rather than extracts ─────────────────────────────────────
#  Extracting a full set needs the container's installed versions to match the
#  repository exactly. When they do not, the guard correctly refuses the whole
#  transaction — which is the engine working, and tells you nothing about
#  multilib. Two earlier drafts of this test passed against a broken engine for
#  that reason. The decisions are the discriminating step.
#
#  ── Why a container ─────────────────────────────────────────────────────────
#  It needs a real dnf, a real repository and a package set whose i686 builds
#  carry /usr/bin, and it must not touch the machine running it. Skips out loud
#  where podman is absent or cannot run privileged, so the suite stays
#  meaningful on a plain runner.
#
#  Run from anywhere: ./tests/test-rime-multilib.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

ENGINE=files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }

if ! command -v podman >/dev/null 2>&1; then
    echo "SKIP  podman is absent; this suite needs a real dnf and repository"
    exit 0
fi

IMAGE=${RIME_MULTILIB_IMAGE:-registry.fedoraproject.org/fedora:43}
PROBE=$(mktemp) || exit 2
trap 'rm -f "$PROBE"' EXIT

cat > "$PROBE" <<'PROBE_EOF'
set -uo pipefail
PKG=/repo/files/system/libexec/rime-pkg
WORK=/tmp/e2e; rm -rf "$WORK"; mkdir -p "$WORK/dl"; export WORK

# fontconfig pulls in glibc, and both ship an i686 build carrying /usr/bin.
# glibc is also on the engine's protected list, which is what makes it the
# package that proves the multilib rule rather than merely exercising it.
dnf5 -y download --resolve --arch=x86_64 --arch=noarch --arch=i686 \
    --destdir "$WORK/dl" fontconfig >/dev/null 2>&1 \
    || { echo "PROBE_SKIP no repository reachable"; exit 0; }

# A native build the "image" ALREADY HAS, at the same EVR. Without one the
# third decision below cannot be exercised at all: `download --resolve` fetches
# only what is NOT installed, so the set it builds can never contain a native
# overlap, and the assertion that the engine still leaves such a build out
# reported SKIP on every run — a could-not-run wearing a pass.
#
# Install first, then download the same name in the same transaction's view of
# the repository, so the two EVRs are equal by construction rather than by
# luck. `zip` is not in the base image, is on no protected list, and pulls
# nothing: what is wanted here is an ordinary package, not an interesting one.
if dnf5 -y install zip >/dev/null 2>&1 \
   && dnf5 -y download --arch=x86_64 --destdir "$WORK/dl" zip >/dev/null 2>&1; then
    echo "PROBE_NATIVE ok"
else
    echo "PROBE_NATIVE failed"
fi

echo "PROBE_SET $(ls "$WORK/dl" | wc -l) rpms, $(ls "$WORK/dl" | grep -c i686) i686"
bash -c "source $PKG >/dev/null 2>&1; set +e; guard_rpms '$WORK/dl'" 2>&1 \
    | grep -oE "(allowing|refusing|skipping|omitting) '[^']*'" \
    | sed 's/^/PROBE_DECISION /'
PROBE_EOF

out=$(podman run --rm --privileged \
        -v "$PWD":/repo:ro,Z -v "$PROBE":/probe.sh:ro,Z \
        "$IMAGE" bash /probe.sh 2>&1)

if printf '%s\n' "$out" | pipe_has PROBE_SKIP; then
    echo "SKIP  $(printf '%s\n' "$out" | grep PROBE_SKIP | sed 's/PROBE_SKIP //')"
    exit 0
fi

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }

printf '%s\n' "$out" | grep PROBE_SET | sed 's/PROBE_SET/      set:/'
decisions=$(printf '%s\n' "$out" | grep PROBE_DECISION | sed 's/PROBE_DECISION //')
printf '%s\n' "$decisions" | sed 's/^/      /' | head -8

# The defect that made every 32-bit application uninstallable: REFUSE_RE
# protects core names so an extension cannot shadow the image's copy, and a
# 32-bit sibling shadows nothing, because the image ships no i686 at all.
if printf '%s\n' "$decisions" | pipe_has "allowing 'glibc-.*\.i686'"; then
    ok "a 32-bit sibling of a protected package is allowed through"
elif printf '%s\n' "$decisions" | pipe_has "refusing 'glibc'"; then
    bad "glibc.i686 refused as a core package, so no 32-bit application can install"
else
    # Not a SKIP. `fontconfig` requires glibc and the download asked for i686,
    # so glibc.i686 is in this set by construction; its absence means the set
    # was not built the way this suite believes, and every decision read below
    # is then about some other set. That is a failure, not an abstention.
    bad "glibc.i686 was not in the set at all, so the multilib rule was never exercised"
fi

# The defect that silently deleted the libraries: the installed-check asked
# `rpm -q <name>` with no architecture, so on multilib it answered with the
# x86_64 build and every i686 library was dropped as already present.
if printf '%s\n' "$decisions" | pipe_has -E "skipping '[^']*\.i686'.*"; then
    bad "an i686 build was dropped as already provided by the image"
else
    ok "no i686 build was mistaken for one the image already ships"
fi

# The fix must not become "install everything": a native build the image
# already ships at the same version must still be skipped, or the overlay
# shadows the image from the other direction.
if ! printf '%s\n' "$out" | pipe_has 'PROBE_NATIVE ok'; then
    bad "the set could not be given a native build the image already has, so this decision was never exercised"
elif printf '%s\n' "$decisions" | pipe_has -E "(skipping|omitting) '[^']*\.(x86_64|noarch)'"; then
    ok "a native build the image already provides is still left out"
else
    bad "a native build the image already ships was carried into the overlay, which shadows the image from the other direction"
fi

echo
printf 'rime-multilib: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
