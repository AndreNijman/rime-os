# rime-bridge-payload: the release pipeline's payload tools (docs/nixos/bridge.md "Where a payload comes from"), with
# their tools on PATH. The scripts themselves are rime-bridge's share/rime-bridge/ (what the OVMF lab runs too).
#
#   bin/rime-bridge-payload-build TOPLEVEL CLOSURE_INFO OUTDIR      payload-build.sh
#   bin/rime-bridge-payload-release [--verify] PUBLIC_KEY_FILE DIR  payload-release.sh (signs with payload-sign.sh,
#                                                                  then the bridge's own verify_payload)
#
# A separate package so the moved system (which carries rime-bridge for `rime migrate finish`) does not carry these.
{
  symlinkJoin,
  writeShellApplication,
  rime-bridge,
  bash,
  coreutils,
  diffutils,
  gnugrep,
  gnused,
  gnutar,
  openssl,
  python3,
  zstd,
}:

let
  share = "${rime-bridge}/share/rime-bridge";
  build = writeShellApplication {
    name = "rime-bridge-payload-build";
    runtimeInputs = [ bash coreutils gnugrep gnused gnutar python3 zstd ];
    text = ''exec bash ${share}/payload-build.sh "$@"'';
  };
  release = writeShellApplication {
    name = "rime-bridge-payload-release";
    runtimeInputs = [ bash coreutils diffutils openssl python3 ];
    text = ''exec bash ${share}/payload-release.sh "$@"'';
  };
in
symlinkJoin {
  name = "rime-bridge-payload";
  paths = [ build release ];
}
