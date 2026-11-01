# The TRANSLATOR GATE (docs/nixos/p2-gates.md P1, PLAN.md 4.5 + D5): `rime pkg translate` reports every package a
# bootc Rime machine installed, item by item, against the catalogue rime-vm is built with. The judge
# (pkg-translate-check.py) counts the records itself from the tree and fails on any record with no row, any row with
# no record, and any row with no outcome or reason.
#
#   checks.pkg-translate          a synthetic bootc tree with every kind of record (the requested list with feature,
#                                 Flatpak, class 2, pending, per-user, refused, local:/deb:/github: and unknown names;
#                                 state.json-only and orphan records; COPR keys and repositories; an AppImage; system and
#                                 user Flatpaks; the system extension; Claude Desktop's hand install; npm packages;
#                                 ~/.local/opt; a capsule; a blueprint). Pins each outcome, then `--write` on a copy:
#                                 the NixOS requested list, the gaming switch and requested.bootc.
#
#   checks.pkg-translate.katana   the same judge on katana's real records. They are NEVER in the repository or the
#                                 store; they reach it at RUN time:
#
#     nix build path:.#checks.x86_64-linux.pkg-translate.katana
#     sudo RIME_KATANA_STATE=/var/lab/rime-nix/artifacts/katana-state \
#          RIME_KATANA_PKG=/var/lab/rime-nix/artifacts/d5/katana-pkg ./result
#
#   (the D2 snapshot omits /var/lib/rime/pkg; RIME_KATANA_PKG is the package-record sidecar taken read-only from
#   katana for this gate). Without both it prints why it is skipped and exits 0. It reads both trees through a
#   symlink farm in a private directory, writes nothing to them, and hashes them before and after.
{ pkgs, self }:

