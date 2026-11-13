# fwupd finds the EFI System Partition on every Rime machine (nix/modules/fedora-parity.nix, the fwupd block).
#
# The nixpkgs module pins EspLocation to /boot. A machine moved from bootc has bootc's /boot there (not an ESP), and
# `sudo rime update` on the L16 (2026-11-13) said "UEFI ESP partition not detected or configured": fwupd never looked
# for the real ESP. Here: a UEFI machine (OVMF, efivars) whose ESP is a GPT partition of type EF00, NOT mounted, and a
# /boot that is a plain directory, as on a moved machine. fwupd must find the partition (UDisks2), as Fedora's does.
#
# Checked with `fwupdtool esp-list`, the plugin's own lookup (the uefi_capsule flags cannot show it: OVMF has no
# capsule table, so the plugin is disabled either way).
#
# Mutant (mutants.esp-pinned, built by hand, must FAIL): the same machine with rime.fedoraParity.enable =
# false, i.e. the stock NixOS fwupd.conf with EspLocation=/boot.
{ pkgs, self }:

let
  mkTest = { parity ? true }: pkgs.testers.runNixOSTest {
    name = "rime-fwupd-esp" + pkgs.lib.optionalString (!parity) "-mutant-esp-pinned";

    node.pkgsReadOnly = false;

    nodes.machine = { lib, ... }: {
      imports = [ self.nixosModules.default ];
      rime.fedoraParity.enable = parity;
      services.fwupd.enable = true;
      virtualisation.useEFIBoot = true;
      virtualisation.emptyDiskImages = [
        { size = 128; driveConfig.deviceExtraOpts.serial = "esp"; }
        { size = 128; driveConfig.deviceExtraOpts.serial = "boot"; }
      ];
      virtualisation.memorySize = 2048;
      environment.systemPackages = [ pkgs.gptfdisk pkgs.dosfstools pkgs.e2fsprogs pkgs.util-linux ];
    };

    testScript = ''
      machine.wait_for_unit("multi-user.target")
      machine.succeed("test -d /sys/firmware/efi/efivars")

      with subtest("an ESP partition that is not mounted, and a /boot that is not an ESP (a moved machine)"):
          d = "/dev/disk/by-id/virtio-esp"
          machine.succeed(f"sgdisk -Z {d} -n 1:2048:0 -t 1:ef00 {d} && udevadm settle")
          machine.succeed(f"mkfs.vfat -F 32 {d}-part1 && mkdir -p /mnt/esp && mount {d}-part1 /mnt/esp"
                          " && mkdir -p /mnt/esp/EFI/BOOT /mnt/esp/EFI/fedora && umount /mnt/esp && udevadm settle")
          # bootc's own /boot, mounted (the installer's LUKS layout: an ext4 partition; the plain layout binds a
          # directory of the top level), as the rime-rootfs-boot generator mounts it on a moved machine.
          machine.succeed("mkfs.ext4 -q -L rime-boot /dev/disk/by-id/virtio-boot && mkdir -p /boot"
                          " && mount /dev/disk/by-id/virtio-boot /boot && mkdir -p /boot/loader /boot/grub2")
          print(machine.succeed("cat /etc/fwupd/fwupd.conf"))

      with subtest("fwupd finds the ESP itself (UDisks2), not the /boot that is not one"):
          machine.succeed("systemctl restart fwupd.service")
          # The flags of uefi_capsule prove nothing here: OVMF has no capsule table, so the plugin is disabled with
          # or without an ESP. esp-list is the lookup the plugin makes.
          rc, out = machine.execute("fwupdtool esp-list 2>&1 | grep -v 'Loading\\|Idle'")
          print(out)
          assert rc == 0 and "Selected volume: /org/freedesktop/UDisks2/block_devices/vdb1" in out, out
          # The warning `sudo rime update` printed on the L16 (fwupd's esp-not-found plugin flag).
          assert "ESP partition not detected" not in out, out
    '';
  };
in
mkTest { } // { mutants.esp-pinned = mkTest { parity = false; }; }
