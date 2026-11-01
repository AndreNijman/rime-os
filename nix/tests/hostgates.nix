# The P2 host gates (docs/nixos/p2-gates.md H2, H3, H6, H7, H8, H10) on the host-layer fixture
# (hostgates-fixture.nix), carried into the rime-vm host at its first activation as the bridge/installer will.
# CI-runnable: nothing here reads a real machine (hostlayer-katana.nix runs the same checks on katana's state).
#
# hostgates-net    machine + peer (a LAN neighbour):
#   H2  the carried Wi-Fi profile comes up: mac80211_hwsim gives the machine a Wi-Fi card (named wlo1, as katana's)
#       and a second radio, moved into its own network namespace, runs hostapd + dnsmasq with the SSID and
#       passphrase READ FROM THE CARRIED PROFILE; NetworkManager associates, gets a DHCP lease, shows it activated.
#   H3  the carried WireGuard profile comes up and carries traffic to the peer; the OpenVPN plugin is installed.
#   network shares: the carried nfs4 line is mounted at boot (remote-fs.target, after the network), the cifs line
#       with no server behind it fails without holding the boot.
# hostgates-disks  machine with two extra disks, attached and removed between boots (QEMU_OPTS):
#   H6  both fstab disks mount (UUIDs from the carried host.toml), the one with passno 2 after a clean fsck;
#   H7  with one disk REMOVED from the machine the boot completes and the other still mounts;
#   H8  fsck containment: with one disk corrupt beyond what `fsck -a` may repair, its fsck fails, it is not
#       mounted, and the boot still completes with / read-write and no emergency/rescue shell;
#   H10 katana's three host-local files: two match what the system now ships (tuning.nix) and the report says so,
#       the third is in the "left behind" report with how to port it; the carried kernel arguments.
{ pkgs, self }:

let
  common = import ./hostlayer-common.nix { inherit pkgs self; };
  fixtureNet = import ./hostgates-fixture.nix { inherit pkgs; netfs = true; };
  fixtureDisks = import ./hostgates-fixture.nix { inherit pkgs; netfs = false; };

  rimeNode = fixture: common.node {
    from = "/var/lib/rime-test/src";
    # Out of the store, with the owners and modes a store path cannot hold.
    prepare = ''
      rm -rf /var/lib/rime-test/src
      cp -r ${fixture}/root /var/lib/rime-test/src
      chmod -R u+w /var/lib/rime-test/src
      while read -r mode uid gid path; do
        chown "$uid:$gid" "/var/lib/rime-test/src/$path"
        chmod "$mode" "/var/lib/rime-test/src/$path"
      done < ${fixture}/perms
    '';
    carryArgs = "--cmdline ${fixture}/cmdline";
  };
