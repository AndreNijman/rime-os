package com.rimeos.remote.core.agent

import kotlinx.serialization.SerialName
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * `rime-agentd`'s `SessionInfo`, as much of it as a phone shows.
 *
 * ## The shapes that catch people
 *
 * * **`Response::Session` does not nest.** It is a serde newtype variant under
 *   an internal tag, so the session's fields sit *beside* `"reply"` rather
 *   than under a `"session"` key. `Response::Sessions` does nest, because
 *   serde cannot serialise a newtype variant wrapping a sequence at all.
 * * **Nothing is `skip_serializing_if`.** An absent optional arrives as
 *   `null`, and `children` arrives as `[]` rather than being missing — the
 *   daemon's own comment says the distinction is load-bearing: an empty list
 *   means "asked and found none", a missing key would mean "this daemon does
 *   not have the graph".
 * * **`started` and `last_activity` are unix SECONDS.** Everything else with a
 *   time in it — `grant_expires_ms`, `ttl_ms` — is milliseconds.
 * * The policy is flattened into the session, so `sandbox`, `network` and the
 *   rest are top-level keys. `origin` among them is policy dimension six;
 *   *which* origin asked is the separate `request_origin`, in kebab-case.
 */
@Serializable
data class AgentSession(
    val id: Int,
    /** The adapter: `claude`, `codex`, `generic`. Not the program. */
    val agent: String = "generic",
    val program: String = "",
    val args: List<String> = emptyList(),
    val cwd: String = "",
    val project: String? = null,
    @SerialName("project_name") val projectName: String? = null,
    val worktree: String? = null,
    val state: String = AgentStates.STARTING,
    /** Free text the runtime attached to the state. `"paused"`, an error, a step. */
    val detail: String? = null,
    /**
     * What a PERSON called this session, or null.
     *
     * Set only by a person — `rime agent run --name`, a rename from the CLI,
     * the Shell or this phone — and never from anything the agent printed.
     * `SessionInfo` always serialises the key, so `"name": null` means "this
     * daemon has names and none is set" and a missing key means an older
     * daemon; both decode to null here, and the difference that matters to a
     * screen (whether renaming is possible) is read off `Hello.features`
     * instead of guessed from a null.
     */
    val name: String? = null,
    /**
     * The agent's own terminal title (OSC 0 / OSC 2), cleaned by the daemon.
     *
     * **Agent-controlled text.** The daemon strips spinner glyphs and control
     * characters and clips it to 80 characters, and this phone still treats it
     * as display-only: nothing branches on it, it is sanitised again before it
     * is drawn ([SessionNames.forDisplay]), and it never reaches a
     * notification — an agent that could put words on a lock screen could put
     * a secret there.
     */
    val title: String? = null,
    val paused: Boolean = false,

    // The policy, flattened. Six of the seven dimensions; `connectors` is
    // omitted because nothing on a phone shows it.
    val sandbox: String? = null,
    val network: String? = null,
    val system: String? = null,
    val secrets: String? = null,
    val native: String? = null,

    @SerialName("request_origin") val requestOrigin: String? = null,
    @SerialName("origin_source") val originSource: String? = null,
    /** Which device asked, when a device did. This phone's own id, usually. */
    val actor: String? = null,

    val telemetry: Telemetry? = null,
    val children: List<ChildInfo> = emptyList(),

    val pid: Int = 0,
    /** Unix **seconds**. */
    val started: Long = 0,
    /** Unix **seconds** of the last PTY output or published event. */
    @SerialName("last_activity") val lastActivity: Long = 0,
    @SerialName("exit_code") val exitCode: Int? = null,
    @SerialName("exit_signal") val exitSignal: Int? = null,
    /**
     * The checkpoint taken before this session started, when one was asked
     * for. Null when none was — which is the default; `auto_checkpoint` is
     * false in `Config::default()`.
     *
     * An **id**, and nothing more: `Checkpoint` itself carries a label, a
     * tree, the branch and HEAD it was taken from and the packages installed
     * since, and none of that crosses this wire. See [undoCommand] for what a
     * phone can honestly do with it.
     */
    val checkpoint: String? = null,
    /** How many clients are attached, this phone included once it attaches. */
    val attached: Int = 0,
    val cols: Int = 80,
    val rows: Int = 24,
) {
    val tone: Tone get() = AgentStates.tone(state)
    val needsYou: Boolean get() = AgentStates.needsYou(state)
    val isTerminal: Boolean get() = AgentStates.isTerminal(state)
    val agentName: String get() = AgentNames.of(agent)

    /** Project name, project path, or working directory — the first that exists. */
    val where: String get() = projectName ?: project ?: cwd

    /**
     * The label a session had before sessions had names: `Claude · rime`.
     *
     * The adapter, then the project's name or the last part of the directory.
     * It is the third choice of [displayName] and the one every session has,
     * which is why a named session still shows it as its secondary line — a
     * name says what the work is, and this says which agent is doing it where.
     */
    val label: String
        get() {
            val place = where.trimEnd('/').substringAfterLast('/')
            return if (place.isEmpty()) agentName else "$agentName · $place"
        }

    /**
     * What to call this session on a screen: its name, else its title, else
     * [label].
     *
     * The order is the contract's (§1.3) and the Shell's and the CLI's too, so
     * the phone, the desktop and `rime agent list` never disagree about what a
     * session is called. A person's name wins over the agent's title because
     * only the person's is a statement anybody made on purpose.
     *
     * **Screens only.** `NotificationContent` does not call this and must not:
     * a name can say what somebody is working on and a title is whatever the
     * agent chose to print, and the lock-screen rule is fixed words, an
     * adapter and a machine.
     */
    val displayName: String
        get() = SessionNames.forDisplay(name, SessionNames.MAX_CHARS)
            ?: SessionNames.forDisplay(title, SessionNames.MAX_TITLE_CHARS)
            ?: label

    /** Whether [displayName] is something other than [label]. */
    val isNamed: Boolean get() = displayName != label

    /**
     * The command that undoes this session's work, to be run **at the
     * machine**.
     *
     * A string to read and not a button to press, and that is measured rather
     * than a choice about caution. `checkpoint::list` and `checkpoint::restore`
     * exist in `rime-agent-core`, but their only non-test callers are in
     * `rimed/rime/src/agent.rs` — the CLI — operating on the local filesystem
     * directly. **There is no checkpoint verb on the socket**: not list, not
     * restore, not undo. `protocol.rs`'s `Request` mentions checkpoints in
     * exactly two places, `RunRequest.checkpoint` (take one before starting)
     * and this field (the id of the one taken), and neither reads or reverses
     * anything.
     *
     * So a phone cannot perform an undo, and it also cannot show its
     * consequences: what `restore` would change, which files it would touch,
     * what it would unwind — all of that is computed on the machine from the
     * checkpoint's tree, and none of it crosses this wire. Showing the command
     * is the whole of what is available, and inventing a consequence summary
     * from the fields that ARE here would be describing a destructive
     * operation from the wrong data.
     */
    val undoCommand: String? get() = checkpoint?.let { "rime agent undo --checkpoint $it" }

    /**
     * How long this session has been going, in milliseconds.
     *
     * [nowMs] is passed in rather than read, because a list that recomputed
     * `System.currentTimeMillis()` per row would show rows a millisecond apart
     * and because a test cannot pin a clock it does not hold.
     */
    fun elapsedMs(nowMs: Long): Long = (nowMs - started * 1000L).coerceAtLeast(0)

    /**
     * How long since anything happened, in milliseconds.
     *
     * Worth showing separately from [elapsedMs] on anything not working: a
     * session that has been "waiting for you" for two minutes and one that has
     * been waiting for two hours are different situations.
     */
    fun idleMs(nowMs: Long): Long = (nowMs - lastActivity * 1000L).coerceAtLeast(0)
}

