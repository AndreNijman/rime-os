# TEST ONLY. Throwaway keys for the update-client VM test (nix/tests/update-client.nix), generated inside this
# derivation and never committed: a stand-in Fulcio CA, signing certificates shaped like Sigstore's (SAN URI +
# the 1.3.6.1.4.1.57264.1.1 OIDC issuer extension), and two binary-cache key pairs. Nothing here is trusted by a
# real Rime system: only a test node that sets rime.update.fulcioRoot to this CA accepts these signatures.
{ runCommand, openssl, nix }:

runCommand "rime-update-fixture-keys-TEST"
  {
    nativeBuildInputs = [ openssl nix ];
    # The identities the test signs as: the one the client pins, and one it must refuse.
    good = "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main";
    wrong = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main";
    issuer = "https://token.actions.githubusercontent.com";
  }
  ''
    set -euo pipefail
    mkdir -p $out
    echo "TEST keys for nix/tests/update-client.nix; not a Rime signing key" > $out/README

    openssl ecparam -name prime256v1 -genkey -noout -out ca.key
    openssl req -x509 -new -key ca.key -sha256 -days 3650 -subj "/O=TEST/CN=TEST rime update fixture CA" \
      -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" -out $out/ca.pem

    # The issuer extension holds the raw string (Fulcio's v1 form), so the config gives it as DER octets.
    issuer_der=$(printf %s "$issuer" | od -An -v -tx1 | tr -d ' \n' | sed 's/../&:/g; s/:$//')
    leaf() {
      openssl ecparam -name prime256v1 -genkey -noout -out $out/$1.key
      openssl req -new -key $out/$1.key -subj "/O=TEST" -out $1.csr
      {
        echo "subjectAltName=critical,URI:$2"
        echo "1.3.6.1.4.1.57264.1.1=DER:$issuer_der"
        echo "basicConstraints=critical,CA:FALSE"
        echo "keyUsage=critical,digitalSignature"
        echo "extendedKeyUsage=codeSigning"
      } > $1.ext
      openssl x509 -req -in $1.csr -CA $out/ca.pem -CAkey ca.key -CAcreateserial -sha256 -days 3650 \
        -extfile $1.ext -out $out/$1.pem
      openssl verify -CAfile $out/ca.pem $out/$1.pem
    }
    leaf good "$good"
    leaf wrong "$wrong"

    # Binary-cache keys: the one the releases are signed with, and one no release is signed with.
    export NIX_STATE_DIR=$TMPDIR/state NIX_LOG_DIR=$TMPDIR/log NIX_CONF_DIR=$TMPDIR
    for k in cache wrong-cache; do
      nix-store --generate-binary-cache-key "rime-test-$k-1" $out/$k.sec $out/$k.pub
    done
  ''
