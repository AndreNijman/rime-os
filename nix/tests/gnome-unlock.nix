# gnome-unlock: rime-gnome-unlock's own tests (nix/pkgs/rime-gnome-unlock/tests; docs/nixos/desktops.md §12.4), run
# by the package's installCheck in the build sandbox: a private dbus-daemon, a fake logind, pam_wrapper + pam_matrix,
# and GNOME's own libgdm client.
#
# Mutants (passthru.mutants, built by hand; each must FAIL):
#   any-peer             the private server admits a connection from any uid
#   pam-user-unchecked   a PAM success counts even when PAM_USER changed during the conversation
#   remote-allowed       a remote (ssh) session may be reauthenticated
#   other-user-channel   a caller may open a channel naming another user
#   channel-cap          a uid may hold any number of channels (no oldest-first eviction)
#   no-idle-timeout      a connected channel with no conversation lives for ever
#   session-removed-ignored   channels outlive logind's SessionRemoved for their session
#   stored-before-start  a conversation whose worker could not start is kept (blocking the service)
#   socket-world-connectable  the private socket is 0666 (any local user reaches the handshake)
#   sync-lookup          OpenReauthenticationChannel's bus/logind lookups block the main loop (Version waits)
#   no-pam-tty           PAM runs without PAM_TTY
#   unbounded-pam-step   a PAM stack that stops answering is never killed
#   worker-eof-passes    a PAM worker that dies without a result counts as a success
#   fd-watchdog-off      the daemon never notices it is running out of file descriptors
{ pkgs, self }:

let
  pkg = pkgs.rime.rime-gnome-unlock;
  mutant = name: from: to:
    pkg.overrideAttrs (o: {
      pname = "rime-gnome-unlock-mutant-${name}";
      postPatch = (o.postPatch or "") + ''
        substituteInPlace rime_gnome_unlock.py --replace-fail ${pkgs.lib.escapeShellArg from} ${pkgs.lib.escapeShellArg to}
      '';
    });
in
{
  gnome-unlock = pkg // {
    mutants = {
      any-peer = mutant "any-peer"
        "lambda _o, _s, creds: peer_allowed(creds, self.uid)" "lambda _o, _s, creds: True";
      pam-user-unchecked = mutant "pam-user-unchecked"
        "ok = code == PAM_SUCCESS and final_user == self.channel.user and" "ok = code == PAM_SUCCESS and";
      remote-allowed = mutant "remote-allowed"
        "if session.get(\"remote\") is not False:" "if False:";
      other-user-channel = mutant "other-user-channel"
        "if username != owner:" "if False:";
      channel-cap = mutant "channel-cap"
        "MAX_CHANNELS_PER_UID = 2" "MAX_CHANNELS_PER_UID = 1000";
      no-idle-timeout = mutant "no-idle-timeout"
        "self._idle = GLib.timeout_add(int(IDLE_TIMEOUT_S * 1000), self._idle_timeout)" "self._idle = 0";
      session-removed-ignored = mutant "session-removed-ignored"
        "channel.close()  # its session is gone" "pass  # its session is gone";
      stored-before-start = mutant "stored-before-start"
        "conversation = Conversation(self, service)\n"
        "conversation = Conversation(self, service)\n            self.conversations[service] = conversation\n";
      socket-world-connectable = mutant "socket-world-connectable"
        "os.chmod(path, 0o600)" "os.chmod(path, 0o666)";
      sync-lookup = mutant "sync-lookup"
        "self._lookups.submit(self._lookup, sender, username, invocation)" "self._lookup(sender, username, invocation)";
      no-pam-tty = mutant "no-pam-tty"
        "channel = Channel(self, session[\"id\"], uid, owner, pam_tty(session))"
        "channel = Channel(self, session[\"id\"], uid, owner, \"\")";
      unbounded-pam-step = mutant "unbounded-pam-step"
        "deadline = time.monotonic() + PAM_STEP_TIMEOUT_S" "deadline = time.monotonic() + 3600";
      worker-eof-passes = mutant "worker-eof-passes"
        "raise Cancelled(\"the PAM worker ended without a result\")"
        "return {\"result\": PAM_SUCCESS, \"user\": self.channel.user}";
      fd-watchdog-off = mutant "fd-watchdog-off"
        "return len(os.listdir(\"/proc/self/fd\")) > limit * 3 // 4" "return False";
    };
  };
}
