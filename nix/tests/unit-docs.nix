# unit-docs: every Rime unit's Documentation= leads somewhere on the NixOS system (lib/unit-docs.sh).
#
# The units come from the bootc image (files/system/units) and from the modules, and their links were written for
# bootc: man:rime(1) (a page no image ever shipped, now rimed's postInstall makes it from `rime --help`) and
# file:///usr/share/doc/rime/*.md (bootc's docs directory, which no NixOS machine has). The check reads the rime-vm
# toplevel, then runs the same script on three broken copies of it, each of which must FAIL for its own reason:
#   no-man-page    the toplevel's units with an empty man directory (rime(1) gone again)
#   bootc-doc      plus one unit carrying a bootc file:///usr/share/doc/rime link (rime-boot-health.service)
#   no-units       a toplevel with no Rime units at all (the script must not pass on finding nothing)
# The rime-vm toplevel runs the stock nixpkgs kernel (linux-6.18.x, cache.nixos.org), not linux-rime: checked
# 2026-11-02 with nix-store -qR on this check's derivation, so it runs in CI (size.nix reads the same toplevel).
{ pkgs, self }:

let
  top = self.nixosConfigurations.rime-vm.config.system.build.toplevel;
  script = ./lib/unit-docs.sh;
in
pkgs.runCommand "rime-unit-docs" { } ''
  set -euo pipefail
  echo "== rime-vm"
  bash ${script} ${top}
  test -f ${top}/sw/share/man/man1/rime.1.gz || test -f ${top}/sw/share/man/man1/rime.1

  mutant() { # $1 = name, $2 = the reason its output must give; the tree is ./m
    if bash ${script} m > $1.log 2>&1; then cat $1.log; echo "FAIL: mutant $1 passed"; exit 1; fi
    cat $1.log
    grep -F -- "$2" $1.log >/dev/null || { echo "FAIL: mutant $1 failed, but not because: $2"; exit 1; }
    echo "killed: $1"
  }

  cd "$(mktemp -d)"
  mkdir -p m/etc/systemd m/sw/share/man/man1
  ln -s ${top}/etc/systemd/system m/etc/systemd/system
  ln -s ${top}/etc/systemd/user m/etc/systemd/user
  mutant no-man-page "user/rime-agentd.service: man:rime(1) leads nowhere"

  rm -r m/sw && ln -s ${top}/sw m/sw
  rm m/etc/systemd/system && mkdir m/etc/systemd/system
  for f in ${top}/etc/systemd/system/*; do ln -s "$f" m/etc/systemd/system/; done
  printf '[Unit]\nDocumentation=file:///usr/share/doc/rime/boot-v2.md\n[Service]\nExecStart=/bin/true\n' \
    > m/etc/systemd/system/rime-boot-health.service
  mutant bootc-doc "system/rime-boot-health.service: file:///usr/share/doc/rime/boot-v2.md leads nowhere"

  rm -r m/etc/systemd && mkdir -p m/etc/systemd/system m/etc/systemd/user
  mutant no-units "found only 0 Rime units"
  touch $out
''
