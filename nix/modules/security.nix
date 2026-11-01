# Security: Rime's security model on NixOS (PLAN.md 4.7, D4; STATE.md A2, A12, A13).
#
# SELinux stays on the bootc images until they sunset (A2). nixpkgs builds systemd without SELinux
# and Rime's two policy modules confined nothing of Rime's own, so the NixOS side does not port
# them. What protects a NixOS Rime machine instead is the gate list G1-G16 in PLAN.md D4; this module
# carries the parts of G1-G9 (and G12) that are operating-system configuration, and
# nix/tests/security.nix proves each of them in a VM, with a mutant that removes the protection.
#
#   G1  the nix-daemon socket is not reachable from an agent session: it lives under /run, which
#       every confined session gets as an empty tmpfs (rime-agent-core sandbox.rs, step 4)
#   G2  the agent sandbox finds bwrap at the fixed path it checks and refuses without it
#   G4  /nix is never writable in a session (that half is rimed code: sandbox.rs NEVER_WRITABLE)
#   G5  TIOCSTI is off (dev.tty.legacy_tiocsti = 0); rime-agentd refuses to confine when it is on
#   G6  polkit: rimed's two actions, the agent runtime's two and the Shell's one, each checked at
#       build time against the rules Containerfile.base enforced (system.checks)
#   G7  rime-agentd is not wrapped and keeps its unit name (packaging, tested in the VM)
#   G8  rime-firewall is the only firewall: NixOS's own firewall and nftables.service are refused
#   G9  sshd accepts keys only (A13: a behaviour change, named in the release note)
#   G12 the lockdown LSM stays in the LSM list NixOS writes on the kernel command line
#
# Decisions recorded here, measured on katana (Fedora 45 Rime) on 2026-11-01:
# - PAM: Rime uses authselect's `local` profile without `with-faillock`, so no PAM service runs
#   pam_faillock. NixOS's default PAM stack is the same shape; nothing is added.
# - audit: auditd is not installed on the bootc image (`systemctl is-enabled auditd` = not-found).
#   NixOS keeps it off too.
# - sysctl: Rime ships no sysctl.d of its own (nothing under files/ sets one); katana's values are
#   Fedora's and the kernel's defaults. NixOS's defaults are the same or stricter (kptr_restrict 1 and
#   yama.ptrace_scope 1, where Fedora has 0), so only legacy_tiocsti is pinned (G5). One reads
#   differently: net.ipv4.conf.all.accept_redirects is 1 in the VM and 0 on katana, where it is a side
#   effect of IP forwarding (podman), not a Fedora setting (katana's default.accept_redirects is 1).
#   The test's baseline subtest prints the full set.
# - AppArmor: not at launch (D4 option C). The NixOS module exists; it comes later as an opt-in.
# - Module signature enforcement: off at launch (A12); the kernel area owns signing.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.security;

  # Test-only: is this protection removed? Only nix/tests/security.nix sets it.
  weak = gate: lib.elem gate cfg.testing.weaken;

  # rime-firewall (bootc: /usr/libexec/rime-firewall, /usr/share/rime/...), with its paths in the store.
  firewall = pkgs.stdenv.mkDerivation {
    pname = "rime-firewall";
    version = "0-unstable";
    dontUnpack = true;
    nativeBuildInputs = [ pkgs.makeWrapper ];
    buildInputs = [ pkgs.bash ];
    installPhase = ''
      runHook preInstall
      install -Dm755 ${../../files/system/libexec/rime-firewall} $out/libexec/rime-firewall
      install -Dm644 ${../../files/system/firewall/services} $out/share/rime/firewall/services
      install -Dm644 ${../../files/system/nftables/rime.nft} $out/share/rime/nftables/rime.nft
      install -Dm755 ${../../files/system/NetworkManager/dispatcher.d/50-rime-hotspot-firewall} \
        $out/libexec/rime-hotspot-firewall
      substituteInPlace $out/libexec/rime-firewall \
        --replace-fail /usr/share/rime/firewall/services $out/share/rime/firewall/services
      substituteInPlace $out/libexec/rime-hotspot-firewall \
        --replace-fail /usr/libexec/rime-firewall $out/libexec/rime-firewall
      patchShebangs --host $out/libexec
      wrapProgram $out/libexec/rime-firewall --prefix PATH : ${lib.makeBinPath firewallPath}
      wrapProgram $out/libexec/rime-hotspot-firewall --prefix PATH : ${lib.makeBinPath firewallPath}
      runHook postInstall
    '';
    meta = {
      description = "Rime's default-drop incoming firewall and its exception tool";
      license = lib.licenses.mit;
      platforms = lib.platforms.linux;
    };
  };
  firewallPath = with pkgs; [ nftables coreutils gnugrep gnused gawk findutils networkmanager ];
  nft = "${pkgs.nftables}/bin/nft";

  # The agent runtime's policy, weakened for the G6 mutant only: break-glass answerable by any caller.
  agentPolicy =
    if weak "g6-polkit" then
      pkgs.runCommand "org.rimeos.agent.policy-weakened" { } ''
        sed -e '/id="org.rimeos.agent.break-glass"/,/<\/action>/ s|<allow_any>no</allow_any>|<allow_any>yes</allow_any>|' \
            -e '/id="org.rimeos.agent.break-glass"/,/<\/action>/ s|<allow_inactive>no</allow_inactive>|<allow_inactive>yes</allow_inactive>|' \
          ${../../files/system/polkit-1/actions/org.rimeos.agent.policy} > $out
        grep '<allow_any>yes' $out >/dev/null
      ''
    else
      ../../files/system/polkit-1/actions/org.rimeos.agent.policy;

  # The three action files (G6). The Shell's comes from its own source, as on bootc
  # (Containerfile.base installs it from the vendored tree), so the file the Shell ships is the one
  # polkit reads.
  polkitActions = pkgs.runCommand "rime-polkit-actions" { } ''
    install -Dm644 ${../../files/system/polkit-1/actions/org.rimeos.rimed.policy} \
      $out/share/polkit-1/actions/org.rimeos.rimed.policy
    install -Dm644 ${agentPolicy} $out/share/polkit-1/actions/org.rimeos.agent.policy
    install -Dm644 ${pkgs.rime.rime-shell.src}/dots-extra/polkit/org.rimeos.shell.agent.policy \
      $out/share/polkit-1/actions/org.rimeos.shell.agent.policy
  '';

  # Containerfile.base:262-301 and :948-962 as a build check: an unparseable action file is skipped
  # by polkitd in silence, a renamed action authorises nothing, and the elevation actions may never
  # be answerable without a human at the active session or cache a yes.
  polkitCheck = actions: pkgs.runCommand "rime-polkit-actions-check" {
    nativeBuildInputs = [ pkgs.libxml2 ];
  } ''
    set -eu
    a=${actions}/share/polkit-1/actions
    for f in "$a"/org.rimeos.*.policy; do xmllint --noout --nonet "$f"; done
    for id in rimed.manage-power rimed.manage-battery; do
      grep 'id="org.rimeos.'"$id"'"' "$a/org.rimeos.rimed.policy" >/dev/null
    done
    for id in agent.system-access agent.break-glass; do
      grep 'id="org.rimeos.'"$id"'"' "$a/org.rimeos.agent.policy" >/dev/null
    done
    grep 'id="org.rimeos.shell.agent.set-always-unrestricted"' "$a/org.rimeos.shell.agent.policy" >/dev/null
    if grep -E '<allow_[a-z]+>[a-z_]*_keep' "$a/org.rimeos.agent.policy"; then
      echo "FATAL: a _keep authorisation caches a yes nobody was asked for"; exit 1; fi
    if grep '<allow_any>yes' "$a/org.rimeos.agent.policy"; then
      echo "FATAL: <allow_any>yes hands the elevation action to any caller"; exit 1; fi
    if grep '<allow_inactive>yes' "$a/org.rimeos.agent.policy"; then
      echo "FATAL: <allow_inactive>yes hands the elevation action to an inactive session"; exit 1; fi
    if grep -E '<allow_(any|inactive)>(yes|auth_self_keep|auth_admin_keep)' \
        "$a/org.rimeos.shell.agent.policy"; then
      echo "FATAL: the shell's policy grants an inactive or remote session"; exit 1; fi
    touch $out
  '';

  daemonSocketDir = "/run/nix/daemon-socket";
