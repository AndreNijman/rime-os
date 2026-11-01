# A bootc Rime OS root in miniature, shaped like katana's (nix/tests/hostlayer.nix carries it into a NixOS VM).
# Everything here is TEST material made inside this derivation: the password hash is "rime" with a fixed salt,
# the ssh host keys are generated at build time (never a file in the repository).
#
#   $out/root    the old system's /etc and /var/lib (only the parts the carry reads, plus some it must leave)
#   $out/perms   "<mode> <uid> <gid> <path>" for what a store path cannot hold: owners and private modes,
#                with the OLD system's numbers (greetd was 973 on Fedora), applied after copying out of the store
#   $out/collision   a second root whose one person collides with a NixOS static id (refused by name)
{ pkgs }:

pkgs.runCommand "rime-hostlayer-fixture"
  {
    nativeBuildInputs = [ pkgs.openssl pkgs.openssh ];
  }
  ''
    r=$out/root
    mkdir -p $r/etc/ssh $r/etc/NetworkManager/system-connections $r/etc/X11/xorg.conf.d $r/etc/rime/firewall.d \
      $r/etc/sing-box $r/etc/cups $r/etc/systemd/system/user-runtime-dir@1001.service.wants \
      $r/etc/pki/akmods/private $r/etc/hypr $r/var/lib/rime-greet "$r/var/lib/bluetooth/AA:BB:CC:DD:EE:FF" \
      $r/usr/etc
    hash=$(openssl passwd -6 -salt rimefixturesalt rime)

    # ── accounts (Fedora's numbering) ──
    cat > $r/etc/passwd <<EOF
    root:x:0:0:Super User:/root:/bin/bash
    bin:x:1:1:bin:/bin:/usr/sbin/nologin
    nobody:x:65534:65534:Kernel Overflow User:/:/usr/sbin/nologin
    dbus:x:81:81:System Message Bus:/:/usr/sbin/nologin
    sshd:x:74:74:Privilege-separated SSH:/usr/share/empty.sshd:/usr/sbin/nologin
    fedorabus:x:4:4:a Fedora account on a NixOS static id:/:/usr/sbin/nologin
    greetd:x:973:973:greetd greeter user:/var/lib/greetd:/sbin/nologin
    ghrunner:x:960:960:GitHub runner:/var/lab/runner:/bin/bash
    andre:x:1000:1000:Andre:/var/home/andre:/bin/zsh
    visitor:x:1001:1001:Visitor:/var/home/visitor:/bin/zsh
    EOF
    cat > $r/etc/group <<EOF
    root:x:0:
    bin:x:1:
    wheel:x:10:andre
    nobody:x:65534:
    dbus:x:81:
    sshd:x:74:
    fedorabus:x:4:
    greetd:x:973:
    ghrunner:x:960:
    andre:x:1000:
    visitor:x:1001:
    EOF
    cat > $r/etc/shadow <<EOF
    root:!locked::0:99999:7:::
    bin:*:20000:0:99999:7:::
    nobody:*:20000:0:99999:7:::
    dbus:!*:20000::::::
    sshd:!*:20000::::::
    fedorabus:!*:20000::::::
    greetd:!*:20000::::::
    ghrunner:!*:20000::::::
    andre:$hash:20000:0:99999:7:::
    visitor:$hash:20000:0:99999:7:::
    EOF
    awk -F: '{ print $1 ":!::" }' $r/etc/group > $r/etc/gshadow
    printf 'andre:524288:65536\nghrunner:1000000:65536\n' > $r/etc/subuid
    cp $r/etc/subuid $r/etc/subgid
    printf 'UID_MIN 1000\nUID_MAX 60000\nGID_MIN 1000\n' > $r/etc/login.defs

    # ── facts that become host.toml ──
    echo fixturebox > $r/etc/hostname
    ln -s ../usr/share/zoneinfo/Australia/Perth $r/etc/localtime
    printf 'LANG="en_US.UTF-8"\nLC_TIME=C.UTF-8\n' > $r/etc/locale.conf
    printf 'KEYMAP=uk\n' > $r/etc/vconsole.conf
    cat > $r/etc/X11/xorg.conf.d/00-keyboard.conf <<EOF
    Section "InputClass"
            Identifier "system-keyboard"
            MatchIsKeyboard "on"
            Option "XkbLayout" "gb"
            Option "XkbOptions" "caps:escape"
    EndSection
    EOF
    cat > $r/etc/fstab <<EOF
    UUID=0b1c2d3e-0000-4000-8000-000000000001 / btrfs subvol=root,compress=zstd:1 0 0
    UUID=0b1c2d3e-0000-4000-8000-000000000002 /boot ext4 defaults 1 2
    UUID=0B1C-2D3E /boot/efi vfat umask=0077,shortname=winnt 0 2
    UUID=0b1c2d3e-0000-4000-8000-000000000001 /var btrfs subvol=var,compress=zstd:1 0 0
    UUID=a41fee21-0000-4000-8000-00000000f1a7 /var/home/andre/Games ext4 defaults,nofail 0 2
    UUID=8c8746e1-0000-4000-8000-00000000f1a7 /var/lab ext4 nofail,noatime 0 0
    EOF
    echo 'luks-fixture UUID=0b1c2d3e-0000-4000-8000-0000000000aa none discard,tpm2-device=auto' > $r/etc/crypttab

    # ── files carried as they are ──
    echo 5e1f0c0ffee04a11b0071e5500ddba11 > $r/etc/machine-id
    for t in ed25519 ecdsa rsa; do
      ssh-keygen -q -N "" -t $t $([ $t = rsa ] && echo -b 2048) -C "TEST fixture host key" -f $r/etc/ssh/ssh_host_''${t}_key
    done
    cat > "$r/etc/NetworkManager/system-connections/Fixture Wired.nmconnection" <<EOF
    [connection]
    id=Fixture Wired
    uuid=6c1e1f0a-0000-4000-8000-000000000001
    type=ethernet
    autoconnect=false
    interface-name=fixture0

    [ipv4]
    method=auto
    EOF
    cat > $r/etc/NetworkManager/system-connections/FixtureWifi-5G.nmconnection <<EOF
    [connection]
    id=FixtureWifi-5G
    uuid=6c1e1f0a-0000-4000-8000-000000000002
    type=wifi
    autoconnect=false

    [wifi]
    mode=infrastructure
    ssid=FixtureWifi-5G

    [wifi-security]
    key-mgmt=wpa-psk
    psk=TEST-not-a-real-passphrase

    [ipv4]
    method=auto
    EOF
    printf '[General]\nName=Fixture Headphones\n' > "$r/var/lib/bluetooth/AA:BB:CC:DD:EE:FF/settings"
    printf '<Printer Fixture>\nDeviceURI ipp://192.0.2.10/ipp/print\nState Idle\n</Printer>\n' > $r/etc/cups/printers.conf
    printf 'visitor\n' > $r/etc/rime/guest-accounts
    ln -s /usr/lib/systemd/system/rime-guest-session@.service \
      $r/etc/systemd/system/user-runtime-dir@1001.service.wants/rime-guest-session@1001.service
    printf 'tcp 2222\n' > $r/etc/rime/firewall.d/fixture-ssh.conf
    printf '{"log":{"level":"warn"}}\n' > $r/etc/sing-box/config.json
    printf andre > $r/var/lib/rime-greet/last-user
    printf hyprland > $r/var/lib/rime-greet/last-session

    # ── what the carry must only check, or leave and name ──
    printf 'registry=https://registry.npmjs.org/\n' > $r/etc/npmrc
    printf 'monitor=,preferred,auto,1\n' > $r/etc/hypr/hyprland.conf
    printf '[Service]\nExecStart=/usr/bin/true\n' > $r/etc/systemd/system/fixture-runner.service
    printf 'TEST not a key\n' > $r/etc/pki/akmods/private/fixture_signing.priv
    printf 'image default\n' > $r/usr/etc/motd
    cp $r/usr/etc/motd $r/etc/motd

    cat > $out/perms <<EOF
    0000 0 0 etc/shadow
    0000 0 0 etc/gshadow
    0600 0 0 etc/ssh/ssh_host_ed25519_key
    0600 0 0 etc/ssh/ssh_host_ecdsa_key
    0600 0 0 etc/ssh/ssh_host_rsa_key
    0600 0 0 etc/NetworkManager/system-connections/Fixture Wired.nmconnection
    0600 0 0 etc/NetworkManager/system-connections/FixtureWifi-5G.nmconnection
    0700 0 0 var/lib/bluetooth
    0600 0 0 etc/sing-box/config.json
    0400 0 0 etc/pki/akmods/private/fixture_signing.priv
    0644 973 973 var/lib/rime-greet/last-user
    0644 973 973 var/lib/rime-greet/last-session
    EOF

    # A person whose uid is a NixOS static id (messagebus, 4). Holding subordinate ids makes it someone to keep
    # exactly, so the carry must refuse, by name, rather than renumber or drop them.
    c=$out/collision
    mkdir -p $c/etc
    printf 'root:x:0:0::/root:/bin/bash\nbuilder:x:4:4:Builder:/var/home/builder:/bin/bash\n' > $c/etc/passwd
    printf 'root:x:0:\nbuilder:x:4:\n' > $c/etc/group
    printf 'root:!::\nbuilder:%s:20000::::::\n' "$hash" > $c/etc/shadow
    printf 'builder:200000:65536\n' > $c/etc/subuid
    cp $c/etc/subuid $c/etc/subgid
  ''
