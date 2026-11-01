# Rime's helper scripts (bootc: /usr/libexec/rime-*). On NixOS they are store paths.
# Skeleton: copies files/system/libexec as is. TODO(libexec worker): patch shebangs and FHS paths,
# wrap each script with its runtime PATH, and decide which bootc-only scripts are not shipped.
{ lib, stdenvNoCC }:

let
  root = ../..;
in
stdenvNoCC.mkDerivation {
  pname = "rime-libexec";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = root + "/files/system/libexec";
  };

  dontBuild = true;
  dontPatchShebangs = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/libexec
    cp -r files/system/libexec/. $out/libexec/
    runHook postInstall
  '';

  meta = {
    description = "Rime helper scripts";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
