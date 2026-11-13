# The terminal greeting, without a VM (greeting-vm is the VM half). Each script is in nix/tests/greeting/:
#
#  1. term_graphics_test.py   rime-term-graphics on a pty acting as the terminal: which protocol for a local
#                             Ghostty/kitty, Alacritty, WezTerm, SSH, tmux (passthrough on/off), GNU screen, the
#                             console, a kitty/sixel/silent answerer; the cache; the logo's width from the cell size;
#                             the terminal's modes restored after a timeout and after Ctrl-C; a silent local terminal
#                             costs <= 50 ms; over SSH a reply at a 120 ms round trip is read, none left for the shell.
#  2. greeting_table_test.py  greeting.sh sourced by interactive bash, zsh and dash: which image (recoloured cache,
#                             shipped, light/dark, SSH), which fastfetch arguments (image: width AND height; label:
#                             "RIME OS" in the accent or Rime Blue, never ASCII art), quiet when it must be, the
#                             caller's positional parameters untouched.
#  3. logo_render_test.py     the BUILT Rime Shell's matugen config on two wallpapers: rime-logo-render's PNGs carry the
#                             wallpaper's accent over the whole mark; a bad colours file keeps the old logo.
#  4. POSIX and lint: greeting.sh parses in sh/bash/zsh/dash and has no bash-only test brackets (Containerfile.base
#     asserts the same); shellcheck on greeting.sh and rime-logo-render.
#  5. Mutants, each must make its test FAIL: rime-term-graphics that never restores the terminal; greeting.sh without
#     --logo-height (fastfetch then clears the screen and the scrollback); rime-logo-render with the colours swapped;
#     rime-term-graphics with the local 35 ms deadline over SSH too (a late reply then reaches the prompt).
{ pkgs, self }:

let
  greeting = pkgs.rime.rime-greeting;
  branding = pkgs.rime.rime-branding;
  shellDir = "${pkgs.rime.rime-shell}/share/rime-shell";
  python = pkgs.python3.withPackages (p: [ p.pillow ]);
  shellDirFiles = ../../files/desktop/shell;
  tests = ./greeting;
in
pkgs.runCommandCC "rime-greeting-check"
  {
    nativeBuildInputs = [ python pkgs.bash pkgs.zsh pkgs.dash pkgs.shellcheck pkgs.matugen pkgs.resvg ];
  }
  ''
    set -eu
    # The greeting NixOS ships: the bootc file with nix/branding/ridge.patch (rime-ridge-files).
    cp ${pkgs.rime.rime-ridge-files}/share/rime/shell/greeting.sh rime-greeting.sh
    cp ${shellDirFiles}/rime-term-graphics.c ${shellDirFiles}/rime-logo-render .
    chmod u+w rime-greeting.sh rime-term-graphics.c rime-logo-render
    tg=${greeting}/libexec/rime-term-graphics
    render=${greeting}/libexec/rime-logo-render
    template=${branding}/share/rime/branding/templates/rime-symbol.svg.in
    shells="${pkgs.bash}/bin/bash ${pkgs.zsh}/bin/zsh ${pkgs.dash}/bin/dash"

    echo "== 1. rime-term-graphics"
    python3 ${tests}/term_graphics_test.py "$tg"

    echo "== 2. greeting.sh"
    python3 ${tests}/greeting_table_test.py rime-greeting.sh $shells

    echo "== 3. recoloured logo"
    python3 ${tests}/logo_render_test.py ${pkgs.matugen}/bin/matugen "$render" ${pkgs.resvg}/bin/resvg \
      "$template" ${shellDir}

    echo "== 4. POSIX + lint"
    dash -n rime-greeting.sh
    bash -n rime-greeting.sh
    zsh -n rime-greeting.sh
    if grep -n '\[\[' rime-greeting.sh; then echo "FAIL: bash-only test brackets in greeting.sh"; exit 1; fi
    shellcheck rime-greeting.sh
    shellcheck rime-logo-render
    shellcheck "$render"

    echo "== 5. mutants"
    # rime-term-graphics: the terminal's modes are never put back (raw mode survives into the shell).
    sed 's/tcsetattr(tty_fd, TCSANOW, &saved_termios);/(void)saved_termios;/' rime-term-graphics.c > m1.c
    if cmp -s rime-term-graphics.c m1.c; then echo "mutant 1 did not apply"; exit 1; fi
    $CC -std=c11 -O2 -o m1 m1.c
    python3 ${tests}/term_graphics_test.py ./m1 --expect-fail

    # greeting.sh: the image logo without a height.
    sed 's/ --logo-height "''${_rime_rows}"//' rime-greeting.sh > m2.sh
    if cmp -s rime-greeting.sh m2.sh; then echo "mutant 2 did not apply"; exit 1; fi
    python3 ${tests}/greeting_table_test.py m2.sh ${pkgs.bash}/bin/bash --expect-fail

    # rime-logo-render: accent and foreground swapped.
    sed 's/^render dark "$accent_dark" "$fg_dark"/render dark "$fg_dark" "$accent_dark"/' "$render" > m3
    if cmp -s "$render" m3; then echo "mutant 3 did not apply"; exit 1; fi
    chmod +x m3
    python3 ${tests}/logo_render_test.py ${pkgs.matugen}/bin/matugen "$PWD/m3" ${pkgs.resvg}/bin/resvg \
      "$template" ${shellDir} --expect-fail

    # rime-term-graphics: the LAN-sized deadline over SSH as well.
    sed 's/return is_remote() ? 300 : 35;/return 35;/' rime-term-graphics.c > m4.c
    if cmp -s rime-term-graphics.c m4.c; then echo "mutant 4 did not apply"; exit 1; fi
    $CC -std=c11 -O2 -o m4 m4.c
    python3 ${tests}/term_graphics_test.py ./m4 --expect-fail

    echo "rime-greeting: all checks passed, 4 mutants caught"
    touch $out
  ''
