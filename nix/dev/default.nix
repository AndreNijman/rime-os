# The persistent development VM (docs/nixos/dev-vm.md).
#
#   vms.<desktop>   legacyPackages.x86_64-linux.dev-vm.<desktop>: system.build.vm of rime-vm + ./vm.nix. Not a
#                   flake package or check: nix/ci/eval-each.sh and CI evaluate those, and four more full systems
#                   would only cost them time. dev-vm builds the one it boots.
#   app             apps.x86_64-linux.dev-vm (nix run path:.#dev-vm): ./dev-vm.sh. It builds the VM from the
#                   checkout it is run in at every `rebuild`, never from the app's own copy of the flake.
#   check           checks.x86_64-linux.dev-vm (no VM): shellcheck (writeShellApplication's, and the host wrapper's),
#                   flake8 on the QMP client, the hyprland VM evaluates, and the script's refusals, against a mutant.
{ pkgs, self }:

let
  lib = pkgs.lib;
  desktops = import ../desktops;

  vmOf =
    desktop: session:
    (self.nixosConfigurations.rime-vm.extendModules {
      modules = [ (import ./vm.nix { inherit desktop session; }) ];
    }).config.system.build.vm;

  vms =
    { hyprland = vmOf null "hyprland"; }
    // lib.mapAttrs (id: d: vmOf id (builtins.head d.sessionIds)) desktops;

  mkApp =
    script: extra:
    pkgs.writeShellApplication ({
      name = "dev-vm";
      runtimeInputs = with pkgs; [ coreutils util-linux openssh python3 socat gnugrep gnused ];
      text = ''
        export DEVVM_QMP=${./qmp.py} DEVVM_LAUNCH=${./launch.py}
      '' + script;
    } // extra);

  app = mkApp (builtins.readFile ./dev-vm.sh) { };

  # Mutant: owns_pid trusts any PID in qemu.pid (the check that keeps `stop` from signalling a process that is not
  # this VM's QEMU). The suite must fail on it, at the case that kills an unrelated process.
  mutant = mkApp (
    builtins.replaceStrings
      [ "owns_pid() {\n  local args\n" ]
      [ "owns_pid() {\n  return 0\n  local args\n" ]
      (builtins.readFile ./dev-vm.sh)
  ) { checkPhase = "true"; }; # its early return is unreachable code to shellcheck

  suite = pkgs.writeShellScript "dev-vm-suite" ''
    set -u
    dv=$1/bin/dev-vm
    fail() { echo "FAIL: $*"; exit 1; }
    export RIME_DEVVM_ROOT=$PWD/root RIME_DEVVM_STOP_WAIT=1
    mkdir -p root

    echo "-- help"
    "$dv" help | grep -F "dev-vm rebuild" > /dev/null || fail "help does not list rebuild"

    echo "-- unknown command and bad options are usage errors (exit 2)"
    "$dv" frobnicate > /dev/null 2>&1; [ $? = 2 ] || fail "unknown command"
    "$dv" --desktop xfce build > /dev/null 2>&1; [ $? = 2 ] || fail "--desktop xfce accepted"
    "$dv" --display gtk start > /dev/null 2>&1; [ $? = 2 ] || fail "--display gtk accepted"

    echo "-- a state name cannot leave the state root"
    for n in ../x a/b .hidden "" "x y" UPPER; do
      "$dv" --name "$n" status > /dev/null 2>&1; [ $? = 2 ] || fail "--name '$n' accepted"
    done
    [ ! -e x ] || fail "--name ../x made a directory"

    echo "-- reset needs --yes"
    mkdir -p root/a && echo disk > root/a/disk.qcow2
    "$dv" --name a reset > /dev/null 2>&1 && fail "reset without --yes succeeded"
    [ -f root/a/disk.qcow2 ] || fail "reset without --yes deleted the disk"

    echo "-- a stale qemu.pid naming an unrelated live process: not running, never signalled"
    sleep 300 & other=$!
    echo "$other" > root/a/qemu.pid
    "$dv" --name a stop > /dev/null 2>&1
    sleep 0.5
    kill -0 "$other" 2> /dev/null || fail "stop killed an unrelated process (pid $other)"
    echo "$other" > root/a/qemu.pid
    "$dv" --name a status | grep -F "a: stopped" > /dev/null || fail "an unrelated PID counted as running"

    echo "-- nor a QEMU-looking process that belongs to another dev VM"
    ${pkgs.python3}/bin/python3 -c 'import time; time.sleep(300)' qemu-system-x86_64 -qmp "unix:$PWD/root/b/qmp.sock,server=on,wait=off" & otherq=$!
    echo "$otherq" > root/a/qemu.pid
    "$dv" --name a status | grep -F "a: stopped" > /dev/null || fail "another VM's QEMU counted as this one"
    "$dv" --name a stop > /dev/null 2>&1
    sleep 0.5
    kill -0 "$otherq" 2> /dev/null || fail "stop killed another VM's QEMU (pid $otherq)"

    echo "-- this VM's QEMU (by its QMP socket) is running, and reset refuses then"
    ${pkgs.python3}/bin/python3 -c 'import time; time.sleep(300)' qemu-system-x86_64 -qmp "unix:$PWD/root/a/qmp.sock,server=on,wait=off" & mine=$!
    echo "$mine" > root/a/qemu.pid
    "$dv" --name a status | grep -F "a: running (pid $mine)" > /dev/null || fail "its own QEMU not seen as running"
    "$dv" --name a reset --yes > /dev/null 2>&1 && fail "reset --yes deleted the disk of a running VM"
    [ -f root/a/disk.qcow2 ] || fail "reset of a running VM deleted the disk"
    kill "$mine"; wait "$mine" 2> /dev/null

    echo "-- reset --yes on a stopped VM deletes only the disk"
    echo key > root/a/id_ed25519
    "$dv" --name a reset --yes > /dev/null || fail "reset --yes failed"
    [ ! -e root/a/disk.qcow2 ] || fail "disk still there"
    [ -f root/a/id_ed25519 ] || fail "reset deleted the ssh key"

    echo "-- list names every dev VM (a long status must not end the loop early)"
    mkdir -p root/b root/c
    [ "$("$dv" list | grep -c ': stopped$')" = 3 ] || fail "list did not name a, b and c"

    kill "$other" "$otherq" 2> /dev/null
    echo "suite passed"
  '';
  hyprlandDrv = builtins.unsafeDiscardStringContext vms.hyprland.drvPath;

  check =
    pkgs.runCommand "rime-dev-vm-check"
      {
        nativeBuildInputs = [ pkgs.shellcheck pkgs.python3Packages.flake8 pkgs.procps ];
      }
      ''
        set -euo pipefail
        echo "== the app builds (writeShellApplication shellchecks dev-vm.sh): ${app}"
        echo "== the host wrapper"
        shellcheck ${./dev-vm}
        echo "== the QMP client"
        flake8 --max-line-length 120 ${./qmp.py} ${./launch.py}
        echo "== the hyprland dev VM evaluates: ${hyprlandDrv}"
        cd "$(mktemp -d)"
        echo "== the suite"
        bash ${suite} ${app}
        echo "== the mutant (owns_pid trusts any PID): the suite must fail"
        rm -rf root x
        if bash ${suite} ${mutant} > mutant.log 2>&1; then cat mutant.log; echo "FAIL: the mutant passed"; exit 1; fi
        cat mutant.log
        grep -F "FAIL: stop killed an unrelated process" mutant.log
        echo "killed: owns_pid trusts any PID"
        echo ${hyprlandDrv} > $out
      '';
in
{
  inherit vms app check;
}
