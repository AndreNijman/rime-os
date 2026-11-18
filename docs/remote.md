# Rime Remote: `rime remote`

Pairing a phone with this machine, and taking it away again.

The phone talks to `rime-remoted`, a **per-user, unprivileged** service that is
a *client* of the agent runtime and not a part of it. Everything it forwards
reaches the runtime under the origin `claude-remote-control`, so a remote
request can edit a project, run tests and push, and cannot approve a root
operation or start a break-glass session, whichever device asks. That boundary is what the feature
rests on: what somebody can do with your phone is bounded by what the origin
`claude-remote-control` may do, whoever is holding the phone.

Reading and revoking need no privilege. **Pairing needs you**: `rime-remoted`
refuses `rime remote pair` for any caller that does not classify as a §7 local
origin, and an agent inside a managed session never does. An agent cannot pair
a device on your behalf.

Five verbs.

## `rime remote enable`

Rime Remote is **on by default** for every person's account (since
2026-09-23). The image enables `rime-remoted` for all user managers, so pairing
from Settings needs no setup step, and the unit's `ConditionUser=!@system`
keeps it off system accounts (the login screen's `greetd` user, and a lingering
root). The service pulls in the agent runtime through
`Wants=rime-agentd.service`. It uses Rime's relay,
`wss://apex-relay.andrenijman.com`, so a paired phone reaches the machine from <!-- rime-rename: keep -->
any network. The relay carries encrypted bytes it cannot read, and sees both
addresses and when and how much data moves (`rime remote status` prints this).
For LAN-only, override `ExecStart=` with `systemctl --user edit rime-remoted`
and drop `--relay`.

This verb turns the service back on for an account where it was switched off:
it runs `systemctl --user enable --now rime-remoted`, the same shape as
`rime agent enable`, and waits for the service to answer. When the service
does not come up, it prints the service's own last log line.
`systemctl --user disable --now rime-remoted` turns it off.

It does **not** open the firewall, and says so. LAN access needs

```
sudo rime firewall allow rime-remote
```

which is root, and is a separate decision because the relay path works without
it. A machine with the port shut is still reachable through a relay; a machine
with it open is reachable directly on the LAN, which is faster.

## `rime remote pair`

Shows a pairing code. The code is good for three minutes, pairs exactly **one**
device, and carries this machine's public key, so the device that scans it can
never be talked into trusting a different machine.

**The terminal prints no QR code.** No encoder is vendored in the CLI, and a
wrong QR is worse than none: a phone scans it, fails, and the person concludes
their camera is broken. The terminal prints the payload itself, an
`apex-remote:` URI whose body is base64url of the compact-JSON offer, and a <!-- rime-rename: keep -->
line pointing at Rime Settings → Pair a device, which draws the QR code. You
can also paste the line into Rime Remote on the phone.

If the service is not answering, `pair` starts it first (clearing a start limit
left by an earlier crash), so a unit that went down once does not block
pairing.

`--text` prints the payload and nothing else **on stdout**. The prose (how long
the code is good for, and the sentence about the public key) goes to stderr.
So

```
rime remote pair --text > offer.txt
```

leaves `offer.txt` holding exactly one line, the `apex-remote:` URI, with the <!-- rime-rename: keep -->
explanation still on the terminal. That split makes the verb scriptable
without a second output mode.

There is **no `rime remote pair --json`**. The payload is already a single
self-describing string. Wrapping it in an object would add a second shape to
keep in step with the phone, and no reader needs it.

## `rime remote devices`

Lists every device this machine has paired, and which of them are revoked:

```
ID                 NAME                 STATE     PATH     LAST SEEN
9f3c1a…            Pixel 8              paired    lan      4m ago
```

`--json` emits the stored `Device` records, which carry:

