# The P2 host gates (docs/nixos/p2-gates.md H2, H3, H6, H7, H8, H10) on the host-layer fixture
# (hostgates-fixture.nix), carried into the rime-vm host at its first activation as the bridge/installer will.
# CI-runnable: nothing here reads a real machine (hostlayer-katana.nix runs the same checks on katana's state).
#
# hostgates-net    machine + peer (a LAN neighbour):
#   H2  the carried Wi-Fi profile comes up: mac80211_hwsim gives the machine a Wi-Fi card (named wlo1, as katana's)
#       and a second radio, moved into its own network namespace, runs hostapd + dnsmasq with the SSID and
#       passphrase READ FROM THE CARRIED PROFILE; NetworkManager associates, gets a DHCP lease, shows it activated.
#       A profile pinned to the old card's MAC address (as katana's 5 GHz one is) does not come up while no card
#       has that address, then comes up on a new radio whose permanent address it is (lib/hwsim-radio.py).
#       The test asks NetworkManager to connect only once the device is managed and a scan saw the SSID (by state:
#       the hosted run 41400977973 asked while wpa_supplicant restarted). It holds wpa_supplicant down for 20 s just
#       before that wait, so the wait is needed on every run. mutant "wifi-no-wait": it neither waits nor lets
#       autoconnect happen, and asks at once: "No suitable device found", every run.
#   H3  the carried WireGuard profile comes up and carries traffic to the peer; so does the carried OpenVPN
#       profile (TLS, against an OpenVPN server on the peer), with the certificate and key files it names
#       carried with it (the OpenVPN one based on eth1, the peer's link: see the subtest).
#   network shares: the carried nfs4 line is mounted at boot (remote-fs.target, after the network), the cifs line
#       with no server behind it fails without holding the boot. rpcbind is idle at boot (no port 111) and an
#       NFSv3 mount with locking starts it on demand (docs/nixos/fedora-defaults.md "rpcbind"); mutant
#       "rpcbind-off": rpcbind turned off outright (the configuration before 11-02), the NFSv3 lock FAILS.
# Mutants are passthru: nix build path:.#checks.x86_64-linux.hostgates-net.mutants.<name>
# hostgates-disks  machine with four extra disks, attached and removed between boots (QEMU_OPTS):
#   H6  the four fstab disks mount (UUIDs from the carried host.toml): ext4 passno 2 and 0, btrfs passno 2, xfs
#       passno 0; each passno 2 one's fsck unit succeeds. The system ships fsck.btrfs, and checks the xfs disk the way
#       xfs is checked (fsck.xfs reads nothing): `xfs_repair -n`, unmounted, every structure read (mutant "no-xfs":
#       the xfs support taken out of the system, FAILS there; the lab makes its filesystems with its own tools). fsck.btrfs checks nothing (it exits 0 without reading the
#       disk), so for btrfs that success only says the line does not hold the boot: the btrfs disk is checked the
#       way btrfs is, a read-only scrub (every block against its checksum) and its error counters. Mutant
#       "btrfs-corrupt": one data block of a file on it overwritten on the disk, the scrub FAILS;
#   H7  with one disk REMOVED from the machine the boot completes and the others still mount;
#   H8  fsck containment: with one disk corrupt beyond what `fsck -a` may repair, its fsck fails, it is not
#       mounted, and the boot still completes with / read-write and no emergency/rescue shell;
#   H10 katana's three host-local files: two match what the system now ships (tuning.nix) and the report says so,
#       the third is in the "left behind" report with how to port it; the carried kernel arguments.
# hostgates-wifi-eval  no VM: a Rime system outside qemu-vm.nix has NetworkManager's wpa_supplicant (the VM tests
#       must turn it back on, so H2 in a VM alone cannot show it), with the qemu-vm.nix evaluation as its mutant.
{ pkgs, self }:

