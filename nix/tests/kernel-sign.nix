# Kernel image MOK signing (PLAN.md D3 + D7; nix/pkgs/kernel/sign.nix, sign-detached.sh, nix/modules/kernel.nix):
#
#   kernel-sign           on the REAL Rime kernel image (rimeKernelImage of a rime.kernel system), with throwaway
#                         keys generated deterministically inside this check (never committed, never the Rime MOK):
#                         - sign-detached.sh (the code nix-kernel-sign.yml runs) makes the detached signature, and
#                           refuses a certificate that is not the pinned one and an image that is already signed;
#                         - the module boots sign.nix's output: the image with that signature attached, verifying
#                           against the pinned test certificate and not against another key; stripping it leaves
#                           an image with no signature; the toplevel's `kernel` link is that image;
#                         - sign.nix FAILS the build for a signature by another key, a signature of a different
#                           image, and an input image that is already signed (pkgs.testers.testBuildFailure);
#                         - with no signature the system boots the unsigned image, and rime.kernel.signing.require
#                           refuses to evaluate it.
#   kernel-sign-workflow  .github/workflows/nix-kernel-sign.yml keeps its shape: GitHub-hosted runners only (never
#                         self-hosted/katana), no PR triggers, read-only token, actions pinned by SHA, the APEX_SB_*
#                         secret names unchanged and only in the signing step's env, behind an environment, the key
#                         shredded in an always() step, the pinned certificate passed to sign-detached.sh, and a
#                         Nix build sandbox that works on ubuntu-24.04 (userns relaxed, sandbox-fallback = false).
{ pkgs, self }:

let
  inherit (pkgs) lib;
  signImage = pkgs.callPackage ../pkgs/kernel/sign.nix { };
  signDetached = ../pkgs/kernel/sign-detached.sh;
  tools = [
    pkgs.sbsigntool
    pkgs.openssl
    pkgs.coreutils
    pkgs.gnugrep
    pkgs.gnused
  ];

  # A throwaway RSA-2048 key + self-signed certificate, deterministic (certtool --provable from a fixed seed, fixed
  # serial and validity), so the check is reproducible. TEST ONLY: lives in this check's store paths, nowhere else.
  testKey =
    name: seed:
    pkgs.runCommand "kernel-sign-test-key-${name}"
      {
        nativeBuildInputs = [
          pkgs.gnutls.bin
          pkgs.openssl
        ];
      }
      ''
        mkdir $out
        certtool --generate-privkey --key-type rsa --bits 2048 --provable --seed=${seed} --outfile key.txt
        openssl pkey -in key.txt -out $out/key.pem
        openssl req -new -x509 -key $out/key.pem -sha256 -set_serial 1 \
          -not_before 20260101000000Z -not_after 20360101000000Z \
          -subj "/CN=Rime kernel-sign check TEST key ${name} (throwaway, DO NOT TRUST)/" -out $out/cert.pem
        openssl x509 -in $out/cert.pem -outform DER -out $out/cert.der
      '';
  keyA = testKey "a" "00000000000000000000000000000000000000000000000000000a11";
  keyB = testKey "b" "00000000000000000000000000000000000000000000000000000b22";

  # A Rime-kernel system; only its kernel image, signing info and toplevel wiring are used.
  mkSystem =
    signing:
    (self.inputs.nixpkgs.lib.nixosSystem {
      modules = [
        self.nixosModules.default
        {
          nixpkgs.hostPlatform = "x86_64-linux";
          rime.kernel.enable = true;
          rime.kernel.signing = signing;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          system.stateVersion = "26.05";
        }
      ];
    }).config;

  unsignedSys = mkSystem { certificate = "${keyA}/cert.pem"; };
  info = unsignedSys.system.build.rimeKernelSigning;
  image = unsignedSys.system.build.rimeKernelImage;
  target = info.target;

  # The detached signature, made the way nix-kernel-sign.yml makes it (sign-detached.sh), with key A.
  detached =
    {
      name,
      key,
      pinned,
      img ? "${image}/${target}",
    }:
    pkgs.runCommand "kernel-sign-detached-${name}" { nativeBuildInputs = tools; } ''
      bash ${signDetached} --image ${img} --hash ${info.imageHash} --key ${key}/key.pem --cert ${key}/cert.pem \
        --pinned ${pinned} --out $out --store-path ${image} --note "checks.kernel-sign ${name}"
    '';
  sigA = detached {
    name = "a";
    key = keyA;
    pinned = "${keyA}/cert.der";
  };
  sigB = detached {
    name = "b";
    key = keyB;
    pinned = "${keyB}/cert.pem";
  };

  signedSys = mkSystem {
    certificate = "${keyA}/cert.pem";
    imageSignature = "${sigA}/${info.imageHash}.sig";
  };
  signedImage = signedSys.system.build.rimeKernelBootImage;

  # Negative builds: each must FAIL.
  tamperedImage = pkgs.runCommand "kernel-sign-tampered-image" { } ''
    mkdir $out
    cp ${image}/${target} $out/${target}
    chmod u+w $out/${target}
    off=$(( $(stat -c %s $out/${target}) / 2 ))
    printf '\x5a' | dd of=$out/${target} bs=1 seek=$off conv=notrunc status=none
  '';
  sigOfTampered = pkgs.runCommand "kernel-sign-sig-of-other-image" { nativeBuildInputs = tools; } ''
    mkdir $out
    sbsign --key ${keyA}/key.pem --cert ${keyA}/cert.pem --detached --output $out/sig ${tamperedImage}/${target}
  '';
  mustFail = {
    wrong-key = signImage {
      name = "mutant-wrong-key";
      inherit image target;
      signature = "${sigB}/${info.imageHash}.sig";
      certificate = "${keyA}/cert.pem";
    };
    other-image = signImage {
      name = "mutant-other-image";
      inherit image target;
      signature = "${sigOfTampered}/sig";
      certificate = "${keyA}/cert.pem";
    };
    already-signed = signImage {
      name = "mutant-already-signed";
      image = signedImage;
      inherit target;
      signature = "${sigA}/${info.imageHash}.sig";
      certificate = "${keyA}/cert.pem";
    };
    # sign-detached.sh: a key whose certificate is not the pinned one (e.g. a rotated secret, cert not committed).
    unpinned-cert = detached {
      name = "unpinned";
      key = keyB;
      pinned = "${keyA}/cert.pem";
    };
    # sign-detached.sh: an image that already carries a signature.
    resign-signed = detached {
      name = "resign";
      key = keyA;
      pinned = "${keyA}/cert.pem";
      img = "${signedImage}/${target}";
    };
  };
  failures = lib.mapAttrs (_: pkgs.testers.testBuildFailure) mustFail;
  expectedLog = {
    wrong-key = "does not verify against the pinned MOK";
    other-image = "Image fails hash check";
    already-signed = "is already signed";
    unpinned-cert = "is not the pinned one";
    resign-signed = "already carries a signature";
  };

  requireUnsigned = builtins.tryEval (mkSystem { require = true; }).system.build.toplevel.drvPath;
  requireSigned = builtins.tryEval (mkSystem {
    require = true;
    certificate = "${keyA}/cert.pem";
    imageSignature = "${sigA}/${info.imageHash}.sig";
  }).system.build.toplevel.drvPath;
