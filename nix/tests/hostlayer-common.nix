# Shared by nix/tests/hostlayer.nix (a synthetic bootc root) and nix/tests/hostlayer-katana.nix (katana's sanitised
# /etc, from a 9p share at run time). Not a test itself.
#
# node { from, prepare }   the rime-vm host plus a stand-in for the bridge/installer: at the first activation (in the
#                          initrd, before `users`) it runs `rime-hostlayer carry --from <from> --root /` exactly once,
#                          then makes the carried people's (empty) homes, which a real machine already has on disk.
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
    { from, prepare ? "" }:
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
          ${hl} carry --from ${from} --root / --system "$systemConfig" > /var/lib/rime-test/carry.log 2>&1 || rc=$?
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
            m.succeed(f"test -L /run/systemd/generator/local-fs.target.wants/{shlex.quote(unit)}")
            assert "Before=local-fs.target" not in text, f"{unit} would hold local-fs.target"
            print(m.execute(f"systemctl status --no-pager {shlex.quote(unit)}")[1])
        m.fail("systemctl is-active emergency.target")
        m.fail("systemctl is-active rescue.target")
        m.succeed("systemctl is-active local-fs.target")
        m.succeed("systemctl is-active multi-user.target")
        print("system:", m.execute("systemctl is-system-running")[1])

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
