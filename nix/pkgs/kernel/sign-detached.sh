#!/usr/bin/env bash
# sign-detached.sh: make the detached MOK signature of Rime's kernel image (PLAN.md D3 + D7).
#
# Run by .github/workflows/nix-kernel-sign.yml on a GitHub-hosted runner with the APEX_SB_* secrets, and by the
# OVMF rig (nix/probe/ovmf, KERNEL=rime) with a throwaway key, so the rig proves the code CI runs. Never inside
# Nix: the private key must not enter the store. Output, for nix/pkgs/kernel/signing/signatures/:
#   <out>/<hash>.sig    PKCS#7 DER, sbsign --detached (what rime.kernel.signing reads)
#   <out>/<hash>.json   provenance: image store path, sha256, certificate fingerprint, time, signer note
#
# Usage: sign-detached.sh --image FILE --hash HASH --key KEY.pem --cert CERT.pem --pinned PINNED.(pem|der)
#                         --out DIR [--store-path P] [--note TEXT]
# Needs: sbsign, sbattach, sbverify (sbsigntools), openssl, sha256sum.
set -euo pipefail

die() { printf 'sign-detached: FATAL: %s\n' "$*" >&2; exit 1; }
image='' hash='' key='' cert='' pinned='' out='' store_path='' note=''
while [[ $# -gt 0 ]]; do
  case $1 in
    --image) image=$2; shift 2;;
    --hash) hash=$2; shift 2;;
    --key) key=$2; shift 2;;
    --cert) cert=$2; shift 2;;
    --pinned) pinned=$2; shift 2;;
    --out) out=$2; shift 2;;
    --store-path) store_path=$2; shift 2;;
    --note) note=$2; shift 2;;
    *) die "unknown argument $1";;
  esac
done
for v in image hash key cert pinned out; do [[ -n ${!v} ]] || die "--$v is required"; done
[[ $hash =~ ^[0-9a-df-np-sv-z]{32}$ ]] || die "--hash '$hash' is not a Nix store hash (32 base-32 characters)"
[[ -f $image ]] || die "no image at $image"

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
to_pem() { openssl x509 -in "$1" -noout 2>/dev/null && openssl x509 -in "$1" -out "$2" || openssl x509 -inform DER -in "$1" -out "$2"; }
to_pem "$cert" "$tmp/cert.pem" || die "$cert is not an X.509 certificate"
to_pem "$pinned" "$tmp/pinned.pem" || die "$pinned is not an X.509 certificate"

# The key and certificate are a pair, and the certificate is the pinned one: a secret pasted in mangled, or a
# rotated key whose certificate was not committed, fails here instead of producing a signature the Nix build
# (sbverify --cert <pinned>) would refuse later.
openssl rsa -in "$key" -noout -check >/dev/null 2>&1 || die "the signing key does not parse as an RSA key"
k=$(openssl rsa -in "$key" -noout -modulus | sha256sum | cut -d' ' -f1)
c=$(openssl x509 -in "$tmp/cert.pem" -noout -modulus | sha256sum | cut -d' ' -f1)
[[ $k == "$c" ]] || die "the signing key and certificate are not a pair"
fp=$(openssl x509 -in "$tmp/cert.pem" -outform DER | sha256sum | cut -d' ' -f1)
pfp=$(openssl x509 -in "$tmp/pinned.pem" -outform DER | sha256sum | cut -d' ' -f1)
[[ $fp == "$pfp" ]] || die "the signing certificate (sha256 $fp) is not the pinned one (sha256 $pfp, $pinned)"

# Only an unsigned image is signed: shim accepts an image if ANY signature on it verifies.
if sbverify --list "$image" 2>&1 | grep -E '^signature [0-9]+' >/dev/null; then
  die "$image already carries a signature"
fi

mkdir -p "$out"
sbsign --key "$key" --cert "$tmp/cert.pem" --detached --output "$out/$hash.sig" "$image" >/dev/null
# Exactly what the Nix side (sign.nix) will do, done here first.
cp "$image" "$tmp/attached"; chmod u+w "$tmp/attached"
sbattach --attach "$out/$hash.sig" "$tmp/attached" >/dev/null
sbverify --cert "$tmp/pinned.pem" "$tmp/attached" >/dev/null 2>&1 \
  || { rm -f "$out/$hash.sig"; die "the fresh signature does not verify against the pinned certificate"; }

img_sha=$(sha256sum "$image" | cut -d' ' -f1)
sig_sha=$(sha256sum "$out/$hash.sig" | cut -d' ' -f1)
subject=$(openssl x509 -in "$tmp/cert.pem" -noout -subject | sed 's/^subject=//')
# Hand-written JSON (no jq on every runner); every value is checked or generated above, none holds a quote.
cat > "$out/$hash.json" <<EOF
{
  "image_hash": "$hash",
  "image_store_path": "${store_path}",
  "image_sha256": "$img_sha",
  "signature_sha256": "$sig_sha",
  "certificate_subject": "$subject",
  "certificate_sha256": "$fp",
  "signed_at": "$(date -u +%FT%TZ)",
  "note": "${note//\"/\'}"
}
EOF
echo "signed $image -> $out/$hash.sig (image sha256 $img_sha, certificate $subject, sha256 $fp)"
