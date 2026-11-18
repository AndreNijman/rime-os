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
→ {"reply":"remote","version":1,"features":["nodelay","control_worker","mux_attach","liveness"],"lan":["192.168.1.232:7717"],"scope":"full"}
```

`scope` says which of the device's keys opened this session: `full` or
`companion` (§2.10). A rime-remoted without the companion link sends no
`scope`; read its absence as `full`.

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
  whole number from 0 to 100, `charging` a boolean, `can` at most 16 words; any
  of them may be left out. A value outside those is `bad_request` and nothing
  is stored. Of `can`, only `"ring"` is kept. (Later sections add `network`,
  `dnd` and more `can` words: §2.6, §2.7, §2.9, §2.11, §2.12, §2.13.) The phone leaves `"ring"` out
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
  An ack counts only from the device the ring (or offer, `offer_ack`) was
  written to, while the computer still waits for it; any other ack, another
  device's included, gets the same `ok` and changes nothing.

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
of `can`, `"ring"`, `"receive"`, `"dismiss"` and `"reply"` (§2.7) are kept.

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
  `org.freedesktop.Notifications` over the session bus (a D-Bus library on
  its own thread, one 3 s deadline per call that connecting shares, so
  nothing reaches the server after the worker gave up on it; the text travels in the message, never on a
  command line), behind a queue of 32. A post that finds it full answers
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
  `"<device name> · <app>"`, the icon `phone`, normal urgency, no actions
  (but see replies below), and the server's own timeout.
- A post with a key already shown replaces that notification in place;
  `phone_notification_removed` closes it, and a key not shown is `"ok"` and
  does nothing. The key-to-id map is in memory, at most 64 per phone, and is
  cleared when the notification server's bus owner changes (a Shell restart
  starts its ids again). Revoke closes everything the phone has open and
  forgets it.
- Posts are limited per phone to 10 at once, then one every 3 s; past that,
  `{"reply":"error","code":"rate_limited"}`. Removals are not limited.

**Dismissed on the computer, gone from the phone.** rime-remoted listens for
`NotificationClosed` from the current owner of
`org.freedesktop.Notifications` only (a signal any other client sends is not
believed). Any close of an id it posted forgets that id. Only reason 2, "the
person dismissed it", goes back to the phone, on its `device_events` channel
(§2.5), and only if the phone's latest `phone_report` lists `"dismiss"` in
`can`:

```json
{"event":"notification_dismissed","key":"9f2c…"}
```

Expired (1), closed by a call (3, which includes rime-remoted's own closes)
and anything else are not sent. The phone acts only on a key it sent to that
machine and that machine still shows, and only while unlocked; it cancels the
notification as a swipe would. Android then reports the removal, so every
other computer it was sent to closes it with `phone_notification_removed`; the
machine that dismissed it is not sent that removal. A phone that does not list
`"dismiss"` (an older app) is never sent the event and keeps the notification.
The phone lists it while it holds notification access, from its next
connection.

**Replied to on the computer, sent from the phone.** `remote_hello.features`
also gains `"phone_notification_reply"`. A post may carry `"reply":"<label>"`,
the title of the notification's own free-form reply action on the phone
(cleaned like a title, cut to 40 characters; empty means none). The phone sends
it only to a machine whose hello has the feature, and only while the person has
turned on "Reply from your computer". rime-remoted asks the server's
`GetCapabilities` once per bus owner; if it lists `"inline-reply"`, a post with
a label gets the actions `["inline-reply", "<label>"]` and the hint
`x-kde-reply-placeholder-text`, and otherwise it is shown as before, without
one. When the server says `NotificationReplied(id, text)` for an id posted for
a phone, rime-remoted sends on that phone's `device_events` channel

```json
{"event":"notification_reply","key":"9f2c…","text":"On my way"}
```

if the phone's latest `phone_report` lists `"reply"` in `can`. The text keeps
its line breaks, loses other control and bidirectional-override characters,
and is cut to 2,000 characters; a reply that is empty after that is not sent.
The server closes the notification after a reply (reason 2); that close is not
a dismissal, so the phone keeps it, and this holds for any reply, sent or not.
A non-empty reply that is not sent, because the phone's report does not list
`"reply"` or the phone has no `device_events` channel open, makes rime-remoted
post its own notification here: app `Rime Remote`, summary `Reply not sent`,
a body that says why and ends `What you typed:` and the text. It belongs to no
phone, so closing it sends nothing, and it has no reply field. A reply written
into the channel counts as sent: the phone does not acknowledge it, so one
written just as the connection drops, or one the phone then fails to send, is
not reported here.
The phone acts only on a key it sent to that machine and that machine still
shows, and only while unlocked. It fills the reply action's `RemoteInput` and
sends that action's `PendingIntent`, as Android's own reply field would: the
app that posted the notification sends the message, as the person. Apps
usually post the notification again after a reply ("You: …"); the phone does
not send the first such update of that key within 5 s to the machine that
replied, so the reply does not pop up there again. Every other machine it
sent that key to gets `phone_notification_removed` for it, and that same first
update within 5 s is not sent to them either: answered on one machine, it is
not news on the others. The phone lists `"reply"`
while it holds notification access and the switch is on.

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

**The lock screen.** The Shell's notification toasts are a layer surface, which
`ext-session-lock` hides while the session is locked, and the lock screen
shows no notifications, so a phone's messages are not readable on a locked
computer.

**Tests never touch the real desktop's notifications.** With
`RIME_REMOTED_NOTIFY_STUB=<dir>`, rime-remoted appends each call to
`<dir>/notifications.jsonl` (`{"op":"notify",…}` / `{"op":"close","id":N}`)
instead of the bus, and reads each complete line `{"id":N,"reason":R}`
appended to `<dir>/closed.jsonl` as the server's `NotificationClosed`; a test
build always uses the stub. The device suite sets it; its broker reads the
calls back with `notify_log` and closes with `notify_closed`. The bus path is
proven against a fake server on a private bus by
`tests/test-remoted-notify-bus.sh` (post, replace, close, a dismissal, an
expiry, a close sent before the post's reply, a stranger's fake signal, and
replies, against a server that offers `"inline-reply"` and one that does not).
A stub line `{"id":N,"reply":"text"}` in `closed.jsonl` is read as
`NotificationReplied`; the stub records a post's `reply` label as given, and
the device suite's broker writes one with `notify_reply`.

### 2.8 From the phone to the computer: what it is playing

`remote_hello.features` gains `"phone_media"`. A phone sends `phone_media` to
no machine whose hello lacks it.

```json
{"cmd":"phone_media","playing":{"app":"Spotify","state":"playing","title":"Song","artist":"Band","album":"LP","duration_ms":215000,"position_ms":42000,"rate":1.0}}
{"cmd":"phone_media","playing":null}
→ {"reply":"ok"}
```

- Answered by rime-remoted, under the handshake's device id; never forwarded.
  `playing: null` means nothing the phone may show is playing.
- `state` is `playing`, `paused` or `stopped`; `title` must not be empty after
  cleaning. `duration_ms` and `position_ms` are whole milliseconds up to 7 days
  (absent: 0, "unknown"); a position past the duration is the duration.
  `rate` is 0 to 8 (absent: 1). Anything else is `bad_request`.
- Cleaned like a notification title (§2.7): one line, control and bidi
  characters removed, `<` `>` replaced, app 60 characters, title, artist and
  album 120.
- Each phone gets one MPRIS player on the session bus,
  `org.mpris.MediaPlayer2.rime_phone.d<hex>`, where `<hex>` is the device id's
  bytes in hex (so no two phones share a name). `Identity` is
  `"<device name> · <app>"` with the store's name, never one from the body.
  `Metadata` has `mpris:trackid` (new per track), `xesam:title`, and
  `xesam:artist` (one entry), `xesam:album` and `mpris:length` when known. No
  artwork.
- **Display only, unless the phone allows control.** Without `can` (below),
  `CanControl` is false and every method (`PlayPause`, `Next`, `Seek`, …)
  answers `NotSupported`. `playerctl` does not act on a player that cannot be
  controlled. rime-remoted's own `media` verb (§2.4) passes
  `--ignore-player=rime_phone`, which leaves out every phone by one name, so a
  phone never controls another phone through the computer.
- **Position.** The phone sends where playback was when it sent, and only when
  something changes (never a ticking stream). rime-remoted stamps the moment
  it read the line; `Position` is that plus the time since, times the rate,
  while playing, and stops at the duration. A report more than 1.5 s from where
  the last one says playback should be is a seek: `Seeked` is emitted, as it
  is for a new track.
- The latest report per phone wins: reports are kept, not queued, and applied
  at most every 200 ms per phone, so a burst becomes a few changes and never
  delays the link.
- **Lifetime.** The player belongs to the connection that last described it.
  It is removed when that connection ends, when the phone is revoked, and on
  `playing: null`. A reconnect that describes it before the old connection is
  noticed gone keeps it.

**What the phone sends.** Nothing until the person grants notification access
(the same listener reads media sessions) and turns on "Media on your computer"
and then each app whose playback may show (none by default). Then, while
unlocked and connected to a machine offering the feature: the newest state of
the app's active media session that is playing (or, if none is, the one that
paused most recently), on connect and on each change of state, track, speed or
position jump, at most every 500 ms (a change inside that wait is sent when it
ends, as the newest state); `playing: null` when nothing allowed is active or
the switch is turned off. A session that is buffering is sent as `playing` at
rate 0; a stopped one is not shown, so the phone never sends `stopped`. `app`
is the app's label, or its package name where Android does not let this app
read the label (an app with no launcher icon). Never sent: artwork, URIs,
queue, and anything from an app not turned on.

**Control from the computer** (`remote_hello.features` also gains
`"phone_media_control"`). The model is a Mac controlling an iPhone's Now
Playing.

```json
{"cmd":"phone_media","playing":{"app":"Spotify","state":"playing","title":"Song",…,"can":["play","pause","next","previous","seek"]}}
```

- `can` lists what the computer may do to this session, from the session's
  own `PlaybackState` actions: `play`, `pause`, `next`, `previous`, `seek`.
  Absent, `null` or `[]`: display only. Unknown words are ignored (a later
  phone may know more); anything but an array of at most 8 strings is
  `bad_request`. A phone sends `can` only to a machine whose hello has
  `phone_media_control`, and only while its "Control from your computer"
  switch is on. remote_hello is the same answer for every phone, so a `can`
  on the wire is itself the negotiation, as `reply` is for §2.7's labels.
- `CanControl` is true when `can` is not empty. `CanPlay`, `CanPause`,
  `CanGoNext`, `CanGoPrevious` and `CanSeek` follow its words, and each change
  is announced with `PropertiesChanged`. Allowing control does not make a new
  track (`mpris:trackid` stays).
- Each method becomes one line on the `device_events` channel of the
  phone that owns the player:
  `{"event":"media_control","action":"play"|"pause"|"play_pause"|"next"|"previous"|"seek","position_ms":N}`
  (`position_ms` only with `seek`; the channel is §2.5's). This is the computer→phone event; §2.4's
  `media_control` verb is the other direction (a phone controlling the
  computer's players) and is unrelated.
  - `Play` needs `play`, `Pause` needs `pause`, `Next` / `Previous` their own
    word. `PlayPause` needs `pause` while playing and `play` otherwise, and is
    sent as `play_pause` (the phone resolves it against its current state).
    `Stop` is `pause` while playing and does nothing otherwise.
  - `Seek(offset)` and `SetPosition(trackid, position)` need `seek`. Both cross
    as an absolute `position_ms`: a relative seek is resolved against
    `Position` at the moment of the call and clamped to 0..duration (0..7
    days when the duration is unknown). `SetPosition` with a `trackid` that is
    not the current track, a negative position or one past the duration is
    ignored, as MPRIS says.
  - A method whose word is missing answers `NotSupported`. A phone with no
    `device_events` channel open answers `Failed` ("the phone is not
    connected to this computer now"): never a success that went nowhere.
    `Raise`, `Quit`, `OpenUri`, rate, volume, shuffle and loop stay refused.
- **The media keys** run `/usr/libexec/rime-media-key play-pause|next|previous`
  (Hyprland, labwc and the Shell's CTRL+SUPER binds). It asks
  `playerctl --ignore-player=rime_phone` when that lists any player, and plain
  `playerctl` only when the computer has no player at all, so a phone gets the
  key only then. (Not `… || playerctl X`: measured with playerctl 2.4.1, a
  computer player whose `CanControl` is false makes the first half fail, and
  the `||` sent the key to the phone while the computer played.) A key pressed
  at the computer's lock screen still runs, and the phone decides.
- **What the phone does with it.** It applies `media_control` only while
  unlocked, with "Media on your computer" and "Control from your computer"
  on, and only to the session it last sent to that machine (the same package
  and session token): through `MediaController.TransportControls` (`play`,
  `pause`, `skipToNext`, `skipToPrevious`, `seekTo`), and only for an action
  that session still allows. Anything else is ignored. The result shows the
  usual way: the session changes, and the phone sends the new state.

**Tests never put a player on the real desktop.** With
`RIME_REMOTED_MEDIA_STUB=<dir>`, rime-remoted appends each change to
`<dir>/media.jsonl` (`{"op":"show","player":…,"identity":…,"status":…,
"title":…,…}` / `{"op":"gone","player":…}`) instead of the bus; a test build
always uses the stub. The stub's `show` line also carries `track` (the
`mpris:trackid`), `can` and `can_control`. Method calls reach the stub the way
§2.7's `closed.jsonl` reaches the notification stub: each complete line
appended to `<dir>/control.jsonl`
(`{"player":…,"method":"PlayPause"|"Play"|"Pause"|"Stop"|"Next"|"Previous"}`,
`{"player":…,"method":"Seek","offset_us":N}`,
`{"player":…,"method":"SetPosition","track":…,"position_us":N}`) is decided
exactly as the bus method would be, and its outcome is appended to
`media.jsonl` as `{"op":"call","player":…,"method":…,"result":"ok"|"not_supported"|"failed"|"bad_line"}`.
Every harness sets it; the device suite refuses to start rime-remoted without
it, and its broker reads the lines back with `media_log` and appends calls with
`media_call` (`MediaOnDeviceTest`, driving the test helper app's media
session). The bus path is proven on a private bus by
`tests/test-remoted-media-bus.sh`: properties read back with `busctl`, the
position moving with the clock, control refused while `can` is empty, the
`Can*` properties and their `PropertiesChanged` once it is not, `busctl` and
`playerctl` calls crossing as `media_control` lines, a stale `SetPosition`
ignored, `Failed` with no events channel, `rime-media-key` going to a computer
player first (also one that takes no keys) and to the phone only when the
computer has none, an update, removal.

### 2.9 Status and Focus: the phone's network and Do Not Disturb, both ways

`remote_hello.features` gains `"phone_focus"`. A phone offers its Focus switch
only to a machine whose hello has it.

```json
{"cmd":"phone_report","battery":62,"network":{"type":"wifi","signal":3},"dnd":true,"can":["ring","focus"]}
→ {"reply":"ok"}
```

- `phone_report` (§2.5) takes two more fields, each optional:
  - `network`: an object. `type` is one of `wifi`, `cellular`, `ethernet`,
    `none`, `other`. `signal` is a whole number from 0 to 4 (Android's own
    bars). Leave `signal` out when the phone does not know it; it is not the
    same as 0.
  - `dnd`: a boolean, true while the phone's interruption filter is anything
    but "all".

  Any other type, a signal outside 0 to 4, or a `dnd` that is not a boolean is
  `bad_request`, and nothing is stored. Both fields are kept in `phones.json`,
  carried in the `devices` reply, and shown by `rime remote devices`, its
  `--json` (on `phone`) and the Shell.
- `can` keeps `"focus"` while the person has Focus sync turned on on the
  phone. A phone without it is never acted on, whatever its `dnd` does.

**Phone → computer.** When a report from a phone whose `can` has `"focus"`
carries a `dnd` different from the last one rime-remoted acted on for that
phone, rime-remoted writes `$XDG_RUNTIME_DIR/rime-remote/focus.json`:

```json
{"dnd":true,"device":"<device id>","at_ms":1763452800000}
```

- `at_ms` is when rime-remoted read the report. The file is written whole
  through a rename: temp file in the same directory, 0600, directory 0700.
  This is written before the report's reply goes back.
- An unchanged `dnd`, or a report that leaves it out, writes nothing, so the
  Shell's watch is not woken.
- "Last acted on" is held in memory per phone, for this daemon's run. It is
  not read back from `phones.json`, because that file outlives the runtime
  directory: after a reboot, a phone already in Do Not Disturb would otherwise
  never be written. A daemon restart therefore writes each phone's state once
  more: if the Shell's differs from the phone's at that moment, the phone's
  wins.
- When a phone drops `"focus"` from `can`, rime-remoted forgets its state.
  When the phone turns Focus back on, its next report counts as the first one
  again. Revoke forgets it too.
- With two phones, the file holds whichever changed last.

**Computer → phone.** A control-socket request, so only the machine's own
user can send it. A paired phone has no verb that does it.

```json
{"cmd":"focus","dnd":true}
→ {"reply":"focused","reached":1}
```

- `{"event":"focus","dnd":<bool>}` goes onto the `device_events` channel of
  each connected phone whose last report has `"focus"` in `can`.
- `reached` counts those writes. Zero is an answer: no phone is connected, or
  none said `"focus"`. There is no push, because a setting is not worth
  waking a phone.
- The CLI form is `rime remote focus on|off`. The Shell runs it when the
  person toggles Do Not Disturb, if its "Do Not Disturb follows your phone"
  switch is on.
- Nothing catches up on connect: a phone that was away when the person
  toggled keeps its own state until the next change on either side.

**The Shell's half** (rime-shell `PhoneFocusService`). The switch "Do Not
Disturb follows your phone" (Settings → Paired devices) is off by default;
off, the Shell reads no file and runs no command. On, a `focus.json` whose
`at_ms` is earlier than the moment following started (the switch turned on,
or the Shell started with it on) is not applied: it describes a moment that
has passed, and a Shell restart must not put a phone's old state over one the
person has chosen since.
- The phone applies it with `NotificationListenerService.requestInterruptionFilter`.
  Listener access is enough; policy access is not needed.

**What the phone sends.** `network` in every report to a machine offering
`phone_report`; `dnd` and `"focus"` only to one whose hello has
`"phone_focus"`. The report is read at the moment it goes, and each machine's
reports go out one at a time, so two Do Not Disturb changes never land in the
wrong order.

- `network` is the default network's: `wifi`, `cellular` or `ethernet` from
  its transports, first match winning (a VPN lists the transport under it, so
  a VPN over Wi-Fi is `wifi`; a bare VPN is `other`); `none` with no default
  network. Wi-Fi bars come from the network's signal strength in dBm on
  Android's own five-level scale (-100 dBm and below is 0, -55 and above is
  4); cellular bars are `SignalStrength.getLevel`. Ethernet, `none` and
  `other` carry no `signal`, and neither does a reading Android did not give.
  No permission is added for either: `NetworkCapabilities.getSignalStrength`
  is not one of the fields Android hides without location access, and
  `TelephonyManager.getSignalStrength` needs none. No location or
  `READ_PHONE_STATE`.
- `dnd` is `getCurrentInterruptionFilter() != INTERRUPTION_FILTER_ALL`, left
  out while Android says the filter is unknown.
- A new report goes at once when `dnd` changes, when the network's `type`
  changes, or when `can` changes (the switch, replying, notification access
  granted or taken away); a change of bars alone at most once a minute; and
  battery as §2.5. The phone hears Do Not Disturb change through
  `ACTION_INTERRUPTION_FILTER_CHANGED`, the network through its default
  network callback, and cellular bars (Android 12 and later, only while the
  default network is cellular) through `TelephonyCallback`.
- `can` has `"focus"` while notification access is granted and the switch
  "Do Not Disturb follows your computer" (on "Notifications on your
  computer", off by default) is on.
- A `focus` line is applied only while the switch is on, notification access
  is granted, the app is unlocked, and the phone's state differs from the
  line's. The switch is checked again on the phone, so a machine that still
  holds an older report saying `"focus"` is not obeyed. `dnd: true` asks for
  `INTERRUPTION_FILTER_PRIORITY`, `false` for `INTERRUPTION_FILTER_ALL`.
- Measured on Android 16 with the app targeting 36: Android keeps a Do Not
  Disturb that an app turns on as a mode of the app's own. `false` from a
  computer ends only that mode, never one the person turned on at the phone
  (the phone keeps its Do Not Disturb, sends nothing, and the two sides stay
  apart until the next change). `cmd notification set_dnd off` does not end
  the app's mode either.
- Never left stranded: the app remembers that it turned Do Not Disturb on
  for a computer (a `focus` line applied `PRIORITY`), and forgets it when a
  line applies `ALL` or when Android reports the filter back at `ALL` by any
  path. While it remembers, turning the switch off or locking the app asks
  the listener for `INTERRUPTION_FILTER_ALL`. A Do Not Disturb the person
  turned on at the phone is never asked off this way. The memory is the
  process's: a process Android ended forgets it.
- Without notification access the listener cannot ask anything, and the
  app's mode stays on (measured, Android 16 emulator: revoking access left
  `implicit_com.rimeos.remote` active, filter `PRIORITY`). It is taken back
  when access returns with the switch off or the app locked. Until then the
  person ends it at the phone: the Quick Settings "Modes" tile shows
  "Modes · Do Not Disturb (Rime Remote)"; tapping it opens a list with
  "Do Not Disturb (Rime Remote) · On · Managed by Rime Remote", and tapping
  that entry turned it off (measured). Settings → Modes lists the same
  entry, whose page has a "Turn off" button (seen, not pressed). Not
  measured on a real phone.
- Proven on the emulator by `FocusOnDeviceTest` (the record carries
  `network` and `dnd`, and `"focus"` only once switched on; `rime remote
  focus on|off` turns the emulator's Do Not Disturb on and off;
  `set_dnd on|off` reaches `focus.json`, read by the broker's `focus_file`
  from the run's own runtime directory; with the switch off, `reached` is 0
  and `focus.json` is not written; turning the switch off or locking ends a
  Do Not Disturb a computer turned on, and leaves the person's own on). The cellular path is not exercised there:
  the emulator's default network is Wi-Fi.

**State, not toggles: why nothing loops.** Each side acts only when the state
it is given differs from its own:

- The Shell sets its Do Not Disturb from `focus.json` only when the value
  differs. It never runs `rime remote focus` for a change that came from the
  file.
- The phone applies a `focus` line only when its own state differs, then
  reports the result.
- rime-remoted writes the file only when a phone's `dnd` changed.

So when the person toggles on the computer, the phone changes, reports, and
the file is written with the value the Shell already has: nothing changes. A
`focus` request never writes `focus.json` itself; only a phone's report does.

**Tests never touch the real desktop's file.** rime-remoted takes the path
from `main.rs` only. A `State` built by a unit test has none and writes
nothing, because the unit binary runs inside the developer's session, where
`$XDG_RUNTIME_DIR` is the one the real Shell watches. Every harness, including
`android/tools/run-device-suite.sh` and its broker's restart, starts the
daemon with a private `XDG_RUNTIME_DIR`. So no `RIME_REMOTED_*_STUB` is needed
for this file.

### 2.10 The companion link: a second key, a smaller session

Feature `companion_link`. A phone holds its device key behind its lock screen,
so after a restart it cannot open a session until it is unlocked. The
companion key is a second X25519 key the phone keeps without that gate; a
session on it may use the device features below and nothing else.

**Minted by a full session only.** No change to pairing:

```json
{"cmd":"companion_register","public_key":"<32-byte X25519 public key, base64url, no padding>"}
→ {"reply":"ok"}
{"cmd":"companion_forget"}
→ {"reply":"ok"}
```

- The device is the handshake's; the body names none. One key per device: a
  new one replaces the old.
- `companion_register` errors: `kind` `bad_request` (no `public_key`, or not 32
  bytes of base64url), `key_in_use` (the key is this device's own key, either
  key of any other device, revoked ones included, or a companion key any
  device ever gave up; one message for every case, so the answer does not say
  which device holds it).
- A companion key given up (replaced, forgotten, or dropped by pairing the
  device again) is retired for good: it can never be registered again, by
  any device, nor paired as a device key.
- Replacing a key, or `companion_forget`, ends that device's open companion
  sessions at once; its full sessions go on. `companion_forget` with no key set
  is `ok`.
- Both verbs from a companion session: `not_allowed`.

**Scope by key.** The handshake decides it: the device's own key opens a
`full` session, its companion key a `companion` session. Nothing the phone
sends changes it. `remote_hello` says which (§2.1).

**What a companion session may do: an allow list, checked before routing.**

| | allowed |
| --- | --- |
| control verbs | `remote_hello`, `phone_report`, `ring_ack`, `offer_ack`, `offer_done`, `phone_notification`, `phone_notification_removed`, `phone_media`, `messages_reply`, `messages_changed`, `call_state`, `call_done`, `call_history_reply` |
| channels (`Open`) | `device_events`, `take_offer` |

Everything else is refused, whatever it is: `push_register` and
`push_unregister` (a push carries `rime-agentd`'s session kind, id and start
time, so only the full key may say where pushes go), `machine_status`, `machine_lock`,
`media`, `media_control`, `volume`, `nex`, `nex_events`, `messages_sent`
(a send needs an unlocked phone, §2.11), `photos_reply` and
`photos_fetch_reply` (photos need an unlocked phone, §2.13), `companion_register`,
`companion_forget`, every `rime-agentd` verb (`hello`, `list`, `input`,
`privilege_request`, `decide`, `clipboard_write`, …), a line with no readable
`cmd`, and every other channel (`attach`, `receive`, `save_file`,
`photos_upload`, nex's feed). A companion session reaches `rime-agentd` never, not even to be told
no:

```json
→ {"reply":"error","kind":"not_allowed","message":"machine_lock is not allowed on a companion link; unlock the phone to use it"}
```

A refused `Open` gets that line on channel zero, then a `Close` for the
channel, in order with the other replies. `Data` for a refused channel is
dropped. A verb a later release adds to rime-remoted is refused on a
companion session until it is added to this list on purpose (the list is
`rimed/rime-remoted/src/companion.rs`; a unit test fails until every verb has
a decision).

`clipboard_write` (§1.8) stays refused: it is a `rime-agentd` verb, and a
companion session forwards nothing there. If rime-remoted answers a clipboard
verb itself one day, it joins the list (`CLIPBOARD_VERBS`).

**Revoke** (`rime remote revoke`) refuses both keys and drops every live
connection the device holds, companion ones included; a refused companion key
gets the same handshake refusal as a key that was never paired. Pairing the
device again starts with no companion key, and the old one stays retired. A
connection is put on the live list and only then authorised a second time, so
a revoke, `companion_forget` or replaced key that lands during its handshake
still ends it.

**`rime remote devices`** says which devices have one ("keeps working after a
restart"; `--json`: `"companion": true`). The control socket's `devices`
reply carries a `companions` list of device ids and never the key.

### 2.11 Messages: the phone's SMS, read and sent from the computer

Feature `phone_messages`. Like Messages on a Mac, or Phone Link's Messages:
the computer lists the phone's SMS conversations, reads one, and sends an SMS
from the phone's number. Nothing is stored on the computer: every answer is
read from the phone when it is asked for, and held in memory only until the
reply to the caller goes.

**The phone offers it.** `can` (§2.5) has `"messages"` only while all hold:
the person turned on "Messages on your computer" in the app (off by default),
Android granted READ_SMS, and the app is unlocked or in companion mode. A
machine never asks a phone without it. The phone checks its switch and
permission again on every request, whatever an older report said.

**Computer → phone**, on the phone's `device_events` channel:

```json
{"event":"messages_request","id":7,"limit":20}
{"event":"messages_request","id":8,"thread":42,"before":1763452800000,"limit":20}
{"event":"messages_send","id":9,"to":["+15555215556"],"text":"on my way"}
{"event":"messages_send","id":10,"thread":42,"text":"on my way"}
```

- No `thread`: the conversation list. With `thread`: that conversation's
  messages. `before` (unix ms) pages back; `limit` is 1 to 50.
- A send names exactly one of `to` (one number) and `thread`. `text` is 1 to
  2,000 characters, no control characters but newlines. A send goes only to
  a phone whose connection is `full` (§2.10) and is refused on the computer,
  before anything is written, for a companion connection.

**Phone → computer**, control verbs:

```json
{"cmd":"messages_reply","id":7,"threads":[{"id":42,"addresses":["+15555215556"],"name":"Pat","snippet":"see you","date_ms":1763452800000,"unread":1,"count":12}],"more":true}
{"cmd":"messages_reply","id":8,"thread":42,"messages":[{"id":901,"date_ms":1763452800000,"from_me":false,"body":"see you","status":"received"}]}
{"cmd":"messages_reply","id":7,"error":"off"}
{"cmd":"messages_sent","id":9,"ok":true}
{"cmd":"messages_sent","id":10,"ok":false,"error":"no_service"}
{"cmd":"messages_changed"}
→ {"reply":"ok"}
```

- Newest first. An answer may come in several lines; every line but the last
  has `"more":true`. At most 64 lines and 512 KiB for one answer, and no more
  items than `limit`; past either, the request fails.
- `status` is `received`, `sent`, `delivered`, `sending` or `failed`. Drafts
  are never sent. `name` is a contact's name, only when the person allowed
  contacts too. A thread has 1 to 10 addresses.
- `error` words: `off`, `no_permission`, `locked`, `busy`, `no_thread`,
  `group`, `too_long`, `failed`, `timeout`, `no_service`. An unknown word is
  read as `failed`. A send into a thread with more than one address is
  `group`: one SMS each would be several conversations, not that one.
- `messages_sent` is `ok` only when Android reported every part of the
  message sent.
- `messages_changed` carries nothing: the phone's SMS store changed, and the
  computer may ask again. The control socket's `messages` reply carries the
  last one's time as `changed_ms`.
- An answer whose `id` is not the open request of that device AND connection
  gets `{"reply":"error","code":"unknown_id"}`; one that breaks a rule above
  gets `bad_request`. The computer cleans every text again (control and bidi
  characters out, a body capped at 2,000 characters and 40 lines, a snippet
  at 160, a name at 60, an address at 40) and does not rely on the phone.

**One request per phone at a time** (`busy`). A request belongs to the
connection it was written on: when that connection ends, or the device is
revoked, it fails at once and what had arrived is dropped. The computer waits
10 s for an answer and 35 s for a send's result; the phone waits up to 25 s
for Android to report a send.

**The control socket.**

```json
{"cmd":"messages","device":"<id>","thread":42,"before":1763452800000,"limit":20}
→ {"reply":"messages","device":"<id>","thread":42,"messages":[…],"changed_ms":1763452800000}
{"cmd":"messages_send","device":"<id>","to":"+1 555 521 5556","text":"on my way"}
→ {"reply":"message_sent","device":"<id>"}
```

`device` may be left out when exactly one connected phone offers messages.
Both verbs are allowed only to a person at this machine: a terminal in the
login session or Rime Settings, as for pairing. A scheduled job, an agent
session or any other process of the user is refused, for reading as well as
sending. `rime remote messages` and `rime remote messages send` are these
two (`--json` prints the reply's fields, or `{"error":…}`).

### 2.12 Calls: the phone's calls, seen and handled from the computer

Feature `phone_calls`. Like calls on a Mac with an iPhone, or Phone Link's
Calls, without the audio: a call ringing on the phone shows on the computer
with Answer and Decline; from the computer the person answers, declines or
hangs up the phone's call, dials a number from it, and reads its call history.
The sound stays on the phone (or a headset paired with it). Nothing is stored
on the computer: the current call is held in memory with the connection that
reported it, and a history answer only until the reply to the caller goes.

**The phone offers it.** `can` (§2.5) has `"calls"` only while the person
turned on "Calls on your computer" in the app (off by default) and Android
granted READ_PHONE_STATE. With it, each of these says what else the computer
may do, and each needs its own permission:

| word | permission | lets the computer |
| --- | --- | --- |
| `calls_history` | READ_CALL_LOG | ask for the history (and the phone can name a ringing caller) |
| `calls_answer` | ANSWER_PHONE_CALLS | answer, decline and hang up |
| `calls_dial` | CALL_PHONE | dial; never sent on a companion connection |

The phone checks its switch and permissions again on every request, whatever
an older report said.

**Phone → computer**, control verbs:

```json
{"cmd":"call_state","state":"ringing","number":"+15555215556","name":"Pat","since_ms":1763452800000}
{"cmd":"call_state","state":"idle","since_ms":1763452830000}
→ {"reply":"ok"}
{"cmd":"call_done","id":3,"ok":true}
{"cmd":"call_done","id":4,"ok":false,"error":"no_call"}
{"cmd":"call_history_reply","id":5,"calls":[{"number":"+15555215556","name":"Pat","type":"missed","date_ms":1763452800000,"duration_s":0}],"more":true}
{"cmd":"call_history_reply","id":5,"error":"off"}
→ {"reply":"ok"}
```

- `call_state`: `state` is `ringing`, `active`, `held` or `idle`; `since_ms`
  (unix ms, the phone's clock) is when it entered that state. `number` and
  `name` may be left out; `name` is a contact's, only when the person allowed
  contacts. The phone says it when the state changes, and once on a new
  connection while a call is going. A `call_state` from a phone whose report
  lacks `"calls"` is refused (`code` `off`) and changes nothing. Android
  reports a ringing call twice, once before the number is known: a numberless
  `ringing` after one with a number keeps the number. Rime Remote on Android
  never sends `held`, because Android does not tell an app that.
- `call_done` answers one `call_action`: `ok`, or `error`.
- `call_history_reply`: newest first, in one or more lines; every line but
  the last has `"more":true`. At most 64 lines and 256 KiB for one answer, and
  no more calls than `limit`. `type` is `incoming`, `outgoing`, `missed` or
  `rejected`; voicemail is left out. `number` is empty for a withheld number.
- `error` words: `off`, `no_permission`, `locked`, `busy`, `no_call`,
  `emergency`, `failed`. An unknown word is read as `failed`. `emergency`: the
  phone does not dial an emergency number for a computer; that call is made
  on the phone.
- As in §2.11: an answer whose `id` is not the open request of that device
  AND connection gets `unknown_id`; one that breaks a rule above gets
  `bad_request`; the computer cleans every text again (a number at 40
  characters, a name at 60) and does not rely on the phone.

**Computer → phone**, on the phone's `device_events` channel:

```json
{"event":"call_action","id":3,"action":"answer"}
{"event":"call_action","id":6,"action":"dial","number":"+15555215556"}
{"event":"call_history_request","id":5,"limit":20,"before":1763452800000}
```

- `action` is `answer`, `decline`, `hangup` or `dial`; only `dial` carries a
  `number` (3 to 20 digits, an optional leading `+`). The computer checks the
  phone's last report and call first: answer and decline need a ringing call,
  hangup any call, dial none.
- **Locked phones.** Answer, decline and hangup are sent on a companion
  connection (§2.10) too, as the phone's own lock screen allows them. Dial is
  not: on a companion connection the computer refuses it before anything is
  written, and the phone refuses it too (`locked`).
- `call_history_request`: `limit` 1 to 50, `before` (unix ms) pages back.

**One request of each kind per phone at a time** (`busy`). A request belongs
to the connection it was written on: when that connection ends, or the device
is revoked, it fails at once. The computer waits 10 s for a history and 10 s
for an action's `call_done`. When the connection that reported a call ends,
the call goes with it.

**The ringing notification.** While a phone's call rings, the computer shows a
notification through the same server as §2.7: summary "Call from <name or
number>" (or "Incoming call"), app name "<phone> · Calls", no timeout, and
Answer and Decline buttons when the report has `calls_answer`. It closes when
the phone says the call stopped ringing, its connection ends, the device is
revoked, or the report loses `"calls"`. A press is heard only from the
notification server's current owner of `org.freedesktop.Notifications`, and
only on this notification, as for an inline reply (§2.7); a press whose action
fails shows a short notice saying why. A fresh ringing call counts against
the phone's notification rate (§2.7); an update does not.

**The control socket.**

```json
{"cmd":"calls","device":"<id>","before":1763452800000,"limit":20}
→ {"reply":"calls","device":"<name>","current":{"state":"ringing","number":"+15555215556","since_ms":1763452800000},"calls":[…]}
{"cmd":"call","device":"<id>","action":"dial","number":"+1 555 521 5556"}
→ {"reply":"call_done","device":"<name>","action":"dial"}
```

`device` may be left out when exactly one paired phone offers calls. When the
history cannot be read, `calls` still answers with `current` and says why as
`history_error`. Both verbs are allowed only to a person at this machine, as
§2.11's. `rime remote calls` is the first; `rime remote calls answer`,
`decline`, `hangup` and `dial NUMBER` are the second (`--json` prints the
reply's fields, or `{"error":…}`).

### 2.13 Photos: the phone's recent photos, browsed and saved from the computer

Feature `phone_photos`. Like Phone Link's Photos: the computer lists the
phone's newest photos and screenshots as small thumbnails, and one request
saves a full photo onto the computer. Images only: no video. Nothing about a
photo is kept on the computer except the thumbnails of the last listing, in
memory-backed storage, and the photos the person chose to save.

**The phone offers it.** `can` (§2.5) has `"photos"` only while all hold: the
person turned on "Photos on your computer" in the app (off by default),
Android lets the app read images (READ_MEDIA_IMAGES on Android 13 and later,
the partial "selected photos" access on Android 14 and later, or
READ_EXTERNAL_STORAGE before 13), and the connection is a full one with the
app unlocked. Never on a companion connection (§2.10): a photo is more than a
battery level, so a phone that was restarted and not yet unlocked shows none.
The phone checks its switch, its access and its lock again on every request,
whatever an older report said. A machine never asks a phone without the word.

**Partial access.** When the person allowed only some photos, Android shows
the app those and no others, and every `photos_reply` line says
`"partial":true`; the computer says so with the list ("only the photos you
picked on the phone are shown").

**Computer → phone**, on the phone's `device_events` channel:

```json
{"event":"photos_request","id":7,"limit":30}
{"event":"photos_request","id":8,"before":1763452800000,"limit":30}
{"event":"photos_fetch","id":9,"photo":1000004211}
```

- `photos_request`: the newest photos, newest first; `before` (unix ms,
  exclusive) pages back by `date_ms`; `limit` is 1 to 60.
- `photos_fetch`: send the original of photo `photo` (an id from a listing)
  over a `photos_upload` channel (below).
- Both are written only to a full connection's channel: for a companion one
  the computer refuses before anything is written.

**Phone → computer**, control verbs:

```json
{"cmd":"photos_reply","id":7,"photos":[{"id":1000004211,"name":"PXL_20261118_0412.jpg","date_ms":1763452800000,"width":4080,"height":3072,"size":3145728,"mime":"image/jpeg","thumb":"<base64url JPEG>"}],"more":true}
{"cmd":"photos_reply","id":7,"photos":[],"partial":true}
{"cmd":"photos_reply","id":7,"error":"off"}
{"cmd":"photos_fetch_reply","id":9,"error":"not_found"}
→ {"reply":"ok"}
```

- `photos_reply`: newest first, in one or more lines; every line but the
  last has `"more":true`. A line is at most 60,000 bytes (a wire frame holds
  65,514), so the phone puts as many photos in a line as fit and one always
  fits. At most 64 lines and 2.5 MiB for one answer, and no more photos than
  `limit`; past either, the request fails.
- A photo: `id` (a whole number, the phone's own), `name`, `date_ms` (when it
  was taken, or added when the phone does not know), `width`, `height`,
  `size` (bytes of the original), `mime` (`image/…`), and `thumb`: a JPEG,
  base64url without padding, longest side at most 256 px, at most 24 KiB
  (the computer accepts up to 32 KiB). The phone re-encodes every thumbnail
  from pixels, so no EXIF, location or other metadata is in it; the computer
  drops every APP1 to APP15 and COM segment again before it writes one, and
  a thumbnail that is not a JPEG is left out (the photo stays in the list
  without one). `thumb` may be left out when the phone could not make one.
- `photos_fetch_reply` answers a `photos_fetch` only when the phone will not
  send it; a photo it sends needs no line here.
- `error` words: `off`, `no_permission`, `locked`, `busy`, `not_found`,
  `too_large`, `failed`. An unknown word is read as `failed`. `not_found`: no
  such photo, or not one the phone may show (partial access).
- As in §2.11: an answer whose `id` is not the open request of that device
  AND the connection it was written on gets `unknown_id`; one that breaks a
  rule above gets `bad_request`; the computer cleans every text again (a name
  at 120 characters, reduced as §1.8 reduces a file name; a mime type at 40
  ASCII characters) and does not rely on the phone.

**The upload.** `Open` with

```json
{"cmd":"photos_upload","id":9,"name":"PXL_20261118_0412.jpg","len":3145728}
→ {"reply":"uploading","len":3145728}
   … exactly len bytes, as Data …
