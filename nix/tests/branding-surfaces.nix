# The layered ridge on Rime's OS surfaces (nix/modules/branding.nix, docs/branding.md "Where the ridge shows").
#
# branding-surfaces-static (build-time, no VM):
#   1. the greeter's logo asset is what make-logo-assets.sh renders from the ridge template (pixels), and the plane-3
#      polygon GreetSurface.qml draws is the template's own plane 3;
#   2. the 25 splash themes keep the brand rules (branding-surfaces-themes.py: only plane 3 changes colour, every
#      image exists with transparency, one script, the engine rules);
#   3. rime-plymouth-accent picks rime-os-accent-NN for a coloured accent and the ridge's rime-os for none or grey,
#      and the initrd picker (files/dracut/rime-plymouth-accent/rime-plymouth-theme) starts the theme the
#      credential names, against the real theme set.
#
# branding-surfaces (VM): a machine with the splash on boots into it and stays there (plymouth-quit is held back), so
#   the screenshots are the boot splash itself: plymouth-default.png (rime-os), plymouth-password.png (the passphrase
#   prompt with three bullets), then rime-plymouth-accent writes the owner's accent to a real ESP and the NEXT boot's
#   initrd starts plymouth-accent.png in rime-os-accent-00. os-release, the icons, the Plymouth logo and the initrd's
#   theme set are checked on the running system. A second machine with the desktop shows the greeter in Rime Blue
#   (greeter-default.png) and in a user's published accent (greeter-accent.png). Every one of these screenshots is read
#   back (plane3()): plane 3 must be on screen in the colour named, next to the neutral planes.
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
        assert bad == 0, f"{bad} coloured pixels in the neutral lockup: plane 3 must be drawn by the greeter, not baked in"
        print("greeter logo: reproducible, transparent, neutral", a.size)
        PY
        shellcheck ${greet}/make-logo-assets.sh
        python3 - $b/templates/rime-lockup-horizontal.svg.in ${greet}/GreetSurface.qml <<'PY'
        import re, sys
        svg, qml = (open(p).read() for p in sys.argv[1:3])
        assert 'viewBox="0 0 553.16 175"' in svg and 'transform="translate(0 0) scale(1)"' in svg, "the lockup template moved: update GreetSurface"
        pts = re.search(r'fill="\{\{accent\}\}" points="([^"]+)"', svg).group(1)
        tpl = [tuple(float(v) for v in p.split(",")) for p in pts.split()]
        start = re.search(r"startX: ([0-9.]+) \* logoBox.unit; startY: ([0-9.]+) \* logoBox.unit", qml)
        lines = re.findall(r"PathLine \{ x: ([0-9.]+)\s*\* logoBox.unit; y: ([0-9.]+) \* logoBox.unit \}", qml)
        drawn = [tuple(float(v) for v in start.groups())] + [tuple(float(v) for v in l) for l in lines]
        assert drawn[-1] == drawn[0], "the greeter's plane 3 is not closed"
        assert drawn[:-1] == tpl, f"GreetSurface draws plane 3 at {drawn[:-1]}, the template has {tpl}"
        assert "553.16 * unit" in qml and "height / 175" in qml, "GreetSurface scales plane 3 by a different box"
        assert "assets/ridge-lockup-base.png" in open("${greet}/shell.qml").read()
        print("greeter plane 3: the template's polygon", tpl)
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
        # no credential: the ridge default stays
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
    # Reads the screenshots back: each one must show plane 3 in the colour it claims (plane3() below).
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
      import os
      from PIL import Image

      def shot(m, name):
          m.screenshot(name)
          print("screenshot:", name + ".png")

      # The pixels, not only the journal: in the logo's box of screenshot `name` (1280x800), plane 3 is `want` (at least
      # 150 px within 20 of it; the proof screenshots measure 278 in the greeter, 504-513 on the splash, and 0 for any
      # other accent) and the neutral planes and wordmark are there (at least 500 near-white px; measured 1091-1659).
      GREETER_LOGO = (550, 372, 640, 440)  # above the username field, whose ring is also the accent
      SPLASH_LOGO = (520, 325, 630, 410)   # above the progress dots, one of which is also the accent

      def plane3(m, name, box, want):
          img = Image.open(os.path.join(m.out_dir, name + ".png")).convert("RGB").crop(box)
          w = tuple(int(want[i:i + 2], 16) for i in (1, 3, 5))
          raw = img.tobytes()  # RGB, three bytes a pixel
          px = [tuple(raw[i:i + 3]) for i in range(0, len(raw), 3)]
          accent = sum(1 for p in px if sum((a - b) ** 2 for a, b in zip(p, w)) <= 400)
          white = sum(1 for p in px if min(p) >= 235)
          print(f"{name}: {accent} px of plane 3 in {want}, {white} px of the neutral planes")
          assert accent >= 150, f"{name}: plane 3 is not {want} ({accent} px)"
          assert white >= 500, f"{name}: the neutral planes are missing ({white} px)"

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
                       "rime-os-accent-23/lockup-accent-hd.png", "rime-plymouth-theme-initrd", "vfat.ko"]:
              assert want in listing, f"initrd lacks {want}"

      with subtest("the boot splash is the ridge (rime-os)"):
          splash.succeed("plymouth --ping")
          j = splash.succeed("journalctl -b -o cat -u rime-plymouth-theme.service --no-pager")
          print(j)
          assert "not found" not in j, "the initrd picker is missing a tool"
          assert "plymouthd.conf:Theme=rime-os\n" in j + "\n", "the initrd did not start rime-os"
          splash.succeed("grep -qx 'Theme=rime-os' /etc/plymouth/plymouthd.conf")
          splash.sleep(3)
          shot(splash, "plymouth-default")
          plane3(splash, "plymouth-default", SPLASH_LOGO, "#91C2DC")  # Rime Blue

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
          plane3(splash, "plymouth-accent", SPLASH_LOGO, "#DC9191")  # accents.json slot 00

      # ── the greeter ───────────────────────────────────────────────────────────────────────────────
      desktop.start()
      desktop.wait_for_unit("greetd.service")
      desktop.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")

      with subtest("the greeter shows the ridge in Rime Blue"):
          desktop.sleep(10)
          shot(desktop, "greeter-default")
          # greetd's journal never carries the greeter's QML output, so a QML error shows here as missing pixels.
          plane3(desktop, "greeter-default", GREETER_LOGO, "#91C2DC")  # Rime Blue

      with subtest("the greeter follows the user's published accent"):
          desktop.succeed("install -d /var/lib/rime-greet/accents && printf rime > /var/lib/rime-greet/last-user"
                          " && printf '#fa98a0' > /var/lib/rime-greet/accents/rime")
          desktop.succeed("systemctl restart greetd")
          desktop.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
          desktop.sleep(10)
          shot(desktop, "greeter-accent")
          plane3(desktop, "greeter-accent", GREETER_LOGO, "#FA98A0")  # the published accent itself
    '';
  };
}
