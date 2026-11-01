# Security gates G1-G9 (PLAN.md D4), each shown in a VM to PASS with its protection and to FAIL with it
# removed. The protections are removed by the test-only option rime.security.testing.weaken (or, for
# the two that live in the rimed package, by a mutated rimed built here), never by editing a node at
# runtime, so every mutant is a configuration a machine could actually have.
#
# Nodes:
#   rime     every protection on
#   weak     G1, G4, G6, G7, G8, G9 removed (none of them stops the others' checks from running)
#   tiocsti  G5 removed (on its own: it makes rime-agentd refuse every confined session)
#   nobwrap  G2 removed (on its own, for the same reason)
#   client   another machine on the LAN, for the firewall and ssh gates
#
# G3 (rime-aid fails closed) is not here: its code is on fix/rime-aid-fails-closed (bootc-first PR 9)
# and reaches feat/nixos with main. G10-G16 are other PRs' gates; G12 is checked in passing.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  keys = import (pkgs.path + "/nixos/tests/ssh-keys.nix") pkgs;
  password = "correct horse battery staple";

  # G4 + G7 mutant: rimed with /nix writable in sessions and rime-agentd behind a wrapper (so
  # /proc/<pid>/exe is .rime-agentd-wrapped). The unit rename that completes G7's mutant is below.
  rimedWeak = pkgs.rime.rimed.overrideAttrs (old: {
    pname = "rimed-weakened";
    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.makeWrapper ];
    postPatch = (old.postPatch or "") + ''
      substituteInPlace rimed/rime-agent-core/src/sandbox.rs \
        --replace-fail 'const NEVER_WRITABLE_TREES: &[&str] = &["/nix"];' \
                       'const NEVER_WRITABLE_TREES: &[&str] = &[];' \
        --replace-fail '    "/nix",
];' '];'
    '';
    postInstall = (old.postInstall or "") + ''
      wrapProgram $out/bin/rime-agentd --set RIME_TEST_WRAPPED 1
    '';
  });

  # The agent runtime and the secret service, as the services area will install them. Started here
  # because rime.services belongs to another area; the unit name is part of G7.
  agentRuntime = { weakened ? false }: { config, ... }:
    let
      rimed = if weakened then rimedWeak else pkgs.rime.rimed;
      unit = if weakened then "rime-agent-runtime" else "rime-agentd";
    in
    {
      environment.systemPackages = [ rimed pkgs.socat pkgs.python3 pkgs.git ];
      systemd.user.services.${unit} = {
        description = "Rime agent runtime (test)";
        wantedBy = [ "default.target" ];
        serviceConfig.ExecStart = "${rimed}/bin/rime-agentd";
      };
      systemd.services.rime-secretd = {
        description = "Rime secret service (test)";
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = "${rimed}/bin/rime-secretd";
          StateDirectory = "rime-secretd";
          StateDirectoryMode = "0700";
          RuntimeDirectory = "rime-secretd";
          RuntimeDirectoryMode = "0755";
        };
      };
      environment.etc."rime-test/rime".source = "${rimed}/bin/rime";
    };

  base = { weaken ? [ ], weakRimed ? false }: { config, ... }: {
    imports = [ self.nixosModules.default (agentRuntime { weakened = weakRimed; }) ];
    rime.security.enable = true;
    rime.security.testing.weaken = weaken;
    virtualisation.memorySize = 1536;

    users.users.alice = {
      isNormalUser = true;
      linger = true;
      inherit password;
      openssh.authorizedKeys.keys = [ keys.snakeOilPublicKey ];
    };
    # rime-agentd serves only callers it can place: a login session or a user service. The test
    # backdoor is neither, so sessions are started from a real login (ssh to localhost, key only).
    environment.etc."rime-test/id" = { source = keys.snakeOilPrivateKey; mode = "0600"; };
    # A directory under /nix that alice owns: what G4 must keep out of a session's writable binds.
    systemd.tmpfiles.rules = [
      "d /nix/var/nix/profiles/per-user/alice 0755 alice users - -"
      "d /home/alice/proj 0755 alice users - -"
    ];

    # Something to reach through the firewall (G8): a web server on port 80, the catalogue's `http`.
    systemd.services.web = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 80 --directory /etc";
    };
  };

  # Runs in the guest: does TIOCSTI inject into the caller's controlling terminal? Exit 0 = it did.
  tiocstiProbe = pkgs.writeScript "tiocsti-probe" ''
    #!${pkgs.python3}/bin/python3
    import fcntl, os, sys, termios
    pid, _ = os.forkpty()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSTI, b"x")
            os._exit(0)
        except OSError as e:
            os._exit(10 + e.errno % 50)
    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    print("injected" if code == 0 else "refused (errno %d)" % (code - 10))
    sys.exit(0 if code == 0 else 1)
  '';
