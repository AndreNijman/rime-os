#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-pkg-legacy-ext.sh — the package engine on a machine whose user
#  extension was built before the rename, as /var/lib/extensions/apex-user.raw.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  katana's Steam, and every other on-demand package in the field, lives in an
#  extension named apex-user. systemd merges an image only under the name its
#  own extension-release file carries, and inside that one it is
#  `extension-release.apex-user`, so the file cannot be renamed on disk. The
#  engine now calls its extension rime-user, and before this change it would
#  have looked for rime-user.raw, found nothing merged, and rebuilt at the
#  first boot (rime-sysext-rebuild.service) — a full re-download, offline it
#  fails, and until it succeeds the machine's packages are whatever the old
#  image left. Worse, a rebuild that did succeed would have written rime-user.raw
#  BESIDE apex-user.raw, and systemd would merge both.
#
#  What must hold, and what this asserts against the SHIPPED engine:
#    * apex-user.raw is the current extension: current_payload names it,
#      merged() sees its release file, `rebuild --if-needed` does NOT rebuild,
#      `verify` says it is intact;
#    * the next real rebuild writes rime-user.raw, and apex-user.raw leaves
#      /var/lib/extensions for the rollback slot, under its own name;
#    * `rime pkg rollback` puts it back as apex-user.raw, where it merges again;
#    * a merge that fails restores apex-user.raw, not a renamed copy;
#    * removing every package removes the pre-rename extension too.
#
#  It runs in a throwaway container of the image's own base
#  (quay.io/fedora/fedora-bootc:45, offline), because the engine's paths are
#  readonly constants under /var/lib and /usr/lib and the cases write there.
#  systemd-sysext is a stub that merges by the same rule as the real one: an
#  image is merged only when its file name matches the name inside it.
#
#  Fails both ways: a copy of the engine with the legacy name broken is run
#  over the same first three cases and must fail them.
#
#  Run from anywhere: ./tests/test-rime-pkg-legacy-ext.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")/.." || exit 2

ENGINE=files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }
IMAGE=${RIME_PKG_LEGACY_IMAGE:-quay.io/fedora/fedora-bootc:45}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-pkg-legacy.XXXXXX") || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %-62s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-62s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-62s %s\n' "$1" "$2"; skip=$((skip+1)); }

if ! command -v podman >/dev/null 2>&1; then
    skipped "the legacy-extension cases" "podman is absent"
    echo "── $pass passed, $fail failed, $skip skipped"; exit 0
fi

# ── the probe: runs inside the container, prints PROBE PASS|FAIL <name> ──────
cat > "$WORK/probe.sh" <<'PROBE_EOF'
set -uo pipefail
ENGINE="$1"
L=apex-user   # rime-rename: keep (the pre-rename extension's name)
N=rime-user
EXT=/var/lib/extensions
REL=/usr/lib/extension-release.d
PKG=/var/lib/rime/pkg

p()  { printf 'PROBE PASS %s\n' "$1"; }
f()  { printf 'PROBE FAIL %s -- %s\n' "$1" "$2"; }
is() { if [ "$2" = "$3" ]; then p "$1"; else f "$1" "expected '$2', got '$3'"; fi; }
tf() { if eval "$2"; then p "$1"; else f "$1" "not true: $2"; fi; }

# The stubs: systemd-sysext merges by name, everything post_merge runs is a
# no-op. A marker file makes one refresh fail, for the restore case.
mkdir -p /stub
cat > /stub/systemd-sysext <<'EOF'
#!/bin/bash
[ "${1:-}" = refresh ] || exit 0
rm -f /usr/lib/extension-release.d/extension-release.*
if [ -e /stub/fail-with-new ] && [ -e /var/lib/extensions/rime-user.raw ]; then
    echo "stub: refusing to merge (test)" >&2; exit 1
