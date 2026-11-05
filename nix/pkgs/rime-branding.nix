# The layered-ridge identity (files/branding/ridge): masters, every export, and the app icon in hicolor.
#
#   $out/share/rime/branding/{src,svg,templates,png,icons,favicon,accents}   as committed (docs/branding.md)
#   $out/share/icons/hicolor/<N>x<N>/apps/rime-os.png                        the dark app-icon tile, 16..512
#   $out/share/icons/hicolor/scalable/apps/rime-os.svg                        the same tile, vector
#   $out/share/icons/hicolor/symbolic/apps/rime-os-symbolic.svg               the bare symbol, one ink (GTK recolours it)
#   distributor-logo, start-here and rime-os-logo                             aliases of rime-os at every size
#
# The exports are generated in the source tree and committed; checks.x86_64-linux.branding proves they match the
# masters. This package only copies.
{ lib, stdenvNoCC }:

stdenvNoCC.mkDerivation {
  pname = "rime-branding";
  version = "1";

  src = lib.fileset.toSource {
    root = ../../files/branding/ridge;
    fileset = lib.fileset.unions (map (d: ../../files/branding/ridge + "/${d}") [
      "src" "svg" "templates" "png" "icons" "favicon" "accents"
    ]);
  };

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    d=$out/share/rime/branding
    mkdir -p $d
    cp -r src svg templates png icons favicon accents $d/

    h=$out/share/icons/hicolor
    for n in 16 24 32 48 64 128 256 512; do
      install -Dm644 icons/app/rime-app-icon-dark-$n.png $h/''${n}x$n/apps/rime-os.png
    done
    install -Dm644 svg/rime-app-icon-dark.svg $h/scalable/apps/rime-os.svg
    install -Dm644 svg/square/rime-symbol-square-mono-black.svg $h/symbolic/apps/rime-os-symbolic.svg
    for dir in $h/*/apps; do
      for f in $dir/rime-os.*; do
        [ -e "$f" ] || continue
        ext=''${f##*.}
        for alias in distributor-logo start-here rime-os-logo; do
          ln -s rime-os.$ext $dir/$alias.$ext
        done
      done
    done
    runHook postInstall
  '';

  meta = {
    description = "Rime OS layered-ridge logo, lockups, app icons, favicons and accent exports";
    platforms = lib.platforms.all;
  };
}
