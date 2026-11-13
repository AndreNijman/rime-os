# The Origin mark on Rime's OS surfaces (nix/modules/branding.nix, docs/branding.md "Where the mark shows").
#
# branding-surfaces-static (build-time, no VM):
#   1. the greeter's logo asset is what make-logo-assets.sh renders from the brand template (pixels), and the five
#      circles GreetSurface.qml draws are the template's own;
#   2. the 25 splash themes keep the brand rules (branding-surfaces-themes.py: only the mark changes colour, every
#      image exists with transparency, one script, the engine rules);
#   3. rime-plymouth-accent picks rime-os-accent-NN for a coloured accent and the default rime-os for none or grey,
#      and the initrd picker (files/dracut/rime-plymouth-accent/rime-plymouth-theme) starts the theme the
#      credential names, against the real theme set.
#
# branding-surfaces (VM): a machine with the splash on boots into it and stays there (plymouth-quit is held back), so
#   the screenshots are the boot splash itself: plymouth-default.png (rime-os), plymouth-password.png (the passphrase
#   prompt with three bullets), then rime-plymouth-accent writes the owner's accent to a real ESP and the NEXT boot's
#   initrd starts plymouth-accent.png in rime-os-accent-00. os-release, the icons, the Plymouth logo and the initrd's
#   theme set are checked on the running system. A second machine with the desktop shows the greeter in Rime Blue
#   (greeter-default.png) and in a user's published accent (greeter-accent.png). Every one of these screenshots is read
#   back (mark()): the mark must be on screen in the colour named, next to the off-white wordmark.
{ pkgs, self }:

let
  lib = pkgs.lib;
  files = ../../files;
  accentScript = files + "/system/libexec/rime-plymouth-accent";
  initrdPicker = files + "/dracut/rime-plymouth-accent/rime-plymouth-theme";
  ridgeScript = files + "/branding/plymouth/ridge/rime-os.script";
  branding = pkgs.rime.rime-branding;
  themes = pkgs.rime.rime-plymouth;
  python = pkgs.python3.withPackages (p: [ p.pillow ]);
  # The NixOS greeter tree (files/desktop/rime-greet keeps the bootc image's spark).
  greet = "${pkgs.rime.rime-ridge-files}/share/rime-greet";
