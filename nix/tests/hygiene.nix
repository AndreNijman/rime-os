# Small regressions from the 11-02 hygiene pass, each with its mutant (run: nix build
# path:.#checks.x86_64-linux.<check>.mutants.<name>; every mutant must FAIL):
#   niri-default      the niri default config the system ships (rime-shell-firstrun copies it to a machine with no
#                     config.kdl) opens a terminal on Mod+T with xdg-terminal-exec, not alacritty, and is the only
#                     default-config.kdl in the system packages. Mutant: upstream's file (niri.doc) in its place.
#   libexec-pycache   rime-libexec builds from a source tree that holds a files/system/libexec/__pycache__ (what
#                     running the Python suites leaves, and what `path:.` copies although git ignores it), and its
#                     output is the same as from a clean tree. Mutant: the install loop without its regular-file guard
#                     ("omitting directory"). The source filter's .pyc exclusion is NOT exercised here (this check
#                     replaces src after the filter); it was proved by hand: a __pycache__ with a .pyc in the worktree
#                     leaves rime-libexec's drvPath unchanged (11-02, /var/lab/rime-nix/artifacts/verify-hygiene/).
#   hostname-default  a machine whose host.toml names no host (and with no /etc/hostname) boots as `rime`, hostnamed
#                     says so, and `hostnamectl hostname` writes /etc/hostname. Mutants: rime-hostlayer's apply
#                     without that default; hostnamed back on nixpkgs' /etc/static/hostname.
{ pkgs, self }:

let
  lib = pkgs.lib;
  vmConfig = self.nixosConfigurations.rime-vm.config;

  # ---- niri-default -------------------------------------------------------------------------------------------
  # The packages that put share/doc/niri/default-config.kdl into the system profile: Rime's, or upstream's niri.doc
  # (by store path, without building anything at evaluation). Exactly one may be there, and it must be Rime's.
  upstreamDoc = self.nixosConfigurations.rime-vm.pkgs.niri.doc.outPath;
  niriDocs = lib.filter (p: (p.name or "") == "rime-niri-default-config" || (p.outPath or "") == upstreamDoc)
    vmConfig.environment.systemPackages;
  niriCheck = name: kdl: pkgs.runCommand name { } ''
    f=${kdl}
    fail() { echo "FAIL: $*" >&2; exit 1; }
    grep -E '^\s*Mod\+T .*\{ spawn "xdg-terminal-exec"; \}' "$f" || fail "Mod+T does not spawn xdg-terminal-exec"
    if grep -n 'spawn "alacritty"' "$f"; then fail "the default config still spawns alacritty"; fi
    echo "niri default: Mod+T runs xdg-terminal-exec" | tee $out
  '';

  # ---- libexec-pycache ----------------------------------------------------------------------------------------
  libexec = pkgs.rime.rime-libexec;
  withPycache = drv: drv.overrideAttrs (o: {
    src = pkgs.runCommand "rime-libexec-src-with-pycache" { } ''
      cp -r ${o.src} $out
      chmod -R u+w $out
      mkdir $out/files/system/libexec/__pycache__
      printf 'not bytecode\n' > $out/files/system/libexec/__pycache__/rime-input-apply.cpython-313.pyc
    '';
  });
  listing = drv: pkgs.runCommand "${drv.name}-listing" { } ''
    cd ${drv} && find . -printf '%y %m %P\n' | sort > $out
  '';
  pycacheCheck = name: drv: pkgs.runCommand name { } ''
    if ! diff -u ${listing libexec} ${listing drv}; then
      echo "FAIL: rime-libexec from a tree with __pycache__ differs from the clean build" >&2; exit 1
    fi
    echo "rime-libexec: a __pycache__ in the source changes nothing ($(wc -l < ${listing drv}) entries)" | tee $out
  '';
  # The package as it was before 11-02 (both guards off), on the tree with a __pycache__.
  unguarded = (withPycache libexec).overrideAttrs (o: {
    installPhase = builtins.replaceStrings [ ''[ -f "$f" ] || continue # a directory (__pycache__) is not a script'' ] [ "" ]
      o.installPhase;
  });

  # ---- hostname-default ---------------------------------------------------------------------------------------
  hostnameTest = name: extra: pkgs.testers.runNixOSTest {
    inherit name;
    node.pkgsReadOnly = false;
    nodes.machine =
      { lib, ... }:
      {
        imports = [
          self.nixosModules.default
          ../hosts/vm.nix
          extra
        ];
        # No name in the closure (hostlayer.nix's "" loses to the test driver's mkDefault "machine" otherwise).
        networking.hostName = lib.mkForce "";
        systemd.services.rime-flathub-setup.enable = lib.mkForce false;
        virtualisation.qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
      };
    testScript = ''
      machine.wait_for_unit("multi-user.target")
      machine.fail("test -e /var/lib/rime/host.toml")
      print(machine.succeed("cat /etc/hostname; hostname; hostnamectl"))
      assert machine.succeed("cat /etc/hostname").strip() == "rime", "/etc/hostname is not rime"
      assert machine.succeed("hostname").strip() == "rime", "the kernel hostname is not rime"
      assert machine.succeed("hostnamectl --static").strip() == "rime", "hostnamed does not say rime"
      # hostnamed writes the machine's own file (Settings and `hostnamectl hostname` go through it).
      machine.succeed("hostnamectl hostname renamed")
      assert machine.succeed("cat /etc/hostname").strip() == "renamed", "hostnamectl did not write /etc/hostname"
      assert machine.succeed("hostname").strip() == "renamed"
    '';
  };
in
{
  niri-default =
    assert lib.assertMsg (map (p: p.name) niriDocs == [ "rime-niri-default-config" ])
      "the system packages must carry Rime's niri default config and not upstream's niri.doc";
    (niriCheck "rime-niri-default" "${lib.head niriDocs}/share/doc/niri/default-config.kdl").overrideAttrs (o: {
      passthru = (o.passthru or { }) // {
        mutants.upstream-file = niriCheck "rime-niri-default-mutant-upstream" "${pkgs.niri.doc}/share/doc/niri/default-config.kdl";
      };
    });

  libexec-pycache = (pycacheCheck "rime-libexec-pycache" (withPycache libexec)).overrideAttrs (o: {
    passthru = (o.passthru or { }) // {
      mutants.unguarded = pycacheCheck "rime-libexec-pycache-mutant-unguarded" unguarded;
    };
  });

  hostname-default = hostnameTest "rime-hostname-default" { } // {
    # hostnamed back on nixpkgs' /etc/static/hostname
    mutants.hostnamed-static = hostnameTest "rime-hostname-default-mutant-hostnamed" (
      { lib, ... }:
      {
        systemd.services.systemd-hostnamed.environment.SYSTEMD_ETC_HOSTNAME = lib.mkOverride 10 "/etc/static/hostname";
      }
    );
    # rime-hostlayer apply without the "rime" default
    mutants.no-default = hostnameTest "rime-hostname-default-mutant" (
        { lib, ... }:
        {
          # after the rime overlay, or that one replaces pkgs.rime whole
          nixpkgs.overlays = lib.mkAfter [
            (_: prev: {
              rime = prev.rime // {
                rime-hostlayer = prev.rime.rime-hostlayer.overrideAttrs (_: {
                  doInstallCheck = false; # its unit test of the default would refuse the mutant
                  postInstall = ''
                    substituteInPlace $out/bin/rime-hostlayer --replace-fail 'hostname = "rime"' 'hostname = None'
                  '';
                });
              };
            })
          ];
        }
      );
  };
}