fi
for f in /var/lib/extensions/*.raw; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .raw)"; inner="$(sed -n 's/^inner=//p' "$f")"
    if [ "$inner" = "$name" ]; then
        : > "/usr/lib/extension-release.d/extension-release.$name"
    else
        echo "stub: $f carries extension-release.$inner, not .$name: not merged" >&2
    fi
done
EOF
for t in ldconfig systemd-sysusers systemd-tmpfiles systemctl udevadm; do
    printf '#!/bin/sh\nexit 0\n' > "/stub/$t"
done
chmod 0755 /stub/*
export PATH="/stub:$PATH"

OSVER="$(. /usr/lib/os-release; printf '%s' "$VERSION_ID")"

legacy_machine() {  # the state an APEX machine arrives with, after the /var move
    rm -rf "$EXT" "$REL" "$PKG" /tmp/REBUILT
    mkdir -p "$EXT" "$REL" "$PKG/rollback"
    printf 'inner=%s\n' "$L" > "$EXT/$L.raw"
    : > "$REL/extension-release.$L"
    printf 'steam\n' > "$PKG/requested"
    jq -n --arg v "$OSVER" --argjson l "$LEVEL" --arg img "$EXT/$L.raw" \
          --arg sha "$(sha256sum "$EXT/$L.raw" | cut -d' ' -f1)" \
          '{requested:["steam"],resolved:["steam-1.0.0.87-1.fc43.x86_64"],os_id:"fedora",
            os_version_id:$v,image:$img,image_sha256:$sha,image_bytes:12,pkg_compat_level:$l}' \
          > "$PKG/state.json"
}
new_payload() { mkdir -p /work; printf 'inner=%s\n' "$N" > /work/$N.raw; printf '%s' /work/$N.raw; }
payloads() { (cd "$1" 2>/dev/null && ls -1 | grep -E '\.raw$|-user$' | sort | tr '\n' ' '); }

set --
# shellcheck disable=SC1090
source "$ENGINE" >/dev/null 2>&1
set +e
LEVEL="$PKG_COMPAT_LEVEL"
# The rebuild the boot service would start, recorded instead of run. The real
# one is kept to be put back: the engine cannot be sourced twice (its
# constants are readonly and it sets -e, so a second source ends this shell).
REAL_REBUILD="$(declare -f rebuild_extension)"
rebuild_extension() { : > /tmp/REBUILT; }

echo "── an APEX machine's extension is the current one ──"
legacy_machine
is "current_payload names apex-user.raw"           "$EXT/$L.raw" "$(current_payload)"
tf "merged() sees extension-release.apex-user"     "merged"
( cmd_rebuild --if-needed ) >/dev/null 2>&1; rc=$?
is "rebuild --if-needed exits 0"                   0 "$rc"
tf "…and rebuilds nothing at boot"                 "[ ! -e /tmp/REBUILT ]"
out="$( ( cmd_verify ) 2>&1)"; rc=$?
is "verify exits 0 on the pre-rename extension"    0 "$rc"
tf "…and calls it intact"                          "grep -q 'extension intact' <<<\"\$out\""
tf "ext_up_to_date: an unchanged set is current"   "ext_up_to_date '$PKG/state.json' 'steam-1.0.0.87-1.fc43.x86_64'"
if [ "${MUTANT:-0}" = 1 ]; then echo "PROBE DONE"; exit 0; fi

echo "── the next rebuild writes rime-user.raw and retires apex-user.raw ──"
( activate_payload "$(new_payload)" ) >/dev/null 2>&1; rc=$?
is "activate exits 0"                              0 "$rc"
is "only rime-user.raw is left to merge"           "$N.raw " "$(payloads "$EXT")"
is "apex-user.raw waits in the rollback slot, under its own name" "$L.raw " "$(payloads "$PKG/rollback")"
tf "the merged extension is rime-user"             "[ -f '$REL/extension-release.$N' ] && [ ! -f '$REL/extension-release.$L' ]"
is "current_payload names rime-user.raw"           "$EXT/$N.raw" "$(current_payload)"

echo "── rollback puts the pre-rename extension back where it merges ──"
( cmd_rollback ) >/dev/null 2>&1; rc=$?
is "rollback exits 0"                              0 "$rc"
is "apex-user.raw is back, under its own name"     "$L.raw " "$(payloads "$EXT")"
tf "…and merges again"                             "[ -f '$REL/extension-release.$L' ]"
is "the rollback slot holds no payload"            "" "$(payloads "$PKG/rollback")"

echo "── two rebuilds later nothing named apex-user is left ──"
( activate_payload "$(new_payload)" ) >/dev/null 2>&1
( activate_payload "$(new_payload)" ) >/dev/null 2>&1
is "extensions: rime-user.raw only"                "$N.raw " "$(payloads "$EXT")"
is "rollback slot: the previous rime-user.raw only" "$N.raw " "$(payloads "$PKG/rollback")"

echo "── a merge that fails restores apex-user.raw, not a renamed copy ──"
legacy_machine; : > /stub/fail-with-new
( activate_payload "$(new_payload)" ) >/dev/null 2>&1; rc=$?
rm -f /stub/fail-with-new
tf "activate reports the failure"                  "[ '$rc' != 0 ]"
is "apex-user.raw is back in place"                "$L.raw " "$(payloads "$EXT")"
tf "…and merged"                                   "[ -f '$REL/extension-release.$L' ]"

echo "── no packages left removes the pre-rename extension too ──"
legacy_machine
eval "$REAL_REBUILD"
( rebuild_extension ) >/dev/null 2>&1; rc=$?
is "an empty set exits 0"                          0 "$rc"
is "no payload is left in /var/lib/extensions"     "" "$(payloads "$EXT")"
tf "nothing is merged"                             "! merged"
echo "PROBE DONE"
PROBE_EOF

run_probe() {  # $1 = engine path inside the container, $2 = MUTANT flag
    podman run --rm --network=none \
        -v "$PWD":/repo:ro,Z -v "$WORK":/host:ro,Z \
        -e MUTANT="${2:-0}" "$IMAGE" bash /host/probe.sh "$1" 2>&1
}

out=$(run_probe "/repo/$ENGINE" 0); prc=$?
if ! grep -q '^PROBE ' <<<"$out"; then
    bad "the container probe ran" "podman exited $prc: $(tail -3 <<<"$out" | tr '\n' ' ')"
else
    # A probe that died half way prints fewer cases and no FAIL: the sentinel
    # is what tells a finished run from a truncated one.
    grep -qx 'PROBE DONE' <<<"$out" || bad "the container probe ran to the end" "it stopped after: $(grep '^PROBE' <<<"$out" | tail -1)"
    while IFS= read -r line; do
        case "$line" in
            "PROBE PASS "*) ok "${line#PROBE PASS }" ;;
            "PROBE FAIL "*) r="${line#PROBE FAIL }"; bad "${r%% -- *}" "${r#* -- }" ;;
            "── "*) echo "$line" ;;
        esac
    done <<<"$out"
fi

echo "── fails both ways: an engine that forgot the old name ──"
sed 's/^readonly LEGACY_EXT_NAME=apex-user/readonly LEGACY_EXT_NAME=no-such-extension/' "$ENGINE" > "$WORK/rime-pkg-mutant"
if cmp -s "$WORK/rime-pkg-mutant" "$ENGINE"; then
    bad "mutant: the legacy name broken" "the sed changed nothing; the mutant tests nothing"
else
    mout=$(run_probe /host/rime-pkg-mutant 1)
    grep -qx 'PROBE DONE' <<<"$mout" || bad "the mutant probe ran to the end" "$(tail -2 <<<"$mout" | tr '\n' ' ')"
    caught=$(grep -c '^PROBE FAIL ' <<<"$mout")
    if [ "$caught" -ge 3 ]; then ok "mutant caught: the legacy name broken ($caught cases fail)"
    else bad "mutant: the legacy name broken" "only $caught cases failed: $(grep '^PROBE' <<<"$mout" | tr '\n' ' ')"; fi
fi

echo
echo "── $pass passed, $fail failed, $skip skipped"
[ "$fail" -eq 0 ]
