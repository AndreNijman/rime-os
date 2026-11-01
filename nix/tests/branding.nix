# The layered-ridge assets (files/branding/ridge). Not a VM test: a build-time check.
#  1. make-ridge-assets.sh, run with the flake's resvg and python3, reproduces every committed export (PNGs compared
#     by pixels, everything else by bytes), so a master edited without regenerating fails here.
#  2. check-ridge-assets.py holds the brand rules: four planes in the masters, only plane 3 takes an accent, the
#     adaptive templates carry {{accent}} once and {{fg}} on every other ink, transparent exports, opaque tiles,
#     the full size ladder, a 16/32/48 .ico.
#  3. pkgs.rime.rime-branding installs the app icon and its aliases into hicolor.
{ pkgs, self }:

let
  ridge = ../../files/branding/ridge;
  branding = pkgs.rime.rime-branding;
  python = pkgs.python3.withPackages (p: [ p.pillow ]);
in
pkgs.runCommand "rime-branding-check"
  {
    nativeBuildInputs = [ pkgs.resvg python pkgs.shellcheck ];
  }
  ''
    cp -r ${ridge} ridge
    chmod -R u+w ridge
    sh ridge/make-ridge-assets.sh "$PWD/fresh"
    python3 ridge/check-ridge-assets.py ${ridge} "$PWD/fresh"
    shellcheck ridge/make-ridge-assets.sh

    h=${branding}/share/icons/hicolor
    for n in 16 24 32 48 64 128 256 512; do
      for i in rime-os distributor-logo start-here rime-os-logo; do
        test -s "$h/''${n}x$n/apps/$i.png" || { echo "FAIL: $h/''${n}x$n/apps/$i.png missing"; exit 1; }
      done
    done
    for f in scalable/apps/rime-os.svg scalable/apps/start-here.svg symbolic/apps/rime-os-symbolic.svg; do
      test -s "$h/$f" || { echo "FAIL: $h/$f missing"; exit 1; }
    done
    for d in src svg templates png icons favicon accents; do
      test -d ${branding}/share/rime/branding/$d || { echo "FAIL: share/rime/branding/$d missing"; exit 1; }
    done
    echo "rime-branding: hicolor icons and share/rime/branding installed"
    touch $out
  ''
