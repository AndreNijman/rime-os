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
    [ "chain-no-configfile" "rime-bridge.py" ''s/    if not missing\["bootupd-static"\]:/    if True:/'' "D3 chain check gone (any grub.cfg read as bootupd's)" ]
    [ "chain-target-unchecked" "rime-bridge.py" ''s/    if target != (boot.get("uuid") or "").lower():/    if False:/'' "D3: a grub.cfg that reads another filesystem trusted" ]
    [ "fallback-unconditional" "rime-bridge.py" ''s/ and has_dir(esp, "fedora") and esp.get("fallback_csv"):/:/'' "removable path trusted without shim's fallback" ]
    [ "wait-on-finished-oneshot" "rime-bridge.py" ''s/in ("activating", "deactivating", "reloading")/in ("active", "activating", "deactivating", "reloading")/'' "start hangs on a RemainAfterExit unit that finished" ]
    [ "chroot-by-name" "rime-bridge.py" ''s/\[shutil.which("chroot") or "\/usr\/sbin\/chroot", t/["chroot", t/'' "chroot looked up in the target's PATH" ]
    [ "containment-names-only" "rime-bridge.py" ''s/if before\[k\]\[x\] != after\[k\]\[x\]})/if False})/'' "H8: a changed file whose name stayed is not seen" ]
    [ "containment-all-custom" "rime-bridge.py" ''s/return hashlib.sha256(re.sub(r"### BEGIN rime-boot.*?### END rime-boot ###\\n?", "", text,/return hashlib.sha256(re.sub(r".*", "", text,/'' "H8: custom.cfg outside the block not compared" ]
    [ "backup-same-disk" "rime-bridge.py" ''s/elif b.get("disk") and b.get("disk") == root.get("disk"):/elif False:/'' "backup on the migrated disk" ]
    [ "rollback-keeps-kernels" "rime-bridge.py" ''s/if not had.get("rime") and os.path.isdir(kdir)/if False and os.path.isdir(kdir)/'' "H8 rollback leaves the hook's kernels on /boot" ]
    # bridge2 (D2 completion): the LUKS-A layout, the boot table, grub.cfg shapes, the signed payload, the preview,
    # key-export and the repair-e archive.
    [ "boot-partition-refused" "rime-bridge.py" ''s/if b.get("fstype") not in BOOT_PART_FSTYPES or not b.get("uuid"):/if True:/'' "the installer's LUKS layout refused again" ]
    [ "boot-device-dropped" "rime-bridge.py" ''s/        t\["device"\] = f"UUID={b\['uuid'\]}"/        pass/'' "host.toml never says where /boot is" ]
    [ "boot-table-appended" "rime-bridge.py" ''s/    if hdr is None:/    if True:/'' "a second [boot] table after carry's" ]
    [ "unknown-shape-guessed" "rime-bridge.py" ''s/    if shape == "unknown":/    if False:/'' "an unknown grub.cfg shape not explained" ]
    [ "stub-target-ignored" "rime-bridge.py" ''s/return "grub2-stub", re.search(UUID_RX, line).group(0).lower(), missing, cmds/return "grub2-stub", None, missing, cmds/'' "a grub2 stub's target never compared" ]
    [ "unsigned-accepted" "rime-bridge.py" ''s/    if sig_text is None or not sig_text.strip():/    if False:/'' "an unsigned payload not refused by name" ]
    [ "signature-unchecked" "rime-bridge.py" ''s/    if not ed25519_verify(keys\[name\], manifest_bytes, sig):/    if False:/'' "a tampered manifest installed" ]
    [ "signature-prefixed" "rime-bridge.py" ''s/ed25519_verify(keys\[name\], manifest_bytes, sig)/ed25519_verify(keys[name], b"rime-bridge-payload-v1\\n" + manifest_bytes, sig)/'' "a format the readiness release's verifier does not use" ]
    [ "signature-file-renamed" "rime-bridge.py" ''s/^PAYLOAD_SIG = "manifest.json.sig"/PAYLOAD_SIG = "manifest.sig"/'' "the signature read from a file the producer never writes" ]
    [ "key-compiled-in" "rime-bridge.py" ''s|^TRUSTED_PAYLOAD_KEYS = None|TRUSTED_PAYLOAD_KEYS = ("rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=",)|'' "the image's key file ignored" ]
    [ "key-file-first-line" "rime-bridge.py" ''s/    name, raw = parse_key(text) if len(text.strip().splitlines()) == 1 else (None, None)/    name, raw = parse_key(text.splitlines()[0])/'' "a key file that is not one line accepted" ]
    [ "any-key-trusted" "rime-bridge.py" ''s/    if name not in keys:/    if False:/'' "a key nobody trusts not refused by name" ]
    [ "ed25519-s-unbounded" "rime-bridge.py" ''s/if a is None or r is None or s >= _L:/if a is None or r is None:/'' "a malleable S accepted" ]
    [ "preview-preview-skipped" "rime-bridge.py" ''s/    for key in ("store", "registration", "preview"):/    for key in ("store", "registration"):/'' "preview.tar not bound to the signature" ]
    [ "preview-specials-crash" "rime-bridge.py" ''s/        tf.extractall(dest, filter=keep)/        tf.extractall(dest, filter="data")/'' "a unit mask in the payload crashes check (the first SB lab run)" ]
    [ "preview-mask-through-link" "rime-bridge.py" ''s/        if os.path.commonpath(\[parent, real\]) != real or os.path.lexists(target):/        if os.path.lexists(target):/'' "a mask written through a link out of the preview dir" ]
    [ "boot-table-clobbers-carry" "rime-bridge.py" ''s/    text = merge_boot_table(read(path, "") or "", table)/    text = merge_boot_table("", table)/'' "start's [boot] write drops what carry wrote (rd.luks.*)" ]
    [ "preview-writes" "rime-bridge.py" ''s/"--cmdline", cmdline, "--plan"\]/"--cmdline", cmdline]/'' "the preview runs a real carry" ]
    [ "key-export-no-header" "rime-bridge.py" ''s/    if luks and luks.get("device"):/    if False:/'' "the LUKS header never leaves the disk" ]
    [ "key-export-no-readback" "rime-bridge.py" ''s/        if luks.get("uuid") and luks\["uuid"\].lower() not in dump.lower():/        if False:/'' "a header backup never read back" ]
    [ "backup-without-keys" "rime-bridge.py" ''s/    export_keys(os.path.join(d, "keys"), facts)/    pass/'' "start's backup without key-export" ]
    [ "repair-e-deletes" "rime-bridge.py" ''s/            os.rename(src, dst)/            shutil.rmtree(src)/'' "the trial's /state deleted, not archived" ]
    [ "repair-e-exdev-unchecked" "rime-bridge.py" ''s/            if tree_sums(src) != tree_sums(dst):/            if False:/; s/            run(\["cp", "-a", "--reflink=auto", src, dst\])/            os.makedirs(dst)/'' "an archive copy removed the original unverified" ]
    [ "trial-top-not-pending" "rime-bridge.py" ''s/        m\["trial_top"\] = \[n for n in TRIAL_TOP if os.path.lexists(os.path.join(top, n))\]/        m["trial_top"] = []/; s/"top": list((facts.get("migration") or {}).get("trial_top", \[\]))/"top": []/'' "start runs with the trial's trees still in the top level" ]
  ];
  hlMutants = [
    [ "hostlayer-no-boot" "rime-hostlayer.py" ''s/"mount", "luks", "boot"}/"mount", "luks"}/'' "[boot] table refused" ]
    [ "hostlayer-boot-device-refused" "rime-hostlayer.py" ''s/        elif k == "device":/        elif False:/'' "[boot] device (a /boot partition) refused" ]
    [ "hostlayer-boot-device-dropped" "rime-hostlayer.py" ''s/        for k in ("device", "fstype"):/        for k in ():/'' "host show drops [boot] device" ]
  ];
  # The rime.rootfs /boot generator (nix/modules/rootfs/boot-generator.py) and its mutants.
  genSrc = ../modules/rootfs/boot-generator.py;
  genMutants = [
    [ "gen-always-bind" "boot-generator.py" ''s/    if dev is not None:/    if False:/'' "a /boot partition bound from the top level's empty /boot" ]
    [ "gen-overwrites" "boot-generator.py" ''s/    if os.path.lexists(target):/    if False:/'' "an fstab boot.mount overwritten" ]
    [ "gen-not-required" "boot-generator.py" ''s/local-fs.target.requires/local-fs.target.wants/'' "/boot not required by local-fs.target" ]
    [ "gen-fsck-unescaped" "boot-generator.py" ''s/            out.append("\\\\x%02x" % ord(c))/            out.append(c)/'' "the fsck unit name unescaped" ]
  ];
  genTest = mut: ''
    set -euo pipefail
    G=${"$"}{1:-gen.py}
    t=$(mktemp -d)
    run() { rm -rf "$t/n"; RIME_HOST_TOML="$t/host.toml" python3 -I "$G" "$t/n" "$t/e" "$t/l" 2>"$t/err"; }
    fail() { echo "FAIL: $*"; exit 1; }
    # no host.toml: the plain layout, /boot bound from the top level
    rm -f "$t/host.toml"; run
    grep -qx 'What=/mnt/rime-top/boot' "$t/n/boot.mount" || fail "no bind without host.toml"
    grep -qx 'Options=bind' "$t/n/boot.mount" || fail "bind options"
    grep -qx 'RequiresMountsFor=/mnt/rime-top' "$t/n/boot.mount" || fail "top mount dependency"
    [ "$(readlink "$t/n/local-fs.target.requires/boot.mount")" = ../boot.mount ] || fail "not required by local-fs.target"
    # the installer's LUKS layout: [boot] device + fstype from the bridge
    printf '[boot]
