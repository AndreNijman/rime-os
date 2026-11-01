#!/usr/bin/env bash
# Measures every number in docs/nixos/closure.md for the checks.x86_64-linux.size.measure.<variant> toplevels:
#   (a) download  = the compressed NAR bytes a machine fetches: one binary cache per compression holds every variant,
#                   and a variant's download is the sum of narinfo FileSize over its closure (= its share of nar/);
#   (b) unpacked  = nix path-info -S (NAR size of the closure);
#   (c) on disk   = the closure copied into a fresh btrfs mounted compress-force=zstd:3, before and after
#                   `nix store optimise` (base.nix sets auto-optimise-store): df used, compsize disk/uncompressed;
#   plus the kernel + initrd the bootloader copies per generation (the ESP floor input).
#
# Run INSIDE the rime-nix container, from the worktree, with the tools from the flake's nixpkgs pin:
#   R=github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325
#   sudo -n podman exec -w /work/<worktree> rime-nix nix shell $R#bash $R#coreutils $R#findutils $R#gawk \
#     $R#gnugrep $R#util-linux $R#btrfs-progs $R#compsize -c bash nix/tests/size-measure.sh /work/scratch/size/run
# PHASES (default "download disk") picks (a)+(b) and/or (c); the two can run at the same time on one <dir>, since
# (a) is CPU-bound (zstd -19, xz) and (c) is I/O-bound. A cache keeps every NAR it already holds, so a later
# download run after a small change compresses only the paths that changed.
# It needs a privileged container (loop devices, mount) and about 12 GB free per cache plus 40 GB sparse per image
# (each image is deleted after its variant). Output: <dir>/download.tsv, <dir>/disk.tsv and, once both hold every
# variant, <dir>/summary.tsv.
set -euo pipefail

