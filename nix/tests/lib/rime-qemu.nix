# QEMU for Rime's NixOS VM tests: nixpkgs' qemu_test with qemu-system-x86_64 replaced by qemu-wait-sockets.sh,
# which waits for every socket QEMU connects to (the virtiofsd ones) before it execs the real binary. Everything
# else (qemu-img, the firmware under share/) is the same package, linked.
{
  lib,
  symlinkJoin,
  replaceVars,
  runtimeShell,
  gawk,
  coreutils,
  qemu,
}:

let
  wrapper = replaceVars ./qemu-wait-sockets.sh {
    inherit runtimeShell;
    realQemu = "${qemu}/bin/qemu-system-x86_64";
    awk = "${gawk}/bin/awk";
    sleep = "${coreutils}/bin/sleep";
  };
in
symlinkJoin {
  name = "qemu-rime-test-${qemu.version}";
  paths = [ qemu ];
  postBuild = ''
    rm $out/bin/qemu-system-x86_64
    install -m755 ${wrapper} $out/bin/qemu-system-x86_64
  '';
  # nixos/lib/qemu-common.nix reads qemuPkg.stdenv.
  passthru = (qemu.passthru or { }) // {
    inherit (qemu) stdenv version;
    unwrapped = qemu;
    inherit wrapper;
  };
  meta = qemu.meta // { mainProgram = "qemu-system-x86_64"; };
}
