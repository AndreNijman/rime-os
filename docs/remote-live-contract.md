# Remote live: the contract between agentd, rime-remoted, the CLI, the Shell and the phone

Working document for the `feat/remote-live` change set (2026-09-30). Every side
builds against THIS file; `android/core/src/test/resources/requests.json` and
`rimed/rime-agent-core/tests/android_requests_wire.rs` are where the phone's
bytes and the daemon's parser meet, so the JSON below is exact, not
illustrative.

Andre's asks: the phone is too slow to connect and to show a terminal ("always
connected"); see an agent's output without opening its terminal; name agents,
synced between phone and desktop, settable at start on the desktop; a reply
sent from the phone types but does not submit; opening a terminal on the phone
leaves the desktop terminal shrunk to the phone's size.

## 1. agentd protocol additions (no PROTOCOL_VERSION bump; old daemons answer `bad_request`)

### 1.1 `hello` advertises features

`Response::Hello` gains `#[serde(default)] features: Vec<String>`:

```json
{"reply":"hello","version":11,"agents":["claude"],"default_agent":"claude",
 "features":["input_submit","rename","peek","session_name","session_title","size_restore",
             "clipboard_write","save_file"]}
```

A client uses a feature only when its string is present. An absent `features`
key means none (an old daemon).

### 1.2 `input` can submit

```json
{"cmd":"input","id":3,"data":"yes please","submit":true}
```

`#[serde(default)] submit: bool`. With `submit:true` the daemon writes `data`,
waits **80 ms** (a constant, `SUBMIT_GAP`), then writes `"\r"` as a SEPARATE
write. Reason, measured 2026-09-30 against Claude Code 2.1.283 in a pty: a
~250-character burst ending in CR is taken as a paste and the CR becomes a
newline in the prompt (not submitted); the same text with the CR written 50 ms
or 250 ms later submits. Short bursts happen to submit, which is why it looked
intermittent. `data` should not end in CR/LF when `submit` is set (the phone's
`Reply.bytes` trims them); the daemon does not strip anything. Reply: `{"reply":"ok"}`.
`rime agent input --submit` uses this flag.

Old-daemon fallback (phone, when `input_submit` is absent): send
`input(data)`, sleep **100 ms** on the client, then `input("\r")`.

### 1.3 Session names

`SessionInfo` gains two fields, both `#[serde(default)]`, both ALWAYS
serialized (no `skip_serializing_if`), so `"name": null` means "this daemon
supports names, none is set" and a missing key means "old daemon":

- `name: Option<String>` — set only by a person: `rime agent run --name`,
  `rename` from the CLI, the Shell or the phone. Never from agent output.
- `title: Option<String>` — the agent's own terminal title (OSC 0 / OSC 2),
  captured by the output scanner: leading spinner/status glyphs (braille
  U+2800–U+28FF, `✳ ✻ ✶ ✢ · * ●` and similar) and surrounding whitespace
  stripped, control characters removed, at most 80 characters, `None` when
  empty. Display-only, agent-controlled, never trusted for anything.

Display order everywhere (Shell, CLI list, phone): `name` → `title` → today's
label (`<agent> · <project_name or cwd basename>`).

Validation (one function in rime-agent-core, used by daemon and CLI): trim;
empty → `None` (clears); at most **64** characters (chars, not bytes); no
control characters (`char::is_control`); refused, not sanitised, with a
sentence saying why.

### 1.4 `run` takes a name

`RunRequest` gains `#[serde(default)] name: Option<String>`. CLI:
`rime agent run --name <NAME>` / `-n <NAME>`, defaulting to the environment
variable `RIME_AGENT_NAME` when the flag is absent. Forwarded by
`forward_argv` (`--host`). NOT added to `settings_a_daemon_could_drop`
(dropping a label widens nothing). `aw <worktree>` defaults the name to the
worktree name when neither is given.

### 1.5 `rename`