| field | meaning |
| --- | --- |
| `id` | short, stable, **derived from the public key**, so two records for one key are impossible |
| `name` | what the owner called it; display only, so a duplicate is untidy and not dangerous |
| `public_key` | the authenticating fact: base64url, no padding |
| `paired_ms` | Unix ms at which pairing completed |
| `last_seen_ms` | Unix ms of the last completed handshake, or absent |
| `revoked_ms` | Unix ms at which the owner revoked it, or absent |
| `requires_user_verification` | whether this machine requires the device to have re-authenticated its user |
| `last_path` | the transport the last completed handshake arrived over |
| `companion` | whether the device has a companion key, so it keeps working after the phone restarts (see below). The key itself is never printed |

The `STATE` column is **derived from `revoked_ms` alone**: `revoked` if it is
set, `paired` if it is not. There is no third state and no stored status field
that could disagree with the timestamp.

The listing reports `requires_user_verification` once, in a sentence at the
end, and not as a column. The desktop cannot see a fingerprint. What the flag
buys is that the device holds its key in a keystore that will not sign without
a biometric or device credential. That is the **device's own claim**, and a
tick in a table would read as a fact this machine had verified.

A device with a companion key is named in one more sentence at the end: it
"keeps working after a restart".

### Keeping the phone's features working after it restarts

A phone keeps its pairing key behind its lock screen, so after the phone
restarts nothing reaches this machine until somebody unlocks it: no battery,
no notifications, no ring. If you turn on **Keep working after the phone
restarts** in the app, the phone makes a second key that is not behind the
lock screen and registers it over its normal connection. This machine stores
it on the same device record.

A connection on that second key gets the phone's own features and nothing
else: its battery and network report, ring and Send to Rime acknowledgements,
its notifications, what it is playing, and its SMS conversations when this
machine asks for them. It cannot send an SMS, open a terminal, start
or steer an agent, use nex, lock the computer, change its media or volume,
touch the clipboard, or change where push notifications go (pushes carry what
your agents are doing, so only the unlocked phone may register for them). Anything else is refused with
`not_allowed`. Unlocking the phone brings back the full connection.

On the phone, the switch is in **Display**, next to Notifications and Media on
your computer, and it is off until you turn it on. Turning it on also asks,
once, to leave the app out of battery optimisation, because otherwise Android
can hold the connection off for long stretches while the phone is idle. You
can say no; the link still works, just later. With the switch on, locking the
app no longer ends the link: the phone drops to the second key and keeps
sending its battery, notifications and what is playing, and you can still ring
it and send it files. Replying to a notification, sending an SMS, controlling
what plays, agents, terminals and the computer's controls wait until you unlock. After a
restart the link comes back at the phone's first unlock, without opening the
app; the app's notification then says it is locked and what keeps reaching
the computer. Turning the switch off, or forgetting the computer, deletes the
key on the phone and tells the computer to forget it (straight away if it is
connected, otherwise the next time the phone connects normally).

Revoking the device ends both keys at once, including a connection the second
key already holds. Pairing the phone again starts without one, and a second
key the phone gave up (by pairing again, or by the switch) is never accepted
again. Turning the
switch off in the app removes the key here too.

## `rime remote revoke <device>`

Takes a device's access away. The argument is a device id, or its name when
that names exactly one device.

Revocation is immediate: `rime-remoted` drops a connection the device already
holds instead of only refusing the next one. It keeps the record, so the
listing can still show that the device was revoked and when.

## `rime remote status`

Shows whether the service is running, and how a device would reach it: the
machine name, the protocol revision, this machine's identity key, how many
devices are paired, the LAN addresses, the relay and rendezvous if one is
configured, and any pairing code still open.

It also lists **every connection open right now**: which device, which path it
came in on, how good that path is as a word, and the round trip in
milliseconds. The number is there for whoever wants it; the word tells you
whether your terminal is going to feel wrong. A relay path prints its
disclosure — what the relay can and cannot see — rather than leaving the reader
to assume.

`--json` is how Rime Settings reads the same measurement: `machine`,
`protocol`, `identity`, `paired`, `lan`, `relay`, `rendezvous`,
`offer_ms_left`, and `connections[]` with `device_id`, `device_name`, `path`,
`since_ms`, `rtt_ms`, `quality` and `disclosure`.

