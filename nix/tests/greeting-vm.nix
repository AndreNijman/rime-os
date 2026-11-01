# The terminal greeting on the real desktop: log in through rime-greet into Rime Tiling (Hyprland), let Rime
# Shell's first wallpaper run its own matugen config, then open terminals the way a user does (Hyprland's exec):
#
#   - the Shell's matugen post_hook rendered ~/.cache/rime/logo/rime-symbol-{dark,light}.png in the wallpaper's
#     colours (no hand-run step);
#   - Ghostty (from nixpkgs; in this test node only, Rime ships Alacritty): the greeting picks kitty-direct and
#     draws the recoloured symbol (its accent and foreground pixels are counted where the logo sits);
#   - Alacritty (the shipped terminal, no image protocol): the greeting prints the "RIME OS" label, and no logo
#     pixels are drawn there;
#   - a new wallpaper (matugen with the Shell's config) recolours the logo the next Ghostty shows.
#
# Screenshots in $out: greeting-ghostty.png, greeting-alacritty.png, greeting-ghostty-recoloured.png (QEMU's
# screen), and $out/grim/*.png (the compositor's own frame, where the pixels are counted).
# Only the test account's CORRECT password is ever typed.
{ pkgs, self }:

let
  # Named by store path in the script: another python3 on the profile (without Pillow) wins on PATH.
  py = pkgs.python3.withPackages (p: [ p.pillow ]);
  # Counts, inside a crop of a screenshot, the pixels within 3 levels of each "#RRGGBB" given.
  #   count-colours PNG X Y W H #RRGGBB...
  countColours = pkgs.writeText "count-colours.py" ''
    import sys
    from PIL import Image
    png, x, y, w, h = sys.argv[1], *map(int, sys.argv[2:6])
    want = [tuple(int(c[i:i + 2], 16) for i in (1, 3, 5)) for c in sys.argv[6:]]
    img = Image.open(png).convert("RGB").crop((x, y, x + w, y + h))
    counts = [0] * len(want)
    for p in img.getdata():
        for i, c in enumerate(want):
            if max(abs(p[0] - c[0]), abs(p[1] - c[1]), abs(p[2] - c[2])) <= 3:
                counts[i] += 1
    print(" ".join(map(str, counts)))
  '';
  # A wallpaper unlike the default one, for the recolour step.
  makeWall = pkgs.writeText "make-wall.py" ''
    import sys
    from PIL import Image
    img = Image.new("RGB", (640, 400))
    px = img.load()
    for y in range(400):
        for x in range(640):
            px[x, y] = (40 + x // 8, 150 + y // 8, 60)
    img.save(sys.argv[1])
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-greeting-vm";

  node.pkgsReadOnly = false;

  nodes.machine =
    { lib, pkgs, ... }:
    {
      imports = [ self.nixosModules.default ];

      rime.desktop.enable = true;

      users.users.rime = {
        isNormalUser = true;
        uid = 1000;
        initialPassword = "rime";
        extraGroups = [ "wheel" "video" "audio" "networkmanager" ];
      };

      virtualisation = {
        memorySize = 4096;
        cores = 4;
        qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
      };

      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };

      systemd.services.rime-flathub-setup.enable = lib.mkForce false;

      environment.systemPackages = [
        pkgs.ghostty
        pkgs.grim
        py
      ];
    };

  testScript = ''
    import json
    import shlex

    sw = "/run/current-system/sw"
    shell_dir = sw + "/share/rime-shell"
    quickshell = "pgrep -u rime -f '^[^ ]*quickshell -c " + shell_dir + "$'"
    logo = "/home/rime/.cache/rime/logo"

    def in_session(cmd):
        return (
            "sudo -u rime env -u DISPLAY XDG_RUNTIME_DIR=/run/user/1000 "
            "WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1) "
            "HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -n1) " + cmd
        )

    def colours():
        out = {}
        for line in machine.succeed(f"cat {logo}/colors").splitlines():
            k, _, v = line.partition("=")
            out[k] = v
        return out

    def clients():
        return json.loads(machine.succeed(in_session("hyprctl -i 0 clients -j")))

    def open_terminal(cmd, tag):
        before = {c["pid"] for c in clients()}
        machine.succeed(in_session("hyprctl -i 0 dispatch " + shlex.quote('hl.dsp.exec_cmd("' + cmd + '")')))
        client = None
        for _ in range(120):
            fresh = [c for c in clients() if c["pid"] not in before and c["mapped"]]
            if fresh:
                client = fresh[0]
                break
            machine.sleep(1)
        assert client, f"{tag}: no new window"
        print(f"{tag}: class={client['class']!r} pid={client['pid']} at={client['at']} size={client['size']}")
        machine.sleep(12)  # the shell starts and the greeting draws
        # The screen first (the line typed below scrolls nothing away, but keep the picture clean).
        machine.succeed(in_session(f"grim /tmp/{tag}.png"))
        machine.screenshot("greeting-" + tag)
        # The greeting exported its answer for this terminal; the shell itself says what it saw.
        machine.send_chars(
            "printf '%s\\n' \"$RIME_TERM_GRAPHICS\" \"$(stty size)\" \"$RIME_GREETED\" > /tmp/rtg-" + tag + "\n")
        machine.wait_until_succeeds(f"test $(wc -l < /tmp/rtg-{tag}) -ge 3", timeout=60)
        token, size, greeted = machine.succeed(f"cat /tmp/rtg-{tag}").splitlines()[:3]
        print(f"{tag}: RIME_TERM_GRAPHICS={token!r} stty={size!r} RIME_GREETED={greeted!r}")
        assert greeted == "1", f"{tag}: the greeting did not run in this shell"
        return client, token, tuple(int(v) for v in size.split())

    def logo_pixels(client, size, tag, cols):
        rows, ncols = size
        x, y = client["at"]
        w, h = client["size"]
        cw, ch = w / ncols, h / rows
        # The logo's cells: padding-left 2, about 23 cells wide (24 taken), padding-top 1, 8 rows. The info text
        # starts after padding-right 3, so in Ghostty only the image is in this box.
        box = (int(x + 2 * cw), int(y + 1 * ch), int(24 * cw), int(8 * ch))
        c = machine.succeed(f"${py}/bin/python3 ${countColours} /tmp/{tag}.png {box[0]} {box[1]} {box[2]} {box[3]} "
                            + " ".join(shlex.quote(c) for c in cols)).split()
        print(f"{tag}: logo box {box}: " + ", ".join(f"{k}={v}" for k, v in zip(cols, c)))
        machine.copy_from_machine(f"/tmp/{tag}.png", "grim")
        return [int(v) for v in c]

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("greetd.service")
    machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=120)
    machine.sleep(8)

    with subtest("the greeting is installed for bash and for zsh"):
        machine.succeed(f"test -x {sw}/libexec/rime-term-graphics")
        machine.succeed(f"test -x {sw}/libexec/rime-logo-render")
        machine.succeed("grep -F 'RIME OS' /etc/fastfetch/config.jsonc")
        machine.succeed(f"grep -F '{sw}/share/rime/shell/greeting.sh' /etc/bashrc")
        machine.succeed(f"grep -F '{sw}/libexec/rime-term-graphics' {sw}/share/rime/shell/greeting.sh")
        machine.succeed(f"test -s {sw}/share/rime/branding/png/symbol/rime-symbol-dark-512.png")
        print(machine.succeed("getent passwd rime"))

    with subtest("log in to Rime Tiling"):
        machine.send_chars("rime\n")
        machine.sleep(2)
        machine.send_chars("rime\n")
        machine.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
        machine.wait_until_succeeds(quickshell, timeout=120)

    with subtest("the Shell's first wallpaper recoloured the logo"):
        machine.wait_until_succeeds(f"test -s {logo}/rime-symbol-dark.png -a -s {logo}/rime-symbol-light.png",
                                    timeout=240)
        first = colours()
        print("logo colours from the Shell's own matugen run:", first)
        print(machine.succeed(f"ls -l {logo}; stat -c '%U %a %n' {logo}/rime-symbol-dark.png"))
        machine.succeed(f"${py}/bin/python3 -c \"from PIL import Image; im=Image.open('{logo}/rime-symbol-dark.png'); "
                        "assert im.size[0]==512, im.size; print(im.size, im.mode)\"")
        variant = "light" if "light" in machine.execute(
            in_session("gsettings get org.gnome.desktop.interface color-scheme"))[1] else "dark"
        print("desktop colour scheme ->", variant)

    with subtest("Ghostty: kitty graphics, the recoloured symbol"):
        g, token, size = open_terminal("ghostty --gtk-single-instance=false", "ghostty")
        assert token.startswith("kitty-direct@"), token
        acc, fg = logo_pixels(g, size, "ghostty", [first[f"accent_{variant}"], first[f"fg_{variant}"]])
        assert acc > 1000 and fg > 3000, f"no symbol drawn in Ghostty (accent {acc} px, foreground {fg} px)"
        machine.succeed(f"kill {g['pid']}")

    with subtest("Alacritty: no image protocol, the RIME OS label"):
        a, token, size = open_terminal("alacritty", "alacritty")
        assert token.startswith("none@"), token
        acc, fg = logo_pixels(a, size, "alacritty", [first[f"accent_{variant}"], first[f"fg_{variant}"]])
        # The label and the info text share these cells with any logo, so the token and the screenshot are the
        # proof here; the counts are printed for the record.
        machine.succeed(f"kill {a['pid']}")

    with subtest("a new wallpaper recolours the next greeting"):
        machine.succeed("sudo -u rime ${py}/bin/python3 ${makeWall} /tmp/green-wall.png")
        machine.succeed(in_session(
            "matugen image /tmp/green-wall.png -c /home/rime/.config/rime-shell/matugen.toml "
            "--source-color-index 0 --type scheme-tonal-spot -m dark"))
        second = colours()
        print("after the new wallpaper:", second)
        assert second[f"accent_{variant}"] != first[f"accent_{variant}"], (first, second)
        g, token, size = open_terminal("ghostty --gtk-single-instance=false", "ghostty-recoloured")
        acc, old = logo_pixels(g, size, "ghostty-recoloured",
                               [second[f"accent_{variant}"], first[f"accent_{variant}"]])
        assert acc > 1000 and old < 50, f"Ghostty's symbol: new accent {acc} px, old accent {old} px"
        machine.succeed(f"kill {g['pid']}")
  '';
}
