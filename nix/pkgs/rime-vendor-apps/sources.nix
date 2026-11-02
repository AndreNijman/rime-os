# The vendor apps' downloads, verified: each output is the one package file that passed rime-pkg's own vendor-key
# check (nix/pkgs/rime-vendor.nix fetchVerified) against the pins in ./pins.nix. A fetch whose bytes differ fails its
# fixed-output hash; bytes that do not verify against the pinned key fail the verifier; either way nothing is built.
{
  lib,
  fetchurl,
  runCommand,
  gzip,
  gnugrep,
  rime-vendor,
}:

let
  pins = import ./pins.nix;
  c = pins.chatgpt;
  d = pins.claude-desktop;

  # OpenAI's index is not signed: before the verifier reads it, check the kept copy is self-consistent and names the
  # pinned file with the pinned sha256 (the verifier takes the first rpm the index names).
  chatgptIndex = runCommand "rime-chatgpt-index-${c.version}" { nativeBuildInputs = [ gzip gnugrep ]; } ''
    set -eu
    want=$(grep -A1 '<data type="primary">' ${c.repomd} | grep -oE '[0-9a-f]{64}' | head -n1)
    got=$(sha256sum ${c.primaryFile} | cut -d' ' -f1)
    [ "$want" = "$got" ] || { echo "the kept primary index is not the one repomd.xml names ($got != $want)" >&2; exit 1; }
    grep -F 'href="${c.primary}"' ${c.repomd} > /dev/null || { echo "repomd.xml does not name ${c.primary}" >&2; exit 1; }
    gzip -dc ${c.primaryFile} > primary.xml
    first=$(grep -oE '<location href="[^"]+\.rpm"' primary.xml | head -n1)
    [ "$first" = '<location href="${c.file}"' ] || { echo "the index's first rpm is $first, not ${c.file}" >&2; exit 1; }
    grep -F '<checksum type="sha256" pkgid="YES">${c.sha256}</checksum>' primary.xml > /dev/null \
      || { echo "the index does not give ${c.file} the pinned sha256" >&2; exit 1; }
    grep -F 'ver="${c.version}"' primary.xml > /dev/null || { echo "the index does not name version ${c.version}" >&2; exit 1; }
    mkdir -p $out/repodata
    cp ${c.repomd} $out/repodata/repomd.xml
    cp ${c.primaryFile} $out/${c.primary}
  '';

  claudePackages = runCommand "rime-claude-desktop-packages-${d.version}" { } ''
    got=$(sha256sum ${d.packages} | cut -d' ' -f1)
    [ "$got" = "${d.packagesSha256}" ] || { echo "the kept Packages is not the pinned one ($got != ${d.packagesSha256})" >&2; exit 1; }
    cp ${d.packages} $out
  '';

  # The vendor repositories as the verifier reads them: repository path -> file.
  files = {
    chatgpt = {
      "repodata/repomd.xml" = "${chatgptIndex}/repodata/repomd.xml";
      ${c.primary} = "${chatgptIndex}/${c.primary}";
      ${c.file} = fetchurl {
        url = "${c.repo}/${c.file}";
        inherit (c) sha256;
      };
    };
    claude-desktop = {
      "dists/stable/InRelease" = d.inRelease;
      # The kept copy (pins.nix says why), checked against the pinned hash; the verifier then checks it against the
      # signed InRelease.
      "dists/stable/main/binary-amd64/Packages" = claudePackages;
      ${d.file} = fetchurl {
        url = "${d.repo}/${d.file}";
        inherit (d) sha256;
      };
    };
  };
in
{
  chatgpt = rime-vendor.fetchVerified {
    name = "chatgpt";
    files = files.chatgpt;
  };

  claude-desktop = rime-vendor.fetchVerified {
    name = "claude-desktop";
    files = files.claude-desktop;
  };

  inherit pins files;
}
