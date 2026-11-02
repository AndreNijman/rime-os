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

    # ── H6-H8 network side: shares in the fstab ──
    ${pkgs.lib.optionalString netfs ''
      cat >> $r/etc/fstab <<EOF
      peer:/export /srv/share nfs4 _netdev,nofail 0 0
      //peer/none /srv/smb cifs nofail,guest 0 0
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

    cat > $out/cmdline <<'EOF'
    BOOT_IMAGE=(hd0,gpt2)/boot/ostree/default-0/vmlinuz-7.2.9 root=UUID=0b1c2d3e-0000-4000-8000-000000000001 rootflags=subvol=root rw rd.luks.uuid=luks-0b1c2d3e-0000-4000-8000-0000000000bb rd.luks.options=0b1c2d3e-0000-4000-8000-0000000000bb=discard,tpm2-device=auto rd.vconsole.keymap=uk resume=UUID=0b1c2d3e-0000-4000-8000-0000000000cc ostree=/ostree/boot.1/default/0 rd.driver.blacklist=nouveau quiet
    EOF
  ''