layout = "custom-cfg"
device = "UUID=b007b007-0000-4000-8000-000000000b00"
fstype = "ext4"
' > "$t/host.toml"; run
    grep -qx 'What=/dev/disk/by-uuid/b007b007-0000-4000-8000-000000000b00' "$t/n/boot.mount" || fail "partition What"
    grep -qx 'Type=ext4' "$t/n/boot.mount" || fail "partition Type"
    grep -qxF 'Requires=systemd-fsck@dev-disk-by\x2duuid-b007b007\x2d0000\x2d4000\x2d8000\x2d000000000b00.service' "$t/n/boot.mount"       || fail "fsck unit name ($(grep Requires= "$t/n/boot.mount"))"
    # an invalid device writes nothing (and says why); a broken host.toml falls back to the bind
    printf '[boot]
device = "UUID=x y"
' > "$t/host.toml"; run
    [ ! -e "$t/n/boot.mount" ] || fail "unit written for an invalid device"
    grep -q 'not valid' "$t/err" || fail "invalid device not explained"
    printf '[boot
' > "$t/host.toml"; run
    grep -qx 'Options=bind' "$t/n/boot.mount" || fail "broken host.toml: no fallback bind"
    # an existing boot.mount (an fstab line) stays
    rm -rf "$t/n"; mkdir -p "$t/n"; echo mine > "$t/n/boot.mount"
    RIME_HOST_TOML="$t/host.toml" python3 -I "$G" "$t/n" 2>/dev/null
    [ "$(cat "$t/n/boot.mount")" = mine ] || fail "existing boot.mount overwritten"
    rm -rf "$t"
  '';
  # A payload manifest naming two small files by sha256 (the openssl cross-check in bridge-unit signs it).
  mkManifest = pkgs.writeText "mk-manifest.py" ''
    import hashlib, json, sys
    d = sys.argv[1]
    sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
    json.dump({"toplevel": "/nix/store/" + "b" * 32 + "-nixos-system", "nix": "/nix/store/" + "c" * 32 + "-nix",
               "store": {"file": "store.tar.zst", "sha256": sha(d + "/store.tar.zst")},
               "registration": {"file": "registration", "sha256": sha(d + "/registration")}},
              open(d + "/manifest.json", "w"))
  '';
  # bootupd's static EFI grub.cfg (grub-static-efi.cfg, what every A install has in \EFI\fedora\), verbatim.
  bootupdStatic = pkgs.writeText "grub-static-efi.cfg" ''
    if [ -e (md/md-boot) ]; then
      # The search command might pick a RAID component rather than the RAID,
      # since the /boot RAID currently uses superblock 1.0.  See the comment in
      # the main grub.cfg.
      set prefix=md/md-boot
    else
      if [ -f ''${config_directory}/bootuuid.cfg ]; then
        source ''${config_directory}/bootuuid.cfg
      fi
      if [ -n "''${BOOT_UUID}" ]; then
        search --fs-uuid "''${BOOT_UUID}" --set prefix --no-floppy
      else
        search --label boot --set prefix --no-floppy
      fi
    fi
    if [ -d ($prefix)/grub2 ]; then
      set prefix=($prefix)/grub2
      configfile $prefix/grub.cfg
    else
      set prefix=($prefix)/boot/grub2
      configfile $prefix/grub.cfg
    fi
    boot
  '';
  # A hand-written menu of a shape Rime does not know (katana's real \EFI\APEX\grub.cfg is unread: P3).
  unknownShape = pkgs.writeText "unknown-shape.cfg" ''
    set timeout=5
    insmod part_gpt
    insmod btrfs
    menuentry 'Hand-made' {
      search --label rime-root --set root
      linux /boot/vmlinuz root=LABEL=rime-root
      initrd /boot/initramfs.img
    }
  '';

  runMutant = dir: tests: env: m:
    let
      name = builtins.elemAt m 0;
      file = builtins.elemAt m 1;
      expr = builtins.elemAt m 2;
      why = builtins.elemAt m 3;
    in
    ''
      # The copy runs as a program too (the hostlayer suite execs it): the sandbox has no /usr/bin/env.
      sed "1s|^#!.*|#!$(command -v python3) -I|" ${dir}/${file} > base-${name}.py
      cp base-${name}.py mut-${name}.py; chmod +x mut-${name}.py
      sed -i ${lib.escapeShellArg expr} mut-${name}.py
      if cmp -s base-${name}.py mut-${name}.py; then echo "mutant ${name}: sed changed nothing"; exit 1; fi
      if ${env}=$PWD/mut-${name}.py python3 -I -m unittest discover -s ${tests} -p 'test_*.py' > mut-${name}.log 2>&1; then
        echo "mutant ${name} SURVIVED (${why})"; cat mut-${name}.log; exit 1
      fi
      echo "mutant ${name} killed (${why}): $(grep -cE '^(FAIL|ERROR):' mut-${name}.log) failing tests"
    '';
