# rime-boot: the boot.loader.external hook of PLAN.md D3's v1 chain (Fedora shim -> Fedora GRUB -> MOK kernel).
# The program is nix/pkgs/rime-boot/rime-boot.py; nix/modules/boot.nix wires it and nix/tests/boot.nix tests it.
{ lib, stdenvNoCC, python3 }:

stdenvNoCC.mkDerivation {
  pname = "rime-boot";
  version = "1";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./rime-boot.py ./README.md ];
  };
  buildInputs = [ python3 ];
  dontBuild = true;
  installPhase = ''
    runHook preInstall
    install -Dm755 rime-boot.py $out/bin/rime-boot
    install -Dm644 README.md $out/share/doc/rime-boot/README.md
    patchShebangs $out/bin/rime-boot
    runHook postInstall
  '';
  doInstallCheck = true;
  installCheckPhase = ''
    $out/bin/rime-boot --help >/dev/null
    # D3 gate: the hook never runs a loader installer or writes NVRAM. It runs no program at all: no subprocess,
    # exec, spawn, popen or system call anywhere in it.
    if grep -nE 'subprocess|os\.(system|exec|spawn|popen|posix_spawn)|pty\.spawn' $out/bin/rime-boot; then
      echo "rime-boot must not run other programs (D3: no bootctl, grub-install or efibootmgr)" >&2; exit 1
    fi
  '';
  meta = {
    description = "Rime's boot.loader.external hook: BLS or custom.cfg GRUB menus for Fedora's shim + GRUB";
    mainProgram = "rime-boot";
    platforms = lib.platforms.linux;
  };
}
