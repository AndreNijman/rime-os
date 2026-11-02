# Equibop on Rime on NixOS (docs/nixos/p2-gates.md P4): the catalogue's `equibop` (nixpkgs' build) runs, and its
# Rich Presence server (arRPC) listens and answers, started the way Gaming Mode starts it: Rime's own
# rime-gaming-discord, hidden and muted, owned by a process whose exit stops it.
#
# No Discord account and no network: Equibop's window cannot load discord.com, which is not what is tested. Without
# a cached Equicord (sessionData/equicord.asar) the offline Equibop never starts arRPC: it waits on the download
# ("Failed to fetch .../equibop.asar. Gonna retry with backoff."), the mutant this check was first seen to fail on. What is
# tested is that the Electron main process starts, that arRPC binds $XDG_RUNTIME_DIR/discord-ipc-0, and that it
# answers a game's IPC handshake (the Discord RPC frame protocol: op 0 HANDSHAKE in, op 1 READY out).
{ pkgs, self }:

let
  # A path literal, not "${self}/...": only this file is hashed, so an unrelated change elsewhere in the repository
  # does not re-run the VM test.
  discord = ../../files/system/libexec/rime-gaming-discord;

  # A game's side of Discord RPC: HANDSHAKE, then the READY dispatch arRPC sends back.
  handshake = pkgs.writeText "rpc-handshake.py" ''
    import json, os, socket, struct, sys
    path = os.path.join(os.environ["XDG_RUNTIME_DIR"], "discord-ipc-0")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(20)
    s.connect(path)
    body = json.dumps({"v": 1, "client_id": "1234567890"}).encode()
    s.sendall(struct.pack("<II", 0, len(body)) + body)
    op, n = struct.unpack("<II", s.recv(8))
    data = b""
    while len(data) < n:
        data += s.recv(n - len(data))
    msg = json.loads(data)
    print(json.dumps({"op": op, "msg": msg}))
    sys.exit(0 if op == 1 and msg.get("evt") == "READY" else 1)
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-equibop";
  node.pkgsReadOnly = false;

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    rime.catalogue.enable = true;
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
    environment.systemPackages = [ pkgs.equibop pkgs.xvfb-run pkgs.python3 pkgs.procps ];
    fonts.enableDefaultPackages = true;
    virtualisation = {
      memorySize = 4096;
      cores = 2;
    };
  };

  testScript = ''
    import json

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1000.service")
    RUN = "/run/user/1000"
    def alice(cmd, fail=False):
        full = f"su - alice -c 'export XDG_RUNTIME_DIR={RUN}; {cmd}' 2>&1"
        return machine.fail(full) if fail else machine.succeed(full)

    with subtest("the catalogue has equibop as a class 2 app (nixpkgs' build)"):
        cat = json.loads(machine.succeed("cat /etc/rime/catalogue.json"))
        e = cat["entries"]["equibop"]
        assert e["kind"] == "nix" and e["buildable"] and e["version"] == "${pkgs.equibop.version}", e

    with subtest("Equibop's settings: arRPC on, first launch done (what a signed-in desktop Equibop leaves)"):
        # Created as alice: `install -D` as root would leave ~/.config root-owned.
        alice("mkdir -p ~/.config/equibop/sessionData")
        # Equibop's first start downloads Equicord's equibop.asar from GitHub into sessionData/equicord.asar and does
        # nothing else (arRPC included) until it has it; the VM is offline. A desktop Equibop that has run once has
        # it, so the fixture puts nixpkgs' build of the same file there.
        alice("cp ${pkgs.equicord}/equibop.asar ~/.config/equibop/sessionData/equicord.asar")
        for path, doc in [("settings.json", {"arRPC": True}), ("state.json", {"firstLaunch": False})]:
            machine.succeed(f"install -o alice -g users -m 644 /dev/stdin /home/alice/.config/equibop/{path} <<'EOF'\n{json.dumps(doc)}\nEOF")
        alice("python3 ${discord} set on")
        st = json.loads(alice("python3 ${discord} status --json"))
        print(st)
        assert st["installed"] and st["kind"] == "native" and st["rich_presence"] and st["ready"], st
        assert "equibop" in st["source"], st

    with subtest("Gaming Mode's helper starts it hidden; arRPC listens and answers a game's handshake"):
        # The owner stands in for the Gaming Mode session: when it exits, Equibop must stop.
        alice("systemd-run --user --unit=owner ${pkgs.coreutils}/bin/sleep 600")
        owner = alice("systemctl --user show -p MainPID --value owner").strip()
        alice(f"systemd-run --user --unit=discord ${pkgs.xvfb-run}/bin/xvfb-run -a ${pkgs.python3}/bin/python3 ${discord} run --owner {owner}")
        try:
            machine.wait_until_succeeds(f"test -S {RUN}/discord-ipc-0", timeout=180)
        except Exception:
            # What Equibop did instead: its output, its processes and its config.
            print(alice("journalctl --user -u discord --no-pager | tail -n 80"))
            print(alice("ps -u alice -o pid=,ppid=,args= | cut -c1-220; ls -la ~/.config/equibop ~/.config/equibop/* | head -n 60"))
            print(alice("tail -n 40 ~/.cache/rime/gaming-discord.log || true"))
            raise
        procs = alice("ps -u alice -o pid=,args=")
        print(procs)
        assert "--ozone-platform=x11" in procs and "--disable-gpu" in procs, procs
        out = alice("python3 ${handshake}")
        print(out)
        ready = json.loads(out.strip().splitlines()[-1])
        assert ready["op"] == 1 and ready["msg"]["evt"] == "READY", ready

    with subtest("the owner exits: Equibop stops and nothing answers on the socket"):
        alice("systemctl --user stop owner")
        # [1]: the pattern must not match the command line that carries it.
        machine.wait_until_fails("pgrep -u alice -f 'ozone-platform=x1[1]'", timeout=60)
        # Nothing answers a game any more (a killed Node server may leave the socket file behind).
        alice("python3 ${handshake}", fail=True)
        print(alice("journalctl --user -u discord --no-pager | tail -n 20"))
        print(alice("tail -n 20 ~/.cache/rime/gaming-discord.log || true"))
  '';
}
