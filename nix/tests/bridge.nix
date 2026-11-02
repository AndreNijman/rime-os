# The bridge (nix/pkgs/rime-bridge, `rime migrate`; PLAN.md D1-D3, docs/nixos/p2-gates.md D1-D6, X1, X3, H9):
#
#   bridge-unit      the package's unit tests (classifier A-F, every named refusal, the NVRAM parser, the
#                    forbidden-tool scan) and the hostlayer's [boot] table, then MUTANTS: each one breaks one rule
#                    in a copy of the program and the suite must fail (a check that passes them proves nothing).
#   bridge-refusals  a NixOS VM with crafted bootc-shaped disks, one per case: B, C, D and F are refused by name,
#                    an A without the renamed signer refuses [renamed-signer] (X3), an A whose free space is below
#                    floor + home + old store refuses [free-space] (H9), and a complete A is accepted. Every disk's
#                    sha256 is the same after `rime migrate check --disk` as before (check writes nothing).
#
# The end-to-end proof (a disk made from the published image, check + repair-e + start in an OVMF guest, NixOS boots
# with the old password and home, the ostree entry still boots) needs the published image and Fedora's firmware
# chain: nix/probe/ovmf/bridge/ (README there), not a flake check.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  bridge = pkgs.rime.rime-bridge;
  hostlayer = pkgs.rime.rime-hostlayer;
  src = ../pkgs/rime-bridge;
  # name, file, sed expression, what it breaks. Each must make the suite FAIL.
  mutants = [
    [ "no-phase-failed" "rime-bridge.py" ''s/if mig.get("phase") == "failed":/if False:/'' "E (phase=failed) is classified as its base" ]
    [ "no-signer-check" "rime-bridge.py" ''s/if not (facts.get("signer") or {}).get("renamed_known"):/if False:/'' "X3 refusal gone" ]
    [ "free-space-flipped" "rime-bridge.py" ''s/and free < need:/and free > need:/'' "H9 comparison inverted" ]
    [ "floor-dropped" "rime-bridge.py" ''s/return FLOOR_BYTES + int(/return 0 + int(/'' "A11 floor not counted" ]
    [ "accept-B" "rime-bridge.py" ''s/^ACCEPTED = ("A", "E")/ACCEPTED = ("A", "B", "E")/'' "topology B bridged" ]
    [ "chain-no-configfile" "rime-bridge.py" ''s/if "configfile" not in g.get("grub_cfg", ""):/if False:/'' "D3 chain check gone" ]
    [ "fallback-unconditional" "rime-bridge.py" ''s/ and has_dir(esp, "fedora") and esp.get("fallback_csv"):/:/'' "removable path trusted without shim's fallback" ]
    [ "wait-on-finished-oneshot" "rime-bridge.py" ''s/in ("activating", "deactivating", "reloading")/in ("active", "activating", "deactivating", "reloading")/'' "start hangs on a RemainAfterExit unit that finished" ]
    [ "chroot-by-name" "rime-bridge.py" ''s/\[shutil.which("chroot") or "\/usr\/sbin\/chroot", t/["chroot", t/'' "chroot looked up in the target's PATH" ]
    [ "containment-names-only" "rime-bridge.py" ''s/if before\[k\]\[x\] != after\[k\]\[x\]})/if False})/'' "H8: a changed file whose name stayed is not seen" ]
    [ "containment-all-custom" "rime-bridge.py" ''s/return hashlib.sha256(re.sub(r"### BEGIN rime-boot.*?### END rime-boot ###\\n?", "", text,/return hashlib.sha256(re.sub(r".*", "", text,/'' "H8: custom.cfg outside the block not compared" ]
    [ "backup-same-disk" "rime-bridge.py" ''s/elif b.get("disk") and b.get("disk") == root.get("disk"):/elif False:/'' "backup on the migrated disk" ]
  ];
  hlMutants = [
    [ "hostlayer-no-boot" "rime-hostlayer.py" ''s/"mount", "luks", "boot"}/"mount", "luks"}/'' "[boot] table refused" ]
  ];
  runMutant = dir: tests: env: m:
    let
      name = builtins.elemAt m 0;
      file = builtins.elemAt m 1;
      expr = builtins.elemAt m 2;
      why = builtins.elemAt m 3;
    in
    ''
      cp ${dir}/${file} mut-${name}.py
      sed -i ${lib.escapeShellArg expr} mut-${name}.py
      if cmp -s ${dir}/${file} mut-${name}.py; then echo "mutant ${name}: sed changed nothing"; exit 1; fi
      if ${env}=$PWD/mut-${name}.py python3 -I -m unittest discover -s ${tests} -p 'test_*.py' > mut-${name}.log 2>&1; then
        echo "mutant ${name} SURVIVED (${why})"; cat mut-${name}.log; exit 1
      fi
      echo "mutant ${name} killed (${why}): $(grep -cE '^(FAIL|ERROR):' mut-${name}.log) failing tests"
    '';
in
{
  bridge-unit = pkgs.runCommand "rime-bridge-unit" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    set -euo pipefail
    RIME_MIGRATE=${bridge}/bin/rime-migrate python3 -I -W error::ResourceWarning \
      -m unittest discover -v -s ${src}/tests -p 'test_*.py' 2>&1 | tail -n 5
    ${lib.concatMapStrings (runMutant src "${src}/tests" "RIME_MIGRATE") mutants}
    ${lib.concatMapStrings (runMutant ../pkgs/rime-hostlayer "${../pkgs/rime-hostlayer}/tests" "RIME_HOSTLAYER")
      hlMutants}
    echo "bridge-unit: suite green, ${toString (builtins.length mutants + builtins.length hlMutants)} mutants killed" | tee $out
  '';

  bridge-refusals = pkgs.testers.runNixOSTest {
    name = "rime-bridge-refusals";
    nodes.machine = { ... }: {
      virtualisation.memorySize = 2048;
      # one 1 GiB disk per case (qcow2, empty), by serial: /dev/disk/by-id/virtio-<case>
      virtualisation.emptyDiskImages = map (c: {
        size = 1024;
        driveConfig.deviceExtraOpts.serial = c;
      }) [ "caseA" "caseB" "caseC" "caseD" "caseF" "caseX3" "caseH9" ];
      environment.systemPackages = [ bridge pkgs.dosfstools pkgs.btrfs-progs pkgs.gptfdisk pkgs.util-linux ];
    };

    testScript = ''
      machine.wait_for_unit("multi-user.target")
      SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main"
      DEP = "ostree/deploy/default/deploy/" + "ab" * 32 + ".0"

      def mk(case, esp_dirs, signer=True, phase=None, composefs=False, home_mb=1, store_mb=1, windows=False):
          d = f"/dev/disk/by-id/virtio-{case}"
          machine.succeed(f"sgdisk -Z {d} -n 1:2048:+128M -t 1:ef00 -n 2:0:0 -t 2:8300 {d} && udevadm settle")
          machine.succeed(f"mkfs.vfat -F 32 {d}-part1 && mkfs.btrfs -q -L rime-root {d}-part2")
          uuid = machine.succeed(f"blkid -s UUID -o value {d}-part2").strip()
          machine.succeed(f"mkdir -p /mnt/e /mnt/r && mount {d}-part1 /mnt/e && mount {d}-part2 /mnt/r")
          for n in esp_dirs + (["Microsoft"] if windows else []):
              machine.succeed(f"mkdir -p /mnt/e/EFI/{n}")
              machine.succeed(f"printf 'search --fs-uuid $BOOT_UUID --set prefixdev\\nconfigfile ($prefixdev)/grub2/grub.cfg\\n'"
                              f" > /mnt/e/EFI/{n}/grub.cfg")
              machine.succeed(f"echo 'set BOOT_UUID=\"{uuid}\"' > /mnt/e/EFI/{n}/bootuuid.cfg")
          root = "/mnt/r/" + ("state/os/default" if composefs else "ostree/deploy/default")
          if composefs:
              machine.succeed("mkdir -p /mnt/r/composefs /mnt/r/state/os/default/var/home/alex")
          else:
              machine.succeed(f"mkdir -p /mnt/r/{DEP}/usr/bin /mnt/r/ostree/repo/objects /mnt/r/ostree/deploy/default/var/home/alex")
              machine.succeed(f"printf 'ELF... %s ...' {'%s' % SIGNER if signer else 'https://github.com/AndreNijman/apex-os/x'}"
                              f" > /mnt/r/{DEP}/usr/bin/rime")
              machine.succeed(f"head -c {store_mb}M /dev/urandom > /mnt/r/ostree/repo/objects/blob")
          machine.succeed(f"head -c {home_mb}M /dev/urandom > {root}/var/home/alex/blob")
          if phase:
              machine.succeed(f"mkdir -p {root}/var/lib/rime/boot-migrate && echo {phase} > {root}/var/lib/rime/boot-migrate/phase")
          machine.succeed("mkdir -p /mnt/r/boot/grub2 && echo 'if [ -f $prefix/custom.cfg ]; then source $prefix/custom.cfg; fi'"
                          " > /mnt/r/boot/grub2/grub.cfg")
          machine.succeed("sync && umount /mnt/e /mnt/r")
          return d

      def check(case, d, want_rc, want, floor=None):
          before = machine.succeed(f"sha256sum < {d}").split()[0]
          env = f"RIME_MIGRATE_FLOOR_BYTES={floor} " if floor is not None else ""
          rc, out = machine.execute(f"{env}rime-migrate check --explain --disk {d}")
          after = machine.succeed(f"sha256sum < {d}").split()[0]
          print(f"--- {case}: rc={rc}\n{out}")
          assert rc == want_rc, f"{case}: rc {rc}, want {want_rc}"
          for w in want:
              assert w in out, f"{case}: {w!r} not in output"
          assert before == after, f"{case}: check changed the disk ({before} -> {after})"
          print(f"{case}: disk sha256 {before} unchanged")

      FLOOR = 300 * 1000**2   # a lab floor so the 1 GiB disks can show both sides of H9 (unit tests cover 19 GB)

      with subtest("A (installer default) is accepted, nothing written"):
          check("A", mk("caseA", ["fedora"]), 0, ["topology: A", "verdict: topology A can be bridged"], FLOOR)

      with subtest("B (GRUB in the Windows ESP) refused by name"):
          check("B", mk("caseB", ["fedora"], windows=True), 3, ["topology: B", "REFUSED [topology-B]"], FLOOR)

      with subtest("C (\\EFI\\rimeinst\\) refused by name"):
          check("C", mk("caseC", ["rimeinst", "fedora"]), 3, ["topology: C", "REFUSED [topology-C]"], FLOOR)

      with subtest("D (legacy \\EFI\\APEX\\) refused by name"):
          check("D", mk("caseD", ["APEX"]), 3, ["topology: D", "REFUSED [topology-D]"], FLOOR)

      with subtest("F (confirmed boot-v2, composefs) refused by name"):
          check("F", mk("caseF", ["systemd"], phase="confirmed", composefs=True), 3,
                ["topology: F", "REFUSED [topology-F]"], FLOOR)

      with subtest("X3: an A that never took the renamed-signer release"):
          check("X3", mk("caseX3", ["fedora"], signer=False), 3, ["topology: A", "REFUSED [renamed-signer]"], FLOOR)

      with subtest("H9: free space below floor + home + old store"):
          check("H9", mk("caseH9", ["fedora"], home_mb=300, store_mb=300), 3, ["topology: A", "REFUSED [free-space]"],
                FLOOR)

      with subtest("the real floor (19 GB) refuses a 1 GiB A"):
          check("A-real-floor", "/dev/disk/by-id/virtio-caseA", 3, ["REFUSED [free-space]", "floor 19.0"])
    '';
  };
}
