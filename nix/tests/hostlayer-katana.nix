# The HOST-LAYER GATE on katana's real /etc (PLAN.md D2): a sanitised copy of katana's bootc state is carried into
# the rime-vm host at its first activation, and the VM must then be katana as far as the host layer goes: andre
# logs in through greetd (the snapshot's password hash is "rime"), uid/gid and subordinate ranges exact, the same
# NetworkManager connections, ssh host keys and machine-id, the extra disks as generated nofail mounts (the disks are
# absent: the boot must not stop), andre's shell rewritten to the system's zsh.
#
# The snapshot is NEVER in the repository or the Nix store. It reaches the VM at RUN time as a read-only 9p share:
#
#   nix build path:.#checks.x86_64-linux.hostlayer-katana.driver
#   sudo RIME_KATANA_STATE=/var/lab/rime-nix/artifacts/katana-state TMPDIR=<a private dir> ./result/bin/nixos-test-driver
#
# (root: the snapshot is root 0700. TMPDIR: the driver keeps its VM state in $TMPDIR/vm-state-machine and deletes
# it at start, so a /tmp shared with other drivers' "machine" VMs breaks both runs). Without RIME_KATANA_STATE (CI,
# `nix flake check`) the test prints why it is skipped and boots nothing. The driver hashes the snapshot before and after, so a write to it fails the gate.
{ pkgs, self }:

let
  common = import ./hostlayer-common.nix { inherit pkgs self; };
in
pkgs.testers.runNixOSTest {
  name = "rime-hostlayer-katana";
  node.pkgsReadOnly = false;

  nodes.machine = {
    imports = [ (common.node { from = "/mnt/carrysrc"; }) ];
    # Mounted in the initrd (neededForBoot), so the carry in the initrd's activation can read it.
    virtualisation.fileSystems."/mnt/carrysrc" = {
      device = "carrysrc";
      fsType = "9p";
      options = [ "trans=virtio" "version=9p2000.L" "ro" "msize=1048576" ];
      neededForBoot = true;
    };
  };

  testScript = common.checks + ''
    import hashlib
    import os

    SRC = "/mnt/carrysrc"
    state = os.environ.get("RIME_KATANA_STATE", "")

    def snapshot_digest(root):
        h = hashlib.sha256()
        for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
            dirnames.sort()
            for name in sorted(dirnames + filenames):
                p = os.path.join(dirpath, name)
                st = os.lstat(p)
                h.update(f"{os.path.relpath(p, root)} {st.st_mode} {st.st_uid} {st.st_gid} {st.st_size} {st.st_mtime_ns}\n".encode())
                if os.path.isfile(p) and not os.path.islink(p):
                    with open(p, "rb") as f:
                        h.update(hashlib.sha256(f.read()).digest())
                elif os.path.islink(p):
                    h.update(os.readlink(p).encode())
        return h.hexdigest()

    if not state:
        print("SKIP rime-hostlayer-katana: RIME_KATANA_STATE is not set (katana's sanitised state is never in the "
              "store or the repository; see nix/tests/hostlayer-katana.nix for how to run the gate)")
    elif not os.path.isfile(os.path.join(state, "etc/passwd")):
        raise Exception(f"RIME_KATANA_STATE={state} has no etc/passwd")
    else:
        before = snapshot_digest(state)
        print("snapshot digest before:", before)
        os.environ["QEMU_OPTS"] = (os.environ.get("QEMU_OPTS", "") +
            f" -virtfs local,path={state},security_model=none,mount_tag=carrysrc,readonly=on").strip()

        machine.wait_for_unit("multi-user.target")

        with subtest("the carry ran from the read-only share"):
            machine.succeed(f"findmnt -no OPTIONS {SRC} | grep -w ro")
            carry_ok(machine)
            report = machine.succeed("cat /var/lib/rime/carry/report.txt")

        with subtest("accounts: andre and ghrunner exact, the shell rewritten"):
            check_accounts(machine, SRC, ["andre", "ghrunner"])
            print(machine.succeed("id andre; id ghrunner; getent passwd greetd"))
            machine.succeed("grep -x 'andre:524288:65536' /etc/subuid")
            machine.succeed("grep -x 'ghrunner:1000000:65536' /etc/subuid")
            machine.succeed("grep -x 'andre:524288:65536' /etc/subgid")
            machine.succeed("grep -x 'ghrunner:1000000:65536' /etc/subgid")
            assert machine.succeed("getent passwd andre | cut -d: -f7").strip() == SW + "/bin/zsh"

        with subtest("files: machine-id, ssh host keys, NetworkManager connections"):
            check_files(machine, SRC)

        with subtest("private keys are named in the report and never copied"):
            keys = [l.strip() for l in report.split("PRIVATE KEYS NOT COPIED", 1)[1].split("\n\n", 1)[0].splitlines()[1:]]
            print("private keys named:", keys)
            for k in keys:
                base = os.path.basename(k)
                machine.fail(f"find / -xdev -name {shlex.quote(base)} | grep .")

        with subtest("host.toml applied; katana's extra disks are nofail mounts and the boot went on"):
            print(machine.succeed("cat /etc/hostname /etc/locale.conf /etc/vconsole.conf; readlink -f /etc/localtime"))
            want = machine.succeed(f"cat {SRC}/etc/hostname").strip()
            assert machine.succeed("hostname").strip() == want, "hostname not applied"
            check_mounts(machine)

        with subtest("andre logs in through greetd"):
            greeter_login(machine, "andre", "rime")

        print(report)
        machine.shutdown()
        after = snapshot_digest(state)
        print("snapshot digest after:", after)
        assert before == after, "the snapshot changed during the gate"
  '';
}
