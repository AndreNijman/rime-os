# rime-bridge: `rime migrate`, the bridge from a bootc Rime OS machine to Rime OS on NixOS (PLAN.md D1-D3).
#
#   bin/rime-migrate                       the program, for the checks here
#   share/rime-bridge/rime-migrate          the same file with `#!/usr/bin/python3 -I`: what runs ON the bootc
#                                          machine (Fedora's python, nothing from /nix), and what the OVMF lab copies
#                                          into its VMs (nix/probe/ovmf/bridge)
#
# One Python file, standard library only. checkPhase runs tests/test_bridge.py: the topology classifier (A-F, E over
# A/B/D), every named refusal, the NVRAM load-option parser and the forbidden-tool scan.
#
# TODO(handover): the bootc side. The readiness release (D1.1) installs share/rime-bridge/rime-migrate as
# /usr/libexec/rime-migrate in the rime-os image and `rime migrate` execs it; that is a bootc PR to main.
{
  lib,
  stdenvNoCC,
  python3,
}:

stdenvNoCC.mkDerivation {
  pname = "rime-bridge";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./rime-bridge.py ./tests ];
  };

  buildInputs = [ python3 ];
  dontBuild = true;
  # share/rime-bridge/rime-migrate must keep #!/usr/bin/python3 (it runs on the bootc machine, which has no /nix);
  # bin/ already names this python explicitly.
  dontPatchShebangs = true;

  installPhase = ''
    runHook preInstall
    install -Dm755 rime-bridge.py $out/share/rime-bridge/rime-migrate
    substituteInPlace $out/share/rime-bridge/rime-migrate --replace-fail '#!/usr/bin/env python3' '#!/usr/bin/python3 -I'
    install -Dm755 rime-bridge.py $out/bin/rime-migrate
    substituteInPlace $out/bin/rime-migrate --replace-fail '#!/usr/bin/env python3' '#!${python3}/bin/python3 -I'
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    head -n1 $out/share/rime-bridge/rime-migrate | grep -qx '#!/usr/bin/python3 -I' \
      || { echo "share/rime-bridge/rime-migrate does not start with #!/usr/bin/python3 -I"; exit 1; }
    # -B: the tests import $out/bin/rime-migrate; without it a __pycache__ lands in $out/bin (non-deterministic).
    RIME_MIGRATE=$out/bin/rime-migrate ${python3}/bin/python3 -I -B -W error::ResourceWarning \
      -m unittest discover -v -s tests -p 'test_*.py'
    if [ -e $out/bin/__pycache__ ]; then echo "bytecode written into \$out/bin"; exit 1; fi
    runHook postInstallCheck
  '';

  meta = {
    description = "Rime OS bridge: rime migrate check, repair-e and start (bootc to NixOS)";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-migrate";
  };
}
