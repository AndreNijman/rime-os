# The bridge (nix/pkgs/rime-bridge, `rime migrate`; PLAN.md D1-D3, docs/nixos/p2-gates.md D1-D6, X1, X3, H9):
#
#   bridge-unit      the package's unit tests (classifier A-F, every named refusal, the NVRAM parser, the
#                    forbidden-tool scan, `rime migrate finish` on a fixture machine) and the hostlayer's [boot]
#                    table, then MUTANTS: each one breaks one rule
#                    in a copy of the program and the suite must fail (a check that passes them proves nothing).
#   bridge-refusals  a NixOS VM with crafted bootc-shaped disks, one per case: B, C, D and F are refused by name,
#                    an A without the renamed signer refuses [renamed-signer] (X3), an A whose data space is below
#                    floor + old store refuses [free-space] (H9), an A whose metadata space cannot hold home's copied
#                    metadata refuses [metadata-space] (H9), and a complete A is accepted. Every disk's sha256 is the
#                    same after `rime migrate check --disk` as before (check writes nothing). H9's metadata estimate
#                    is then held against a REAL `cp -a --reflink=always` of a home of 40,000 small files.
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
    [ "free-space-flipped" "rime-bridge.py" ''s/    p\["data_ok"\] = not data_need > data_avail/    p["data_ok"] = not data_need < data_avail/'' "H9 data comparison inverted" ]
    [ "floor-dropped" "rime-bridge.py" ''s/    data_need = FLOOR_BYTES + store/    data_need = 0 + store/'' "A11 floor not counted" ]
    # bridge-space: H9 models what start consumes (data: floor + old store; metadata: home's, copied by the reflink).
    [ "metadata-gate-dropped" "rime-bridge.py" ''s/        if not plan\["meta_ok"\]:/        if False:/'' "H9: a disk that cannot hold home's copied metadata accepted" ]
    [ "metadata-ratio-ignored" "rime-bridge.py" ''s/    meta_ratio = float(u\["meta_ratio"\])/    meta_ratio = 1.0/'' "H9: DUP metadata counted once" ]
    [ "home-as-data" "rime-bridge.py" ''s/    data_need = FLOOR_BYTES + store/    data_need = FLOOR_BYTES + store + hbytes/'' "H9: home charged as data again (the L16 refused)" ]
    [ "global-reserve-dropped" "rime-bridge.py" ''s/    reserve = int(u\["global_reserve"\])/    reserve = 0/'' "H9: the kernel's global reserve counted as free metadata" ]
    [ "share-uncapped" "rime-bridge.py" ''s/(min(1.0, hbytes \/ used) if used/((hbytes \/ used) if used/'' "H9: home's metadata share above all metadata (du counts uncompressed bytes)" ]
    [ "per-name-dropped" "rime-bridge.py" ''s/    home_meta = max(share, per_name)/    home_meta = share/'' "H9: a home of small files on a disk of large ones underestimated" ]
    [ "unallocated-twice" "rime-bridge.py" ''s/max(0, unalloc - data_draw_raw)/unalloc/'' "H9: unallocated space given to data and metadata both" ]
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
    # verify-bridge2: the commands call their gates (not only the helpers), and the verifier compares x as well as y.
    [ "start-unverified" "rime-bridge.py" ''s/^    man = verify_payload(a.payload)$/    man = json.load(open(os.path.join(a.payload, "manifest.json")))/'' "start installs an unsigned or tampered payload" ]
    [ "start-without-preview" "rime-bridge.py" ''s/^    rc, text = carry_preview(a.payload, man)$/    rc, text = carry_preview(a.payload, man) if "preview" in man else (0, "")/'' "start begins without showing the left-behind report" ]
    [ "check-carry-refusal-dropped" "rime-bridge.py" ''s/                if rc:$/                if False:/'' "check says bridgeable when the carry plan fails" ]
    [ "key-export-same-disk" "rime-bridge.py" ''s/    if b.get("disk") and b\["disk"\] == (facts.get("root") or {}).get("disk"):/    if False:/'' "key-export writes the keys onto the disk they unlock" ]
    [ "manifest-paths-unchecked" "rime-bridge.py" ''s/    if not re.fullmatch(r"\/nix\/store\/\[0-9a-z\]{32}-\[^\/\]+", man\["toplevel"\]) or not re.fullmatch(/    if False and not re.fullmatch(/'' "a signed toplevel outside /nix/store chrooted into" ]
    [ "ed25519-y-only" "rime-bridge.py" ''s/    return (lhs\[0\] \* rhs\[2\] - rhs\[0\] \* lhs\[2\]) % _P == 0 and /    return /'' "a negated S (same y, other x) accepted" ]
    # integrate-wave3: every start step after the record exists is inside the rollback; check and start honour it.
    [ "start-early-steps-outside-rollback" "rime-bridge.py" ''s/^    except BaseException as e:$/    except BaseException as e:\n        if state["steps"][-1] in ("backup", "old side", "fsck gate before"):\n            raise/'' "a start failing in the backup, masks or first fsck gate leaves units masked and no rollback" ]
    [ "check-ignores-start-record" "rime-bridge.py" ''s/^    if "failed" in rec and last == "rolled back":$/    if True:/'' "check and start run over an unfinished start" ]
    # topology-e: the bridge reads the start record exactly as the readiness release's `rime` and rime-update-check do.
    [ "record-failed-truthy" "rime-bridge.py" ''s/^    if "failed" in rec and last == "rolled back":$/    if rec.get("failed") and last == "rolled back":/'' "a start that rolled back after a Ctrl-C (failed empty) refused start-unfinished while rime allows its verbs" ]
    [ "failed-text-empty-on-interrupt" "rime-bridge.py" ''s/^        state\["failed"\] = str(e) or type(e).__name__ .*$/        state["failed"] = str(e)/'' "an interrupted start records an empty failed (older readers refuse it)" ]
    [ "record-done-not-first" "rime-bridge.py" ''s/^    if rec.get("done") is not None:$/    if rec.get("done") is not None and last != "rolled back":/'' "a done record with rolled back last allowed (rime says Done)" ]
    [ "record-done-truthy" "rime-bridge.py" ''s/^    if rec.get("done") is not None:$/    if rec.get("done"):/'' "an empty done read as not done (rime says Done)" ]
    [ "record-steps-any-type" "rime-bridge.py" ''s/^    steps = steps if isinstance(steps, list) else \[\]$/    steps = steps or []/'' "a record whose steps is not a list crashes check" ]
    # no-drive (2026-11-10): --backup is optional; without it @pre-nixos is the restore point and is restore-tested.
    [ "backup-required-again" "rime-bridge.py" ''s/^    s.add_argument("--backup")$/    s.add_argument("--backup", required=True)/'' "a move refused without an external drive" ]
    [ "restore-point-untested" "rime-bridge.py" ''s/^            state\["restore_point"\] = test_restore_point(/            state["restore_point"] = None and test_restore_point(/'' "no off-disk backup and the snapshot never restore-tested" ]
    [ "restore-test-placeholder-trusted" "rime-bridge.py" ''s/    if not os.path.isdir(snap_home) or not same_subvolume(snap, snap_home):/    if not os.path.isdir(snap_home):/'' "a nested home's empty placeholder taken for home" ]
    [ "restore-test-compares-nothing" "rime-bridge.py" ''s/^    if files and not equal:$/    if False:/'' "a restore test that compared no file passes" ]
    [ "restore-test-no-mtime-guard" "rime-bridge.py" ''s/        if not stat.S_ISREG(b.st_mode) or (a.st_size, a.st_mtime_ns) != (b.st_size, b.st_mtime_ns):/        if not stat.S_ISREG(b.st_mode):/'' "a file written after the snapshot fails the move" ]
    [ "home-subvolume-allowed" "rime-bridge.py" ''s/^    if backup is None and home.get("in_top_level") is False:$/    if False:/'' "a home subvolume moved with no restore point" ]
    # PR #161 review: nested subvolumes, placeholders, sampling, concurrent writes, escaped names.
    [ "home-nested-allowed" "rime-bridge.py" ''s/^    if home.get("nested_subvolumes"):$/    if False:/'' "a subvolume nested in home left behind by the copy" ]
    [ "home-layout-ancestor-ignored" "rime-bridge.py" ''s/^    if any(home_rel == p or home_rel.startswith(p + "\/") for p in paths):$/    if False:/'' "a subvolume /var or home taken for the top level" ]
    [ "placeholder-inode-trusted" "rime-bridge.py" ''s/^    return a.st_dev == b.st_dev and b.st_ino != BTRFS_EMPTY_SUBVOL_INO$/    return a.st_dev == b.st_dev/'' "a snapshot's empty placeholder taken for the directory" ]
    [ "restore-test-nested-trusted" "rime-bridge.py" ''s/^    if nested:$/    if False:/'' "a restore point missing a nested part of home passes" ]
    [ "restore-test-empty-snapshot-trusted" "rime-bridge.py" ''s/^    if not files and has_regular_file(live_home):$/    if False:/'' "an empty snapshot of a full home passes" ]
    [ "restore-test-no-recheck" "rime-bridge.py" ''s/^        if c is None or (c.st_size, c.st_mtime_ns, c.st_ino) != (b.st_size, b.st_mtime_ns, b.st_ino):$/        if False:/'' "a file written while read fails the move" ]
    [ "sample-misses-the-end" "rime-bridge.py" ''s/^    return \[items\[i \* (n - 1) \/\/ (k - 1)\] for i in range(k)\]$/    return items[::max(1, n \/\/ k)][:k]/'' "the restore sample never reaches the end of home" ]
    [ "names-unescaped" "rime-bridge.py" ''s/c if c.isprintable() else/c if True else/'' "a file name writes control characters to the terminal" ]
    # The L16's first real start: absolute links in a container layer in home taken for nested subvolumes.
    [ "restore-test-follows-links" "rime-bridge.py" ''s/^        for d in \[d for d in dirs if not os.path.islink(os.path.join(root, d))$/        for d in [d for d in dirs if True/'' "a link to a directory elsewhere taken for a nested subvolume" ]
  ];
  hlMutants = [
    [ "hostlayer-no-boot" "rime-hostlayer.py" ''s/"mount", "luks", "boot"}/"mount", "luks"}/'' "[boot] table refused" ]
    [ "hostlayer-boot-device-refused" "rime-hostlayer.py" ''s/        elif k == "device":/        elif False:/'' "[boot] device (a /boot partition) refused" ]
    [ "hostlayer-boot-device-dropped" "rime-hostlayer.py" ''s/        for k in ("device", "fstype"):/        for k in ():/'' "host show drops [boot] device" ]
    [ "hostlayer-microcode-dropped" "rime-hostlayer.py" ''s/            boot\["microcode"\] = self.microcode/            pass/'' "carry drops the CPU vendor: a bridged machine loads both vendors' microcode" ]
    [ "hostlayer-microcode-swapped" "rime-hostlayer.py" ''s/"GenuineIntel": "intel", "AuthenticAMD": "amd"/"GenuineIntel": "amd", "AuthenticAMD": "intel"/'' "the vendors swapped: an Intel machine gets only AMD microcode" ]
  ];
  # `rime migrate finish` (rime-migrate-finish.py): each mutant breaks one precondition or one property of --yes.
  finishMutants = [
    [ "booted-ostree-unchecked" "rime-migrate-finish.py" ''s/    if os.path.exists(sx.ostree_booted) or re.search(r"(^|\\s)ostree=", cmdline):/    if False:/'' "finish runs on the old (ostree) system" ]
    [ "home-subvol-unchecked" "rime-migrate-finish.py" ''s/home\["root"\] != "\/@home"/False/'' "finish runs with /var/home not from @home" ]
    [ "deletes-home" "rime-migrate-finish.py" ''s/^OLD_TOP = ("@pre-nixos", "ostree")/OLD_TOP = ("@pre-nixos", "ostree", "@home")/'' "@home on the delete list (the keep guard must stop it)" ]
    [ "keep-guard-dropped" "rime-migrate-finish.py" ''s/    if not parts or parts\[0\] in KEEP_TOP or ".." in parts:/    if False:/'' "nothing stops a kept tree being deleted" ]
    [ "entries-left" "rime-migrate-finish.py" ''s/^            os.unlink(f)$/            pass/'' "the GRUB ostree entries left behind (caught at the end, not recorded done)" ]
    [ "rerun-refused" "rime-migrate-finish.py" ''s/    if fin.get("done") is not None:/    if fin:/'' "an interrupted finish can never be completed" ]
    [ "move-aside-not-idempotent" "rime-migrate-finish.py" ''s/            if os.path.lexists(src):/            if True:/'' "a re-run after the move aside fails" ]
    [ "second-run-allowed" "rime-migrate-finish.py" ''s/    if fin.get("done") is not None:/    if False:/'' "a finished machine runs finish again" ]
    [ "good-boots-ignored" "rime-migrate-finish.py" ''s/    if len(boots) < GOOD_BOOTS:/    if False:/'' "finish before NixOS has booted three times" ]
    [ "in-use-ignored" "rime-migrate-finish.py" ''s/    if users:/    if False:/'' "finish deletes files a process has open" ]
    [ "mounted-ignored" "rime-migrate-finish.py" ''s/    if mounted:/    if False:/'' "finish deletes a mounted old side" ]
    [ "stats-ignored" "rime-migrate-finish.py" ''s/    elif stats:/    elif False:/'' "finish deletes on a filesystem that reports errors" ]
    [ "containment-dropped" "rime-migrate-finish.py" ''s/        if diff:/        if False:/'' "a change to custom.cfg, /boot/rime, an ESP or a kept subvolume not seen" ]
    [ "record-not-saved" "rime-migrate-finish.py" ''s/^    if not os.path.exists(os.path.join(sx.state, "start-record.json")):/    if False:/'' "the start record deleted with ostree/: an interrupted run cannot resume" ]
    [ "nested-shallow-first" "rime-migrate-finish.py" ''s/key=lambda s: (-s.count("\/"), s))/key=lambda s: (s.count("\/"), s))/'' "a nested subvolume deleted before the ones inside it" ]
    [ "dry-run-writable" "rime-migrate-finish.py" ''s/writable=a.yes/writable=True/'' "the dry run mounts the top level read-write" ]
    [ "menu-unchecked" "rime-migrate-finish.py" ''s/    if not n:/    if False:/'' "the ostree entries removed with no NixOS menu to boot" ]
    [ "menu-top-level-only" "rime-migrate-finish.py" ''s|for p in ("/boot/rime/", "/rime/")|for p in ("/boot/rime/",)|'' "a machine with a /boot partition (the encrypted layout) refused for a menu it has" ]
    [ "pre-nixos-moved" "rime-migrate-finish.py" ''s/^MOVE_ASIDE = ("ostree",)/MOVE_ASIDE = OLD_TOP/'' "the read-only @pre-nixos moved aside (btrfs refuses: EROFS)" ]
    [ "entries-not-committed" "rime-migrate-finish.py" ''s/^        sx.commit(sx.boot)      # before/        pass  # before/'' "the entries' removal only in btrfs's log: GRUB lists the old system after a power cut" ]
    [ "os-error-unrecorded" "rime-migrate-finish.py" ''s/    except (Failed, OSError) as e:/    except Failed as e:/'' "a refused rename leaves a traceback and no failed record" ]
    [ "record-done-ignored" "rime-migrate-finish.py" ''s/    if r is None or r\[0\] != "already-moved":/    if False:/'' "finish over a start that did not finish" ]
    [ "finish-claims-a-backup" "rime-migrate-finish.py" ''s/^                if rec.get("backup") else$/                if True else/'' "finish points at an off-disk backup start never took" ]
  ];
  runFinishMutant = m:
    let
      name = builtins.elemAt m 0;
      expr = builtins.elemAt m 2;
      why = builtins.elemAt m 3;
    in
    ''
      # finish loads the bridge beside it: each mutant gets a directory with both.
      mkdir fm-${name}
      sed "1s|^#!.*|#!$(command -v python3) -I|" ${src}/rime-bridge.py > fm-${name}/rime-migrate
      sed "1s|^#!.*|#!$(command -v python3) -I|" ${src}/rime-migrate-finish.py > fm-${name}/base
      cp fm-${name}/base fm-${name}/rime-migrate-finish
      sed -i ${lib.escapeShellArg expr} fm-${name}/rime-migrate-finish
      if cmp -s fm-${name}/base fm-${name}/rime-migrate-finish; then echo "mutant ${name}: sed changed nothing"; exit 1; fi
      if RIME_MIGRATE_FINISH=$PWD/fm-${name}/rime-migrate-finish python3 -I -B -m unittest discover -s ${src}/tests \
          -p 'test_finish*.py' > fm-${name}.log 2>&1; then
        echo "mutant ${name} SURVIVED (${why})"; cat fm-${name}.log; exit 1
      fi
      echo "mutant ${name} killed (${why}): $(grep -cE '^(FAIL|ERROR):' fm-${name}.log) failing tests"
    '';
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

  # N small files under DIR, 100 per directory, 1-3000 bytes each (seeded): what a home of dotfiles and sources looks
  # like to btrfs (under one sector: inline, in the metadata tree).
  smallFiles = pkgs.writeText "small-files.py" ''
    import os, random, sys
    d, n = sys.argv[1], int(sys.argv[2])
    rnd = random.Random(9)
    for i in range(n):
        sub = os.path.join(d, f"d{i // 100:04d}")
        if i % 100 == 0:
            os.makedirs(sub, exist_ok=True)
        with open(os.path.join(sub, f"file-{i:06d}.txt"), "wb") as f:
            f.write(rnd.randbytes(rnd.randint(1, 3000)))
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
    RIME_MIGRATE=${bridge}/bin/rime-migrate RIME_MIGRATE_FINISH=${bridge}/bin/rime-migrate-finish python3 -I -B -W error::ResourceWarning \
      -m unittest discover -v -s ${src}/tests -p 'test_*.py' 2>&1 | tail -n 5
    ${lib.concatMapStrings (runMutant src "${src}/tests" "RIME_MIGRATE") mutants}
    # The hostlayer suite unmutated first: its mutants count only if it is green here.
    sed "1s|^#!.*|#!$(command -v python3) -I|" ${../pkgs/rime-hostlayer}/rime-hostlayer.py > hl.py; chmod +x hl.py
    RIME_HOSTLAYER=$PWD/hl.py python3 -I -m unittest discover -s ${../pkgs/rime-hostlayer}/tests \
      -p 'test_*.py' > hostlayer.log 2>&1 || { cat hostlayer.log; exit 1; }
    tail -n 3 hostlayer.log
    ${lib.concatMapStrings (runMutant ../pkgs/rime-hostlayer "${../pkgs/rime-hostlayer}/tests" "RIME_HOSTLAYER")
      hlMutants}
    ${lib.concatMapStrings runFinishMutant finishMutants}

    # The payload signature end to end with OpenSSL, in the readiness release's format: payload-sign.sh (the
    # release's signer) signs with a Nix-format secret key; the signature verifies with OpenSSL (pkeyutl -verify
    # -rawin, DER SubjectPublicKeyInfo: the check payload-sign.sh runs after signing) AND with the bridge's own ed25519,
    # the ONLY verifier on the bootc side (the readiness release's `rime migrate` execs the bridge with no gate of its
    # own), reading the key file the bootc image ships.
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

    echo "bridge-unit: suite green, ${toString (builtins.length mutants + builtins.length hlMutants + builtins.length finishMutants)} mutants killed" | tee $out
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
      }) [ "caseA" "caseB" "caseC" "caseD" "caseF" "caseX3" "caseH9" "caseL" "caseU" "caseM" ]
      # H9's metadata estimate against a real reflink copy: a disk big enough for a home of 40,000 small files
      ++ [ { size = 4096; driveConfig.deviceExtraOpts.serial = "caseR"; } ];
      environment.systemPackages = [ bridge pkgs.dosfstools pkgs.btrfs-progs pkgs.gptfdisk pkgs.util-linux
        pkgs.cryptsetup pkgs.e2fsprogs pkgs.python3 ];
      boot.kernelModules = [ "dm_crypt" ];
    };

    testScript = ''
      machine.wait_for_unit("multi-user.target")
      SIGNER = "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main"
      DEP = "ostree/deploy/default/deploy/" + "ab" * 32 + ".0"

      def mk(case, esp_dirs, signer=True, phase=None, composefs=False, home_mb=1, store_mb=1, windows=False,
             luks=False, grub_cfg="${bootupdStatic}", home_small=0, fill=False):
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
          if home_small:
              # a home of many small files (dotfiles, sources, caches): 100 per directory, 1-3000 bytes, seeded
              machine.succeed(f"python3 ${smallFiles} {root}/var/home/alex/src {home_small}")
          if fill:
              # the rest of the disk taken by data outside home and the store, then every other 8 MiB file deleted:
              # free space left INSIDE data chunks (they stay allocated), nothing unallocated for new metadata
              machine.succeed("mkdir -p /mnt/r/fill && i=0; while dd if=/dev/zero of=/mnt/r/fill/$i bs=1M count=8"
                              " status=none 2>/dev/null; do i=$((i+1)); done; rm -f /mnt/r/fill/$i; sync"
                              " && for f in /mnt/r/fill/*[02468]; do rm -f $f; done; sync"
                              " && btrfs filesystem sync /mnt/r")
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

      with subtest("H9: data space below floor + old store (home is not charged: it is a reflink copy)"):
          check("H9", mk("caseH9", ["fedora"], home_mb=300, store_mb=300), 3,
                ["topology: A", "REFUSED [free-space] data:", "floor 0.3 + old store 0.3",
                 "home is a reflink copy: no data at start"], FLOOR)

      with subtest("H9: home's copied metadata does not fit (metadata chunks + unallocated) and is refused by name"):
          check("M", mk("caseM", ["fedora"], home_small=60000, fill=True), 3,
                ["topology: A", "REFUSED [metadata-space] metadata:", "global reserve"], 50 * 1000**2)
          # The copy the gate refused, really run (the disk is a throwaway): printed, not asserted. The gate is meant
          # to refuse before this point, with a margin; ENOSPC here says the refusal was needed, success that the
          # margin was not all used.
          d = "/dev/disk/by-id/virtio-caseM"
          machine.succeed(f"mount {d}-part2 /mnt/r && btrfs subvolume create /mnt/r/@home")
          rc, out = machine.execute("cp -a --reflink=always --one-file-system /mnt/r/ostree/deploy/default/var/home/."
                                    " /mnt/r/@home 2>&1 | tail -n 3; sync; btrfs filesystem usage -b /mnt/r")
          print(f"M: the refused copy, run anyway: rc={rc}\n{out}")
          machine.succeed("umount /mnt/r")

      with subtest("H9: the metadata estimate against a REAL reflink copy (start's snapshot + cp)"):
          import json as _json, re
          d = mk("caseR", ["fedora"], store_mb=1500, home_small=40000)
          j = _json.loads(machine.succeed(f"RIME_MIGRATE_FLOOR_BYTES={FLOOR} rime-migrate check --json --disk {d}"))
          sp = j["space"]
          assert sp["data_ok"] and sp["meta_ok"], sp
          def used(path):
              t = machine.succeed(f"btrfs filesystem sync {path} && btrfs filesystem usage -b {path}")
              found = {}
              for m in re.finditer(r"^(Data|Metadata),[^:]*:\s*Size:\d+,\s*Used:(\d+)", t, re.M):
                  found[m.group(1)] = int(m.group(2))
              assert set(found) == {"Data", "Metadata"}, t
              return found["Data"], found["Metadata"]
          machine.succeed(f"mount {d}-part2 /mnt/r")
          d0, m0 = used("/mnt/r")
          machine.succeed("btrfs subvolume snapshot -r /mnt/r /mnt/r/@pre-nixos && btrfs subvolume create /mnt/r/@home")
          d1, m1 = used("/mnt/r")
          machine.succeed("cp -a --reflink=always --one-file-system /mnt/r/ostree/deploy/default/var/home/. /mnt/r/@home")
          d2, m2 = used("/mnt/r")
          machine.succeed("umount /mnt/r")
          copied = m2 - m0
          print(f"R: home {sp['home']} B, {sp['entries']} names, {sp['small_bytes']} B under 4 KiB; estimate: share "
                f"{sp['share']}, per name {sp['per_name']}, home_meta {sp['home_meta']}, need {sp['meta_need']} "
                f"(logical, before x ratio {sp['meta_ratio']})")
          print(f"R: measured: snapshot + subvolume metadata {m1 - m0} B, data {d1 - d0} B; after the copy metadata "
                f"+{copied} B ({copied / max(1, sp['entries']):.0f} B per name), data +{d2 - d0} B; estimate / measured "
                f"= {sp['home_meta'] / max(1, copied):.2f} (before the {sp['margin']} margin)")
          assert copied <= sp["home_meta"], f"the reflink copy took {copied} B of metadata, the estimate is {sp['home_meta']}"
          assert d2 - d0 <= sp["home"] // 20 + 64 * 1024**2, f"the reflink copy took {d2 - d0} B of DATA"

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