/**
 * What the agent told the runtime about itself.
 *
 * Every field is optional and `null` means **"we have never heard"**, which is
 * not the same as zero. A context gauge drawn at 0% for a session that has
 * never reported is a gauge that is lying.
 *
 * The two rate-limit figures are **account-wide, not per session**. Six
 * sessions on one login report the same number, and a list that drew it per
 * row would be repeating one fact six times; the daemon's own comment says so.
 */
@Serializable
data class Telemetry(
    /** `"Opus 4.5"`, not `claude-opus-4-5`. */
    val model: String? = null,
    @SerialName("context_pct") val contextPct: Double? = null,
    @SerialName("five_hour_pct") val fiveHourPct: Double? = null,
    @SerialName("five_hour_reset") val fiveHourReset: Long? = null,
    @SerialName("seven_day_pct") val sevenDayPct: Double? = null,
    @SerialName("seven_day_reset") val sevenDayReset: Long? = null,
    /** The git branch, read from `.git/HEAD` by the daemon, not from the agent. */
    val branch: String? = null,
    @SerialName("observed_at") val observedAt: Long = 0,
)

/**
 * One node of the agent graph: a subagent, or a process the session forked.
 *
 * The graph is **intra-session**. `parent` names another child *within the
 * same session*, which is what makes it a tree — an MCP server forked by a
 * language server forked by the agent is three levels down. There is no
 * parent-session field anywhere, and a phone that drew one would be inventing
 * a relationship the runtime does not model.
 */
@Serializable
data class ChildInfo(
    val id: String,
    /** `subagent` or `process`. */
    val kind: String = "process",
    val label: String = "",
    val started: Long = 0,
    val ended: Long? = null,
    @SerialName("ended_by") val endedBy: String? = null,
    val parent: String? = null,
    val pid: Int? = null,
    @SerialName("rss_kb") val rssKb: Long? = null,
) {
    val live: Boolean get() = ended == null
    val isSubagent: Boolean get() = kind == "subagent"
}

/** What the daemon says about itself, and the only request a mismatched client can rely on. */
@Serializable
data class Hello(
    val version: Int = 0,
    /**
     * The adapters this runtime has.
     *
     * This is the closest thing to the "profile" P1-054's criterion names.
     * There is no profile verb on this socket — `rime-agent-core/src/profile.rs`
     * exists and is not reachable through `Request` — so what a phone can
     * actually offer when starting an agent is: which adapter, which directory,
     * which worktree. Saying that plainly is better than inventing a picker
     * for something the runtime will not accept.
     */
    val agents: List<String> = emptyList(),
    @SerialName("default_agent") val defaultAgent: String = "",
    /**
     * What this daemon can do beyond the verbs every version has.
     *
     * `#[serde(default)]` on the far side and empty here when absent, and an
     * empty list means exactly what an absent key does: an older daemon, which
     * gets today's behaviour for everything. A client uses a feature ONLY when
     * its string is present — never on a version number, which cannot say
     * that a feature was backported, and never by trying the verb and reading
     * the refusal, which for `input` would type a sentence it then could not
     * submit.
     */
    val features: List<String> = emptyList(),
) {
    fun has(feature: String): Boolean = feature in features
}