let
  inherit (pkgs) lib;
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
  mkDisks = mutant: pkgs.testers.runNixOSTest {
    name = "rime-hostgates-disks" + lib.optionalString (mutant != null) "-mutant-${mutant}";
    node.pkgsReadOnly = false;
    passthru.mutants = lib.optionalAttrs (mutant == null) (lib.genAttrs [ "no-xfs" "btrfs-corrupt" ] mkDisks);

    nodes.machine = {
      imports = [ (rimeNode fixtureDisks) ];
      boot.supportedFilesystems.xfs = lib.mkIf (mutant == "no-xfs") (lib.mkForce false);
    };

    testScript = common.checks + ''
      E2 = "${pkgs.e2fsprogs}/bin"

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
          machine.succeed("grep LIBINPUT_IGNORE_DEVICE /etc/udev/rules.d/99-dualsense-touchpad.rules")
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

      with subtest("the extra disks are absent at the first boot: the boot does not stop"):
          check_mounts(machine)

      mounts = {m["where"]: m for m in json.loads(machine.succeed(f"{HL} host get mount"))}
      games = mounts["/var/home/andre/Games"]
      lab = mounts["/var/lab"]
      media = mounts["/var/media"]
      scratch = mounts["/var/scratch"]
      assert games["passno"] == 2 and lab["passno"] == 0, mounts
      assert (media["type"], media["passno"], scratch["type"], scratch["passno"]) == ("btrfs", 2, "xfs", 0), mounts
      disks = DiskLab(machine, {"games": games, "lab": lab, "media": media, "scratch": scratch})

      with subtest("the system can check what the fstab names: fsck.btrfs (xfs: after H6, by checking it)"):
          machine.succeed("test -x /run/current-system/sw/bin/fsck.btrfs")

      with subtest("blank disks (no filesystem with those UUIDs) do not hold the boot; they get their filesystems"):
          disks.restart(list(disks.mounts))
          disks.booted_whole()
          disks.make_filesystems()

      with subtest("H6: every fstab disk (ext4, btrfs, xfs) mounts at boot, the passno 2 ones' fsck units succeed"):
          disks.h6_all_mount()

      with subtest("H6: the btrfs disk is checked as btrfs is: fsck.btrfs reads nothing, a read-only scrub reads it all"):
          # The fsck unit of the passno 2 btrfs line succeeded above, but btrfs-progs' fsck.btrfs exits 0 without
          # opening the disk (btrfs verifies checksums as it reads; a boot never runs `btrfs check`). What checks a
          # btrfs disk is a scrub: data and metadata read back against their checksums, errors counted per device.
          machine.succeed("head -c 4194304 /dev/urandom > /var/media/blob", "sync -f /var/media/blob")
          if "${toString mutant}" == "btrfs-corrupt":
              # MUTANT: one 4 KiB data block of the file overwritten on the disk under btrfs (FIEMAP gives btrfs'
              # logical address, btrfs-map-logical the device offset), then the page cache dropped.
              import re
              ext = machine.succeed("${pkgs.e2fsprogs}/bin/filefrag -v -b4096 /var/media/blob")
              print(ext)
              first = re.search(r"^\s*0:\s*\d+\.\.\s*\d+:\s*(\d+)\.\.", ext, re.M)
              assert first is not None, "filefrag printed no first extent"
              logical = int(first.group(1)) * 4096
              where = machine.succeed(f"${pkgs.btrfs-progs}/bin/btrfs-map-logical -l {logical} /dev/disk/by-id/virtio-media")
              print(where)
              phys = int(where.split("physical ")[1].split()[0])
              machine.succeed(f"dd if=/dev/urandom of=/dev/disk/by-id/virtio-media bs=4096 count=1 seek={phys // 4096} "
                              "oflag=direct conv=notrunc", "sync", "echo 3 > /proc/sys/vm/drop_caches")
          print(machine.succeed("btrfs scrub start -Br /var/media 2>&1"))
          print(machine.succeed("btrfs device stats -c /var/media"))

      with subtest("H6: the xfs disk is checked as xfs is, with what the system ships: xfs_repair -n reads it all"):
          # fsck.xfs is a no-op script (xfs replays its log at mount); an xfs disk is checked unmounted by
          # xfs_repair -n, which reads every structure and changes nothing. With the system's own tools only.
          unit = disks.unit_of("scratch")
          machine.succeed(f"systemctl stop {unit}")
          assert disks.mounted_uuid("scratch") is None, "the xfs disk is still mounted"
          print(machine.succeed("PATH=/run/current-system/sw/bin xfs_repair -n /dev/disk/by-id/virtio-scratch 2>&1"))
          machine.succeed("test -x /run/current-system/sw/bin/fsck.xfs")
          machine.succeed(f"systemctl start {unit}")
          disks.wait_mounted("scratch")

      with subtest("H7: the games disk taken out of the machine; the boot completes, the other disks mount"):
          disks.h7_one_absent("games")

      with subtest("H8: fsck containment: a corrupt disk is not mounted and does not stop the boot"):
          disks.h8_corrupt("games", E2)
    '';
  };

  mkNet = mutant: pkgs.testers.runNixOSTest {
    name = "rime-hostgates-net" + lib.optionalString (mutant != null) "-mutant-${mutant}";
    node.pkgsReadOnly = false;
    passthru.mutants = lib.optionalAttrs (mutant == null) (lib.genAttrs [ "rpcbind-off" "wifi-no-wait" ] mkNet);

    nodes.machine = {
      imports = [ (rimeNode fixtureNet) common.wifiLab ];
      environment.systemPackages = [ pkgs.wireguard-tools ];
      services.rpcbind.enable = lib.mkIf (mutant == "rpcbind-off") (lib.mkForce false);
    };

    # A LAN neighbour: the far end of both VPNs, and an NFS server (v4 and v3).
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
        environment.systemPackages = [ pkgs.wireguard-tools pkgs.openvpn ];
      };

    testScript = common.checks + lib.optionalString (mutant == "wifi-no-wait") ''
      WIFI_WAIT = False
    '' + ''
      WIFI_HOLD = 20
      FIX = "${fixtureNet}"

      peer.start()
      peer.wait_for_unit("nfs-server.service")
      peer.succeed("echo from-the-peer > /export/marker")
      # The VPNs' far end, keyed with the fixture's TEST keys and certificates.
      # OpenVPN on udp6 (dual stack): the profile names the host `peer`, which the machine resolves to its IPv6
      # address first.
      peer.succeed(
          "ip link add wg0 type wireguard",
          f"wg set wg0 listen-port 51820 private-key {FIX}/wg/server.key "
          f"peer $(cat {FIX}/wg/machine.pub) allowed-ips 10.77.0.2/32",
          "ip addr add 10.77.0.1/24 dev wg0",
          "ip link set wg0 up",
          f"systemd-run --unit=rime-test-openvpn openvpn --dev tun --proto udp6 --port 1194 --topology subnet "
          f"--server 10.78.0.0 255.255.255.0 --ca {FIX}/openvpn/ca.crt --cert {FIX}/openvpn/server.crt "
          f"--key {FIX}/openvpn/server.key --dh none --verb 3",
      )

      machine.start()
      machine.wait_for_unit("multi-user.target")

      with subtest("the carry ran once, at the first activation"):
          carry_ok(machine)

      with subtest("H2: the carried Wi-Fi profile comes up on a simulated access point"):
          assert wifi_up(machine, "FixtureWifi-5G"), "the carried Wi-Fi profile could not be tried"

      with subtest("H2: a profile pinned to the old card's MAC address comes up only on a card with that address"):
          assert wifi_up(machine, "FixtureWifi-Pinned"), "the MAC-pinned profile could not be tried"

      with subtest("H3: the carried WireGuard profile comes up and carries traffic"):
          vpn_up(machine, "FixtureVPN", "10.77.0.1")
          hs = peer.succeed("wg show wg0 latest-handshakes").split()
          print("peer's latest handshake:", hs)
          assert len(hs) == 2 and hs[1] != "0", "the peer saw no handshake"
          openvpn_plugin(machine)

      with subtest("H3: the carried OpenVPN profile comes up with the files it names, carried with it"):
          for f, mode in (("ca.crt", "644"), ("fixture.crt", "644"), ("fixture.key", "600")):
              got = machine.succeed(f"stat -c %a /etc/openvpn/client/{f}").strip()
              assert got == mode, f"/etc/openvpn/client/{f}: mode {got}, not {mode}"
          peer.wait_until_succeeds("ip -br addr show tun0 | grep -F 10.78.0.1/24", timeout=60)
          # Brought up on eth1, the link the peer is on. Without a base device NetworkManager bases a VPN on the
          # primary connection (here QEMU's user network, eth0) and routes the server's address through ITS gateway,
          # which never reaches the peer (seen 11-02: the handshake left on eth1, every data packet on eth0). On a
          # real machine the server is reached through the primary connection, so that choice is right there.
          vpn_up(machine, "FixtureOpenVPN", "10.78.0.1", base="eth1")
          print(peer.succeed("journalctl --no-pager -u rime-test-openvpn | tail -n 5"))

      with subtest("network shares: nfs4 mounted after the network, cifs with no server does not hold the boot"):
          check_mounts(machine)
          machine.wait_until_succeeds("findmnt -no FSTYPE /srv/share | grep -x nfs4", timeout=120)
          assert machine.succeed("cat /srv/share/marker").strip() == "from-the-peer"
          machine.succeed("test -x /run/current-system/sw/bin/mount.nfs")
          machine.succeed("test -x /run/current-system/sw/bin/mount.cifs")
          machine.fail("findmnt /srv/smb")
          print(machine.execute("systemctl status --no-pager srv-smb.mount")[1])

      with subtest("rpcbind: idle at boot (bootc shipped it off; NFSv4 needs none), started by an NFSv3 lock"):
          machine.fail("systemctl is-active rpcbind.service")
          machine.fail("systemctl is-active rpcbind.socket")
          assert machine.succeed("ss -Hlnt 'sport = :111'").strip() == "", "port 111 is listening at boot"
          machine.succeed("mkdir -p /mnt/v3")
          machine.succeed("mount -t nfs -o vers=3 peer:/export /mnt/v3")
          assert machine.succeed("cat /mnt/v3/marker").strip() == "from-the-peer"
          # A lock over NFSv3 goes through the lock manager (NLM): rpc-statd, which needs rpcbind, here.
          machine.succeed("flock --exclusive --timeout 30 /mnt/v3/marker true")
          machine.succeed("systemctl is-active rpc-statd.service")
          machine.succeed("systemctl is-active rpcbind.service")
          machine.succeed("umount /mnt/v3")
    '';
  };
