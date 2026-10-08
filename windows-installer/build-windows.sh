#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  build-windows.sh — cross-compile the portable installer to a Windows .exe.
#
#  WHY A CONTAINER
#
#  This repository is built and developed on Rime, which is bootc and
#  read-only: `dnf install` on the host is refused outright ("this bootc system
#  is configured to be read-only"). So the toolchain cannot be installed
#  alongside the source, and the first round of this work recorded "No Windows
#  target/linker or Windows VM is available here" and stopped. It was available
#  the whole time; it just could not live on the host.
#
#  WHY THE VERSIONS ARE PINNED THE WAY THEY ARE
#
#  Rust's standard library for a target must match the compiler EXACTLY — a
#  1.98 std will not link against a 1.97 rustc. Fedora ships
#  `rust-std-static-x86_64-pc-windows-gnu` alongside `rust`, so taking both
#  from the same repository snapshot is what keeps them in step. That is the
#  usual reason cross-compiling Rust "cannot be done" on a machine where it
#  demonstrably can.
#
#  `mingw64-winpthreads-static` is not incidental: without it the GNU target
#  links against a DLL the target machine will not have.
#
#  USAGE
#
#      windows-installer/build-windows.sh [OUTDIR]
#
#  The installer image the .exe installs from is fixed at compile time
#  (src/pin.rs). Set RIME_ISO_URL, RIME_ISO_BYTES and RIME_ISO_SHA256 to bake
#  one in; without them the .exe builds and runs, and refuses to install.
#
#  Writes rime-windows-installer.exe into OUTDIR (default ./dist). Prints the
#  produced file's type, because "the build exited 0" and "a PE32+ binary
#  exists" are different claims and only the second one matters.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/dist}"
IMAGE="${RIME_WIN_BUILD_IMAGE:-registry.fedoraproject.org/fedora:43}"

if ! command -v podman >/dev/null 2>&1; then
    echo "SKIP  podman is absent; the Windows cross-build needs a container" >&2
    exit 77
fi

mkdir -p "$OUT"

# `:z` rather than `:Z`: OUT is often the shared lab work directory, and a
# private MCS category pair would take it away from a guest run already using
# it. `cp -r /src /build` rather than building in the bind mount: the mount is
# read-only on purpose, so a stray `target/` cannot land in the work tree and
# the host's own build artefacts cannot influence the result.
podman run --rm \
    -e RIME_ISO_URL="${RIME_ISO_URL:-}" -e RIME_ISO_BYTES="${RIME_ISO_BYTES:-}" \
    -e RIME_ISO_SHA256="${RIME_ISO_SHA256:-}" \
    -v "$HERE":/src:ro,z \
    -v "$OUT":/out:z \
    "$IMAGE" bash -euo pipefail -c '
        dnf install -y -q --setopt=install_weak_deps=False \
            rust cargo mingw64-gcc mingw64-winpthreads-static mingw64-binutils \
            rust-std-static-x86_64-pc-windows-gnu >/dev/null

        echo "toolchain: $(rustc --version)"
        # Assert the target std is actually present. Without this the build
        # fails later with a link error that reads like a source problem.
        test -d /usr/lib/rustlib/x86_64-pc-windows-gnu \
            || { echo "FATAL: no x86_64-pc-windows-gnu std in this image" >&2; exit 1; }

        cp -r /src /build
        cd /build
        [ -n "$RIME_ISO_SHA256" ] && echo "pinned image: $RIME_ISO_URL ($RIME_ISO_BYTES bytes, sha256 $RIME_ISO_SHA256)" \
            || echo "no image pinned: this .exe will refuse to install"
        cargo build --release --locked --offline --target x86_64-pc-windows-gnu

        exe=target/x86_64-pc-windows-gnu/release/rime-windows-installer.exe
        test -s "$exe" || { echo "FATAL: no .exe produced" >&2; exit 1; }
        cp "$exe" /out/
    '

exe="$OUT/rime-windows-installer.exe"
test -s "$exe" || { echo "FATAL: $exe missing after the build" >&2; exit 1; }

# The build exiting 0 is not the claim worth making. This is.
type_line="$(file -b "$exe")"
printf 'built: %s\n' "$exe"
printf 'type : %s\n' "$type_line"
case "$type_line" in
    PE32+*x86-64*) ;;
    *) echo "FATAL: not a 64-bit Windows PE: $type_line" >&2; exit 1 ;;
esac