dir=${1:?usage: [PHASES="download disk"] size-measure.sh <scratch dir> [variant...]}
shift
variants=("$@")
[ ${#variants[@]} -gt 0 ] || variants=(rime-vm rime-vm-no-gaming rime-vm-gaming-no32 plain plain-gaming plain-gaming-no32)
phases=" ${PHASES:-download disk} "
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
zcache=$dir/cache-zstd19
xcache=$dir/cache-xz

declare -A top
for v in "${variants[@]}"; do
  top[$v]=$(nix build "path:.#checks.x86_64-linux.size.measure.$v" --no-link --print-out-paths)
  nix path-info -r "${top[$v]}" > "$dir/$v.paths"
  echo "$v ${top[$v]} $(wc -l < "$dir/$v.paths") paths"
done

# Sum one narinfo field over a list of store paths.
narinfo_sum() { # <cache dir> <paths file> <field>
  local cache=$1 list=$2 field=$3
  while read -r p; do
    h=${p#/nix/store/}
    echo "$cache/${h%%-*}.narinfo"
  done < "$list" | xargs cat | awk -v f="$field:" '$1 == f { s += $2 } END { printf "%d\n", s }'
}

if [[ $phases == *" download "* ]]; then
  # (a) Copy every variant into both caches. Store paths shared between variants are compressed once.
  for c in "$zcache?compression=zstd&compression-level=19" "$xcache?compression=xz"; do
    start=$(date +%s)
    nix copy --no-check-sigs --to "file://$c" "${top[@]}"
    echo "copied to $c in $(( $(date +%s) - start )) s"
  done

  # The method's own check: over every distinct NAR file the cache's narinfos name (two store paths with identical
  # contents share one nar/ file), the FileSize sum equals du -sb of nar/.
  for c in "$zcache" "$xcache"; do
    find "$c" -maxdepth 1 -name '*.narinfo' \
      -exec awk '$1 == "URL:" { u = $2 } $1 == "FileSize:" { s = $2 } ENDFILE { print u, s }' {} + |
      sort -u | awk '{ n++; t += $2 } END { printf "%d %d\n", n, t }' > "$dir/union.sum"
    read -r nars sum < "$dir/union.sum"
    echo "$c: $nars distinct NARs, FileSize sum $sum, du -sb nar/ $(du -sb "$c/nar" | cut -f1)," \
      "files in nar/ $(find "$c/nar" -type f | wc -l)"
  done

  printf 'variant\tpaths\tnar_bytes\tzstd19_bytes\txz_bytes\tkernel_initrd_bytes\ttoplevel\n' > "$dir/download.tsv"
  for v in "${variants[@]}"; do
    t=${top[$v]}
    nar=$(nix path-info -S --json "$t" | grep -o '"closureSize":[0-9]*' | cut -d: -f2)
    zst=$(narinfo_sum "$zcache" "$dir/$v.paths" FileSize)
    xz=$(narinfo_sum "$xcache" "$dir/$v.paths" FileSize)
    boot=$(( $(stat -L -c %s "$t/kernel") + $(stat -L -c %s "$t/initrd") ))
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$v" "$(wc -l < "$dir/$v.paths")" "$nar" "$zst" "$xz" "$boot" "$t" \
      >> "$dir/download.tsv"
  done
  cat "$dir/download.tsv"
fi

if [[ $phases == *" disk "* ]]; then
  printf 'variant\tdf_used\tcompsize_disk\tcompsize_uncompressed\tdf_used_optimised\tcompsize_disk_optimised\ttoplevel\n' \
    > "$dir/disk.tsv"
  for v in "${variants[@]}"; do
    t=${top[$v]}
    # (c) A fresh btrfs, mounted the way D2/D9 mount @nix.
    img=$dir/btrfs-$v.img
    mnt=$dir/mnt-$v
    rm -f "$img"
    truncate -s 40G "$img"
    mkfs.btrfs -q "$img"
    mkdir -p "$mnt"
    mount -o loop,compress-force=zstd:3 "$img" "$mnt"
    trap 'umount "$mnt" 2>/dev/null || true' EXIT
    nix copy --no-check-sigs --to "local?root=$mnt" "$t"
    sync
    btrfs filesystem sync "$mnt"
    used=$(df -B1 --output=used "$mnt" | tail -1 | tr -d ' ')
    compsize -b "$mnt/nix/store" > "$dir/$v.compsize"
    cdisk=$(awk '$1 == "TOTAL" { print $3 }' "$dir/$v.compsize")
    cunc=$(awk '$1 == "TOTAL" { print $4 }' "$dir/$v.compsize")
    nix store optimise --store "local?root=$mnt" 2>&1 | tail -1
    sync
    btrfs filesystem sync "$mnt"
    used2=$(df -B1 --output=used "$mnt" | tail -1 | tr -d ' ')
    compsize -b "$mnt/nix/store" > "$dir/$v.compsize-optimised"
    cdisk2=$(awk '$1 == "TOTAL" { print $3 }' "$dir/$v.compsize-optimised")
    btrfs filesystem df -b "$mnt" > "$dir/$v.btrfs-df"
    umount "$mnt"
    trap - EXIT
    rm -f "$img"
    rmdir "$mnt"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$v" "$used" "$cdisk" "$cunc" "$used2" "$cdisk2" "$t" >> "$dir/disk.tsv"
    tail -1 "$dir/disk.tsv"
  done
fi

# Join the two tables when both exist and were measured on the same toplevels.
if [ -f "$dir/download.tsv" ] && [ -f "$dir/disk.tsv" ]; then
  awk -F'\t' -v OFS='\t' '
    FNR == 1 { next }
    NR == FNR { d[$1] = $2 OFS $3 OFS $4 OFS $5 OFS $6; t[$1] = $7; next }
    ($1 in d) && t[$1] == $7 { print $1, $2, $3, $4, $5, $6, d[$1] }' \
    "$dir/disk.tsv" "$dir/download.tsv" > "$dir/summary.body"
  { printf 'variant\tpaths\tnar_bytes\tzstd19_bytes\txz_bytes\tkernel_initrd_bytes\tdf_used\tcompsize_disk\tcompsize_uncompressed\tdf_used_optimised\tcompsize_disk_optimised\n'
    cat "$dir/summary.body"; } > "$dir/summary.tsv"
  rm -f "$dir/summary.body"
  cat "$dir/summary.tsv"
fi
