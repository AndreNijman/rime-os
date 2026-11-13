# The NixOS install path of installer/rime-install (rime-install-nixos.sh; PLAN.md D9), in a VM.
#
#   installer-nixos       the SAME engine the ISO runs (`rime-install --headless ANSWERS`), from a NixOS VM acting as
#                         the live environment. Before writing a byte it proves the system is a signed release
#                         (rime-install-nixos-release: the release manifest's cosign signature over a TEST Fulcio CA,
#                         the toplevel, every path's narinfo bound to the manifest, then `nix store verify` with the
#                         trusted key itself) and refuses, by name: a disk below the plain floor and a disk below
#                         the gaming floor when gaming is chosen, a partition below the gaming partition floor (the
#                         plain system fits it: dry run) (all from nix/release/floors.nix through the ISO's
#                         nixos-choices.json, nix/installer/choices.nix), an ISO with no choices list, gaming chosen
#                         on a release with no plain-gaming system, no timezone, no release record, a release signed
#                         by another identity, a tampered manifest, a release whose system is not the ISO's, a cache
#                         whose copy is not the release's (NarHash), an ISO key the release does not name, a cache
#                         signed by another key of the SAME name, another disk carrying Rime's labels, no network
#                         route and no DNS for an https-style cache, and Windows' own ESP in partition mode. Then it
#                         installs five times:
#                           whole disk from a file:// cache (GPT, btrfs subvolumes, Fedora's shim/GRUB layout with
#                             fixture binaries, the boot hook's BLS entry, host.toml, the account, the NVRAM entry, the
#                             installed release recorded the way `rime update` records one, /var/log/journal No_COW),
#                             and `rime trust --gate` passes on the installed system (nixos-enter: its own
#                             /run/current-system) against the signed release served from a registry;
#                           whole disk with gaming chosen: the release's plain-gaming toplevel (a stand-in variant of
#                             the small target), the gaming switch `rime install steam` writes, the gate passes;
#                           whole disk over http from TWO caches, Rime's and an upstream one holding the libc closure
#                             (the production cache.rimeos.com + cache.nixos.org split, rime update's per-origin copy),
#                             with the production netinstall MARKER file present (the bootc netinstall steps stay off);
#                           LUKS with a German keymap (the unlock prompt's keymap reaches the BLS entry; the D9
#                             refusal is gone), then the recovery key saved to another stick (rime-install-save-key);
#                           partition mode on a Windows-shaped disk, from a live medium on Rime's own ESP (what the
#                             Windows app stages): Windows' ESP and NTFS partition byte-identical afterwards, a GRUB
#                             entry for Windows, the one-shot "Rime OS Setup" NVRAM entry removed, the ESP named RIME-ESP.
#   installer-target-eval nixosConfigurations.rime (the base every release variant extends: what the ISO installs is
#                         the release's plain or plain-gaming toplevel built on it) evaluates, with the layout above.
#   installer-choices     the ISO's nixos-choices.json is floors.nix's published figures for plain and plain-gaming,
#                         names release variants, the shipped catalogue's gaming switch and `steam`; the engine and the
#                         ISO build state no floor of their own; the GUI shows the stamped floors and passes gaming.
#
# The real chain (Fedora shim -> GRUB -> the signed kernel, Secure Boot on, LUKS) is proven by nix/installer/lab
# (docs/nixos/installer.md), not here.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  # THROWAWAY Fulcio CA, signing identities and cache keys (nix/release/fixture-keys.nix).
  keys = pkgs.callPackage ../release/fixture-keys.nix { };

  # Where the test's release is served (a registry on the installer VM), as `rime update` / `rime trust --gate` of
  # the installed system read it from /etc/rime/update.toml.
  repo = "localhost:5000/rime/os";

  # A small system with the installer's target layout and the boot hook: what the engine installs here.
  # Through nixpkgs' flake lib (as nixosConfigurations are), so system.nixos.revision is set (base.nix's registry).
  # It trusts the TEST release identity and registry, and carries `rime`, so the installed system's own
  # `rime trust --gate` can be run on it.
  targetSystem = self.inputs.nixpkgs.lib.nixosSystem {
    modules = [
      self.nixosModules.default
      ../installer/target.nix
      {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        rime.installTarget.enable = true;
        rime.boot.enable = true;
        documentation.enable = false;
        system.stateVersion = "26.11";
        rime.update = {
          repository = repo;
          fulcioRoot = "${keys}/ca.pem";
        };
        virtualisation.containers.registries.settings.registry = [ { location = "localhost:5000"; insecure = true; } ];
      }
      # `rime trust --gate` on the installed disk. Its own module: the outer `pkgs` has no overlay, and this
      # system's `pkgs` cannot set nixpkgs.hostPlatform (it is computed from it).
      ({ pkgs, ... }: { environment.systemPackages = [ pkgs.rime.rimed ]; })
    ];
  };
  target = targetSystem.config.system.build.toplevel;
  # The release's plain-gaming system, stood in for by the small target under that variant's name (the real one,
  # nix/release/variants.nix, is installed and booted by the OVMF lab): what the installer must install when gaming is
  # chosen is the release's toplevel of that variant, whatever it holds.
  targetGaming = (targetSystem.extendModules {
    modules = [
      {
        system.name = "rime-plain-gaming";
        environment.etc."rime-test-variant".text = "plain-gaming";
      }
    ];
  }).config.system.build.toplevel;

  # What build-live-iso.sh stamps as nixos-choices.json (NIXOS_CHOICES).
  choicesData = import ../installer/choices.nix;
  choices = pkgs.writeText "nixos-choices.json" (builtins.toJSON choicesData);

  # The engine exactly as the ISO carries it (Containerfile.installer + build-live-iso.sh), shebangs made to work on
  # NixOS; the release verifier beside the checker, as /usr/lib/rime-installer holds both.
  engine = pkgs.runCommand "rime-install-under-test" { } ''
    mkdir -p $out/bin $out/libexec
    cp ${../../installer/rime-install} $out/bin/rime-install
    cp ${../../installer/rime-install-nixos.sh} $out/bin/rime-install-nixos.sh
    cp ${../../installer/rime-install-nixos-release.py} $out/bin/rime-install-nixos-release.py
    cp ${../release/rime_nix_release.py} $out/bin/rime_nix_release.py
    cp ${../../installer/rime-install-save-key} $out/bin/rime-install-save-key
    cp ${../installer/mint-test-release.py} $out/libexec/mint-test-release.py
    chmod +x $out/bin/rime-install $out/bin/rime-install-save-key
    patchShebangs $out/bin
  '';

  # rime-luks-enroll's contract (--device, --recovery-out; the passphrase in $PASSWORD) with a cheap key slot: the
  # real helper is the OVMF lab's (it needs no TPM there either). Its recovery key has systemd-cryptenroll's shape.
  enrolStub = pkgs.writeShellScript "rime-luks-enroll-TEST" ''
    set -eu
    dev=$2 out=$4
    key=$(head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n' | tr 0123456789abcdef cbdefghijklnrtuv \
          | sed 's/.\{8\}/&-/g; s/-$//')
    umask 077
    printf %s "$PASSWORD" > /run/enrol-pass; printf %s "$key" > /run/enrol-key
    ${pkgs.cryptsetup}/bin/cryptsetup luksAddKey --batch-mode --key-file /run/enrol-pass --pbkdf pbkdf2 \
      --pbkdf-force-iterations 1000 "$dev" /run/enrol-key
    printf %s "$key" > "$out"
    rm -f /run/enrol-pass /run/enrol-key
  '';
in
{
  installer-target-eval = pkgs.runCommand "rime-installer-target-eval" { } ''
    echo ${builtins.unsafeDiscardStringContext self.nixosConfigurations.rime.config.system.build.toplevel.drvPath} > $out
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (mp: fs: ''
      echo "${mp} ${fs.device} ${fs.fsType} ${lib.concatStringsSep "," fs.options}" >> $out
    '') (lib.filterAttrs (mp: _: lib.elem mp [ "/" "/nix" "/var/home" "/boot" ])
      self.nixosConfigurations.rime.config.fileSystems))}
    grep -x '/ /dev/disk/by-label/rime-root btrfs x-initrd.mount,subvol=@nixos,compress-force=zstd:3' $out
    grep -x '/nix /dev/disk/by-label/rime-root btrfs x-initrd.mount,subvol=@nix,compress-force=zstd:3' $out
    grep -x '/var/home /dev/disk/by-label/rime-root btrfs subvol=@home,compress-force=zstd:3' $out
    # /boot is the generator's (host.toml [boot] layout = "bls" -> RIME-ESP), never a build-time fstab line: the same
    # toplevel boots a machine moved from bootc (nix/tests/bridge.nix published-boot).
    if grep '^/boot ' $out; then echo "/boot is in fileSystems"; exit 1; fi
    test -n "${self.nixosConfigurations.rime.config.systemd.generators.rime-rootfs-boot or ""}"
    test "${self.nixosConfigurations.rime.config.rime.boot.layout}" = auto
    cat $out
  '';

  # The ISO's production caches (installer/nixos-production.env, build-live-iso.sh's defaults) are the ones an
  # installed system's `rime update` uses: one release, one cache, one key, whether it is installed or updated.
  # The installer's floors and its one choice are floors.nix's and the catalogue's, never numbers of its own.
  installer-choices =
    let
      floors = import ../release/floors.nix;
      variants = import ../release/variants.nix { inherit self; };
      catalogueJson = builtins.fromJSON (builtins.unsafeDiscardStringContext self.nixosConfigurations.rime.config.environment.etc."rime/catalogue.json".text);
      c = choicesData;
      eq = what: a: b: lib.optionalString (a != b) ''
        echo ${lib.escapeShellArg "installer-choices: ${what} is ${builtins.toJSON a}, expected ${builtins.toJSON b}"}; exit 1
      '';
    in
    pkgs.runCommand "rime-installer-choices" { } ''
      ${lib.concatStrings (lib.mapAttrsToList (n: v: ''
        ${eq "${n} disk_gb" v.disk_gb floors.published.${n}}
        ${eq "${n} partition_gb" v.partition_gb floors.publishedPartition.${n}}
        ${eq "${n} is a release variant" (variants ? ${n}) true}
        ${eq "${n} features" v.features (variants.${n}.selected or null)}
      '') c.variants)}
      ${eq "the variants offered" (lib.attrNames c.variants) [ "plain" "plain-gaming" ]}
      ${eq "the gaming switch" c.features.gaming.switch catalogueJson.features.gaming.switch}
      ${eq "the gaming names" (map (n: catalogueJson.entries.${n}.feature or null) c.features.gaming.names) [ "gaming" ]}
      # No floor of the engine's or the ISO build's own: the numbers come from the stamp only.
      for f in ${../../installer/rime-install-nixos.sh} ${../../installer/build-live-iso.sh}; do
        if grep -En '(MIN_GB|MIN_BYTES)=[0-9"$(]*[0-9]' "$f"; then echo "installer-choices: $f states a floor"; exit 1; fi
      done
      grep -F 'nixos-choices.json' ${../../installer/build-live-iso.sh} >/dev/null
      grep -F '"$NIXOS_STAMPS/nixos-choices.json"' ${../../installer/rime-install-nixos.sh} >/dev/null
      # The GUI shows these floors (it states none of its own) and offers gaming only when the stamp offers it.
      # Its gaming_offer() is exec'd alone: importing the GUI needs GTK.
      RIME_NIXOS_CHOICES=${choices} ${pkgs.python3}/bin/python3 -I - ${../../installer/rime-installer-gui} <<'PY'
      import json, os, re, sys
      src = open(sys.argv[1]).read()
      g = {"os": os, "json": json}
      exec(compile(src[src.index("NIXOS_CHOICES = "):src.index("def netinstall")], "gui", "exec"), g)
      o = g["gaming_offer"]()
      assert o and o["plain"] == (${toString c.variants.plain.disk_gb}, ${toString c.variants.plain.partition_gb}), o
      assert o["gaming"] == (${toString c.variants.plain-gaming.disk_gb}, ${toString c.variants.plain-gaming.partition_gb}), o
      g["NIXOS_CHOICES"] = "/nonexistent"
      assert g["gaming_offer"]() is None
      assert re.search(r'"confirm_esp_id", "gaming"\)', src), "spawn_engine does not pass gaming"
      PY
      cp ${choices} $out
    '';

  installer-production-defaults =
    let u = self.nixosConfigurations.rime.config.rime.update;
    in pkgs.runCommand "rime-installer-production-defaults" { } ''
      env=${../../installer/nixos-production.env}
      want() { grep -Fx "$1=$2" "$env" >/dev/null || { echo "nixos-production.env: $1 is not $2 (rime.update)"; exit 1; }; }
      want NIXOS_DEFAULT_CACHE_URL ${lib.escapeShellArg u.cacheUrl}
      want NIXOS_DEFAULT_CACHE_KEY ${lib.escapeShellArg (lib.head u.cacheKeys)}
      want NIXOS_DEFAULT_UPSTREAM_URL ${lib.escapeShellArg u.upstreamCacheUrl}
      want NIXOS_DEFAULT_UPSTREAM_KEY ${lib.escapeShellArg (lib.head u.upstreamCacheKeys)}
      want NIXOS_DEFAULT_RELEASE_REPO ${lib.escapeShellArg u.repository}
      test ${toString (lib.length u.cacheKeys)} = 1 && test ${toString (lib.length u.upstreamCacheKeys)} = 1
      # build-live-iso.sh reads these, and only https production URLs leave the build without a TEST seam.
      grep -F '. "$HERE/nixos-production.env"' ${../../installer/build-live-iso.sh} >/dev/null
      cp $env $out
    '';

  # What the installed system looks like on its first boot, from the configuration a release ISO installs:
  # `splash` once on the kernel command line (plymouth adds it; desktop.nix only says quiet), a Fedora-style
  # ~/.zshrc for every new account (no zsh-newuser-install wizard on first login), the staged Windows installer
  # removed from the ESP on first boot.
  installer-polish-eval =
    let c = self.nixosConfigurations.rime.config;
    in pkgs.runCommand "rime-installer-polish-eval" { } ''
      test ${toString (lib.count (p: p == "splash") c.boot.kernelParams)} = 1 \
        || { echo "splash is on the kernel command line ${toString (lib.count (p: p == "splash") c.boot.kernelParams)} times"; exit 1; }
      test ${toString (lib.count (p: p == "quiet") c.boot.kernelParams)} = 1
      ${lib.optionalString c.programs.zsh.enable ''
        test "${c.environment.etc."skel/.zshrc".mode or "missing"}" = 0644
        grep -Fx 'autoload -U compinit' ${c.environment.etc."skel/.zshrc".source}
      ''}
      test ${lib.boolToString c.programs.zsh.enable} = true
      grep -F 'rimeinst' ${c.systemd.units."rime-staged-cleanup.service".unit}/rime-staged-cleanup.service
      grep -Fx 'ConditionPathExists=/boot/rimeinst/handoff.cfg' ${c.systemd.units."rime-staged-cleanup.service".unit}/rime-staged-cleanup.service
      test -z "${c.networking.hostName}"   # the name is host.toml's (rime-hostlayer apply), never the image's
      # ... so hostnamed reads that file, not NixOS' /etc/static/hostname (absent): `hostnamectl --static`
      # said "(unset)" on the OVMF first boot until this was set.
      grep -Fx 'Environment="SYSTEMD_ETC_HOSTNAME=/etc/hostname"' \
        ${c.systemd.units."systemd-hostnamed.service".unit}/systemd-hostnamed.service \
        || { echo "systemd-hostnamed does not read /etc/hostname"; exit 1; }
      echo ok > $out
    '';

  installer-nixos = pkgs.testers.runNixOSTest {
    name = "rime-installer-nixos";

    nodes.installer = { pkgs, ... }: {
      virtualisation = {
        useEFIBoot = true;
        memorySize = 4096;
        cores = 4;
        diskSize = 12288;
        # vdb: the whole-disk target (21.5 GB, above every floor); vdc: 16 GiB = 17.2 GB, above the plain floor
        # (16 GB) and below the gaming one (19 GB); vdd: a Windows-shaped disk for partition mode; vde: a small stick
        # for the recovery key; vdf: 14 GiB = 15.0 GB, below the plain floor.
        emptyDiskImages = [ 20480 16384 24576 64 14336 ];
        additionalPaths = [ target targetGaming ];
      };
      nix.settings.experimental-features = [ "nix-command" ];
      # The release registry the installed system's `rime trust --gate` reads (update.toml repository).
      services.dockerRegistry.enable = true;
      virtualisation.containers.enable = true; # skopeo's policy.json, to publish the release into it
      # The caches the http installs fetch from, served on this machine (the engine resolves their names first).
      networking.hosts."127.0.0.1" = [ "rime-cache.test" "upstream-cache.test" ];
      environment.systemPackages = [
        engine
        pkgs.gptfdisk
        pkgs.parted
        pkgs.dosfstools
        pkgs.btrfs-progs
        pkgs.cryptsetup
        pkgs.efibootmgr
        pkgs.mtools
        pkgs.util-linux
        pkgs.python3
        pkgs.openssl
        pkgs.ntfs3g
        pkgs.findutils
        pkgs.e2fsprogs
        pkgs.skopeo
      ];
    };

    testScript = ''
      import json, shlex
      SYSTEM = "${target}"
      GAMING = "${targetGaming}"
      KEYS = "${keys}"
      REPO = "${repo}"
      PY = "${pkgs.python3}/bin/python3"
      MINT = f"{PY} -I ${engine}/libexec/mint-test-release.py --lib ${engine}/bin/rime_nix_release.py"
      ENV = ("RIME_NIXOS_STAMPS=/tmp/stamps RIME_NIXOS_EFI_SRC=/tmp/efi "
             "RIME_NIXOS_GRUB_STATIC=/tmp/grub-static")

      installer.wait_for_unit("multi-user.target")
      installer.succeed("rm -f /etc/localtime")  # no timezone in the live env: the answer has to name one

      def mint(out, *extra, key=f"{KEYS}/good.key", cert=f"{KEYS}/good.pem"):
          print(installer.succeed(
              f"{MINT} --system {SYSTEM} --also-variant plain-gaming={GAMING} --repo {REPO} --cache file:///tmp/cache "
              f"--key \"$(cat {KEYS}/cache.pub)\" --sign-key {key} --sign-cert {cert} --ca {KEYS}/ca.pem --out {out} "
              + " ".join(extra)))

      def stamps(d, release=None):
          # A copy of the ISO's stamps; RELEASE: another release record in place of the good one.
          installer.succeed(f"rm -rf {d} && cp -r /tmp/stamps {d}")
          if release:
              installer.succeed(f"rm -rf {d}/nixos-release && cp -r {release} {d}/nixos-release")

      with subtest("a TEST-signed cache holding the system, its signed release record, and the ISO's stamps"):
          installer.succeed(
              "mkdir -p /tmp/stamps /tmp/efi /tmp/grub-static/configs.d",
              f"nix copy --to 'file:///tmp/cache?compression=zstd&secret-key={KEYS}/cache.sec' {SYSTEM} {GAMING}",
              f"cp {KEYS}/cache.pub /tmp/stamps/nixos-cache.pub",
              "cp ${choices} /tmp/stamps/nixos-choices.json",
              f"echo {SYSTEM} > /tmp/stamps/nixos-system",
              "echo file:///tmp/cache > /tmp/stamps/nixos-cache",
              "echo unsigned > /tmp/stamps/kernel-signed",
              "for f in shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV; do echo fixture-$f > /tmp/efi/$f; done",
              "echo 'fixture: static efi' > /tmp/grub-static/grub-static-efi.cfg",
              "echo 'fixture: static pre' > /tmp/grub-static/grub-static-pre.cfg",
              "echo 'fixture: blscfg' > /tmp/grub-static/configs.d/10_blscfg.cfg",
          )
          mint("/tmp/stamps/nixos-release")

      with subtest("the release is served from a registry, as `rime trust --gate` of an installed system fetches it"):
          installer.wait_for_unit("docker-registry.service")
          installer.wait_for_open_port(5000)
          d = installer.succeed("sha256sum /tmp/stamps/nixos-release/manifest/manifest.json | cut -d' ' -f1").strip()
          installer.succeed(
              f"skopeo copy --quiet --preserve-digests --dest-tls-verify=false dir:/tmp/stamps/nixos-release/manifest docker://{REPO}:nix-test",
              f"skopeo copy --quiet --preserve-digests --dest-tls-verify=false dir:/tmp/stamps/nixos-release/sig docker://{REPO}:sha256-{d}.sig",
              f"test \"$(skopeo inspect --raw --tls-verify=false docker://{REPO}@sha256:{d} | sha256sum | cut -d' ' -f1)\" = {d}",
          )

      def gate(variant, top):
          # The installed system's own `rime trust --gate`, with its own /run/current-system (nixos-enter activates
          # it) and update.toml: the signed release it was installed as names this toplevel.
          installer.succeed(
              "mkdir -p /mnt && mount -o subvol=@nixos /dev/vdb2 /mnt && mount -o subvol=@nix /dev/vdb2 /mnt/nix")
          rc, out = installer.execute("nixos-enter --root /mnt -- rime trust --gate 2>&1")
          installer.succeed("umount -R /mnt")
          print(out)
          assert rc == 0, out
          assert f"system:    {top} is the signed {variant} system of 2026.11.02" in out, out
          assert "signature: verified" in out, out

      def answers(disk, **over):
          fp = installer.succeed(f"lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE {disk} | head -1").strip()
          a = dict(mode="disk", disk=disk, username="alex", password="checkpass1", hostname="rime-check",
                   timezone="Australia/Perth", locale="en_AU.UTF-8", keymap="de", encrypt="no",
                   confirmed="ERASE", confirm_target=disk, confirm_disk_id=fp)
          if over.get("mode") == "partition":
              for k, dev in (("confirm_target_id", over["target"]), ("confirm_esp_id", over["esp"])):
                  a[k] = installer.succeed(f"lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE {dev} | head -1").strip()
              a["confirm_target"] = over["target"]
          a.update(over)
          body = "".join(f"{k}={v}\n" for k, v in a.items() if v is not None)
          installer.succeed(f"printf %s {shlex.quote(body)} > /tmp/answers")

      def run(extra=""):
          return installer.execute(f"{ENV} {extra} rime-install --headless /tmp/answers 2>&1")

      def untouched(disk):
          # blkid -p exits 2 when it finds no signature at all: no partition table, no filesystem.
          installer.succeed(f"rc=0; blkid -p {disk} || rc=$?; test $rc -eq 2")

      def refused(disk, extra, *want):
          rc, out = run(extra)
          print(out)
          assert rc != 0 and "Nothing has been erased" in out, out
          for w in want:
              assert w in out, (w, out)
          untouched(disk)

      with subtest("refused by name, nothing written: a disk below the plain floor (floors.nix: ${toString choicesData.variants.plain.disk_gb} GB)"):
          answers("/dev/vdf")
          refused("/dev/vdf", "", "/dev/vdf is too small for Rime OS: 15 GB",
                  "Rime OS needs a disk of at least ${toString choicesData.variants.plain.disk_gb} GB")

      with subtest("refused by name, nothing written: gaming chosen on a disk below the gaming floor (${toString choicesData.variants.plain-gaming.disk_gb} GB)"):
          answers("/dev/vdc", gaming="yes")
          refused("/dev/vdc", "", "/dev/vdc is too small for Rime OS with gaming (Steam): 17 GB",
                  "needs a disk of at least ${toString choicesData.variants.plain-gaming.disk_gb} GB",
                  "Without gaming Rime OS needs ${toString choicesData.variants.plain.disk_gb} GB")

      with subtest("refused by name, nothing written: gaming chosen on a partition below the gaming partition floor (${toString choicesData.variants.plain-gaming.partition_gb} GB); plain fits it (dry run)"):
          # vdf2 is about 14.5 GB: above the plain partition floor (14 GB), below the gaming one (16 GB).
          installer.succeed(
              "sgdisk -n 1:0:+512M -t 1:ef00 -n 2:0:0 -t 2:8304 /dev/vdf", "udevadm settle",
              "mkfs.vfat -F 32 -n ESPTEST /dev/vdf1")
          answers("/dev/vdf", mode="partition", target="/dev/vdf2", esp="/dev/vdf1", gaming="yes")
          rc, out = run()
          print(out)
          assert rc != 0 and "Nothing has been erased" in out, out
          for w in ("/dev/vdf2 is too small for Rime OS with gaming (Steam): 14 GB",
                    "needs a partition of at least ${toString choicesData.variants.plain-gaming.partition_gb} GB",
                    "Without gaming Rime OS needs ${toString choicesData.variants.plain.partition_gb} GB"):
              assert w in out, (w, out)
          installer.succeed("t=$(blkid -p -s TYPE -o value /dev/vdf2 || true); test -z \"$t\"")
          answers("/dev/vdf", mode="partition", target="/dev/vdf2", esp="/dev/vdf1")
          rc, out = run("RIME_DRY_RUN=1")
          print(out)
          assert rc == 0 and "RIME-INSTALL-DRYRUN-OK" in out, out
          installer.succeed("t=$(blkid -p -s TYPE -o value /dev/vdf2 || true); test -z \"$t\"",
                            "wipefs -a /dev/vdf1 /dev/vdf2 /dev/vdf", "udevadm settle")

      with subtest("the same disk passes every check for the plain system (dry run: nothing written)"):
          answers("/dev/vdc")
          rc, out = run("RIME_DRY_RUN=1")
          print(out)
          assert rc == 0 and "RIME-INSTALL-DRYRUN-OK" in out, out
          untouched("/dev/vdc")

      with subtest("refused by name, nothing written: an ISO with no list of its systems and their floors"):
          stamps("/tmp/s-nochoices")
          installer.succeed("rm /tmp/s-nochoices/nixos-choices.json")
          answers("/dev/vdb")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-nochoices", "carries no list of the systems it installs")

      with subtest("refused by name, nothing written: gaming chosen, and the release has no plain-gaming system"):
          print(installer.succeed(
              f"{MINT} --system {SYSTEM} --repo {REPO} --cache file:///tmp/cache --key \"$(cat {KEYS}/cache.pub)\" "
              f"--sign-key {KEYS}/good.key --sign-cert {KEYS}/good.pem --ca {KEYS}/ca.pem --out /tmp/rel-plain"))
          stamps("/tmp/s-plainonly", "/tmp/rel-plain")
          answers("/dev/vdb", gaming="yes")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-plainonly", "release 2026.11.02 has no plain-gaming system")

      with subtest("refused by name, nothing written: a gaming answer that is neither yes nor no"):
          answers("/dev/vdb", gaming="maybe")
          refused("/dev/vdb", "", "bad gaming 'maybe'")

      with subtest("refused by name, nothing written: no timezone"):
          answers("/dev/vdb", timezone=None)
          rc, out = run()
          print(out)
          assert rc != 0 and "timezone" in out.lower() and "Nothing has been erased" in out, out
          untouched("/dev/vdb")

      answers("/dev/vdb")

      with subtest("refused by name, nothing written: an ISO with no release record"):
          stamps("/tmp/s-norel")
          installer.succeed("rm -rf /tmp/s-norel/nixos-release")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-norel", "carries no signed release record")

      with subtest("refused by name, nothing written: a release signed by another identity"):
          mint("/tmp/rel-wrong", key=f"{KEYS}/wrong.key", cert=f"{KEYS}/wrong.pem")
          stamps("/tmp/s-wrong", "/tmp/rel-wrong")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-wrong", "failed its release check",
                  "signed by https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main")

      with subtest("refused by name, nothing written: a release manifest changed after it was signed"):
          stamps("/tmp/s-tamper")
          # The manifest names its layer by digest; a layer edited after signing no longer matches it.
          installer.succeed(
              "cd /tmp/s-tamper/nixos-release/manifest && "
              f"{PY} -c 'import json,sys; m=json.load(open(\"manifest.json\")); print(m[\"layers\"][0][\"digest\"].split(\":\")[1])' > /tmp/layer",
              "sed -i 's/\"release\":\"2026.11.02\"/\"release\":\"2026.11.03\"/' /tmp/s-tamper/nixos-release/manifest/$(cat /tmp/layer)",
              "grep -F '2026.11.03' /tmp/s-tamper/nixos-release/manifest/$(cat /tmp/layer)",
          )
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-tamper", "the release manifest does not match its signed digest")

      with subtest("refused by name, nothing written: the ISO's system is not the release's"):
          stamps("/tmp/s-top")
          other = installer.succeed("readlink -f /run/current-system").strip()
          installer.succeed(f"echo {other} > /tmp/s-top/nixos-system")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-top", f"this ISO installs {other}, and release 2026.11.02's plain system is {SYSTEM}")

      with subtest("refused by name, nothing written: the cache's copy of the system is not the release's"):
          mint("/tmp/rel-narhash", "--bad-narhash")
          stamps("/tmp/s-narhash", "/tmp/rel-narhash")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-narhash",
                  f"file:///tmp/cache's copy of {SYSTEM} is not the one the signed release names (NarHash")

      with subtest("refused by name, nothing written: an ISO trusting a key the release does not name"):
          stamps("/tmp/s-otherkey")
          installer.succeed(f"cp {KEYS}/wrong-cache.pub /tmp/s-otherkey/nixos-cache.pub")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-otherkey",
                  "the ISO trusts the cache key rime-test-wrong-cache-1, and release 2026.11.02 names other keys (rime-test-cache-1)")

      with subtest("refused by name, nothing written: a cache signed by another key OF THE SAME NAME"):
          # Same paths, same NarHash, a signature line naming the right key: only the signature itself is wrong.
          # The checker's narinfo lookup cannot see that; `nix store verify` with the trusted key does.
          installer.succeed(
              "nix key generate-secret --key-name rime-test-cache-1 > /tmp/same.sec",
              f"nix copy --to 'file:///tmp/cache-same?compression=zstd&secret-key=/tmp/same.sec' {SYSTEM}",
          )
          refused("/dev/vdb", "RIME_NIXOS_CACHE=file:///tmp/cache-same",
                  "is not signed by rime-test-cache-1 (file:///tmp/cache-same)")
          installer.succeed("rm -rf /tmp/cache-same")

      with subtest("refused by name, nothing written: another disk already answers to Rime's root label"):
          installer.succeed("mkfs.btrfs -q -f -L rime-root /dev/vdc")
          refused("/dev/vdb", "", "/dev/vdc already carries the label \"rime-root\"")
          installer.succeed("wipefs -a /dev/vdc")

      with subtest("installs the prebuilt system onto the whole disk"):
          answers("/dev/vdb")
          rc, out = run("RIME_EXTRA_KARGS='console=ttyS0,115200'")
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out, out
          print(installer.succeed("cat /var/log/rime-install.log | tail -n 40"))
          assert "System:    plain" in out, out

      with subtest("GPT: a 2 GiB ESP and a Linux root partition"):
          parts = installer.succeed("lsblk -bnrpo NAME,SIZE,PARTTYPE,FSTYPE,LABEL /dev/vdb").splitlines()
          print(parts)
          esp = parts[1].split(); root = parts[2].split()
          assert esp[0] == "/dev/vdb1" and int(esp[1]) == 2048 * 1024 * 1024, esp
          assert esp[2] == "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" and esp[3:] == ["vfat", "RIME-ESP"], esp
          assert root[0] == "/dev/vdb2" and root[2] == "4f68bce3-e8cd-4db1-96e7-fbcaf984b709", root
          assert root[3:] == ["btrfs", "rime-root"], root

      installer.succeed("mkdir -p /mnt /esp && mount -o subvolid=5 /dev/vdb2 /mnt && mount /dev/vdb1 /esp")

      with subtest("btrfs: @nixos, @nix, @home; the system mounts them compressed"):
          # Top level only (-o): systemd-tmpfiles' `v` lines nest /srv, /tmp, /var/tmp, ... inside @nixos.
          subs = installer.succeed("btrfs subvolume list -o /mnt | awk '{print $NF}' | sort").split()
          assert subs == ["@home", "@nix", "@nixos"], subs
          # The installed /etc/fstab links into /etc/static (absolute): read the system's own, the profile is it.
          fstab = installer.succeed(f"cat {SYSTEM}/etc/fstab")
          for s in ("@nixos", "@nix", "@home"):
              assert f"subvol={s},compress-force=zstd:3" in fstab, fstab
          installer.succeed(f"test -e /mnt/@nix/store/{SYSTEM.removeprefix('/nix/store/')}")
          installer.succeed("test -L /mnt/@nixos/home && test \"$(readlink /mnt/@nixos/home)\" = /var/home || test \"$(readlink /mnt/@nixos/home)\" = var/home")

      with subtest("the installed system is the prebuilt one: /etc/NIXOS, the system profile"):
          installer.succeed("test -e /mnt/@nixos/etc/NIXOS")
          gen = installer.succeed("readlink /mnt/@nix/var/nix/profiles/system").strip()
          assert installer.succeed(f"readlink /mnt/@nix/var/nix/profiles/{gen}").strip() == SYSTEM

      with subtest("the installed release is recorded the way `rime update` records one"):
          applied = json.loads(installer.succeed("cat /mnt/@nixos/var/lib/rime/nix/applied.json"))
          print(applied)
          r = applied["releases"][0]
          digest = installer.succeed("sha256sum /tmp/stamps/nixos-release/manifest/manifest.json | cut -d' ' -f1").strip()
          assert r["release"] == "2026.11.02" and r["toplevel"] == SYSTEM and r["generation"] == 1, r
          assert r["digest"] == "sha256:" + digest, (r, digest)
          installer.succeed(
              f"test \"$(cat /mnt/@nixos/var/lib/rime/nix/releases/2026.11.02/digest)\" = sha256:{digest}",
              f"grep -F '{SYSTEM}' /mnt/@nixos/var/lib/rime/nix/releases/2026.11.02/manifest.json",
              "test \"$(stat -c %a /mnt/@nixos/var/lib/rime/nix/applied.json)\" = 644",
          )

      with subtest("journald's directory is No_COW before anything is written to it; no feature switched on"):
          attrs = installer.succeed("lsattr -d /mnt/@nixos/var/log/journal").split()[0]
          print(attrs)
          assert "C" in attrs, attrs
          installer.succeed("test -z \"$(ls -A /mnt/@nixos/var/log/journal)\"")
          installer.succeed("test ! -e /mnt/@nixos/var/lib/rime/switch/rime-feature@gaming.target.on")

      with subtest("ESP: Fedora's shim and GRUB, the static config, the boot hook's BLS entry"):
          installer.succeed(
              "cmp /esp/EFI/fedora/shimx64.efi /tmp/efi/shimx64.efi",
              "cmp /esp/EFI/BOOT/BOOTX64.EFI /tmp/efi/shimx64.efi",
              "cmp /esp/EFI/fedora/grubx64.efi /tmp/efi/grubx64.efi",
              "cmp /esp/EFI/fedora/grub.cfg /tmp/grub-static/grub-static-efi.cfg",
              "grep -x '### BEGIN 10_blscfg.cfg ###' /esp/grub2/grub.cfg",
          )
          uuid = installer.succeed("blkid -s UUID -o value /dev/vdb1").strip()
          installer.succeed(f"grep -x 'set BOOT_UUID=\"{uuid}\"' /esp/grub2/bootuuid.cfg /esp/EFI/fedora/bootuuid.cfg")
          entry = installer.succeed("cat /esp/loader/entries/*.conf")
          print(entry)
          assert f"init={SYSTEM}/init" in entry and "console=ttyS0,115200" in entry, entry

      with subtest("host.toml: hostname, timezone, locale, keymap"):
          toml = installer.succeed("cat /mnt/@nixos/var/lib/rime/host.toml")
          print(toml)
          vendor = installer.succeed("awk -F': *' '/^vendor_id/ { print $2; exit }' /proc/cpuinfo").strip()
          mc = {"GenuineIntel": "intel", "AuthenticAMD": "amd"}.get(vendor, "both")
          for line in ('hostname = "rime-check"', 'timezone = "Australia/Perth"', 'LANG = "en_AU.UTF-8"',
                       'xkb_layout = "de"', 'layout = "bls"', f'microcode = "{mc}"'):
              assert line in toml, (line, toml)

      with subtest("the account: a password, wheel, a home on @home"):
          installer.succeed("grep -E '^alex:[^!*:]' /mnt/@nixos/etc/shadow")
          installer.succeed("grep -E '^wheel:.*\\balex\\b' /mnt/@nixos/etc/group")
          installer.succeed("test -d /mnt/@home/alex")
          # A static name from the first activation on (nixos-install's, via rime-hostlayer apply from host.toml),
          # and the image's ~/.zshrc, so zsh's first-login wizard does not run.
          assert installer.succeed("cat /mnt/@nixos/etc/hostname").strip() == "rime-check"
          installer.succeed(f"cmp /mnt/@home/alex/.zshrc {SYSTEM}/etc/skel/.zshrc")

      with subtest("a firmware entry for shim"):
          print(installer.succeed("efibootmgr -v"))
          installer.succeed("efibootmgr -v | grep -i 'Rime OS' | grep -i 'shimx64.efi'")

      installer.succeed("umount /esp /mnt")

      with subtest("the installed system passes `rime trust --gate` before its first update"):
          gate("plain", SYSTEM)

      with subtest("gaming chosen: the release's plain-gaming system, its switch on, the gate passes"):
          answers("/dev/vdb", gaming="yes")
          rc, out = run()
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out and "System:    plain-gaming (Steam and Gaming Mode included)" in out, out
          installer.succeed("mkdir -p /mnt && mount -o subvolid=5 /dev/vdb2 /mnt")
          gen = installer.succeed("readlink /mnt/@nix/var/nix/profiles/system").strip()
          assert installer.succeed(f"readlink /mnt/@nix/var/nix/profiles/{gen}").strip() == GAMING
          # What boots is that system: the boot entry names its init (/etc is only set up by its first activation).
          installer.succeed("mkdir -p /esp && mount /dev/vdb1 /esp")
          entry = installer.succeed("cat /esp/loader/entries/*.conf")
          installer.succeed("umount /esp")
          assert f"init={GAMING}/init" in entry and f"init={SYSTEM}/init" not in entry, entry
          r = json.loads(installer.succeed("cat /mnt/@nixos/var/lib/rime/nix/applied.json"))["releases"][0]
          assert r["toplevel"] == GAMING, r
          sw = "/mnt/@nixos/var/lib/rime/switch/rime-feature@gaming.target.on"
          assert installer.succeed(f"cat {sw}") == "steam\n"
          installer.succeed(f"test \"$(stat -c %a {sw})\" = 644")
          installer.succeed("umount /mnt")
          gate("plain-gaming", GAMING)

      with subtest("two caches over http: Rime's paths from Rime's, the libc closure from the upstream one"):
          # The production shape (cache.rimeos.com + cache.nixos.org): the release says which cache each path comes
          # from (origin), each cache is verified with its own key, and the paths are copied per origin. Rime's
          # cache here does NOT hold the upstream paths: the install can only succeed by taking them upstream.
          installer.succeed(
              f"nix path-info -r $(nix path-info -r {SYSTEM} | grep -E -- '-glibc-[0-9][^/]*$' | sort | head -1) > /tmp/up.paths",
              "test -s /tmp/up.paths",
              "nix key generate-secret --key-name rime-test-upstream-1 > /tmp/up.sec",
              "nix key convert-secret-to-public < /tmp/up.sec > /tmp/up.pub",
              "nix copy --to 'file:///tmp/upcache?compression=zstd&secret-key=/tmp/up.sec' $(cat /tmp/up.paths)",
              "cp -al /tmp/cache /tmp/httpcache",
              "for p in $(cat /tmp/up.paths); do h=$(basename $p | cut -c1-32); rm /tmp/httpcache/$h.narinfo; done",
              f"systemd-run --unit rime-cache-http {PY} -m http.server 8080 --bind 127.0.0.1 --directory /tmp/httpcache",
              f"systemd-run --unit upstream-cache-http {PY} -m http.server 8081 --bind 127.0.0.1 --directory /tmp/upcache",
          )
          installer.wait_for_open_port(8080)
          installer.wait_for_open_port(8081)
          mint("/tmp/rel-up", "--upstream-cache file:///tmp/upcache", "--upstream-key \"$(cat /tmp/up.pub)\"",
               "--upstream-paths /tmp/up.paths")
          stamps("/tmp/s-up", "/tmp/rel-up")
          installer.succeed("cp /tmp/up.pub /tmp/s-up/nixos-upstream-cache.pub")
          HTTP = ("RIME_NIXOS_STAMPS=/tmp/s-up RIME_NIXOS_CACHE=http://rime-cache.test:8080 "
                  "RIME_NIXOS_UPSTREAM_CACHE=http://upstream-cache.test:8081")

      with subtest("refused by name, nothing written: no network route to a cache that is downloaded from"):
          installer.succeed("wipefs -a /dev/vdb2 /dev/vdb1 /dev/vdb", "udevadm settle")
          answers("/dev/vdb")
          installer.succeed("ip route del default || true")
          refused("/dev/vdb", HTTP, "not connected to any network")
          installer.succeed("ip route add default via 192.168.1.254 dev eth1")

      with subtest("refused by name, nothing written: no DNS for the cache"):
          refused("/dev/vdb", HTTP.replace("http://rime-cache.test:8080", "http://nowhere.invalid:8080"),
                  "cannot look up nowhere.invalid (no DNS)")

      with subtest("refused by name, nothing written: a release that takes paths upstream, on an ISO that names no upstream cache"):
          stamps("/tmp/s-up-none", "/tmp/rel-up")
          refused("/dev/vdb", "RIME_NIXOS_STAMPS=/tmp/s-up-none RIME_NIXOS_CACHE=http://rime-cache.test:8080",
                  "takes", "store paths from the upstream cache, and this ISO names none")

      with subtest("installs over http from both caches"):
          answers("/dev/vdb")
          # A production NixOS ISO carries the netinstall MARKER file (build-live-iso.sh writes it; the GUI's network
          # page reads it) and no pinned image digest, so rime-install starts with NETINSTALL=1 from the file itself,
          # exactly as on the ISO. The NixOS library switches it off: the bootc netinstall steps (the pinned digest,
          # skopeo, the registry) must never run on this path, or this install would die on the missing digest.
          installer.succeed("mkdir -p /usr/lib/rime-installer && touch /usr/lib/rime-installer/netinstall",
                            "test ! -e /usr/lib/rime-installer/image-digest")
          rc, out = run(HTTP)
          installer.succeed("rm -rf /usr/lib/rime-installer")
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out, out
          ilog = installer.succeed("cat /var/log/rime-install.log")
          assert "Network install: Rime OS will be downloaded" not in ilog, ilog
          summary = json.loads(installer.succeed("cat /run/rime-installer/nixos/release/summary.json"))
          print(summary)
          ups = len(installer.succeed("cat /tmp/up.paths").split())
          assert summary["upstream"] == ups and summary["rime"] > 0, (summary, ups)
          assert "nixos: TEST cache http://rime-cache.test:8080" in ilog, ilog
          installer.succeed(
              "mount -o subvol=@nix /dev/vdb2 /mnt",
              "for p in $(cat /tmp/up.paths); do test -e /mnt/store/$(basename $p); done",
              f"test -e /mnt/store/{SYSTEM.removeprefix('/nix/store/')}",
              "umount /mnt",
          )

      with subtest("LUKS with a German keymap: installed, and the unlock prompt's keymap is on the boot entry"):
          # D9 refused this until the initrd carried every console keymap (nix/modules/keymap.nix): the boot hook
          # now puts host.toml's console keymap on every entry (rd.vconsole.keymap=de).
          answers("/dev/vdb", encrypt="yes", lukspass="rimecheckluks42", keymap="de")
          rc, out = run("RIME_KBD_KEYMAPS=${pkgs.kbd}/share/keymaps RIME_LUKS_ENROLL_LOCAL=${enrolStub} "
                        "RIME_LUKS_PBKDF_MEMORY=32768 RIME_RECOVERY_DIR=/tmp/recovery")
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out, out
          installer.succeed("test \"$(blkid -s TYPE -o value /dev/vdb2)\" = crypto_LUKS")
          installer.succeed("mount /dev/vdb1 /esp")
          entry = installer.succeed("cat /esp/loader/entries/*.conf")
          print(entry)
          uuid = installer.succeed("cryptsetup luksUUID /dev/vdb2").strip()
          assert f"rd.luks.uuid={uuid}" in entry and "rd.vconsole.keymap=de" in entry, entry
          installer.succeed("umount /esp")
          key = installer.succeed("sed -n 's/^recovery_key=//p' /run/rime-installer/result").strip()
          assert len(key) == 71, key
          installer.succeed(f"printf %s {key} | cryptsetup open --test-passphrase --key-file - /dev/vdb2")

      with subtest("the recovery key: saved to another stick, never to the installed disk"):
          def save(dev, extra=""):
              return installer.execute(f"printf '%s\\n' {key} | {extra} rime-install-save-key {dev} rime-check")
          rc, out = save("/dev/vdb", "RIME_RECOVERY_ALLOW_FIXED=1")
          print(out)
          assert rc == 1 and "is the disk Rime OS is installed on" in out, out
          rc, out = save("/dev/vde")
          print(out)
          assert rc == 1 and "/dev/vde is not a removable disk" in out, out
          rc, out = save("/dev/vde", "RIME_RECOVERY_ALLOW_FIXED=1")
          print(out)
          assert rc == 1 and "no filesystem the installer can write to" in out and "it formats nothing" in out, out
          # An encrypted stick with NO label: the label column is empty, and must not hide the volume's type.
          installer.succeed("printf x | cryptsetup luksFormat --batch-mode --pbkdf pbkdf2 --pbkdf-force-iterations 1000 "
                            "--key-file - /dev/vde")
          rc, out = save("/dev/vde", "RIME_RECOVERY_ALLOW_FIXED=1")
          print(out)
          assert rc == 1 and "holds an encrypted volume" in out, out
          installer.succeed("wipefs -a /dev/vde")
          installer.succeed("mkfs.vfat -n KEYSTICK /dev/vde")
          rc, out = save("/dev/vde", "RIME_RECOVERY_ALLOW_FIXED=1")
          print(out)
          assert rc == 0 and out.strip() == "RIME-RECOVERY-SAVED: rime-recovery-key-rime-check.txt on /dev/vde", out
          installer.succeed(
              "mkdir -p /stick && mount -o ro /dev/vde /stick",
              f"grep -x '    {key}' /stick/rime-recovery-key-rime-check.txt",
              "umount /stick",
          )
          rc, out = save("/dev/vde", "RIME_RECOVERY_ALLOW_FIXED=1")
          assert rc == 0 and "rime-recovery-key-rime-check-2.txt" in out, out
          rc, out = installer.execute("printf 'not-a-key\\n' | RIME_RECOVERY_ALLOW_FIXED=1 rime-install-save-key /dev/vde")
          assert rc == 1 and "that is not a recovery key" in out, out

      with subtest("a Windows-shaped disk: Windows' ESP, MSR, NTFS; Rime's own ESP the Windows app staged; free space"):
          installer.succeed(
              "sgdisk -n 1:0:+260M -t 1:ef00 -c 1:'EFI system partition' -n 2:0:+16M -t 2:0c01 "
              "-n 3:0:+2G -t 3:0700 -c 3:'Basic data partition' -n 4:0:+2G -t 4:ef00 -c 4:RIME-EFI "
              "-n 5:0:+18G -t 5:8304 /dev/vdd",
              "udevadm settle",
              "mkfs.vfat -F 32 -n SYSTEM /dev/vdd1",
              "mmd -i /dev/vdd1 ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot",
              "echo fixture-bootmgfw > /tmp/bootmgfw.efi && mcopy -i /dev/vdd1 /tmp/bootmgfw.efi ::/EFI/Microsoft/Boot/bootmgfw.efi",
              "mkfs.ntfs -Q -q -L Windows /dev/vdd3",
              "mkfs.vfat -F 32 -n RIME-EFI /dev/vdd4",
              "mkdir -p /stage && mount /dev/vdd4 /stage",
              "mkdir -p /stage/rimeinst /stage/EFI/rimeinst /stage/EFI/BOOT",
              "cp /tmp/efi/shimx64.efi /stage/EFI/BOOT/BOOTX64.EFI && cp /tmp/efi/shimx64.efi /stage/EFI/rimeinst/shimx64.efi",
              "echo fixture-squashfs > /stage/rimeinst/squashfs.img",
              "umount /stage",
              "efibootmgr -q --create --disk /dev/vdd --part 4 --label 'Rime OS Setup' --loader '\\EFI\\rimeinst\\shimx64.efi'",
          )
          bn = installer.succeed("efibootmgr | sed -n 's/^Boot\\([0-9A-F]\\{4\\}\\)\\*\\{0,1\\} Rime OS Setup.*/\\1/p'").strip()
          assert len(bn) == 4, bn
          installer.succeed(
              "mount /dev/vdd4 /stage",
              f"printf 'bootnum={bn}\\n' > /stage/rimeinst/handoff.cfg",
              "umount /stage",
          )
          win = installer.succeed("sha256sum /dev/vdd1 /dev/vdd2 /dev/vdd3 | cut -d' ' -f1")

      with subtest("refused by name, nothing written: partition mode onto Windows' own ESP"):
          answers("/dev/vdd", mode="partition", target="/dev/vdd5", esp="/dev/vdd1")
          rc, out = run()
          print(out)
          assert rc != 0 and "/dev/vdd1 is Windows' EFI System Partition" in out and "Nothing has been erased" in out, out
          installer.succeed("t=$(blkid -p -s TYPE -o value /dev/vdd5 || true); test -z \"$t\"")  # no filesystem (a partition always shows its GPT entry)

      # The live environment the Windows app staged boots from Rime's ESP and keeps it mounted read-only there.
      installer.succeed("mkdir -p /run/initramfs/live && mount -o ro /dev/vdd4 /run/initramfs/live")

      with subtest("refused by name, nothing written: another disk already answers to RIME-ESP"):
          answers("/dev/vdd", mode="partition", target="/dev/vdd5", esp="/dev/vdd4")
          rc, out = run()
          print(out)
          assert rc != 0 and "/dev/vdb1 already carries the label \"RIME-ESP\"" in out and "Nothing has been erased" in out, out
          installer.succeed("t=$(blkid -p -s TYPE -o value /dev/vdd5 || true); test -z \"$t\"")  # no filesystem (a partition always shows its GPT entry)
          installer.succeed("wipefs -a /dev/vdb1 /dev/vdb2 /dev/vdb", "udevadm settle")

      with subtest("installs into the free partition, beside Windows, from the staged live medium"):
          answers("/dev/vdd", mode="partition", target="/dev/vdd5", esp="/dev/vdd4")
          rc, out = run()
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out and "beside Windows" in out, out
          print(installer.succeed("tail -n 30 /var/log/rime-install.log"))

      with subtest("partition mode: Windows untouched, Rime's ESP named and booting, the setup entry gone"):
          assert installer.succeed("sha256sum /dev/vdd1 /dev/vdd2 /dev/vdd3 | cut -d' ' -f1") == win
          installer.succeed(
              "test \"$(blkid -p -s LABEL -o value /dev/vdd4)\" = RIME-ESP",
              "test \"$(blkid -p -s LABEL -o value /dev/vdd5)\" = rime-root",
              "findmnt -no OPTIONS /run/initramfs/live | grep -E '(^|,)ro(,|$)'",
              "umount /run/initramfs/live && mount -o ro /dev/vdd4 /esp",
              "ls /esp/loader/entries/*.conf",
              "cmp /esp/EFI/fedora/shimx64.efi /tmp/efi/shimx64.efi",
              "grep -F 'chainloader /EFI/Microsoft/Boot/bootmgfw.efi' /esp/grub2/custom.cfg",
              "grep -F \"search --no-floppy --fs-uuid --set=root $(blkid -s UUID -o value /dev/vdd1)\" /esp/grub2/custom.cfg",
              # The staged installer stays until the installed system's first boot (it ran from it).
              "test -f /esp/rimeinst/handoff.cfg",
              "umount /esp",
              f"grep -F 'rm -rf /boot/rimeinst /boot/EFI/rimeinst' {SYSTEM}/etc/systemd/system/rime-staged-cleanup.service",
          )
          nv = installer.succeed("efibootmgr -v")
          print(nv)
          assert "Rime OS Setup" not in nv, nv
          installer.succeed("efibootmgr -v | grep -F 'Rime OS' | grep -i 'shimx64.efi' | grep -iF 'HD(4,GPT'")
    '';
  };
}