in
{
  bridge-unit = pkgs.runCommand "rime-bridge-unit" { nativeBuildInputs = [ pkgs.python3 pkgs.openssl ]; } ''
    set -euo pipefail
    RIME_MIGRATE=${bridge}/bin/rime-migrate python3 -I -W error::ResourceWarning \
      -m unittest discover -v -s ${src}/tests -p 'test_*.py' 2>&1 | tail -n 5
    ${lib.concatMapStrings (runMutant src "${src}/tests" "RIME_MIGRATE") mutants}
    # The hostlayer suite unmutated first: its mutants count only if it is green here.
    sed "1s|^#!.*|#!$(command -v python3) -I|" ${../pkgs/rime-hostlayer}/rime-hostlayer.py > hl.py; chmod +x hl.py
    RIME_HOSTLAYER=$PWD/hl.py python3 -I -m unittest discover -s ${../pkgs/rime-hostlayer}/tests \
      -p 'test_*.py' > hostlayer.log 2>&1 || { cat hostlayer.log; exit 1; }
    tail -n 3 hostlayer.log
    ${lib.concatMapStrings (runMutant ../pkgs/rime-hostlayer "${../pkgs/rime-hostlayer}/tests" "RIME_HOSTLAYER")
      hlMutants}

    # The payload signature end to end with OpenSSL, in the readiness release's format: payload-sign.sh (the
    # release's signer) signs with a Nix-format secret key; the signature verifies with the exact command the bootc
    # side's `rime migrate start` runs (openssl pkeyutl -verify -rawin, DER SubjectPublicKeyInfo) AND with the bridge's
    # own ed25519 (an independent implementation), reading the key file the bootc image ships.
    mkdir pl
    printf store > pl/store.tar.zst; printf reg > pl/registration
    python3 -I ${mkManifest} pl
    openssl genpkey -algorithm ed25519 -out test.pem
    { openssl pkey -in test.pem -outform DER | tail -c 32; openssl pkey -in test.pem -pubout -outform DER | tail -c 32; } > secret.bin
    echo "rime-bridge-TEST-1:$(base64 -w0 secret.bin)" > secret.key
    bash ${bridge}/share/rime-bridge/payload-sign.sh secret.key pl > nixos-bridge-payload.pub
    [ "$(cut -d: -f1 pl/manifest.json.sig)" = rime-bridge-TEST-1 ] || { echo "manifest.json.sig names another key"; exit 1; }
    { printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'; cut -d: -f2 nixos-bridge-payload.pub | base64 -d; } > pub.der
    cut -d: -f2 pl/manifest.json.sig | base64 -d > sig.bin
    openssl pkeyutl -verify -pubin -keyform DER -inkey pub.der -rawin -in pl/manifest.json -sigfile sig.bin
    sed "s|^PAYLOAD_KEY_FILE = .*|PAYLOAD_KEY_FILE = \"$PWD/nixos-bridge-payload.pub\"|" ${bridge}/bin/rime-migrate > img-bridge.py
    python3 -I -c 'import importlib.machinery as m; b = m.SourceFileLoader("b", "img-bridge.py").load_module(); print("payload-sign.sh payload, bridge verifier:", b.verify_payload("pl")["signed_by"])'
    cp pl/manifest.json manifest.orig
    printf ' ' >> pl/manifest.json
    if openssl pkeyutl -verify -pubin -keyform DER -inkey pub.der -rawin -in pl/manifest.json -sigfile sig.bin >/dev/null 2>&1; then
      echo "openssl accepted a manifest changed after signing"; exit 1
    fi
    if python3 -I -c 'import importlib.machinery as m; b = m.SourceFileLoader("b", "img-bridge.py").load_module(); b.verify_payload("pl")' 2> tamper.err; then
      echo "a manifest changed after signing was accepted"; exit 1
    fi
    grep 'payload-signature-bad' tamper.err >/dev/null
    echo "one byte appended after signing: openssl refuses, bridge refuses [payload-signature-bad]"
    cp manifest.orig pl/manifest.json
    rm nixos-bridge-payload.pub
    if python3 -I -c 'import importlib.machinery as m; b = m.SourceFileLoader("b", "img-bridge.py").load_module(); b.verify_payload("pl")' 2> nokey.err; then
      echo "a payload was accepted with no key file"; exit 1
    fi
    grep 'payload-key-missing' nokey.err >/dev/null && echo "no key file in the image: refused [payload-key-missing]"
    # payload-sign.sh refuses a secret whose halves do not belong together.
    { head -c 32 secret.bin; head -c 32 /dev/zero; } | base64 -w0 | sed 's/^/rime-bridge-TEST-1:/' > bad.key
    if bash ${bridge}/share/rime-bridge/payload-sign.sh bad.key pl > /dev/null 2> bad.err; then
      echo "payload-sign.sh signed with a mismatched key"; exit 1
    fi
    grep 'does not belong' bad.err >/dev/null && echo "payload-sign.sh: a mismatched secret refused"

    echo "bridge-unit: suite green, ${toString (builtins.length mutants + builtins.length hlMutants)} mutants killed" | tee $out
  '';

  # rime.rootfs: the /boot generator against host.toml fixtures (+ mutants), and the option set evaluated both ways
  # (boot = "bootc" has no /boot in fileSystems and installs the generator; installTarget is boot = "esp").
  rootfs-unit =
    let
      eval = mods: (self.inputs.nixpkgs.lib.nixosSystem {
        modules = [ self.nixosModules.default { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; system.stateVersion = "26.11"; } ] ++ mods;
      }).config;
      migrated = eval [ { rime.rootfs.enable = true; } ];
      fresh = eval [ ../installer/target.nix { rime.installTarget = { enable = true; rootLabel = "rime-other"; }; } ];
      gen = migrated.rime.rootfs.bootGenerator;
      facts = {
        migrated_boot_in_fstab = migrated.fileSystems ? "/boot";
        migrated_top = migrated.fileSystems."/mnt/rime-top".options;
        migrated_generator = migrated.systemd.generators.rime-rootfs-boot or null;
        migrated_luks = migrated.rime.luks.enable;
        fresh_boot = fresh.fileSystems."/boot".device;
        fresh_layout = fresh.rime.boot.layout;
        fresh_label = fresh.fileSystems."/".device;
        fresh_rootfs = fresh.rime.rootfs.boot;
        fresh_generator = fresh.systemd.generators ? rime-rootfs-boot;
        compress = map (m: builtins.elem "compress-force=zstd:3" migrated.fileSystems.${m}.options) [ "/" "/nix" "/var/home" ];
      };
      ok = facts.migrated_boot_in_fstab == false && facts.migrated_top == [ "subvolid=5" ]
        && facts.migrated_generator == "${gen}" && facts.migrated_luks && facts.fresh_boot == "/dev/disk/by-label/RIME-ESP"
        && facts.fresh_layout == "bls" && facts.fresh_label == "/dev/disk/by-label/rime-other" && facts.fresh_rootfs == "esp"
        && facts.fresh_generator == false && facts.compress == [ true true true ];
    in
    assert lib.assertMsg ok "rime.rootfs evaluates wrong: ${builtins.toJSON facts}";
    pkgs.runCommand "rime-rootfs-unit" { nativeBuildInputs = [ pkgs.python3 ]; } ''
      set -euo pipefail
      echo '${builtins.toJSON facts}'
      cp ${gen} gen.py
      bash ${pkgs.writeText "gen-test.sh" (genTest null)} gen.py
      echo "generator: plain bind, LUKS-A partition, invalid device, broken host.toml, existing unit: all as specified"
      ${lib.concatMapStrings (m:
        let name = builtins.elemAt m 0; expr = builtins.elemAt m 2; why = builtins.elemAt m 3; in ''
          cp gen.py mut-${name}.py; chmod u+w mut-${name}.py
          sed -i ${lib.escapeShellArg expr} mut-${name}.py
          if cmp -s gen.py mut-${name}.py; then echo "mutant ${name}: sed changed nothing"; exit 1; fi
          if bash ${pkgs.writeText "gen-test.sh" (genTest null)} mut-${name}.py > mut-${name}.log 2>&1; then
            echo "mutant ${name} SURVIVED (${why})"; exit 1
          fi
          echo "mutant ${name} killed (${why}): $(tail -n1 mut-${name}.log)"
        '') genMutants}
      echo "rootfs-unit: evaluation as specified, generator green, ${toString (builtins.length genMutants)} mutants killed" | tee $out
    '';

  bridge-refusals = pkgs.testers.runNixOSTest {
    name = "rime-bridge-refusals";
    nodes.machine = { ... }: {
      virtualisation.memorySize = 2048;
      # one 1 GiB disk per case (qcow2, empty), by serial: /dev/disk/by-id/virtio-<case>
      virtualisation.emptyDiskImages = map (c: {
        size = 1024;
        driveConfig.deviceExtraOpts.serial = c;
      }) [ "caseA" "caseB" "caseC" "caseD" "caseF" "caseX3" "caseH9" "caseL" "caseU" ];
      environment.systemPackages = [ bridge pkgs.dosfstools pkgs.btrfs-progs pkgs.gptfdisk pkgs.util-linux
        pkgs.cryptsetup pkgs.e2fsprogs ];
      boot.kernelModules = [ "dm_crypt" ];
    };

    testScript = ''
      machine.wait_for_unit("multi-user.target")
      SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main"
      DEP = "ostree/deploy/default/deploy/" + "ab" * 32 + ".0"

      def mk(case, esp_dirs, signer=True, phase=None, composefs=False, home_mb=1, store_mb=1, windows=False,
             luks=False, grub_cfg="${bootupdStatic}"):
          d = f"/dev/disk/by-id/virtio-{case}"
          if luks:
              # The installer's LUKS layout (installer/rime-install): ESP, ext4 /boot (label rime-boot), LUKS2 root.
              machine.succeed(f"sgdisk -Z {d} -n 1:2048:+128M -t 1:ef00 -n 2:0:+128M -t 2:8300 -n 3:0:0 -t 3:8309 {d}"
                              " && udevadm settle")
              machine.succeed(f"printf lab-passphrase | cryptsetup luksFormat --type luks2 --batch-mode --label RIMEROOT"
                              f" --pbkdf pbkdf2 --pbkdf-force-iterations 1000 {d}-part3 -")
              lu = machine.succeed(f"cryptsetup luksUUID {d}-part3").strip()
              machine.succeed(f"printf lab-passphrase | cryptsetup open {d}-part3 luks-{lu} -")
              rootdev = f"/dev/mapper/luks-{lu}"
              machine.succeed(f"mkfs.vfat -F 32 {d}-part1 && mkfs.ext4 -q -L rime-boot {d}-part2"
                              f" && mkfs.btrfs -q -L rime-root {rootdev}")
              uuid = machine.succeed(f"blkid -s UUID -o value {d}-part2").strip()
              machine.succeed(f"mkdir -p /mnt/e /mnt/r /mnt/b && mount {d}-part1 /mnt/e && mount {rootdev} /mnt/r"
                              f" && mount {d}-part2 /mnt/b && mkdir -p /mnt/r/boot")
          else:
              machine.succeed(f"sgdisk -Z {d} -n 1:2048:+128M -t 1:ef00 -n 2:0:0 -t 2:8300 {d} && udevadm settle")
              machine.succeed(f"mkfs.vfat -F 32 {d}-part1 && mkfs.btrfs -q -L rime-root {d}-part2")
              uuid = machine.succeed(f"blkid -s UUID -o value {d}-part2").strip()
              machine.succeed(f"mkdir -p /mnt/e /mnt/r && mount {d}-part1 /mnt/e && mount {d}-part2 /mnt/r")
          for n in esp_dirs + (["Microsoft"] if windows else []):
              machine.succeed(f"mkdir -p /mnt/e/EFI/{n} && cp {grub_cfg} /mnt/e/EFI/{n}/grub.cfg")
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
          b = "/mnt/b" if luks else "/mnt/r/boot"
          machine.succeed(f"mkdir -p {b}/grub2 && echo 'if [ -f $prefix/custom.cfg ]; then source $prefix/custom.cfg; fi'"
                          f" > {b}/grub2/grub.cfg")
          if luks:
              # an ostree /boot partition: loader -> loader.0 with BLS entries (what is_boot_partition looks for)
              machine.succeed("mkdir -p /mnt/b/loader.0/entries && ln -s loader.0 /mnt/b/loader && umount /mnt/b")
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

      with subtest("D2: LUKS-A (the installer's encrypted layout, /boot on ext4) is bridged, the volume open"):
          check("L", mk("caseL", ["fedora"], luks=True), 0,
                ["topology: A", "/boot: partition", "ext4", "boot chain:", "shape=bootupd-static", ": verified",
                 "/boot is its own ext4 partition", "verdict: topology A can be bridged"], FLOOR)

      with subtest("an unknown \\EFI\\fedora\\grub.cfg shape: boot-chain-unverified, and what was read"):
          check("U", mk("caseU", ["fedora"], grub_cfg="${unknownShape}"), 3,
                ["REFUSED [boot-chain-unverified]", "has a shape Rime does not know", "commands set, insmod, menuentry",
                 "bootupd-static needs", "missing: source bootuuid.cfg", "shape=unknown"], FLOOR)

      with subtest("the real floor (19 GB) refuses a 1 GiB A"):
          check("A-real-floor", "/dev/disk/by-id/virtio-caseA", 3, ["REFUSED [free-space]", "floor 19.0"])
    '';
  };
}
