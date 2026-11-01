# The terminal greeting's two helpers (files/desktop/shell), reached on the profile as
# /run/current-system/sw/libexec/<name> (base.nix links /libexec; greeting.sh names /usr/libexec/<name>, which
# rime-libexec's FHS table rewrites):
#
#   rime-term-graphics   C: which image protocol the terminal speaks (kitty-direct|kitty|sixel|iterm|none) and the
#                        logo's width in cells. C because it runs before every terminal's first prompt.
#   rime-logo-render     sh: Rime Shell's matugen post_hook; recolours the adaptive symbol from rime-branding and
#                        renders ~/.cache/rime/logo/rime-symbol-{dark,light}.png with resvg.
#
# resvg (not librsvg's rsvg-convert): both were already in the rime-vm closure, resvg is the one the branding
# exports are rendered with, so the recoloured logo is drawn by the same renderer as the shipped one.
{ lib, stdenv, resvg, coreutils, gnused, gnugrep, rime-branding }:

stdenv.mkDerivation {
  pname = "rime-greeting";
  version = "1";

  src = lib.fileset.toSource {
    root = ../../files/desktop/shell;
    fileset = lib.fileset.unions [
      ../../files/desktop/shell/rime-term-graphics.c
      ../../files/desktop/shell/rime-logo-render
    ];
  };

  buildPhase = ''
    runHook preBuild
    $CC -std=c11 -O2 -Wall -Wextra -Werror -Wconversion -o rime-term-graphics rime-term-graphics.c
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm755 rime-term-graphics $out/libexec/rime-term-graphics
    install -Dm755 rime-logo-render $out/libexec/rime-logo-render
    substituteInPlace $out/libexec/rime-logo-render \
      --replace-fail 'template=''${2:-/usr/share/rime/branding/templates/rime-symbol.svg.in}' \
                     'template=''${2:-${rime-branding}/share/rime/branding/templates/rime-symbol.svg.in}' \
      --replace-fail 'resvg=resvg' 'resvg=${lib.getExe resvg}' \
      --replace-fail 'set -eu' 'set -eu
    PATH=${lib.makeBinPath [ coreutils gnused gnugrep ]}:$PATH'
    runHook postInstall
  '';

  # rime-logo-render runs from matugen's post_hook with whatever PATH the session has; its tools are pinned.
  meta = {
    description = "Rime OS terminal greeting helpers: image protocol detection and the recoloured logo";
    platforms = lib.platforms.linux;
    mainProgram = "rime-term-graphics";
  };
}
