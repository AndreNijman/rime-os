#!/bin/sh
# Render the greeter's logo from the layered-ridge adaptive template (files/branding/ridge/templates):
# assets/ridge-lockup-base.png = the horizontal lockup with planes 1, 2, 4 and the wordmark in off-white and plane 3
# left out. GreetSurface.qml draws plane 3 itself, as a polygon in the accent colour (Rime Blue, or the user's
# published accent), so only plane 3 ever changes colour and no shader is needed to tint it.
#
# Needs resvg pinned to the flake's nixpkgs:
#   nix shell github:NixOS/nixpkgs/<rev in flake.lock>#resvg --command ./make-logo-assets.sh
# checks.x86_64-linux.branding-surfaces re-renders it and fails if the committed PNG differs.
# Arguments (optional): the templates directory, the output directory.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tpl=${1:-$here/../../branding/ridge/templates}
out=${2:-$here/assets}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e 's/{{fg}}/#FAFAFA/g' -e 's/{{accent}}/none/g' "$tpl/rime-lockup-horizontal.svg.in" > "$tmp/base.svg"
mkdir -p "$out"
# 3x the 56 px the greeter draws it at, so a HiDPI output stays sharp.
resvg --height 168 "$tmp/base.svg" "$out/ridge-lockup-base.png"
