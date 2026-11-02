# Shared by nix/tests/hostlayer.nix (a synthetic bootc root) and nix/tests/hostlayer-katana.nix (katana's sanitised
# /etc, from a 9p share at run time). Not a test itself.
#
# node { from, prepare, carryArgs }
#                          the rime-vm host plus a stand-in for the bridge/installer: at the first activation (in the
#                          initrd, before `users`) it runs `rime-hostlayer carry --from <from> --root / <carryArgs>`
#                          exactly once (carryArgs is shell text, evaluated there: e.g. --cmdline, --image-etc), then
#                          makes the carried people's (empty) homes, which a real machine already has on disk.
# wifiLab                  test scaffolding for H2: mac80211_hwsim's tools, its first radio named wlo1 (katana's Wi-Fi
#                          name), its second radio (the access point) kept from NetworkManager, and
#                          rime-test-hwsim-radio (lib/hwsim-radio.py): one more radio with a given PERMANENT MAC, for a
#                          profile pinned to the old machine's card.
# checks                   Python for the test scripts: the HOST-LAYER GATE, given the old root as the VM sees it.
{ pkgs, self }:

let
  greeterVm = {
    qemu.options = [
      "-vga none"
      "-device virtio-gpu-pci,xres=1280,yres=800"
    ];
  };
