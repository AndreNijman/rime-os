# gnome-keyring: one fix on top of Fedora's build

`build-rpms.sh` applies `gnome-keyring-property-read-no-client.patch` to
Fedora's own `gnome-keyring` SRPM in `Containerfile.base`'s
`gnome-keyring-builder` stage. It follows Fedora: it rebuilds whichever
gnome-keyring core ships, and nothing of ours pins a version or checksum, so
a Fedora gnome-keyring update never stops a release. The packages
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
| 50.0-1.fc44 (Fedora 44) | 10 of 10 | 13,853 |
| 50.0-1.rime1.fc44 (this patch, rebased by `build-rpms.sh`) | 0 of 10, every read answered | 50,000 |

An earlier run of the same reads against Fedora's build: 9 of 10 died.

## How the build follows Fedora

`build-rpms.sh OUTDIR`, run as root in a container FROM core:

1. Reads core's installed `gnome-keyring` (NVR and source package).
2. Fetches Fedora's SRPM for exactly that build and accepts it only when
   `rpmkeys --checksig`, against a private keyring holding ONLY Fedora's
   release key for that Fedora version, prints `digests signatures OK` (an
   unsigned rpm prints `digests OK` and exits 0). It tries Fedora's source
   repositories (`dnf5 download --source`), then Koji's signed copy, then
   Koji's unsigned build with the signature header from Koji's sigcache
   spliced in (the same splice as `rime-pkg`). Koji keeps every build, so
   an older core still builds after the repositories move on.
3. Prepares Fedora's source (`rpmbuild -bp`, Fedora's own patches applied)
   and decides:

| decision | when | what happens |
|---|---|---|
| `patched` | the patch applies (`git apply --check`) | patch added as `Patch9001`, release `.rime1`, the prepared source checked for both halves of the fix, rebuilt. A version other than `PROVEN` in `build-rpms.sh` is a GitHub `::warning::` ("rebased") |
| `upstream` | the patch reverse-applies, or both halves are there in other words (the session lookup calls `gkd_secret_service_ensure_client`, and the object lookup no longer has `g_return_val_if_fail (session, ...)`) | Fedora's `gnome-keyring-pam` for that build is fetched and verified the same way; the image keeps Fedora's build; a `::warning::` says this directory can be dropped |
| `fail` | neither | the build stops. The only case that needs a person |

4. Writes `OUTDIR/result` (`MODE`, `NVR`, `BUILT`, `PROVEN`). The base build
   installs from it, keeps it as `/usr/share/rime/gnome-keyring-fix`, and
   build-image puts it in the run summary.

`build-rpms.sh --classify SOURCE_ROOT` prints the decision for an unpacked
source tree without touching it; the test uses it on trees made from the
patch's own hunks.

Checked for real (logs in the PR): Fedora 45 from the repositories, from
Koji's signed copy and from the sigcache splice (all `patched`, `.rime1`);
Fedora 44's 50.0-1.fc44 (different version and key: `patched`, rebased,
`.rime1`, warning); the patch's post-image in place of the patch
(`upstream`, Fedora's `gnome-keyring-pam` fetched and verified); a patch
with one wrong context line (`fail`); and an unsigned copy served where
the signed one should be (`digests SIGNATURES NOT OK`, refused).

## When the build warns or stops

- `rebased`: the patch applied to a new Fedora version and the markers are
  in the prepared source. Run the race on it (`RIME_KEYRING_RACE=1` as a
  normal user with the rebuilt packages installed; PR validation does this
  against Fedora 45's current build) and set `PROVEN` to it.
- `upstream`: confirm with the race (`RIME_KEYRING_RACE_EXPECT=survive
  RIME_KEYRING_RACE_MARKER=0` on Fedora's build), then delete the
  `gnome-keyring-builder` stage, the install that uses it (put back a plain
  `dnf5 -y install gnome-keyring-pam`), this directory and the test.
- `fail`: rebase the patch onto the new source, or confirm Fedora fixed it
  another way (then extend the `upstream` markers in `build-rpms.sh`).
