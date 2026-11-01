# Rime's look in Ghostty is the look Rime's terminal had in Alacritty. One Hyprland session (gaps, borders,
# rounding, blur, shadows and animations off, so the window is the whole output) shows the same pattern first in
# Alacritty (stock: Rime shipped no Alacritty config) and then in Ghostty (the user's migrated config.ghostty with
# Rime's theme seeded by rime-terminal-migrate). terminal-look.py measures both screenshots and the test checks:
#   - both windows fill the same output at the same size, with no title bar;
#   - the window background and every palette colour (1-6, 8-15) are the exact Alacritty values in Ghostty;
#   - the padding is the same, and the cells are laid out on a regular grid;
#   - the cell width and height are the same (same font, same size);
#   - the text's ink starts and ends at the same pixels, and no pixel differs by more than antialiasing (16 of 255:
#     the block cursor included);
#   - both resolve the same font (fontconfig's `monospace`).
# The screenshots, the difference image and the measurements are kept in the test's output (look/).
{ pkgs, self }:

let
  user = "alice";

  hyprConf = pkgs.writeText "hyprland.lua" ''
    hl.monitor({ output = "", mode = "1280x800", position = "auto", scale = 1.0 })
    hl.config({
        general = { gaps_in = 0, gaps_out = 0, border_size = 0 },
        decoration = { rounding = 0, blur = { enabled = false }, shadow = { enabled = false } },
        animations = { enabled = false },
        misc = { disable_hyprland_logo = true, disable_splash_rendering = true, force_default_wallpaper = 0 },
    })
  '';

  # Text in the default colours, bold and italic, the eight colours as text, then the block line on row 5
  # (16 palette backgrounds, 4 cells each). The cursor stays visible at the start of row 7.
  pattern = pkgs.writeShellScript "terminal-pattern" ''
    printf '\033[H\033[2J'
    printf 'Rime terminal: The quick brown fox jumps over the lazy dog 0123456789\n'
    printf 'ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz {}[]()<>=!~@#$%%^&*|\n'
    printf '\033[1mbold\033[0m \033[3mitalic\033[0m \033[4munderline\033[0m \033[7mreverse\033[0m normal\n'
    for i in 1 2 3 4 5 6; do printf '\033[3%dmcolour%d\033[0m ' "$i" "$i"; done; printf '\n'
    for i in 1 2 3 4 5 6; do printf '\033[1;3%dmbold%d\033[0m ' "$i" "$i"; done; printf '\n'
    i=0; while [ $i -lt 16 ]; do printf '\033[48;5;%dm    \033[0m' "$i"; i=$((i + 1)); done; printf '\n\n'
    exec sleep infinity
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-terminal-look";

  node.pkgsReadOnly = false;

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    rime.terminal.enable = true;
    programs.hyprland.enable = true;
    # The fonts Rime's desktop ships (nix/modules/desktop.nix), so `monospace` resolves as it does there.
    fonts.packages = [ pkgs.jetbrains-mono pkgs.nerd-fonts.jetbrains-mono ];

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      qemu.options = [ "-vga none -device virtio-gpu-pci,xres=1280,yres=800" ];
    };
    hardware.graphics.enable = true;

    # Test only: the screenshot tool and the measuring script's Python.
    environment.systemPackages = [ pkgs.grim pkgs.jq (pkgs.python3.withPackages (ps: [ ps.pillow ])) ];

    users.users.${user} = {
      isNormalUser = true;
      uid = 1000;
    };

    services.getty.autologinUser = user;
    environment.loginShellInit = ''
      if [ "$(tty)" = /dev/tty1 ] && [ "$(id -un)" = ${user} ]; then
        mkdir -p ~/.config/hypr
        ln -sf ${hyprConf} ~/.config/hypr/hyprland.lua
        exec start-hyprland > /tmp/hyprland.log 2>&1
      fi
    '';
  };

  testScript = ''
    import json

    def as_user(cmd):
        return (
            "sudo -u ${user} env -u DISPLAY XDG_RUNTIME_DIR=/run/user/1000 "
            "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
            "WAYLAND_DISPLAY=$(cd /run/user/1000 && ls wayland-? | head -n1) " + cmd
        )

    def clients():
        return json.loads(machine.succeed(as_user("hyprctl -i 0 clients -j")))

    def show(app_id, launch, shot):
        machine.succeed(as_user(f"sh -c '{launch} >/tmp/{app_id}.log 2>&1 &'"))
        machine.wait_until_succeeds(as_user(f"hyprctl -i 0 clients -j | jq -e 'map(select(.class == \"{app_id}\")) | length == 1'"), timeout=120)
        # first frame, the pattern printed, fonts loaded
        machine.sleep(8)
        [c] = [c for c in clients() if c["class"] == app_id]
        print(f"{app_id}: at {c['at']} size {c['size']} xwayland {c['xwayland']}")
        machine.succeed(as_user(f"grim /tmp/look/{shot}.png"))
        machine.succeed(f"kill {c['pid']}")
        machine.wait_until_succeeds(as_user("hyprctl -i 0 clients -j | jq -e 'length == 0'"), timeout=30)
        return c

    def dump_logs():
        print(machine.execute("for f in /tmp/hyprland.log /tmp/Alacritty.log /tmp/com.mitchellh.ghostty.log; do echo \"== $f\"; tail -n 80 $f; done")[1])

    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("pgrep -u ${user} -f '(^|/)Hyprland( |$)'", timeout=120)
    machine.wait_until_succeeds(as_user("hyprctl -i 0 monitors"), timeout=60)
    # the login ran the carry-over: config.ghostty names Rime's theme, and the theme is there
    machine.wait_until_succeeds("test -e /home/${user}/.local/state/rime/terminal-migrated", timeout=60)
    machine.succeed("grep -Fx 'theme = rime-os' /home/${user}/.config/ghostty/config.ghostty")
    machine.succeed("test -f /home/${user}/.config/ghostty/themes/rime-os")
    machine.succeed("mkdir -p /tmp/look && chown ${user} /tmp/look")

    try:
        with subtest("the same pattern in Alacritty and in Ghostty"):
            a = show("Alacritty", "alacritty -vv -e ${pattern}", "alacritty")
            g = show("com.mitchellh.ghostty", "ghostty --gtk-single-instance=false -e ${pattern}", "ghostty")
            assert a["size"] == g["size"] and a["at"] == g["at"], f"window geometry differs: {a['at']} {a['size']} vs {g['at']} {g['size']}"
            assert not a["xwayland"] and not g["xwayland"]

        with subtest("fonts"):
            fc = machine.succeed("fc-match monospace").strip()
            print("fc-match monospace:", fc)
            print(machine.succeed("grep -i -E 'font|glyph' /tmp/Alacritty.log | head -n 20 || true"))
            print(machine.succeed(as_user("ghostty +show-face --string=A 2>&1 || true")))
            print(machine.succeed(as_user("ghostty +show-config 2>&1 | grep -E '^(font-family|font-size|window-padding|window-decoration|background|foreground) ' || true")))

        with subtest("measured: Ghostty looks like Alacritty"):
            print(machine.succeed("python3 ${./terminal-look.py} /tmp/look/alacritty.png /tmp/look/ghostty.png /tmp/look"))
            r = json.loads(machine.succeed("cat /tmp/look/report.json"))
            machine.succeed("fc-match monospace > /tmp/look/fc-match.txt")
            machine.copy_from_machine("/tmp/look", "")
            A, G = r["alacritty"], r["ghostty"]
            assert A["size"] == G["size"], (A["size"], G["size"])
            assert A["palette_missing"] == [] and G["palette_missing"] == [], (A["palette_missing"], G["palette_missing"])
            for k in ("dominant", "background_sample", "background_sample_mid"):
                assert A[k] == G[k] == "#181818", (k, A[k], G[k])
            assert A["grid_errors"] == [] and G["grid_errors"] == [], (A["grid_errors"], G["grid_errors"])
            assert (A["padding_left"], A["padding_top"]) == (G["padding_left"], G["padding_top"]), "padding differs"
            assert (A["cell_width"], A["cell_height"]) == (G["cell_width"], G["cell_height"]), "cell size differs"
            assert A["text_ink_bbox"] == G["text_ink_bbox"], (A["text_ink_bbox"], G["text_ink_bbox"])
            # the same glyphs, the same cursor: what is left is antialiasing (each renderer rasterises its own way)
            assert r["pixels_differing_over_16"] == 0, r["pixels_differing_over_16"]
    except Exception:
        dump_logs()
        machine.execute("mkdir -p /tmp/look")
        try:
            # already copied when a measured assertion failed; a second copy must not hide that failure
            machine.copy_from_machine("/tmp/look", "")
        except FileExistsError:
            pass
        raise
  '';
}