in
{
  branding-surfaces-static =
    pkgs.runCommand "rime-branding-surfaces-static"
      {
        # Pillow 12 warns that getdata() goes away in Pillow 14; the pinned Pillow has it.
        PYTHONWARNINGS = "ignore::DeprecationWarning";
        nativeBuildInputs = [
          python
          pkgs.resvg
          pkgs.shellcheck
          pkgs.gawk
        ];
      }
      ''
        b=${branding}/share/rime/branding
        T=${themes}/share/plymouth/themes

        echo "== 1. the greeter's logo"
        sh ${greet}/make-logo-assets.sh $b/templates $PWD/greet
        python3 - ${greet}/assets/ridge-lockup-base.png $PWD/greet/ridge-lockup-base.png <<'PY'
        import sys
        from PIL import Image, ImageChops
        a, b = (Image.open(p) for p in sys.argv[1:3])
        assert a.mode == "RGBA" and b.mode == "RGBA", (a.mode, b.mode)
        assert a.size == b.size, f"committed {a.size}, rendered {b.size}: rerun make-logo-assets.sh"
        assert ImageChops.difference(a, b).getbbox() is None, "assets/ridge-lockup-base.png drifted from the template: rerun make-logo-assets.sh"
        assert min(a.getchannel("A").getdata()) == 0, "the greeter logo has no transparency"
        bad = sum(1 for (r, g, bl, al) in a.getdata() if al >= 64 and (max(r, g, bl) - min(r, g, bl) > 12 or min(r, g, bl) < 200))
        assert bad == 0, f"{bad} coloured pixels in the neutral lockup: the mark must be drawn by the greeter, not baked in"
        print("greeter logo: reproducible, transparent, neutral", a.size)
        PY
        shellcheck ${greet}/make-logo-assets.sh
        python3 - $b/templates/rime-lockup-horizontal.svg.in ${greet}/GreetSurface.qml <<'PY'
        import re, sys
        svg, qml = (open(p).read() for p in sys.argv[1:3])
        assert 'viewBox="0 0 294.475 84"' in svg and 'transform="translate(0 0) scale(1)"' in svg, "the lockup template moved: update GreetSurface"
        # the template's bodies, as the circles they are drawn from: "M{cx - r} {cy}a{r} {r} 0 1 0 {2r} 0a... Z"
        tpl = []
        for body in ("coat", "ring", "core"):
            d = re.search(rf'<path id="{body}" fill="\{{\{{accent\}}\}}" fill-rule="evenodd" d="([^"]+)"', svg).group(1)
            for x, y, r in re.findall(r"M([0-9.]+) ([0-9.]+)a([0-9.]+) ", d):
                tpl.append((round(float(x) + float(r), 3), round(float(y), 3), float(r)))
        m = re.search(r"readonly property var circles: \[(.*?)\n\s*\]", qml, re.S)
        drawn = [tuple(float(v) for v in c) for c in re.findall(r"\[([0-9.]+), ([0-9.]+), ([0-9.]+)\]", m.group(1))]
        drawn = [(round(x, 3), round(y, 3), r) for x, y, r in drawn]
        assert drawn == tpl, f"GreetSurface draws the circles {drawn}, the template has {tpl}"
        assert qml.count("PathSvg { path: logoBox.arcs(") == 3 and qml.count("ShapePath.OddEvenFill") == 3, "GreetSurface does not draw three bodies even-odd"
        for k in ("arcs(0, 1)", "arcs(2, 3)", "arcs(4, 4)"):
            assert k in qml, f"GreetSurface does not draw the body {k}"
        # the curve renderer fills tangent contours wrongly under even-odd (a solid disc): the geometry renderer, multisampled
        assert "Shape.GeometryRenderer" in qml and "layer.samples: 8" in qml and "CurveRenderer" not in qml, "GreetSurface must use the geometry renderer"
        assert "294.475 * unit" in qml and "height / 84" in qml, "GreetSurface scales the mark by a different box"
        assert "assets/ridge-lockup-base.png" in open("${greet}/shell.qml").read()
        print("greeter mark: the template's circles", tpl)
        PY

        echo "== 2. the splash themes"
        python3 ${./branding-surfaces-themes.py} $T $b/accents/accents.json ${ridgeScript}

        echo "== 3. theme selection"
        accent() {  # $1 = the owner's accent (or "" for none) -> the Theme= rime-plymouth-accent writes
          rm -rf sel; mkdir -p sel/greet/accents sel/esp
          printf rime > sel/greet/last-user
          [ -z "$1" ] || printf '%s' "$1" > sel/greet/accents/rime
          RIME_GREET_VAR=$PWD/sel/greet RIME_PLYMOUTH_CONF=$PWD/sel/plymouthd.conf RIME_PLYMOUTH_THEMES=$T \
            RIME_ESP_DIRS=$PWD/sel/esp sh ${accentScript}
          sed -n 's/^Theme=//p' sel/plymouthd.conf
        }
        n=0
        for pair in '#91c2dc rime-os-accent-13' '#fa98a0 rime-os-accent-00' '#a6d0f7 rime-os-accent-14' \
                    '#808080 rime-os' ' rime-os'; do
          in=''${pair% *}; want=''${pair##* }
          got=$(accent "$in")
          [ "$got" = "$want" ] || { echo "FAIL: accent '$in' -> '$got', want $want"; exit 1; }
          echo "accent '$in' -> $got"; n=$((n + 1))
        done
        # the credential it leaves is what the initrd reads
        accent '#fa98a0' >/dev/null
        test "$(cat sel/esp/loader/credentials/rime.accent.cred)" = 00
        printf '[Daemon]\nShowDelay=0\nTheme=rime-os\n' > initrd.conf
        RIME_PLYMOUTH_THEMES=$T RIME_PLYMOUTH_CONF=$PWD/initrd.conf RIME_ESP_DIRS=$PWD/sel/esp \
          sh ${initrdPicker}
        grep -qx 'Theme=rime-os-accent-00' initrd.conf || { echo "FAIL: the initrd picker did not start the accent"; cat initrd.conf; exit 1; }
        # no credential: the default theme stays
        printf '[Daemon]\nTheme=rime-os\n' > initrd.conf; mkdir -p empty-esp
        RIME_PLYMOUTH_THEMES=$T RIME_PLYMOUTH_CONF=$PWD/initrd.conf RIME_ESP_DIRS=$PWD/empty-esp \
          sh ${initrdPicker}
        grep -qx 'Theme=rime-os' initrd.conf
        echo "selection: $n accents mapped, the initrd starts the credential's theme"

        echo "branding-surfaces-static: PASS" | tee $out
      '';

  branding-surfaces = pkgs.testers.runNixOSTest {
    name = "rime-branding-surfaces";

    node.pkgsReadOnly = false;
    # Reads the screenshots back: each one must show the mark in the colour it claims (mark() below).
    extraPythonPackages = p: [ p.pillow ];

    nodes = {
      # The splash on its own: no desktop, so nothing takes the display from plymouthd.
      splash =
        { lib, pkgs, ... }:
        {
          imports = [ self.nixosModules.default ];

          boot.plymouth.enable = true;
          boot.kernelParams = [
            "quiet"
            "plymouth.ignore-serial-consoles"
          ];
          # A KMS device in the initrd (QEMU's direct kernel boot leaves no firmware framebuffer for simpledrm).
          boot.initrd.kernelModules = [ "virtio_gpu" ];
          virtualisation = {
            qemu.options = [
              "-vga none"
              "-device virtio-gpu-pci,xres=1280,yres=800"
            ];
            # vdb becomes the EFI System Partition rime-plymouth-accent writes and the initrd reads.
            emptyDiskImages = [ 64 ];
          };

          # Hold the splash after boot so the screenshot is the boot splash itself, and keep getty off its VT.
          systemd.services.plymouth-quit.wantedBy = lib.mkForce [ ];
          systemd.services.plymouth-quit-wait.wantedBy = lib.mkForce [ ];
          systemd.services."getty@tty1".enable = false;
          systemd.services."autovt@tty1".enable = false;

          environment.systemPackages = [
            pkgs.gptfdisk
            pkgs.dosfstools
            pkgs.cpio
            pkgs.zstd
            pkgs.rime.rime-libexec
          ];
        };

      desktop =
        { lib, ... }:
        {
          imports = [ self.nixosModules.default ];
          rime.desktop.enable = true;
          users.users.rime = {
            isNormalUser = true;
            uid = 1000;
            initialPassword = "rime";
          };
          virtualisation = {
            memorySize = 4096;
            cores = 4;
            qemu.options = [
              "-vga none"
              "-device virtio-gpu-pci,xres=1280,yres=800"
            ];
          };
          # As nix/tests/desktop.nix: sway's GLES2 renderer does not run on QEMU's GPU.
          environment.sessionVariables = {
            WLR_RENDERER = "pixman";
            QT_QUICK_BACKEND = "software";
          };
          systemd.services.rime-flathub-setup.enable = lib.mkForce false;
        };
    };

    testScript =
      { nodes, ... }:
      ''
      import math, os
      from PIL import Image

      def shot(m, name):
          m.screenshot(name)
          print("screenshot:", name + ".png")

      # The pixels, not only the journal: in the lockup's box of screenshot `name` (1280x800), the mark is `want` (at
      # least 150 px within 20 of it; it covers about 1,600 px in the greeter and 2,800 on the splash, where the boot
      # wave dims at most one body, leaving at least the core and the ring, about 1,200 px) and the off-white wordmark
      # is there (at least 500 near-white px).
      GREETER_LOGO = (536, 372, 744, 440)  # the 196 x 56 lockup, above the username field (whose ring is the accent)
      SPLASH_LOGO = (500, 325, 780, 410)   # the 262 x 75 lockup at 46 % of the screen's height

      def mark(m, name, box, want):
          img = Image.open(os.path.join(m.out_dir, name + ".png")).convert("RGB").crop(box)
          w = tuple(int(want[i:i + 2], 16) for i in (1, 3, 5))
          raw = img.tobytes()  # RGB, three bytes a pixel
          px = [tuple(raw[i:i + 3]) for i in range(0, len(raw), 3)]
          accent = sum(1 for p in px if sum((a - b) ** 2 for a, b in zip(p, w)) <= 400)
          white = sum(1 for p in px if min(p) >= 235)
          print(f"{name}: {accent} px of the mark in {want}, {white} px of the wordmark")
          assert accent >= 150, f"{name}: the mark is not {want} ({accent} px)"
          assert white >= 500, f"{name}: the wordmark is missing ({white} px)"
          # the three bodies, not a solid disc: inside the mark's square the accent covers what even-odd leaves,
          # (42^2 - 33^2 + 26^2 - 20^2 + 15^2) / 42^2 = 2/3 of the disc (antialiased edges, not counted, pull it lower)
          xs = [i % img.width for i, p in enumerate(px) if sum((a - b) ** 2 for a, b in zip(p, w)) <= 400]
          ys = [i // img.width for i, p in enumerate(px) if sum((a - b) ** 2 for a, b in zip(p, w)) <= 400]
          dw, dh = max(xs) - min(xs) + 1, max(ys) - min(ys) + 1
          cover = accent / (math.pi * dw * dh / 4)
          print(f"{name}: the mark is {dw} x {dh} px, the accent covers {cover:.2f} of its disc")
          assert abs(dw - dh) <= 3, f"{name}: the mark is not round ({dw} x {dh})"
          assert 0.45 <= cover <= 0.80, f"{name}: the accent covers {cover:.2f} of the disc, not the three bodies (2/3)"

      # ── the splash ────────────────────────────────────────────────────────────────────────────────
      splash.start()
      splash.wait_for_unit("multi-user.target")

      with subtest("os-release, icons and the Plymouth logo are Rime's"):
          osr = splash.succeed("cat /etc/os-release")
          print(osr)
          kv = dict(l.split("=", 1) for l in osr.splitlines() if "=" in l)
          kv = {k: v.strip('"') for k, v in kv.items()}
          want = {"NAME": "Rime OS", "PRETTY_NAME": "Rime OS", "ID": "nixos", "VARIANT_ID": "rime", "DEFAULT_HOSTNAME": "rime", "LOGO": "rime-os",
                  "HOME_URL": "https://rimeos.com/", "ANSI_COLOR": "0;38;2;145;194;220", "VENDOR_NAME": "Rime OS"}
          for k, v in want.items():
              assert kv.get(k) == v, f"os-release {k}={kv.get(k)!r}, want {v!r}"
          assert "ID_LIKE" not in kv, f"ID is nixos itself, no ID_LIKE: {kv.get('ID_LIKE')!r}"
          assert kv["CPE_NAME"].startswith("cpe:/o:rimeos:rime_os:"), kv["CPE_NAME"]
          assert "nixos.org" not in osr, "os-release still points at nixos.org"
          splash.succeed("test -s /run/current-system/sw/share/icons/hicolor/256x256/apps/rime-os.png")
          splash.succeed("test -s /run/current-system/sw/share/icons/hicolor/scalable/apps/distributor-logo.svg"
                         " || test -s /run/current-system/sw/share/icons/hicolor/256x256/apps/distributor-logo.png")
          splash.succeed("test -s /run/current-system/sw/share/pixmaps/rime-os.png")
          splash.succeed("cmp /etc/plymouth/logo.png ${branding}/share/rime/branding/png/symbol/rime-symbol-dark-128.png")

      with subtest("the initrd carries all 25 themes, the picker and vfat"):
          initrd = "${nodes.splash.system.build.initialRamdisk}/${nodes.splash.system.boot.loader.initrdFile}"
          print(splash.succeed("ls -lL /run/booted-system/initrd " + initrd))
          listing = splash.succeed(f"zstd -dc {initrd} > /tmp/initrd.cpio && cpio -it --quiet < /tmp/initrd.cpio")
          for want in ["rime-os/rime-os.plymouth", "rime-os-accent-00/rime-os-accent-00.plymouth",
                       "rime-os-accent-23/lockup-coat-hd.png", "rime-plymouth-theme-initrd", "vfat.ko"]:
              assert want in listing, f"initrd lacks {want}"

      with subtest("the boot splash is the Origin lockup (rime-os)"):
          splash.succeed("plymouth --ping")
          j = splash.succeed("journalctl -b -o cat -u rime-plymouth-theme.service --no-pager")
          print(j)
          assert "not found" not in j, "the initrd picker is missing a tool"
          assert "plymouthd.conf:Theme=rime-os\n" in j + "\n", "the initrd did not start rime-os"
          splash.succeed("grep -qx 'Theme=rime-os' /etc/plymouth/plymouthd.conf")
          splash.sleep(3)
          shot(splash, "plymouth-default")
          mark(splash, "plymouth-default", SPLASH_LOGO, "#91C2DC")  # Rime Blue

      with subtest("the passphrase prompt, with three bullets"):
          splash.execute("plymouth ask-for-password --prompt='Enter passphrase for root' >/tmp/pw 2>&1 &")
          splash.sleep(2)
          splash.send_chars("abc")
          splash.sleep(2)
          shot(splash, "plymouth-password")
          splash.send_key("ret")
          splash.wait_until_succeeds("test \"$(cat /tmp/pw)\" = abc")

      with subtest("the owner's accent reaches the next boot through the ESP"):
          splash.succeed("sgdisk -n1:0:0 -t1:ef00 /dev/vdb && udevadm settle && mkfs.vfat -n ESP /dev/vdb1")
          splash.succeed("mkdir -p /var/lib/rime-greet/accents && printf rime > /var/lib/rime-greet/last-user"
                         " && printf '#fa98a0' > /var/lib/rime-greet/accents/rime")
          splash.succeed("/run/current-system/sw/libexec/rime-plymouth-accent")
          splash.succeed("grep -qx 'Theme=rime-os-accent-00' /etc/plymouth/plymouthd.conf")
          splash.succeed("mkdir -p /run/esp && mount -t vfat -o ro /dev/vdb1 /run/esp")
          splash.succeed("test \"$(cat /run/esp/loader/credentials/rime.accent.cred)\" = 00")
          splash.succeed("umount /run/esp")
          splash.shutdown()
          splash.start()
          splash.wait_for_unit("multi-user.target")
          out = splash.succeed("journalctl -b -o short-monotonic --no-pager -u rime-plymouth-theme.service -u plymouth-start.service")
          print(out)
          assert "not found" not in out, "the initrd picker is missing a tool"
          assert "plymouthd.conf:Theme=rime-os-accent-00" in out, "the initrd did not start the owner's accent"
          splash.succeed("plymouth --ping")
          splash.sleep(3)
          shot(splash, "plymouth-accent")
          mark(splash, "plymouth-accent", SPLASH_LOGO, "#DC9191")  # accents.json slot 00

      # ── the greeter ───────────────────────────────────────────────────────────────────────────────
      desktop.start()
      desktop.wait_for_unit("greetd.service")
      desktop.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")

      with subtest("the greeter shows the Origin mark in Rime Blue"):
          desktop.sleep(10)
          shot(desktop, "greeter-default")
          # greetd's journal never carries the greeter's QML output, so a QML error shows here as missing pixels.
          mark(desktop, "greeter-default", GREETER_LOGO, "#91C2DC")  # Rime Blue

      with subtest("the greeter follows the user's published accent"):
          desktop.succeed("install -d /var/lib/rime-greet/accents && printf rime > /var/lib/rime-greet/last-user"
                          " && printf '#fa98a0' > /var/lib/rime-greet/accents/rime")
          desktop.succeed("systemctl restart greetd")
          desktop.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
          desktop.sleep(10)
          shot(desktop, "greeter-accent")
          mark(desktop, "greeter-accent", GREETER_LOGO, "#FA98A0")  # the published accent itself
    '';
  };
}
