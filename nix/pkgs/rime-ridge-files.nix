# Rime's identity on NixOS: the Origin mark in Rime Blue (#91C2DC). The bootc image keeps the
# chartreuse spark, so the files both systems share (files/**, which Containerfile.base copies as they are) hold the
# spark, and this package is where NixOS gets the Origin versions of the ones that show it (the package and patch
# keep the names they had under the layered ridge, Origin's predecessor):
#
#   share/rime-greet/              the greeter tree: shell.qml, GreetSurface.qml and README.md with nix/branding/ridge.patch,
#                                  the neutral wordmark (assets/ridge-lockup-base.png) instead of the spark PNGs, and the
#                                  script that renders it (make-logo-assets.sh)
#   share/rime/shell/greeting.sh   the terminal greeting: the symbol through rime-term-graphics, or a "RIME OS" label
#   etc/fastfetch/config.jsonc     fastfetch's config: the label logo and Rime Blue keys
#
# Paths stay in their Fedora spelling here; the consumers (desktop.nix rimeGreet, rime-libexec, greeting.nix) rewrite
# them as they do for every other file from files/**. The patch is applied with no fuzz, so a change to one of the
# shared files that touches a branded line fails this build instead of shipping a half-branded file.
{ lib, runCommand, gnugrep }:

let
  files = ../../files;
  branding = ../branding;
in
runCommand "rime-ridge-files" { nativeBuildInputs = [ gnugrep ]; } ''
  set -eu
  mkdir -p work/desktop/shell work/system/fastfetch
  cp -r ${files + "/desktop/rime-greet"} work/desktop/rime-greet
  cp ${files + "/desktop/shell/rime-greeting.sh"} work/desktop/shell/rime-greeting.sh
  cp ${files + "/system/fastfetch/config.jsonc"} work/system/fastfetch/config.jsonc
  chmod -R u+w work

  patch --batch --forward --fuzz=0 --no-backup-if-mismatch -p1 -d work < ${branding + "/ridge.patch"}
  if find work -name '*.rej' -o -name '*.orig' | grep .; then echo "ridge.patch left rejects" >&2; exit 1; fi

  g=work/desktop/rime-greet
  rm $g/assets/spark-chartreuse.png $g/assets/spark-gold.png $g/assets/spark-white.png
  install -m0644 ${branding + "/rime-greet/assets/ridge-lockup-base.png"} $g/assets/ridge-lockup-base.png
  install -m0755 ${branding + "/rime-greet/make-logo-assets.sh"} $g/make-logo-assets.sh

  # What a user sees: the Origin mark and Rime Blue, and nothing of the spark or the ridge.
  grep -q 'assets/ridge-lockup-base.png' $g/shell.qml
  grep -q '"#91c2dc"' $g/shell.qml
  grep -q 'ShapePath.OddEvenFill' $g/GreetSurface.qml
  if grep -n 'PathLine' $g/GreetSurface.qml; then echo "the NixOS greeter still draws the old ridge's plane 3" >&2; exit 1; fi
  if grep -rn 'spark\|d9f99d' $g/*.qml; then echo "the NixOS greeter still names the spark" >&2; exit 1; fi
  grep -q '/usr/libexec/rime-term-graphics' work/desktop/shell/rime-greeting.sh
  if grep -n 'rime-logo.txt\|--logo-color-1 "''${_rime_logo_color}"' work/desktop/shell/rime-greeting.sh; then
    echo "the NixOS greeting still draws the spark" >&2; exit 1
  fi
  grep -q '"keys": "38;2;145;194;220"' work/system/fastfetch/config.jsonc
  if grep -n 'rime-logo.txt' work/system/fastfetch/config.jsonc; then echo "fastfetch still shows the spark" >&2; exit 1; fi

  mkdir -p $out/share/rime/shell $out/etc/fastfetch
  cp -r $g $out/share/rime-greet
  install -m0644 work/desktop/shell/rime-greeting.sh $out/share/rime/shell/greeting.sh
  install -m0644 work/system/fastfetch/config.jsonc $out/etc/fastfetch/config.jsonc
''
