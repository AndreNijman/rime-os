# Committed kernel image signatures

One detached MOK signature per kernel image, made by `.github/workflows/nix-kernel-sign.yml`:

- `<hash>.sig`: PKCS#7 (DER) from `sbsign --detached` over `system.build.rimeKernelImage`. `<hash>` is that
  image's 32-character store hash (`nix eval --json …config.system.build.rimeKernelSigning`).
- `<hash>.json`: provenance (image store path and sha256, certificate fingerprint, time, workflow run).

`rime.kernel.signing.imageSignature` picks up `<hash>.sig` by itself; `nix/pkgs/kernel/sign.nix` attaches it and
fails the build unless it verifies against `../rime-mok.pem`. A signature is committed here, not fed to the build
as a workflow artifact, so that the rime-os commit (with the rime-shell commit) still identifies a release (D6).

No private key ever lives in this repository or the Nix store. Nothing here is secret.
