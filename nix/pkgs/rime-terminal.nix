# Rime's terminal: Ghostty's Rime theme and the one-time Alacritty -> Ghostty carry-over (files/desktop/ghostty).
#
#   libexec/rime-terminal-migrate   run once per user at graphical login by nix/modules/terminal.nix
#   share/rime/ghostty/rime-os      the theme it seeds into ~/.config/ghostty/themes (Alacritty's stock look;
#                                   Rime Shell renders over it with wallpaper colours)
#
# checkPhase proves, without a VM:
#   - the unit tests (exact output for Andre's two real Alacritty configs and synthetic ones for every schema
#     generation, imports, the never-overwrite and run-once rules) and every mutant of tests/mutants.py dies;
#   - Ghostty itself accepts every line the converter writes, the theme, and a whole migrated config that names
#     the theme (`ghostty +validate-config`, the Ghostty Rime ships);
#   - the theme's static block is the same in the fallback and in Rime Shell's matugen template, so the look does
#     not change when the Shell renders over the fallback.
{
  lib,
  stdenvNoCC,
  python3,
  ghostty,
  rime-shell-src,
}:

let
  root = ../..;
  python = python3.withPackages (ps: [ ps.pyyaml ]); # pyyaml: Alacritty <= 0.12 configs were YAML
  shellTemplate = "${rime-shell-src}/src/config/ghostty-colors.template";
in
stdenvNoCC.mkDerivation {
  pname = "rime-terminal";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = root + "/files/desktop/ghostty";
  };

  nativeBuildInputs = [ python ];
  buildInputs = [ python ];
  nativeCheckInputs = [ ghostty ];
  doCheck = true;
  dontBuild = true;

  checkPhase = ''
    runHook preCheck
    cd files/desktop/ghostty
    export HOME=$TMPDIR/home && mkdir -p $HOME

    python3 -m unittest discover -s tests -v
    python3 tests/mutants.py

    validate() {
      local msg  # not "out": that is the derivation's output path
      if ! msg="$(ghostty +validate-config --config-file="$1" 2>&1)" || [ -n "$msg" ]; then
        echo "FAIL: Ghostty rejects $2:"; echo "$msg"; exit 1
      fi
      echo "ghostty accepts $2"
    }
    # every line the converter writes, fixture by fixture
    for d in tests/fixtures/*/; do
      python3 -c 'import sys; sys.stdout.write(open(sys.argv[1]).read().split("----\n", 1)[0])' \
        "$d/expected.txt" > $TMPDIR/lines.conf
      validate $TMPDIR/lines.conf "the lines converted from $d"
    done
    validate rime-os "the rime-os theme"
    # a whole migrated home: config.ghostty names the theme, and Ghostty must find it
    mkdir -p $HOME/.config/alacritty
    cp tests/fixtures/full/alacritty.toml $HOME/.config/alacritty/
    python3 rime-terminal-migrate --theme rime-os
    test -f $HOME/.config/ghostty/themes/rime-os
    validate $HOME/.config/ghostty/config.ghostty "a migrated config.ghostty with theme = rime-os"
    # a missing theme must be an error, or the check above proves nothing
    mv $HOME/.config/ghostty/themes $HOME/.config/ghostty/themes.away
    if ghostty +validate-config --config-file=$HOME/.config/ghostty/config.ghostty >/dev/null 2>&1; then
      echo "FAIL: +validate-config does not notice a missing theme"; exit 1
    fi

    # the static block: fallback == Rime Shell's template
    static() { python3 -c '
    import sys
    t = open(sys.argv[1]).read()
    if "# rime-static-begin\n" not in t:
        sys.exit(3)
    sys.stdout.write(t.split("# rime-static-begin\n", 1)[1].split("# rime-static-end\n", 1)[0])
    ' "$1"; }
    static rime-os > $TMPDIR/fallback.static
    if static ${shellTemplate} > $TMPDIR/shell.static; then
      if ! cmp -s $TMPDIR/fallback.static $TMPDIR/shell.static; then
        echo "FAIL: the theme's static block differs between rime-os and Rime Shell's ghostty template:"
        diff $TMPDIR/fallback.static $TMPDIR/shell.static || true
        exit 1
      fi
      echo "static block identical in the fallback and Rime Shell's template"
    else
      echo "NOTE: this Rime Shell's ghostty template has no static block (older Shell): comparison skipped"
    fi
    cd - >/dev/null
    runHook postCheck
  '';

  installPhase = ''
    runHook preInstall
    install -Dm0755 files/desktop/ghostty/rime-terminal-migrate $out/libexec/rime-terminal-migrate
    install -Dm0644 files/desktop/ghostty/rime-os $out/share/rime/ghostty/rime-os
    patchShebangs --host $out/libexec/rime-terminal-migrate
    head -n1 $out/libexec/rime-terminal-migrate | grep -F '${python}/bin/python'
    runHook postInstall
  '';

  meta = {
    description = "Rime's Ghostty theme and the one-time Alacritty to Ghostty settings carry-over";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
