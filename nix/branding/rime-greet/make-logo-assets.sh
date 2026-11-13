#!/bin/sh
# Render the NixOS greeter's logo from the Origin adaptive template (files/branding/ridge/templates):
# assets/ridge-lockup-base.png = the horizontal lockup's wordmark in off-white, on the lockup's canvas, with the mark
# left out. GreetSurface.qml draws the mark itself, as vectors in the accent colour (Rime Blue, or the user's
# published accent), so no shader is needed to tint it. (The file name is the one it had under the layered ridge.)
#
# The bootc image's greeter keeps the spark (files/desktop/rime-greet); nix/pkgs/rime-ridge-files.nix puts this
# asset into the NixOS greeter tree. Needs resvg pinned to the flake's nixpkgs:
#   nix shell github:NixOS/nixpkgs/<rev in flake.lock>#resvg --command ./make-logo-assets.sh
# checks.x86_64-linux.branding-surfaces re-renders it and fails if the committed PNG differs.
# Arguments (optional): the templates directory, the output directory.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tpl=${1:-$here/../../../files/branding/ridge/templates}
out=${2:-$here/assets}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e 's/{{fg}}/#FAFAFA/g' -e 's/{{accent}}/none/g' "$tpl/rime-lockup-horizontal.svg.in" > "$tmp/base.svg"
mkdir -p "$out"
# 3x the 56 px the greeter draws it at, so a HiDPI output stays sharp.
resvg --height 168 "$tmp/base.svg" "$out/ridge-lockup-base.png"