in
{
  hostgates-net = mkNet null;

  hostgates-disks = mkDisks null;

  # H2 on a real machine: the VM tests turn wpa_supplicant back on (qemu-vm.nix forces it off in every VM), so this
  # checks that a Rime system WITHOUT the VM module (the shape of a machine on its own disk) gets it from
  # NetworkManager's module, D-Bus controlled: without it NetworkManager cannot bring any Wi-Fi profile up.
  hostgates-wifi-eval =
    let
      evalWith = extra: (import (pkgs.path + "/nixos/lib/eval-config.nix") {
        system = null;
        modules = [
          self.nixosModules.default
          {
            nixpkgs.hostPlatform = "x86_64-linux";
            rime = { desktop.enable = true; services.enable = true; gaming.enable = true; security.enable = true; };
            fileSystems."/" = { device = "/dev/disk/by-label/rime"; fsType = "btrfs"; };
          }
        ] ++ extra;
      }).config;
      facts = cfg: {
        "networkmanager.enable" = cfg.networking.networkmanager.enable;
        "networkmanager.wifi.backend" = cfg.networking.networkmanager.wifi.backend;
        "wireless.enable" = cfg.networking.wireless.enable;
        "wireless.dbusControlled" = cfg.networking.wireless.dbusControlled;
        "wpa_supplicant on the system bus" =
          builtins.any (p: pkgs.lib.hasPrefix "wpa_supplicant" (p.name or "")) cfg.services.dbus.packages;
      };
      good = f: builtins.all (v: v == true || v == "wpa_supplicant") (builtins.attrValues f);
      machine = facts (evalWith [ ]);
      # The mutant the check must see: the same system inside qemu-vm.nix (as every VM test), wpa_supplicant off.
      inVm = facts (evalWith [ (pkgs.path + "/nixos/modules/virtualisation/qemu-vm.nix") ]);
    in
    pkgs.runCommand "rime-hostgates-wifi-eval" { } ''
      echo "machine: "${pkgs.lib.escapeShellArg (builtins.toJSON machine)} | tee $out
      echo "in qemu-vm.nix (mutant): "${pkgs.lib.escapeShellArg (builtins.toJSON inVm)} | tee -a $out
      ${if !(good machine) then "echo 'a Rime machine would have no working wpa_supplicant for NetworkManager' >&2; exit 1"
        else if good inVm then "echo 'the check cannot tell: wpa_supplicant is on even inside qemu-vm.nix' >&2; exit 1"
        else "true"}
    '';
}
