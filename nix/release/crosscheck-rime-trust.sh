#!/usr/bin/env bash
# crosscheck-rime-trust: ask the REAL client verifier (`rime trust --gate`, verify.rs) about a published object.
#
#   crosscheck-rime-trust --rime BIN --repo REPO --tag TAG --fulcio-root PEM --signer URI --issuer URI [--plain-http]
#
# The publish self-check is a Python port of verify.rs (rime_nix_release.py verify_sig_dir). This script keeps the
# port honest: it copies the object's real `.sig` from the registry into the fixture layout `rime trust` reads under
# $RIME_TRUST_ROOT (the one tests/test-rime-trust.sh and tests/test-rime-rollout.sh use: a bootc deployment whose
# origin is REPO:TAG), installs the given trust anchors under the binary's own trust paths, and runs the gate. Exit
# status is the gate's: 0 = it would accept the signature.
set -euo pipefail

rime="" repo="" tag="" root_pem="" signer="" issuer="" tls=()
while [ $# -gt 0 ]; do
    case "$1" in
        --rime) rime="$2"; shift 2 ;;
        --repo) repo="$2"; shift 2 ;;
        --tag) tag="$2"; shift 2 ;;
        --fulcio-root) root_pem="$2"; shift 2 ;;
        --signer) signer="$2"; shift 2 ;;
        --issuer) issuer="$2"; shift 2 ;;
        --plain-http) tls=(--src-tls-verify=false); shift ;;
        *) echo "crosscheck-rime-trust: unknown argument $1" >&2; exit 2 ;;
    esac
done
for v in rime repo tag root_pem signer issuer; do
    [ -n "${!v}" ] || { echo "crosscheck-rime-trust: --${v//_/-} is required" >&2; exit 2; }
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fx="$work/root"
# The trust files rime reads are under its own package (nix/pkgs/rimed.nix TRUST_*), so under the fixture root too.
pkg="$(dirname "$(dirname "$(readlink -f "$rime")")")"
trust="$fx$pkg/share/rime-os/trust"
csum=f3f505fc39fb268c59f4458365c96b764a7bd7d30f2f51e98bb6a009666b7852
bootcsum=1d98b51dd76621b656c50e4f22dc7e5eade9b0f869443a3efa90eee08eb9373e
mkdir -p "$fx/proc" "$fx/etc" "$trust" "$fx/registry" "$fx/ostree/boot.0/default/$bootcsum" \
         "$fx/ostree/deploy/default/deploy/$csum.0"
printf 'root=UUID=x rw quiet ostree=/ostree/boot.0/default/%s/0\n' "$bootcsum" > "$fx/proc/cmdline"
ln -sfn "../../../deploy/default/deploy/$csum.0" "$fx/ostree/boot.0/default/$bootcsum/0"
printf '[origin]\ncontainer-image-reference=ostree-unverified-registry:%s:%s\n' "$repo" "$tag" \
    > "$fx/ostree/deploy/default/deploy/$csum.0.origin"
printf '0123456789abcdef0123456789abcdef\n' > "$fx/etc/machine-id"

digest="$(skopeo --insecure-policy inspect "${tls[@]/src-/}" --format '{{.Digest}}' "docker://$repo:$tag")"
printf '{"deployments":[{"booted":true,"base-commit-meta":{"ostree.manifest-digest":"%s"}}]}\n' "$digest" \
    > "$fx/rpm-ostree-status.json"
printf '%s\n' "$digest" > "$fx/registry/resolve.$tag"
sig="${digest/:/-}.sig"
skopeo --insecure-policy copy "${tls[@]}" "docker://$repo:$sig" "dir:$fx/registry/$sig" >/dev/null

cp "$root_pem" "$trust/fulcio-root.pem"
printf '%s\n' "$signer" > "$trust/expected-signer"
printf '%s\n' "$issuer" > "$trust/expected-issuer"

set +e
RIME_TRUST_ROOT="$fx" "$rime" trust --gate --json > "$work/out" 2> "$work/err"
rc=$?
set -e
cat "$work/out"
sed 's/^/rime trust (stderr): /' "$work/err" >&2
echo "crosscheck-rime-trust: rime trust --gate exited $rc for $repo:$tag ($digest)" >&2
exit "$rc"