/** The daemon said no, in its own vocabulary. */
class AgentError(val kind: String, override val message: String) : Exception(message)

/**
 * Building requests and reading replies.
 *
 * Requests are built as strings rather than serialised from objects, and that
 * is deliberate for one reason: a control payload **must not contain a
 * newline** — `rime-remote-core`'s wire refuses one in both directions,
 * because a payload carrying its own terminator would let a client smuggle a
 * second request into one frame. A hand-built line is one it is obvious to
 * check; a serialiser's pretty-printer is one it is easy to forget.
 */
object Agentd {
    val json: Json = Json {
        // A daemon newer than this app sends fields it has never heard of, and
        // the right response to that is to show what it does understand.
        ignoreUnknownKeys = true
        explicitNulls = false
        encodeDefaults = false
    }

    fun hello(): String = """{"cmd":"hello"}"""

    fun list(): String = """{"cmd":"list"}"""

    fun info(id: Int): String = """{"cmd":"info","id":$id}"""

    fun attach(id: Int, cols: Int, rows: Int, replay: Int = DEFAULT_REPLAY): String =
        """{"cmd":"attach","id":$id,"cols":$cols,"rows":$rows,"replay":$replay}"""

    fun resize(id: Int, cols: Int, rows: Int): String =
        """{"cmd":"resize","id":$id,"cols":$cols,"rows":$rows}"""

    /**
     * A signal, by the daemon's own names.
     *
     * `pause` and `resume` are not verbs on this socket; they are `SIGSTOP`
     * and `SIGCONT`, and `paused` becomes true only after the kill succeeds.
     * Naming them here rather than at the call site keeps that fact in one
     * place instead of in every button.
     */
    fun signal(id: Int, signal: String): String =
        """{"cmd":"signal","id":$id,"signal":"${escape(signal)}"}"""

    fun pause(id: Int): String = signal(id, "stop")

    fun resume(id: Int): String = signal(id, "cont")

    /** Ask politely. `kill` is `SIGKILL` and skips whatever cleanup the agent does. */
    fun stop(id: Int): String = signal(id, "term")

    fun interrupt(id: Int): String = signal(id, "int")

    /**
     * Type into a live session's terminal without attaching to it.
     *
     * `Request::Input { id, data }` writes RAW BYTES to the PTY master
     * (`session::write_input`) and appends nothing — no newline, no encoding.
     * A reply sent without a terminator sits unsubmitted on the agent's input
     * line, which looks to the user exactly like nothing having happened, so
     * callers go through [Reply.bytes] rather than passing text straight in.
     *
     * A phone is allowed this verb. `privilege::refuse_input` refuses exactly
     * two callers — a connection that IS a managed session, and one whose
     * origin could not be classified — and a paired device is neither. See
     * `Reply.kt` for why that is the daemon's reasoning rather than a gap.
     */
    fun input(id: Int, data: String): String =
        """{"cmd":"input","id":$id,"data":"${escape(data)}"}"""

    /**
     * Type into a live session and then press Return, as two writes.
     *
     * `{"cmd":"input","id":N,"data":"…","submit":true}` — only against a
     * daemon whose `hello` lists [Features.INPUT_SUBMIT]. The daemon writes
     * [data], waits 80 ms, and writes `"\r"` separately. The gap is the whole
     * fix, and it was measured rather than guessed (2026-09-30, Claude Code
     * 2.1.283 in a pty): a ~250-character burst that ends in CR is read as a
     * PASTE, and the CR becomes a newline inside the prompt instead of a
     * submit. The same text with the CR written 50 ms later submits. Short
     * bursts happened to submit, which is why the failure looked random.
     *
     * [data] must not end in CR or LF — [Reply.text] trims them — because the
     * daemon strips nothing and a trailing CR would be the paste all over
     * again, followed by a second Return.
     *
     * The flag is sent only when true, like every optional key here: `submit`
     * is `#[serde(default)]`, and a daemon older than it rejects the key
     * outright — which is exactly the daemon this builder is never used for.
     */
    fun input(id: Int, data: String, submit: Boolean): String =
        if (submit) {
            """{"cmd":"input","id":$id,"data":"${escape(data)}","submit":true}"""
        } else {
            input(id, data)
        }

    /**
     * Name a session, or clear its name (contract §1.5).
     *
     * `{"cmd":"rename","id":N,"name":"auth refactor"}`, or `"name":null` to
     * clear. **The one builder here that sends an explicit null**, and the
     * opposite rule from [revoke]'s: there an absent key means "every grant"
     * and a null would be a mistake, here the null IS the instruction —
     * "this session has no name now" — and leaving the key out would be a
     * request that says nothing. The daemon answers with the updated session
     * (`{"reply":"session", …}`), read with [readSession].
     *
     * The name is checked by [SessionNames.check] BEFORE it gets here, so the
     * person reads a sentence about a 70-character name on this screen rather
     * than a refusal from a machine somewhere else; the daemon checks again
     * with the same rule, and its check is the one that counts.
     */
    fun rename(id: Int, name: String?): String =
        if (name == null) {
            """{"cmd":"rename","id":$id,"name":null}"""
        } else {
            """{"cmd":"rename","id":$id,"name":"${escape(name)}"}"""
        }