in
{
  options.rime.security = {
    enable = lib.mkEnableOption "Rime's security policy";

    ssh.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Run sshd, as the bootc image does (rime.nft always accepts tcp/22: remote agents,
        `rime host run` and `rime build --on` are ssh). Keys only, whatever this is set to (A13).
      '';
    };

    firewall.package = lib.mkOption {
      type = lib.types.package;
      default = firewall;
      defaultText = lib.literalMD "rime-firewall built from `files/system`";
      readOnly = true;
      description = ''
        rime-firewall with its store paths. `rime firewall` (rimed ops.rs FIREWALL_ENGINE, bootc
        /usr/libexec/rime-firewall) must run `''${package}/libexec/rime-firewall` on NixOS.
      '';
    };

    polkitActions = lib.mkOption {
      type = lib.types.package;
      default = polkitActions;
      readOnly = true;
      internal = true;
      description = "The three Rime polkit action files (G6).";
    };

    polkitActionsCheck = lib.mkOption {
      type = lib.types.package;
      default = polkitCheck polkitActions;
      readOnly = true;
      internal = true;
      description = ''
        The build check over this machine's action files (G6). In system.checks unless the G6 mutant
        is on; the security test builds the mutant's copy and expects it to fail.
      '';
    };

    testing.weaken = lib.mkOption {
      type = lib.types.listOf (lib.types.enum [
        "g1-nix-socket"
        "g2-bwrap"
        "g5-tiocsti"
        "g6-polkit"
        "g8-second-firewall"
        "g9-ssh-password"
      ]);
      default = [ ];
      internal = true;
      visible = false;
      description = ''
        TEST ONLY. Removes the named protection so nix/tests/security.nix can show that its gate
        check fails without it. Never set this on a real machine.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      warnings = lib.optional (cfg.testing.weaken != [ ])
        "rime.security.testing.weaken is set (${lib.concatStringsSep ", " cfg.testing.weaken}): this machine's security gates are deliberately broken";
    }

    # ── G8: rime-firewall is the only firewall ─────────────────────────────────────────────────
    # NixOS's firewall defaults to on. Left on, its drop policy stacks with rime.nft's (every base
    # chain on a hook is evaluated and a drop in any one is final), so a port the user opens with
    # `rime firewall allow` stays shut. nftables.service is refused for a second reason: it runs
    # `flush ruleset`, which would delete Rime's table.
    {
      networking.firewall.enable = lib.mkDefault (weak "g8-second-firewall");
      assertions = [
        {
          assertion = config.networking.firewall.enable -> weak "g8-second-firewall";
          message = "rime.security: rime-firewall is Rime's only firewall; set networking.firewall.enable = false and use `rime firewall allow` (PLAN.md D4 G8).";
        }
        {
          assertion = !config.networking.nftables.enable;
          message = "rime.security: networking.nftables.enable runs nftables.service, which flushes the whole ruleset including Rime's `inet rime` table; leave it off (PLAN.md D4 G8).";
        }
      ];

      # `firewall` is the one rime-firewall on the system profile: /run/current-system/sw/libexec/rime-firewall is
      # what `rime firewall` runs (rimed RIME_FIREWALL) and share/rime/nftables/rime.nft what rime-devices reports.
      # rime-libexec deliberately does not ship a second copy (buildEnv would pick one silently).
      environment.systemPackages = [ pkgs.nftables firewall ];

      # The unit is files/system/units/rime-firewall.service with store paths.
      systemd.services.rime-firewall = {
        description = "Rime default incoming firewall";
        documentation = [ "man:nft(8)" ];
        # Before the network is configured, so no interface is ever up and unfiltered.
        unitConfig.DefaultDependencies = false;
        wants = [ "network-pre.target" ];
        before = [ "network-pre.target" "shutdown.target" ];
        conflicts = [ "shutdown.target" "nftables.service" ];
        wantedBy = [ "multi-user.target" ];
        # A switch reloads rather than restarts: a restart runs ExecStop, which deletes the table and
        # leaves the machine open until the start. The ruleset file deletes and recreates its own
        # table in one transaction, so a reload is atomic for the policy; the exceptions follow it.
        reloadIfChanged = true;
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = [
            "${nft} -f ${firewall}/share/rime/nftables/rime.nft"
            # Exceptions after the policy, so a malformed one cannot take the base policy down.
            "-${firewall}/libexec/rime-firewall reload"
          ];
          ExecReload = [
            "${nft} -f ${firewall}/share/rime/nftables/rime.nft"
            "-${firewall}/libexec/rime-firewall reload"
          ];
          # Flushing on stop leaves the machine open; stopping is a deliberate act and says so.
          ExecStop = "${nft} delete table inet rime";
        };
      };

      # User decisions (`rime firewall allow`), imperative on purpose: never managed by environment.etc.
      systemd.tmpfiles.rules = [ "d /etc/rime/firewall.d 0755 root root - -" ];

      networking.networkmanager.dispatcherScripts = lib.mkIf config.networking.networkmanager.enable [
        {
          source = "${firewall}/libexec/rime-hotspot-firewall";
          type = "basic";
        }
      ];
    }

    # ── G9: sshd, keys only (A13) ──────────────────────────────────────────────────────────────
    # nixpkgs defaults both to yes. bootc never restricted password login, so this is a behaviour
    # change for anyone who logs in with a password today; the release note says so (D4).
    {
      services.openssh = {
        enable = lib.mkDefault cfg.ssh.enable;
        settings = {
          PasswordAuthentication = weak "g9-ssh-password";
          KbdInteractiveAuthentication = weak "g9-ssh-password";
          PermitRootLogin = lib.mkDefault "prohibit-password";
        };
      };
    }

    # ── G1: no nix-daemon socket inside an agent session ──────────────────────────────────────
    # The socket is mode 0666 and allowed-users is `*`, so any process that can reach it can ask the
    # daemon to build and to add GC roots. `--ro-bind / /` would expose /nix/var/nix/daemon-socket,
    # and connecting to a socket on a read-only mount still works. So the daemon listens under /run,
    # which the agent sandbox replaces with an empty tmpfs for every confined session (the same
    # default-deny mask that already keeps the system bus out), and the canonical path is a symlink
    # to it: on the host every Nix client follows it; inside a session it points at nothing.
    # A tmpfs mask of the directory itself would only hold until something recreated the socket.
    (lib.mkIf (config.nix.enable && !weak "g1-nix-socket") {
      systemd.sockets.nix-daemon.listenStreams = [ "" "${daemonSocketDir}/socket" ];
      systemd.tmpfiles.rules = [
        "d ${daemonSocketDir} 0755 ${config.nix.daemonUser} ${config.nix.daemonGroup} - -"
        "L+ /nix/var/nix/daemon-socket/socket - - - - ${daemonSocketDir}/socket"
      ];
    })

    # ── G2: the sandbox's bwrap is where the sandbox looks, and nowhere else ──────────────────
    # rime-agent-core resolves bwrap at a fixed path, never through PATH (sandbox.rs bwrap_path), and
    # refuses every confined session when it is missing. rimed's build-time path module compiles that
    # path in as bubblewrap's store path (nix/pkgs/rimed.nix BWRAP), so nothing is linked under /usr.
    # bwrap stays unprivileged (no setuid wrapper): the sandbox uses user namespaces.
    # The G2 mutant is a rimed whose compiled path names nothing (tests skipped: only the path changes).
    (lib.mkIf (weak "g2-bwrap") {
      rime.services.rimed.package = pkgs.rime.rimed.overrideAttrs (old: {
        env = old.env // { RIME_PATH_BWRAP = "/nonexistent/bwrap"; };
        doCheck = false;
      });
    })

    # ── G5 and kernel parameters ───────────────────────────────────────────────────────────────
    {
      # TIOCSTI lets a process push keystrokes into its controlling terminal; an agent session keeps
      # the user's PTY (no --new-session), so this must be off. rime-agentd refuses to confine a
      # session while it is on (sandbox.rs preflight). Kernel default since 6.2 is 0; pinned here.
      boot.kernel.sysctl."dev.tty.legacy_tiocsti" = if weak "g5-tiocsti" then 1 else 0;

      # G12: NixOS writes `lsm=` on the command line (landlock,yama,bpf), which overrides CONFIG_LSM.
      # Whether that drops lockdown depends on the kernel. nixpkgs' default kernel sets
      # SECURITY_LOCKDOWN_LSM_EARLY, so lockdown initialises whatever `lsm=` says (measured: a VM
      # with no Rime module and `lsm=landlock,yama,bpf` still lists lockdown first). Rime's kernel
      # (nix/pkgs/kernel/config, as on katana) does not set it, so there lockdown loads only if `lsm=`
      # names it (katana today: capability,lockdown,yama,selinux,bpf,landlock,ima, lockdown [none]).
      # Keep the LSM loaded so its state stays readable (`rime live doctor` reads it). The security
      # test checks the command line, since its VMs run nixpkgs' kernel.
      security.lsm = lib.mkBefore [ "lockdown" ];
    }

    # ── G6: polkit ─────────────────────────────────────────────────────────────────────────────
    {
      security.polkit.enable = true;
      # NixOS links share/polkit-1 from system packages into the directory polkitd reads.
      environment.systemPackages = [ polkitActions ];
      system.checks = lib.optional (!weak "g6-polkit") cfg.polkitActionsCheck;
      # files/system/polkit-1/rules.d/49-rime-singbox.rules (rime-shell's dots-extra twin is the same
      # rule): the person at the machine may start, stop and restart the VPN without a password.
      security.polkit.extraConfig = builtins.readFile ../../files/system/polkit-1/rules.d/49-rime-singbox.rules;
    }
  ]);
}
