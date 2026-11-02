# The HOST-LAYER GATE on katana's real /etc (PLAN.md D2): a sanitised copy of katana's bootc state is carried into
# the rime-vm host at its first activation, and the VM must then be katana as far as the host layer goes: andre
# logs in through greetd (the snapshot's password hash is "rime"), uid/gid and subordinate ranges exact, the same
# NetworkManager connections, ssh host keys and machine-id, the extra disks as generated nofail mounts (the disks are
# absent: the boot must not stop), andre's shell rewritten to the system's zsh. And the P2 host gates
# (docs/nixos/p2-gates.md) on that state: H2 a carried Wi-Fi profile comes up on a simulated access point,
# H3 what katana has of VPN profiles, H10 katana's host-local files matched or left behind.
#
# The snapshot is NEVER in the repository or the Nix store. It reaches the VM at RUN time as a read-only 9p share:
#
#   nix build path:.#checks.x86_64-linux.hostlayer-katana.driver
#   sudo RIME_KATANA_STATE=/var/lab/rime-nix/artifacts/katana-state RIME_KATANA_RUNTIME=<dir> \
#        TMPDIR=<a private dir> ./result/bin/nixos-test-driver
#
# RIME_KATANA_RUNTIME (optional, root 0700, never committed) holds what the bridge reads from the RUNNING old system
# rather than its /etc: `cmdline` (a copy of its /proc/cmdline, the bridge's --cmdline) and `usr-etc/` (its image's
# /usr/etc without shadow, gshadow, *.key and private/, the carry's --image-etc). Without it the carry runs with
# neither, and the test says so.
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
    imports = [
      (common.node {
        from = "/mnt/carrysrc";
        carryArgs = "$([ -s /mnt/katanart/cmdline ] && echo --cmdline /mnt/katanart/cmdline) "
          + "$([ -d /mnt/katanart/usr-etc ] && echo --image-etc /mnt/katanart/usr-etc)";
      })
      common.wifiLab
    ];
    # Mounted in the initrd (neededForBoot), so the carry in the initrd's activation can read it.
    virtualisation.fileSystems."/mnt/carrysrc" = {
      device = "carrysrc";
      fsType = "9p";
      options = [ "trans=virtio" "version=9p2000.L" "ro" "msize=1048576" ];
      neededForBoot = true;
    };
    virtualisation.fileSystems."/mnt/katanart" = {
      device = "katanart";
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
    runtime = os.environ.get("RIME_KATANA_RUNTIME", "")

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
        if not runtime:
            # An empty share: the carry then runs without --cmdline and --image-etc.
            runtime = os.path.join(os.environ.get("TMPDIR", "/tmp"), "katana-runtime-absent")
            os.makedirs(runtime, exist_ok=True)
            print("RIME_KATANA_RUNTIME is not set: the carry runs without the old kernel command line and image /usr/etc")
        print("runtime share:", sorted(os.listdir(runtime)))
        os.environ["QEMU_OPTS"] = (os.environ.get("QEMU_OPTS", "") +
            f" -virtfs local,path={state},security_model=none,mount_tag=carrysrc,readonly=on" +
            f" -virtfs local,path={runtime},security_model=none,mount_tag=katanart,readonly=on").strip()

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

        rep = json.loads(machine.succeed("cat /var/lib/rime/carry/report.json"))
        checked = {c["path"]: c["result"] for c in rep["checked"]}
        left = {l["path"]: l["why"] for l in rep["left_behind"]}

        with subtest("H10: katana's host-local files are matched to the system or left behind with a reason"):
            host_local_report(checked, left)
            with_image = machine.execute("test -d /mnt/katanart/usr-etc")[0] == 0
            nodefault = [p for p, w in left.items() if w == "no image default to compare with"]
            print("image /usr/etc given:", with_image, "| left behind:", len(left),
                  "| of them with no image default to compare:", len(nodefault))
            if with_image:
                assert not nodefault, nodefault[:5]

        with subtest("the old kernel command line"):
            print("kernel command line:", checked["kernel command line"])
            if machine.execute("test -s /mnt/katanart/cmdline")[0] == 0:
                # katana has no LUKS volume, no resume and no console keymap on its command line.
                assert checked["kernel command line"].startswith("carried to host.toml [boot] kernel_params: none;")
                assert "rd.driver.blacklist=nouveau" in checked["kernel command line"]
                machine.fail(f"{HL} host get boot")

        with subtest("H2: a carried Wi-Fi profile comes up on a simulated access point"):
            conns = [l.rsplit(":", 1) for l in machine.succeed("nmcli -g UUID,TYPE connection show").splitlines()]
            wifi = [u for u, t in conns if t == "802-11-wireless"]
            print("Wi-Fi profiles carried:", len(wifi))
            assert wifi, "katana's state has no Wi-Fi profile"
            pinned = lambda u: bool(nm_get(machine, u, "802-11-wireless.mac-address")) or \
                nm_get(machine, u, "connection.interface-name") not in ("", "wlo1")
            up = False
            for u in sorted(wifi, key=lambda u: not pinned(u)):
                if wifi_up(machine, u):
                    up = True
                    break
            assert up, "no carried Wi-Fi profile came up"

        with subtest("H3: katana's VPN profiles"):
            vpns = [u for u, t in conns if t in ("vpn", "wireguard")]
            print("VPN profiles carried:", len(vpns), "| /etc/wireguard, /etc/openvpn in the snapshot:",
                  machine.succeed(f"find {SRC}/etc/wireguard {SRC}/etc/openvpn -type f 2>/dev/null | wc -l").strip())
            openvpn_plugin(machine)

        with subtest("andre logs in through greetd"):
            greeter_login(machine, "andre", "rime")

        # katana's two extra disks, as blank virtual disks given the filesystems (UUIDs) its fstab names: read here
        # from the carried host.toml at run time, never written into the repository.
        local = [m for m in json.loads(machine.succeed(f"{HL} host get mount")) if m["what"].startswith("UUID=")]
        print("katana's extra disks:", [(m["where"], m["type"], m["passno"]) for m in local])
        assert len(local) == 2, f"expected katana's two fstab disks, found {len(local)}"
        names = ["disk1", "disk2"]
        disks = DiskLab(machine, dict(zip(names, local)))

        with subtest("katana's disks attached blank: the boot does not stop; they get their filesystems"):
            disks.restart(names)
            disks.booted_whole()
            disks.make_filesystems()

        with subtest("H6: both of katana's fstab disks mount at boot"):
            disks.h6_all_mount()

        with subtest("H7: the second disk taken out of the machine; the boot completes, the first still mounts"):
            disks.h7_one_absent("disk2")

        with subtest("H8: fsck containment on katana's state: the first disk corrupt"):
            disks.h8_corrupt("disk1", "${pkgs.e2fsprogs}/bin")

        print(report)
        machine.shutdown()
        after = snapshot_digest(state)
        print("snapshot digest after:", after)
        assert before == after, "the snapshot changed during the gate"
  '';
}
