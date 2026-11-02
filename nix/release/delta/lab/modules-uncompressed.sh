#!/usr/bin/env bash
# delta.md §6.1: what the weekly kernel-modules delta would be with CONFIG_MODULE_COMPRESS_NONE.
# The weekly pair's module trees (nixpkgs linux-6.18.58/.59-modules, .ko.xz) are copied and every module
# decompressed in place: the tree a MODULE_COMPRESS_NONE build installs (same objects, same signatures, .ko).
# Then the NAR, its full size at zstd -3 / -19 --long=27, and the delta at -19 --long=27 --patch-from, for the tree
# as shipped (.ko.xz) and decompressed (.ko). Inside the rime-nix container:  modules-uncompressed.sh OLD NEW WORKDIR
set -euo pipefail
old=$1 new=$2 w=$3
rm -rf "$w"; mkdir -p "$w"
for side in old new; do
  src=${!side}
  cp -a "$src" "$w/$side-ko"
  chmod -R u+w "$w/$side-ko"
  n=$(find "$w/$side-ko" -name '*.ko.xz' | wc -l)
  find "$w/$side-ko" -name '*.ko.xz' -print0 | xargs -0 -P 8 -n 64 xz -d
  echo "$side: $src, $n modules decompressed, $(find "$w/$side-ko" -name '*.ko.xz' | wc -l) left"
  nix-store --dump "$src" > "$w/$side-xz.nar"
  nix-store --dump "$w/$side-ko" > "$w/$side-ko.nar"
  rm -rf "$w/$side-ko"
done
wlog() { python3 -c 'import sys; print(max(27, max(int(a) for a in sys.argv[1:]).bit_length()))' "$@"; }
printf 'tree\tnar\tzstd3\tzstd19_long27\tdelta\twindow_log\n'
for t in xz ko; do
  o=$w/old-$t.nar n=$w/new-$t.nar
  nar=$(stat -c %s "$n")
  z3=$(zstd -q -3 -T0 -c "$n" | wc -c)
  z19=$(zstd -q -19 --long=27 -T0 -c "$n" | wc -c)
  wl=$(wlog "$(stat -c %s "$o")" "$nar")
  zstd -q -19 --long=27 -T1 --patch-from="$o" -c "$n" > "$w/$t.delta"
  zstd -q -d --long="$wl" --patch-from="$o" -c "$w/$t.delta" | cmp - "$n"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$t" "$nar" "$z3" "$z19" "$(stat -c %s "$w/$t.delta")" "$wl"
done
du -sb "$w"/*.nar
rm -rf "$w"
