# Equibop on Rime on NixOS (docs/nixos/p2-gates.md P4): `rime install equibop` puts the catalogue's build (Rime's,
# nix/pkgs/rime-equibop.nix: nixpkgs' Equibop with nixpkgs' Equicord) into the rime-apps profile from a release
# record, and its Rich Presence server (arRPC) listens and answers, started the way Gaming Mode starts it: Rime's own
# rime-gaming-discord, hidden and muted, owned by a process whose exit stops it.
#
# No Discord account and no network: Equibop's window cannot load discord.com, which is not what is tested. Nothing is
# seeded either: stock nixpkgs Equibop, offline and with no sessionData/equicord.asar, never starts arRPC (it waits on
# github.com/Equicord/Equicord/releases/latest/download/equibop.asar, "Gonna retry with backoff"), and that download is
# an app updating itself outside `rime update`. Rime's build must start offline, answer, and write no Equicord copy.
# What is tested is that the Electron main process starts, that arRPC binds $XDG_RUNTIME_DIR/discord-ipc-0, and that
# it answers a game's IPC handshake (the Discord RPC frame protocol: op 0 HANDSHAKE in, op 1 READY out).
#
# Mutant (passthru.mutants.stock-equibop, built by hand): the catalogue's equibop pointed back at nixpkgs' stock
# build. arRPC never binds, so "arRPC listens" fails.
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

  test = pkgs.testers.runNixOSTest {
  name = "rime-equibop";
  node.pkgsReadOnly = false;

  nodes.machine = { config, pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    rime.catalogue.enable = true;
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
    # Equibop itself is NOT in the system: it comes from the release record, as on a real machine.
    environment.systemPackages = [ config.rime.services.rimed.package pkgs.xvfb-run pkgs.python3 pkgs.procps ];
    environment.etc."rime-test/equibop-outs.json".source = config.system.build.rimeCatalogue;
    virtualisation.additionalPaths = [ config.system.build.rimeCatalogue.passthru.apps.equibop ];
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

    with subtest("the catalogue has equibop as a class 2 app (Rime's build), and rime install puts it in rime-apps"):
        cat = json.loads(machine.succeed("cat /etc/rime/catalogue.json"))
        e = cat["entries"]["equibop"]
        assert e["kind"] == "nix" and e["buildable"] and e["version"] == "${pkgs.rime.rime-equibop.version}", e
        # The release record: only equibop's build of this system's catalogue (what the update client writes).
        outs = json.loads(machine.succeed("cat /etc/rime-test/equibop-outs.json"))
        machine.succeed("install -m 644 /dev/stdin /var/lib/rime/nix/catalogue-outs.json <<'EOF'\n" + json.dumps({"apps": {"equibop": outs["apps"]["equibop"]}}) + "\nEOF")
        out = machine.succeed("rime install equibop 2>&1")
        assert "equibop: installed (in /nix/var/nix/profiles/rime-apps" in out, out
        where = alice("command -v equibop").strip()
        assert where.startswith("/nix/var/nix/profiles/rime-apps/"), where

    with subtest("Equibop's settings: arRPC on, first launch done (what a signed-in desktop Equibop leaves)"):
        # Created as alice: `install -D` as root would leave ~/.config root-owned.
        alice("mkdir -p ~/.config/equibop/sessionData")
        # No Equicord is seeded into sessionData: Rime's Equibop reads nixpkgs' Equicord from the store.
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
        # --setenv=PATH: the login PATH, with the rime-apps profile, as the Gaming Mode session hands it down.
        alice(f"systemd-run --user --unit=discord --setenv=PATH ${pkgs.xvfb-run}/bin/xvfb-run -a ${pkgs.python3}/bin/python3 ${discord} run --owner {owner}")
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
        # Nothing was downloaded or tried: no Equicord copy in sessionData, no fetch in Equibop's output.
        alice("test ! -e ~/.config/equibop/sessionData/equicord.asar")
        logs = alice("journalctl --user -u discord --no-pager; cat ~/.cache/rime/gaming-discord.log 2>/dev/null || true")
        for bad in ("Equicord/Equicord/releases", "releases/latest/download"):
            assert bad not in logs, f"{bad!r} in Equibop's output:\n{logs}"

    with subtest("the owner exits: Equibop stops and nothing answers on the socket"):
        alice("systemctl --user stop owner")
        # [1]: the pattern must not match the command line that carries it.
        machine.wait_until_fails("pgrep -u alice -f 'ozone-platform=x1[1]'", timeout=60)
        # Nothing answers a game any more (a killed Node server may leave the socket file behind).
        alice("python3 ${handshake}", fail=True)
        print(alice("journalctl --user -u discord --no-pager | tail -n 20"))
        print(alice("tail -n 20 ~/.cache/rime/gaming-discord.log || true"))
  '';
};

in
# The mutant is an attribute of the check (checks.x86_64-linux.equibop.mutants.stock-equibop), not a check itself.
test
// {
  mutants.stock-equibop = test.extendNixOS {
    module = {
      rime.catalogue.extraEntries.equibop = {
        kind = "nix";
        attrs = [ "equibop" ];
        summary = "nixpkgs' stock Equibop (mutant)";
      };
    };
  };
}
