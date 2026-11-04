# gnome-keyring: a property read must never abort the daemon that holds the user's unlocked login keyring.
#
# The crash (gnome-keyring 50.0 at the nixpkgs pin, GLib 2.88; docs/nixos/open-items.md): the secret service keeps
# one "client" per bus name and creates it from a message filter, which only schedules the creation as a
# G_PRIORITY_HIGH idle so that it runs "before the actual method call". Now and then it runs after: the call finds
# no client, `gkd_secret_service_get_pkcs11_session` returns NULL (assertion 'client' failed), the collection's
# property getter returns NULL without an error, and GDBus aborts the process
# (`invoke_get_property_in_idle_cb: assertion failed: (error != NULL)`). Measured with a plain
# `busctl --user get-property org.freedesktop.secrets /org/freedesktop/secrets/collection/login
# org.freedesktop.Secret.Collection Locked` from a fresh connection each time, against the unpatched daemon on a
# private bus: it died at read 306 (`/var/lab/rime-nix/artifacts/ppd-keyring/keyring/`). Any program on the
# session bus can do that read (libsecret's proxies read collection properties when they start), and the daemon it
# kills is the one the login password unlocked: until the next login every app asks again or fails.
#
# The patch (gnome-keyring-client-race.patch):
#   - the client is created where it is needed (the PKCS#11 session lookup and publishing a session), so the order
#     of the two idles no longer matters; the filter's own idle then finds it and does nothing;
#   - the object lookup behind the collection and item property getters sets a D-Bus error when it has no
#     session, so a getter that fails answers the caller with an error instead of aborting the daemon.
# A carried patch, nothing sent upstream.
{ config, lib, pkgs, ... }:

{
  config = lib.mkIf config.services.gnome.gnome-keyring.enable {
    nixpkgs.overlays = [
      (_final: prev: {
        gnome-keyring = prev.gnome-keyring.overrideAttrs (o: {
          patches = (o.patches or [ ]) ++ [ ./gnome-keyring-client-race.patch ];
          passthru = (o.passthru or { }) // {
            rimeClientRacePatch = true;
            unpatched = prev.gnome-keyring;
          };
        });
      })
    ];
    assertions = [{
      assertion = pkgs.gnome-keyring.passthru.rimeClientRacePatch or false;
      message = "rime: gnome-keyring must carry nix/modules/gnome-keyring-client-race.patch (a property read aborts the unpatched daemon)";
    }];
  };
}
