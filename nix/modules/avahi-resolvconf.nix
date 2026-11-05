# avahi and openresolv: avahi must survive a DNS change that lands while it starts, and a dead avahi must not block
# the next start.
#
# The race (nixpkgs at the pin, avahi 0.8, openresolv 3.17.4): every `resolvconf -u` (resolvconf.service at boot,
# NetworkManager on each DNS change) runs openresolv's libc.d/avahi-daemon hook, which sends SIGHUP to the PID in
# /run/avahi-daemon/pid. avahi writes that file (avahi-daemon/main.c, daemon_pid_file_create) BEFORE it installs
# its signal handlers (run_server → daemon_signal_init), so a hook that runs in between kills it with SIGHUP's
# default action. systemd counts that as a clean exit, so nothing restarts it, and the PID file stays: avahi's own
# stale-file removal runs as root inside nixpkgs' sandbox, whose CapabilityBoundingSet has no CAP_DAC_OVERRIDE, in a
# directory owned avahi:avahi 0755, so the unlink fails and every later start says "Failed to create PID file: File
# exists" until a reboot (mDNS, printer discovery and `rime install`'s live switch all fail; docs/nixos/open-items.md).
#
# The fix, in two parts:
#   1. The hook reads its PID file name from /etc/resolvconf.conf (`avahi_daemon_pidfile`, sourced by openresolv's
#      libc before it runs libc.d). It now names /run/avahi-daemon/ready, which systemd writes only once the unit is
#      started: avahi is Type=dbus, it takes its bus name after daemon_signal_init, so by then SIGHUP reloads
#      instead of killing. A DNS change still reaches a running avahi (the reload re-reads resolv.conf), exactly
#      as before; one that lands while avahi starts is not lost either, since avahi reads resolv.conf after it
#      takes its bus name.
#   2. Whatever killed avahi (a signal, the OOM killer), the next start begins clean: the PID and ready files are
#      removed before each start and after each stop by a command run with full privileges (`+`). systemd runs one
#      instance of the unit, so a file present at ExecStartPre is always a dead instance's.
{ config, lib, pkgs, ... }:

let
  dir = "/run/avahi-daemon";
  rm = "${pkgs.coreutils}/bin/rm";
  # systemd passes $MAINPID (avahi itself: the unit runs it in the foreground, so the main PID is the daemon).
  markReady = pkgs.writeShellScript "avahi-daemon-ready" ''
    set -eu
    printf '%s\n' "$1" > ${dir}/ready.tmp
    ${pkgs.coreutils}/bin/mv -f ${dir}/ready.tmp ${dir}/ready
  '';
in
{
  config = lib.mkIf (config.services.avahi.enable && config.networking.resolvconf.enable) {
    networking.resolvconf.extraConfig = ''
      # Rime (nix/modules/avahi-resolvconf.nix): SIGHUP avahi only once it handles the signal.
      avahi_daemon_pidfile=${dir}/ready
    '';

    systemd.services.avahi-daemon.serviceConfig = {
      ExecStartPre = [ "+${rm} -f ${dir}/pid ${dir}/ready ${dir}/ready.tmp" ];
      ExecStartPost = [ "+${markReady} $MAINPID" ];
      ExecStopPost = [ "+${rm} -f ${dir}/pid ${dir}/ready ${dir}/ready.tmp" ];
    };
  };
}