in

# Evaluation-level facts, checked before anything builds.
assert !info.signed;
assert unsignedSys.system.build.rimeKernelBootImage == image;
assert signedSys.system.build.rimeKernelSigning.signed;
assert signedSys.system.build.rimeKernelSigning.imageHash == info.imageHash;
assert lib.hasInfix (builtins.unsafeDiscardStringContext "${signedImage}/${target}") (
  builtins.unsafeDiscardStringContext signedSys.system.systemBuilderCommands
);
assert !requireUnsigned.success;
assert requireSigned.success;
# The default certificate is the committed Rime MOK, and no signature is committed for this test image.
assert (mkSystem { }).rime.kernel.signing.certificate == ../pkgs/kernel/signing/rime-mok.pem;
assert !(mkSystem { }).system.build.rimeKernelSigning.signed;
{
  kernel-sign = pkgs.runCommand "rime-kernel-sign" { nativeBuildInputs = tools; } ''
    set -euo pipefail
    img=${signedImage}/${target}
    echo "image ${image} (hash ${info.imageHash}, ${info.modDirVersion})"
    cat ${sigA}/${info.imageHash}.json

    echo "--- the committed Rime MOK certificate is the one bootc ships (rime-mok.der), byte for byte"
    # Reading the file here makes a checkout without it (e.g. swallowed by .gitignore's *.pem) fail evaluation.
    test "$(openssl x509 -in ${../pkgs/kernel/signing/rime-mok.pem} -outform DER | sha256sum | cut -d' ' -f1)" \
      = 2fdd2f55e18d6d56ca97094bb9ffa116d9ad962c8cb5591484ee1e2a6958bcbb
    if openssl pkey -in ${../pkgs/kernel/signing/rime-mok.pem} -noout 2>/dev/null; then
      echo "FAIL: rime-mok.pem holds a private key"; exit 1
    fi

    echo "--- the booted image verifies against the pinned (test) certificate"
    sbverify --cert ${keyA}/cert.pem "$img"
    echo "--- and not against another key"
    if sbverify --cert ${keyB}/cert.pem "$img"; then echo "FAIL: verified against key B"; exit 1; fi
    echo "--- exactly one signature"
    sbverify --list "$img" | tee list
    test "$(grep -cE '^signature [0-9]+' list)" = 1
    echo "--- the unsigned image carries none"
    if sbverify --list ${image}/${target} 2>&1 | grep -E '^signature [0-9]+' >/dev/null; then echo "FAIL: unsigned image is signed"; exit 1; fi
    echo "--- stripping the signature leaves an image that fails verification"
    cp "$img" stripped; chmod u+w stripped; sbattach --remove stripped
    if sbverify --cert ${keyA}/cert.pem stripped; then echo "FAIL: stripped image still verifies"; exit 1; fi

    ${lib.concatStrings (
      lib.mapAttrsToList (n: f: ''
        echo "--- must fail: ${n}"
        test "$(cat ${f}/testBuildFailure.exit)" != 0
        grep -F ${lib.escapeShellArg expectedLog.${n}} ${f}/testBuildFailure.log
      '') failures
    )}
    echo "PASS: kernel image signing (sign-detached.sh -> sign.nix -> toplevel), 5 refusals" | tee $out
  '';

  kernel-sign-workflow =
    pkgs.runCommand "rime-kernel-sign-workflow"
      {
        nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) ];
      }
      ''
        python3 -I ${./kernel-sign-workflow.py} ${../../.github/workflows/nix-kernel-sign.yml} | tee $out
      '';
}
