#!/usr/bin/env bash
# payload-sign.sh SECRET_KEY PAYLOAD_DIR: sign a bridge payload's manifest (docs/nixos/bridge.md "The payload is
# signed"). The release pipeline runs it with the release key; the OVMF lab with a TEST key (nix/probe/ovmf/bridge).
#
#   SECRET_KEY   a Nix-format secret key file, one line "<name>:<base64 of 64 bytes: ed25519 seed || public key>",
#                as `nix key generate-secret --key-name rime-bridge-payload-1` writes it
#   PAYLOAD_DIR  the payload: manifest.json (and the files it names by sha256)
#
# Writes PAYLOAD_DIR/manifest.json.sig = "<name>:<base64 of the 64-byte ed25519 signature>" over the exact bytes of
# manifest.json, then verifies it the way the bootc side's `rime migrate start` does (openssl pkeyutl -verify -rawin
# against the public half) and prints the public key line, which is what the bootc image ships as
# /usr/share/rime-os/trust/nixos-bridge-payload.pub. The secret seed exists only in a 0700 temporary directory.
# Needs: openssl (3.x, ed25519 with -rawin), base64, head, tail, cmp.
set -euo pipefail
die() { printf 'payload-sign: %s\n' "$*" >&2; exit 1; }
[[ $# -eq 2 ]] || die "usage: payload-sign.sh SECRET_KEY PAYLOAD_DIR"
secret=$1 dir=$2
[[ -f $dir/manifest.json ]] || die "$dir/manifest.json: no such file"
line=$(cat -- "$secret")
[[ $line == *:* && $line != *$'\n'* ]] || die "$secret is not one \"<name>:<base64>\" line"
name=${line%%:*}
[[ -n $name && $name != *[[:space:]]* ]] || die "$secret has no key name"

umask 077
t=$(mktemp -d)
trap 'rm -rf -- "$t"' EXIT
printf '%s' "${line#*:}" | base64 -d > "$t/secret.bin" 2>/dev/null || die "$secret: the key is not base64"
[[ $(stat -c %s "$t/secret.bin") -eq 64 ]] || die "$secret: the key is not 64 bytes (seed || public key)"
# PKCS#8 for an ed25519 seed (RFC 8410) and the SubjectPublicKeyInfo of its public key.
{ printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; head -c 32 "$t/secret.bin"; } > "$t/key.der"
openssl pkey -inform DER -in "$t/key.der" -pubout -outform DER -out "$t/pub.der"
tail -c 32 "$t/secret.bin" > "$t/pub.half"
tail -c 32 "$t/pub.der" | cmp -s - "$t/pub.half" || die "$secret: its public half does not belong to its seed"

openssl pkeyutl -sign -keyform DER -inkey "$t/key.der" -rawin -in "$dir/manifest.json" -out "$t/sig.bin"
[[ $(stat -c %s "$t/sig.bin") -eq 64 ]] || die "openssl made a signature that is not 64 bytes"
printf '%s:%s\n' "$name" "$(base64 -w0 < "$t/sig.bin")" > "$dir/manifest.json.sig"
# Read back, from the files as written, exactly as the bootc side checks them.
sig=$(cat "$dir/manifest.json.sig")
printf '%s' "${sig#*:}" | base64 -d > "$t/sig.check"
openssl pkeyutl -verify -pubin -keyform DER -inkey "$t/pub.der" -rawin -in "$dir/manifest.json" \
  -sigfile "$t/sig.check" >/dev/null || die "the signature just written does not verify"
echo "payload-sign: $dir/manifest.json signed by $name (verified)" >&2
printf '%s:%s\n' "$name" "$(base64 -w0 < "$t/pub.half")"
