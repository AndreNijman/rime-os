# rime-bridge: `rime migrate`, the bridge from a bootc Rime OS machine to Rime OS on NixOS (PLAN.md D1-D3).
#
#   bin/rime-migrate                       the program, for the checks here
#   share/rime-bridge/rime-migrate          the same file with `#!/usr/bin/python3 -I`: what runs ON the bootc
#                                          machine (Fedora's python, nothing from /nix), and what the OVMF lab copies
#                                          into its VMs (nix/probe/ovmf/bridge)
#   share/rime-bridge/payload-sign.sh       writes the payload's manifest.json.sig (the release pipeline; the lab)
#   share/rime-bridge/payload-build.sh      builds a payload from a toplevel and its closureInfo (the release; the lab)
#   share/rime-bridge/payload-release.sh    signs a payload, refuses a key other than the trusted one, checks it with
#                                          the bridge's own verify_payload (the release; pkgs.rime.rime-bridge-payload)
#   bin/rime-migrate-finish                 `rime migrate finish`, on the moved NixOS system: removes the old side
#                                          (rime-migrate-finish.py; it loads bin/rime-migrate beside it for the start
#                                          record's reading, so the record is read one way everywhere)
#
# One Python file, standard library only. checkPhase runs tests/test_bridge.py: the topology classifier (A-F, E over
# A/B/D), every named refusal, the NVRAM load-option parser and the forbidden-tool scan.
#
# The bootc side: the readiness release (D1.1, draft rime-os #137) installs rime-bridge.py as /usr/libexec/rime-migrate
# (first line #!/usr/bin/python3 -I) and its `rime migrate` execs it (docs/nixos/bridge.md "Handover").
{
  lib,
  stdenvNoCC,
  python3,
  util-linux,
  btrfs-progs,
  coreutils,
  cryptsetup,
}:

stdenvNoCC.mkDerivation {
  pname = "rime-bridge";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./rime-bridge.py ./rime-migrate-finish.py ./payload-sign.sh ./payload-build.sh ./payload-release.sh ./tests ];
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
    # The release pipeline's signer of the payload manifest (docs/nixos/bridge.md); run with bash, needs openssl.
    install -Dm755 payload-sign.sh $out/share/rime-bridge/payload-sign.sh
    # The payload's builder and the release's sign-and-check (payload.nix wraps both with their tools).
    install -Dm755 payload-build.sh $out/share/rime-bridge/payload-build.sh
    install -Dm755 payload-release.sh $out/share/rime-bridge/payload-release.sh
    install -Dm755 rime-bridge.py $out/bin/rime-migrate
    substituteInPlace $out/bin/rime-migrate --replace-fail '#!/usr/bin/env python3' '#!${python3}/bin/python3 -I'
    install -Dm755 rime-migrate-finish.py $out/bin/rime-migrate-finish
    substituteInPlace $out/bin/rime-migrate-finish \
      --replace-fail '#!/usr/bin/env python3' '#!${python3}/bin/python3 -I' \
      --replace-fail 'TOOLS = ""' 'TOOLS = "${lib.makeBinPath [ util-linux btrfs-progs coreutils cryptsetup ]}"'
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    head -n1 $out/share/rime-bridge/rime-migrate | grep -qx '#!/usr/bin/python3 -I' \
      || { echo "share/rime-bridge/rime-migrate does not start with #!/usr/bin/python3 -I"; exit 1; }
    # -B: the tests import $out/bin/rime-migrate; without it a __pycache__ lands in $out/bin (non-deterministic).
    RIME_MIGRATE=$out/bin/rime-migrate RIME_MIGRATE_FINISH=$out/bin/rime-migrate-finish ${python3}/bin/python3 -I -B -W error::ResourceWarning \
      -m unittest discover -v -s tests -p 'test_*.py'
    if [ -e $out/bin/__pycache__ ]; then echo "bytecode written into \$out/bin"; exit 1; fi
    runHook postInstallCheck
  '';

  meta = {
    description = "Rime OS bridge: rime migrate check, repair-e and start (bootc to NixOS), and finish (on NixOS)";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-migrate";
  };
}
