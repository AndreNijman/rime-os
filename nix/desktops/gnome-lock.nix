# GNOME's screen lock under rime-greet (docs/nixos/desktops.md §12.4). Merged into the gnome entry's config in
# ./default.nix, so it exists only while rime.desktops.gnome.enable is on.
#
# GNOME Shell 50 builds its lock screen only when org.gnome.DisplayManager answers, and checks the password through
# it (libgdm reauthentication). GDM itself cannot run beside greetd: its daemon always starts a greeter on every
# graphical seat. rime-gnome-unlock (nix/pkgs/rime-gnome-unlock) is the one part of GDM the lock uses: a caller's
# own local graphical session, PAM authentication only, then logind UnlockSession.
{ config, lib, pkgs, ... }:

let
  unlock = pkgs.rime.rime-gnome-unlock;
in
{
  assertions = [
    {
      # Both would own org.gnome.DisplayManager; GDM is off by D8.
      assertion = !config.services.displayManager.gdm.enable;
      message = "rime.desktops.gnome's screen lock (rime-gnome-unlock) and GDM both own org.gnome.DisplayManager.";
    }
  ];

  services.dbus.packages = [ unlock ];

  # What GDM's gdm-password does for a reauthentication (auth substack login), and nothing else: the daemon calls
  # only pam_authenticate, and the other stacks refuse in case anything ever asks.
  security.pam.services.rime-gnome-unlock.text = ''
    auth substack login
    account required pam_deny.so
    password required pam_deny.so
    session required pam_deny.so
  '';

  systemd.services.rime-gnome-unlock = {
    description = "GNOME screen lock password check (org.gnome.DisplayManager)";
    documentation = [ "file://${unlock}/lib/rime-gnome-unlock/rime_gnome_unlock.py" ];
    # Up before the first GNOME login; the bus would start it on the first call anyway.
    wantedBy = [ "multi-user.target" ];
    before = [ "display-manager.service" ];
    # GNOME Shell asks whether it can lock ONCE, when it starts (loginManager.js canLock: a synchronous Version read):
    # a daemon that is down then, or stuck in a start limit, leaves that session without a lock until it logs out.
    # So it always comes back, however often it fails, and the bus can start it at any time.
    unitConfig.StartLimitIntervalSec = 0;
    serviceConfig = {
      Type = "dbus";
      BusName = "org.gnome.DisplayManager";
      ExecStart = lib.getExe unlock;
      Restart = "always";
      RestartSec = "1s";
      # The private reauthentication sockets: the directory lets nobody list it (GDM's own is 0711 too); each socket
      # is 0600 and owned by its session's user (CAP_CHOWN).
      RuntimeDirectory = "rime-gnome-unlock";
      RuntimeDirectoryMode = "0711";
      # Fixed, so the daemon's own check (exit for a restart past 3/4 of it) has a known bound.
      LimitNOFILE = 1024;
      # Root, because it runs PAM for other users. What the login stack needs of root, and nothing else:
      #   CAP_DAC_READ_SEARCH   pam_unix reading /etc/shadow; modules looking into /run/user/<uid> or a home
      #   CAP_SETUID/SETGID     pam_gnome_keyring (and similar) acting as the user in a child process
      #   CAP_AUDIT_WRITE       libpam's audit records (AF_NETLINK)
      #   CAP_CHOWN             giving each private socket to its session's user
      CapabilityBoundingSet = [ "CAP_CHOWN" "CAP_DAC_READ_SEARCH" "CAP_SETUID" "CAP_SETGID" "CAP_AUDIT_WRITE" ];
      # No ProtectHome: modules in the login stack may read the user's home or /run/user; no MemoryDenyWriteExecute:
      # ctypes' PAM conversation callback needs an executable trampoline.
      NoNewPrivileges = true;
      PrivateTmp = true;
      PrivateDevices = true;
      ProtectSystem = "full";
      ProtectProc = "invisible";
      ProtectKernelTunables = true;
      ProtectKernelModules = true;
      ProtectKernelLogs = true;
      ProtectControlGroups = true;
      ProtectClock = true;
      ProtectHostname = true;
      RestrictNamespaces = true;
      RestrictRealtime = true;
      RestrictSUIDSGID = true;
      LockPersonality = true;
      SystemCallArchitectures = "native";
      # A call outside the set kills the process (SIGSYS): a PAM worker, that one attempt; the daemon, a restart.
      SystemCallFilter = [ "@system-service" ];
      RestrictAddressFamilies = [ "AF_UNIX" "AF_NETLINK" ];
    };
  };

  programs.dconf.profiles.user.databases = [
    {
      # "Switch User" asks the display manager for a second greeter (Gdm.goto_login_session, CreateTransientDisplay),
      # which greetd cannot give; the button would only fail. Rime's own sessions have no user switching either.
      settings."org/gnome/desktop/lockdown".disable-user-switching = true;
    }
  ];
}
