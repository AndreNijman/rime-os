# The VM test harness itself (nix/tests/lib/): the virtiofsd race fix (docs/nixos/p1-exit.md).
#
#   vm-harness     no VM. Runs the QEMU wrapper (lib/qemu-wait-sockets.sh) against a fake QEMU and sockets that a
#                  helper creates late, binds without listening, or never creates. Then runs the same cases against
#                  a mutant of the wrapper that does not wait, which must fail.
#   vm-harness-vm  a real VM whose virtiofsd starts 5 s late (as under load, but every time). With stock nixpkgs QEMU
#                  starts first and fails ("Failed to connect to .../shared"), and the driver hangs on the orphaned
#                  virtiofsd's pipe; with the wrapper it boots and the shared directories are mounted.
#                  The stock half is `passthru.stock` (a 300 s global timeout; not a flake check, run by hand:
#                  nix build path:.#checks.x86_64-linux.vm-harness-vm.stock, which must FAIL).
{ pkgs, self }:

let
  lib = pkgs.lib;

  fakeQemu = pkgs.writeShellScriptBin "qemu-system-x86_64" ''
    # Records that it ran, and whether every client socket it was given was listening at that moment.
    out=''${FAKE_QEMU_OUT:?}
    echo ran > "$out"
    prev=
    for a in "$@"; do
      if [ "$prev" = -chardev ]; then
        case "$a" in *server=on*) ;; socket,*)
          p=''${a##*path=}; p=''${p%%,*}
          if [ -S "$p" ] && ${pkgs.gawk}/bin/awk -v p="$p" '$4 == "00010000" && $NF == p { f = 1 } END { exit !f }' /proc/net/unix; then
            echo "listening $p" >> "$out"
          else
            echo "NOT-LISTENING $p" >> "$out"
          fi ;;
        esac
      fi
      prev=$a
    done
  '';
  fakeQemuPkg = fakeQemu // {
    version = "0-fake";
    stdenv = pkgs.stdenv;
  };
  wrapped = pkgs.callPackage ./lib/rime-qemu.nix { qemu = fakeQemuPkg; };

  # A mutant: the wait loop never waits.
  mutantScript = pkgs.runCommand "qemu-wait-sockets-mutant.sh" { } ''
    substitute ${./lib/qemu-wait-sockets.sh} $out --replace-fail 'until listening "$s"; do' 'until true; do'
  '';
  mutantWrapper = pkgs.replaceVarsWith {
    src = mutantScript;
    isExecutable = true;
    replacements = {
      runtimeShell = pkgs.runtimeShell;
      realQemu = "${fakeQemu}/bin/qemu-system-x86_64";
      awk = "${pkgs.gawk}/bin/awk";
      sleep = "${pkgs.coreutils}/bin/sleep";
    };
  };

  # Makes a UNIX socket: after DELAY seconds bind it, after LISTEN_DELAY more seconds listen, then hold it.
  sockHelper = pkgs.writeText "sock.py" ''
    import os, socket, sys, time
    path, delay, listen_delay = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
    time.sleep(delay)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(path)
    time.sleep(listen_delay)
    s.listen(1)
    time.sleep(120)
  '';

  unit = pkgs.runCommand "rime-vm-harness" { nativeBuildInputs = [ pkgs.python3 pkgs.gawk ]; } ''
    set -euo pipefail
    cd "$(mktemp -d)"
    export FAKE_QEMU_OUT=$PWD/qemu.out

    suite() { # $1 = the qemu-system-x86_64 under test
      local q=$1 start t
      rm -f qemu.out s1 s2 s3

      echo "-- a virtiofsd socket that appears 2 s late: QEMU starts only once it listens"
      python3 ${sockHelper} $PWD/s1 2 0 & h=$!
      start=$SECONDS
      "$q" -name x -chardev socket,id=shared,path=$PWD/s1 -device foo 2> late.err
      t=$((SECONDS - start))
      kill $h
      grep -x "listening $PWD/s1" qemu.out || { echo "case late: QEMU ran before the socket listened"; cat qemu.out; return 1; }
      # said, naming the socket (the stress runs count these lines: each one a race QEMU would have lost)
      grep -F "rime-qemu: 1 of 1 socket(s) not listening yet (s1)" late.err || { echo "case late: not reported"; cat late.err; return 1; }
      [ "$t" -ge 1 ] || { echo "case late: did not wait ($t s)"; return 1; }

      echo "-- bound but not listening for 2 s (the moment between bind and listen)"
      rm -f qemu.out s2
      python3 ${sockHelper} $PWD/s2 0 2 & h=$!
      "$q" -chardev socket,id=xchg,path=$PWD/s2
      kill $h
      grep -x "listening $PWD/s2" qemu.out || { echo "case bound: QEMU ran before listen()"; cat qemu.out; return 1; }

      echo "-- a server chardev (QEMU listens) is not waited for"
      rm -f qemu.out
      start=$SECONDS
      "$q" -chardev socket,id=mon,path=$PWD/nothing,server=on,wait=off
      [ $((SECONDS - start)) -le 1 ] || { echo "case server: waited for a socket QEMU itself creates"; return 1; }
      grep -x ran qemu.out

      echo "-- never appears: refuses after RIME_VM_SOCKET_WAIT, names it, and stops the run script's children"
      rm -f qemu.out
      set +e
      # the run script's shape: a background child, then exec QEMU in the same process
      RIME_VM_SOCKET_WAIT=2 bash -c 'sleep 600 & echo $! > child.pid; exec "$0" -chardev socket,id=shared,path='"$PWD"'/s3' "$q" 2> err.txt
      rc=$?
      set -e
      cat err.txt
      [ "$rc" = 1 ] || { echo "case never: rc $rc"; return 1; }
      [ ! -e qemu.out ] || { echo "case never: QEMU was started"; return 1; }
      grep -F "$PWD/s3 is not listening after 2 s" err.txt
      sleep 0.5
      # gone, or a zombie nobody has reaped yet (its parent, the wrapper, exited)
      st=$(awk '{ sub(/^.*\) /, ""); print $1 }' /proc/"$(cat child.pid)"/stat 2>/dev/null || echo gone)
      case "$st" in gone|Z) ;; *) echo "case never: the background child is still running ($st)"; return 1 ;; esac
    }

    echo "== the wrapper"
    suite ${wrapped}/bin/qemu-system-x86_64
    # The rest of the package is the same QEMU, linked.
    test -L ${wrapped}/bin/qemu-img || test ! -e ${fakeQemu}/bin/qemu-img

    echo "== the mutant that does not wait: the suite must fail"
    test -x ${mutantWrapper}
    if (suite ${mutantWrapper}) > mutant.log 2>&1; then cat mutant.log; echo "FAIL: the mutant passed"; exit 1; fi
    cat mutant.log
    # killed for the reason under test, not for a broken mutant
    grep -F "case late: QEMU ran before the socket listened" mutant.log
    echo "killed: the wrapper that does not wait"
    touch $out
  '';

  slowVirtiofsd = pkgs.writeShellScriptBin "virtiofsd" ''
    # As under load: virtiofsd is slow to come up, every time.
    sleep 5
    exec ${lib.getExe pkgs.virtiofsd} "$@"
  '';

  vmTest =
    extra:
    pkgs.testers.runNixOSTest {
      imports = [ extra ];
      name = "rime-vm-harness";
      nodes.machine =
        { pkgs, ... }:
        {
          virtualisation.host.pkgs = lib.mkForce (pkgs // { virtiofsd = slowVirtiofsd; });
          virtualisation.memorySize = 1024;
        };
      testScript = ''
        machine.wait_for_unit("multi-user.target")
        # the nix store and the driver's shared directory both come from virtiofsd
        machine.succeed("findmnt -t virtiofs /nix/.ro-store")
        machine.succeed("findmnt -t virtiofs /tmp/shared")
      '';
    };
in
{
  vm-harness = unit;
  vm-harness-vm = vmTest {
    passthru.stock = vmTest {
      qemu.package = lib.mkForce pkgs.qemu_test;
      globalTimeout = 300;
    };
  };
}
