#!/usr/bin/env bash
# payload-release.sh PUBLIC_KEY_FILE PAYLOAD_DIR: sign a payload payload-build.sh made, for release.
# payload-release.sh --verify PUBLIC_KEY_FILE PAYLOAD_DIR: only the check (a payload pulled back from the registry).
#
#   PUBLIC_KEY_FILE   the public line the bootc image trusts (files/system/trust/nixos-bridge-payload.pub)
#   RIME_BRIDGE_PAYLOAD_KEY (environment)  the Nix-format secret key line payload-sign.sh takes
#
# Signs with payload-sign.sh, then refuses unless the public half of the secret is PUBLIC_KEY_FILE's line: a payload
# signed by any other key is one every bootc machine would refuse [payload-key-untrusted]. Then checks the payload
# with the bridge's own verify_payload (share/rime-bridge/rime-migrate beside this script, the program bootc runs)
# against PUBLIC_KEY_FILE. On any refusal the signature just written is removed, so a refused payload is never left
# looking signed. The secret exists only in a 0700 temporary directory.
# Needs: bash, python3, and payload-sign.sh's openssl, base64, head, tail, cmp.
set -euo pipefail
die() { printf 'payload-release: %s\n' "$*" >&2; exit 1; }
verify_only=0
if [[ ${1:-} == --verify ]]; then verify_only=1; shift; fi
[[ $# -eq 2 ]] || die "usage: payload-release.sh [--verify] PUBLIC_KEY_FILE PAYLOAD_DIR"
pub=$1 dir=$2
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
want=$(cat -- "$pub") || die "$pub: cannot read"
[[ $want =~ ^[^:[:space:]]+:[A-Za-z0-9+/]{43}=$ ]] || die "$pub is not one \"<name>:<base64 ed25519 public key>\" line"
[[ -f $dir/manifest.json ]] || die "$dir/manifest.json: no such file"

verify() {
  python3 -I -B - "$here/rime-migrate" "$pub" "$dir" <<'PY'
import importlib.machinery, importlib.util, sys
prog, pub, d = sys.argv[1:]
loader = importlib.machinery.SourceFileLoader("rime_migrate", prog)
spec = importlib.util.spec_from_loader("rime_migrate", loader)
b = importlib.util.module_from_spec(spec)
loader.exec_module(b)
try:
    man = b.verify_payload(d, b.payload_keys(pub))
except b.Refused as e:
    sys.exit(f"payload-release: the bridge refuses the payload [{e.name}]: {e}")
print(f"payload-release: the bridge accepts {d}: toplevel {man['toplevel']}, signed by {man['signed_by']}",
      file=sys.stderr)
PY
}

if [[ $verify_only == 1 ]]; then
  [[ -f $dir/manifest.json.sig ]] || die "$dir/manifest.json.sig: no such file"
  verify
  exit 0
fi

[[ -n ${RIME_BRIDGE_PAYLOAD_KEY:-} ]] || die "RIME_BRIDGE_PAYLOAD_KEY is empty"
[[ ! -e $dir/manifest.json.sig ]] || die "$dir/manifest.json.sig exists already: build the payload again"
umask 077
t=$(mktemp -d)
signed=0
cleanup() {
  rm -rf -- "$t"
  [[ $signed == 1 ]] || rm -f -- "$dir/manifest.json.sig"
}
trap cleanup EXIT
printf '%s\n' "$RIME_BRIDGE_PAYLOAD_KEY" > "$t/secret"
got=$(bash "$here/payload-sign.sh" "$t/secret" "$dir")
[[ $got == "$want" ]] || die "the secret's public half ($got) is not the trusted key in $pub ($want)"
verify
signed=1
echo "payload-release: $dir signed by ${want%%:*} and accepted by the bridge" >&2
