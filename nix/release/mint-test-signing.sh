#!/usr/bin/env bash
# mint-test-signing: THROWAWAY signing material for the stand-in, shaped like keyless cosign's.
#
#   mint-test-signing CADIR OUTDIR SAN [ISSUER]
#
# CADIR gets a TEST root and intermediate (created once, reused by later calls, so a second identity chains to the
# same root). OUTDIR gets a P-256 leaf whose SAN is SAN and whose GitHub-issuer extensions (1.3.6.1.4.1.57264.1.1
# raw, .1.8 DER) say ISSUER, exactly what Fulcio issues to a GitHub Actions job, plus cosign.key (the leaf key in
# cosign's format, empty password), leaf.pem and chain.pem: what `rime-nix-release publish --test-signing` takes.
# Nothing here is ever a Rime key. Every subject says TEST.
set -euo pipefail
ca="${1:?CADIR}" out="${2:?OUTDIR}" san="${3:?SAN}" issuer="${4:-https://token.actions.githubusercontent.com}"
cosign="${RIME_COSIGN:-cosign}"
mkdir -p "$ca" "$out"
if [ ! -s "$ca/root.pem" ]; then
    openssl ecparam -name prime256v1 -genkey -noout -out "$ca/root.key"
    openssl req -new -x509 -key "$ca/root.key" -subj '/O=rime TEST/CN=rime TEST fulcio root' -days 30 \
        -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign -out "$ca/root.pem"
    openssl ecparam -name prime256v1 -genkey -noout -out "$ca/int.key"
    openssl req -new -key "$ca/int.key" -subj '/O=rime TEST/CN=rime TEST fulcio intermediate' -out "$ca/int.csr"
    printf 'basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n' > "$ca/int.ext"
    openssl x509 -req -in "$ca/int.csr" -CA "$ca/root.pem" -CAkey "$ca/root.key" -CAcreateserial -days 30 \
        -extfile "$ca/int.ext" -out "$ca/int.pem" 2>/dev/null
fi
openssl ecparam -name prime256v1 -genkey -noout -out "$out/leaf.key"
openssl pkcs8 -topk8 -nocrypt -in "$out/leaf.key" -out "$out/leaf.p8"
openssl req -new -key "$out/leaf.key" -subj '/O=rime TEST/CN=rime TEST signing leaf' -out "$out/leaf.csr"
printf 'subjectAltName=critical,URI:%s\n1.3.6.1.4.1.57264.1.1=DER:%s\n1.3.6.1.4.1.57264.1.8=ASN1:UTF8String:%s\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=codeSigning\n' \
    "$san" "$(printf '%s' "$issuer" | od -An -tx1 | tr -d ' \n')" "$issuer" > "$out/leaf.ext"
openssl x509 -req -in "$out/leaf.csr" -CA "$ca/int.pem" -CAkey "$ca/int.key" -CAcreateserial -days 2 \
    -extfile "$out/leaf.ext" -out "$out/leaf.pem" 2>/dev/null
cat "$ca/int.pem" "$ca/root.pem" > "$out/chain.pem"
cp "$ca/root.pem" "$out/fulcio-root.pem"
rm -f "$out/cosign.key" "$out/cosign.pub"
COSIGN_PASSWORD="" "$cosign" import-key-pair --key "$out/leaf.p8" --output-key-prefix "$out/cosign" >/dev/null
echo "TEST signing material for $san in $out" >&2