← {"reply":"uploaded","len":3145728}   (as Data on the channel, then Close)
```

- Answered by rime-remoted, not forwarded. Taken on any FULL connection of
  the device the fetch was written to (the phone opens a connection of its
  own for an upload, as for §1.8), only while that fetch waits, and once. An
  unknown, finished or other device's id is
  `{"reply":"error","code":"unknown_id",…}` and a close.
- Everything refusable (the id, `len` of 1 byte to 64 MiB, the name) is
  refused BEFORE `uploading`, so no byte crosses to be thrown away.
- Nothing arriving for 30 s ends the upload, and so does the whole upload
  taking longer than 300 s plus one second per 128 KiB. A channel closed
  before `len` bytes fails the save and leaves no file.
- The original is sent as Android gives it to the app. Rime Remote does not
  ask for ACCESS_MEDIA_LOCATION, so on Android 10 and later Android blanks
  the location in a photo's EXIF before the app reads it (measured on the
  Android 16 emulator: a pushed photo with GPS tags is saved on the computer
  without them, and a photo without a location is saved byte for byte).
  Android 9 hands the file over as it is.

**One list and one fetch per phone at a time** (`busy`). A list belongs to
the connection it was written on, and fails at once when that connection ends
or the device is revoked. A fetch fails at once on revoke, and when the
connection it was written on ends before an upload began. The computer waits
30 s for a whole list, 20 s for an upload to begin (or a refusal), then as
above.

**Thumbnails on the computer.** Each photo's thumbnail is written to
`$XDG_RUNTIME_DIR/rime-remote/photos/<device id>/<photo id>.jpg` (the
directory 0700, the file 0600; the runtime directory is memory-backed and
the user's own, never the home directory). The file name is built from the
whole number, never from a phone's text. A listing without `before` replaces
the device's files; one with `before` adds a page to them, up to 600 files,
past which it replaces them too. They are all removed when the connection that
asked for the listing ends, when the device is revoked, when its report no
longer has `"photos"`, and when rime-remoted starts.

**Saving.** The person's own process opens the file: `rime remote photos
save` creates a hidden file (`O_EXCL|O_NOFOLLOW`, 0600) in the folder, and
passes it with the request as one `SCM_RIGHTS` descriptor, as `send` passes
a file (§2.6): rime-remoted has a private `/tmp` and no view of the caller's
working directory, so a path would be wrong. rime-remoted writes the photo's
bytes into it and answers; the CLI then checks the length, gives the file
mode 0644 and its name. A name already there is never replaced: the copy
becomes `PXL_… (2).jpg`, and so on (a hard link, which fails rather than
replaces; a rename where the file system has no hard links). A save that
fails removes the hidden file. The folder is `--to DIR`, else
`$XDG_PICTURES_DIR/Rime Remote` (from `~/.config/user-dirs.dirs`), else
`~/Pictures/Rime Remote`, created if missing.

**The control socket.**

```json
{"cmd":"photos","device":"<id>","before":1763452800000,"limit":30}
→ {"reply":"photos","device":"<name>","photos":[{"id":…,"name":…,"date_ms":…,"width":…,"height":…,"size":…,"mime":…,"thumb":"/run/user/1000/rime-remote/photos/<device id>/<photo id>.jpg"}],"partial":false}
{"cmd":"photos_save","device":"<id>","photo":1000004211}   + the open file, SCM_RIGHTS
→ {"reply":"photo_saved","device":"<name>","photo":1000004211,"name":"PXL_20261118_0412.jpg","len":3145728}
```

`thumb` in the reply is the file's path, or absent when there is none.
`device` may be left out when exactly one paired phone offers photos. Both
verbs are allowed only to a person at this machine, as §2.11's: a scheduled
job, an agent session or any other process of the user is refused, for
listing as well as saving. `rime remote photos` is the first (`--json` prints
the reply's fields, or `{"error":…}`); `rime remote photos save ID [--to
DIR]` is the second (`--json`: `{"device","photo","path","len"}`).

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
