# What a Rime machine does when the kernel dies (docs/kernel-crash.md), the same on NixOS as on bootc:
#
#   * the kernel arguments of files/system/kargs.d/30-rime-crash.toml, read from that one file (bootc copies it to
#     /usr/lib/bootc/kargs.d), so the two systems cannot drift: a readable panic screen, a restart 30 s after a
#     panic, and the panic's log tail kept in EFI variables for systemd-pstore;
#   * libexec: files/system/units/rime-crash-record.service (root, at boot, before logins: the summary in
#     /var/lib/rime/crash/last that `rime crash` prints, and a line per crash in history) and
#     rime-crash-notice.{service,timer} (user: one notification per crash, retried when it did not get through).
#
# Independent of rime.kernel.enable: nixpkgs' kernel reads the same arguments, and a machine on either kernel must
# restart after a panic instead of waiting forever.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.crash;
  libexec = pkgs.rime.rime-libexec;
  helper = name: "${libexec}/libexec/${name}";
in
{
  options.rime.crash.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = ''
      Restart 30 s after a kernel panic with a readable panic screen, keep the panic's kernel log in EFI variables
      (collected by systemd-pstore at the next boot), and say what happened once after the restart (`rime crash`).
    '';
  };

  # The arguments, read from the file bootc ships. An option only so nix/tests/kernel-panic.nix can build its mutants
  # (one argument left out each); nothing else sets it.
  options.rime.crash.kernelParams = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    internal = true;
    default = (builtins.fromTOML (builtins.readFile ../../files/system/kargs.d/30-rime-crash.toml)).kargs;
    description = "The kernel arguments of files/system/kargs.d/30-rime-crash.toml.";
  };

  config = lib.mkIf cfg.enable {
    # Last on the command line: the kernel takes the last `panic=` it is given, so an earlier one (the NixOS test
    # framework adds `panic=1`) cannot win over Rime's.
    boot.kernelParams = lib.mkAfter cfg.kernelParams;

    # systemd-pstore is upstream's (nixpkgs enables it, wantedBy sysinit.target); this only reads what it collected.
    systemd.services.rime-crash-record = {
      description = "Rime OS kernel crash summary";
      documentation = [ "man:rime(1)" ];
      after = [ "systemd-pstore.service" "local-fs.target" ];
      before = [ "systemd-user-sessions.service" ];
      wantedBy = [ "multi-user.target" ];
      unitConfig.ConditionDirectoryNotEmpty = "/var/lib/systemd/pstore";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = helper "rime-crash-record";
        StateDirectory = "rime/crash";
        StateDirectoryMode = "0755";
        TimeoutStartSec = "30s";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateNetwork = true;
        PrivateTmp = true;
      };
    };

    systemd.user.services.rime-crash-notice = {
      description = "Rime OS kernel crash notice";
      documentation = [ "man:rime(1)" ];
      unitConfig = {
        ConditionPathExists = "/var/lib/rime/crash/history";
        StartLimitIntervalSec = "10min";
        StartLimitBurst = 5;
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = helper "rime-crash-notice";
        Restart = "on-failure";
        RestartSec = "1min";
        NoNewPrivileges = true;
      };
    };
    systemd.user.timers.rime-crash-notice = {
      description = "Say once per session whether a kernel crash restarted the machine";
      wantedBy = [ "timers.target" ];
      timerConfig = { OnStartupSec = "1min"; Unit = "rime-crash-notice.service"; };
    };
  };
}
