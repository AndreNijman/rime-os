# Shared-machine recipes — guest and kiosk (roadmap P2-016)

Everything in this directory is **inert on a stock APEX**. No guest account is
created, no kiosk config is installed, no unit is enabled. That is deliberate
and it is the criterion:

> Kiosk/shared-device policy is possible without weakening ordinary desktop
> use.

A change that locked every desktop down in order to make kiosk possible would
have failed that, not met it. So the ordinary login path is untouched, and
`tests/test-apex-shared-machine.sh` asserts the untouched-ness directly — the
shipped `greetd-config.toml` has no auto-login stanza, and the suite fails if
one appears.

## What ships, and where it lands

| File | Installs to | Active? |
|---|---|---|
| `guest-accounts` | `/etc/apex/guest-accounts` | Yes, and **empty** |
| `greetd-kiosk.toml` | `/usr/share/apex/shared-machine/greetd-kiosk.toml` | No — a recipe |
| `sway-kiosk.conf` | `/usr/share/apex/shared-machine/sway-kiosk.conf` | No — a recipe |
| `../libexec/apex-guest-wipe` | `/usr/libexec/apex-guest-wipe` | Installed, refuses everything |
| `../units/apex-guest-wipe@.service` | `/usr/lib/systemd/system/` | Installed, not enabled |

## Guest sessions, and the thing everybody gets wrong

The usual recipe for a disposable guest is a home directory on tmpfs. On APEX
that is **not enough**, and the reason is specific to this system:

P0-002 moved credentials *out of the home directory* on purpose.
`apex-secretd` keeps them in `/var/lib/apex-secretd/users/<uid>/`, root-owned
and `0700`, so that a process running as the user cannot read them. That
directory is not in the home, is not on the tmpfs, and does not go away when
the session ends.

So a guest who stores a credential leaves it for the next guest, who logs in to
the same uid and inherits the whole namespace: the credentials, the per-project
grants and the standing approvals. The home evaporating hides that rather than
fixing it.

`apex-guest-wipe` is the part that makes the claim true. It clears three
things, and the middle one is the reason it exists:

1. the contents of the home directory (not the directory — that is a mount
   point or a `tmpfiles.d` line);
2. `/var/lib/apex-secretd/users/<uid>/`, the protected credential namespace;
3. `/var/lib/apex-greet/last-user`, but only when it names the guest.

It is **not** a boundary against the guest while the session is running. The
guest is a real account with a real uid and the ordinary confinement applies.
It is the guarantee that nothing of theirs is still here afterwards.

### Turning it on

Three steps, none of which this repo performs:

```sh
# 1. the account. NOT in wheel, and a home the wipe is allowed to empty.
sudo useradd -m -d /home/apex-guest -s /bin/bash apex-guest
sudo passwd -d apex-guest          # no password, if that is the intent

# 2. allow the wipe to act on it
echo apex-guest | sudo tee -a /etc/apex/guest-accounts

# 3. run the wipe when the guest's session ends
sudo systemctl enable apex-guest-wipe@apex-guest.service
```

The engine refuses every account that is not in that file. It also refuses uid
0, a home of `/`, `/home`, `/var/home`, `/root` or empty, an account that does
not exist, and an account that still has a logind session. Four fences, checked
in that order, before anything is removed — modelled on the four
`apex-disposable` puts on its recursive removal, for the same reason: a wrong
argument here is unrecoverable.

### What is not done yet

**Nothing fires the wipe when the guest logs out.** `systemctl enable` puts it
in `multi-user.target`, so it runs at boot and clears whatever the previous
session left behind — that is the safe deployment and it is what the enable
step above gives you.

A draft of the unit carried `After=user-%i.slice`, which looked like the
missing piece and was not. `%i` is the account *name*; logind names its slices
by **uid** — `user-1500.slice`, never `user-apex-guest.slice` — and a template
unit cannot derive one from the other. systemd treats an ordering against a
unit that does not exist as a silent no-op, so that line ordered nothing at all.
It is gone, and `tests/test-apex-shared-machine.sh` fails if it comes back.

Wiring the wipe to logind's session end properly — a `BindsTo=` + `ExecStop=`
pair against the real `user-<uid>.slice`, or a `pam_exec` line at
`session close` — needs a machine with a real second account to verify on and
is **untested**. What protects the live-session case meanwhile is the engine's
fourth fence: it asks logind who is logged in and refuses while the account
still has a session, so an early run is a refusal rather than a wiped desktop.
The engine itself is exercised end to end by
`tests/test-apex-shared-machine.sh`, fences included.

## Kiosk

`greetd-kiosk.toml` + `sway-kiosk.conf`. The important design note is in the
first of those and is worth repeating here because it is the thing a reader
will get wrong:

**greetd's `[initial_session]` auto-login runs once per boot, not once per
logout.** greetd(5), verified against the shipped greetd 0.10.3, says it "will
only be executed during the first run of greetd since boot ... checked through
the presence of the runfile". A kiosk built on it drops to the greeter the
first time its app exits and stays there. The kiosk session therefore goes in
`[default_session]`, which greetd restarts "whenever no session is running".

The kiosk boundary is made of **absent configuration**: `sway-kiosk.conf` does
not `include /etc/sway/config.d/*`, so there is no terminal binding, no
launcher, no exec binding and no workspace switching. Nothing is taken away
from anybody else's desktop to achieve it.

There is no authentication on the kiosk path — whoever can see the screen is
the kiosk user. That is what a kiosk is, and it is why the account must be
purpose-made, not in `wheel`, and not the machine owner's.

## Not enabled on the development laptop

None of this is enabled on the L16 this was written on, and no account was
created there. `getent passwd` on that machine lists exactly one account at or
above uid 1000. Everything asserted about guest and kiosk in the test suite is
asserted against fixture trees, not against a live second user.
