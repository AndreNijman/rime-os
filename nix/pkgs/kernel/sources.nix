# The three kernel inputs, fetched by the URL and sha256 kernel/kernel.pin records (the same bytes the bootc
# kernel tier verified with sha256sum -c). fetchurl, not fetchFromGitHub: the pin hashes the tarball/file
# itself, and a NAR hash would be a second, unrelated pin for the same input.
{ lib, fetchurl }:

let
  pin = import ./pin.nix { inherit lib; };
in
{
  inherit pin;

  src = fetchurl {
    inherit (pin.src) url sha256;
    name = "linux-${pin.tag}.tar.gz";
  };

  upstreamConfig = fetchurl {
    inherit (pin.config) url sha256;
    name = "linux-cachyos-config-${pin.version}";
  };

  borePatch = fetchurl {
    inherit (pin.bore) url sha256;
    name = "0001-bore-cachy.patch";
  };

  # `uname -r` of the built kernel. Keeps "cachyos" (anything that globs *cachyos* still matches) and names the
  # Rime build number from the pin, like bootc's `<ver>-cachyos1.apex<N>.fc45.x86_64`, minus the Fedora parts.
  localVersion = "-cachyos1-rime${pin.build}";
  modDirVersion = "${pin.version}-cachyos1-rime${pin.build}";
}
