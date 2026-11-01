# rimed and its helper daemons on a NixOS machine (PLAN.md 4.3): the services start under the hardening bootc ships,
# rimed answers on the system bus, and the CLI talks to it.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-rimed";

  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
    rime.services.enable = true;
    # An ordinary account whose user manager runs from boot, so the user units start without a login.
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
  };

  testScript = ''
    def user(cmd):
        return machine.succeed(f"su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 {cmd}'")

    machine.wait_for_unit("multi-user.target")

    with subtest("rimed is up and owns org.rimeos.Rimed1"):
        machine.wait_for_unit("rimed.service")
        # The unit is the shipped one, hardening included (a NixOS-generated unit would have none of it).
        assert machine.succeed("systemctl show -P MemoryDenyWriteExecute rimed.service").strip() == "yes"
        assert machine.succeed("systemctl show -P ProtectHome rimed.service").strip() == "yes"
        out = machine.succeed("busctl introspect org.rimeos.Rimed1 /org/rimeos/Rimed1")
        print(out)
        assert "org.rimeos.Rimed1" in out, "no org.rimeos.Rimed1 interface"
        # An unprivileged caller may talk to it (the bus policy's default context).
        user("busctl --system introspect org.rimeos.Rimed1 /org/rimeos/Rimed1 >/dev/null")

    with subtest("the rime CLI"):
        print(machine.succeed("rime --version"))
        print(machine.succeed("rime status"))
        print(user("rime status"))

    with subtest("rime-secretd: running, state 0700 under its old name"):
        machine.wait_for_unit("rime-secretd.service")
        assert machine.succeed("systemctl show -P ProtectSystem rime-secretd.service").strip() == "full"
        caps = machine.succeed("systemctl show -P CapabilityBoundingSet rime-secretd.service").split()
        assert sorted(caps) == ["cap_setgid", "cap_setuid", "cap_sys_ptrace"], caps
        machine.succeed("test -L /var/lib/rime-secretd")
        assert machine.succeed("stat -c %a /var/lib/apex-secretd").strip() == "700"

    with subtest("rime-lid: enabled, and on a machine without a lid it ends cleanly"):
        machine.succeed("systemctl is-enabled rime-lid.service")
        machine.wait_until_succeeds("test \"$(systemctl show -P SubState rime-lid.service)\" != start", timeout=60)
        machine.sleep(2)
        state = machine.succeed("systemctl show -P ActiveState,Result,SubState rime-lid.service")
        print(state)
        assert "failed" not in state, state

    with subtest("polkit actions are installed"):
        machine.succeed("pkaction --action-id org.rimeos.rimed.manage-power")

    with subtest("the AI model store (tmpfiles)"):
        assert machine.succeed("stat -c %a /var/lib/rime/ai/staging").strip() == "700"
        assert machine.succeed("stat -c %a /var/lib/rime/ai/models/blobs").strip() == "755"

    with subtest("user units: rime-remoted enabled for every account, the opt-in ones are not"):
        machine.wait_for_unit("user@1000.service")
        assert user("systemctl --user is-enabled rime-remoted.service").strip() == "enabled"
        # Not enabled: NixOS links the shipped units in ("linked-runtime"), nothing pulls them in by itself.
        for unit in ("rime-agentd", "rime-aid"):
            state = user(f"systemctl --user is-enabled {unit}.service || true").strip()
            print(unit, state)
            assert state not in ("enabled", "enabled-runtime"), f"{unit}: {state}"
        # remoted's Wants= starts the agent runtime.
        machine.wait_until_succeeds("su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active rime-agentd.service'", timeout=60)
        # rime-agentd keeps the PATH the package wrote for NixOS (no NixOS drop-in replaced it).
        env = user("systemctl --user show -P Environment rime-agentd.service")
        assert "/run/current-system/sw/bin" in env and "/usr/bin" not in env, env
        assert user("systemctl --user show -P ReadOnlyPaths rime-aid.service").strip() == "/var/lib/rime/ai"
        # The shipped [Install] lets an account opt in (what `rime agent enable` runs), and back out.
        user("systemctl --user enable rime-agentd.service")
        assert user("systemctl --user is-enabled rime-agentd.service").strip() == "enabled"
        user("systemctl --user disable rime-agentd.service")
  '';
}
