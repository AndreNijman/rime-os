# The layered-ridge boot splash: rime-os (the default, plane 3 in Rime Blue) and rime-os-accent-00..23 (plane 3 in
# the owner's accent bucket), generated at build time from rime-branding's adaptive lockup template
# (files/branding/plymouth/ridge/make-ridge-themes.py). Nothing raster is committed for these.
#
#   $out/share/plymouth/themes/rime-os/                 lockup-base, lockup-accent, dots, bullet, prompt, script
#   $out/share/plymouth/themes/rime-os-accent-NN/       the accent images + script; neutral images link to ../rime-os
#
# Theme files name their images under /etc/plymouth/themes/<theme>: NixOS links that path to the theme set both in
# the initrd (boot.initrd.systemd.contents, nix/modules/branding.nix) and on the running system (plymouth module).
{ lib, stdenvNoCC, resvg, python3, dejavu_fonts, rime-branding }:

stdenvNoCC.mkDerivation {
  pname = "rime-plymouth";
  version = "1";

  src = lib.fileset.toSource {
    root = ../../files/branding/plymouth/ridge;
    fileset = lib.fileset.unions [
      ../../files/branding/plymouth/ridge/make-ridge-themes.py
      ../../files/branding/plymouth/ridge/rime-os.script
    ];
  };

  nativeBuildInputs = [ resvg python3 ];
  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    b=${rime-branding}/share/rime/branding
    python3 make-ridge-themes.py $b/templates $b/accents/accents.json rime-os.script \
      ${dejavu_fonts.minimal}/share/fonts/truetype/DejaVuSans.ttf themes
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/share/plymouth
    cp -a themes $out/share/plymouth/themes
    test $(ls -d $out/share/plymouth/themes/rime-os-accent-* | wc -l) -eq 24
    runHook postInstall
  '';

  passthru.defaultTheme = "rime-os";

  meta = {
    description = "Rime OS boot splash (layered ridge) and its 24 per-owner accent themes";
    platforms = lib.platforms.linux;
  };
}