## When the service is not running

`rime remote pair` tries to start the service itself, as above. Every other
verb that talks to `rime-remoted` fails with the remedy instead of a bare
connection error: it names the socket it tried and tells you to run
`systemctl --user enable --now rime-remoted`, which is what `rime remote
enable` does for you.

## What a paired device may ask, and the one thing it may not

`rime-remoted` forwards and does not filter. Its `control()` refuses exactly
two verbs on the control channel, `attach` and `receive`, because both take the
connection over and would wedge it. It hands everything else to `rime-agentd`
under the origin `claude-remote-control`. So `rime-agentd`'s own per-verb rules
decide what a phone can do, in one place, and no denylist here can drift from
them. (The exceptions are verbs `rime-remoted` answers itself
and never forwards: its hello, the companion key, nex, the phone's answers
about its messages, and the five machine verbs in [The computer itself](#the-computer-itself-status-lock-and-media).)

A connection on a phone's **companion key** is the opposite: it forwards
nothing to `rime-agentd` at all, and may use only the phone's own features
([above](#keeping-the-phones-features-working-after-it-restarts);
`docs/remote-live-contract.md` §2.10 has the list).

Three verbs serve the phone's pickers, and the first two are cheap:

| verb | what it answers | cost |
| --- | --- | --- |
| `projects` | every project the runtime remembers: root, name, slug, detected toolchains, when it was last opened, and the capsule (§8) it is bound to | one small JSON record per project; no subprocess |
| `profiles` | one row per adapter: its program, whether that program is on **this machine's** `PATH`, whether the adapter needs a program named by the caller, and whether its profile (§5) is installed here | a `stat` per table entry and per `PATH` element |
| `worktrees` | per-worktree git status for every remembered project, or for one slug | runs `git worktree list`, a rev walk and `merge-tree --write-tree` in each project: seconds on a large machine |

A client builds a picker from the first two and asks the third only for the
project the user chose. That split is why they are separate verbs.

`projects` and `profiles` carry **counts and paths the machine already
publishes through `worktrees`, and no content**. A profile row never carries
the configured model, a plugin, a marketplace, an MCP server, a skill or a
credential. `rime agent profile doctor` has all of that, and none of it crosses
this wire. `secret` is a count of credential-class entries that exist, never a
name and never a value.

**`rime-agentd` refuses `decide` from any origin that is not local, by
design.** Every verb in the privilege vocabulary is a root capability
(`request.rs`'s `Verb::capability`, all eight), so §7's row for both columns
ends at a human at this machine. The check sits *before* the pending lookup, so
a paired device gets `permission_denied` whether or not the request id exists,
and cannot use the refusal to discover which requests are open on somebody's
computer. A phone can see pending requests, see what was decided, and
**revoke** a grant of either kind. Revoking carries no origin check, because
revocation only ever removes authority.

The one exception is a different verb with a different shape.
`renew_system_grant` accepts a non-local caller when the owner has written
`origin = remote_elevation_allowed`, and that opt-in costs a `second_factor`: a
WebAuthn assertion from a key the owner enrolled with `rime agent key add`,
over a nonce the daemon issued and bound to that one session, grant kind and
window. If remote approval of a root operation is ever built, it has to take
that shape; a relaxed origin check would not do.

## The computer itself: status, lock and media

Five verbs never reach `rime-agentd`: `rime-remoted` answers them itself, as
the user it runs as (`docs/remote-live-contract.md` §2.4). A phone reads the
battery, the power mode, whether the screen is locked and whether an update is
waiting (`machine_status`); locks the screen (`machine_lock`); and reads and
drives the player and the default output's volume (`media`, `media_control`,
`volume`). The app's **Computer** screen, reached from the agent list, is built
on them.

They need no grant because they are what the person could do at the keyboard,
and none of them can unlock anything: there is no unlock verb, and there will
not be one. A lock is reported as done only once logind's `LockedHint` confirms
it, within five seconds; otherwise the phone says it asked. Every other verb
has a two-second budget, and anything not read in that time comes back as
unknown rather than as a guess. Nothing here checks for
an update; `update` is rimed's cached state.

Test daemons use stub backends only. `run-device-suite.sh` refuses to
start `rime-remoted` without `RIME_REMOTED_MACHINE_STUB`, so a test run cannot
lock the developer's screen or pause their music.

## Send to Rime: from the phone onto the computer

Share text or files to Rime Remote from any Android app. The app opens a Send
screen that shows what was shared and asks which computer it goes to; nothing
is sent until the person taps Send. Text goes to the computer's clipboard
(`clipboard_write`), files to its Downloads folder (`save_file`), contract
§1.8. The Computer screen also has **Send this phone's clipboard**.

- A file is saved under its own name, never over another file: a clash becomes
  `name (2).ext`, and the phone shows the name the computer chose.
- Up to 256 MB per file and 8 KB of text. Longer text is refused, never cut.
- The app reads only what was shared to it, through the read grant that comes
  with the share. It asks for no storage permission, and it refuses `file://`
  links and its own content.
- A file stopped or cut off part way is never kept. If the connection drops
  after the last byte, the phone says the file may or may not be there.
- A managed agent session may not use either verb: only a person at the
  computer or on a paired phone may.

Test daemons write into a stub: `run-device-suite.sh` refuses to start
`rime-agentd` without `RIME_AGENTD_DESKTOP_STUB`, so a test run cannot replace
the developer's clipboard or write into their Downloads.

## `rime remote ring <device>`: find your phone

```
$ rime remote ring "Pixel 8"
rime: Pixel 8 is ringing.
```

The phone plays its alarm sound until someone taps **Stop** on the
notification, or for 30 seconds. The argument is a device id, or a name that
fits exactly one device, as for `revoke`. Only the computer's own user can
ring a phone. A paired phone cannot ring anything.

- If the app is connected, the ring goes straight to the phone, and `rime`
  reports that it is ringing only after the phone confirms it.
- If the app is not connected, the ring goes through the phone's push service
  (see below). `rime` then says it asked, because nothing confirms it. A push
  that arrives more than a minute late does not ring.
- If it cannot ring the phone, `rime` says why and exits 1. Three causes: an
  older app, notifications switched off, or no push service and no connection.

Because the phone plays the ring as an alarm, it rings in silent and vibrate
mode and through Do Not Disturb, unless Do Not Disturb is set to total silence
(measured on Android 16). The alarm volume controls how loud it is, and at zero
it makes no sound.

### The phone's battery

While the app is connected, it reports the phone's battery level and whether
it is charging. It sends a new report when it connects, when charging starts or
stops, when the level changes by 5 points, and at least every 10 minutes.
`rime remote devices` shows the last report under the table:

```
Pixel 8: battery 62%, charging, reported 3m ago
```

The phone also reports how it is connected (Wi-Fi, mobile data, Ethernet or
none, with its signal bars when it knows them) and whether its Do Not Disturb
is on. The line shows whatever was reported:

```
Pixel 8: battery 62%, charging, Wi-Fi signal 3/4, Do Not Disturb, reported 3m ago
```

With `--json`, a paired device that has reported carries a `phone` field
(`battery`, `charging`, `network` with `type` and `signal`, `dnd`, `can`,
`reported_ms`). Revoking the device deletes it.

## `rime remote focus on|off`: Do Not Disturb on both

Do Not Disturb can follow you between the phone and the computer, the way
Focus does between an iPhone and a Mac. It is off until you turn it on, and you
turn it on separately on each side:

- **On the phone:** in Rime Remote, open **Notifications on your computer**
  and turn on **Do Not Disturb follows your computer**. It needs the same
  notification access as notification mirroring, and no other permission.
- **On the computer:** in Rime Settings, open Paired devices and turn on
  **Do Not Disturb follows your phone**.

With both on:

- When you turn Do Not Disturb on or off on the phone, the computer follows.
- When you toggle it in the Shell, the Shell runs `rime remote focus on` or
  `off`, and every connected phone with Focus sync on follows.

```
$ rime remote focus on
rime: told 1 phone to turn Do Not Disturb on.
```

It sets a state rather than flipping one. A phone that is already there stays
as it is, and nothing bounces back and forth. When no phone is connected with
Focus sync on, `rime` says so and still exits 0. A phone that was not connected
does not catch up later: the next change on either side sets both. Only the
computer's own user can run it. A paired phone has no command for this
computer's Do Not Disturb. It can only report its own, and the Shell follows
that report only when you have turned that on.

On the phone, a Do Not Disturb the computer turns on is a mode of its own,
and `off` from the computer ends only that one (measured on Android 16). If
you turned Do Not Disturb on at the phone, the computer cannot turn it off,
and the phone stays as it is.

Turning "Do Not Disturb follows your computer" off on the phone, or locking
the app, ends a Do Not Disturb a computer turned on, and leaves one you turned
on yourself. If you take the app's notification access away first, it cannot:
end it from the Quick Settings "Modes" tile, where it is listed as "Do Not
Disturb (Rime Remote)".

## `rime remote send <device>`: files and text to your phone

```
$ rime remote send "Pixel 8" holiday.jpg notes.pdf
rime: Pixel 8 has holiday.jpg and is taking it now.
rime: Pixel 8 has notes.pdf and is taking it now.
$ rime remote send "Pixel 8" --clipboard
rime: Pixel 8 has the text and is taking it now.
$ echo "door code 4417" | rime remote send "Pixel 8" --text -
```

The phone takes what you send without asking. Text goes onto its clipboard,
and a notification says so without showing the text. A file goes into its
Downloads folder, and a notification opens it. A file whose name is already
taken there gets a numbered name: nothing is replaced. The phone refuses a
file it has no room for and leaves 64 MiB free. Files need Android 10 or
later; on Android 9 the phone refuses a file and says so in a notification,
and text still works.

Because the phone does not ask, **only you can send**: `rime-remoted` refuses
a `send` that does not come from a terminal or Rime Settings, so an agent
cannot send anything to your phone. `--clipboard` reads this computer's
clipboard with `wl-paste`, and only as text. At most 8,192 bytes of text and
256 MiB per file can be sent at once.

- If the app is connected, the phone gets the offer straight away and takes it
  over the same connection.
- If it is not, what you send waits on this computer for 15 minutes, and the
  phone is asked through its push service to come and take it. Opening the
  app also takes it. After 15 minutes, or if you revoke the phone, it is gone.
- The phone has to have told this computer that it can receive (an app with
  notifications allowed). Until then, `send` is refused and says why.

## Your phone's notifications on the computer

Rime Remote can show your phone's notifications on the computers it is paired
with, the way a Mac shows an iPhone's. In the app, open the settings on the
computers screen and choose **Notifications on your computer**:

1. **Allow notification access.** Android asks for it in Settings. On Android
   13 and later, an app installed from a file is "restricted" at first: open
   App info from the same screen, tap the menu in the corner, choose
   **Allow restricted settings**, then allow access.
2. **Turn on the apps** whose notifications you want on the computer. None are
   on until you turn them on.

From then on, while the app is unlocked and connected, a notification from one
of those apps appears on the computer as "Pixel 8 · Signal", with its title and
text. When it updates on the phone it updates on the computer. Dismiss it on
either side and it goes from both: dismissed on one computer, it leaves the
phone and every other computer too. One that times out on the computer stays
on the phone.

**Reply from the computer.** Turn on **Reply from your computer** on the same
screen, and a message you can answer from the phone's own notification gets a
reply field on the computer.
What you type there is sent from the phone by the app that posted the
notification, as you, exactly as if you had used the reply field on the phone.
The notification closes on the computer and stays on the phone.
If the phone is not connected, or replying has been turned off on it since,
the computer shows **Reply not sent** with what you typed.

- Only the app's name, the title and the text cross, plus whether it can be
  replied to. Pictures and other actions do not, and the computer keeps
  nothing once it is closed.
- Never shown: Rime Remote's own notifications, a work profile's, ones an app
  marks as private to the phone or hidden on the lock screen, ongoing ones
  (downloads, navigation, a call in progress) and media controls.
- Nothing waits for a computer that is not connected, and connecting does not
  replay what you missed.
- A locked computer does not show them: the Shell's notifications stay behind
  the lock screen.
- A phone can show 10 at once and then one every 3 seconds; anything faster is
  refused, so a noisy app cannot flood the desktop. Revoking the phone closes
  every notification it has open.

## `rime remote messages`: your phone's SMS on the computer

Rime Remote can show your phone's SMS conversations on the computer and send
an SMS from the phone's number, the way Messages on a Mac does with an
iPhone. In the app, open **Messages on your computer** in the computers
screen's settings, tap **Allow SMS**, and turn it on. It is off until you do.
On Android 13 and later, an app installed from a file may be refused SMS
without a prompt: the screen says so, and **Allow restricted settings** in
App info lets you allow it. **Names from your contacts** is a separate,
optional permission; without it the computer shows numbers.

```
$ rime remote messages
ID       WITH                       WHEN      UNREAD  LAST
42       +15555215556               2m ago    1       see you there
(from Pixel 8. `rime remote messages --thread ID` shows one.)
$ rime remote messages --thread 42
$ echo "on my way" | rime remote messages send --to +15555215556
rime: Pixel 8 sent the message.
$ rime remote messages send --thread 42 < reply.txt
```

- The computer asks the phone every time and keeps nothing. With the app
  locked, reading still works if the phone keeps working after a restart
  (above); sending needs the app unlocked.
- **Only you can use it.** `rime-remoted` answers `messages` only to a
  terminal in your login session or Rime Settings, so an agent, a scheduled
  job or any other program of yours can neither read nor send. The text of a
  send comes from standard input, never the command line, where other
  programs could see it.
- One number per send, at most 2,000 characters. A group conversation and
  picture messages are not shown and cannot be sent to.
- `--json` prints the conversations or messages for the Shell or a script,
  and `{"error":…}` with exit status 1 when it fails.

## `rime remote calls`: your phone's calls on the computer

A call ringing on your phone can show on the computer, with **Answer** and
**Decline**, the way a Mac shows an iPhone's calls. From the computer you can
also hang up, call a number from the phone, and see recent calls. There is no
sound on the computer: talk on the phone, or on a headset paired with it. In
the app, open **Calls on your computer** in the computers screen's settings,
tap **Allow calls**, and turn it on. It is off until you do. As for SMS, an
app installed from a file may need **Allow restricted settings** in App info
before Android lets it read the call log.

```
$ rime remote calls
Pixel 8: a call is ringing with +15555215556.
WHEN      TYPE      WITH                               LENGTH
2m ago    missed    +15555215556                       -
1h ago    outgoing  Pat (+15555215556)                 4m 12s
$ rime remote calls answer
rime: Pixel 8 answered the call.
$ rime remote calls hangup
$ rime remote calls dial +15555215556
```

- While a call rings, a notification shows who it is from, if the phone
  knows. Press **Answer** or **Decline** there, or use `rime remote calls
  answer` and `decline`. It closes when the call stops ringing.
- Answering, declining and hanging up work while the app is locked, if the
  phone keeps working after a restart (above), as they do on the phone's lock
  screen. Calling a number needs the app unlocked. The phone does not dial an
  emergency number for the computer.
- Each part has its own Android permission (see calls, call log, answer
  calls, make calls); the screen shows what is not allowed, and the computer
  says so if you ask for it.
- **Only you can use it.** As for messages, `rime-remoted` answers `calls` only
  to a terminal in your login session or Rime Settings. The number you dial is
  on the command line; a number is not a secret the way a message is.
- The computer keeps nothing: the history is read from the phone each time,
  and the call it shows goes when the phone disconnects.
- `--json` prints `{"device":…,"current":…,"calls":[…]}` for the Shell or a
  script, and `{"error":…}` with exit status 1 when it fails.

## `rime remote photos`: your phone's photos on the computer

Your phone's newest photos and screenshots can be seen on the computer, and
any one saved to it whole, the way Phone Link shows an Android phone's
photos. In the app, tap **Display** on the computers screen, open **Photos on
your computer**, tap **Allow photos**, and turn it on. It is off until you
do. Videos are not shown.

```
$ rime remote photos
ID         WHEN      SIZE      PIXELS      NAME
1000123    2m ago    2.4 MB    4000x3000   PXL_20261118_101500.jpg
1000119    1h ago    310 KB    1080x2400   Screenshot_20261118-091200.png
(from Pixel 8. `rime remote photos save ID` saves one here.)
$ rime remote photos save 1000123
rime: saved /home/you/Pictures/Rime Remote/PXL_20261118_101500.jpg (2.4 MB) from Pixel 8.
$ rime remote photos save 1000119 --to ~/Desktop
```

- A photo is saved into **Rime Remote** in your Pictures folder, or the folder
  you give with `--to`, under the phone's name for it. A file already there is
  never replaced: the new one is named `… (2).jpg`. A photo is at most 64 MiB.
- If you let Rime Remote see only some photos (Android 14 and later offer
  **Select photos**), only those are listed, and the computer says so.
- The list needs the app unlocked on the phone; a phone that keeps working
  after a restart (above) still shows nothing while the app is locked.
- The small previews are kept under your runtime folder
  (`$XDG_RUNTIME_DIR/rime-remote/photos`), readable by you alone, with the
  camera's details and location taken out. They go when the phone
  disconnects, when you turn the feature off, or when you revoke the phone.
  A saved photo is the phone's file as Android gives it to the app: from
  Android 10 on, Android takes the location out of it first.
- **Only you can use it.** As for messages, `rime-remoted` answers `photos`
  only to a terminal in your login session or Rime Settings, so an agent or a
  scheduled job can neither see nor save your photos.
- `--json` prints `{"device":…,"partial":…,"photos":[…]}` with each preview's
  path in `thumb`, for the Shell or a script; `save --json` prints the saved
  file's `path`. Both print `{"error":…}` with exit status 1 when they fail.
  `--before MS` (a photo's `date_ms`) gets the next page.

## What your phone is playing, on the computer

The song or podcast playing on your phone can show in the Shell's player, next
to the computer's own players, the way a Mac shows what an iPhone is playing.
In the app, open **Media on your computer** in the computers screen's settings
(it needs the same notification access as above), turn it on, and turn on each
app whose playback may show. None are on until you turn them on.

The computer shows "Pixel 8 · Spotify" with the title, artist, album and how
far in it is, and it follows the phone: pause, skip or seek there and it
changes here. When the phone stops playing, disconnects or is revoked, it goes.

- **Control from your computer** (on that screen, under the media switch; on
  by default once media is on) lets the Shell's player play, pause, skip and
  seek what the phone is playing, as far as the app on the phone allows: a
  button the app does not offer stays dimmed. Turn it off and the computer
  only shows. The phone acts only while it is unlocked, and only on what it
  last showed you.
- The media keys (and CTRL+SUPER+Space, Left and Right) go to the computer's
  own players first. They reach the phone only when nothing on the computer
  is a player at all, so a paused browser tab still keeps them on the
  computer.
- No artwork, links or queue cross: only the app's name, title, artist, album,
  length, position and whether it is playing.
- Nothing is sent while the app is locked or for an app you have not turned on.


## Notifications when the app is closed

An agent that needs you at two in the morning has to be able to reach a phone
whose screen is off and whose app Android killed hours ago. Until P1-058 it
could not: the app derived alerts from a poll loop inside itself, so the app
had to be running for an alert to exist.

Two paths now raise the same alerts. The poll loop still does it while the app
is open. When the app is closed, `rime-remoted` watches the runtime itself and
posts an **encrypted 51-byte envelope** to a push server, which a UnifiedPush
distributor on the phone turns into a notification.

### nex approvals wake the phone too

A nex approval on a machine (Rime's agent harness asking before it runs
something) sends the same kind of alert: **"nex needs a decision ·
<machine>"**. The alert holds no command text; the nex screen shows that once
the app unlocks. A tap opens nex on that machine and connects to it first if
the app had stopped. A machine that no longer offers nex opens the Agent Center
instead.

While a phone holds a push registration, the watcher asks nexd what waits, at
four-second intervals, as a device named `rime-remote-push`. One alert goes out
per approval it has not seen. nexd's first answer raises nothing, and an
approval you already heard about stays quiet when nexd comes back after a
restart.

### What you have to do: one thing

**Install a UnifiedPush distributor on the phone.** [ntfy][ntfy] is the usual
choice, is free, and needs no account: install it, and Rime Remote finds it on
the next connection to a machine.

Nothing else. In particular:

* **nothing to stand up, deploy or configure on the desktop.** `rime-remoted`
  already runs, and the watcher is part of it.
* **no account anywhere**: not with Google, not with Anthropic, not with ntfy.
  `ntfy.sh` accepts anonymous subscriptions.
* **no Firebase, no Google Play services, and no Google dependency** in the
  image or in the app. The APK asks for no permission to do this: not
  `FOREGROUND_SERVICE`, not `WAKE_LOCK`, not
  `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`.
* **no `relay/` deployment is involved.** The relay is a rendezvous both ends
  dial into at the same time, and push exists for the case where one end is
  absent.

If you run your own push server, point the distributor at it. The desktop
treats the endpoint as opaque and accepts a private-network address. It refuses
only `http://`, because the endpoint URL *is* the capability to notify the
phone.

**With more than one distributor installed, Rime Remote uses the first by
package name and does not ask.** The app has no chooser. Every distributor sees
the same thing (the timing of an alert and nothing else), so the choice matters
little. If it is the wrong one for you, uninstall the distributor you do not
want and reconnect.

**Without a distributor installed, nothing breaks and nothing changes**: the
app behaves as it did before, with alerts while it is open and none while it is
closed.

### What the push server can see

The desktop seals the envelope with ChaCha20-Poly1305 under a key the phone
generated and handed to one machine inside the already-authenticated Noise
channel. The push server and the distributor never have that key.

The plaintext is 22 bytes and holds **five fixed-width integers**: a kind, an
adapter code, a session number, that session's start time and a sequence
number. It has no string field, so a future change cannot put a prompt, a path,
a command line or a file name into it without changing the format and every
committed test vector.

The operator of a push server learns that an endpoint received something, when,
and that it was 51 bytes. The size is identical for every kind, so the length
does not disclose which kind it was. The operator does not learn the machine,
the project, the adapter, or what the agent was doing. The request carries no
`User-Agent`, no `Topic` and nothing naming the machine.

The phone renders the notification *text* from fixed strings plus the machine
name you chose at pairing and the adapter name. It never uses the session's
`detail`, which is where Rime records command lines, paths, grep patterns and
task descriptions.

### What stops a hostile app faking one

The app exports its receiver, because a distributor is a separate app and must
be able to broadcast to it. The manifest does not protect it. The app acts on a
message only if it **decrypts** under the key one paired machine holds, and
only if its sequence number is higher than the last one seen. The app drops
anything else silently: no notification, no state change, no answer.

Revoking a device on the desktop drops its push registration as well as its
pairing, so a revoked phone stops being woken even though push needs no
connection. Forgetting a machine on the phone unregisters with the distributor,
so the push server stops holding a subscription nobody reads.

[ntfy]: https://ntfy.sh
