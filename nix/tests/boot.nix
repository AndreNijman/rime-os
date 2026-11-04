# Rime's boot hook (nix/pkgs/rime-boot, nix/modules/boot.nix; PLAN.md D3 v1 chain):
#
#   boot-hook-unit  nix/tests/boot-hook-test.py against the packaged hook: BLS and custom-cfg layouts, default =
#                   the profile's current generation, content-addressed copies of <toplevel>/kernel (the signed
#                   image) and the initrd, GC, configurationLimit/pins/booted, the free-space floor refusing with
#                   nothing written, a failure mid-copy leaving the old menu, custom.cfg kept byte for byte,
#                   ostree's loader never written, host.toml layout and kernel_params, the read-only check mode, and
#                   the Secure Boot key block (the bootc helper's bytes and custom.cfg rule, checked against its awk).
#   boot-hook-vm    a NixOS VM where switch-to-configuration boot runs the hook for real: generations 1 and 2 are
#                   listed, the default follows nix-env --switch-generation (rime rollback), a deleted generation's
#                   entry goes, a too-small /boot refuses by name and leaves it empty, and neither the activation
#                   script nor the hook names bootctl, grub-install or efibootmgr (D3 gate). Secure Boot key: no
#                   certificate and no entry on an ESP without MokManager; with it, the DER of rime.boot.mokCertificate
#                   at EFI/rime/rime-mok.der and one "Enrol the Rime OS Secure Boot key" block, which
#                   rime-secureboot-key.service puts back.
#
# The OVMF end-to-end proof (Fedora shim + GRUB, Secure Boot on and off, a bad generation recovered through the
# menu) needs Fedora's signed binaries and is nix/probe/ovmf/boothook/ (README there), not a flake check.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  hook = pkgs.rime.rime-boot;
  # A throwaway TEST certificate (its key is discarded here) standing in for the Rime MOK in boot-hook-vm: PEM for
  # rime.boot.mokCertificate, and the DER openssl makes of it, which the hook must put on the ESP byte for byte.
  testMok = pkgs.runCommand "rime-boot-test-mok" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey rsa:2048 -nodes -keyout /dev/null -days 3650 -subj "/CN=rime-boot hook TEST MOK" \
      -out $out/cert.pem
    openssl x509 -in $out/cert.pem -outform DER -out $out/cert.der
  '';
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
      rime.boot.mokCertificate = "${testMok}/cert.pem";
      system.switch.enable = true; # the test framework turns it off by default
      # Generation 2's closure, so nix-env can make it a generation of the system profile inside the VM.
      virtualisation.additionalPaths = [ nodes.next.system.build.toplevel ];
    };
    # Never started: its toplevel is generation 2 (one /etc file differs; the kernel is shared, the initrd is not).
    nodes.next = { ... }: {
      imports = [ self.nixosModules.default ];
      rime.boot.enable = true;
      rime.boot.mokCertificate = "${testMok}/cert.pem";
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

        with subtest("Secure Boot key: an ESP without Fedora's MokManager gets neither the certificate nor the entry"):
            machine.fail("test -e /boot/EFI/rime")
            machine.fail("grep -F -- '--id rime-enrol-key' /boot/grub2/custom.cfg")
            out = machine.succeed("${gen1}/bin/switch-to-configuration boot 2>&1")
            assert "has no EFI/fedora/mmx64.efi" in out and "boot menu entry not written" in out, out

        with subtest("Secure Boot key: with MokManager on the ESP, the DER certificate and exactly one entry"):
            machine.succeed("mkdir -p /boot/EFI/fedora && printf 'MZ stub' > /boot/EFI/fedora/mmx64.efi")
            machine.succeed("${gen1}/bin/switch-to-configuration boot")
            machine.succeed("cmp /boot/EFI/rime/rime-mok.der ${testMok}/cert.der")
            machine.succeed("test \"$(grep -cx '### BEGIN rime-secureboot-key ###' /boot/grub2/custom.cfg)\" = 1")
            machine.succeed("test \"$(tail -n1 /boot/grub2/custom.cfg)\" = '### END rime-secureboot-key ###'")
            machine.succeed("grep -F -- '--id rime-enrol-key' /boot/grub2/custom.cfg")
            machine.succeed("grep -Fx '  source ''${prefix}/rime-nixos.cfg' /boot/grub2/custom.cfg")
            before = machine.succeed("sha256sum /boot/grub2/custom.cfg /boot/EFI/rime/rime-mok.der")
            # The unit the entry's own text names puts a missing certificate back.
            machine.succeed("rm /boot/EFI/rime/rime-mok.der")
            machine.succeed("systemctl start rime-secureboot-key.service")
            assert machine.succeed("sha256sum /boot/grub2/custom.cfg /boot/EFI/rime/rime-mok.der") == before

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

  # The prune-to-fit path on a REAL full ESP (wave-3 open item: boot-hook-test.py mocks free_bytes). A 256 MiB FAT32
  # disk is the ESP at /boot, statvfs and ENOSPC are the kernel's. Five generations whose initrds each carry an
  # incompressible pad, sized so that the kernel, both microcode files and three initrds fit above the 32 MiB floor
  # and a fourth does not. Generation 1 is the BOOTED one (/run/booted-system) and the oldest.
  #   gen1..gen3  installed one by one: nothing is left out, the ESP fills up;
  #   gen4        the ESP cannot take its initrd (measured in the VM first): the hook leaves out generation 2, the
  #               oldest it may (generation 1 is booted, generation 4 the default), removes its initrd, writes 4;
  #   gen5        generation 3 goes next; generation 1 is still listed and its files are still on the ESP.
  # After every install every file every entry names is on the ESP with its source's size, and `rime-boot check`
  # agrees. Mutant (docs/nixos/boot-matrix.md, "ESP budget"): a hook that never leaves a generation out copies
  # gen4's initrd into a full ESP and fails with ENOSPC (switch-to-configuration boot exits non-zero).
  # Then the pruned ESP boots: generation 5's entry (kernel, initrds, options) is kexec'd off the ESP and the VM comes
  # up as generation 5. Mutant: a hook that writes an initrd of the right size with the wrong bytes passes every size
  # check and `rime-boot check`, and fails this boot.
  # No separate Secure Boot boot of the pruned ESP (DECIDED-NOT-TO, docs/nixos/boot-matrix.md "ESP budget"): it is the
  # layout the boot matrix boots through Fedora's shim and GRUB (nix/probe/ovmf/boothook) with fewer entries, and
  # every file every remaining entry names is checked here, byte size and `rime-boot check` both.
  boot-esp-prune =
    let
      espMiB = 256;
      padMiB = 28; # measured: kernel 13.2 MiB, microcode 15.2, initrd 28.6 + pad; 3 fit in 256 MiB above 32, 4 do not
      pad = n: pkgs.runCommand "rime-esp-prune-pad-${toString n}" { nativeBuildInputs = [ pkgs.python3 ]; } ''
        python3 -I -c '
        import hashlib, sys
        out = open(sys.argv[1], "wb")
        seed = b"rime-esp-prune-" + sys.argv[2].encode()
        for i in range(${toString padMiB}):
            out.write(hashlib.shake_256(seed + i.to_bytes(4, "big")).digest(1 << 20))
        ' $out ${toString n}
      '';
      gen = n: { ... }: {
        imports = [ self.nixosModules.default ];
        rime.boot.enable = true;
        system.switch.enable = true;
        environment.etc."rime-boot-test-generation".text = "${toString n}\n";
        # Incompressible bytes in the (systemd) initrd: each generation's initrd is its own file of a known size.
        boot.initrd.systemd.contents."/rime-esp-pad".source = pad n;
      };
      later = [ 2 3 4 5 ];
    in
    pkgs.testers.runNixOSTest {
      name = "rime-boot-esp-prune";
      node.pkgsReadOnly = false;

      nodes = {
        machine = { nodes, pkgs, ... }: {
          imports = [ (gen 1) ];
          virtualisation.memorySize = 2048;
          virtualisation.emptyDiskImages = [ espMiB ];
          virtualisation.additionalPaths = map (n: nodes."gen${toString n}".system.build.toplevel) later;
          environment.systemPackages = [ pkgs.dosfstools pkgs.kexec-tools ];
        };
      } // lib.genAttrs (map (n: "gen${toString n}") later) (name: gen (lib.toInt (lib.removePrefix "gen" name)));

      testScript = { nodes, ... }:
        let
          top = n: if n == 1 then nodes.machine.system.build.toplevel else nodes."gen${toString n}".system.build.toplevel;
        in
        ''
          import json, re
          P = "/nix/var/nix/profiles/system"
          RESERVE = 32 * 1024 * 1024
          TOP = {${lib.concatMapStringsSep ", " (n: "${toString n}: \"${top n}\"") ([ 1 ] ++ later)}}
          machine.wait_for_unit("multi-user.target")
          assert machine.succeed("readlink -f /run/booted-system").strip() == TOP[1]

          def free():
              a, s = machine.succeed("stat -f -c '%a %S' /boot").split()
              return int(a) * int(s)

          def size(p):
              return int(machine.succeed(f"stat -L -c %s {p}").strip())

          def initrd_of(n):
              return machine.succeed(f"readlink -f {TOP[n]}/initrd").strip()

          def entries():
              return sorted(int(m) for m in re.findall(r"rime-generation-(\d+)\.conf",
                                                         machine.succeed("ls /boot/loader/entries")))

          def install(n):
              machine.succeed(f"nix-env -p {P} --set {TOP[n]}")
              rc, out = machine.execute(f"{TOP[n]}/bin/switch-to-configuration boot 2>&1")
              print(f"--- install generation {n}: rc={rc}\n{out}")
              return rc, out

          def consistent(want):
              """Every listed generation's entry names files that are on the ESP with the source's size."""
              assert entries() == want, (entries(), want)
              on_esp = set(machine.succeed("ls /boot/rime/nixos").split())
              named = set()
              for n in want:
                  e = machine.succeed(f"cat /boot/loader/entries/rime-generation-{n}.conf")
                  for kind, path in re.findall(r"^(linux|initrd) (\S+)$", e, re.M):
                      name = path.rsplit("/", 1)[1]
                      named.add(name)
                      assert name in on_esp, (n, name, sorted(on_esp))
                  assert f"init={TOP[n]}/init " in e, e
                  got = size(f"/boot/rime/nixos/$(sed -n 's|^initrd .*/||p' /boot/loader/entries/rime-generation-{n}.conf | tail -1)")
                  assert got == size(initrd_of(n)), (n, got)
              # nothing on the ESP that no entry names (the pruned generation's initrd is gone)
              assert on_esp == named, sorted(on_esp ^ named)
              rc, out = machine.execute("rime-boot check")
              c = json.loads(out)
              assert rc == 0 and c["up_to_date"], out
              assert free() >= RESERVE, f"the hook went below its floor: {free()} B free"

          with subtest("a 256 MiB FAT32 ESP at /boot, the booted generation 1 installed"):
              machine.succeed("mkfs.vfat -F 32 -n RIMEESP /dev/vdb && mkdir -p /boot "
                              "&& mount -t vfat -o umask=0077 /dev/vdb /boot && mkdir /boot/grub2")
              rc, out = install(1)
              assert rc == 0 and "left out" not in out, out
              consistent([1])

          with subtest("generations 2 and 3 fit: nothing is left out, the ESP fills"):
              for n in (2, 3):
                  rc, out = install(n)
                  assert rc == 0 and "left out" not in out, out
                  consistent(list(range(1, n + 1)))
              print(f"free after generation 3: {free()} B")

          with subtest("generation 4 does not fit: the oldest optional generation (2) is left out, never the booted 1"):
              # The ESP really is full for it: measured, not assumed.
              need = size(initrd_of(4))
              assert free() < need + RESERVE, f"the ESP still has room ({free()} B free, {need} B needed): tests nothing"
              i2 = machine.succeed("sed -n 's|^initrd .*/||p' /boot/loader/entries/rime-generation-2.conf | tail -1").strip()
              rc, out = install(4)
              assert rc == 0, out
              assert re.search(r"generation 2 left out of the menu", out), out
              assert not re.search(r"generation [13] left out", out), out
              consistent([1, 3, 4])
              machine.fail(f"test -e /boot/rime/nixos/{i2}")
              machine.succeed("grep -x 'set default=rime-generation-4' /boot/grub2/rime-nixos.cfg")
              # still in the profile: only the menu left it out
              machine.succeed(f"test -e {P}-2-link")

          with subtest("generation 5: generation 3 goes next; the booted generation 1 stays listed and on the ESP"):
              rc, out = install(5)
              assert rc == 0, out
              assert re.search(r"generation 3 left out of the menu", out), out
              # generation 2 is still in the profile, so it is weighed and left out again; 1 and 4 never are
              assert not re.search(r"generation [14] left out", out), out
              consistent([1, 4, 5])
              machine.succeed("grep -x 'set default=rime-generation-5' /boot/grub2/rime-nixos.cfg")

          with subtest("the pruned ESP boots: generation 5's entry, with its own files and options, kexec'd off the ESP"):
              # The test VM starts with QEMU's -kernel, so a reboot never reads the ESP. kexec loads exactly what the
              # default entry names, read off the full, pruned ESP: its kernel, its initrd files in the entry's order
              # (each padded to 4 bytes and concatenated, as GRUB hands them over) and its options line. A size check
              # cannot see a file whose bytes are wrong; a boot can (mutant: docs/nixos/boot-matrix.md "ESP budget").
              # GRUB reading the menu itself: the boot matrix (nix/probe/ovmf/boothook), the same files and layout.
              e = machine.succeed("cat /boot/loader/entries/rime-generation-5.conf")
              km = re.search(r"^linux (\S+)$", e, re.M)
              om = re.search(r"^options (.*)$", e, re.M)
              assert km and om, e
              kernel = km.group(1).rsplit("/", 1)[1]
              initrds = [p.rsplit("/", 1)[1] for p in re.findall(r"^initrd (\S+)$", e, re.M)]
              opts = om.group(1)
              assert len(initrds) >= 2 and f"init={TOP[5]}/init" in opts, e
              machine.succeed("rm -f /tmp/esp-initrd && for f in "
                              + " ".join(f"/boot/rime/nixos/{n}" for n in initrds)
                              + "; do cat $f >> /tmp/esp-initrd && s=$(stat -c %s /tmp/esp-initrd)"
                              + " && head -c $(( (4 - s % 4) % 4 )) /dev/zero >> /tmp/esp-initrd; done")
              machine.succeed(f"kexec -l /boot/rime/nixos/{kernel} --initrd=/tmp/esp-initrd --command-line='{opts}'")
              machine.execute("systemctl kexec >&2 &", check_return=False)
              machine.connected = False
              machine.connect()
              machine.wait_for_unit("multi-user.target")
              booted = machine.succeed("readlink -f /run/booted-system").strip()
              assert booted == TOP[5], f"did not boot generation 5 off the ESP: booted {booted}"
              machine.succeed("grep -x 5 /etc/rime-boot-test-generation")
              cmd = machine.succeed("cat /proc/cmdline").strip()
              assert cmd == opts, (cmd, opts)
        '';
    };
}
