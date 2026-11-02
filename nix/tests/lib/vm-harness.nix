# A test-level module every Rime NixOS VM test gets (nix/tests/default.nix wraps pkgs.testers.runNixOSTest with
# it), so a fix to the test harness reaches every check without each test file opting in.
#
# 1. The virtiofsd race (docs/nixos/p1-exit.md): the nodes run QEMU through rime-qemu.nix, which waits for every
#    virtiofsd socket before it starts QEMU. mkDefault, so a test can still name its own QEMU.
{ config, lib, ... }:

{
  qemu.package = lib.mkDefault (config.hostPkgs.callPackage ./rime-qemu.nix { qemu = config.hostPkgs.qemu_test; });
}
