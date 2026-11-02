#!/usr/bin/env bash
# delta.md §6.1 (experiment, not shipped): Rime's own kernel built with MODULE_COMPRESS unset ("-d MODULE_COMPRESS"
# in nix/pkgs/kernel/config.nix, reverted). The built module tree (plain .ko) against the same tree with every module
# compressed the way the kernel's modules_install does (scripts/Makefile.modinst: zstd, level 3): the NAR, and what a
# release ships for the path whole (zstd -3 = nix copy's default, -19 = stage since delta.md §8.1, -19 --long=27 = the
# deltas' level). The plain tree measured here had lost its module signatures to the fixup phase's strip (§6.1), so
# both trees are ~1.5 MB smaller than a signed one. A delta of Rime's kernel needs two kernel versions; the weekly
# pair's measurement is modules-uncompressed.sh.
# Inside the rime-nix container:  modules-rime.sh MODULES_OUT WORKDIR
set -euo pipefail
mods=$1 w=$2
rm -rf "$w"; mkdir -p "$w"
echo "tree: $mods, $(find "$mods" -name '*.ko' | wc -l) .ko, $(find "$mods" -name '*.ko.*' | wc -l) compressed"
cp -a "$mods" "$w/zst"; chmod -R u+w "$w/zst"
find "$w/zst" -name '*.ko' -print0 | xargs -0 -P 8 -n 64 zstd -q -T1 --rm
echo "re-compressed copy: $(find "$w/zst" -name '*.ko.zst' | wc -l) .ko.zst"
nix-store --dump "$mods" > "$w/ko.nar"
nix-store --dump "$w/zst" > "$w/zst.nar"
rm -rf "$w/zst"
printf 'tree\tnar\tzstd3\tzstd19\tzstd19_long27\n'
for t in zst ko; do
  n=$w/$t.nar
  printf '%s\t%s\t%s\t%s\t%s\n' "$t" "$(stat -c %s "$n")" "$(zstd -q -3 -T0 -c "$n" | wc -c)" "$(zstd -q -19 -T0 -c "$n" | wc -c)" "$(zstd -q -19 --long=27 -T0 -c "$n" | wc -c)"
done
rm -rf "$w"