let
  vm = self.nixosConfigurations.rime-vm.config;
  catalogue = pkgs.writeText "rime-catalogue.json" vm.environment.etc."rime/catalogue.json".text;
  rime = pkgs.rime.rimed;
  python = pkgs.python3;
  judge = ./pkg-translate-check.py;

  # What each synthetic record must become (outcomes of translate.rs's Outcome enum).
  expect = builtins.toJSON {
    steam = "feature";
    gamescope = "feature";
    "steam-devices" = "feature";
    blender = "carried"; # already a system Flatpak on the machine
    chromium = "flatpak";
    wine = "kept";
    "helium-bin" = "pending";
    "libgcc.i686" = "refused";
    "glib2.i686" = "refused";
    "local:mytool" = "refused";
    "deb:claude-desktop" = "pending";
    "github:owner/thing" = "refused";
    kernel = "refused";
    notathing = "lookup";
    "claude-code" = "kept";
    ghost = "refused";
    imput = "dropped";
    "_copr:copr.fedorainfracloud.org:imput:helium.repo" = "dropped";
    "Thing.AppImage" = "carried";
    "rime-user.raw" = "dropped";
    "claude-desktop" = "pending";
    "md.obsidian.Obsidian" = "flatpak";
    neovim = "kept";
    "com.spotify.Client" = "carried";
    "andre: npm @anthropic-ai/claude-code" = "carried";
    "andre: npm prettier" = "carried";
    "andre: ~/.local/opt/equibop" = "carried";
    "andre: [gaming]" = "carried";
    "andre: fedora.json" = "carried";
  };

  fixture = ''
    mk() { mkdir -p "$(dirname "$1")"; printf '%b' "$2" > "$1"; }
    t=$1
    mk $t/var/lib/rime/pkg/requested 'steam\nblender\nchromium\nwine\nhelium-bin\nlibgcc.i686\nglib2.i686\ngamescope\nsteam-devices\nlocal:mytool\ndeb:claude-desktop\ngithub:owner/thing\nnotathing\nclaude-code\n# a comment\n\nsteam\n'
    mk $t/var/lib/rime/pkg/state.json '{"requested":["steam","kernel"],"resolved":["x"],"local_files":["mytool","ghost"],"deb_files":["claude-desktop"],"vendor_verified":["claude-desktop"],"unsigned_accepted":["mytool"],"github_sources":{"mytool":"owner/mytool"}}'
    mk $t/var/lib/rime/pkg/local/mytool.rpm ""
    mk $t/var/lib/rime/pkg/copr-keys/imput/helium.gpg ""
    mk $t/var/lib/rime/pkg/etc.list 'chromium/chromium.conf\n'
    mk $t/var/lib/rime/pkg/var.list '/var/lib/x\n'
    mk $t/var/lib/rime/pkg/rollback/state.json '{"requested":["steam","oldthing"]}'
    mk $t/etc/yum.repos.d/_copr:copr.fedorainfracloud.org:imput:helium.repo ""
    mk $t/etc/yum.repos.d/fedora.repo ""
    mk $t/etc/yum.repos.d/README ""
    mk $t/var/lib/rime/appimage/Thing.AppImage ""
    mk $t/var/lib/rime/appimage/Thing.trust ""
    mk $t/var/lib/rime/appimage/Gone.json "{}"
    mkdir -p $t/var/lib/flatpak/app/org.blender.Blender $t/var/lib/flatpak/app/com.spotify.Client
    mk $t/var/lib/extensions/rime-user.raw ""
    mkdir -p $t/var/usrlocal/lib/claude-desktop $t/var/usrlocal/lib/rime-appimage/Thing $t/var/usrlocal/lib/rime-appimage/Old $t/var/usrlocal/lib/somelib
    h=$t/var/home/andre
    mk $h/.local/lib/node_modules/@anthropic-ai/claude-code/package.json "{}"
    mk $h/.local/lib/node_modules/prettier/package.json "{}"
    mk $h/.local/lib/node_modules/.package-lock.json "{}"
    mkdir -p $h/.local/opt/equibop $h/.local/opt/ollama $h/.local/share/flatpak/app/org.gimp.GIMP
    mk $h/.local/share/rime/env/fedora.json "{}"
    mk $h/.config/rime/blueprint.toml '[gaming]\nenabled = true\n[apps]\ninstall = ["neovim", "md.obsidian.Obsidian"]\n[agent]\ndefault = "claude"\n'
    ln -s var/home $t/home
  '';

  test = pkgs.runCommand "rime-pkg-translate" { nativeBuildInputs = [ python pkgs.jq ]; } ''
    set -eu
    make_fixture() { ${fixture} }
    make_fixture $TMPDIR/tree
    python3 ${judge} --rime ${rime}/bin/rime --catalogue ${catalogue} --root $TMPDIR/tree \
      --expect '${expect}' --report $TMPDIR/report.json

    # Unknown sources are reported absent, not invented: an empty root has no rows.
    mkdir $TMPDIR/empty
    python3 ${judge} --rime ${rime}/bin/rime --catalogue ${catalogue} --root $TMPDIR/empty
    test "$(${rime}/bin/rime pkg translate --root $TMPDIR/empty --catalogue ${catalogue} --json | jq '.rows | length')" = 0

    # --write: the NixOS records, under the root.
    cp -a $TMPDIR/tree $TMPDIR/w
    ${rime}/bin/rime pkg translate --root $TMPDIR/w --catalogue ${catalogue} --write > /dev/null
    diff <(jq -r '.requested_after[]' $TMPDIR/report.json) $TMPDIR/w/var/lib/rime/pkg/requested
    grep -x local:mytool $TMPDIR/w/var/lib/rime/pkg/requested.bootc
    test "$(cat $TMPDIR/w/var/lib/rime/switch/rime-feature@gaming.target.on)" = "$(printf 'steam\ngamescope\nsteam-devices')"
    jq -e '.flatpak_install == ["org.chromium.Chromium", "md.obsidian.Obsidian"]' $TMPDIR/w/var/lib/rime/pkg/translate.json
    cmp $TMPDIR/tree/var/lib/rime/pkg/requested $TMPDIR/w/var/lib/rime/pkg/requested.bootc
    mkdir -p $out
    cp $TMPDIR/report.json $out/
  '';

  katana = pkgs.writeShellScript "rime-pkg-translate-katana" ''
    set -eu
    state=''${RIME_KATANA_STATE:-} sidecar=''${RIME_KATANA_PKG:-}
    if [ -z "$state" ] || [ -z "$sidecar" ]; then
      echo "SKIP rime-pkg-translate-katana: RIME_KATANA_STATE and RIME_KATANA_PKG must both be set (katana's records are never in the store or the repository; see nix/tests/pkg-translate.nix)"
      exit 0
    fi
    test -f "$sidecar/var/lib/rime/pkg/requested" || { echo "RIME_KATANA_PKG=$sidecar has no var/lib/rime/pkg/requested" >&2; exit 1; }
    digest() { (cd "$1" && find . -printf '%p %m %s %T@ %y\n' | LC_ALL=C sort && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r ${pkgs.coreutils}/bin/sha256sum) | ${pkgs.coreutils}/bin/sha256sum | cut -d' ' -f1; }
    before_state=$(digest "$state") before_pkg=$(digest "$sidecar")
    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' EXIT
    # One tree: the snapshot, then the sidecar's records over it (symlinks: nothing is copied or written).
    mkdir "$tmp/root"
    ${pkgs.coreutils}/bin/cp -rs "$state/." "$tmp/root/"
    ${pkgs.coreutils}/bin/cp -rs --remove-destination "$sidecar/." "$tmp/root/"
    rc=0
    ${python}/bin/python3 ${judge} --rime ${rime}/bin/rime --catalogue ${catalogue} --root "$tmp/root" \
      --report "''${RIME_TRANSLATE_REPORT:-$tmp/report.json}" || rc=$?
    test "$(digest "$state")" = "$before_state" || { echo "FAIL: the snapshot changed" >&2; rc=1; }
    test "$(digest "$sidecar")" = "$before_pkg" || { echo "FAIL: the sidecar changed" >&2; rc=1; }
    echo "snapshot digest $before_state, sidecar digest $before_pkg (unchanged unless FAIL above)"
    exit $rc
  '';
in
{
  pkg-translate = test.overrideAttrs (old: { passthru = (old.passthru or { }) // { inherit katana; }; });
}