    /**
     * The tail of a session's output, without attaching (contract §1.6).
     *
     * `{"cmd":"peek","id":N,"bytes":8192}`. The daemon reads its in-memory
     * scrollback ring — not the transcript on disk — caps [bytes] at
     * [PEEK_MAX], and answers `{"reply":"peek","data":"<base64>",…}` with the
     * PTY's size. No resize, no attach, and `attached` does not move, which is
     * what makes it safe to ask every second and a half while a screen is
     * open: the desktop's terminal is not touched.
     */
    fun peek(id: Int, bytes: Int = PEEK_MAX): String =
        """{"cmd":"peek","id":$id,"bytes":$bytes}"""

    /** The daemon's cap on one [peek], and its default. */
    const val PEEK_MAX: Int = 8192

    /**
     * Read what is on the COMPUTER's clipboard (P1-059 criterion 3, receive).
     *
     * The only verb in this vocabulary that carries something out of the
     * machine which no session produced — everything else either asks about
     * sessions or pushes into one.
     *
     * **No `id`, and that is the design and not an omission.** One Wayland
     * seat has one clipboard, so there is nothing to name. It follows that
     * this is not a session action: it must not be offered from a screen that
     * only exists while some session is waiting for input, because the moment
     * a user wants the computer's clipboard is the moment they copied
     * something on the computer, which has nothing to do with what an agent
     * is doing.
     *
     * **It is not [input] with the phone's clipboard**, which is what the
     * Paste button does and which travels the other way. It is also not
     * `rime send --clipboard` — that is ssh to another Linux host in the §20
     * registry, `wl-paste` here and `wl-copy` there. Three rounds of this app
     * called this verb missing because the similar name was read as the same
     * feature.
     *
     * A phone is allowed it. `privilege::refuse_clipboard` refuses exactly
     * two callers — a connection that IS a managed session, and one whose
     * origin could not be classified — and a paired device is neither. It is
     * deliberately NOT `inject`'s gate, which also refuses a non-local
     * origin: that would refuse the only caller this verb exists for.
     *
     * A plain control request, checked rather than assumed:
     * `rime-remoted`'s `proxy::takes_over_the_channel` is `attach` and
     * `receive` and nothing else, so unlike [receive] this rides the ordinary
     * [com.rimeos.remote.core.agent.MachineLink.request] door.
     *
     * The daemon caps the answer at 8 KiB so the reply fits one control
     * frame; past that it refuses with the limit named rather than clipping,
     * because half of what somebody copied is worse than being told to send a
     * file.
     */
    fun clipboard(): String = """{"cmd":"clipboard"}"""

    /**
     * Set the computer's clipboard to [text] (contract §1.8).
     *
     * The phone-to-computer direction, and not a session verb: it names no
     * id. Only against a daemon whose `hello.features` has
     * [Features.CLIPBOARD_WRITE].
     */
    fun clipboardWrite(text: String): String =
        """{"cmd":"clipboard_write","text":"${escape(text)}"}"""

    /**
     * Save a file from this phone into the computer's Downloads folder.
     *
     * A takeover verb like [receive], so it travels as a `Frame.Open` payload
     * ([com.rimeos.remote.core.link.Upload.save]). The daemon picks the folder
     * and the final name; [name] is only asked for.
     */
    fun saveFile(name: String, len: Long): String =
        """{"cmd":"save_file","name":"${escape(name)}","len":$len}"""

    /**
     * Hand a file to a session whose bytes are on this phone.
     *
     * `Request::Receive { id, name, len }` is the second verb that takes a
     * connection over — `attach` is the other — so it travels as the payload
     * of a `Frame.Open` and never as a control frame. The daemon answers
     * `receiving`, reads exactly [len] bytes off the channel, and answers
     * again with the `injected` reply. See [com.rimeos.remote.core.link.Upload].
     *
     * `name` is a NAME, not a path, and this app does not sanitise it: the
     * daemon's `inject::safe_name` reduces it to an alphabet with no `/`, no
     * space, no quote and no `$`, and a client that pre-reduced it would be a
     * second implementation of a rule the daemon enforces anyway — the one
     * place a difference between the two would show is a file whose real name
     * the user is then not told about. [Handoff.Files.preview] shows what the
     * daemon will make of it, and does not change what is sent.
     *
     * [len] is checked against [Handoff.Files.MAX_BYTES] before the request is
     * built, because the daemon refuses an oversize upload before the takeover
     * reply and a phone that asked anyway would have spent a round trip to be
     * told a number it already had.
     */
    fun receive(id: Int, name: String, len: Long): String =
        """{"cmd":"receive","id":$id,"name":"${escape(name)}","len":$len}"""

