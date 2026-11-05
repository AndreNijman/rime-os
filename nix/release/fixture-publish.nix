# TEST ONLY: the update-client VM test's publisher (fixture-publish.py), with the tools it runs on PATH and the
# test's keys and locations baked in. Installed on the test's server node; never part of a Rime system.
{
  lib,
  runCommand,
  makeWrapper,
  python3,
  skopeo,
  openssl,
  nix,
  rustfs-cli,
  keys,
  repo,
  cache ? "/srv/rime-cache",
  bucket ? "local/rime-cache",
}:

runCommand "rime-fixture-publish-TEST" { nativeBuildInputs = [ makeWrapper ]; } ''
  mkdir -p $out/bin $out/share
  cp ${./fixture-publish.py} $out/share/fixture-publish.py
  makeWrapper ${python3}/bin/python3 $out/bin/rime-fixture-publish \
    --add-flags "-I $out/share/fixture-publish.py" \
    --prefix PATH : ${lib.makeBinPath [ skopeo openssl nix rustfs-cli ]} \
    --set FIXTURE_KEYS ${keys} \
    --set FIXTURE_REPO ${repo} \
    --set FIXTURE_CACHE ${cache} \
    --set FIXTURE_BUCKET ${bucket}
''
