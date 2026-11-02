# The vendor-key check of D5 (PLAN.md 4.5): ChatGPT and Claude Desktop come from their vendors, not from nixpkgs, and
# only through the same check rime-pkg makes on bootc (files/system/libexec/rime-pkg, vendor_fetch_chatgpt and
# vendor_fetch_claude_desktop). That code is NOT re-written here: `verifier` is rime-pkg's own block, cut out of the
# script by its markers, so one check exists and both systems run it.
#
#   verifier                  bin/rime-vendor-verify NAME REPO OUT: runs vendor_fetch NAME against REPO (a directory
#                             laid out as the vendor's repository) and copies the verified file to OUT. Any refusal is
#                             rime-pkg's own message and a non-zero exit.
#   fetchVerified { name, files }
#                             a derivation whose output is the verified vendor file. `files` maps repository paths
#                             (e.g. "dists/stable/InRelease") to fixed-output fetches; the build lays them out as a
#                             file:// repository and runs the verifier on it, so a download that does not verify
#                             against the pinned key fails the build.
#
# The fingerprints are rime-pkg's (CHATGPT_KEY_FPR, CLAUDE_KEY_FPR): `readonly`, with no override. checks.vendor-verify
# rewrites them in a COPY of the block to throwaway keys, exactly as tests/test-rime-pkg-rpm6.sh does.
#
# The pins and the packaged apps: nix/pkgs/rime-vendor-apps (pins.nix: one version of each, its bytes by sha256 and
# the vendor's signed index kept beside it; sources.nix: fetchVerified of exactly those; chatgpt.nix, claude-desktop.nix:
# the apps the catalogue installs). checks.vendor-apps installs both from a release record, offline.
{
  lib,
  runCommand,
  writeShellApplication,
  bash,
  coreutils,
  curl,
  gnupg,
  rpm,
  cpio,
  gzip,
  gnused,
  gnugrep,
  gawk,
  binutils,
  gnutar,
  xz,
  zstd,
}:

let
  rimePkg = ../../files/system/libexec/rime-pkg;

  # rime-pkg's vendor block: from the pins to the end of vendor_fetch_claude_desktop.
  block = runCommand "rime-vendor-block.sh" { } ''
    awk '/^readonly CHATGPT_KEY_FPR=/{on=1} /^# vendor_refresh/{on=0} on' ${rimePkg} > $out
    for f in 'readonly CHATGPT_KEY_FPR=' 'readonly CLAUDE_KEY_FPR=' 'vendor_fetch_chatgpt() {' 'vendor_fetch_claude_desktop() {' 'rpmkeys --define' 'gpgv --keyring'; do
      grep -F -- "$f" $out > /dev/null || { echo "rime-pkg's vendor block lost: $f" >&2; exit 1; }
    done
  '';

  mkVerifier =
    vendorBlock:
    writeShellApplication {
      name = "rime-vendor-verify";
      runtimeInputs = [ coreutils curl gnupg rpm cpio gzip gnused gnugrep gawk binutils gnutar xz zstd ];
      text = ''
        # rime-vendor-verify NAME REPO OUT
        name=$1 repo=$2 out=$3
        msg() { printf 'rime-pkg: %s\n' "$*" >&2; }
        die() { printf 'rime-pkg: error: %s\n' "$*" >&2; exit 1; }
        export RIME_PKG_CHATGPT_REPO="file://$repo" RIME_PKG_CLAUDE_REPO="file://$repo"
        # shellcheck disable=SC1091
        source ${vendorBlock}
        work=$(mktemp -d)
        trap 'rm -rf -- "$work"' EXIT
        got=$(vendor_fetch "$name" "$work")
        install -D -m 0444 "$got" "$out"
        echo "verified: $name -> $out (sha256 $(sha256sum "$out" | cut -d' ' -f1))"
      '';
    };

  verifier = mkVerifier block;
in
{
  inherit block verifier mkVerifier;

  fetchVerified =
    {
      name,
      files,
      verify ? verifier,
    }:
    runCommand "rime-vendor-${name}" { } ''
      mkdir repo
      ${lib.concatStringsSep "\n" (
        lib.mapAttrsToList (p: src: ''install -D -m 0644 ${src} "repo/${p}"'') files
      )}
      ${verify}/bin/rime-vendor-verify ${lib.escapeShellArg name} "$PWD/repo" "$out/${name}.${
        if name == "chatgpt" then "rpm" else "deb"
      }"
    '';
}
