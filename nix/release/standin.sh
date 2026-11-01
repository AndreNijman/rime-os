# rime-release-standin: the whole D6 publish pipeline on one machine, against local stand-ins for the binary cache
# and GHCR, with THROWAWAY keys. Same tool, same subcommands and same order as the publish job in nix-release.yml.
#
#   rime-release-standin WORKDIR [--backend github|s3] [--os SHA40] [--shell SHA40] [--today YYYY.MM.DD]
#                        [--keep-running]
#
#   --backend github (default, the production shape): cache.rimeos.com = GitHub Pages of rime-cache, NARs = its
#   release assets, narinfo URL: absolute. Two origins, as in production:
#     Pages (narinfos)    python http.server over the checkout of a bare git repo's main (a post-receive hook
#                         deploys it, as Pages does), http://127.0.0.1:$STANDIN_PAGES_PORT (18090)
#     GitHub (NARs)       fake-github (nix/release/fake_github.py, TEST): API + uploads http://127.0.0.1:18081,
#                         release downloads http://127.0.0.1:18082 answering 302 to objects http://127.0.0.1:18083
#     git remote          file://WORKDIR/rime-cache.git (token TEST: rime-test-cache-token)
#   --backend s3 (the fallback): versitygw, http://127.0.0.1:$STANDIN_S3_PORT (19000), bucket rime-cache, anonymous
#   read; the whole closure is mirrored there.
#
#   OCI (GHCR stand-in) distribution, 127.0.0.1:$STANDIN_REGISTRY_PORT (15000), repository andrenijman/rime-os
#   upstream cache      $STANDIN_UPSTREAM (https://cache.nixos.org) trusted with $STANDIN_UPSTREAM_KEY
#   narinfo key         rime-test-cache-1, generated into WORKDIR/keys (TEST)
#   cosign identity     nix-release.yml@refs/heads/main under a TEST Fulcio root, WORKDIR/signing (TEST)
#
# Publishes the fixture toplevels (nix/release/fixtures.nix), then downloads the release into a separate store
# the way a client would, and writes WORKDIR/published.json: everything a consumer needs (endpoints, manifest ref,
# public keys). Runs as root in the rime-nix container (the s3 backend's nix-store --import needs it).
set -euo pipefail

work="${1:?usage: rime-release-standin WORKDIR [--backend github|s3] [--os SHA40] [--shell SHA40] [--today YYYY.MM.DD] [--keep-running]}"
shift
backend=github
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
        --backend) backend="$2"; shift 2 ;;
        *) echo "rime-release-standin: unknown argument $1" >&2; exit 2 ;;
    esac
done
case "$backend" in github|s3) ;; *) echo "rime-release-standin: --backend github|s3" >&2; exit 2 ;; esac
s3_port="${STANDIN_S3_PORT:-19000}"
pages_port="${STANDIN_PAGES_PORT:-18090}"
gh_api="http://127.0.0.1:${STANDIN_GH_API_PORT:-18081}"
gh_download="http://127.0.0.1:${STANDIN_GH_DOWNLOAD_PORT:-18082}"
gh_objects="127.0.0.1:${STANDIN_GH_OBJECTS_PORT:-18083}"
gh_token=rime-test-cache-token
pages="http://127.0.0.1:$pages_port"
reg_port="${STANDIN_REGISTRY_PORT:-15000}"
upstream="${STANDIN_UPSTREAM:-https://cache.nixos.org}"
upstream_key="${STANDIN_UPSTREAM_KEY:-cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=}"
signer=https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main
issuer=https://token.actions.githubusercontent.com
repo="127.0.0.1:$reg_port/andrenijman/rime-os"
endpoint="http://127.0.0.1:$s3_port"
cache="s3://rime-cache?endpoint=$endpoint&region=auto&compression=zstd"
public="$endpoint/rime-cache"

