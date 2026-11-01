#!/usr/bin/bash
# ok() and bad() never fail, so `cond && ok || bad` is a safe if/else here.
# shellcheck disable=SC2015
# End-to-end test of rechunk/rechunk-image on a real bootc image.
#
#   1. a small "release" on top of quay.io/fedora/fedora-bootc (a fake Shell,
#      a fake daemon) is rechunked with no previous plan
#   2. the next "release" changes only the Shell and is rechunked against the
#      first one's plan: exactly ONE layer may change, and it must be the Shell's
#   3. a plan that does not describe the previous image is refused (fresh plan)
#   4. verify-lossless refuses two images whose files differ, two whose file
#      capabilities differ, and an image it cannot inventory capabilities in
#
# Needs root, podman, skopeo, jq and ~15 GB of container storage. Pulls the
# base image (about 1 GB). RIME_RECHUNK_BASE overrides it.
set -euo pipefail
export LC_ALL=C

here=$(cd "$(dirname "$0")/.." && pwd)
tool="$here/rechunk/rechunk-image"
BASE=${RIME_RECHUNK_BASE:-quay.io/fedora/fedora-bootc:45}
T=localhost/rime-rechunk-e2e
pass=0 fail=0
ok()  { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

[ "$(id -u)" = 0 ] || { echo "SKIP: needs root (rootful podman storage)" >&2; exit 77; }
for c in podman skopeo jq; do
    command -v "$c" >/dev/null || { echo "SKIP: $c missing" >&2; exit 77; }
done

work=$(mktemp -d "${TMPDIR:-/var/tmp}/rechunk-e2e.XXXXXX")
cleanup() {
    for t in 1 2 1c 1c-plan 2c 2c-plan 3c 3c-plan nocap stripcap; do podman rmi -f "$T:$t" >/dev/null 2>&1 || true; done
    rm -rf "$work"
}
trap cleanup EXIT

podman pull -q "$BASE" >/dev/null

# ── release 1 ────────────────────────────────────────────────────────────────
cat > "$work/Containerfile.1" <<EOF
FROM $BASE
RUN mkdir -p /usr/share/rime-shell && echo 'Shell v1' > /usr/share/rime-shell/Main.qml \\
 && head -c 2000000 /dev/urandom > /usr/bin/rime-agentd && chmod 0755 /usr/bin/rime-agentd
EOF
podman build -q --network=none -f "$work/Containerfile.1" -t "$T:1" "$work" >/dev/null

"$tool" --from "$T:1" --output "$T:1c" > "$work/log1" 2>&1 || { cat "$work/log1"; bad "release 1 rechunks"; exit 1; }
ok "release 1 rechunks, verifies lossless, passes bootc lint"
grep -q 'verify-lossless: identical' "$work/log1" && ok "lossless check ran" || bad "lossless check ran"
podman image exists "$T:1c-plan" && ok "plan image written" || bad "plan image written"

ann() { # image -> "<diff_id> <size> <components>" per layer (diff_id = uncompressed content)
    jq -rn --argjson m "$(skopeo inspect --raw "containers-storage:$1")" \
           --argjson c "$(skopeo inspect --config --raw "containers-storage:$1")" \
        '[$c.rootfs.diff_ids, $m.layers] | transpose[] | "\(.[0]) \(.[1].size) \(.[1].annotations["ostree.components"] // "")"'
}
ann "$T:1c" > "$work/ann1"
grep -E ' rime-shell(,|$)' "$work/ann1" > /dev/null && ok "the Shell has a layer of its own" || bad "the Shell has a layer of its own"
grep -E ' rime-daemons(,|$)' "$work/ann1" > /dev/null && ok "the daemons have a layer of their own" || bad "the daemons have a layer of their own"

# ── release 2: only the Shell changed ────────────────────────────────────────
printf 'FROM %s\nRUN echo "Shell v2" > /usr/share/rime-shell/Main.qml\n' "$T:1" > "$work/Containerfile.2"
podman build -q --network=none -f "$work/Containerfile.2" -t "$T:2" "$work" >/dev/null

"$tool" --from "$T:2" --output "$T:2c" \
    --previous "containers-storage:$T:1c" --previous-plan "containers-storage:$T:1c-plan" \
    > "$work/log2" 2>&1 || { cat "$work/log2"; bad "release 2 rechunks"; exit 1; }
grep -q 'keeping its layer plan' "$work/log2" && ok "release 2 keeps release 1's plan" || bad "release 2 keeps release 1's plan"
ann "$T:2c" > "$work/ann2"
changed=$(awk 'NR == FNR { seen[$1] = 1; next } !($1 in seen)' "$work/ann1" "$work/ann2")
# Content layers carry the components they hold. The layers with no components
# are ostree's own metadata (dirtree + commit objects, the repo config), which
# changes whenever ANY file does: a few MB, the floor of every update.
content=$(awk 'NF >= 3' <<<"$changed")
meta_bytes=$(awk 'NF == 2 { s += $2 } END { print s + 0 }' <<<"$changed")
n=$(printf '%s' "$content" | grep -c . || true)
if [ "$n" = 1 ] && grep -E ' rime-shell(,|$)' <<<"$content" >/dev/null; then
    ok "a Shell-only release changes exactly one content layer, the Shell's"
else
    bad "a Shell-only release changes exactly one content layer, the Shell's ($n changed: $content)"
fi
if [ "$meta_bytes" -lt $((64 * 1024 * 1024)) ]; then
    ok "and only $((meta_bytes / 1024)) KiB of ostree metadata besides"
else
    bad "ostree metadata that changed is $((meta_bytes / 1024)) KiB (want < 64 MiB)"
fi
[ "$(wc -l < "$work/ann1")" = "$(wc -l < "$work/ann2")" ] && ok "layer count stable" || bad "layer count stable"

# ── a plan for some other image is refused ──────────────────────────────────
"$tool" --from "$T:2" --output "$T:3c" \
    --previous "containers-storage:$BASE" --previous-plan "containers-storage:$T:1c-plan" \
    > "$work/log3" 2>&1 || { cat "$work/log3"; bad "mismatched plan still builds"; exit 1; }
grep -q 'does not describe the layers' "$work/log3" && grep -q 'starts a fresh layer plan' "$work/log3" \
    && ok "a plan for a different image is refused" || bad "a plan for a different image is refused"

# ── the lossless check bites ─────────────────────────────────────────────────
if "$here/rechunk/verify-lossless" "$T:1" "$T:2c" > "$work/log4" 2>&1; then
    bad "verify-lossless refuses different files"
else
    grep -q 'sums differs' "$work/log4" && ok "verify-lossless refuses different files" || bad "verify-lossless refuses different files (wrong reason)"
fi

# Same files, one capability dropped: only the capability inventory tells them apart.
# (setcap -r is not enough: container storage does not record an xattr-only
# change, so the layer keeps the capability. A fresh copy carries no xattrs.)
printf 'FROM %s\nRUN cp --preserve=mode,ownership /usr/bin/newuidmap /usr/bin/newuidmap.new && mv -f /usr/bin/newuidmap.new /usr/bin/newuidmap\n' \
    "$T:1" > "$work/Containerfile.stripcap"
podman build -q --network=none -f "$work/Containerfile.stripcap" -t "$T:stripcap" "$work" >/dev/null
if "$here/rechunk/verify-lossless" "$T:1" "$T:stripcap" > "$work/log5" 2>&1; then
    bad "verify-lossless refuses a dropped file capability"
else
    grep -q 'caps differs' "$work/log5" && ok "verify-lossless refuses a dropped file capability" \
        || bad "verify-lossless refuses a dropped file capability (wrong reason)"
fi

# No getcap in the image: both inventories would be empty and "match". Refused.
# shellcheck disable=SC2016 # expands in the image's RUN, not here
printf 'FROM %s\nRUN while command -v getcap >/dev/null; do rm -f "$(command -v getcap)"; done\n' "$T:1" \
    > "$work/Containerfile.nocap"
podman build -q --network=none -f "$work/Containerfile.nocap" -t "$T:nocap" "$work" >/dev/null
if "$here/rechunk/verify-lossless" "$T:nocap" "$T:nocap" > "$work/log6" 2>&1; then
    bad "verify-lossless refuses an image it cannot read capabilities in"
else
    grep -q 'no getcap in this image' "$work/log6" && ok "verify-lossless refuses an image it cannot read capabilities in" \
        || bad "verify-lossless refuses an image it cannot read capabilities in (wrong reason)"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
