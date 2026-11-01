# Rime Shell: the Quickshell/QML desktop shell, from the rime-shell flake input (flake = false).
# Skeleton: installs the tree and a launcher. The desktop worker owns runtime deps, wrapping and fonts.
{ lib, stdenvNoCC, makeWrapper, quickshell, src }:

stdenvNoCC.mkDerivation {
  pname = "rime-shell";
  version = "0-unstable";
  inherit src;

  nativeBuildInputs = [ makeWrapper ];

  dontBuild = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/share/rime-shell $out/bin
    cp -r . $out/share/rime-shell/
    rm -f $out/share/rime-shell/flake.nix $out/share/rime-shell/flake.lock
    makeWrapper ${lib.getExe quickshell} $out/bin/rime-shell \
      --add-flags "-c $out/share/rime-shell"
    runHook postInstall
  '';

  meta = {
    description = "Rime's Quickshell/QML desktop shell";
    homepage = "https://github.com/AndreNijman/rime-shell";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-shell";
  };
}