    /**
     * Per-worktree status for every remembered project, or for one slug.
     *
     * ## This verb EXISTS. The comment that used to stand here said it did not.
     *
     * An earlier round of this app built exactly this request, and then
     * deleted it along with its parser under the belief that
     * `rime-agent-core`'s `Request` vocabulary "is Hello, Run, List, Info,
     * Attach, Resize, Signal, Event, Logs, Remove, Prune, and the privilege
     * and secret verbs". That was read off `request.rs`'s `Verb` — the
     * *privileged operation* vocabulary — and not off `protocol.rs`'s
     * `Request`, which is the wire. `protocol.rs` has carried
     * `Worktrees { project: Option<String> }` since `473b7f60` (2026-09-08);
     * `rime-agentd/src/main.rs:906` dispatches it; and `rime-remoted`'s
     * `control()` refuses exactly one verb, `attach`, so it is forwarded.
     * See [WorktreeStatus] for the full account.
     *
     * `project` is a project **slug**, never a path. That is a security
     * property of the request and not a convenience: answering it makes the
     * daemon run git — including `merge-tree --write-tree`, which writes
     * objects — in the named directory, and every confined session has this
     * socket bound in. Keyed on a slug the daemon resolves by *searching* the
     * remembered set, the reachable directories are exactly the ones the user
     * already chose to remember. Sending a path here does not widen that; it
     * just gets a `bad_request`.
     */
    fun worktrees(project: String? = null): String =
        if (project.isNullOrEmpty()) {
            """{"cmd":"worktrees"}"""
        } else {
            """{"cmd":"worktrees","project":"${escape(project)}"}"""
        }

    /**
     * The one adapter that has no program of its own.
     *
     * `rime-agentd` decides this on the adapter's **id**, not on a capability
     * it reports: `session.rs:135` reads
     * `req.args.first().filter(|_| adapter.id == "generic")` and refuses when
     * that is empty. That is the rule the daemon actually applies, and the
     * line above this one used to end "and the daemon publishes no flag that
     * would let a client ask instead".
     *
     * **It does now.** `Request::Profiles` answers with
     * [AgentProfile.commandRequired] per adapter, derived from the compiled-in
     * adapter table, so an adapter added later that behaves the same way needs
     * no release of this app. Pass [profiles] whenever the screen has them.
     *
     * The id rule is kept as the fallback and not as the answer, because the
     * machine on the other end may be older than the verb: a daemon that
     * cannot be asked still branches on `generic`, and a client that treated
     * "no profiles listing" as "no adapter needs a command" would offer the
     * button that could only fail — which is the defect a device found.
     *
     * `Hello.agents` lists `generic` on every runtime, so a phone that offered
     * it without asking for a command offered a button that could only fail.
     * It did, until a device measured it.
     */
    fun commandIsRequired(agent: String?, profiles: List<AgentProfile> = emptyList()): Boolean {
        val stated = profiles.firstOrNull { it.agent == agent }
        return stated?.commandRequired ?: (agent == GENERIC_ADAPTER)
    }

    /** The adapter id that carries no program. */
    const val GENERIC_ADAPTER = "generic"

    /**
     * Every project the machine remembers.
     *
     * The cheap half of [worktrees], and the one a picker asks. `worktrees`
     * runs git in every remembered project — including `merge-tree
     * --write-tree`, which writes objects — and that cost is the documented
     * reason the Start screen offered a free-text directory instead of a
     * picker. This reads one small JSON record per project and runs no
     * subprocess, so a screen may ask it on open and ask `worktrees` only for
     * the project the user then chose.
     *
     * Takes no arguments. Not even an optional slug: narrowing a listing of
     * three to one saves nothing that is worth a second shape on the wire, and
     * `worktrees` already has the narrow form for the expensive question.
     */
    fun projects(): String = """{"cmd":"projects"}"""

    /**
     * Every adapter, with its program and its profile as they stand there.
     *
     * The verb that lets a Start screen say, before the tap rather than after
     * it, that an agent is not installed on that machine — [AgentProfile] has
     * the account of why each field is worth a round trip. Also carries
     * [AgentProfile.commandRequired], which this app previously derived from
     * the adapter id because nothing published it.
     */
    fun profiles(): String = """{"cmd":"profiles"}"""

