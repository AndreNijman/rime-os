# Two settings found written by hand into katana's /etc (docs/nixos/fedora-defaults.md "Host-local on katana"),
# promoted to the product because every Rime machine of the same kind has the same problem (p2-gates H10). The
# host layer's carry compares a machine's own /etc/sysctl.d and /etc/udev/rules.d files with these by their
# settings, so on a migrated katana the report says "the new system ships the same settings" instead of leaving
# them behind.
#
# The third katana file, /etc/udev/rules.d/60-bfq-nvme.rules (every NVMe disk on the bfq I/O scheduler), is NOT
# promoted: it was tuned for one machine and nothing measured it elsewhere (bfq costs CPU per I/O on a fast NVMe,
# and the kernel's own default there is none/mq-deadline). The carry names it in the "left behind" report with how
# to port it (services.udev).
{ config, lib, pkgs, ... }:

let
  desktop = config.rime.desktop.enable;
  gaming = config.rime.gaming.enable;
in
{
  config = lib.mkMerge [
    # Writeback: Fedora's (and the kernel's) ratio defaults let 10 %/20 % of RAM accumulate as dirty pages before
    # writeback starts/blocks. On a 32-64 GB gaming machine that is gigabytes: a Steam download or shader-cache
    # write on btrfs then flushes as one multi-gigabyte burst and stalls the desktop. Byte limits cap it at
    # 256 MiB / 1 GiB whatever the RAM (on a 4 GB machine they are close to the ratio defaults anyway).
    (lib.mkIf desktop {
      environment.etc."sysctl.d/90-desktop-writeback.conf".text = ''
        # Rime OS: cap dirty page cache so large Steam/btrfs writes are written back steadily instead of in
        # multi-gigabyte bursts that stall the desktop (nix/modules/tuning.nix).
        vm.dirty_background_bytes = 268435456
        vm.dirty_bytes = 1073741824
      '';
    })
    # DualSense: Steam Input reads the controller's touchpad itself. libinput also takes it as a touchpad, so a
    # thumb on it moves the desktop pointer under the game. Gaming machines only: without Steam the touchpad as a
    # pointer is the useful default.
    (lib.mkIf gaming {
      services.udev.packages = [
        (pkgs.writeTextFile {
          name = "rime-dualsense-touchpad-rules";
          destination = "/etc/udev/rules.d/99-dualsense-touchpad.rules";
          text = ''
            # Rime OS: the DualSense touchpad is Steam Input's, not a desktop pointer (nix/modules/tuning.nix).
            ACTION=="add|change", SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_VENDOR_ID}=="054c", ENV{ID_MODEL_ID}=="0ce6", ENV{ID_INPUT_TOUCHPAD}=="1", ENV{LIBINPUT_IGNORE_DEVICE}="1", ENV{ID_INPUT_MOUSE}="0", ENV{ID_INPUT_POINTER}="0"
          '';
        })
      ];
    })
  ];
}
