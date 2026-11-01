# rime-kbd-xkb: console keymaps converted from every XKB layout and variant, under the names Fedora's kbd gives them
# (share/keymaps/xkb/<layout>.map.gz and xkb/<layout>-<variant>.map.gz).
#
# Why (PLAN.md D2, R19, S1): bootc's installer turns the chosen XKB layout into a console keymap name through Fedora's
# keymap tree (installer/rime-install console_keymap_for: "<layout>-<variant>" when that file exists, else "<layout>",
# else systemd's kbd-model-map). Fedora's kbd ships these converted maps; nixpkgs' kbd does not (it has
# i386/qwerty/uk, i386/qwertz/sg, ... but no "gb" or "ch"). A migrated machine's host.toml therefore carries names like
# "gb" or "ch" that NixOS's loadkeys cannot find: systemd-vconsole-setup fails, the console keeps the kernel's
# built-in "us" map, and the LUKS passphrase prompt reads different characters from the ones the owner types.
# console.packages puts this tree into /etc/kbd (stage 2) AND into the initrd's /etc/kbd/keymaps
# (nixos/modules/config/console.nix: both are consoleEnv), so every such name loads at the passphrase prompt.
#
# The conversion is Fedora's kbd.spec one: ckbcomp -rules base LAYOUT [VARIANT], gzip; a map that cannot type an
# ASCII "A" (U+0041) is dropped, as Fedora drops it (rhbz#1031848). A name kbd itself already ships (us, de, fr, ...)
# is not generated, so it keeps resolving to kbd's own map: loadkeys would otherwise see two files of one name.
{ lib, runCommand, ckbcomp, xkeyboard_config, kbd, gzip, findutils, gawk, gnugrep, coreutils }:

runCommand "rime-kbd-xkb-${xkeyboard_config.version}"
{
  nativeBuildInputs = [ ckbcomp gzip findutils gawk gnugrep coreutils ];
  meta = {
    description = "Console keymaps converted from XKB layouts, with Fedora kbd's xkb/ names (for host.toml keymaps)";
    platforms = lib.platforms.linux;
  };
}
''
  lst=${xkeyboard_config}/share/X11/xkb/rules/base.lst
  test -f "$lst"
  out_dir=$out/share/keymaps/xkb
  mkdir -p "$out_dir" work

  # Names kbd already ships, in any directory of its tree.
  find ${kbd}/share/keymaps -type f -name '*.map*' -printf '%f\n' | sed -E 's/\.map(\.gz|\.bz2|\.zst)?$//' \
    | sort -u > work/kbd-names

  # "<layout>" and "<layout> <variant>" jobs, from the rules' layout and variant sections.
  awk '/^! layout/{s=1;next} /^!/{s=0} s&&NF{print $1}' "$lst" > work/layouts
  awk '/^! variant/{s=1;next} /^!/{s=0} s&&NF{l=$2; sub(":$","",l); print l" "$1}' "$lst" > work/variants
  test "$(wc -l < work/layouts)" -gt 50
  test "$(wc -l < work/variants)" -gt 200
  cat work/layouts work/variants > work/jobs

  conv() {   # conv LAYOUT [VARIANT]: one keymap, or nothing when ckbcomp cannot convert it
    local name=$1''${2:+-$2}
    if grep -qxF -- "$name" work/kbd-names; then echo "$name" >> work/skipped-kbd; return 0; fi
    if ckbcomp -rules base "$@" > "work/$name.map" 2>/dev/null && grep -q 'U+0041' "work/$name.map"; then
      gzip -9n < "work/$name.map" > "$out_dir/$name.map.gz"
    else
      echo "$name" >> work/dropped
    fi
    rm -f "work/$name.map"
  }
  export -f conv
  export out_dir
  xargs -P "''${NIX_BUILD_CORES:-1}" -L 1 bash -c 'conv "$@"' _ < work/jobs

  n=$(find "$out_dir" -name '*.map.gz' | wc -l)
  echo "rime-kbd-xkb: $n keymaps from $(wc -l < work/jobs) layouts and variants;" \
    "$(cat work/skipped-kbd 2>/dev/null | wc -l) left to kbd's own map of that name;" \
    "$(cat work/dropped 2>/dev/null | wc -l) dropped (no conversion, or no ASCII A): $(sort work/dropped 2>/dev/null | tr '\n' ' ')"
  # The names the keymap gate (checks.keymap-luks) and bootc's installer rows depend on.
  for k in gb ch us-dvorak de-neo; do
    if grep -qxF -- "$k" work/kbd-names; then continue; fi
    test -s "$out_dir/$k.map.gz" || { echo "rime-kbd-xkb: $k.map.gz missing" >&2; exit 1; }
  done
  test "$n" -gt 300
''