mkdir -p "$work"/{s3,registry,keys,signing,ca,out,build,gh,pages}
work="$(cd "$work" && pwd)"
export AWS_ACCESS_KEY_ID=rimetestaccess AWS_SECRET_ACCESS_KEY=rimetestsecretkey AWS_DEFAULT_REGION=auto
: > "$work/pids"
stop() { while read -r pid; do kill "$pid" 2>/dev/null || true; done < "$work/pids"; }
[ "$keep" = yes ] || trap stop EXIT

if [ "$backend" = s3 ]; then
    versitygw --access "$AWS_ACCESS_KEY_ID" --secret "$AWS_SECRET_ACCESS_KEY" --region auto \
        --port "127.0.0.1:$s3_port" posix "$work/s3" > "$work/s3.log" 2>&1 &
    echo $! >> "$work/pids"
else
    # rime-cache as Andre bootstrapped it: main with nix-cache-info, CNAME, .nojekyll, README. Pages = main's tree.
    if [ ! -d "$work/rime-cache.git" ]; then
        git init --quiet --bare -b main "$work/rime-cache.git"
        printf '#!/bin/sh\nexec git --work-tree=%s checkout -f main\n' "$work/pages" > "$work/rime-cache.git/hooks/post-receive"
        chmod +x "$work/rime-cache.git/hooks/post-receive"
        rm -rf "$work/seed"
        git init --quiet -b main "$work/seed"
        printf 'StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 50\n' > "$work/seed/nix-cache-info"
        echo cache.rimeos.com > "$work/seed/CNAME"
        : > "$work/seed/.nojekyll"
        echo "TEST stand-in of rime-cache" > "$work/seed/README.md"
        git -C "$work/seed" add -A
        git -C "$work/seed" -c user.name=TEST -c user.email=test@invalid commit --quiet -m "rime-cache (TEST seed)"
        git -C "$work/seed" push --quiet "$work/rime-cache.git" main
    fi
    python3 -m http.server --bind 127.0.0.1 --directory "$work/pages" "$pages_port" > "$work/pages.log" 2>&1 &
    echo $! >> "$work/pids"
    fake-github --dir "$work/gh" --repo AndreNijman/rime-cache --token "$gh_token" --api "${gh_api#http://}" \
        --download "${gh_download#http://}" --objects "$gh_objects" > "$work/gh.log" 2>&1 &
    echo $! >> "$work/pids"
fi
cat > "$work/registry.yml" <<YML
version: 0.1
storage: { filesystem: { rootdirectory: $work/registry } }
http: { addr: 127.0.0.1:$reg_port }
YML
registry serve "$work/registry.yml" > "$work/registry.log" 2>&1 &
echo $! >> "$work/pids"
for _ in $(seq 50); do
    if [ "$backend" = s3 ]; then
        curl -s -o /dev/null "http://127.0.0.1:$reg_port/v2/" && curl -s -o /dev/null "$endpoint" && break
    else
        curl -s -o /dev/null "http://127.0.0.1:$reg_port/v2/" && curl -s -o /dev/null "$pages/nix-cache-info" \
          && curl -s -o /dev/null "$gh_api/" && break
    fi
    sleep 0.2
done

if [ "$backend" = s3 ]; then
    aws --endpoint-url "$endpoint" s3api head-bucket --bucket rime-cache >/dev/null 2>&1 \
      || aws --endpoint-url "$endpoint" s3api create-bucket --bucket rime-cache \
            --create-bucket-configuration LocationConstraint=auto >/dev/null
    aws --endpoint-url "$endpoint" s3api put-bucket-policy --bucket rime-cache --policy \
        '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":["s3:GetObject"],"Resource":["arn:aws:s3:::rime-cache/*"]}]}'
fi

if [ ! -s "$work/keys/rime-test-cache-1.sec" ]; then
    nix --extra-experimental-features nix-command key generate-secret --key-name rime-test-cache-1 \
        > "$work/keys/rime-test-cache-1.sec"
fi
cache_pub="$(nix --extra-experimental-features nix-command key convert-secret-to-public < "$work/keys/rime-test-cache-1.sec")"
[ -s "$work/signing/cosign.key" ] || mint-test-signing "$work/ca" "$work/signing" "$signer" "$issuer"

