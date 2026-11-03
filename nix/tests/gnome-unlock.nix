# gnome-unlock: rime-gnome-unlock's own tests (nix/pkgs/rime-gnome-unlock/tests; docs/nixos/desktops.md §12.4), run
# by the package's installCheck in the build sandbox: a private dbus-daemon, a fake logind, pam_wrapper + pam_matrix,
# and GNOME's own libgdm client.
#
# Mutants (passthru.mutants, built by hand; each must FAIL):
#   any-peer             the private server admits a connection from any uid
#   pam-user-unchecked   a PAM success counts even when PAM_USER changed during the conversation
#   remote-allowed       a remote (ssh) session may be reauthenticated
#   other-user-channel   a caller may open a channel naming another user
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
    };
  };
}