in
{
  node =
    { from, prepare ? "", carryArgs ? "" }:
    { config, lib, pkgs, ... }:
    let
      hl = "${pkgs.rime.rime-hostlayer}/bin/rime-hostlayer";
    in
    {
      imports = [
        self.nixosModules.default
        ../hosts/vm.nix
      ];

      virtualisation = greeterVm;
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;

      # The closure names no machine; the name comes from host.toml (rime-vm's own name would hide that).
      networking.hostName = lib.mkForce "";

      system.activationScripts.rimeTestCarry = lib.stringAfter [ "specialfs" ] ''
        if [ ! -e /var/lib/rime/carry/done ]; then
          mkdir -p /var/lib/rime-test
          ${prepare}
          rc=0
          ${hl} carry --from ${from} --root / --system "$systemConfig" ${carryArgs} > /var/lib/rime-test/carry.log 2>&1 || rc=$?
          echo "$rc" > /var/lib/rime-test/carry.rc
          ${pkgs.gawk}/bin/awk -F: '$3 >= 1000 && $3 < 65534 && $6 ~ /^\/var\/home\// { print $3, $4, $6 }' /etc/passwd |
            while read -r uid gid home; do
              [ -d "$home" ] || { mkdir -p "$home"; chown "$uid:$gid" "$home"; chmod 0700 "$home"; }
            done
        fi
      '';
      # The carried subuid/subgid must be what the keeper saves.
      system.activationScripts.rimeHostlayerSubidSave.deps = [ "rimeTestCarry" ];

      environment.systemPackages = [ pkgs.openssh ];
    };

  wifiLab =
    { lib, pkgs, ... }:
    {
      # qemu-vm.nix turns wpa_supplicant off in every VM (mkVMOverride false); NetworkManager's module turns it on
      # (D-Bus controlled) on every other machine, which checks.hostgates-wifi-eval asserts. Back on here.
      networking.wireless.enable = lib.mkOverride 9 true;
      environment.systemPackages = with pkgs; [
        hostapd
        iw
        dnsmasq
        (writeScriptBin "rime-test-hwsim-radio" ("#!${python3}/bin/python3 -I\n" + builtins.readFile ./lib/hwsim-radio.py))
      ];
      environment.etc."systemd/network/10-rime-test-wlo1.link".text = ''
        [Match]
        MACAddress=02:00:00:00:00:00

        [Link]
        Name=wlo1
      '';
      networking.networkmanager.unmanaged = [ "mac:02:00:00:00:01:00" ];
      # A 5 GHz profile (band=a) needs a regulatory domain that allows channel 36.
      hardware.wirelessRegulatoryDatabase = true;
    };

  checks = ''
    import json
    import shlex

    SW = "/run/current-system/sw"
    HL = SW + "/bin/rime-hostlayer"

    def carry_ok(m):
        rc = m.succeed("cat /var/lib/rime-test/carry.rc").strip()
        print(m.succeed("cat /var/lib/rime-test/carry.log"))
        assert rc == "0", f"the carry exited {rc}"

    def src_lines(m, src, rel):
        return [l for l in m.succeed(f"cat {src}/{rel}").splitlines() if l.strip()]

    def check_accounts(m, src, people):
        old = {l.split(":")[0]: l.split(":") for l in src_lines(m, src, "etc/passwd")}
        for name in people:
            o = old[name]
            new = m.succeed(f"getent passwd {name}").strip().split(":")
            print("passwd", name, ":".join(new))
            assert new[2] == o[2] and new[3] == o[3], f"{name}: {new[2]}:{new[3]} != old {o[2]}:{o[3]}"
            assert new[5] == o[5], f"{name}: home {new[5]} != old {o[5]}"
            base = o[6].rsplit("/", 1)[1]
            want = SW + "/bin/" + base if base in ("zsh", "bash", "fish", "sh") else new[6]
            assert new[6] == want, f"{name}: shell {new[6]}, expected {want} (old {o[6]})"
            m.succeed(f"test -x {new[6]}")
            m.succeed(f"grep -qxF {shlex.quote(new[6])} /etc/shells || test {new[6]} = {SW}/bin/nologin")
        for f in ("subuid", "subgid"):
            have = set(src_lines(m, "/", "etc/" + f))
            print(f"/etc/{f}:", sorted(have))
            for line in src_lines(m, src, "etc/" + f):
                assert line in have, f"{line} is not in /etc/{f}"

    def check_files(m, src):
        mid = m.succeed("cat /etc/machine-id").strip()
        old = m.succeed(f"cat {src}/etc/machine-id").strip()
        print("machine-id", mid, "old", old)
        assert mid == old, f"machine-id {mid} != {old}"
        m.wait_for_unit("sshd.service")
        for t in ("ed25519", "ecdsa", "rsa"):
            pub = f"{src}/etc/ssh/ssh_host_{t}_key.pub"
            if m.execute(f"test -f {pub}")[0] != 0:
                continue
            want = m.succeed(f"ssh-keygen -lf {pub}").split()[1]
            here = m.succeed(f"ssh-keygen -lf /etc/ssh/ssh_host_{t}_key.pub").split()[1]
            served = m.succeed(f"ssh-keyscan -T 10 -t {t} 127.0.0.1 2>/dev/null | ssh-keygen -lf -").split()[1]
            print(f"ssh {t}: snapshot {want} /etc {here} served {served}")
            assert want == here == served, f"ssh {t} host key differs"
        m.wait_for_unit("NetworkManager.service")
        names = set(m.succeed("nmcli -g NAME connection show").splitlines())
        print("NetworkManager connections:", sorted(names))
        old = set(m.succeed(
            f"cat {src}/etc/NetworkManager/system-connections/*.nmconnection | sed -n 's/^id=//p'").splitlines())
        print("carried connection names:", sorted(old))
        assert old and old <= names, f"missing connections: {old - names}"

    def check_mounts(m):
        print(m.succeed(f"{HL} host show"))
        mounts = json.loads(m.succeed(f"{HL} host get mount"))
        assert mounts, "host.toml has no [[mount]]"
        for mnt in mounts:
            where = mnt["where"]
            unit = m.succeed(f"systemd-escape --path --suffix=mount {shlex.quote(where)}").strip()
            text = m.succeed(f"cat /run/systemd/generator/{shlex.quote(unit)}")
            print(unit, text, sep="\n")
            assert "nofail" in text, f"{unit} lost nofail"
            # A network share hangs from remote-fs.target after the network (systemd-fstab-generator's rule).
            netfs = mnt["type"] in ("nfs", "nfs4", "cifs", "smb3") or "_netdev" in mnt["options"].split(",")
            target = "remote-fs.target" if netfs else "local-fs.target"
            m.succeed(f"test -L /run/systemd/generator/{target}.wants/{shlex.quote(unit)}")
            assert f"Before={target}" not in text, f"{unit} would hold {target}"
            if netfs:
                assert "After=network-online.target" in text, f"{unit} does not wait for the network"
            print(m.execute(f"systemctl status --no-pager {shlex.quote(unit)}")[1])
        m.fail("systemctl is-active emergency.target")
        m.fail("systemctl is-active rescue.target")
        m.succeed("systemctl is-active local-fs.target")
        m.succeed("systemctl is-active multi-user.target")
        print("system:", m.execute("systemctl is-system-running")[1])

    def nm_get(m, conn, field, secret=False):
        flags = "-s -g" if secret else "-g"
        return m.succeed(f"nmcli --escape no {flags} {field} connection show {shlex.quote(conn)}").strip()

    # H2's lab: mac80211_hwsim's two radios (wlo1, the machine's card; the other, the access point, moved into the
    # network namespace "ap"), set up once; the access point is restarted for each profile's SSID.
    WIFI = {}
    # Wait for NetworkManager's state before asking it to connect (False only in the mutant "wifi-no-wait").
    WIFI_WAIT = True

    def wifi_lab(m):
        if WIFI:
            return WIFI
        m.succeed("modprobe mac80211_hwsim radios=2")
        m.wait_until_succeeds("test -e /sys/class/net/wlo1", timeout=30)
        ap = m.succeed("for i in /sys/class/net/*; do "
                       "[ \"$(cat $i/address)\" = 02:00:00:00:01:00 ] && basename $i; done; true").strip()
        assert ap, "no second radio"
        phy = m.succeed(f"cat /sys/class/net/{ap}/phy80211/name").strip()
        m.succeed("ip netns add ap", f"iw phy {phy} set netns name ap", f"ip -n ap addr add 10.66.0.1/24 dev {ap}")
        WIFI.update(ap=ap, serving=None)
        return WIFI

    def wifi_ap(m, ssid, psk, band):
        """The access point serves this SSID, passphrase and band (restarted when a profile needs another)."""
        lab = wifi_lab(m)
        if lab["serving"] == (ssid, psk, band):
            return
        ap = lab["ap"]
        m.execute("systemctl stop rime-test-hostapd rime-test-dnsmasq; "
                  "systemctl reset-failed rime-test-hostapd rime-test-dnsmasq")
        radio = ["hw_mode=g", "channel=6"]
        if band == "a":
            m.succeed("iw reg set US")
            radio = ["hw_mode=a", "channel=36", "country_code=US", "ieee80211d=1"]
        conf = "\n".join([f"interface={ap}", "driver=nl80211", "ssid2=" + ssid.encode().hex()] + radio + [
            "wpa=2", "wpa_key_mgmt=WPA-PSK", "rsn_pairwise=CCMP", f"wpa_passphrase={psk}", ""])
        import base64
        m.succeed(f"echo {base64.b64encode(conf.encode()).decode()} | base64 -d > /run/rime-test-hostapd.conf")
        ns = "-p NetworkNamespacePath=/run/netns/ap"
        m.succeed(f"systemd-run {ns} --unit=rime-test-hostapd hostapd /run/rime-test-hostapd.conf")
        m.wait_until_succeeds(f"ip netns exec ap iw dev {ap} info | grep 'type AP'", timeout=60)
        m.succeed(f"systemd-run {ns} --unit=rime-test-dnsmasq dnsmasq -k --user=root --port=0 --no-resolv "
                  f"--interface={ap} --bind-interfaces --dhcp-range=10.66.0.10,10.66.0.50,1h "
                  "--dhcp-leasefile=/run/rime-test-leases --pid-file=")
        lab["serving"] = (ssid, psk, band)

    def wifi_ready(m, dev, ssid):
        """NetworkManager connects a profile only on a device it manages with a live supplicant, and only to a
        network a scan has seen. Wait for both, by state: the hosted run 41400977973 asked 0.3 s after hostapd
        started, while wpa_supplicant was restarting, and NetworkManager answered "No suitable device found"."""
        if not WIFI_WAIT:
            return
        m.wait_until_succeeds(f"s=$(nmcli -g GENERAL.STATE device show {dev}) && [ \"''${{s%% *}}\" -ge 30 ]",
                              timeout=120)
        m.wait_until_succeeds(f"nmcli -t --escape no -f SSID device wifi list ifname {dev} --rescan yes "
                              f"| grep -Fx -- {shlex.quote(ssid)}", timeout=120)

    def wifi_pinned_card(m, conn, mac, iface):
        """The card a MAC-pinned profile names, as the old machine had it: a new radio whose PERMANENT address is
        that MAC (NetworkManager matches the permanent address; `ip link set address` would not do), named as the
        profile pins it. Before it exists the profile must not come up anywhere: the pin holds."""
        out = m.fail(f"nmcli --wait 20 connection up {shlex.quote(conn)} 2>&1")
        print(f"H2: {conn!r} before its card exists: {out.strip()}")
        print("H2: new radio", m.succeed(f"rime-test-hwsim-radio {mac}").strip(), "with permanent address", mac)
        # By the radio's permanent address: NetworkManager randomises the current one while it scans.
        # grep, not a shell loop: the driver's shell runs with pipefail, so a loop whose LAST interface does not
        # match fails even when an earlier one printed its name.
        try:
            found = m.wait_until_succeeds(f"grep -lx {mac} /sys/class/net/*/phy80211/macaddress", timeout=30)
        except Exception:
            print(m.execute("grep -H . /sys/class/ieee80211/*/macaddress; ip -br link; iw dev")[1])
            raise
        dev = found.split()[0].split("/")[4]
        if iface and iface != dev:
            m.succeed(f"ip link set {dev} down", f"ip link set {dev} name {iface}")
            dev = iface
        return dev

    def wifi_up(m, conn):
        """H2: bring the carried Wi-Fi profile up on a simulated access point. The SSID, passphrase and band
        are read from the CARRIED profile (as NetworkManager now has it), never from the test. A profile pinned
        to the old machine's card by MAC address comes up on a radio given that permanent address. False (and
        why) for one pinned only to an interface name other than wlo1 (no known machine has one)."""
        wifi_lab(m)
        mac = nm_get(m, conn, "802-11-wireless.mac-address").lower()
        iface = nm_get(m, conn, "connection.interface-name")
        if mac and mac != "02:00:00:00:00:00":
            dev = wifi_pinned_card(m, conn, mac, iface)
        elif iface and iface != "wlo1":
            print(f"H2: {conn!r} is pinned to interface {iface!r} and to no MAC address; its keyfile is accepted "
                  "(listed, type wifi).")
            assert nm_get(m, conn, "connection.type") == "802-11-wireless"
            return False
        else:
            dev = "wlo1"
        ssid = nm_get(m, conn, "802-11-wireless.ssid")
        # The passphrase as NetworkManager holds it, waited for by state: asked while wpa_supplicant restarts (it
        # does when a radio appears), `nmcli -s` has answered with an EMPTY one, and the access point then refused
        # its configuration ("invalid WPA passphrase length 0"; 11-02, 2 pinned CPUs and 6 busy loops).
        psk = m.wait_until_succeeds(f"nmcli --escape no -s -g 802-11-wireless-security.psk connection show "
                                    f"{shlex.quote(conn)} | grep .", timeout=120).strip()
        band = nm_get(m, conn, "802-11-wireless.band")
        wifi_ap(m, ssid, psk, band)
        wifi_ready(m, dev, ssid)
        if nm_get(m, conn, "connection.autoconnect") == "yes":
            for _ in range(45):
                if m.execute(f"nmcli -g GENERAL.STATE connection show {shlex.quote(conn)} | grep -x activated")[0] == 0:
                    print(f"H2: {conn!r} came up by itself (autoconnect)")
                    break
                m.sleep(2)
        if m.execute(f"nmcli -g GENERAL.STATE connection show {shlex.quote(conn)} | grep -x activated")[0] != 0:
            m.succeed(f"nmcli --wait 90 connection up {shlex.quote(conn)}")
        state = nm_get(m, conn, "GENERAL.STATE")
        on = nm_get(m, conn, "GENERAL.DEVICES")
        addr = m.succeed(f"nmcli -g IP4.ADDRESS device show {dev}").strip()
        link = m.succeed(f"iw dev {dev} link")
        print(f"H2: {conn!r} state {state} on {on}, {dev} {addr}")
        print(link)
        print(m.succeed("cat /run/rime-test-leases"))
        assert state == "activated", state
        assert on == dev, f"{conn!r} came up on {on!r}, not {dev!r}"
        assert addr.startswith("10.66.0."), addr
        assert "Connected to 02:00:00:00:01:00" in link, link
        return True

    def vpn_up(m, conn, far, base=None):
        """H3: the carried VPN profile comes up and traffic crosses it. base: the device whose connection the VPN
        is based on (NetworkManager routes the VPN server through it); default the primary connection."""
        on = f" ifname {base}" if base else ""
        print(m.succeed(f"nmcli --wait 60 connection up {shlex.quote(conn)}{on}"))
        assert nm_get(m, conn, "GENERAL.STATE") == "activated"
        try:
            m.wait_until_succeeds(f"ping -c1 -W3 {far}", timeout=60)
        except Exception:
            print(m.execute(f"ip -br addr; ip route get {far}; ip -6 route; nmcli device; "
                            "journalctl --no-pager -b -u NetworkManager | tail -n 40")[1])
            raise
        if nm_get(m, conn, "connection.type") == "wireguard":
            print(m.succeed("wg show"))

    def openvpn_plugin(m):
        names = m.succeed("ls /etc/NetworkManager/VPN/")
        print("NetworkManager VPN plugins:", names.split())
        assert "nm-openvpn-service.name" in names.split(), names

    class DiskLab:
        """H6-H8 on the machine's carried [[mount]]s of local disks: one raw file per disk, attached (or taken out
        of the machine) between boots through QEMU_OPTS, named by its serial (/dev/disk/by-id/virtio-<name>). The
        filesystems get the UUIDs host.toml names, so the generated units find them as on the old machine."""

        # How each filesystem is made with the UUID host.toml names, and its smallest disk (mkfs.xfs refuses
        # under 300 MB).
        MKFS = {"ext4": ("mkfs.ext4 -q -U {uuid} -L {label} {dev}", 128 << 20),
                "btrfs": ("mkfs.btrfs -q -f -U {uuid} -L {label} {dev}", 128 << 20),
                "xfs": ("mkfs.xfs -q -f -m uuid={uuid} -L {label} {dev}", 320 << 20)}

        def __init__(self, m, mounts):
            import os
            self.m = m
            self.mounts = mounts                  # name -> the host.toml [[mount]] (what = UUID=..., where, passno)
            self.uuid = {n: mt["what"].split("=", 1)[1] for n, mt in mounts.items()}
            for n, mt in mounts.items():
                assert mt["what"].startswith("UUID="), f"{n}: {mt['what']} is not a UUID= mount"
            tmp = os.environ.get("TMPDIR", "/tmp")
            self.path = {n: os.path.join(tmp, f"rime-disk-{n}.raw") for n in mounts}
            for n, p in self.path.items():
                kind = mounts[n]["type"]
                assert kind in self.MKFS, f"{n}: this lab makes {sorted(self.MKFS)}, not {kind}"
                with open(p, "wb") as f:
                    f.truncate(self.MKFS[kind][1])
            self.base_opts = os.environ.get("QEMU_OPTS", "")
            self.baseline_failed = None

        def attach(self, names):
            # The machine's extra disks for its next start; a name left out is a disk taken out of the machine.
            import os
            os.environ["QEMU_OPTS"] = (self.base_opts + " " + " ".join(
                f"-drive file={self.path[n]},if=none,id=d{n},format=raw -device virtio-blk-pci,drive=d{n},serial={n}"
                for n in names)).strip()
            print("disks attached:", list(names))

        def restart(self, names):
            self.m.shutdown()   # a no-op when it is not running
            self.attach(names)
            self.m.start()
            self.m.wait_for_unit("multi-user.target")

        def unit_of(self, n):
            where = self.mounts[n]["where"]
            return self.m.succeed(f"systemd-escape --path --suffix=mount {shlex.quote(where)}").strip()

        def fsck_unit(self, n):
            return self.m.succeed(f"systemd-escape --template systemd-fsck@.service "
                                  f"--path /dev/disk/by-uuid/{self.uuid[n]}").strip()

        def mounted_uuid(self, n):
            rc, out = self.m.execute(f"findmnt -no UUID {shlex.quote(self.mounts[n]['where'])}")
            return out.strip() if rc == 0 else None

        def failed_units(self):
            return set(self.m.succeed("systemctl list-units --failed --plain --no-legend | awk '{print $1}'").split())

        def booted_whole(self):
            m = self.m
            m.fail("systemctl is-active emergency.target")
            m.fail("systemctl is-active rescue.target")
            m.succeed("systemctl is-active local-fs.target multi-user.target")
            m.succeed("findmnt -no OPTIONS / | tr , '\\n' | grep -x rw")
            print("system:", m.execute("systemctl is-system-running")[1], "| failed:", sorted(self.failed_units()))

        def wait_mounted(self, n):
            where = shlex.quote(self.mounts[n]["where"])
            self.m.wait_until_succeeds(f"findmnt -no UUID {where} | grep -x {self.uuid[n]}", timeout=60)
            assert self.m.succeed(f"cat {where}/.rime-test-disk").strip() == n, f"{n}: not the disk written"

        def make_filesystems(self):
            """The blank disks get the filesystems host.toml names (by UUID), and a marker each."""
            m = self.m
            for n in self.mounts:
                assert self.mounted_uuid(n) is None, f"{n}: a blank disk is mounted?"
                mkfs = self.MKFS[self.mounts[n]["type"]][0]
                m.succeed(mkfs.format(uuid=self.uuid[n], label=n, dev=f"/dev/disk/by-id/virtio-{n}"))
            m.succeed("udevadm settle")
            for n in self.mounts:
                m.succeed(f"systemctl start {self.unit_of(n)}")
                m.succeed(f"echo {n} > {shlex.quote(self.mounts[n]['where'])}/.rime-test-disk")

        def h6_all_mount(self):
            self.restart(list(self.mounts))
            self.booted_whole()
            for n, mt in self.mounts.items():
                self.wait_mounted(n)
                kind = self.m.succeed(f"findmnt -no FSTYPE {shlex.quote(mt['where'])}").strip()
                assert kind == mt["type"], f"{n}: mounted as {kind}, host.toml says {mt['type']}"
                if mt["passno"]:
                    fsck = self.fsck_unit(n)
                    out = self.m.succeed(f"systemctl show -p Result -p ActiveState {shlex.quote(fsck)}")
                    print(fsck, out.split())
                    assert "Result=success" in out and "ActiveState=active" in out, out
            self.baseline_failed = self.failed_units()

        def h7_one_absent(self, gone):
            rest = [n for n in self.mounts if n != gone]
            self.restart(rest)
            self.booted_whole()
            for n in rest:
                self.wait_mounted(n)
            assert self.mounted_uuid(gone) is None, "a disk that is not there is mounted?"
            self.m.fail(f"test -e /dev/disk/by-id/virtio-{gone}")
            print(self.m.execute(f"systemctl status --no-pager {self.unit_of(gone)}")[1])

        def h8_corrupt(self, bad, e2):
            """fsck containment: damage on one disk that `fsck -a` must NOT repair by itself. Its fsck fails, it is
            not mounted, the failure does not spread (no emergency shell, / read-write, the other disks mounted,
            nothing else failed than at a healthy boot)."""
            import os
            import shutil
            import subprocess
            assert self.mounts[bad]["passno"], f"{bad} has passno 0: no fsck runs on it"
            self.m.shutdown()   # a no-op when it is not running
            # The root directory's inode cleared, and the superblock no longer says "clean" (as after a crash):
            # without the second step e2fsck -p trusts the clean flag and checks nothing.
            for req in ("clri <2>", "ssv state 0"):
                subprocess.run([f"{e2}/debugfs", "-w", "-R", req, self.path[bad]], check=True)
            probe = self.path[bad] + ".probe"
            shutil.copyfile(self.path[bad], probe)
            rc = subprocess.run([f"{e2}/e2fsck", "-p", probe], capture_output=True, text=True)
            os.unlink(probe)
            print("e2fsck -p on a copy:", rc.returncode, rc.stdout, rc.stderr)
            assert rc.returncode & 4, f"the damage is one fsck -a repairs (rc {rc.returncode}): not a containment test"
            self.restart(list(self.mounts))
            self.booted_whole()
            fsck = self.fsck_unit(bad)
            self.m.wait_until_succeeds(f"systemctl is-failed {shlex.quote(fsck)}", timeout=120)
            print(self.m.succeed(f"journalctl -b --no-pager -u {shlex.quote(fsck)}"))
            assert self.mounted_uuid(bad) is None, "the corrupt disk was mounted"
            for n in self.mounts:
                if n != bad:
                    self.wait_mounted(n)
            self.m.wait_for_unit("graphical.target")
            failed = self.failed_units()
            new = failed - (self.baseline_failed or set()) - {fsck, self.unit_of(bad)}
            print("failed now:", sorted(failed), "| at the healthy boot:", sorted(self.baseline_failed or ()))
            assert not new, f"the bad disk's failure spread to: {sorted(new)}"

    def host_local_report(checked, left):
        """H10: katana's host-local files. Two match what the system ships (nix/modules/tuning.nix), so the report
        says nothing needs carrying; the I/O scheduler rule is left behind with how to port it."""
        same = "the new system ships the same settings: nothing to carry"
        for p in ("/etc/udev/rules.d/99-dualsense-touchpad.rules", "/etc/sysctl.d/90-desktop-writeback.conf"):
            print(p, "->", checked.get(p), "|", left.get(p))
            assert checked.get(p) == same, f"{p}: {checked.get(p)!r} / left behind: {left.get(p)!r}"
        p = "/etc/udev/rules.d/60-bfq-nvme.rules"
        print(p, "->", left.get(p))
        assert p in left and "services.udev" in left[p], left.get(p)

    def greeter_login(m, user, password):
        # The carried last-user is prefilled, so the greeter focuses the password field (GreetSurface.qml).
        m.wait_for_unit("greetd.service")
        m.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=180)
        m.sleep(10)
        m.screenshot("greeter")
        uid = m.succeed(f"id -u {user}").strip()
        m.send_chars(password + "\n")
        m.wait_until_succeeds(
            "for s in $(loginctl list-sessions --no-legend | awk '$3==\"" + user + "\"{print $1}'); do "
            "loginctl show-session $s -p Class -p Service | tr '\\n' ' '; echo; done | "
            "grep 'Class=user' | grep 'Service=greetd'", timeout=180)
        m.wait_until_succeeds(f"ls /run/user/{uid}/hypr/*/.socket.sock", timeout=180)
        print(m.succeed("loginctl list-sessions --no-legend"))
        m.sleep(5)
        m.screenshot("desktop")
  '';
}