# ── the build job ──
rime-nix-release closure --variant "plain=$STANDIN_PLAIN" --variant "luks=$STANDIN_LUKS" --out "$work/build/closure.json"
rime-nix-release split --closure "$work/build/closure.json" --upstream "$upstream" --out "$work/build/split.json"
rm -rf "$work/artifact"
common=(--plan "$STANDIN_PLAN" --secret-key-file "$work/keys/rime-test-cache-1.sec"
        --upstream "$upstream" --upstream-key "$upstream_key" --cache-pubkey "$cache_pub"
        --repo "$repo" --plain-http --fulcio-root "$work/signing/fulcio-root.pem"
        --rime-os "$os_rev" --rime-shell "$shell_rev" --nixpkgs "$nixpkgs_rev" --today "$today"
        --allow-test-variants --test-signing "$work/signing" --out "$work/out")
if [ "$backend" = s3 ]; then
    rime-nix-release export --closure "$work/build/closure.json" --split "$work/build/split.json" --out "$work/artifact"
    # ── the publish job ──
    rime-nix-release publish --backend s3 --artifact "$work/artifact" --cache "$cache" \
        --public "$public" --cache-url-hint "$public" "${common[@]}"
    caches=(--cache "$public")
else
    rime-nix-release stage --closure "$work/build/closure.json" --split "$work/build/split.json" --out "$work/artifact"
    # ── the publish job ──
    RIME_CACHE_TOKEN="$gh_token" rime-nix-release publish --backend github --artifact "$work/artifact" \
        --gh-repo AndreNijman/rime-cache --gh-remote "file://$work/rime-cache.git" \
        --gh-api "$gh_api" --gh-uploads "$gh_api" --gh-download "$gh_download" \
        --public "$pages" --cache-url-hint "$pages" "${common[@]}"
    public="$pages"
    caches=(--cache "$pages" --cache "$upstream")
fi

# ── a client ──
rm -rf "$work/client-store"
rime-nix-release consume --repo "$repo" --plain-http --fulcio-root "$work/signing/fulcio-root.pem" \
    "${caches[@]}" --channel edge --variant plain --store "$work/client-store" | tee "$work/consumed.json"

python3 - "$work" "$public" "$cache" "$repo" "$cache_pub" "$signer" "$issuer" "$backend" "$upstream" "$upstream_key" \
    "$gh_download" <<'PY'
import json, sys
work, public, cache, repo, pub, signer, issuer, backend, upstream, upkey, gh_download = sys.argv[1:]
st = json.load(open(f"{work}/out/published.json"))
if backend == "github":
    cache_doc = {"backend": "github", "narinfo_url": public, "nar_origin": gh_download,
                 "git_remote": f"file://{work}/rime-cache.git", "narinfo_public_key": pub,
                 "upstream_url": upstream, "upstream_public_key": upkey,
                 "substituters": [public, upstream], "trusted_public_keys": [pub, upkey]}
    caches = f"--cache {public} --cache {upstream}"
else:
    cache_doc = {"backend": "s3", "public_url": public, "s3_url": cache, "bucket": "rime-cache",
                 "narinfo_public_key": pub, "substituters": [public], "trusted_public_keys": [pub]}
    caches = f"--cache {public}"
doc = {
    "TEST": "throwaway keys; nothing here is a Rime key",
    "cache": cache_doc,
    "registry": {"repository": repo, "plain_http": True, "manifest_tag": st.get("manifest_tag"),
                 "manifest_digest": st.get("manifest_digest"), "channels_tag": "nix-channels",
                 "channels_digest": st.get("channels_digest"), "release": st.get("release")},
    "trust": {"fulcio_root": f"{work}/signing/fulcio-root.pem", "signer": signer, "issuer": issuer},
    "consume": ("rime-nix-release consume --repo %s --plain-http --fulcio-root %s/signing/fulcio-root.pem "
                "%s --channel edge --variant plain" % (repo, work, caches)),
}
json.dump(doc, open(f"{work}/published.json", "w"), indent=1)
print(json.dumps(doc, indent=1))
PY
