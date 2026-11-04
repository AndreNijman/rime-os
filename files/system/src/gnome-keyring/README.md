# gnome-keyring: one fix on top of Fedora's build

`gnome-keyring-property-read-no-client.patch` is applied to Fedora's own
`gnome-keyring` SRPM (pinned by NVR and sha256 in `Containerfile.base`,
stage `gnome-keyring-builder`) by `build-rpms.sh`. The packages
(`gnome-keyring`, `gnome-keyring-pam`) are marked `.rime1`, and the base
build refuses to finish with any other gnome-keyring.
`tests/test-rime-keyring-property-read.sh` keeps those pieces in place, and
PR validation races a real daemon with it (stock, then rebuilt).

## The crash

A program that reads a property of the login collection can kill the
keyring daemon:

```
** (gnome-keyring-daemon): CRITICAL **: gkd_secret_service_get_pkcs11_session: assertion 'client' failed
** (gnome-keyring-daemon): CRITICAL **: secret_objects_lookup_gck_object_for_path: assertion 'session' failed
GLib-GIO:ERROR:../gio/gdbusconnection.c:4765:invoke_get_property_in_idle_cb: assertion failed: (error != NULL)
Aborted (core dumped)
```

The daemon that dies is the `--login` one: the one the login password
unlocked. After it dies the next app that asks for a secret starts a fresh
daemon with the login keyring locked, and the user is asked for their
password again (or the app just gets no secrets).

## Why

1. The daemon keeps a record (a "client", with its PKCS#11 session) per D-Bus
   caller. A message filter sees every incoming call and creates the record,
   but only by scheduling `ensure_client_for_sender` as a high-priority idle.
2. GDBus answers a property read from its own idle. Now and then that idle
   runs before the one that creates the client.
3. `gkd_secret_service_get_pkcs11_session()` then finds no client and returns
   NULL; `secret_objects_lookup_gck_object_for_path()` returns NULL through
   `g_return_val_if_fail`, without setting an error.
4. A property getter that returns NULL without an error is a programming
   error to GDBus, and it aborts the process.

`busctl --user get-property org.freedesktop.secrets
/org/freedesktop/secrets/collection/login org.freedesktop.Secret.Collection
Locked` is enough, run often enough: every run is a new connection, so a new
client.

## The fix

- `gkd_secret_service_get_pkcs11_session()` and
  `gkd_secret_service_publish_dispatch()` create the caller's client if the
  idle has not run yet (`gkd_secret_service_ensure_client`, which does
  nothing when it already exists).
- `secret_objects_lookup_gck_object_for_path()` sets a D-Bus error when it
  still has no session, so a getter can never return NULL silently.

## Proof (Fedora 45 container, as a normal user, private bus)

`tests/test-rime-keyring-property-read.sh` with `RIME_KEYRING_RACE=1`:
10 daemons, each unlocked with a throwaway password, up to 5,000 reads of
`collection/login`'s `Locked` each.

| gnome-keyring | daemons that died | reads |
|---|---|---|
| 51.1-1.fc45 (Fedora) | 10 of 10 (after reads 27 to 1,186) | 4,185 |
| 51.1-1.rime1.fc45 (this patch) | 0 of 10, every read answered | 50,000 |

An earlier run of the same reads against Fedora's build: 9 of 10 died.

## When Fedora ships a new gnome-keyring

The base build stops: core's gnome-keyring is no longer `GNOME_KEYRING_NVR`.
Check whether the new build fixes the abort (run the race against it:
`RIME_KEYRING_RACE=1 RIME_KEYRING_RACE_EXPECT=die` as a normal user with
Fedora's package installed; the PR-validation step does exactly this).

- Fixed: delete the `gnome-keyring-builder` stage, the install that uses it
  (put back a plain `dnf5 -y install gnome-keyring-pam`), this directory and
  the test.
- Not fixed: set `GNOME_KEYRING_NVR` and `GNOME_KEYRING_SRPM_SHA256` to the
  new build (the sha256 of Koji's src.rpm at the URL in `build-rpms.sh`) and
  rebuild. `build-rpms.sh` stops if the patch no longer applies, or applies
  and the prepared source lacks the fix.
