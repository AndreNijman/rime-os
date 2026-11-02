# Rime's boot hook (nix/pkgs/rime-boot, nix/modules/boot.nix; PLAN.md D3 v1 chain):
#
#   boot-hook-unit  nix/tests/boot-hook-test.py against the packaged hook: BLS and custom-cfg layouts, default =
#                   the profile's current generation, content-addressed copies of <toplevel>/kernel (the signed
#                   image) and the initrd, GC, configurationLimit/pins/booted, the free-space floor refusing with
#                   nothing written, a failure mid-copy leaving the old menu, custom.cfg kept byte for byte,
#                   ostree's loader never written, host.toml layout and kernel_params, the read-only check mode.
#   boot-hook-vm    a NixOS VM where switch-to-configuration boot runs the hook for real: generations 1 and 2 are
#                   listed, the default follows nix-env --switch-generation (rime rollback), a deleted generation's
#                   entry goes, a too-small /boot refuses by name and leaves it empty, and neither the activation
#                   script nor the hook names bootctl, grub-install or efibootmgr (D3 gate).
#
# The OVMF end-to-end proof (Fedora shim + GRUB, Secure Boot on and off, a bad generation recovered through the
# menu) needs Fedora's signed binaries and is nix/probe/ovmf/boothook/ (README there), not a flake check.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  hook = pkgs.rime.rime-boot;
in
{
  boot-hook-unit = pkgs.runCommand "rime-boot-hook-unit" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 -I -W error::ResourceWarning ${./boot-hook-test.py} ${hook}/bin/rime-boot > $out 2>&1 \
      || { cat $out; exit 1; }
    cat $out
  '';

  boot-hook-vm = pkgs.testers.runNixOSTest {
    name = "rime-boot-hook";
    node.pkgsReadOnly = false;

    nodes.machine = { nodes, ... }: {
      imports = [ self.nixosModules.default ];
      rime.boot.enable = true;
      system.switch.enable = true; # the test framework turns it off by default
      # Generation 2's closure, so nix-env can make it a generation of the system profile inside the VM.
      virtualisation.additionalPaths = [ nodes.next.system.build.toplevel ];
    };
    # Never started: its toplevel is generation 2 (one /etc file differs; the kernel is shared, the initrd is not).
    nodes.next = { ... }: {
      imports = [ self.nixosModules.default ];
      rime.boot.enable = true;
      system.switch.enable = true;
      environment.etc."rime-boot-test-generation".text = "2\n";
    };

    testScript = { nodes, ... }:
      let
        gen1 = nodes.machine.system.build.toplevel;
        gen2 = nodes.next.system.build.toplevel;
      in
      ''
        import json
        P = "/nix/var/nix/profiles/system"
        machine.wait_for_unit("multi-user.target")

        def check():
            rc, out = machine.execute("rime-boot check")
            return rc, json.loads(out)

        with subtest("D3 gate: the activation script and the hook name no loader installer"):
            stc = machine.succeed("cat ${gen1}/bin/switch-to-configuration")
            assert "${nodes.machine.system.build.rimeBootHook}" in stc, stc
            # rime-boot itself names them only in its docstring; its package's install check proves it runs no
            # program at all (no subprocess/exec/spawn/popen/system).
            machine.fail("grep -lE 'bootctl|grub-install|grub2-install|efibootmgr' ${gen1}/activate "
                         "${nodes.machine.system.build.rimeBootHook}")
            machine.fail("grep -nE 'subprocess|os[.](system|exec|spawn|popen)' ${pkgs.rime.rime-boot}/bin/rime-boot")

        with subtest("switch-to-configuration boot writes generation 1 as the default"):
            machine.succeed(f"nix-env -p {P} --set ${gen1}")
            machine.succeed("mkdir -p /boot/grub2")
            machine.succeed("${gen1}/bin/switch-to-configuration boot")
            print(machine.succeed("find /boot -type f | sort; cat /boot/grub2/rime-nixos.cfg /boot/loader/entries/*"))
            machine.succeed("test -f /boot/loader/entries/rime-generation-1.conf")
            machine.succeed("grep -x 'set default=rime-generation-1' /boot/grub2/rime-nixos.cfg")
            machine.succeed("grep -x 'options init=${gen1}/init .*' /boot/loader/entries/rime-generation-1.conf")
            rc, out = check()
            assert rc == 0 and out["default_generation"] == 1 and out["default_toplevel"] == "${gen1}", out

        with subtest("generation 2 becomes the default, generation 1 stays listed, the kernel file is shared"):
            machine.succeed(f"nix-env -p {P} --set ${gen2}")
            machine.succeed("${gen2}/bin/switch-to-configuration boot")
            entries = machine.succeed("ls /boot/loader/entries").split()
            assert entries == ["rime-generation-1.conf", "rime-generation-2.conf"], entries
            machine.succeed("grep -x 'set default=rime-generation-2' /boot/grub2/rime-nixos.cfg")
            machine.succeed("grep '^title Rime (generation 1, ' /boot/loader/entries/rime-generation-1.conf")
            files = machine.succeed("ls /boot/rime/nixos").split()
            # one kernel (named by content, shared by both generations) + each generation's own initrd + one copy
            # of each vendor's early microcode (host.toml [boot] microcode unset = both), shared by both generations
            intel = [f for f in files if f.endswith("-intel-ucode.img")]
            amd = [f for f in files if f.endswith("-amd-ucode.img")]
            assert len(intel) == 1 and len(amd) == 1, files
            rest = [f for f in files if f not in intel + amd]
            assert len(rest) == 3 and len([f for f in rest if f.endswith("bzImage.efi")]) == 1, files
            for e in ("rime-generation-1.conf", "rime-generation-2.conf"):
                lines = machine.succeed(f"grep '^initrd ' /boot/loader/entries/{e}").splitlines()
                assert len(lines) == 3 and intel[0] in lines[0] and amd[0] in lines[1], lines
            k = machine.succeed("grep -h '^linux ' /boot/loader/entries/*.conf").splitlines()
            assert len(k) == 2 and k[0] == k[1], k
            rc, out = check()
            assert rc == 0 and out["default_generation"] == 2, out

        with subtest("rime rollback: --switch-generation 1 + the hook makes generation 1 the default"):
            machine.succeed(f"nix-env -p {P} --switch-generation 1")
            rc, out = check()
            assert rc == 1 and not out["up_to_date"], out  # profile moved, menu not yet
            machine.succeed(f"{P}/bin/switch-to-configuration boot")
            machine.succeed("grep -x 'set default=rime-generation-1' /boot/grub2/rime-nixos.cfg")
            rc, out = check()
            assert rc == 0 and out["default_generation"] == 1, out

        with subtest("a deleted generation's entry goes"):
            machine.succeed(f"nix-env -p {P} --delete-generations 2")
            machine.succeed("rime-boot install")
            assert machine.succeed("ls /boot/loader/entries").split() == ["rime-generation-1.conf"]

        with subtest("a /boot too small for the kernel and initrd: refused by name, nothing written"):
            machine.succeed("mount -t tmpfs -o size=2m tmpfs /boot && mkdir /boot/grub2")
            rc, out = machine.execute(f"{P}/bin/switch-to-configuration boot 2>&1")
            print(out)
            assert rc != 0, out
            assert "REFUSED: not enough space on /boot" in out and "Nothing was written" in out, out
            assert machine.succeed("find /boot -mindepth 1").split() == ["/boot/grub2"]
            machine.succeed("umount /boot")
      '';
  };
}