in
pkgs.testers.runNixOSTest {
  name = "rime-security";
  node.pkgsReadOnly = false;

  nodes = {
    rime = base { };
    weak = base {
      weaken = [ "g1-nix-socket" "g6-polkit" "g8-second-firewall" "g9-ssh-password" ];
      weakRimed = true;
    };
    tiocsti = base { weaken = [ "g5-tiocsti" ]; };
    nobwrap = base { weaken = [ "g2-bwrap" ]; };
    client = { ... }: {
      environment.systemPackages = [ pkgs.sshpass pkgs.curl ];
    };
  };

  testScript = { nodes, ... }: ''
    import shlex

    AGENTD = {"rime": "rime-agentd", "weak": "rime-agent-runtime", "tiocsti": "rime-agentd", "nobwrap": "rime-agentd"}

    def alice(m, cmd, check=True):
        full = ("su - alice -c " + shlex.quote(
            "export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; " + cmd))
        if check:
            return m.succeed(full)
        return m.execute(full)

    def login(m, cmd):
        """Run `cmd` as alice inside a real login session (session-N.scope), as a person at the machine would."""
        return m.execute("ssh -i /etc/rime-test/id -o StrictHostKeyChecking=no -o BatchMode=yes alice@localhost "
                         + shlex.quote(cmd + " 2>&1"))

    def session(m, name, script, sandbox="project", cwd="/home/alice/proj"):
        """Start a detached agent session running `script`; return (rc, output of `rime agent run`)."""
        cmd = ("/etc/rime-test/rime agent run --agent generic --sandbox " + sandbox
               + " --cwd " + cwd + " -d -n " + name + " -- /bin/sh -c " + shlex.quote(script))
        return login(m, cmd)

    def wait_file(m, path):
        m.wait_until_succeeds("test -s " + path, timeout=60)
        return m.succeed("cat " + path)

    start_all()
    for m in (rime, weak, tiocsti, nobwrap):
        m.wait_for_unit("multi-user.target")
        m.wait_for_unit("user@1000.service")
        m.wait_until_succeeds("test -S /run/user/1000/rime-agentd/control.sock", timeout=60)
        m.wait_for_unit("sshd.service")
        m.wait_until_succeeds("ssh -i /etc/rime-test/id -o StrictHostKeyChecking=no -o BatchMode=yes alice@localhost true", timeout=60)
    client.wait_for_unit("multi-user.target")

    with subtest("baseline: what the protected machine runs"):
        print(rime.succeed("cat /proc/cmdline; cat /sys/kernel/security/lsm; echo; ls -l /nix/var/nix/daemon-socket/"))
        print(rime.succeed("for k in dev.tty.legacy_tiocsti kernel.kptr_restrict kernel.dmesg_restrict kernel.yama.ptrace_scope kernel.unprivileged_bpf_disabled fs.protected_hardlinks fs.protected_symlinks fs.protected_regular fs.protected_fifos fs.suid_dumpable kernel.sysrq kernel.perf_event_paranoid kernel.randomize_va_space net.ipv4.conf.all.rp_filter net.ipv4.conf.default.rp_filter net.ipv4.conf.all.accept_redirects net.ipv4.conf.all.secure_redirects net.ipv4.conf.all.send_redirects net.ipv4.conf.all.accept_source_route net.ipv6.conf.all.accept_redirects net.ipv4.tcp_syncookies net.ipv4.icmp_echo_ignore_broadcasts user.max_user_namespaces vm.unprivileged_userfaultfd kernel.io_uring_disabled; do echo \"$k = $(sysctl -n $k 2>&1)\"; done"))

    # ── G1 ──────────────────────────────────────────────────────────────────────────────────
    probe_g1 = ("${pkgs.socat}/bin/socat -u /dev/null UNIX-CONNECT:/nix/var/nix/daemon-socket/socket"
                " > ./g1.out 2>&1; echo rc=$? >> ./g1.out")

    def g1(m):
        m.succeed("rm -f /home/alice/proj/g1.out")
        print(session(m, "g1", probe_g1))
        out = wait_file(m, "/home/alice/proj/g1.out")
        print(m.name + " G1 in-session connect: " + out)
        if "rc=0" in out:
            return True
        # Refused, and for the reason under test: the path is not there in the session (not, say, a
        # missing socat or an unreadable store, which would make this check pass for nothing).
        assert "No such file or directory" in out, m.name + " G1: refused, but not for the reason under test: " + out
        return False

    with subtest("G1: the nix-daemon socket is unreachable from an agent session"):
        # The host side still works through the link (positive control).
        alice(rime, "nix --extra-experimental-features nix-command store info --store daemon")
        alice(weak, "nix --extra-experimental-features nix-command store info --store daemon")
        assert not g1(rime), "G1 FAILED on the protected machine: a session reached the nix-daemon socket"
        assert g1(weak), "G1 mutant: with the socket at its default path a session DOES reach it (the gate's check must see this)"

    # ── G2 ──────────────────────────────────────────────────────────────────────────────────
    with subtest("G2: a confined session runs only through bwrap at its fixed path, and is refused without it"):
        rime.succeed("rm -f /home/alice/proj/g2.out")
        print(session(rime, "g2", "test -e /run/dbus/system_bus_socket && echo bus-visible > ./g2.out || echo confined > ./g2.out"))
        assert wait_file(rime, "/home/alice/proj/g2.out").strip() == "confined", "G2 FAILED: the session was not confined"
        # Observation, not a gate: a confined session's /run is an empty tmpfs, so on NixOS it also loses
        # /run/current-system (the system PATH) and /run/wrappers. Printed for the services/rimed owners.
        rime.succeed("rm -f /home/alice/proj/g2-run.out")
        session(rime, "g2run", "for p in /run/current-system /run/wrappers /etc/static; do test -e $p && echo $p present || echo $p absent; done > ./g2-run.out")
        print("rime confined session sees: " + wait_file(rime, "/home/alice/proj/g2-run.out"))
        rc, out = session(nobwrap, "g2", "echo ran > ./g2.out")
        print("nobwrap: rc=%d %s" % (rc, out))
        assert rc != 0 and "bubblewrap" in out, "G2 mutant: without bwrap the session must be refused, naming bubblewrap"
        nobwrap.sleep(3)
        nobwrap.fail("test -e /home/alice/proj/g2.out")

    # ── G4 ──────────────────────────────────────────────────────────────────────────────────
    nixdir = "/nix/var/nix/profiles/per-user/alice"

    def g4(m):
        m.succeed("rm -f " + nixdir + "/marker")
        rc, out = session(m, "g4", "echo agent > ./marker", cwd=nixdir)
        print("%s G4: rc=%d %s" % (m.name, rc, out))
        if rc != 0:
            assert "may never be made writable" in out, "G4: refused, but not for the reason under test: " + out
            m.sleep(2)
            m.fail("test -e " + nixdir + "/marker")
            return False
        wait_file(m, nixdir + "/marker")
        return True

    with subtest("G4: nothing under /nix is ever writable in a session"):
        assert not g4(rime), "G4 FAILED on the protected machine: a session wrote under /nix"
        assert g4(weak), "G4 mutant: without the /nix rule a session writes into the user's Nix profile directory"

    # ── G5 ──────────────────────────────────────────────────────────────────────────────────
    with subtest("G5: TIOCSTI is blocked, and rime-agentd refuses to confine while it is not"):
        rime.succeed("test \"$(sysctl -n dev.tty.legacy_tiocsti)\" = 0")
        rc, out = alice(rime, "${tiocstiProbe}", check=False)
        print("rime TIOCSTI: " + out)
        assert rc != 0, "G5 FAILED on the protected machine: TIOCSTI injected a keystroke"
        rc, out = alice(tiocsti, "${tiocstiProbe}", check=False)
        print("tiocsti TIOCSTI: " + out)
        assert rc == 0, "G5 mutant: with legacy_tiocsti=1 the injection works (the gate's check must see this)"
        rc, out = session(tiocsti, "g5", "echo ran > ./g5.out")
        print("tiocsti session: rc=%d %s" % (rc, out))
        assert rc != 0 and "legacy_tiocsti" in out, "the agent runtime must refuse to confine while TIOCSTI is on"

    # ── G6 ──────────────────────────────────────────────────────────────────────────────────
    def pk_any(m, action):
        # A process outside any logind session: polkit answers it with <allow_any>.
        return m.execute("systemd-run --quiet --wait --pipe --uid=alice /bin/sh -c "
                         + shlex.quote("/run/current-system/sw/bin/pkcheck --action-id " + action
                                       + " --process $$$$") + " 2>&1")  # systemd-run turns $$ into $

    with subtest("G6: polkit carries Rime's three action files with human-only elevation"):
        # The build check refuses the mutant's policy file (and is in system.checks on every real machine).
        failed = "${pkgs.testers.testBuildFailure nodes.weak.rime.security.polkitActionsCheck}"
        polkit_log = open(failed + "/testBuildFailure.log").read()
        print("build check on the weakened policy: " + polkit_log)
        assert "FATAL" in polkit_log, "the polkit build check accepted a weakened policy"
        for action in ["org.rimeos.rimed.manage-power", "org.rimeos.rimed.manage-battery",
                       "org.rimeos.agent.system-access", "org.rimeos.agent.break-glass",
                       "org.rimeos.shell.agent.set-always-unrestricted"]:
            print(rime.succeed("pkaction --verbose --action-id " + action))
        bg = rime.succeed("pkaction --verbose --action-id org.rimeos.agent.break-glass")
        assert "implicit any:      no" in bg and "implicit inactive: no" in bg and "implicit active:   auth_admin" in bg, bg
        for m, expect_denied in ((rime, True), (weak, False)):
            rc, out = pk_any(m, "org.rimeos.agent.break-glass")
            print("%s break-glass from outside a session: rc=%d %s" % (m.name, rc, out))
            if expect_denied:
                # 1 = "not authorized"; anything else (126 = bad arguments) would prove nothing.
                assert rc == 1, "G6 FAILED: break-glass was not refused as unauthorized for a caller with no session"
            else:
                assert rc == 0, "G6 mutant: with <allow_any>yes break-glass is granted to anyone (the gate's check must see this)"

    # ── G7 ──────────────────────────────────────────────────────────────────────────────────
    def g7(m):
        m.succeed("rm -f /home/alice/proj/g7.out")
        rime_bin = m.succeed("readlink -f /etc/rime-test/rime").strip()
        print(session(m, "g7", rime_bin + " secret remove nothing-here > ./g7.out 2>&1; echo rc=$? >> ./g7.out",
                      sandbox="unrestricted"))
        out = wait_file(m, "/home/alice/proj/g7.out")
        print(m.name + " G7: " + out)
        return "an agent session cannot" in out

    with subtest("G7: the secret service recognises an agent session (agentd unwrapped, unit name kept)"):
        rime.wait_for_unit("rime-secretd.service")
        weak.wait_for_unit("rime-secretd.service")
        exe = rime.succeed("readlink /proc/$(pgrep -u alice -x rime-agentd)/exe")
        assert exe.strip().endswith("/bin/rime-agentd"), exe
        rime.succeed("grep rime-agentd.service /proc/$(pgrep -u alice -x rime-agentd)/cgroup")
        assert g7(rime), "G7 FAILED: secretd let an agent session change what is allowed"
        assert not g7(weak), "G7 mutant: wrapped agentd under another unit name is not recognised (the gate's check must see this)"

    # ── G8 ──────────────────────────────────────────────────────────────────────────────────
    def reachable(host):
        rc, _ = client.execute("curl --silent --max-time 5 --output /dev/null http://" + host + "/")
        return rc == 0

    with subtest("G8: rime-firewall is the only firewall"):
        fw = "${nodes.rime.rime.security.firewall.package}/libexec/rime-firewall"
        for m in (rime, weak):
            m.wait_for_unit("rime-firewall.service")
            m.wait_for_unit("web.service")
            m.wait_until_succeeds("curl --silent --max-time 2 --output /dev/null http://localhost/")
        print(rime.succeed("nft list ruleset"))
        rime.fail("systemctl is-active firewall.service")
        assert not reachable("rime"), "rime-firewall's default drop must hold before any exception"
        for m in (rime, weak):
            print(m.succeed(fw + " allow http"))
            print(m.succeed(fw + " status"))
        assert reachable("rime"), "G8 FAILED: a port opened with rime-firewall is not reachable"
        assert not reachable("weak"), "G8 mutant: with NixOS's firewall also on, the opened port stays shut (the gate's check must see this)"
        # The exception survives a firewall restart and is applied from /etc/rime/firewall.d.
        rime.succeed("systemctl restart rime-firewall.service")
        assert reachable("rime"), "the exception did not survive a rime-firewall restart"
        print(rime.succeed(fw + " deny http"))
        assert not reachable("rime"), "deny did not close the port again"

    # ── G9 ──────────────────────────────────────────────────────────────────────────────────
    def ssh_password(host):
        rc, out = client.execute(
            "sshpass -p " + shlex.quote("${password}") + " ssh -o StrictHostKeyChecking=no "
            "-o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive "
            "-o ConnectTimeout=10 alice@" + host + " true 2>&1")
        print(host + " password login: rc=%d %s" % (rc, out))
        return rc == 0

    with subtest("G9: sshd accepts keys only"):
        client.succeed("install -m 600 ${keys.snakeOilPrivateKey} /root/id")
        for m in (rime, weak):
            m.wait_for_unit("sshd.service")
            client.wait_until_succeeds("ssh -i /root/id -o StrictHostKeyChecking=no -o ConnectTimeout=5 alice@" + m.name + " true", timeout=60)
        print(rime.succeed("sshd -T | grep -Ei '^(passwordauthentication|kbdinteractiveauthentication|permitrootlogin)'"))
        assert not ssh_password("rime"), "G9 FAILED: sshd accepted a password"
        assert ssh_password("weak"), "G9 mutant: with password login on, the same login succeeds (the gate's check must see this)"

    with subtest("G12 (in passing): lockdown stays in the LSM list"):
        # These VMs run nixpkgs' kernel, which initialises lockdown early whatever `lsm=` says, so the
        # runtime list below cannot tell the module's setting apart from its absence (the client,
        # with no Rime module, lists lockdown too). Rime's kernel does not (LOCKDOWN_LSM_EARLY unset),
        # so the setting that matters there is the `lsm=` the system writes: checked on the command
        # line, against the client as the machine without it.
        def lsm_param(m):
            for word in m.succeed("cat /proc/cmdline").split():
                if word.startswith("lsm="):
                    return word[len("lsm="):].split(",")
            return []
        rime_lsm, client_lsm = lsm_param(rime), lsm_param(client)
        print("lsm= on rime: %s; on client: %s" % (rime_lsm, client_lsm))
        assert "lockdown" in rime_lsm, "G12 FAILED: the command line's lsm= drops lockdown on a kernel without LOCKDOWN_LSM_EARLY"
        assert client_lsm and "lockdown" not in client_lsm, "G12 mutant: NixOS's own lsm= (no Rime module) leaves lockdown out (the gate's check must see this)"
        lsm = rime.succeed("cat /sys/kernel/security/lsm")
        assert "lockdown" in lsm, lsm
        print(rime.succeed("cat /sys/kernel/security/lockdown"))
  '';
}
