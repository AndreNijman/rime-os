#!/bin/sh
# Regenerate every Rime brand export (the Origin mark) (svg/ templates/ png/ icons/ favicon/ accents/) from the masters in src/.
# Needs resvg and python3 (standard library only), pinned to the flake's nixpkgs. From this directory:
#
#   nix shell github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#resvg \
#     github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325#python3 \
#     --command ./make-ridge-assets.sh
#
# (Use the nixpkgs rev in flake.lock if it has moved.) Then commit the result: checks.x86_64-linux.branding
# regenerates the exports and fails on any file that differs from what is committed.
# Optional argument: an output directory (default: this directory).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
exec python3 "$here/make-ridge-assets.py" "${1:-$here}"
