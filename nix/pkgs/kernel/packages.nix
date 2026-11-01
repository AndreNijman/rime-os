# linuxPackages for Rime's kernel: nixpkgs' linuxPackagesFor, plus Rime's out-of-tree module pins.
#
#   nvidia   nixpkgs nvidiaPackages.new_feature = 615.78.08 at the flake's nixpkgs pin, the same driver bootc
#            ships (RPMFusion akmod-nvidia 615.78.08). Open kernel modules (Turing+), see nix/modules/kernel.nix.
#   msi-ec   BeardOverflow commit d7fbbd88 by the tarball sha256 bootc verified (Containerfile.core
#            MSI_EC_COMMIT/MSI_EC_SHA256). nixpkgs carries 050d4394, two commits older. Installed under
#            updates/ so it wins over the in-tree msi-ec (battery hook only, no Cooler Boost/fan mode).
#   xone, xpadneo  nixpkgs' (xone 0.5.8 dlundqvist, xpadneo 0.10.5).
#
# Used two ways: `rimeKernelPackagesFor kernel` for checks, and as a kernelPackages extension in the NixOS module
# (where boot.kernelPackages has already applied NixOS's own kernel.override).
{ lib, fetchurl }:

let
  msiEcCommit = "d7fbbd88e6831e56801b860e46475cbf8ddbc7c1";
  msiEcSha256 = "ef3396a4c870f74a3c21b6623fa6e31da1569c88d92171a7b987f8bcb3c18279";
  nvidiaVersion = "615.78.08";

  extension = self: super: {
    msi-ec = super.msi-ec.overrideAttrs (old: {
      version = "0-unstable-rime-${lib.substring 0 8 msiEcCommit}";
      src = fetchurl {
        url = "https://github.com/BeardOverflow/msi-ec/archive/${msiEcCommit}.tar.gz";
        sha256 = msiEcSha256;
        name = "msi-ec-${msiEcCommit}.tar.gz";
      };
      makeFlags = (old.makeFlags or [ ]) ++ [ "INSTALL_MOD_DIR=updates" ];
      # d7fbbd88's Makefile starts `_SRC := $(if $(src),$(src),.)` (for akmod). stdenv exports $src = the
      # tarball, so make would include <tarball>/Makefile.vars. Unset it for the build and install phases.
      preBuild = (old.preBuild or "") + ''
        unset src
      '';
    });

    # The driver Rime pins, by version: a nixpkgs bump that moves new_feature off 615.78.08 fails evaluation
    # here instead of silently changing the NVIDIA driver.
    rimeNvidia =
      let
        p = self.nvidiaPackages.new_feature;
      in
      assert lib.assertMsg (p.version == nvidiaVersion)
        "Rime pins NVIDIA ${nvidiaVersion}; this nixpkgs has new_feature = ${p.version}. Pin it with nvidiaPackages.mkDriver or move the pin deliberately.";
      p;
  };
in
{
  inherit extension nvidiaVersion msiEcCommit;
}
