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
 "features":["input_submit","rename","peek","session_name","session_title","size_restore"]}
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