    /**
     * Start a session.
     *
     * `cwd`, `cols` and `rows` are the only required fields of `RunRequest`;
     * everything else has a serde default, and sending a key with a null in it
     * is not the same as leaving it out. So optional fields are appended only
     * when they have a value.
     */
    fun run(
        cwd: String,
        cols: Int,
        rows: Int,
        agent: String? = null,
        prompt: String? = null,
        worktree: String? = null,
        checkpoint: Boolean = false,
        args: List<String> = emptyList(),
    ): String = buildString {
        append("""{"cmd":"run","cwd":"""").append(escape(cwd)).append('"')
        append(""","cols":""").append(cols)
        append(""","rows":""").append(rows)
        if (!agent.isNullOrEmpty()) append(""","agent":"""").append(escape(agent)).append('"')
        // `RunRequest.args` — "extra arguments appended after the adapter's
        // own". Sent only when there are some, like every other optional key
        // here, so a request from this phone stays the shape a daemon that
        // predates the field can parse.
        //
        // It is here because without it a phone cannot start the `generic`
        // adapter AT ALL: the daemon answers "the generic adapter needs a
        // program to run; pass one after `--`", and `args` is where that
        // program goes. The phone offered `generic` in its picker anyway,
        // because the picker is `Hello.agents` and the daemon lists it.
        // MEASURED against a real `rime-agentd` from a Pixel 7a, which is how
        // a gap between a picker and a wire builder gets noticed.
        if (args.isNotEmpty()) {
            append(""","args":[""")
            args.forEachIndexed { i, a ->
                if (i > 0) append(',')
                append('"').append(escape(a)).append('"')
            }
            append(']')
        }
        if (!prompt.isNullOrEmpty()) append(""","prompt":"""").append(escape(prompt)).append('"')
        if (!worktree.isNullOrEmpty()) append(""","worktree":"""").append(escape(worktree)).append('"')
        // Sent only when true. `RunRequest.checkpoint` is `#[serde(default)]`
        // and false is the default, so `"checkpoint":false` and the key's
        // absence mean the same thing to the daemon — but a daemon that
        // predates the field would reject the key outright, and this app runs
        // against whatever the machine has.
        if (checkpoint) append(""","checkpoint":true""")
        append('}')
    }

    // ---- approvals (P1-057) ---------------------------------------------
    //
    // Note which verb is NOT here: `decide`. See the head of `Approvals.kt` —
    // `privilege.rs:1176` refuses it from any non-local origin, before the
    // pending check, and no setting changes that. A builder for it would be a
    // builder for a request that is always refused.

    /** Every privilege request the daemon has, decided or not. */
    fun requests(): String = """{"cmd":"requests"}"""

    /** Per-project grants: project root -> grant keys. */
    fun grants(): String = """{"cmd":"grants"}"""

    /** System-access grants, each with the state the daemon computed for it. */
    fun systemGrants(): String = """{"cmd":"system_grants"}"""

    /**
     * Withdraw a per-project grant, or every grant for the project.
     *
     * Omitting `key` revokes them all — `Request::Revoke.key` is
     * `#[serde(default)]` and absent means "everything for this project". That
     * is a wide action, so the key is never omitted by accident here: a caller
     * passing null has asked for it.
     *
     * Allowed from a phone, unlike `decide`, and the asymmetry is the point:
     * `privilege.rs:1289` gates this on the caller not being a managed session
     * and applies no origin check, because revoking only ever removes
     * authority. A device that cannot grant anything can still take it back.
     */
    fun revoke(project: String, key: String? = null): String = buildString {
        append("""{"cmd":"revoke","project":"""").append(escape(project)).append('"')
        if (key != null) append(""","key":"""").append(escape(key)).append('"')
        append('}')
    }

    /** End a live system-access grant now. Allowed from a phone, as above. */
    fun revokeSystemGrant(id: Int): String =
        """{"cmd":"revoke_system_grant","id":$id}"""

    fun readRequests(reply: String): List<PrivilegeRequest> {
        val obj = require(reply, "requests")
        val array = obj["requests"] ?: return emptyList()
        return json.decodeFromJsonElement(
            ListSerializer(PrivilegeRequest.serializer()),
            array,
        )
    }

    /**
     * One request, from a `privilege_request`.
     *
     * Read from the top level like [readSession]: `Response::Request` is a
     * newtype variant under an internal tag, so `id`, `verb` and the rest sit
     * beside `"reply"` rather than under a key.
     */
    fun readRequest(reply: String): PrivilegeRequest {
        val obj = require(reply, "request")
        return json.decodeFromJsonElement(PrivilegeRequest.serializer(), obj)
    }

    fun readGrants(reply: String): Grants {
        val obj = require(reply, "grants")
        val projects = obj["projects"] ?: return emptyMap()
        return json.decodeFromJsonElement(
            MapSerializer(
                String.serializer(),
                ListSerializer(
                    String.serializer(),
                ),
            ),
            projects,
        )
    }

    /**
     * System grants, paired with the state the daemon computed for each.
     *
     * `states` arrives as `[["active","14m left"], …]` — an array of
     * two-element ARRAYS, because that is how serde serializes a Rust tuple.
     * Decoding it as objects would throw, and decoding it as a flat list of
     * strings would silently pair every grant with the wrong half.
     *
     * The two lists are the same length by the daemon's contract. A reply that
     * breaks it is not trusted into a zip: a grant with no state is shown with
     * an empty one rather than dropped, because a live root grant missing from
     * a list the user is reading is the worst outcome available here.
     */
    fun readSystemGrants(reply: String): List<Pair<SystemGrant, GrantState>> {
        val obj = require(reply, "system_grants")
        val grants = obj["grants"]?.let {
            json.decodeFromJsonElement(
                ListSerializer(SystemGrant.serializer()),
                it,
            )
        } ?: return emptyList()
        val states = obj["states"]?.let {
            json.decodeFromJsonElement(
                ListSerializer(
                    ListSerializer(
                        String.serializer(),
                    ),
                ),
                it,
            )
        } ?: emptyList()
        return grants.mapIndexed { i, g ->
            val pair = states.getOrNull(i).orEmpty()
            g to GrantState(pair.getOrElse(0) { "" }, pair.getOrElse(1) { "" })
        }
    }

    /**
     * JSON string escaping, including the one that is not about JSON.
     *
     * A newline inside a string would be `\n` in the output and therefore not
     * a real newline — but a raw control character in a path or a prompt would
     * be, and the wire refuses a control payload containing one. So control
     * characters are escaped as `\uXXXX` rather than passed through, and the
     * frame is always encodable.
     */
    fun escape(value: String): String = buildString(value.length + 8) {
        for (ch in value) {
            when {
                ch == '"' -> append("\\\"")
                ch == '\\' -> append("\\\\")
                ch == '\n' -> append("\\n")
                ch == '\r' -> append("\\r")
                ch == '\t' -> append("\\t")
                ch.code < 0x20 || ch.code == 0x7F -> append("\\u%04x".format(ch.code))
                else -> append(ch)
            }
        }
    }

    // ---- replies --------------------------------------------------------

    private fun tag(reply: String): String =
        runCatching { json.parseToJsonElement(reply) }.getOrNull()
            ?.let { (it as? JsonObject)?.get("reply")?.jsonPrimitive?.content } ?: ""

    /** Throws [AgentError] when the reply is one, and returns the object otherwise. */
    private fun require(reply: String, expected: String): JsonObject {
        val element = runCatching { json.parseToJsonElement(reply) }.getOrNull() as? JsonObject
            ?: throw AgentError("internal", "the machine sent something that is not a reply: ${reply.take(200)}")
        when (val t = element["reply"]?.jsonPrimitive?.content) {
            expected -> return element
            "error" -> throw AgentError(
                element["kind"]?.jsonPrimitive?.content ?: "internal",
                element["message"]?.jsonPrimitive?.content ?: "the runtime refused and did not say why",
            )
            else -> throw AgentError("internal", "expected a `$expected` reply and got `$t`")
        }
    }

    fun readSessions(reply: String): List<AgentSession> {
        val obj = require(reply, "sessions")
        val array = obj["sessions"] ?: return emptyList()
        return json.decodeFromJsonElement(ListSerializer(AgentSession.serializer()), array)
    }

    /**
     * One session, from an `info` or a `run`.
     *
     * The fields are read from the top level, **not** from a `"session"` key:
     * `Response::Session` is a newtype variant under an internal tag.
     */
    fun readSession(reply: String): AgentSession {
        val obj = require(reply, "session")
        return json.decodeFromJsonElement(AgentSession.serializer(), obj)
    }

    /**
     * The computer's clipboard, from a `clipboard` reply.
     *
     * **An empty string is a real answer** and means the computer's clipboard
     * is empty. `Response::Clipboard`'s own documentation says so, and the
     * caller has to keep the distinction: reporting "empty" as a failure
     * sends the user looking for a permission to grant when the machine
     * simply had nothing. Every genuine failure arrives as an [AgentError]
     * through [require] instead — not text, over the cap, no compositor, a
     * wedged application — each with a sentence the daemon wrote for a person
     * to read, which callers should show rather than paraphrase.
     *
     * Read from the top level and not from a nested object, like
     * [readSession]: `Response::Clipboard` is an internally-tagged variant.
     * A missing `text` is treated as empty rather than thrown, because the
     * one shape that could produce it is a daemon that renamed the field,
     * and "the clipboard was empty" is a better failure for that than a
     * parse error the user cannot act on.
     */
    fun readClipboard(reply: String): String =
        require(reply, "clipboard")["text"]?.jsonPrimitive?.content.orEmpty()

    /**
     * How many bytes the computer's clipboard now holds, from a
     * `clipboard_written` reply (contract §1.8). Throws [AgentError] on a
     * refusal, like every parser here.
     */
    fun readClipboardWritten(reply: String): Int =
        require(reply, "clipboard_written")["bytes"]?.jsonPrimitive?.content?.toIntOrNull() ?: 0

    /**
     * The worktree rows, from a `worktrees` reply.
     *
     * Throws [AgentError] like every other parser here, with one case given a
     * name: see [isTooOld]. The rows are returned flat, in the daemon's own
     * order, because that order is what [Project.group] reads.
     */
    fun readWorktrees(reply: String): List<WorktreeStatus> {
        val obj = require(reply, "worktrees")
        val array = obj["worktrees"] ?: return emptyList()
        return json.decodeFromJsonElement(
            ListSerializer(WorktreeStatus.serializer()),
            array,
        )
    }

    /**
     * The remembered projects.
     *
     * An empty list is a real answer — a machine where nobody has opened a
     * project yet — and arrives as `{"reply":"projects","projects":[]}`, not
     * as an error. A caller must render it as "nothing opened there yet" and
     * never as a failure; the version-skew case is the separate one
     * [isTooOld] names, and it arrives as an `AgentError`.
     */
    fun readProjects(reply: String): List<ProjectRecord> {
        val obj = require(reply, "projects")
        val array = obj["projects"] ?: return emptyList()
        return json.decodeFromJsonElement(ListSerializer(ProjectRecord.serializer()), array)
    }

    /** One row per adapter the machine can launch. See [AgentProfile]. */
    fun readProfiles(reply: String): List<AgentProfile> {
        val obj = require(reply, "profiles")
        val array = obj["profiles"] ?: return emptyList()
        return json.decodeFromJsonElement(ListSerializer(AgentProfile.serializer()), array)
    }

    /**
     * Whether this refusal is "that machine's runtime predates this verb".
     *
     * Refusal, absence and could-not-run are three different answers, and on
     * this socket two of them arrive as the same `kind`. `rime-agentd` answers
     * an unknown `cmd` from `serde_json::from_str::<Request>` failing, which
     * it reports as `bad_request` with the message `unparseable request:
     * unknown variant \`worktrees\`, expected one of …` (`main.rs:612`) — and
     * `worktrees` with a slug nothing matches *also* answers `bad_request`,
     * with `no remembered project with slug …`. A screen that showed the first
     * as "no projects" would be reporting a version skew as an empty machine.
     *
     * The `unknown variant` text is serde's, not Rime's, so this is keyed on
     * both halves: the daemon's own prefix and serde's phrase. It is only ever
     * used to choose *wording*; nothing is retried or skipped on the strength
     * of it.
     *
     * This matters today and not hypothetically: the image on this developer's
     * own machine is from 2026-09-05 and the verb landed on 2026-09-08, so
     * every `worktrees` request against it takes exactly this path.
     */
    fun isTooOld(error: AgentError): Boolean =
        error.kind == "bad_request" &&
            error.message.startsWith("unparseable request:") &&
            error.message.contains("unknown variant")

    fun readHello(reply: String): Hello {
        val obj = require(reply, "hello")
        return json.decodeFromJsonElement(Hello.serializer(), obj)
    }

    /**
     * A `peek` reply, with its bytes decoded.
     *
     * Standard padded base64, and decoded with `java.util.Base64` rather than
     * a hand-rolled decoder like [com.rimeos.remote.core.Base64Url]. That one
     * exists because the JDK's URL decoder is LAX about padding in a place
     * where a key must have exactly one spelling; this is terminal output for
     * a preview, where the JDK's basic decoder is exactly the RFC 4648 §4
     * alphabet the daemon writes and leniency costs nothing. Present on every
     * Android this app supports (API 26 and up).
     *
     * A reply whose data will not decode is an [AgentError] rather than an
     * empty preview: an empty panel would say "this agent has printed
     * nothing", which is a claim.
     */
    fun readPeek(reply: String, requested: Int = PEEK_MAX): Peek {
        val obj = require(reply, "peek")
        val text = obj["data"]?.jsonPrimitive?.content.orEmpty()
        val data = try {
            java.util.Base64.getDecoder().decode(text)
        } catch (e: IllegalArgumentException) {
            throw AgentError("internal", "the machine's preview was not base64: ${e.message}")
        }
        fun int(key: String, fallback: Int): Int =
            obj[key]?.jsonPrimitive?.content?.toIntOrNull() ?: fallback
        return Peek(
            id = int("id", -1),
            data = data,
            cols = int("cols", 80),
            rows = int("rows", 24),
            state = obj["state"]?.jsonPrimitive?.content.orEmpty(),
            requested = requested,
        )
    }

    /**
     * An `ok`, or the error it actually was.
     *
     * The parsers are named `read*` and the builders are named for their
     * verbs, because `worktrees(String)` was both at once for an hour and
     * Kotlin silently resolved a list of requests to `List<Any>`.
     */
    fun readOk(reply: String) {
        if (tag(reply) == "ok") return
        require(reply, "ok")
    }

    /**
     * Tell the machine where to wake this phone (P1-058).
     *
     * ## Not an `rime-agentd` verb, and that is on purpose
     *
     * `rime-remoted` answers this one itself, before the line reaches the
     * daemon. The daemon binds a Unix socket in a 0700 directory and has never
     * heard of a phone, a relay or a push endpoint; a verb about *how this
     * connection is reached* belongs to the process that owns the connection.
     *
     * The registration is bound to the device id the **handshake** proved, so
     * this request deliberately names no device: one could only be a way to
     * redirect somebody else's notifications.
     *
     * ## What a machine that is too old does
     *
     * It forwards the line to `rime-agentd`, which answers `bad_request` with
     * serde's "unknown variant". [isTooOld] already recognises exactly that,
     * so the caller reads it as "this machine cannot wake me; poll while I am
     * open" rather than as a failure — which is the correct outcome and the
     * reason the reply shape is the daemon's own.
     *
     * @param endpoint the URL the UnifiedPush distributor gave this phone.
     * @param key base64url of 32 bytes this phone generated. It never leaves
     *   this channel, which is already authenticated and end to end.
     */
    fun pushRegister(endpoint: String, key: String): String =
        """{"cmd":"push_register","endpoint":"${escape(endpoint)}","key":"${escape(key)}"}"""

    /** Stop this machine pushing to this phone. Idempotent at the far end. */
    fun pushUnregister(): String = """{"cmd":"push_unregister"}"""

    /**
     * Ask `rime-remoted` what it can do and where it is listening (contract
     * §2.1).
     *
     * Like [pushRegister], answered by `rime-remoted` itself and never by
     * `rime-agentd` — which is why it is NOT in `requests.json`: that fixture
     * is every request the DAEMON must parse, and the Rust half deserialises
     * each entry into agentd's `Request`. An `rime-remoted` older than the
     * verb forwards it, and agentd answers `bad_request`; [readRemoteHello]
     * is not where that is handled, `MachineLink.remoteHello` is, by reading
     * any refusal as [RemoteHello.NONE].
     */
    fun remoteHello(): String = """{"cmd":"remote_hello"}"""

    /** `{"reply":"remote",…}`, or the [AgentError] it was. */
    fun readRemoteHello(reply: String): RemoteHello {
        val obj = require(reply, "remote")
        return json.decodeFromJsonElement(RemoteHello.serializer(), obj)
    }

    /**
     * How much scrollback a phone asks for when it attaches: 64 KiB.
     *
     * It used to be 256 KiB — the daemon's whole `SCROLLBACK_BYTES`, on the
     * argument that asking for less meant reconnecting into a terminal missing
     * the part that mattered. What that cost was measured by the person using
     * it: opening a terminal took long enough to be the complaint. A replay is
     * paid on every attach and every reconnect, over a relay as often as not,
     * and the phone's screen shows perhaps sixty rows of it. 64 KiB is still
     * several hundred lines at eighty columns — far more than the screen and a
     * good way back through the scrollback — for a quarter of the bytes.
     *
     * Anybody who needs the whole history has the transcript on the machine,
     * and a session that is open on the desktop has all of it there.
     */
    const val DEFAULT_REPLAY: Int = 64 * 1024
}
