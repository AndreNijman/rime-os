#!/usr/bin/bash
# ok() and bad() never fail, so `cond && ok || bad` is a safe if/else here.
# shellcheck disable=SC2015
# Tests rechunk/assign-components: which files get a layer component, and that
# a bad rules file is refused. Runs against a fake root with a fake ownership
# list, so it needs no rpmdb, no podman and no root — only a filesystem with
# user xattrs (ext4, xfs, btrfs, tmpfs >= 6.6).
set -euo pipefail
export LC_ALL=C

here=$(cd "$(dirname "$0")/.." && pwd)
tool="$here/rechunk/assign-components"
conf_shipped="$here/rechunk/components.conf"
pass=0 fail=0
ok()  { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

base=$(mktemp -d "${TMPDIR:-/var/tmp}/rechunk-test.XXXXXX")
trap 'rm -rf "$base"' EXIT

probe="$base/probe"
: > "$probe"
if ! setfattr -n user.probe -v 1 "$probe" 2>/dev/null; then
    echo "SKIP: $base has no user xattr support" >&2
    exit 77
fi

comp_of() { getfattr --only-values -n user.component "$1" 2>/dev/null || true; }
ival_of() { getfattr --only-values -n user.update-interval "$1" 2>/dev/null || true; }

mkroot() {
    root="$base/root.$1"
    mkdir -p "$root/usr/share/fonts/nerd" "$root/usr/bin" "$root/usr/lib/modules/7.0/extra" \
             "$root/usr/share/rime-shell/src" "$root/etc"
    printf 'font' > "$root/usr/share/fonts/nerd/a.ttf"
    printf 'font-owned' > "$root/usr/share/fonts/nerd/owned.ttf"
    printf 'bin' > "$root/usr/bin/rime-agentd"
    printf 'bin' > "$root/usr/bin/rimed"
    printf 'bin' > "$root/usr/bin/other"
    printf 'ko' > "$root/usr/lib/modules/7.0/extra/nvidia.ko"
    printf 'qml' > "$root/usr/share/rime-shell/src/Main.qml"
    printf 'x' > "$root/etc/hostfile"
    head -c 300000 /dev/zero > "$root/usr/bin/big-unlisted"
    ln -s rimed "$root/usr/bin/rimed-link"
    printf '/usr/share/fonts/nerd/owned.ttf\n/usr/bin/other\n' > "$base/owned.$1"
}

run() { # name conf -> sets out, rc
    out=$(RIME_RECHUNK_OWNED_LIST="$base/owned.$1" "$tool" "$2" "$base/root.$1" 2>&1) && rc=0 || rc=$?
}

# ── 1. the happy path ─────────────────────────────────────────────────────────
mkroot main
cat > "$base/c.main" <<'EOF'
# comment line
rime-nerd-fonts   yearly  /usr/share/fonts/nerd
rime-daemons      daily   /usr/bin/rime-*     # trailing comment
rime-daemons      daily   /usr/bin/rimed
rime-kernel-mods  monthly /usr/lib/modules/*/extra
rime-shell        daily   /usr/share/rime-shell
rime-early        daily   /usr/share/rime-shell/src/Main.qml
rime-gone         weekly  /usr/share/does-not-exist
EOF
run main "$base/c.main"
r=$base/root.main
[ "$rc" = 0 ] && ok "valid rules exit 0" || bad "valid rules exit 0 (rc=$rc: $out)"
[ "$(comp_of "$r/usr/share/fonts/nerd/a.ttf")" = rime-nerd-fonts ] && ok "directory rule tags files under it" || bad "directory rule tags files under it"
[ "$(ival_of "$r/usr/share/fonts/nerd/a.ttf")" = yearly ] && ok "update interval set" || bad "update interval set"
[ -z "$(comp_of "$r/usr/share/fonts/nerd/owned.ttf")" ] && ok "package-owned file left alone" || bad "package-owned file left alone"
[ "$(comp_of "$r/usr/bin/rime-agentd")" = rime-daemons ] && ok "glob rule tags a match" || bad "glob rule tags a match"
[ "$(comp_of "$r/usr/bin/rimed")" = rime-daemons ] && ok "second rule of a component" || bad "second rule of a component"
[ -z "$(comp_of "$r/usr/bin/other")" ] && ok "file no rule names left alone" || bad "file no rule names left alone"
[ "$(comp_of "$r/usr/lib/modules/7.0/extra/nvidia.ko")" = rime-kernel-mods ] && ok "glob in a directory component" || bad "glob in a directory component"
[ "$(comp_of "$r/usr/share/rime-shell/src/Main.qml")" = rime-shell ] && ok "first matching rule wins" || bad "first matching rule wins ($(comp_of "$r/usr/share/rime-shell/src/Main.qml"))"
grep -qx 'component rime-gone files 0 bytes 0' <<<"$out" && ok "missing path reported, not fatal" || bad "missing path reported, not fatal"
grep -qx 'component rime-nerd-fonts files 1 bytes 4' <<<"$out" && ok "per-component count and bytes" || bad "per-component count and bytes: $out"
grep -q 'largest untagged: /usr/bin/big-unlisted' <<<"$out" && ok "untagged payload reported" || bad "untagged payload reported"
grep -qx 'untagged-unpackaged files 2 bytes 300001' <<<"$out" && ok "untagged total excludes owned + tagged" || bad "untagged total: $(grep untagged-unpackaged <<<"$out")"

# ── 2. the glob is expanded under ROOT, never against the running system ─────
mkroot glob
printf 'rime-etc daily /etc/host*\n' > "$base/c.glob"
run glob "$base/c.glob"
[ "$rc" = 0 ] && [ "$(comp_of "$base/root.glob/etc/hostfile")" = rime-etc ] \
    && ok "glob expands under ROOT only" || bad "glob expands under ROOT only (rc=$rc: $out)"

# ── 3. refusals ──────────────────────────────────────────────────────────────
mkroot refuse
for case in \
    "too-few|rime-x daily" \
    "interval|rime-x hourly /usr/bin/rimed" \
    "relative|rime-x daily usr/bin/rimed" \
    "name|rime/x daily /usr/bin/rimed" \
    "conflict|rime-x daily /usr/bin/rimed
rime-x yearly /usr/bin/other"; do
    name=${case%%|*}
    printf '%s\n' "${case#*|}" > "$base/c.$name"
    run refuse "$base/c.$name"
    [ "$rc" = 1 ] && ok "refuses $name" || bad "refuses $name (rc=$rc)"
done

# ── 4. the shipped rules parse ───────────────────────────────────────────────
mkroot shipped
run shipped "$conf_shipped"
[ "$rc" = 0 ] && ok "shipped components.conf parses" || bad "shipped components.conf parses: $out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
