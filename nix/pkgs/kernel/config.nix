# Generates Rime's kernel .config from the pinned CachyOS config, the way the bootc spec's %prep did, plus the
# few options NixOS needs. Output: one file, the complete .config after `make olddefconfig`.
#
# The kernel itself is built from the COMMITTED copy of this output (./config), not from this derivation, so the
# config is reviewable in a diff and NixOS can read it at eval time without import-from-derivation. The
# `kernel-config-fresh` check rebuilds this and fails if ./config is stale. To regenerate after a pin bump:
#   nix build path:.#checks.x86_64-linux.kernel-config-fresh   (fails, prints the diff)
#   nix build path:.#rime-kernel-config && cp result nix/pkgs/kernel/config
#
# NOT used: nixpkgs' lib.kernel common-config / generate-config.pl. linuxManualConfig takes the config as is, so
# nothing in nixpkgs silently changes a Rime option; every change from upstream is in `edits` below.
{
  lib,
  stdenv,
  buildPackages,
  path,
  bison,
  flex,
  perl,
  bc,
  openssl,
  elfutils,
  zstd,
  python3Minimal,
  kmod,
  hexdump,
  cpio,
  pahole,
  gmp,
  libmpc,
  mpfr,
  zlib,
  fetchurl,
}:

let
  sources = import ./sources.nix { inherit lib fetchurl; };
  inherit (sources) pin;

  commonMakeFlags = import "${path}/pkgs/os-specific/linux/kernel/common-flags.nix" {
    inherit lib stdenv buildPackages;
  };

  # scripts/config arguments, applied in order to the upstream config before olddefconfig.
  edits = [
    # ── bootc parity: kernel/kernel-cachyos.spec %prep, unchanged ──
    "-e CACHY -e SCHED_BORE"
    "-u DEFAULT_HOSTNAME"
    "-d HZ_300 -e HZ_1000 --set-val HZ 1000"
    "--set-val X86_64_VERSION 3"
    "-e IMA_SECURE_AND_OR_TRUSTED_BOOT -e IMA -e IMA_APPRAISE_BOOTPARAM -e IMA_APPRAISE -e IMA_ARCH_POLICY"
    # The spec's LSM list minus selinux: decision A2 drops SELinux on the NixOS side (no policy ships).
    "--set-str LSM lockdown,yama,integrity,bpf,landlock"
    # Explicit: the Fedora build root had no rustc, so bootc's kernel ended with RUST unset; same here.
    "-d RUST"
    # ── NixOS ──
    # kmodigest (nixpkgs common-flags.nix cmd_sign) signs every module with a deterministic per-module
    # certificate. An empty key stops the kernel generating its own ephemeral signing key (bootc's
    # certs/signing_key.pem), which is what made module signatures unreproducible. The extra-certificate slot
    # is what `linuxPackages.rebuildImage` fills with modules.cer (PLAN D7). Same values as common-config.nix.
    "--set-str MODULE_SIG_KEY \"\""
    "-e SYSTEM_EXTRA_CERTIFICATE --set-val SYSTEM_EXTRA_CERTIFICATE_SIZE 3120000"
    "-e CRYPTO_ECDSA -e CRYPTO_MLDSA"
    # Rime's own release string (see sources.nix).
    "--set-str LOCALVERSION ${sources.localVersion}"
  ];
in
stdenv.mkDerivation {
  pname = "rime-kernel-config";
  version = pin.version;

  inherit (sources) src;
  patches = [ sources.borePatch ];

  depsBuildBuild = [ buildPackages.stdenv.cc ];
  nativeBuildInputs = [
    bison
    flex
    perl
    bc
    openssl
    elfutils
    zstd
    python3Minimal
    kmod
    hexdump
    cpio
    pahole
    gmp
    libmpc
    mpfr
    zlib
  ];

  __structuredAttrs = true;
  makeFlags = commonMakeFlags;

  postPatch = ''
    patchShebangs scripts
  '';

  configurePhase = ''
    runHook preConfigure
    cp ${sources.upstreamConfig} .config
    chmod u+w .config
    ${lib.concatMapStringsSep "\n" (e: "scripts/config --file .config ${e}") edits}
    make "''${makeFlags[@]}" olddefconfig
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    got="$(sed -n 's/^CONFIG_PAHOLE_VERSION=//p' .config)"
    if [ "$got" != "${pin.paholeVersionExpected}" ]; then
      echo "FATAL: CONFIG_PAHOLE_VERSION=$got, kernel.pin requires ${pin.paholeVersionExpected} (pahole ${pahole.version} in this nixpkgs)"
      exit 1
    fi
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -m 0644 .config $out
    runHook postInstall
  '';

  passthru = { inherit edits sources; };
}