in
{
  hostgates-net = pkgs.testers.runNixOSTest {
    name = "rime-hostgates-net";
    node.pkgsReadOnly = false;

    nodes.machine = {
      imports = [ (rimeNode fixtureNet) common.wifiLab ];
      environment.systemPackages = [ pkgs.wireguard-tools ];
    };

    # A LAN neighbour: the far end of the VPN, and an NFS server.
    nodes.peer =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        services.nfs.server = {
          enable = true;
          exports = "/export 192.168.1.0/24(rw,no_subtree_check,no_root_squash)";
        };
        systemd.tmpfiles.rules = [ "d /export 0755 root root -" ];
        boot.kernelModules = [ "wireguard" ];
        environment.systemPackages = [ pkgs.wireguard-tools ];
      };

    testScript = common.checks + ''
      FIX = "${fixtureNet}"

      peer.start()
      peer.wait_for_unit("nfs-server.service")
      peer.succeed("echo from-the-peer > /export/marker")
      # The VPN's far end, keyed with the fixture's TEST keys.
      peer.succeed(
          "ip link add wg0 type wireguard",
          f"wg set wg0 listen-port 51820 private-key {FIX}/wg/server.key "
          f"peer $(cat {FIX}/wg/machine.pub) allowed-ips 10.77.0.2/32",
          "ip addr add 10.77.0.1/24 dev wg0",
          "ip link set wg0 up",
      )

      machine.start()
      machine.wait_for_unit("multi-user.target")

      with subtest("the carry ran once, at the first activation"):
          carry_ok(machine)

      with subtest("H2: the carried Wi-Fi profile comes up on a simulated access point"):
          assert wifi_up(machine, "FixtureWifi-5G"), "the carried Wi-Fi profile could not be tried"

      with subtest("H3: the carried WireGuard profile comes up and carries traffic"):
          vpn_up(machine, "FixtureVPN", "10.77.0.1")
          hs = peer.succeed("wg show wg0 latest-handshakes").split()
          print("peer's latest handshake:", hs)
          assert len(hs) == 2 and hs[1] != "0", "the peer saw no handshake"
          openvpn_plugin(machine)

      with subtest("network shares: nfs4 mounted after the network, cifs with no server does not hold the boot"):
          check_mounts(machine)
          machine.wait_until_succeeds("findmnt -no FSTYPE /srv/share | grep -x nfs4", timeout=120)
          assert machine.succeed("cat /srv/share/marker").strip() == "from-the-peer"
          machine.succeed("test -x /run/current-system/sw/bin/mount.nfs")
          machine.succeed("test -x /run/current-system/sw/bin/mount.cifs")
          machine.fail("findmnt /srv/smb")
          print(machine.execute("systemctl status --no-pager srv-smb.mount")[1])
          # bootc shipped rpcbind off; NFSv4 needs none.
          machine.fail("systemctl is-active rpcbind.service")
          machine.fail("ss -lnt | grep -w 111")
    '';
  };

  hostgates-disks = pkgs.testers.runNixOSTest {
    name = "rime-hostgates-disks";
    node.pkgsReadOnly = false;

    nodes.machine = {
      imports = [ (rimeNode fixtureDisks) ];
    };

    testScript = common.checks + ''
      import os
      import shutil
      import subprocess

      FIX = "${fixtureDisks}"
      E2 = "${pkgs.e2fsprogs}/bin"
      tmp = os.environ.get("TMPDIR", "/tmp")
      disks = {n: os.path.join(tmp, f"hostgates-{n}.raw") for n in ("games", "lab")}
      for path in disks.values():
          with open(path, "wb") as f:
              f.truncate(128 << 20)
      base_opts = os.environ.get("QEMU_OPTS", "")

      def attach(names):
          # The machine's extra disks for its next start; a name left out is a disk taken out of the machine.
          os.environ["QEMU_OPTS"] = (base_opts + " " + " ".join(
              f"-drive file={disks[n]},if=none,id={n},format=raw -device virtio-blk-pci,drive={n},serial={n}"
              for n in names)).strip()
          print("disks attached:", names)

      def restart(names):
          machine.shutdown()
          attach(names)
          machine.start()
          machine.wait_for_unit("multi-user.target")

      def unit_of(where):
          return machine.succeed(f"systemd-escape --path --suffix=mount {shlex.quote(where)}").strip()

      def fsck_unit(uuid):
          return machine.succeed(
              f"systemd-escape --template systemd-fsck@.service --path /dev/disk/by-uuid/{uuid}").strip()

      def mounted_uuid(where):
          rc, out = machine.execute(f"findmnt -no UUID {shlex.quote(where)}")
          return out.strip() if rc == 0 else None

      def booted_whole():
          machine.fail("systemctl is-active emergency.target")
          machine.fail("systemctl is-active rescue.target")
          machine.succeed("systemctl is-active local-fs.target multi-user.target")
          machine.succeed("findmnt -no OPTIONS / | tr , '\\n' | grep -x rw")
          print("system:", machine.execute("systemctl is-system-running")[1])

      attach(["games", "lab"])
      machine.start()
      machine.wait_for_unit("multi-user.target")

      with subtest("the carry ran once, at the first activation"):
          carry_ok(machine)
          report = json.loads(machine.succeed("cat /var/lib/rime/carry/report.json"))
          print(machine.succeed("cat /var/lib/rime/carry/report.txt"))

      with subtest("H10: katana's host-local files are matched to the system or left behind with a reason"):
          checked = {c["path"]: c["result"] for c in report["checked"]}
          left = {l["path"]: l["why"] for l in report["left_behind"]}
          host_local_report(checked, left)
          # What the system ships in their place, live.
          machine.succeed("grep 'LIBINPUT_IGNORE_DEVICE' /etc/udev/rules.d/99-dualsense-touchpad.rules")
          assert machine.succeed("sysctl -n vm.dirty_bytes").strip() == "1073741824"
          assert machine.succeed("sysctl -n vm.dirty_background_bytes").strip() == "268435456"

      with subtest("the old kernel command line: the LUKS volume, its options and keymap reach host.toml"):
          boot = json.loads(machine.succeed(f"{HL} host get boot"))
          print("host.toml [boot]:", boot)
          assert boot["kernel_params"] == [
              "rd.luks.uuid=luks-0b1c2d3e-0000-4000-8000-0000000000bb",
              "rd.luks.options=0b1c2d3e-0000-4000-8000-0000000000bb=discard,tpm2-device=auto",
              "rd.vconsole.keymap=uk",
              "resume=UUID=0b1c2d3e-0000-4000-8000-0000000000cc",
          ], boot
          luks = {v["device"]: v for v in json.loads(machine.succeed(f"{HL} host get luks"))}
          print("host.toml [[luks]]:", luks)
          v = luks["UUID=0b1c2d3e-0000-4000-8000-0000000000bb"]
          assert v["options"] == "discard,tpm2-device=auto", v
          karg = checked["kernel command line"]
          assert "rd.driver.blacklist=nouveau" in karg.split("not carried", 1)[1], karg

      mounts = {m["where"]: m for m in json.loads(machine.succeed(f"{HL} host get mount"))}
      games = mounts["/var/home/andre/Games"]
      lab = mounts["/var/lab"]
      assert games["passno"] == 2 and lab["passno"] == 0, mounts
      uuid = {n: m["what"].split("=", 1)[1] for n, m in (("games", games), ("lab", lab))}

      with subtest("first boot: blank disks (no filesystem with those UUIDs) do not hold the boot"):
          booted_whole()
          check_mounts(machine)
          assert mounted_uuid(games["where"]) is None and mounted_uuid(lab["where"]) is None

      with subtest("the disks get the filesystems host.toml names"):
          for n in ("games", "lab"):
              machine.succeed(f"mkfs.ext4 -q -U {uuid[n]} -L {n} /dev/disk/by-id/virtio-{n}")
          machine.succeed("udevadm settle")
          for n, m in (("games", games), ("lab", lab)):
              machine.succeed(f"systemctl start {unit_of(m['where'])}")
              machine.succeed(f"echo {n} > {shlex.quote(m['where'])}/.rime-test-disk")

      with subtest("H6: both fstab disks mount at boot"):
          restart(["games", "lab"])
          booted_whole()
          for n, m in (("games", games), ("lab", lab)):
              machine.wait_until_succeeds(f"findmnt -no UUID {shlex.quote(m['where'])} | grep -x {uuid[n]}", timeout=60)
              assert machine.succeed(f"cat {shlex.quote(m['where'])}/.rime-test-disk").strip() == n
          fsck = fsck_unit(uuid["games"])
          out = machine.succeed(f"systemctl show -p Result -p ActiveState {shlex.quote(fsck)}")
          print(fsck, out)
          assert "Result=success" in out and "ActiveState=active" in out, out

      with subtest("H7: the games disk taken out of the machine; the boot completes, the other disk mounts"):
          restart(["lab"])
          booted_whole()
          machine.wait_until_succeeds(f"findmnt -no UUID /var/lab | grep -x {uuid['lab']}", timeout=60)
          assert mounted_uuid(games["where"]) is None, "a disk that is not there is mounted?"
          machine.fail("test -e /dev/disk/by-id/virtio-games")
          print(machine.execute(f"systemctl status --no-pager {unit_of(games['where'])}")[1])

      with subtest("H8: fsck containment: a corrupt disk is not mounted and does not stop the boot"):
          machine.shutdown()
          # Clear the root directory's inode: damage `fsck -a` (preen) must NOT repair on its own.
          subprocess.run([f"{E2}/debugfs", "-w", "-R", "clri <2>", disks["games"]], check=True)
          probe = disks["games"] + ".probe"
          shutil.copyfile(disks["games"], probe)
          rc = subprocess.run([f"{E2}/e2fsck", "-p", probe], capture_output=True, text=True)
          os.unlink(probe)
          print("e2fsck -p on a copy:", rc.returncode, rc.stdout, rc.stderr)
          assert rc.returncode & 4, f"the corruption is one fsck -a repairs (rc {rc.returncode}): not a containment test"
          attach(["games", "lab"])
          machine.start()
          machine.wait_for_unit("multi-user.target")
          booted_whole()
          fsck = fsck_unit(uuid["games"])
          machine.wait_until_succeeds(f"systemctl is-failed {shlex.quote(fsck)}", timeout=120)
          print(machine.succeed(f"journalctl -b --no-pager -u {shlex.quote(fsck)}"))
          assert mounted_uuid(games["where"]) is None, "the corrupt disk was mounted"
          machine.wait_until_succeeds(f"findmnt -no UUID /var/lab | grep -x {uuid['lab']}", timeout=60)
          machine.wait_for_unit("graphical.target")
    '';
  };
}
