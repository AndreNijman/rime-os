# Attach a detached MOK signature to Rime's kernel image (PLAN.md D3 + D7, decision A8).
#
# The hosted job .github/workflows/nix-kernel-sign.yml signs `system.build.rimeKernelImage` with the Rime MOK
# (`sbsign --detached`, nix/pkgs/kernel/sign-detached.sh) and the resulting PKCS#7 file is committed to
# nix/pkgs/kernel/signing/signatures/<image store hash>.sig (D6: every build input of a release is in rime-os).
# This derivation puts the two together: a copy of the unsigned image with the signature appended to its PE
# certificate table (`sbattach --attach`). The private key never enters the Nix store or a builder: only the
# public certificate and the signature do.
#
# The build FAILS unless the result verifies against the pinned public certificate (`sbverify --cert`), so a
# signature made for a different image (any kernel, module or nixpkgs change makes a new image), by a key other
# than the pinned MOK, or a non-reproducible image whose bytes differ from what was signed can never reach a
# toplevel as a "signed" kernel that shim would then refuse at boot.
{
  lib,
  runCommand,
  sbsigntool,
  openssl,
}:

{
  # A directory (derivation) holding the unsigned image, e.g. system.build.rimeKernelImage.
  image,
  # The image's file name inside `image` (kernel.target: bzImage).
  target,
  # Detached PKCS#7 signature of exactly that file (sbsign --detached).
  signature,
  # The public certificate (PEM or DER) the signature must verify against: the Rime MOK.
  certificate,
  name ? "linux-image-signed",
}:

runCommand name
  {
    nativeBuildInputs = [
      sbsigntool
      openssl
    ];
    passthru = { inherit image target signature certificate; };
  }
  ''
    set -euo pipefail
    img=${image}/${target}

    # The pinned certificate, as PEM (sbverify reads PEM only).
    if openssl x509 -in ${certificate} -noout 2>/dev/null; then
      openssl x509 -in ${certificate} -out cert.pem
    else
      openssl x509 -inform DER -in ${certificate} -out cert.pem
    fi
    fp=$(openssl x509 -in cert.pem -noout -fingerprint -sha256 | cut -d= -f2)
    subj=$(openssl x509 -in cert.pem -noout -subject)

    # The input must carry no signature of its own: sbattach would keep it beside ours, and shim accepts an image
    # if ANY of its signatures verifies.
    if sbverify --list "$img" 2>&1 | grep -E '^signature [0-9]+' >/dev/null; then
      echo "FATAL: $img is already signed; sign.nix takes the unsigned image (system.build.rimeKernelImage)" >&2
      exit 1
    fi

    mkdir -p $out
    cp "$img" $out/${target}
    chmod u+w $out/${target}
    sbattach --attach ${signature} $out/${target}

    if ! sbverify --cert cert.pem $out/${target} 2>&1 | tee verify.log; then
      cat >&2 <<EOF
    FATAL: the detached signature does not verify against the pinned MOK.
      image:       ${image}
      signature:   ${signature}
      certificate: $subj (sha256 $fp)
    Either the signature was made for a different image (a kernel, module or nixpkgs change makes a new
    image: run .github/workflows/nix-kernel-sign.yml for this configuration and commit the new .sig), the
    image built here differs bit for bit from the one that was signed (not reproducible), or it was signed
    by a key other than the pinned certificate.
    EOF
      exit 1
    fi
    grep -x 'Signature verification OK' verify.log >/dev/null

    # Exactly one signature: ours.
    n=$(sbverify --list $out/${target} 2>&1 | grep -cE '^signature [0-9]+' || true)
    if [ "$n" != 1 ]; then
      echo "FATAL: expected exactly one signature on the image, found $n" >&2
      exit 1
    fi
    echo "signed ${target}: verifies against $subj (sha256 $fp)"
  ''
