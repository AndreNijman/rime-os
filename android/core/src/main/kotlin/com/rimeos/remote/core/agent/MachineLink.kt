package com.rimeos.remote.core.agent

import com.rimeos.remote.core.FrameChannel
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Mux
import com.rimeos.remote.core.link.Upload
import com.rimeos.remote.core.phone.CompanionLink
import com.rimeos.remote.core.phone.Scope
import java.io.Closeable
import java.util.concurrent.TimeoutException
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * The control plane: one connection, used for asking a machine questions.
 *
 * ## Why it is not the PTY's connection
 *
 * Because the PTY's connection is busy being a PTY. `rime-remoted` answers a
 * control frame from a single-threaded loop that **blocks while `rime-agentd`
 * thinks** — `serve.rs` — and a privilege request legitimately waits as long
 * as the person does, which is why `Mux.CONTROL_TIMEOUT_MS` is five minutes.
 * Sharing one connection would mean a list refresh could sit behind a prompt
 * somebody has gone to lunch without answering, with the terminal frozen
 * behind it.
 *
 * There is no per-device connection cap on the far side to make this
 * expensive: `rime-remoted` opens a fresh unix connection to `rime-agentd` per
 * control round trip anyway, and `net.rs` caps message size rather than
 * connections. Checked, rather than assumed.
 *
 * ## Reconnecting is a property of asking, not of a loop
 *
 * There is no reconnect *thread* here, and that is deliberate. A dead
 * connection is noticed on the next question and replaced then — or, since
 * the phone started holding this connection open on purpose (the foreground
 * service in `:app`), by whoever is holding it: [connectNow] opens one without
 * asking anything, and [LinkEvent.Dropped] is the cue to call it again. The
 * policy of WHEN to reconnect lives with the holder, because only the holder
 * knows whether anybody still wants the connection.
 *
 * ## A dead connection is noticed within forty seconds, not five minutes
 *
 * `rime-remoted` pings every fifteen seconds from a thread of its own, so an
 * idle control connection still has traffic, and a connection with NO traffic
 * for [SILENCE_MS] is gone — a phone that walked out of Wi-Fi range does not
 * get a FIN. Before the watchdog, a request on such a connection waited out
 * [Mux.CONTROL_TIMEOUT_MS], five minutes, which is how "it takes ages" was
 * sometimes simply "it is waiting on a socket that died a while ago". The
 * watchdog closes the connection, which fails every waiting request with
 * [Disconnected] at once, so a question is retried on a fresh connection and
 * an instruction is reported as not known to have landed.
 *
 * Five minutes stays the per-request ceiling, and that is not a contradiction:
 * a request waiting on a human is on a connection whose pings are still
 * arriving, and the watchdog never touches a connection that is talking.
 *
 * ## Terminals can ride it (`mux_attach`)
 *
 * Against a machine whose `rime-remoted` advertises [Features.MUX_ATTACH], a
 * terminal opens a channel on THIS connection ([shared]) instead of dialling
 * its own — which was a full handshake, over the relay as often as not, every
 * time a terminal was opened. Such a machine answers control requests on a
 * worker so the frame loop, and the terminal's bytes, keep moving while one is
 * slow. Against any other machine the note below still holds, and terminals
 * still dial their own.
 *
 * One retry, and only for [Disconnected]: a request that reached the daemon
 * and was refused must not be sent twice. `signal` is the case that makes this
 * more than tidiness — a `SIGTERM` delivered twice because the reply was lost
 * is a second signal into whatever the agent was doing next.
 */
