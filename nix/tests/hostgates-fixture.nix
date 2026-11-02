# The host-layer fixture (hostlayer-fixture.nix) with what the P2 host gates (docs/nixos/p2-gates.md H2-H10) carry
# on top. TEST material only, made inside this derivation:
#
#   $out/root      the fixture root plus: a WireGuard VPN profile (its private key derived here from a fixed test
#                  seed), network shares in the fstab (with `netfs`), and katana's three host-local files
#                  (docs/nixos/fedora-defaults.md "Host-local on katana"), as settings only, with comments of their own
#   $out/perms     the fixture's owners and modes, plus the VPN profile's
#   $out/cmdline   an old bootc kernel command line: a LUKS volume, its options and console keymap, resume, and
#                  arguments the carry must leave (the old boot's own, a module blacklist)
#   $out/wg        the WireGuard TEST keys: server.key and machine.pub for the peer node, server.pub
#   $out/openvpn   the OpenVPN server's TEST CA, certificate and key for the peer node (made here, random keys;
#                  the client's certificate and key are in the root, under /etc/openvpn/client, where the carried
#                  OpenVPN profile names them)
{ pkgs, netfs ? true }:

let
  base = import ./hostlayer-fixture.nix { inherit pkgs; };
in
pkgs.runCommand "rime-hostgates-fixture"
  {
    nativeBuildInputs = [ pkgs.wireguard-tools pkgs.openssl ];
  }
  ''
    mkdir -p $out
    cp -r ${base}/root $out/root
    chmod -R u+w $out/root
    r=$out/root

    # ── WireGuard TEST keys: fixed seeds, so the derivation is reproducible ──
    mkdir -p $out/wg
    key() { printf '%s' "$1" | openssl dgst -sha256 -binary | base64; }
    key rime-hostgates-TEST-server > $out/wg/server.key
    key rime-hostgates-TEST-machine > machine.key
    wg pubkey < $out/wg/server.key > $out/wg/server.pub
    wg pubkey < machine.key > $out/wg/machine.pub

    # ── H3: a saved VPN profile (NetworkManager keyfile, as nmcli writes one) ──
    cat > $r/etc/NetworkManager/system-connections/FixtureVPN.nmconnection <<EOF
    [connection]
    id=FixtureVPN
    uuid=6c1e1f0a-0000-4000-8000-000000000003
    type=wireguard
    autoconnect=false
    interface-name=wg-fixture

    [wireguard]
    private-key=$(cat machine.key)

    [wireguard-peer.$(cat $out/wg/server.pub)]
    endpoint=peer:51820
    allowed-ips=10.77.0.0/24;

    [ipv4]
    address1=10.77.0.2/24
    method=manual

    [ipv6]
    method=disabled
    EOF

    # ── H2: a Wi-Fi profile pinned to the old machine's card by its MAC address (made up, locally administered),
    # as katana's 5 GHz one is: it may come up only on a card with that permanent address. ──
    cat > $r/etc/NetworkManager/system-connections/FixtureWifi-Pinned.nmconnection <<EOF
    [connection]
    id=FixtureWifi-Pinned
    uuid=6c1e1f0a-0000-4000-8000-000000000006
    type=wifi
    autoconnect=false

    [wifi]
    mac-address=02:52:49:4D:45:01
    mode=infrastructure
    ssid=FixtureWifi-Pinned

    [wifi-security]
    key-mgmt=wpa-psk
    psk=TEST-not-a-real-passphrase-either

    [ipv4]
    method=auto
    EOF

    # ── H3: an OpenVPN profile whose certificates live in files it names (NetworkManager keyfile, TLS) ──
    # A TEST CA issues the peer's server certificate and this machine's client certificate (EC P-256, no
    # passphrase; cert-pass-flags=4 says so, as nmcli writes it). The carry must take the files with the profile.
    mkdir -p $out/openvpn $r/etc/openvpn/client
    ec="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 36500"
    openssl req -x509 -new $ec -subj /CN=rime-hostgates-TEST-ca \
      -keyout ca.key -out $out/openvpn/ca.crt 2>/dev/null
    openssl req -x509 -new $ec -subj /CN=peer -CA $out/openvpn/ca.crt -CAkey ca.key \
      -addext extendedKeyUsage=serverAuth -keyout $out/openvpn/server.key -out $out/openvpn/server.crt 2>/dev/null
    openssl req -x509 -new $ec -subj /CN=rime-hostgates-TEST-machine -CA $out/openvpn/ca.crt -CAkey ca.key \
      -addext extendedKeyUsage=clientAuth -keyout $r/etc/openvpn/client/fixture.key \
      -out $r/etc/openvpn/client/fixture.crt 2>/dev/null
    cp $out/openvpn/ca.crt $r/etc/openvpn/client/ca.crt
    openssl verify -CAfile $out/openvpn/ca.crt $out/openvpn/server.crt $r/etc/openvpn/client/fixture.crt
    cat > $r/etc/NetworkManager/system-connections/FixtureOpenVPN.nmconnection <<EOF
    [connection]
    id=FixtureOpenVPN
    uuid=6c1e1f0a-0000-4000-8000-000000000005
    type=vpn
    autoconnect=false

    [vpn]
    service-type=org.freedesktop.NetworkManager.openvpn
    connection-type=tls
    remote=peer:1194
    dev-type=tun
    ca=/etc/openvpn/client/ca.crt
    cert=/etc/openvpn/client/fixture.crt
    key=/etc/openvpn/client/fixture.key
    cert-pass-flags=4

    [ipv4]
    method=auto
    never-default=true

    [ipv6]
    method=disabled
    EOF

    # ── H6-H8 network side: shares in the fstab ──
    ${pkgs.lib.optionalString netfs ''
      cat >> $r/etc/fstab <<EOF
      peer:/export /srv/share nfs4 _netdev,nofail 0 0
      //peer/none /srv/smb cifs nofail,guest 0 0
      EOF
    ''}
    # ── H6-H8 disk side: besides the base fixture's two ext4 disks, a btrfs one (fsck'd: passno 2, fsck.btrfs
    # answers success) and an xfs one (passno 0), as a Fedora machine may carry. ──
    ${pkgs.lib.optionalString (!netfs) ''
      cat >> $r/etc/fstab <<EOF
      UUID=f1a70003-0000-4000-8000-00000000f1a7 /var/media btrfs nofail,compress=zstd:1 0 2
      UUID=f1a70004-0000-4000-8000-00000000f1a7 /var/scratch xfs nofail,noatime 0 0
      EOF
    ''}

    # ── H10: katana's host-local files, same settings, comments of the fixture's own ──
    mkdir -p $r/etc/udev/rules.d $r/etc/sysctl.d
    cat > $r/etc/udev/rules.d/60-bfq-nvme.rules <<'EOF'
    # fixture: every NVMe disk on bfq
    ACTION=="add|change", KERNEL=="nvme[0-9]n[0-9]", ATTR{queue/scheduler}="bfq"
    EOF
    cat > $r/etc/udev/rules.d/99-dualsense-touchpad.rules <<'EOF'
    # fixture: the DualSense touchpad is not a pointer
    ACTION=="add|change", SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_VENDOR_ID}=="054c", ENV{ID_MODEL_ID}=="0ce6", ENV{ID_INPUT_TOUCHPAD}=="1", ENV{LIBINPUT_IGNORE_DEVICE}="1", ENV{ID_INPUT_MOUSE}="0", ENV{ID_INPUT_POINTER}="0"
    EOF
    cat > $r/etc/sysctl.d/90-desktop-writeback.conf <<'EOF'
    # fixture: writeback in steady steps
    vm.dirty_background_bytes=268435456
    vm.dirty_bytes =   1073741824
    EOF

    cp ${base}/perms $out/perms
    chmod u+w $out/perms
    echo "0600 0 0 etc/NetworkManager/system-connections/FixtureVPN.nmconnection" >> $out/perms
    echo "0600 0 0 etc/NetworkManager/system-connections/FixtureOpenVPN.nmconnection" >> $out/perms
    echo "0600 0 0 etc/openvpn/client/fixture.key" >> $out/perms
    echo "0600 0 0 etc/NetworkManager/system-connections/FixtureWifi-Pinned.nmconnection" >> $out/perms

    cat > $out/cmdline <<'EOF'
    BOOT_IMAGE=(hd0,gpt2)/boot/ostree/default-0/vmlinuz-7.2.9 root=UUID=0b1c2d3e-0000-4000-8000-000000000001 rootflags=subvol=root rw rd.luks.uuid=luks-0b1c2d3e-0000-4000-8000-0000000000bb rd.luks.options=0b1c2d3e-0000-4000-8000-0000000000bb=discard,tpm2-device=auto rd.vconsole.keymap=uk resume=UUID=0b1c2d3e-0000-4000-8000-0000000000cc ostree=/ostree/boot.1/default/0 rd.driver.blacklist=nouveau quiet
    EOF
  ''