```json
{"cmd":"rename","id":3,"name":"auth refactor"}
{"cmd":"rename","id":3,"name":null}
```

Reply: the updated session, `{"reply":"session", ...SessionInfo fields...}`.
Callers: allowed for exactly the callers `privilege::refuse_input` allows (a
person's shell, the Shell, a paired phone); refused for a managed session and
an unclassifiable caller — only a person names a session. Live sessions only
(`no_such_session` otherwise). The record is written (`registry::write_record`).
CLI: `rime agent rename <ID> <NAME>` and `rime agent rename <ID> --clear`.

### 1.6 `peek`: the tail of a session's output, without attaching

```json
{"cmd":"peek","id":3,"bytes":8192}
```

`#[serde(default)] bytes: Option<usize>`; the daemon caps it at **8192**
(`PEEK_MAX`), default 8192. Reads the in-memory scrollback ring (NOT the disk
transcript, which stops at 32 MiB). No resize, no attach, no change to
`attached`. Reply:

```json
{"reply":"peek","id":3,"data":"<base64 of raw PTY bytes>","cols":120,"rows":40,"state":"working"}
```

Base64 (standard alphabet, padded) because raw PTY bytes JSON-escape to up to
6× and a control frame is at most 65514 bytes: 8192 → ≤ 10924 base64 chars,
far inside it. The tail may start mid-escape-sequence or mid-UTF-8; the phone
feeds it to its terminal emulator at `cols`×`rows` and shows the bottom lines.
Allowed for the callers `refuse_input` allows (not for a managed session:
agents do not read each other's screens).

### 1.7 The phone's terminal no longer shrinks the desktop's

Today `handle_attach` resizes the PTY to whoever attached last, and nothing
puts it back. New rule (`size_restore` feature):

- agentd remembers the last size set by a LOCAL client (the `rime agent run` /
  `attach` terminal: its attach and its `resize` requests), per session.
- A remote-origin attach (origin class `RemoteControl`) still resizes to the
  phone while the phone is attached, but when the LAST remote attacher
  detaches, the PTY is resized back to the remembered local size (if the
  session has one).
- Stretch, only if attribution is clean: input arriving from a local client
  while a phone is attached resizes back to the local size ("latest typer
  wins", tmux's `window-size latest`).

Attribute by `privilege::origin` class, never by socket identity:
rime-remoted opens a fresh agentd connection per control frame, so a phone's
`resize` does not arrive on its attach socket.

### 1.8 From the phone to the computer: `clipboard_write` and `save_file`

Two verbs that carry something the person is holding on the phone onto the
computer itself, not into a session. `hello.features` lists `clipboard_write`
and `save_file`. Both are refused to the callers `refuse_clipboard` refuses: a
managed session (an agent that could set the clipboard could change what a
person pastes into a terminal, and one that could drop files in Downloads
could leave things for a person to open), and a connection that cannot be
classified. A local client and a paired phone may use them.

```json
{"cmd":"clipboard_write","text":"…"}
→ {"reply":"clipboard_written","bytes":12}
```

- Sets the computer's clipboard to `text`, verbatim, as
  `text/plain;charset=utf-8` (`wl-copy`, the text on stdin, never in argv).
- `text` is at most 8192 bytes of UTF-8 (the same cap as `clipboard`), and
  must not be empty. Larger is `bad_request` with the limit named.
- No graphical session: `{"reply":"error","kind":"internal","message":…}` in
  the same words `clipboard` uses. One 5 s budget for the whole write.

```json
{"cmd":"save_file","name":"IMG_2041.jpg","len":2097152}
→ {"reply":"saving","len":2097152}
   … exactly len bytes …
→ {"reply":"saved","name":"IMG_2041.jpg","path":"/home/u/Downloads/IMG_2041.jpg"}
```

- A takeover verb like `receive`: it travels as a `Frame.Open` payload, and
  rime-remoted refuses it on channel zero. Everything refusable (the caller,
  the name, `len`, the free space) is refused BEFORE `saving`, so no byte
  crosses the link to be thrown away.
- The folder is the user's Downloads folder (`XDG_DOWNLOAD_DIR` from
  `~/.config/user-dirs.dirs`, else `~/Downloads`), created if missing.
- `name` is a name, never a path: everything up to the last `/` or `\` is
  dropped, leading dots and spaces are dropped (no hidden files), and it is
  cut to 200 bytes keeping the extension. A name with a control character is
  refused. A name that is empty after that becomes `shared file`.
- A file of that name already there is never replaced: the copy becomes
  `IMG_2041 (2).jpg`, and so on. `name` in the reply is what it became.
- The bytes go to a hidden temporary file in the same folder, created with
  `O_EXCL|O_NOFOLLOW` and mode 0644, which is renamed into place only once
  every byte has arrived. An upload that stops half-way leaves nothing.
- `len` is at most 256 MiB and at least 1, and the folder's file system must
  have `len` plus 64 MiB free. Nothing arriving for 30 s ends the upload, and
  so does the whole upload taking longer than 300 s plus one second per
  128 KiB.

**Tests never touch the real desktop.** With `RIME_AGENTD_DESKTOP_STUB=<dir>`
rime-agentd writes the clipboard to `<dir>/clipboard` and saves files into
`<dir>/Downloads`, and never runs `wl-copy` or reads `user-dirs.dirs`. Unit
tests always use a stub. `run-device-suite.sh` always sets it and refuses to
start rime-agentd without it.

## 2. rime-remoted additions

### 2.1 `remote_hello`, answered by rime-remoted itself (never forwarded)

```json
{"cmd":"remote_hello"}
→ {"reply":"remote","version":1,"features":["nodelay","control_worker","mux_attach","liveness"],"lan":["192.168.1.232:7717"]}
```

An old rime-remoted forwards it to agentd, which answers
`{"reply":"error","kind":"bad_request",...}`: the phone reads that as "no
features". `lan` is the current list of addresses this machine listens on
(same filter as pairing), so the phone can refresh its stored list.

### 2.2 Behaviour

- `TCP_NODELAY` on every accepted socket; one `write` per frame (header and
  body in one buffer).
- `control_worker`: control requests are handled on a per-connection worker
  thread, in order (FIFO replies preserved: the phone matches replies by order),
  so `Data`/`Open`/`Close`/`Ping`/`Pong` keep flowing while a slow control
  request runs. `Open` (attach/receive) setup also off the frame loop.
- `mux_attach`: with `control_worker`, a terminal opened on the SAME connection
  as control is safe; the phone may then stop dialling a connection per
  terminal.
- `liveness`: the session ping (15 s) now expects pongs; 3 missed → close the
  connection (the phone reconnects).
- Relay waiting socket: a read deadline (no frame for 75 s despite 30 s pings →
  re-dial); re-dial backoff capped at **10 s** (was 60); a `409` meaning "a host
  is already waiting" is not counted as a failure.

### 2.3 `nex`: the agent harness, through the same pairing

nex's daemon (`nexd`) listens on a local socket only
(`$XDG_RUNTIME_DIR/nex/nexd.sock`, or `rime-remoted --nexd <path>`). A paired
phone reaches it through rime-remoted, so there is no second pairing, listener
or revocation. `remote_hello.features` gains `"nex"` while nexd answers (asked
on every hello).

Every connection rime-remoted opens to nexd first sends
`system.declare_origin {"kind":"device","id":<the authenticated device id>}`
and reads its reply. If the declaration is refused, times out or is malformed,
the phone's request is never sent. A declared device only loses authority in
nexd. It cannot call local-only methods (-32002), it can work only in a
workspace that already has a session, and it can approve only the scopes
listed in `remote_approve`. Every write is recorded with actor
`device:<id>`. The phone cannot declare an origin itself (`bad_request`).

```json
{"cmd":"nex","request":{"method":"sessions.list","params":{}}}
→ {"reply":"nex","response":{"jsonrpc":"2.0","id":1,"result":[...]}}
```

- nexd's own errors stay inside `response.error`. `{"reply":"error","kind":"unavailable"}`
  means nexd could not be reached; `forbidden` means the declaration was refused.
- `params.request_id` is nexd's idempotency key. rime-remoted passes it through
  and never adds one, so a phone that re-sends a write after a lost reply gets
  nexd's recorded answer and the write is not done twice.
- An answer bigger than one control frame comes back as `kind: "too_large"`.
  To get it, send the same line as a channel `Open`: the reply is
  `{"reply":"streaming"}`, the response line follows as `Data`, and then the
  channel closes.
- `Open` with `{"cmd":"nex_events","after_gseq":N}` (or `"tail":true`) is nex's
  global event feed. It carries one response line (the backlog after `N`, at
  most 10,000 events, and `last_gseq`), then one `event` notification per line,
  each with `params.gseq`, for as long as the channel is open. The response
  always arrives before the notifications. A backlog over the cap continues in
  notifications without waiting for a new event. The channel is read-only:
  `Data` from the phone is dropped.
- Each request has one 30 s deadline, from connecting through the declaration
  to the reply, because the control worker handles requests in order. Running
  out of time is `kind: "unavailable"` with "nex did not answer within 30 s".
- `after_gseq` that is not a whole number is refused (`bad_request`), not read
  as 0.
- A one-shot `nex` channel ends by itself after its answer. The phone does not
  need to close it.
- Push kind **9** (`NexApproval`) means "nex has an approval you have not been
  told about". Session -1, adapter 0, start 0: the envelope names nothing. While
  a phone holds a push registration, each poll of the push loop opens its own
  connection, declares `{"kind":"device","id":"rime-remote-push"}`, calls
  `tools.pending` and closes, within 5 s. Kind 9 goes out when an `approval_id`
  appears that the previous answer lacked. The first answer raises nothing.
  While nexd is silent the loop keeps the last answer, so an approval it already
  knew about raises nothing when nexd answers again. The phone shows "nex needs
  a decision · <machine>". A tap opens nex on that machine and, with no machine
  connected, dials it first, waits up to 60 s for a hello that lists `nex`, and
  stays on the Agent Center if the hello lacks it.

### 2.4 The machine itself: status, lock, media and volume

Five verbs, answered by rime-remoted itself and never forwarded. They work on
the machine the phone is paired with, as the user rime-remoted runs as, and
need no grant: they are what the person could do at the keyboard, and none of
them can unlock anything. `remote_hello.features` lists `machine_status`,
`machine_lock` and `media` on every build that has them; there is no live
probe in the hello.

Each verb has one 2 s budget for everything it asks the machine (the control
worker answers requests in order, so a hung `wpctl` must not stall the link).
Backends that do not answer in time give `null` fields, not an error.
`machine_lock` alone has 5 s, because its confirmation passes through
hypridle, the Shell and the compositor before logind records it.

```json
{"cmd":"machine_status"}
→ {"reply":"machine_status",
   "battery":{"percent":81,"state":"charging"},
   "on_ac":true,
   "power_mode":"balanced",
   "locked":false,
   "update":"available"}
```

- `battery` is `null` on a machine without one. `state` is the kernel's
  status, lower case: `charging`, `discharging`, `full`, `not charging` or
  `unknown`. Read from `/sys/class/power_supply`.
- `on_ac` and `power_mode` (rimed's `Power.OnAcPower` and `Power.Tier`) and
  `update` (rimed's `Updates.State`: `up-to-date`, `available`, `downloading`,
  `installing`, `deferred`, `restart`, `failed` or `unknown`) are `null` when
  rimed does not answer. Cached state only: nothing here checks for an update.
- `locked` is logind's `LockedHint` for the user's graphical session, which the
  Shell sets once the compositor confirms the lock. `null` when there is no
  graphical session or it cannot be read.

```json
{"cmd":"machine_lock"}
→ {"reply":"machine_lock","locked":true}
```

- Runs `loginctl lock-session` on the user's graphical session, then reads
  `LockedHint` back until it says locked or the budget runs out. `locked` is
  what was read, so `false` means "asked, and the screen did not confirm".
  That is not a failure: some lockers never set `LockedHint` (COSMIC's does
  not), and the phone says it asked rather than that it failed.
- No graphical session: `{"reply":"error","kind":"unavailable","message":"there
  is no screen to lock on this computer"}`.
- There is no unlock verb, and there will not be one.

```json
{"cmd":"media"}
→ {"reply":"media",
   "player":{"name":"spotify","status":"playing","title":"…","artist":"…"},
   "volume":{"percent":40,"muted":false}}
{"cmd":"media_control","action":"play_pause"}   (or "next", "previous")
{"cmd":"volume","percent":55}                    (0 to 100)
{"cmd":"volume","muted":true}
→ the same "media" reply, read after the change
```

- `player` is the player `playerctl` picks (the one playing, else the last
  active), `null` when none is running. `status` is `playing`, `paused` or
  `stopped`. `title` and `artist` are capped at 200 characters each and may be
  empty.
- `volume` is the default output (`wpctl … @DEFAULT_AUDIO_SINK@`), `null` when
  it cannot be read. `percent` above 100 is refused (`bad_request`), as is an
  unknown `action` or a `volume` line with neither field.
- An action that fails (no player, `wpctl` missing) is
  `{"reply":"error","kind":"unavailable","message":…}` in the tool's own words.

**Tests never touch the real desktop.** Every session of one user shares one
D-Bus user bus and one graphical session, so a test daemon running as the
developer would pause their music and lock their screen. With
`RIME_REMOTED_MACHINE_STUB=<dir>` rime-remoted uses no real backend at all: it
answers from `<dir>/state.json` (the same shape as the replies above) and
appends every action to `<dir>/actions.jsonl`. `run-device-suite.sh` always
sets it.

### 2.5 The phone itself: its report, and ring

`remote_hello.features` gains `"device_events"` and `"phone_report"`. A phone
uses neither with a machine whose hello lacks them.

```json
{"cmd":"phone_report","battery":62,"charging":true,"can":["ring"]}
→ {"reply":"ok"}
```

- Answered by rime-remoted, under the handshake's device id. `battery` is a
  whole number from 0 to 100, `charging` a boolean, `can` at most 8 words; any
  of them may be left out. A value outside those is `bad_request` and nothing
  is stored. Of `can`, only `"ring"` is kept. The phone leaves `"ring"` out
  while its notifications or its "Ring my phone" channel are switched off.
  It reports when it connects, and again when charging starts or stops, the
  level moves 5 points, or 10 minutes pass.
- A phone acknowledges a ring only once its notification is posted.
- The last report per device is in `phones.json` beside `devices.json` (0600,
  written whole). `rime remote devices` lists it; the control reply carries it
  as `phones`, keyed by device id; `--json` puts it on each paired device as
  `phone`. Revoke deletes it.
- `Open` with `{"cmd":"device_events"}` answers `{"reply":"events"}` and stays
  open. It is read-only: `Data` from the phone is dropped. Opening it again
  replaces the device's previous channel, which closes. It carries one JSON
  object per line. The only event today is
  `{"event":"ring","id":N,"at":<unix seconds>,"machine":"<name>"}`.
- The phone answers a ring with `{"cmd":"ring_ack","id":N}` → `{"reply":"ok"}`.

**Ring** is a control-socket request (`rime remote ring <device>`, by id or
name, as `revoke`), so only the machine's own user can send one. A paired phone
has no verb that rings anything: `ring` and `device_events` sent as control
lines are forwarded to rime-agentd, which does not know them.

1. With a `device_events` channel open, the event is written there. An ack
   within 4 s answers `{"reply":"rang","device":<name>,"path":"link"}`.
2. Otherwise, or with no ack, push kind **10** (`Ring`) goes to that one device,
   never to every registered phone. Session -1, adapter 0, `started` = the
   ring's `at`. Sent only to a phone whose last report has `"ring"` in `can`,
   because a build without kind 10 drops it unseen. The push service accepting
   it answers `{"reply":"rang",…,"path":"push"}`: asked, not confirmed.
3. Otherwise `{"reply":"error","message":…}` says why: the phone never said it
   can ring, it has no push registration, the push service refused, or the
   device is revoked.

The phone drops a push ring whose `started` is more than 60 s old, so a push
delivered late does not ring. A ring on the channel is live and is not checked
against the phone's clock. A push ring whose `started` equals the `at` of a
ring the phone already rang from that machine is the same ring (its ack was
lost) and is dropped too.

### 2.6 From the computer to the phone: `send`, `take_offer`

The other direction of §1.8: a file, or some text for the phone's clipboard,
from somebody at this computer onto their phone. `remote_hello.features` gains
`"take_offer"`. A phone puts `"receive"` in its report's `can` when it will
take what is sent (notifications allowed, so the person is told what arrived);
of `can`, `"ring"` and `"receive"` are kept.

**Send** is a control-socket request (`rime remote send`), and like `pair` it is
refused unless the caller is a human at this machine (a local terminal or the
Rime Shell, by `origin::observe_pid`). The phone takes what arrives without
asking, so the gate is here: an agent in a managed session could otherwise
put text on the clipboard a person pastes from, or leave files for them to
open, the reason §1.8 refuses `clipboard_write` and `save_file` to managed
sessions.

```json
{"cmd":"send","device":"Pixel 8","kind":"file","name":"report.pdf"}   + the open file, SCM_RIGHTS
{"cmd":"send","device":"Pixel 8","kind":"text","text":"…"}
→ {"reply":"sent","device":"Pixel 8","path":"link"|"push"|"waiting","id":7}
```

- A file travels as a descriptor passed with the request line (one
  `SCM_RIGHTS` descriptor, on the same `sendmsg` as the line's first byte),
  never as a path: rime-remoted has a private `/tmp`, and a descriptor is what
  the person could read when they asked. It must be a regular file of 1 byte
  to 256 MiB. `name` is reduced as §1.8 reduces it (and the phone reduces it
  again). `text` is 1 to 8192 bytes of UTF-8.
- What is sent waits in an outbox, in memory, for 15 minutes or until the phone
  says it has it. A restart of rime-remoted empties it.
- With a `device_events` channel open, `{"event":"offer","id":N,"at":<unix s>,
  "machine":…,"kind":"file"|"text","name":…,"len":L}` is written there (`name`
  is empty for text), and the phone answers `{"cmd":"offer_ack","id":N}` →
  `{"reply":"ok"}` once it has started taking it. An ack within 4 s is
  `"path":"link"`.
- Otherwise, to a phone whose report has `"receive"` in `can`: push kind **11**
  (`Sent`) to that one device, session -1, adapter 0, `started` = `at`, no
  text, and `"path":"push"`. A phone without push gets `"path":"waiting"`:
  it is offered the next time it opens `device_events` within the 15 minutes.
  A phone that never said `"receive"` is refused with a sentence, and so is a
  revoked one.
- Every offer still waiting is written to a device's `device_events` channel
  right after it opens, oldest first.

**Take.** `Open` with `{"cmd":"take_offer","id":N}` answers
`{"reply":"offer","kind":…,"name":…,"len":L}`, then exactly `L` bytes, then the
channel closes. An offer is taken only under the device id it was sent to; an
unknown, expired or other device's id is `{"reply":"error","code":"not_found"}`
and a close. Taking again (after a dropped link) starts from byte 0. Then
`{"cmd":"offer_done","id":N}` → `{"reply":"ok"}` drops it from the outbox.

The phone drops an offer whose `(machine, id, at)` it has already taken. Text
goes onto its clipboard, with a notification that says from where and not
what. A file is saved into Downloads through MediaStore, never replacing one
there, with a notification that opens it. A file larger than the phone's free
space less 64 MiB is not taken. On Android 9 (no MediaStore Downloads) a file
is refused with a notification, and finished with `offer_done`: the only
folder writable there without a storage permission is app storage other apps
can read.

**Tests never touch the real desktop or phone storage outside the app's.**
`rime remote send --clipboard` reads the clipboard with `wl-paste`; the device
suite never uses it, and sends text and files through its broker.

### 2.7 From the phone to the computer: its notifications

`remote_hello.features` gains `"phone_notifications"`. A phone sends neither
verb to a machine whose hello lacks it.

```json
{"cmd":"phone_notification","key":"9f2c…","app":"Signal","title":"Mia","text":"Lunch at 12?"}
{"cmd":"phone_notification_removed","key":"9f2c…"}
→ {"reply":"ok"}
```

- Answered by rime-remoted, under the handshake's device id. `"ok"` means
  accepted for showing, not shown: one worker thread posts to the desktop's
  `org.freedesktop.Notifications` (`busctl --user`, 3 s per call), behind a
  queue of 32. A post that finds it full answers
  `{"reply":"error","code":"busy"}`; a removal is `"ok"` either way.
- `key` is 1 to 64 characters of `[A-Za-z0-9_-]`: the phone's hash of
  Android's key (the first 16 bytes of SHA-256, as hex), never the key itself.
  A missing or other key, or a post with neither title nor text, is
  `bad_request`.
- The computer cleans what it shows: control and bidirectional-override
  characters go, app and title become one line, the text keeps at most 6 line
  breaks, and each is cut to 60, 120 and 1,000 characters (with "…"). `<` and
  `>` become `‹` and `›` everywhere and the body's `&` is escaped, so a
  message never reads as markup. It is shown with the app name
  `"<device name> · <app>"`, the icon `phone`, normal urgency, no actions, and
  the server's own timeout.
- A post with a key already shown replaces that notification in place;
  `phone_notification_removed` closes it, and a key not shown is `"ok"` and
  does nothing. The key-to-id map is in memory, at most 64 per phone, and is
  cleared when the notification server's bus owner changes (a Shell restart
  starts its ids again). Revoke closes everything the phone has open and
  forgets it.
- Posts are limited per phone to 10 at once, then one every 3 s; past that,
  `{"reply":"error","code":"rate_limited"}`. Removals are not limited.

**What the phone sends.** Nothing until the person grants notification access
and turns apps on one by one in "Notifications on your computer" (none are on
by default). Then only while the app is unlocked and a connection to a machine
offering the feature is open: nothing is queued for a machine that is not
connected, nothing is replayed when one connects, and nothing is written to
the phone's storage. Never sent: this app's own notifications, a work profile's
or another user's, `FLAG_LOCAL_ONLY`, `VISIBILITY_SECRET`, group summaries,
ongoing and foreground-service notifications, and media controls. What is
sent is the app's label, `EXTRA_TITLE_BIG` or `EXTRA_TITLE`, and
`EXTRA_BIG_TEXT` or `EXTRA_TEXT`, cut to the computer's caps. Per machine the
phone keeps only the newest state of each notification, sends nothing
identical to what is shown, sends a removal only for one it sent, and posts at
most 8 at once and one every 4 s, inside the computer's limit.

**Known limit.** `busctl` takes the title and text as arguments, so for the
few milliseconds of each call they are in that process's `/proc/<pid>/cmdline`,
readable by other processes that can see it. Moving the backend to a D-Bus
library closes it.

**The lock screen.** The Shell's notification toasts are a layer surface, which
`ext-session-lock` hides while the session is locked, and the lock screen
shows no notifications, so a phone's messages are not readable on a locked
computer.

**Tests never touch the real desktop's notifications.** With
`RIME_REMOTED_NOTIFY_STUB=<dir>`, rime-remoted appends each call to
`<dir>/notifications.jsonl` (`{"op":"notify",…}` / `{"op":"close","id":N}`)
instead of the bus; a test build always uses the stub. The device suite sets it
and its broker reads it back with `notify_log`. The bus path is proven against
a fake server on a private bus by `tests/test-remoted-notify-busctl.sh`.

## 3. The phone

- Connect: race the last-good path first, then LAN addresses and the relay in
  parallel (relay after a 300 ms head start for LAN), first completed Noise
  handshake wins, losers closed. Remember the last-good path per machine
  (through `MachineStore`/`AppStorage` only: `android/tools/no-second-write-path.sh`).
  Refresh stored LAN addresses from `remote_hello.lan`. Retry a relay `409`
  (guest: no desktop waiting) at 100, 250, 500, 1000, 2000 ms.
- One biometric prompt per launch (unlock the app and unwrap the identity in
  one auth), if the current code allows it without weakening the key rules.
- Always connected: a foreground service (`foregroundServiceType="specialUse"`,
  sideloaded so no Play declaration; update `ManifestTest`) holds the link
  while the app is unlocked; connect on unlock without waiting for a tap;
  reconnect on network change (`ConnectivityManager` callback) and on silence
  (no desktop ping for 40 s). Screen-off Doze may still drop the socket; the
  point is instant reconnect without re-auth.
- Terminals on the control connection when `remote_hello.features` has
  `mux_attach`; a new connection otherwise (today's behaviour). Replay
  default 64 KiB (was 256 KiB).
- Reply uses `input` + `submit:true` when `input_submit` is advertised, the
  100 ms two-request fallback otherwise.
- Live output: the session screen shows the bottom ~15 lines of a `peek`,
  refreshed every 1.5 s while visible; Agent Center rows show the last
  non-empty line for live sessions (peek only while the list is on screen).
- Names: display order as §1.3 everywhere in the app; rename from the session
  screen (`rename`). Names stay OUT of lock-screen notifications
  (`Notifications.kt` privacy rule is unchanged).
- Log one line per connect: `connected to <machine> via <lan|relay> in <N> ms`.

## 4. The Shell (rime-shell)

Display order as §1.3 in `SessionRow`, `RemoteSessionRow`, search and desktop
notifications; an inline rename in `SessionRow` through
`AgentService._act(["rime","agent","rename",id,name])` (the Agent Center
invariants forbid running anything directly).

## 5. `a -a codex`: "codex is not installed or not on PATH"

agentd resolves (`pty::resolve_program`, session.rs:154) and later executes an
adapter's program against ITS OWN `PATH`, which is whatever the user manager
had when the daemon started at boot: `/usr/local/sbin:/usr/local/bin:/usr/bin`
on the L16 (read from `/proc/<pid>/environ`, 2026-09-30). The login session
adds `~/.local/bin` to the manager's environment later, so a daemon started
before it never sees it, and `npm i -g @openai/codex` with the npm prefix
`~/.local` puts `codex` exactly there (`~/.local/bin/codex`). Fix in the unit
(`files/system/units/rime-agentd.service`):

    Environment=PATH=%h/.local/bin:%h/bin:%h/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/bin

user directories first, as a login shell has them (the manager's environment
on the L16 has `~/.local/bin` first). Assert it in whichever test already
reads that unit. `inherited_path()` then hands the same PATH to sessions, so a
tool the agent runs from `~/.local/bin` resolves too.