class MachineLink(
    /** Opens a fresh connection. May throw; the caller sees the throw. */
    private val connect: () -> FrameChannel,
    private val onEvent: (LinkEvent) -> Unit = {},
    /**
     * No frame — keepalive included — for this long means the connection is
     * gone. Zero disables the watchdog.
     */
    private val silenceMs: Long = SILENCE_MS,
    /** How often the watchdog looks. Injected so a test need not wait. */
    private val watchdogPollMs: Long = WATCHDOG_POLL_MS,
    /**
     * The scope of the key [connect] dials with. [Scope.COMPANION] for the
     * companion key: every request and channel on this link is then checked
     * against the device-feature lists before it is sent ([CompanionLink]), and a
     * refused one throws [AgentError] `not_allowed` without touching the wire.
     */
    dialled: Scope = Scope.FULL,
) : Closeable {
    /** What a caller is told about the connection underneath. */
    sealed class LinkEvent {
        data class Connected(val machine: String) : LinkEvent()

        /** The connection went; the next request will make a new one. */
        data class Dropped(val cause: Throwable?) : LinkEvent()

        /**
         * Nothing arrived for [forMs], so the connection is being treated as
         * dead. Always followed by a [Dropped].
         *
         * Its own event because it has its own cause: a connection that
         * CLOSED was closed by something, and one that went quiet was not —
         * which is the difference between "the machine went to sleep" and
         * "this phone lost its network", and a person reading a log needs to
         * know which.
         */
        data class Silent(val forMs: Long) : LinkEvent()
    }

    /** One installed connection: its multiplexer and the channel under it. */
    private class Installed(val mux: Mux, val channel: FrameChannel)

    private val lock = Any()
    private var current: Installed? = null

    /**
     * Held while a connection is being made, so that two callers who both
     * find the link down make ONE handshake between them rather than one
     * each. The loser used to connect anyway and throw its connection away —
     * a whole Noise handshake, over the relay as often as not, for nothing.
     */
    private val connecting = ReentrantLock()

    @Volatile
    private var closed = false

    /** How many connections this link has opened. The reconnect's own evidence. */
    @Volatile
    var connections: Int = 0
        private set

    /**
     * What this link may do. Starts as the dialled key's scope and can only
     * narrow: a hello that says `"scope":"companion"` makes a full-key link a
     * companion one ([CompanionLink.effective]); nothing makes it wider.
     */
    @Volatile
    var scope: Scope = dialled
        private set

    /**
     * How many requests or channels this link refused to send because its
     * scope did not allow them. The gate's own evidence: a companion link
     * whose phone side is right never refuses anything.
     */
    @Volatile
    var refused: Int = 0
        private set

    private fun guard(line: ByteArray, open: Boolean) {
        val allowed = if (open) CompanionLink.allowsOpen(scope, line) else CompanionLink.allowsRequest(scope, line)
        if (!allowed) {
            refused++
            throw CompanionLink.refusal(line)
        }
    }

    /** Whether a connection is currently up. */
    val connected: Boolean get() = synchronized(lock) { current != null }

    /** The machine's name for itself, from the live connection, or null. */
    val machine: String? get() = synchronized(lock) { current?.channel?.machine }

    // ---- the verbs ------------------------------------------------------

    fun hello(): Hello = Agentd.readHello(request(Agentd.hello()))

    /**
     * What this machine's `rime-remoted` can do, and where it is listening.
     *
     * [RemoteHello.NONE] — no features, no addresses — for a machine whose
     * `rime-remoted` predates the verb. That one forwards the line to
     * `rime-agentd`, which answers `bad_request`; ANY refusal is read the same
     * way, because every refusal means the same thing to a caller: use
     * nothing new. A connection that dropped is not a refusal and still throws.
     */
    fun remoteHello(): RemoteHello = try {
        Agentd.readRemoteHello(request(Agentd.remoteHello())).also {
            scope = CompanionLink.effective(scope, it.scope)
        }
    } catch (_: AgentError) {
        RemoteHello.NONE
    }

    /**
     * The tail of a session's output (contract §1.6). A question: retried.
     *
     * Only against a machine whose `hello` lists [Features.PEEK]; an older one
     * answers `bad_request`, which arrives as the [AgentError] it is.
     */
    fun peek(id: Int, bytes: Int = Agentd.PEEK_MAX): Peek =
        Agentd.readPeek(request(Agentd.peek(id, bytes)), bytes)

    /**
     * Name a session, or clear its name with null, and get it back.
     *
     * **Not retried**, though setting the same name twice would be harmless:
     * this is an instruction, and the rule on this class is that only
     * questions are asked twice. A caller whose connection dropped is told so
     * and can look.
     */
    fun rename(id: Int, name: String?): AgentSession =
        Agentd.readSession(request(Agentd.rename(id, name), retry = false))

    /**
     * Every session the daemon knows, in the order the Agent Center draws
     * them.
     *
     * Sorted here rather than by the screen, so that a notification, a badge
     * count and a list cannot disagree about which session is first.
     */
    fun sessions(): List<AgentSession> = Order.flat(Agentd.readSessions(request(Agentd.list())))

    /** The daemon's own order, for a caller that wants to sort differently. */
    fun sessionsUnsorted(): List<AgentSession> = Agentd.readSessions(request(Agentd.list()))

    fun info(id: Int): AgentSession = Agentd.readSession(request(Agentd.info(id)))

    /**
     * Deliver a signal, by the daemon's name for it.
     *
     * Not retried on a dropped connection — see the class note. The caller is
     * told the connection went and can decide, which is the only safe place
     * for that decision: only a person knows whether the agent they meant to
     * stop is one they mind stopping twice.
     */
    fun signal(id: Int, signal: String) = Agentd.readOk(request(Agentd.signal(id, signal), retry = false))

    fun pause(id: Int) = signal(id, "stop")

    fun resume(id: Int) = signal(id, "cont")

    /** Ask politely: `SIGTERM`. */
    fun stop(id: Int) = signal(id, "term")

    fun interrupt(id: Int) = signal(id, "int")

    /**
     * Start a session, and get it back.
     *
     * Never retried, for the obvious reason: a `run` whose reply was lost may
     * have started an agent, and a second attempt would start a second one in
     * the same directory. `cwd` must be absolute — `RunRequest` says so — and
     * that is checked here rather than discovered as a daemon error.
     */
    fun run(
        cwd: String,
        cols: Int,
        rows: Int,
        agent: String? = null,
        prompt: String? = null,
        worktree: String? = null,
        checkpoint: Boolean = false,
        /**
         * Extra arguments after the adapter's own — `RunRequest.args`.
         *
         * For `generic` this is the program, and it is not optional there: the
         * daemon refuses a `generic` session with none. See
         * [Agentd.commandIsRequired].
         */
        args: List<String> = emptyList(),
        /**
         * What `profiles` said about the adapters, when the caller has it.
         *
         * Only ever used to decide whether this adapter needs a program named
         * by the caller. Empty falls back to the id rule, which is what a
         * machine older than the `profiles` verb still needs.
         */
        profiles: List<AgentProfile> = emptyList(),
    ): AgentSession {
        require(cwd.startsWith("/")) { "a working directory must be absolute, and `$cwd` is not" }
        require(!(Agentd.commandIsRequired(agent, profiles) && args.isEmpty())) {
            "the $agent adapter runs a program you name, and none was given"
        }
        return Agentd.readSession(
            request(
                Agentd.run(cwd, cols, rows, agent, prompt, worktree, checkpoint, args),
                retry = false,
            ),
        )
    }

    /**
     * Type into a live session, without attaching.
     *
     * **Never retried**, and this is the strongest case for that on the whole
     * link: `input` is the only verb here that is not idempotent in the
     * ordinary sense. A `run` whose reply was lost may have started an agent;
     * an `input` whose reply was lost HAS put the bytes on the terminal, and a
     * retry types the user's sentence a second time — into an agent that has
     * by then acted on the first copy. Better to tell the caller the
     * connection went and let them look.
     *
     * Bytes go through [Reply.bytes], never straight from a text field: the
     * daemon appends no terminator, so a reply sent as typed is a reply the
     * agent never receives.
     */
    fun input(id: Int, data: String) =
        Agentd.readOk(request(Agentd.input(id, data), retry = false))

    /** [input], with the daemon pressing Return itself. See [Agentd.input]. */
    fun input(id: Int, data: String, submit: Boolean) =
        Agentd.readOk(request(Agentd.input(id, data, submit), retry = false))

    /**
     * Type a reply and SUBMIT it: [Reply.plan], carried out.
     *
     * [submitSupported] is whether the machine's `hello` lists
     * [Features.INPUT_SUBMIT]; the caller knows, this does not ask. Nothing
     * here is retried, for [input]'s reason.
     *
     * A plan of two requests can fail between them, and that case has its
     * own exception, [NotSubmitted], because it means something the other
     * failures do not: the words ARE on the agent's input line, and pressing
     * Return is all that is missing. Told "nothing was sent", a person would
     * send it again and the agent would get the sentence twice.
     */
    fun reply(
        id: Int,
        raw: String,
        submitSupported: Boolean,
        sleep: (Long) -> Unit = { Thread.sleep(it) },
    ) {
        var typed = false
        for (step in Reply.plan(id, raw, submitSupported)) {
            when (step) {
                is Reply.Step.Wait -> sleep(step.ms)
                is Reply.Step.Send -> try {
                    Agentd.readOk(request(step.line, retry = false))
                    typed = true
                } catch (e: Exception) {
                    if (typed) throw NotSubmitted(e)
                    throw e
                }
            }
        }
    }

    /**
     * What is on the COMPUTER's clipboard right now (P1-059 criterion 3).
     *
     * **Retried**, unlike [input] directly above it, and the contrast is the
     * point: `input` is an instruction whose replay types a user's sentence
     * twice, and this is a question. Asking twice reads the clipboard twice
     * and changes nothing at the machine. A caller who lost the reply is
     * better served by a second ask than by an error.
     *
     * Takes no session id — one seat has one clipboard — so it is a property
     * of the LINK, and the only precondition is that the link is up. It is
     * `machine`-scoped in the same way [worktrees] is.
     *
     * Blocking, so callers run it inside `withContext(Dispatchers.IO)`. It
     * cannot hang the phone for long even if the computer misbehaves: the
     * daemon bounds its own read at five seconds and kills the tool, because
     * a Wayland clipboard is served by the application that owns it and a
     * wedged application never serves it. That bound is at the machine
     * deliberately — `Mux.CONTROL_TIMEOUT_MS` is five minutes, so without it
     * a frozen editor on the computer would freeze this phone's whole
     * connection, terminal included.
     *
     * **Returns the EMPTY STRING when the computer's clipboard is empty, and
     * that is a real answer rather than a failure** — `Response::Clipboard`
     * says so in its own words. It is the one outcome callers keep getting
     * wrong: reported as an error it sends somebody hunting for a permission
     * to grant when the machine simply had nothing on it, and reported as
     * nothing at all it reads as a button that does not work.
     *
     * Every genuine refusal arrives as an [AgentError] instead — not text,
     * over the cap, no compositor, a wedged application — each carrying a
     * sentence the daemon wrote for a person to read, which callers should
     * show rather than paraphrase. `Agentd.isTooOld` names the one that means
     * the computer's runtime predates the verb rather than that it refused.
     */
    fun clipboard(): String = Agentd.readClipboard(request(Agentd.clipboard()))

    /**
     * Put [text] on the computer's clipboard ("Send to Rime", contract §1.8).
     * Retried after a drop like a read: writing the same text twice leaves the
     * same clipboard. Returns the bytes the daemon says it wrote.
     */
    fun clipboardWrite(text: String): Int =
        Agentd.readClipboardWritten(request(Agentd.clipboardWrite(text)))

    // ---- push (P1-058) ---------------------------------------------------
    //
    // The only two verbs here that `rime-agentd` never sees: `rime-remoted`
    // answers them itself, because they are about how THIS connection is
    // reached and the daemon has no concept of a transport. They travel on
    // channel zero like everything else, so a machine too old to know them
    // forwards them and answers `bad_request` — which `Agentd.isTooOld`
    // already recognises, and which callers must read as "this machine cannot
    // wake me" rather than as a failure.

    /**
     * Tell this machine where to wake this phone.
     *
     * **Not retried.** A retry can only arrive after the connection dropped,
     * and the phone will send it again on the next connection anyway — where a
     * lost reply is a free round trip rather than a registration reported as
     * failed after it landed. Nothing on a screen is waiting for this.
     */
    fun pushRegister(endpoint: String, key: String) =
        Agentd.readOk(request(Agentd.pushRegister(endpoint, key), retry = false))

    /** Stop this machine pushing to this phone. Idempotent at the far end. */
    fun pushUnregister() = Agentd.readOk(request(Agentd.pushUnregister(), retry = false))

    // ---- the companion link (contract §2.10) ------------------------------
    //
    // Full-key verbs, answered by `rime-remoted` itself. On a companion link
    // both are refused here before they are sent, as on the computer.

    /**
     * Name [publicKey] as this phone's companion key on this machine.
     *
     * Retried after a drop: the machine answers ok to the key it already has,
     * so sending the same key twice changes nothing. A key the machine has
     * retired or that another device holds answers [CompanionLink.KEY_IN_USE].
     */
    fun companionRegister(publicKey: ByteArray) = Agentd.readOk(request(CompanionLink.register(publicKey)))

    /** Retire this phone's companion key on this machine. Idempotent there. */
    fun companionForget() = Agentd.readOk(request(CompanionLink.forget()))

    /**
     * Per-worktree status for every remembered project, or for one slug.
     *
     * Retried on a dropped connection like every other question here, and this
     * one deserves the note: answering it makes the daemon run git in every
     * remembered project, `merge-tree --write-tree` included. That writes
     * objects, which sounds like an action — but it writes only unreferenced
     * ones into the object database to answer "would this merge", and the
     * answer to asking twice is the same answer. It is a question.
     *
     * It is also slow, which is why it is not folded into the Agent Center's
     * four-second poll. `Mux.CONTROL_TIMEOUT_MS` is five minutes, matching
     * `rime-remoted`'s own, so a large repository has room.
     */
    /**
     * Hand a file to a session whose bytes are on this phone (P1-059).
     *
     * **On a connection of its own**, and this is the one verb here that does
     * not travel on the connection this class holds. Two reasons, both about
     * the connection rather than about the file: `receive` TAKES A CHANNEL
     * OVER, so it cannot go through [request] at all — `rime-remoted` refuses
     * a takeover verb on channel zero by name — and a multi-megabyte upload
     * ahead of everything else in [Mux]'s strict FIFO is a session list that
     * arrives when the photo finishes. [com.rimeos.remote.core.link.Upload]
     * has the whole account.
     *
     * Blocking, like everything else here, and for longer than anything else
     * here: callers run it inside `withContext(Dispatchers.IO)`.
     */
    fun upload(
        id: Int,
        name: String,
        len: Long,
        source: () -> java.io.InputStream,
        onProgress: (Long) -> Unit = {},
    ): Upload.Landed {
        if (scope == Scope.COMPANION) {
            refused++
            throw CompanionLink.refusal("""{"cmd":"receive"}""".toByteArray())
        }
        return Upload.send(connect, id, name, len, source, onProgress)
    }

    fun worktrees(project: String? = null): List<WorktreeStatus> =
        Agentd.readWorktrees(request(Agentd.worktrees(project)))

    /** The same, grouped into the projects the daemon walked. */
    fun projects(): List<Project> = Project.group(worktrees())

    // ---- the picker verbs (P1-054) ---------------------------------------
    //
    // Deliberately NOT folded into `projects()` above, which is the expensive
    // worktree walk under a similar name. The two answer different questions:
    // `projects()` is "what is the git status of everything", which runs
    // `merge-tree --write-tree` in every remembered project; these two are
    // "what could I start an agent in, and with what", which read records and
    // stat paths. A screen that opened by calling the wrong one would put
    // seconds of git in front of a text field, which is exactly why the Start
    // screen had no picker.

    /**
     * Every project the machine remembers, most recently opened first.
     *
     * Already in that order from the daemon; [ProjectRecord.ordered] restates
     * it rather than trusting it, because the first row is a screen's default
     * selection and a silent dependence on somebody else's sort is a default
     * that changes without anyone choosing to change it.
     */
    fun projectRecords(): List<ProjectRecord> =
        ProjectRecord.ordered(Agentd.readProjects(request(Agentd.projects())))

    /** Every adapter, with its program and profile as they stand there. */
    fun profiles(): List<AgentProfile> = Agentd.readProfiles(request(Agentd.profiles()))

    // ---- approvals (P1-057) ---------------------------------------------
    //
    // There is no `decide` here and there must not be one. See `Approvals.kt`.

    fun requests(): List<PrivilegeRequest> = Agentd.readRequests(request(Agentd.requests()))

    /** Only the ones still waiting on a human — at the machine, not here. */
    fun pendingRequests(): List<PrivilegeRequest> = requests().filter { it.isPending }

    fun grants(): Grants = Agentd.readGrants(request(Agentd.grants()))

    fun systemGrants(): List<Pair<SystemGrant, GrantState>> =
        Agentd.readSystemGrants(request(Agentd.systemGrants()))

    /**
     * Withdraw a per-project grant, or all of them for the project.
     *
     * **Not retried**, and that is not tidiness. `revoke` without a key
     * removes every grant for a project, and a reply lost after the daemon
     * acted would have the retry answer `no_such_request` — reporting a
     * failure for something that succeeded, which on a screen about authority
     * is the wrong way round. The caller is told the connection went.
     */
    fun revoke(project: String, key: String? = null): Grants =
        Agentd.readGrants(request(Agentd.revoke(project, key), retry = false))

    /** End a live system-access grant now. Not retried, for the reason above. */
    fun revokeSystemGrant(id: Int): List<Pair<SystemGrant, GrantState>> =
        Agentd.readSystemGrants(request(Agentd.revokeSystemGrant(id), retry = false))

    // ---- the connection -------------------------------------------------

    /**
     * Send one request, opening or replacing the connection as needed.
     *
     * [retry] is what separates a question from an instruction. A question
     * that was lost costs nothing to ask again; an instruction that was lost
     * may or may not have been carried out, and only the caller knows whether
     * repeating it is safe.
     */
    @Throws(Disconnected::class, AgentError::class, TimeoutException::class)
    fun request(line: String, retry: Boolean = true): String {
        check(!closed) { "this link is closed" }
        val first = live()
        return try {
            first.request(line)
        } catch (e: Disconnected) {
            retire(first, e)
            if (!retry) throw e
            // One more, on a connection made for this attempt. If that fails
            // the throw reaches the caller: two dead connections in a row is
            // a machine that is not there, not a link that needs another go.
            live().request(line)
        }
    }

    /**
     * Open the connection now, without asking anything on it.
     *
     * For the holder that keeps this link up while the app is unlocked: the
     * point of being "always connected" is that the handshake has already
     * happened by the time somebody taps a machine. A no-op when a live
     * connection exists.
     */
    fun connectNow() {
        check(!closed) { "this link is closed" }
        live()
    }

    /**
     * The live multiplexer, for a terminal that rides this connection.
     *
     * Only for a machine that advertises [Features.MUX_ATTACH]; see the class
     * note. Reconnects if needed, exactly as a request would. The caller must
     * close only its own CHANNEL on it and never the multiplexer, which
     * belongs to this link.
     */
    fun shared(): Mux {
        check(!closed) { "this link is closed" }
        return live()
    }

    /**
     * Is this connection still alive, right now? Pings and waits up to
     * [timeoutMs] for any frame to come back; drops the connection if none
     * does.
     *
     * For the moment the forty-second watchdog is too slow: the phone has
     * just changed networks, or come back to the foreground. Returns true
     * without judging when it cannot judge — no connection to test is
     * `false`, a channel that cannot ping or a request already in flight is
     * `true`. The second matters: an older `rime-remoted` answers a ping from
     * the same loop that is busy with that request, so silence during one is
     * not evidence of death, and the watchdog is still watching.
     */
    fun probe(timeoutMs: Long = PROBE_MS, sleep: (Long) -> Unit = { Thread.sleep(it) }): Boolean {
        val installed = synchronized(lock) { current } ?: return false
        if (installed.mux.outstanding > 0) return true
        val before = System.nanoTime()
        val sent = runCatching { installed.channel.ping() }.getOrDefault(false)
        if (!sent) return synchronized(lock) { current === installed }
        val deadline = before + timeoutMs * 1_000_000
        while (System.nanoTime() < deadline) {
            val last = installed.channel.lastFrameNanos ?: return true
            if (last > before) return true
            if (synchronized(lock) { current !== installed }) return false
            sleep(PROBE_STEP_MS)
        }
        onEvent(LinkEvent.Silent((System.nanoTime() - before) / 1_000_000))
        retire(installed.mux, Disconnected("${installed.channel.machine} did not answer a ping"))
        return false
    }

    /** Close the current connection, if any. The next request opens another. */
    fun disconnect() {
        synchronized(lock) { current }?.let { retire(it.mux, null) }
    }

    private fun live(): Mux {
        synchronized(lock) { current }?.let { found ->
            if (!stale(found)) return found.mux
            onEvent(LinkEvent.Silent(idleMs(found)))
            retire(found.mux, Disconnected("nothing arrived from ${found.channel.machine} for ${idleMs(found)}ms"))
        }
        return connecting.withLock {
            // Somebody else may have connected while this caller waited for
            // the lock; theirs is used rather than a second handshake.
            synchronized(lock) { current }?.let { return@withLock it.mux }
            if (closed) throw Disconnected("this link is closed")
            // Outside [lock]: opening a connection is a handshake over a
            // socket, and holding the monitor across it would block every
            // other caller, including the one trying to close this link.
            val channel = connect()
            val listener = Listener()
            val m = Mux(channel, listener, guard = ::guard)
            listener.mux = m
            val thread = Thread({ m.pump() }, "rime-link-${channel.machine}")
            thread.isDaemon = true
            val installed = synchronized(lock) {
                if (closed || current != null) {
                    null
                } else {
                    Installed(m, channel).also {
                        current = it
                        connections++
                    }
                }
            }
            if (installed == null) {
                // The link closed while this one was connecting. The
                // connection is closed rather than leaked.
                runCatching { m.close() }
                runCatching { channel.close() }
                synchronized(lock) { current }?.let { return@withLock it.mux }
                throw Disconnected("this link to ${channel.machine} closed while connecting")
            }
            thread.start()
            watch(installed)
            onEvent(LinkEvent.Connected(channel.machine))
            m
        }
    }

    private fun idleMs(installed: Installed): Long {
        val last = installed.channel.lastFrameNanos ?: return 0
        return (System.nanoTime() - last) / 1_000_000
    }

    private fun stale(installed: Installed): Boolean =
        silenceMs > 0 && installed.channel.lastFrameNanos != null && idleMs(installed) >= silenceMs

    /**
     * Watch one connection for silence, until it is no longer the current one.
     *
     * One thread per connection, daemon, polling. `lastFrameNanos` is written
     * by the channel's reader for every frame, pings included, so this is the
     * question "has ANY frame arrived", which is the one that tells an idle
     * connection from a dead one — see [FrameChannel.lastFrameNanos].
     */
    private fun watch(installed: Installed) {
        if (silenceMs <= 0 || installed.channel.lastFrameNanos == null) return
        val thread = Thread({
            try {
                while (synchronized(lock) { current === installed }) {
                    Thread.sleep(watchdogPollMs)
                    if (synchronized(lock) { current !== installed }) return@Thread
                    if (stale(installed)) {
                        onEvent(LinkEvent.Silent(idleMs(installed)))
                        // Closing is what fails the waiting requests and wakes
                        // the pump; nothing here reconnects.
                        retire(
                            installed.mux,
                            Disconnected("nothing arrived from ${installed.channel.machine} for ${idleMs(installed)}ms"),
                        )
                        return@Thread
                    }
                }
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }, "rime-link-watchdog-${installed.channel.machine}")
        thread.isDaemon = true
        thread.start()
    }

    /**
     * Retire [m] if it is still the current connection, and say so once.
     *
     * Keyed on the multiplexer rather than on "whatever is current", which is
     * the bug the older `drop` had in waiting: a late disconnect from a
     * connection that had ALREADY been replaced would have cleared its
     * replacement.
     */
    private fun retire(m: Mux, cause: Throwable?) {
        val going = synchronized(lock) {
            val c = current ?: return
            if (c.mux !== m) return
            current = null
            c
        }
        runCatching { going.mux.close() }
        onEvent(LinkEvent.Dropped(cause))
    }

    private inner class Listener : Mux.Listener {
        /** Set straight after the multiplexer is built, before its pump starts. */
        @Volatile
        var mux: Mux? = null

        // Channels opened on this connection carry listeners of their own (a
        // terminal riding it, `mux_attach`), and the multiplexer hands their
        // data and close to those. Anything arriving here is for a channel
        // nobody is listening to any more, which is a late frame for a
        // terminal that has gone, and dropping it is right.
        override fun onData(channel: UInt, bytes: ByteArray) = Unit

        override fun onClose(channel: UInt, reason: String) = Unit

        override fun onDisconnect(cause: Throwable?) {
            // The connection ended on its own — the machine went away, or
            // slept. Cleared here so the next request opens a new one rather
            // than failing against a corpse.
            mux?.let { retire(it, cause) }
        }
    }

    override fun close() {
        closed = true
        synchronized(lock) { current }?.let { retire(it.mux, null) }
    }

    companion object {
        /**
         * Forty seconds: two and a half of the desktop's fifteen-second pings.
         *
         * Shorter than the terminal's forty-five (`PtyAttachment`), because a
         * control connection that has gone is one every screen is waiting on,
         * and longer than two pings so a single late one on a busy machine or
         * a slow hop is not a reconnect.
         */
        const val SILENCE_MS: Long = 40_000

        /** Often enough that the answer is within a ping of the truth. */
        const val WATCHDOG_POLL_MS: Long = 2_000

        /** How long [probe] waits for a pong. A round trip, generously. */
        const val PROBE_MS: Long = 3_000

        private const val PROBE_STEP_MS: Long = 25
    }
}

/**
 * A reply was typed on the agent's terminal and the Return that submits it
 * did not arrive — the second request of the fallback plan failed.
 *
 * Its own type because the person's next move is different: the words are on
 * the agent's input line, so sending the reply again would put them there
 * twice. What is needed is a bare Return, from the reply box or the terminal.
 */
class NotSubmitted(cause: Throwable) : Exception(
    "the reply was typed but the Return that submits it did not arrive (${cause.message})",
    cause,
)
