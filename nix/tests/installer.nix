# The NixOS install path of installer/rime-install (rime-install-nixos.sh; PLAN.md D9), in a VM.
#
#   installer-nixos       the SAME engine the ISO runs (`rime-install --headless ANSWERS`), from a NixOS VM acting as
#                         the live environment, onto a blank 20 GiB virtio disk: GPT (2 GiB ESP + a Linux root),
#                         btrfs with @nixos/@nix/@home, Fedora's shim/GRUB layout on the ESP (fixture binaries: the
#                         real ones are the OVMF lab's job), a prebuilt system copied from a signed file:// cache,
#                         the boot hook's BLS entry, host.toml, the account, the NVRAM entry. It also refuses, by
#                         name and before writing a byte: a disk below the 19 GB floor, no timezone, partition mode,
#                         a cache signed by another key (also one of the SAME name), and LUKS with a non-US unlock
#                         keymap (D9).
#   installer-target-eval nixosConfigurations.rime (what the release ISO installs) evaluates, with the layout above.
#
# The real chain (Fedora shim -> GRUB -> the signed kernel, Secure Boot on, LUKS) is proven by nix/installer/lab
# (docs/nixos/installer.md), not here.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  # A small system with the installer's target layout and the boot hook: what the engine installs here.
  # Through nixpkgs' flake lib (as nixosConfigurations are), so system.nixos.revision is set (base.nix's registry).
  target = (self.inputs.nixpkgs.lib.nixosSystem {
    modules = [
      self.nixosModules.default
      ../installer/target.nix
      {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        rime.installTarget.enable = true;
        rime.boot.enable = true;
        documentation.enable = false;
        system.stateVersion = "26.11";
      }
    ];
  }).config.system.build.toplevel;

  # The engine exactly as the ISO carries it (Containerfile.installer), shebang made to work on NixOS.
  engine = pkgs.runCommand "rime-install-under-test" { } ''
    mkdir -p $out/bin
    cp ${../../installer/rime-install} $out/bin/rime-install
    cp ${../../installer/rime-install-nixos.sh} $out/bin/rime-install-nixos.sh
    chmod +x $out/bin/rime-install
    patchShebangs $out/bin
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
    grep -x '/boot /dev/disk/by-label/RIME-ESP vfat umask=0077' $out
    test "${self.nixosConfigurations.rime.config.rime.boot.layout}" = bls
    cat $out
  '';

  installer-nixos = pkgs.testers.runNixOSTest {
    name = "rime-installer-nixos";

    nodes.installer = { pkgs, ... }: {
      virtualisation = {
        useEFIBoot = true;
        memorySize = 4096;
        cores = 4;
        diskSize = 8192;
        # vdb: the target (21.5 GB, above the floor); vdc: 16 GiB = 17.2 GB, below it.
        emptyDiskImages = [ 20480 16384 ];
        additionalPaths = [ target ];
      };
      nix.settings.experimental-features = [ "nix-command" ];
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
      ];
    };

    testScript = ''
      import shlex
      SYSTEM = "${target}"
      ENV = ("RIME_NIXOS_STAMPS=/tmp/stamps RIME_NIXOS_EFI_SRC=/tmp/efi "
             "RIME_NIXOS_GRUB_STATIC=/tmp/grub-static")

      installer.wait_for_unit("multi-user.target")
      installer.succeed("rm -f /etc/localtime")  # no timezone in the live env: the answer has to name one

      with subtest("a TEST-signed cache holding the system, and the ISO's stamps"):
          installer.succeed(
              "mkdir -p /tmp/stamps /tmp/efi /tmp/grub-static/configs.d /tmp/bad",
              "nix key generate-secret --key-name rime-installer-check-TEST-1 > /tmp/cache.sec",
              "nix key convert-secret-to-public < /tmp/cache.sec > /tmp/stamps/nixos-cache.pub",
              f"nix copy --to 'file:///tmp/cache?compression=zstd&secret-key=/tmp/cache.sec' {SYSTEM}",
              f"echo {SYSTEM} > /tmp/stamps/nixos-system",
              "echo file:///tmp/cache > /tmp/stamps/nixos-cache",
              "echo unsigned > /tmp/stamps/kernel-signed",
              "for f in shimx64.efi grubx64.efi mmx64.efi BOOTX64.CSV; do echo fixture-$f > /tmp/efi/$f; done",
              "echo 'fixture: static efi' > /tmp/grub-static/grub-static-efi.cfg",
              "echo 'fixture: static pre' > /tmp/grub-static/grub-static-pre.cfg",
              "echo 'fixture: blscfg' > /tmp/grub-static/configs.d/10_blscfg.cfg",
              # A second key that signed nothing: an ISO stamped with it must refuse the cache.
              "nix key generate-secret --key-name rime-installer-check-OTHER-1 | nix key convert-secret-to-public > /tmp/bad/pub",
          )

      def answers(disk, **over):
          fp = installer.succeed(f"lsblk -bdnP -o MAJ:MIN,SIZE,WWN,SERIAL,PTUUID,PARTUUID,PARTTYPE {disk} | head -1").strip()
          a = dict(mode="disk", disk=disk, username="alex", password="checkpass1", hostname="rime-check",
                   timezone="Australia/Perth", locale="en_AU.UTF-8", keymap="de", encrypt="no",
                   confirmed="ERASE", confirm_target=disk, confirm_disk_id=fp)
          a.update(over)
          body = "".join(f"{k}={v}\n" for k, v in a.items() if v is not None)
          installer.succeed(f"printf %s {shlex.quote(body)} > /tmp/answers")

      def run(extra=""):
          return installer.execute(f"{ENV} {extra} rime-install --headless /tmp/answers 2>&1")

      def untouched(disk):
          # blkid -p exits 2 when it finds no signature at all: no partition table, no filesystem.
          installer.succeed(f"rc=0; blkid -p {disk} || rc=$?; test $rc -eq 2")

      with subtest("refused by name, nothing written: a disk below the 19 GB floor"):
          answers("/dev/vdc")
          rc, out = run()
          print(out)
          assert rc != 0, out
          assert "/dev/vdc is too small" in out and "at least 19 GB" in out and "Nothing has been erased" in out, out
          untouched("/dev/vdc")

      with subtest("refused by name, nothing written: no timezone"):
          answers("/dev/vdb", timezone=None)
          rc, out = run()
          print(out)
          assert rc != 0 and "timezone" in out.lower() and "Nothing has been erased" in out, out
          untouched("/dev/vdb")

      with subtest("refused by name, nothing written: partition mode"):
          answers("/dev/vdb", mode="partition")
          rc, out = run()
          print(out)
          assert rc != 0 and "whole disk only" in out, out
          untouched("/dev/vdb")

      with subtest("refused by name, nothing written: a cache not signed by the ISO's key"):
          answers("/dev/vdb")
          installer.succeed("mkdir -p /tmp/stamps-bad && cp /tmp/stamps/* /tmp/stamps-bad/ && cp /tmp/bad/pub /tmp/stamps-bad/nixos-cache.pub")
          rc, out = run("RIME_NIXOS_STAMPS=/tmp/stamps-bad")
          print(out)
          assert rc != 0 and "is not signed by rime-installer-check-OTHER-1" in out, out
          untouched("/dev/vdb")

      with subtest("refused by name, nothing written: a cache signed by another key OF THE SAME NAME"):
          # The pre-wipe check verifies the signatures with the stamped key, not the key's name.
          answers("/dev/vdb")
          installer.succeed(
              "mkdir -p /tmp/stamps-same && cp /tmp/stamps/* /tmp/stamps-same/",
              "nix key generate-secret --key-name rime-installer-check-TEST-1 | nix key convert-secret-to-public > /tmp/stamps-same/nixos-cache.pub",
              "! cmp -s /tmp/stamps/nixos-cache.pub /tmp/stamps-same/nixos-cache.pub",
          )
          rc, out = run("RIME_NIXOS_STAMPS=/tmp/stamps-same")
          print(out)
          assert rc != 0 and "is not signed by rime-installer-check-TEST-1" in out and "Nothing has been erased" in out, out
          untouched("/dev/vdb")

      with subtest("refused by name, nothing written: LUKS with a non-US unlock keymap (D9)"):
          # The generic initrd has the US console keymap only, so a 'de' passphrase prompt cannot be honoured yet.
          # The engine maps the XKB layout to a console keymap through the kbd tree (Fedora's /usr/lib/kbd/keymaps on
          # the ISO); without one every layout resolves to us, so the check hands it nixpkgs' kbd tree.
          answers("/dev/vdb", encrypt="yes", lukspass="rimecheckluks42", keymap="de")
          rc, out = run("RIME_KBD_KEYMAPS=${pkgs.kbd}/share/keymaps")
          print(out)
          assert rc != 0 and "encrypted install with the 'de" in out and "Nothing has been erased" in out, out
          untouched("/dev/vdb")

      with subtest("installs the prebuilt system onto the whole disk"):
          answers("/dev/vdb")
          rc, out = run("RIME_EXTRA_KARGS='console=ttyS0,115200'")
          print(out)
          assert rc == 0 and "RIME-INSTALL-OK" in out, out
          print(installer.succeed("cat /var/log/rime-install.log | tail -n 40"))

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
          for line in ('hostname = "rime-check"', 'timezone = "Australia/Perth"', 'LANG = "en_AU.UTF-8"',
                       'xkb_layout = "de"', 'layout = "bls"'):
              assert line in toml, (line, toml)

      with subtest("the account: a password, wheel, a home on @home"):
          installer.succeed("grep -E '^alex:[^!*:]' /mnt/@nixos/etc/shadow")
          installer.succeed("grep -E '^wheel:.*\\balex\\b' /mnt/@nixos/etc/group")
          installer.succeed("test -d /mnt/@home/alex")

      with subtest("a firmware entry for shim"):
          print(installer.succeed("efibootmgr -v"))
          installer.succeed("efibootmgr -v | grep -i 'Rime OS' | grep -i 'shimx64.efi'")
    '';
  };
}
