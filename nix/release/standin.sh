# rime-release-standin: the whole D6 publish pipeline on one machine, against local stand-ins for R2 and GHCR, with
# THROWAWAY keys. Same tool, same subcommands and same order as the publish job in nix-release.yml.
#
#   rime-release-standin WORKDIR [--os SHA40] [--shell SHA40] [--today YYYY.MM.DD] [--keep-running]
#
#   S3 (R2 stand-in)    versitygw, http://127.0.0.1:$STANDIN_S3_PORT (19000), bucket rime-cache, anonymous read
#   OCI (GHCR stand-in) distribution, 127.0.0.1:$STANDIN_REGISTRY_PORT (15000), repository andrenijman/rime-os
#   upstream cache      $STANDIN_UPSTREAM (https://cache.nixos.org) trusted with $STANDIN_UPSTREAM_KEY
#   narinfo key         rime-test-cache-1, generated into WORKDIR/keys (TEST)
#   cosign identity     nix-release.yml@refs/heads/main under a TEST Fulcio root, WORKDIR/signing (TEST)
#
# Publishes the fixture toplevels (nix/release/fixtures.nix), then downloads the release into a separate store
# the way a client would, and writes WORKDIR/published.json: everything a consumer needs (endpoint, bucket,
# manifest ref, public keys). Needs root for nix-store --import (the stand-in runs in the rime-nix container).
set -euo pipefail

work="${1:?usage: rime-release-standin WORKDIR [--os SHA40] [--shell SHA40] [--today YYYY.MM.DD] [--keep-running]}"
shift
os_rev=1111111111111111111111111111111111111111
shell_rev=2222222222222222222222222222222222222222
nixpkgs_rev=1c89083e451e891152365b799595cf7790a43325
today=2026.11.01
keep=no
while [ $# -gt 0 ]; do
    case "$1" in
        --os) os_rev="$2"; shift 2 ;;
        --shell) shell_rev="$2"; shift 2 ;;
        --today) today="$2"; shift 2 ;;
        --keep-running) keep=yes; shift ;;
        *) echo "rime-release-standin: unknown argument $1" >&2; exit 2 ;;
    esac
done
s3_port="${STANDIN_S3_PORT:-19000}"
reg_port="${STANDIN_REGISTRY_PORT:-15000}"
upstream="${STANDIN_UPSTREAM:-https://cache.nixos.org}"
upstream_key="${STANDIN_UPSTREAM_KEY:-cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=}"
signer=https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main
issuer=https://token.actions.githubusercontent.com
repo="127.0.0.1:$reg_port/andrenijman/rime-os"
endpoint="http://127.0.0.1:$s3_port"
cache="s3://rime-cache?endpoint=$endpoint&region=auto&compression=zstd"
public="$endpoint/rime-cache"

mkdir -p "$work"/{s3,registry,keys,signing,ca,out,build}
work="$(cd "$work" && pwd)"
export AWS_ACCESS_KEY_ID=rimetestaccess AWS_SECRET_ACCESS_KEY=rimetestsecretkey AWS_DEFAULT_REGION=auto
: > "$work/pids"
stop() { while read -r pid; do kill "$pid" 2>/dev/null || true; done < "$work/pids"; }
[ "$keep" = yes ] || trap stop EXIT

versitygw --access "$AWS_ACCESS_KEY_ID" --secret "$AWS_SECRET_ACCESS_KEY" --region auto \
    --port "127.0.0.1:$s3_port" posix "$work/s3" > "$work/s3.log" 2>&1 &
echo $! >> "$work/pids"
cat > "$work/registry.yml" <<YML
version: 0.1
storage: { filesystem: { rootdirectory: $work/registry } }
http: { addr: 127.0.0.1:$reg_port }
YML
registry serve "$work/registry.yml" > "$work/registry.log" 2>&1 &
echo $! >> "$work/pids"
for _ in $(seq 50); do
    curl -s -o /dev/null "http://127.0.0.1:$reg_port/v2/" && curl -s -o /dev/null "$endpoint" && break
    sleep 0.2
done

aws --endpoint-url "$endpoint" s3api head-bucket --bucket rime-cache >/dev/null 2>&1 \
  || aws --endpoint-url "$endpoint" s3api create-bucket --bucket rime-cache \
        --create-bucket-configuration LocationConstraint=auto >/dev/null
aws --endpoint-url "$endpoint" s3api put-bucket-policy --bucket rime-cache --policy \
    '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":["s3:GetObject"],"Resource":["arn:aws:s3:::rime-cache/*"]}]}'

if [ ! -s "$work/keys/rime-test-cache-1.sec" ]; then
    nix --extra-experimental-features nix-command key generate-secret --key-name rime-test-cache-1 \
        > "$work/keys/rime-test-cache-1.sec"
fi
cache_pub="$(nix --extra-experimental-features nix-command key convert-secret-to-public < "$work/keys/rime-test-cache-1.sec")"
[ -s "$work/signing/cosign.key" ] || mint-test-signing "$work/ca" "$work/signing" "$signer" "$issuer"

# ── the build job ──
rime-nix-release closure --variant "plain=$STANDIN_PLAIN" --variant "luks=$STANDIN_LUKS" --out "$work/build/closure.json"
rime-nix-release split --closure "$work/build/closure.json" --upstream "$upstream" --out "$work/build/split.json"
rime-nix-release export --closure "$work/build/closure.json" --split "$work/build/split.json" --out "$work/artifact"

# ── the publish job ──
rime-nix-release publish --artifact "$work/artifact" --plan "$STANDIN_PLAN" \
    --cache "$cache" --secret-key-file "$work/keys/rime-test-cache-1.sec" \
    --upstream "$upstream" --upstream-key "$upstream_key" \
    --public "$public" --cache-pubkey "$cache_pub" --cache-url-hint "$public" \
    --repo "$repo" --plain-http --fulcio-root "$work/signing/fulcio-root.pem" \
    --rime-os "$os_rev" --rime-shell "$shell_rev" --nixpkgs "$nixpkgs_rev" --today "$today" \
    --allow-test-variants --test-signing "$work/signing" --out "$work/out"

# ── a client ──
rm -rf "$work/client-store"
rime-nix-release consume --repo "$repo" --plain-http --fulcio-root "$work/signing/fulcio-root.pem" \
    --cache "$public" --channel edge --variant plain --store "$work/client-store" | tee "$work/consumed.json"

python3 - "$work" "$public" "$cache" "$repo" "$cache_pub" "$signer" "$issuer" <<'PY'
import json, sys
work, public, cache, repo, pub, signer, issuer = sys.argv[1:]
st = json.load(open(f"{work}/out/published.json"))
doc = {
    "TEST": "throwaway keys; nothing here is a Rime key",
    "cache": {"public_url": public, "s3_url": cache, "bucket": "rime-cache", "narinfo_public_key": pub},
    "registry": {"repository": repo, "plain_http": True, "manifest_tag": st.get("manifest_tag"),
                 "manifest_digest": st.get("manifest_digest"), "channels_tag": "nix-channels",
                 "channels_digest": st.get("channels_digest"), "release": st.get("release")},
    "trust": {"fulcio_root": f"{work}/signing/fulcio-root.pem", "signer": signer, "issuer": issuer},
    "consume": ("rime-nix-release consume --repo %s --plain-http --fulcio-root %s/signing/fulcio-root.pem "
                "--cache %s --channel edge --variant plain" % (repo, work, public)),
}
json.dump(doc, open(f"{work}/published.json", "w"), indent=1)
print(json.dumps(doc, indent=1))
PY
