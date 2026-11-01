# Shared by nix/tests/hostlayer.nix (a synthetic bootc root) and nix/tests/hostlayer-katana.nix (katana's sanitised
# /etc, from a 9p share at run time). Not a test itself.
#
# node { from, prepare, carryArgs }
#                          the rime-vm host plus a stand-in for the bridge/installer: at the first activation (in the
#                          initrd, before `users`) it runs `rime-hostlayer carry --from <from> --root / <carryArgs>`
#                          exactly once (carryArgs is shell text, evaluated there: e.g. --cmdline, --image-etc), then
#                          makes the carried people's (empty) homes, which a real machine already has on disk.
# wifiLab                  test scaffolding for H2: mac80211_hwsim's tools, its first radio named wlo1 (katana's Wi-Fi
#                          name), its second radio (the access point) kept from NetworkManager.
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
    { pkgs, ... }:
    {
      environment.systemPackages = with pkgs; [ hostapd iw dnsmasq ];
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

    def wifi_up(m, conn):
        """H2: bring the carried Wi-Fi profile up on a simulated access point. The SSID, passphrase and band
        are read from the CARRIED profile (as NetworkManager now has it), never from the test. False (and why)
        when the profile is pinned to hardware a simulated radio cannot be."""
        m.succeed("modprobe mac80211_hwsim radios=2")
        m.wait_until_succeeds("test -e /sys/class/net/wlo1", timeout=30)
        mac = nm_get(m, conn, "802-11-wireless.mac-address")
        iface = nm_get(m, conn, "connection.interface-name")
        if mac and mac.lower() != "02:00:00:00:00:00" or iface and iface != "wlo1":
            print(f"H2: {conn!r} is pinned to hardware (mac-address set: {bool(mac)}, interface-name {iface!r}); "
                  "a simulated radio cannot be that card. Its keyfile is accepted (listed, type wifi).")
            assert nm_get(m, conn, "connection.type") == "802-11-wireless"
            return False
        ap = m.succeed("for i in /sys/class/net/*; do "
                       "[ \"$(cat $i/address)\" = 02:00:00:00:01:00 ] && basename $i; done; true").strip()
        assert ap, "no second radio"
        phy = m.succeed(f"cat /sys/class/net/{ap}/phy80211/name").strip()
        m.succeed("ip netns add ap", f"iw phy {phy} set netns name ap")
        ssid = nm_get(m, conn, "802-11-wireless.ssid")
        psk = nm_get(m, conn, "802-11-wireless-security.psk", secret=True)
        band = nm_get(m, conn, "802-11-wireless.band")
        radio = ["hw_mode=g", "channel=6"]
        if band == "a":
            m.succeed("iw reg set US")
            radio = ["hw_mode=a", "channel=36", "country_code=US", "ieee80211d=1"]
        conf = "\n".join([f"interface={ap}", "driver=nl80211", "ssid2=" + ssid.encode().hex()] + radio + [
            "wpa=2", "wpa_key_mgmt=WPA-PSK", "rsn_pairwise=CCMP", f"wpa_passphrase={psk}", ""])
        import base64
        m.succeed(f"echo {base64.b64encode(conf.encode()).decode()} | base64 -d > /run/rime-test-hostapd.conf")
        ns = "-p NetworkNamespacePath=/run/netns/ap"
        m.succeed(f"ip -n ap addr add 10.66.0.1/24 dev {ap}",
                  f"systemd-run {ns} --unit=rime-test-hostapd hostapd /run/rime-test-hostapd.conf")
        m.wait_until_succeeds(f"ip netns exec ap iw dev {ap} info | grep 'type AP'", timeout=60)
        m.succeed(f"systemd-run {ns} --unit=rime-test-dnsmasq dnsmasq -k --user=root --port=0 --no-resolv "
                  f"--interface={ap} --bind-interfaces --dhcp-range=10.66.0.10,10.66.0.50,1h "
                  "--dhcp-leasefile=/run/rime-test-leases --pid-file=")
        if nm_get(m, conn, "connection.autoconnect") == "yes":
            for _ in range(45):
                if m.execute(f"nmcli -g GENERAL.STATE connection show {shlex.quote(conn)} | grep -x activated")[0] == 0:
                    print(f"H2: {conn!r} came up by itself (autoconnect)")
                    break
                m.sleep(2)
        if m.execute(f"nmcli -g GENERAL.STATE connection show {shlex.quote(conn)} | grep -x activated")[0] != 0:
            m.succeed(f"nmcli --wait 90 connection up {shlex.quote(conn)}")
        state = nm_get(m, conn, "GENERAL.STATE")
        addr = m.succeed("nmcli -g IP4.ADDRESS device show wlo1").strip()
        link = m.succeed("iw dev wlo1 link")
        print(f"H2: state {state}, wlo1 {addr}")
        print(link)
        print(m.succeed("cat /run/rime-test-leases"))
        assert state == "activated", state
        assert addr.startswith("10.66.0."), addr
        assert "Connected to 02:00:00:00:01:00" in link, link
        return True

    def vpn_up(m, conn, far):
        """H3: the carried VPN profile comes up and traffic crosses it."""
        print(m.succeed(f"nmcli --wait 60 connection up {shlex.quote(conn)}"))
        assert nm_get(m, conn, "GENERAL.STATE") == "activated"
        m.wait_until_succeeds(f"ping -c1 -W3 {far}", timeout=60)
        print(m.succeed("wg show"))

    def openvpn_plugin(m):
        names = m.succeed("ls /etc/NetworkManager/VPN/")
        print("NetworkManager VPN plugins:", names.split())
        assert "nm-openvpn-service.name" in names.split(), names

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
