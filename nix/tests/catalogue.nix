# The package catalogue on NixOS (PLAN.md 4.5, D5): `rime install/remove/search/list` through
# nix/catalogue/catalogue.nix (rimed/rime/src/nixos/pkg.rs), nix-ld and appimage-run (nix/modules/packages-catalogue.nix).
#
#   catalogue       VM test. A Flathub stand-in (a tiny local Flatpak repo built here, added as the remote "flathub":
#                   no network) for the flatpak kind, by catalogue name, by bare name and by application id; the steam
#                   toggle (gaming off: the switch is written and the feature's target follows it); refusals with their
#                   named reasons; a class 2 app installed into the rime-apps profile from a release record; nix-ld
#                   running a foreign dynamically linked binary that needs a library from Rime's set (expat).
#   catalogue-data  Evaluation only: rime-vm's rendered catalogue sends every target `rime install` handled on bootc
#                   to the kind decided for it, and every class 2 app evaluates (system.build.rimeCatalogue drvPath).
{ pkgs, self }:

let
  inherit (pkgs) lib;

  # A Flatpak repository with one runtime and one application, built without network (flatpak build-export needs a
  # runtime with usr/ and files/, an app with files/ and export/, and both metadata files).
  # `greeting` is what the app prints: a second repo with another greeting is a newer commit of the same app.
  mkFlatpakRepo = suffix: greeting: pkgs.runCommand "rime-test-flatpak-repo${suffix}" { nativeBuildInputs = [ pkgs.flatpak pkgs.ostree ]; } ''
    set -eu
    export HOME=$TMPDIR
    rt=$TMPDIR/runtime app=$TMPDIR/app
    mkdir -p $rt/usr/bin $rt/files $app/files/bin $app/export
    cp ${pkgs.pkgsStatic.busybox}/bin/busybox $rt/usr/bin/busybox
    ln -s busybox $rt/usr/bin/sh
    printf '[Runtime]\nname=org.rime.TestPlatform\nruntime=org.rime.TestPlatform/x86_64/1\nsdk=org.rime.TestPlatform/x86_64/1\n' > $rt/metadata
    printf '#!/bin/sh\necho ${greeting}\n' > $app/files/bin/rime-test-hello
    chmod +x $app/files/bin/rime-test-hello
    printf '[Application]\nname=org.rime.TestHello\nruntime=org.rime.TestPlatform/x86_64/1\nsdk=org.rime.TestPlatform/x86_64/1\ncommand=rime-test-hello\n' > $app/metadata
    flatpak build-export --runtime --disable-fsync --no-update-summary $out $rt 1
    flatpak build-export --disable-fsync --no-update-summary $out $app master
    flatpak build-update-repo $out
    ostree --repo=$out refs
  '';
  flatpakRepo = mkFlatpakRepo "" "hello from the rime test flatpak";
  flatpakRepo2 = mkFlatpakRepo "-2" "hello from the second build";

  # A program built the way a vendor tarball's is: interpreter /lib64/ld-linux-x86-64.so.2, no RUNPATH, and a library
  # (libexpat) that is in Rime's nix-ld set but not in nix-ld's own defaults.
  foreign = pkgs.runCommandCC "rime-foreign-elf" {
    nativeBuildInputs = [ pkgs.patchelf ];
    buildInputs = [ pkgs.expat ];
  } ''
    cat > main.c <<'EOF'
    #include <stdio.h>
    #include <expat.h>
    int main(void) { printf("foreign binary ran with %s\n", XML_ExpatVersion()); return 0; }
    EOF
    mkdir -p $out/bin
    $CC main.c -lexpat -o $out/bin/rime-foreign
    patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath $out/bin/rime-foreign
    test "$(patchelf --print-interpreter $out/bin/rime-foreign)" = /lib64/ld-linux-x86-64.so.2
    test -z "$(patchelf --print-rpath $out/bin/rime-foreign)"
    patchelf --print-needed $out/bin/rime-foreign | grep -x libexpat.so.1 >/dev/null
  '';

  # claude-code's own release binary, exactly as Anthropic's installer puts it in ~/.local/bin: nixpkgs' pinned
  # download (sha256 from Anthropic's release manifest), decompressed and NOT patched, so its interpreter is the FHS
  # /lib64/ld-linux-x86-64.so.2 that only nix-ld serves on NixOS (p2-gates P3).
  claudeNative = pkgs.runCommand "claude-code-native-${pkgs.claude-code.version}" {
    nativeBuildInputs = [ pkgs.zstd pkgs.patchelf ];
  } ''
    mkdir -p $out
    unzstd -q ${pkgs.claude-code.src} -o $out/claude
    chmod 755 $out/claude
    test "$(patchelf --print-interpreter $out/claude)" = /lib64/ld-linux-x86-64.so.2
    if [ -n "$(patchelf --print-rpath $out/claude)" ]; then echo "has an rpath"; exit 1; fi
  '';

  # The class 2 app the VM installs: in the VM's store already (additionalPaths), as the cache would provide it.
  testApp = pkgs.hello;

  test = pkgs.testers.runNixOSTest {
    name = "rime-catalogue";

    # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
    node.pkgsReadOnly = false;

    nodes.machine = { config, pkgs, ... }: {
      imports = [ self.nixosModules.default ];

      rime.catalogue = {
        enable = true;
        extraEntries = {
          rime-test-hello = {
            kind = "flatpak";
            id = "org.rime.TestHello";
            summary = "the test Flatpak";
            why = "it is the test fixture's application";
          };
          hello = {
            kind = "nix";
            attrs = [ "hello" ];
            summary = "GNU hello";
          };
        };
      };
      # Gaming is opt-in (A4): this system does not carry it, so `rime install steam` selects it for the next update.
      rime.gaming.enable = false;

      environment.systemPackages = [ config.rime.services.rimed.package ];

      # Flatpak as rime.desktop sets it up, without the desktop; the fixture stands in for Flathub.
      services.flatpak.enable = true;
      xdg.portal = {
        enable = true;
        extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
        config.common.default = "*";
      };

      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };

      virtualisation = {
        memorySize = 2048;
        additionalPaths = [ flatpakRepo flatpakRepo2 foreign testApp claudeNative ];
      };
    };

    testScript = ''
      import json

      machine.wait_for_unit("multi-user.target")
      machine.succeed("flatpak remote-add --system --no-gpg-verify flathub file://${flatpakRepo}")

      def has(out, text):
          assert text in out, f"expected {text!r} in:\n{out}"

      with subtest("the catalogue is the one rendered from the data file"):
          cat = json.loads(machine.succeed("cat /etc/rime/catalogue.json"))
          assert cat["entries"]["steam"] == {"kind": "toggle", "feature": "gaming", "summary": cat["entries"]["steam"]["summary"]}, cat["entries"]["steam"]
          assert cat["features"]["gaming"]["in_system"] is False, cat["features"]["gaming"]
          assert cat["entries"]["hello"]["buildable"] is True and cat["entries"]["hello"]["version"], cat["entries"]["hello"]

      with subtest("a flatpak target installs from the remote, by catalogue name"):
          out = machine.succeed("rime install rime-test-hello 2>&1")
          has(out, "it is the test fixture's application")
          has(out, "installed from flathub: org.rime.TestHello")
          machine.succeed("flatpak info --system org.rime.TestHello")
          has(machine.succeed("rime pkg list 2>&1"), "org.rime.TestHello")
          # Flatpak keeps its own record, as on bootc: nothing goes into the requested list.
          machine.fail("grep -x rime-test-hello /var/lib/rime/pkg/requested")
          machine.succeed("rime remove rime-test-hello 2>&1")
          machine.fail("flatpak info --system org.rime.TestHello")

      with subtest("a bare name not in the catalogue is found on the remote by exact name"):
          out = machine.succeed("rime install testhello 2>&1")
          has(out, "'testhello' is not in Rime's catalogue; Flathub has it as org.rime.TestHello")
          machine.succeed("flatpak info --system org.rime.TestHello")
          machine.succeed("rime remove org.rime.TestHello 2>&1")
          machine.succeed("rime install org.rime.TestHello 2>&1")
          machine.succeed("flatpak info --system org.rime.TestHello")
          has(machine.succeed("rime search hello 2>&1"), "org.rime.TestHello")
          out = machine.fail("rime install nosuchapp 2>&1")
          has(out, "Flathub has no application of that name")
          has(out, "nothing was installed")
          machine.succeed("rime remove org.rime.TestHello 2>&1")

      with subtest("steam is the gaming toggle: selected for this machine, the feature's target follows the switch"):
          out = machine.succeed("rime install steam 2>&1")
          has(out, "steam comes with Rime's Gaming system, which this machine does not have yet")
          has(out, "`sudo rime update` installs the system that carries it as soon as Rime's release offers one")
          machine.succeed("test \"$(cat /var/lib/rime/switch/rime-feature@gaming.target.on)\" = steam")
          machine.succeed("grep -x steam /var/lib/rime/pkg/requested")
          # rime-feature@gaming.target starts only while the switch exists (what units of the feature hang off).
          machine.succeed("systemctl restart rime-features.service")
          machine.wait_until_succeeds("systemctl is-active rime-feature@gaming.target")
          out = machine.succeed("rime install gamescope 2>&1")
          machine.succeed("test \"$(cat /var/lib/rime/switch/rime-feature@gaming.target.on)\" = \"$(printf 'steam\\ngamescope')\"")
          has(machine.succeed("rime pkg list 2>&1"), "feature gaming (selected; comes with the next update)")
          out = machine.succeed("rime remove steam 2>&1")
          has(out, "Gaming stays on for gamescope")
          machine.succeed("test -e /var/lib/rime/switch/rime-feature@gaming.target.on")
          out = machine.succeed("rime remove gamescope 2>&1")
          has(out, "Gaming is switched off for this machine")
          machine.fail("test -e /var/lib/rime/switch/rime-feature@gaming.target.on")
          machine.fail("grep -x steam /var/lib/rime/pkg/requested")
          machine.succeed("systemctl stop rime-feature@gaming.target")
          machine.succeed("systemctl start rime-feature@gaming.target")
          machine.fail("systemctl is-active rime-feature@gaming.target")

      with subtest("refused targets say why, and nothing is installed"):
          for name, reason in [
              ("kernel", "it is part of the operating system and changes only with a system update"),
              ("nvidia-driver", "it is part of the operating system and changes only with a system update"),
              ("./thing.rpm", "Rime on NixOS has no RPM packages and no system extension"),
              ("Thing.AppImage", "AppImages run directly on Rime"),
              ("waydroid", "Waydroid needs system configuration"),
              ("libgcc.i686", "Steam's come with the gaming system: rime install steam"),
              ("chatgpt", "ChatGPT needs a Rime build that checks OpenAI's signing key"),
          ]:
              out = machine.fail(f"rime install {name} 2>&1")
              has(out, f"refusing {name}: ")
              has(out, reason)
              has(out, "nothing was installed")
          # One refused name stops the whole request.
          has(machine.fail("rime install rime-test-hello kernel 2>&1"), "nothing was installed")
          machine.fail("flatpak info --system org.rime.TestHello")
          has(machine.fail("rime install --allow-unsigned htop 2>&1"), "nothing unsigned is installed")
          has(machine.fail("rime repo enable-copr a/b 2>&1"), "COPR repositories do not exist on Rime on NixOS")
          machine.fail("test -e /var/lib/rime/switch/rime-feature@virtualization.target.on")

      with subtest("qemu-kvm (bootc's name) selects the virtual machines feature"):
          has(machine.succeed("rime install qemu-kvm 2>&1"), "comes with Rime's Virtual machines system")
          machine.succeed("test \"$(cat /var/lib/rime/switch/rime-feature@virtualization.target.on)\" = qemu")
          has(machine.succeed("rime remove qemu 2>&1"), "Virtual machines is switched off for this machine")
          machine.fail("test -e /var/lib/rime/switch/rime-feature@virtualization.target.on")

      with subtest("a class 2 app comes from the release record into the rime-apps profile"):
          has(machine.fail("rime install hello 2>&1"), "no release record naming the builds of Rime's catalogue apps")
          machine.succeed("echo '{\"apps\": {\"hello\": \"${testApp}\"}}' > /var/lib/rime/nix/catalogue-outs.json")
          has(machine.succeed("rime install hello 2>&1"), "hello: installed (in /nix/var/nix/profiles/rime-apps")
          machine.succeed("grep -x hello /var/lib/rime/pkg/requested")
          has(machine.succeed("su - alice -c hello"), "Hello, world!")
          machine.succeed("rime remove hello 2>&1")
          machine.fail("su - alice -c 'command -v hello'")
          machine.fail("grep -x hello /var/lib/rime/pkg/requested")
          machine.succeed("rime pkg rollback 2>&1")
          has(machine.succeed("su - alice -c hello"), "Hello, world!")
          # The requested list moves back with the profile: `rime pkg list` and the next upgrade agree with it.
          machine.succeed("grep -x hello /var/lib/rime/pkg/requested")
          has(machine.succeed("rime pkg list 2>&1"), "hello")
          machine.succeed("rime pkg upgrade 2>&1")
          has(machine.succeed("su - alice -c hello"), "Hello, world!")
          machine.succeed("rime remove hello 2>&1")

      with subtest("an ordinary user is told why install needs root, in NixOS terms"):
          out = machine.fail("su - alice -c 'rime install hello' 2>&1")
          has(out, "it changes what is installed for every user")
          assert "system extension" not in out, out

      with subtest("a requested list carried over from bootc is read through the catalogue's names"):
          # bootc wrote the name as it was typed; the catalogue maps the spelling (here: case) to its entry.
          carried = "printf 'HELLO\\nsteam\\n' > /var/lib/rime/pkg/requested"
          machine.succeed(carried)
          has(machine.succeed("rime pkg list 2>&1"), "app, as hello")
          machine.succeed("rime remove hello 2>&1")
          machine.fail("grep -ix hello /var/lib/rime/pkg/requested")
          machine.succeed("grep -x steam /var/lib/rime/pkg/requested")
          machine.fail("su - alice -c 'command -v hello'")
          machine.succeed(carried)
          machine.succeed("rime pkg upgrade 2>&1")
          has(machine.succeed("su - alice -c hello"), "Hello, world!")
          machine.succeed("rm /var/lib/rime/pkg/requested")

      with subtest("pkg upgrade also updates the invoking user's own Flatpaks (rime-pkg cmd_flatpak_upgrade)"):
          machine.succeed("su - alice -c 'flatpak --user remote-add --no-gpg-verify userrepo file://${flatpakRepo}'")
          machine.succeed("su - alice -c 'flatpak --user install -y --noninteractive userrepo org.rime.TestHello' 2>&1")
          before = machine.succeed("su - alice -c 'flatpak --user info --show-commit org.rime.TestHello'").strip()
          machine.succeed("su - alice -c 'flatpak --user remote-modify --url=file://${flatpakRepo2} userrepo'")
          # The environment sudo gives root (HOME=/root): `--user` must still mean alice's installation.
          machine.succeed("HOME=/root SUDO_USER=alice rime pkg upgrade 2>&1")
          after = machine.succeed("su - alice -c 'flatpak --user info --show-commit org.rime.TestHello'").strip()
          assert before != after, f"alice's Flatpak stayed at {before}"
          machine.succeed("su - alice -c 'flatpak --user uninstall -y --noninteractive org.rime.TestHello' 2>&1")

      with subtest("claude-code's release binary in ~/.local/bin runs under nix-ld (P3)"):
          machine.succeed("su - alice -c 'mkdir -p ~/.local/bin && cp ${claudeNative}/claude ~/.local/bin/claude'")
          where = machine.succeed("su - alice -c 'command -v claude'").strip()
          assert where == "/home/alice/.local/bin/claude", where
          has(machine.succeed("su - alice -c 'claude --version' 2>&1"), "${pkgs.claude-code.version}")
          # The file that ran is the unpatched release binary, whose interpreter is the path only nix-ld serves.
          machine.succeed("cmp ${claudeNative}/claude /home/alice/.local/bin/claude")
          machine.succeed("test -e /lib64/ld-linux-x86-64.so.2")

      with subtest("nix-ld runs a foreign dynamically linked binary"):
          has(machine.succeed("su - alice -c ${foreign}/bin/rime-foreign"), "foreign binary ran with expat_")
          # Without the login environment too: nix-ld falls back to the system's library set.
          has(machine.succeed("env -i ${foreign}/bin/rime-foreign"), "foreign binary ran with expat_")
          machine.succeed("command -v appimage-run")
          machine.succeed("test -e /proc/sys/fs/binfmt_misc/appimage_type_2")
    '';
  };

  # rime-vm, the system the tests and the release are built from.
  vm = self.nixosConfigurations.rime-vm.config;

  # Every target `rime install` handled on bootc, and the kind it must get on NixOS (D5, A4).
  expected = {
    steam = "toggle";
    gamescope = "toggle";
    gamemode = "toggle";
    steam-devices = "toggle";
    qemu = "toggle";
    libvirt = "toggle";
    edk2-ovmf = "toggle";
    swtpm = "toggle";
    virtiofsd = "toggle";
    blender = "flatpak";
    chromium = "flatpak";
    libreoffice = "flatpak";
    obs = "flatpak";
    obs-studio = "flatpak";
    zed = "flatpak";
    discord = "flatpak";
    zen = "flatpak";
    go = "nix";
    golang = "nix";
    rust = "nix";
    java = "nix";
    java-latest-openjdk-devel = "nix";
    wine = "nix";
    mangohud = "nix";
    llama-cpp = "nix";
    chatgpt = "nix";
    claude-desktop = "nix";
    helium-bin = "nix";
    claude-code = "user-npm";
    equibop = "nix";
    "libgcc.i686" = "refuse";
    "libSM.i686" = "refuse";
    waydroid = "refuse";
  };

  catalogueData = pkgs.runCommand "rime-catalogue-data" {
    nativeBuildInputs = [ pkgs.jq ];
    catalogue = builtins.unsafeDiscardStringContext vm.environment.etc."rime/catalogue.json".text;
    expected = builtins.toJSON expected;
    passAsFile = [ "catalogue" "expected" ];
    # Evaluating this evaluates every class 2 app (a missing attribute throws); nothing is built.
    appsDrv = builtins.unsafeDiscardStringContext vm.system.build.rimeCatalogue.drvPath;
    gamingInVm = lib.boolToString vm.rime.gaming.enable;
  } ''
    set -eu
    echo "class 2 apps: $appsDrv"
    jq -e --slurpfile want "$expectedPath" '
      . as $c
      | def canon($n): ($n | ascii_downcase) as $l | ($c.aliases[$l] // (if $c.entries[$n] then $n else $l end));
        [ $want[0] | to_entries[] | . as $e
          | ($c.entries[canon($e.key)].kind // "missing") as $got
          | select($got != $e.value)
          | "\($e.key): expected \($e.value), got \($got)\n" ]
      | if length == 0 then true else (map(stderr) | false) end
    ' "$cataloguePath"
    # A buildable class 2 app carries its version; a pending one carries its reason.
    jq -e 'all(.entries[] | select(.kind == "nix"); (.buildable and (.version | length) > 0) or ((.buildable | not) and (.pending | length) > 0))' "$cataloguePath"
    # Flathub ids are application ids, and the in_system flag is the system's own option.
    jq -e '[.entries[] | select(.kind == "flatpak") | .id | test("^[A-Za-z][A-Za-z0-9_-]*(\\.[A-Za-z0-9_-]+){2,}$")] | all' "$cataloguePath"
    test "$(jq -r .features.gaming.in_system "$cataloguePath")" = "$gamingInVm"
    test "$(jq -r .features.virtualization.offered "$cataloguePath")" = true
    test "$(jq -r .features.virtualization.in_system "$cataloguePath")" = false
    touch $out
  '';
in
{
  catalogue = test;
  catalogue-data = catalogueData;
}
