package com.rimeos.remote.core.link

import com.rimeos.remote.core.Frame
import com.rimeos.remote.core.FrameChannel
import java.io.Closeable
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * One connection to a machine, with several conversations on it.
 *
 * ## The ordering rule, which is the whole of this file
 *
 * `rime-remoted` answers a `Control` frame with a `Control` frame, and it does
 * so **from a single-threaded loop that blocks while `rime-agentd` thinks**
 * (`serve.rs`: `Frame::Control(line) => { let reply = control(...); send(...) }`).
 * There is no request id on the wire. So the only thing that pairs a reply
 * with its request is that they are in the same order, and a client that let
 * two requests be outstanding at once and matched them by arrival is a client
 * that shows one session's details under another's name.
 *
 * Hence: a strict FIFO of outstanding requests, and replies resolve the head.
 *
 * ## `Open` is answered on channel zero, and sometimes not at all
 *
 * An `Open` gets its answer as a `Control` frame too — the daemon's
 * `{"reply":"attached","id":N}` — which is why it takes a place in the same
 * queue. But the desktop has two failure shapes and only one of them sends a
 * reply:
 *
 * * the daemon said something other than `attached` — `Control(reply)` then
 *   `Close(channel, "")`;
 * * anything else went wrong — **only** `Close(channel, reason)`.
 *
 * A queue that waited for a `Control` per `Open` would therefore hand the
 * *next* request's reply to the failed attach, and every reply after that
 * would be one behind. So a `Close` naming the channel of an unanswered
 * `Open` at the head resolves it, as a failure carrying the reason.
 *
 * ## Several terminals, one connection (`mux_attach`)
 *
 * A channel opened with its own [Listener] gets that channel's `Data` and
 * `Close` delivered to it rather than to the connection's listener, and is
 * told when the connection ends. That is what lets a terminal ride the
 * control connection instead of dialling one of its own — which a machine
 * whose `rime-remoted` advertises `mux_attach` has said is safe, because its
 * control requests no longer block the frame loop. Channel ids come from
 * [nextChannelId] so two terminals on one connection cannot collide.
 *
 * ## Why there is no locking around the reader
 *
 * One thread calls [pump] and it is the only caller of [FrameChannel.receive].
 * Everything else it touches is either a concurrent structure or guarded by
 * [lock]. Sending is safe from any thread because the channel serialises it.
 */
class Mux(
    private val channel: FrameChannel,
    private val listener: Listener,
    /**
     * Whether a reply to an `Open` means the channel is now carrying a PTY.
     *
     * A parameter because the daemon answers an `Open` with either
     * `{"reply":"attached",…}` or an error, and a channel recorded as open on
     * an error is a channel whose trailing `Close("")` would then be reported
     * as "the session ended" — a refusal dressed up as a bereavement.
     */
    private val accepted: (ByteArray) -> Boolean = ::attachAccepted,
) : Closeable {
    /** What the owner of a [Mux] is told about the channels on it. */
    interface Listener {
        /** Terminal bytes for an open channel. */
        fun onData(channel: UInt, bytes: ByteArray)

        /** A channel ended. [reason] is empty for an ordinary close. */
        fun onClose(channel: UInt, reason: String)

        /**
         * The connection itself ended.
         *
         * Always called exactly once, whatever ended it, including [close].
         * A caller that reconnects hangs its retry off this.
         */
        fun onDisconnect(cause: Throwable?)
    }

    private class Pending(
        /** Non-null when this is an `Open`, so a `Close` can resolve it. */
        val channel: UInt?,
        val future: CompletableFuture<ByteArray>,
        /** For an `Open`: which reply means the channel is now open. */
        val accepted: ((ByteArray) -> Boolean)? = null,
    )

    private val lock = Any()

    /**
     * Held across enqueue-and-send, so that the order requests are queued in
     * IS the order they reach the wire.
     *
     * Without it two threads could queue A then B and write B then A, and the
     * daemon — which answers in wire order — would hand B's reply to A. That
     * was rare while one screen asked one question at a time. It is the norm
     * now that the phone holds its connection open: the hub asks `hello` and
     * `remote_hello` together, the session screen peeks every second and a
     * half, the list polls every four, and a terminal may open a channel on
     * the same connection. A swap there is one session's output shown under
     * another's name, or every feature silently read as absent.
     *
     * Separate from [lock] because a send can block on a slow socket, and the
     * pump thread takes [lock] for every frame it dispatches.
     */
    private val sendOrder = Any()
    private val pending = ArrayDeque<Pending>()
    private val open = HashSet<UInt>()

    /** Channels whose traffic goes to a listener of their own. See the class note. */
    private val channelListeners = HashMap<UInt, Listener>()

    private val nextChannel = java.util.concurrent.atomic.AtomicInteger(1)

    @Volatile
    private var closed = false

    @Volatile
    private var disconnected = false

    val machine: String get() = channel.machine

    /**
     * Read frames until the connection ends.
     *
     * Blocking, and meant to own a thread. It returns rather than throwing:
     * the end of a connection is the normal way this finishes, and the cause
     * reaches the caller through [Listener.onDisconnect] where a reconnect
     * loop is already listening.
     */
    fun pump() {
        var cause: Throwable? = null
        try {
            while (!closed) {
                dispatch(channel.receive())
            }
        } catch (e: Throwable) {
            // Not swallowed: `closed` distinguishes "we hung up" from "it
            // died", and only the second is a cause worth reporting.
            if (!closed) cause = e
        } finally {
            finish(cause)
        }
    }

    private fun dispatch(frame: Frame) {
        when (frame) {
            is Frame.Control -> resolveHead(frame.line, null)
            is Frame.Data -> {
                val to = synchronized(lock) {
                    if (frame.channel in open) channelListeners[frame.channel] ?: listener else null
                }
                to?.onData(frame.channel, frame.bytes)
            }
            is Frame.Close -> {
                // A close that answers an unanswered `Open` is the desktop's
                // "it went wrong and there is no daemon reply" path.
                if (!resolveHead(null, frame)) {
                    val to = synchronized(lock) {
                        open.remove(frame.channel)
                        channelListeners.remove(frame.channel) ?: listener
                    }
                    to.onClose(frame.channel, frame.reason)
                }
            }
            // `Open` never arrives at a client: the desktop opens nothing.
            // Ping and Pong are handled inside the channel and never surface.
            else -> Unit
        }
    }

    /**
     * Hand a reply to the head of the queue.
     *
     * Returns whether it was consumed, which only matters for the [Frame.Close]
     * case: a close that resolved an `Open` must not also be reported as a
     * channel ending, because no channel was ever opened.
     */
    private fun resolveHead(reply: ByteArray?, close: Frame.Close?): Boolean {
        val head = synchronized(lock) {
            if (close != null) {
                // The unanswered `Open` for this channel, wherever it is in
                // the queue — not only at the head. A desktop in order puts it
                // at the head anyway; one that is not must still not leave an
                // `Open` waiting for a `Control` that will never come, which
                // would hand the NEXT reply to it and every reply after that
                // to the request before its own.
                val at = pending.indexOfFirst { it.channel == close.channel }
                if (at < 0) return false
                channelListeners.remove(close.channel)
                return@synchronized pending.removeAt(at)
            }
            val head = pending.firstOrNull() ?: return false
            pending.removeFirst()
            // Recorded on the pump thread and not by the caller, because the
            // desktop sends the scrollback immediately after the reply: the
            // first `Data` frame can arrive before the caller has woken up.
            if (reply != null && head.channel != null) {
                if ((head.accepted ?: accepted)(reply)) {
                    open.add(head.channel)
                } else {
                    // Refused: the `Close("")` that follows belongs to nobody,
                    // and a terminal told "your channel closed" for a channel
                    // that never opened would report a refusal as the session
                    // ending.
                    channelListeners.remove(head.channel)
                }
            }
            head
        }
        if (reply != null) {
            head.future.complete(reply)
        } else if (close != null) {
            head.future.completeExceptionally(
                ChannelRefused(
                    if (close.reason.isEmpty()) {
                        "the machine closed the channel without saying why"
                    } else {
                        close.reason
                    },
                ),
            )
        }
        return true
    }

    private fun finish(cause: Throwable?) {
        val (waiting, channels) = synchronized(lock) {
            if (disconnected) return
            disconnected = true
            val copy = pending.toList()
            pending.clear()
            open.clear()
            val listeners = channelListeners.values.toList()
            channelListeners.clear()
            copy to listeners
        }
        val ended = cause ?: Disconnected("the connection to ${channel.machine} ended")
        // Every outstanding request fails, and none is left to time out on its
        // own. A caller blocked on a reply that can never come is the shape of
        // bug that looks like a slow network for thirty seconds.
        for (p in waiting) p.future.completeExceptionally(ended)
        // Every terminal riding this connection hears that it went, before
        // the connection's owner does: the owner may reconnect at once, and a
        // terminal still waiting on the old connection would then never learn
        // it had to re-attach.
        for (l in channels) runCatching { l.onDisconnect(cause) }
        listener.onDisconnect(cause)
    }

    /** How many requests are waiting for a reply. */
    val outstanding: Int get() = synchronized(lock) { pending.size }

    /**
     * A channel id nobody on this connection has used.
     *
     * From 1, because 0 is the control channel, and never reused within one
     * connection: a late `Close` for a terminal that has gone must not land
     * on the next one.
     */
    fun nextChannelId(): UInt = nextChannel.getAndIncrement().toUInt()

    /**
     * Send one `rime-agentd` request and wait for its reply.
     *
     * The deadline is generous by default because the daemon's own is: a
     * request that reaches a human — a privilege prompt — legitimately takes
     * as long as the person does, and `rime-remoted` waits five minutes for
     * exactly that. A phone that gave up after ten seconds would report a
     * failure while a dialog was still on screen.
     */
    @Throws(Disconnected::class, TimeoutException::class)
    fun request(line: ByteArray, timeoutMs: Long = CONTROL_TIMEOUT_MS): ByteArray {
        val future = enqueueAndSend(null, Frame.Control(line))
        return await(future, timeoutMs)
    }

    fun request(line: String, timeoutMs: Long = CONTROL_TIMEOUT_MS): String =
        request(line.toByteArray(Charsets.UTF_8), timeoutMs).toString(Charsets.UTF_8)

    /**
     * Open a PTY channel, and return the daemon's reply to the `attach`.
     *
     * The reply is returned rather than interpreted: "no such session 4" is
     * the daemon's sentence and belongs on the screen in its own words.
     * A channel is only recorded as open when a reply arrived, so a refused
     * attach leaves nothing behind.
     */
    @Throws(Disconnected::class, ChannelRefused::class, TimeoutException::class)
    fun openChannel(
        id: UInt,
        attach: ByteArray,
        timeoutMs: Long = CONTROL_TIMEOUT_MS,
        /**
         * Where this channel's `Data` and `Close` go, instead of the
         * connection's listener. Registered BEFORE the `Open` is sent, because
         * the scrollback follows the reply immediately and is dispatched on
         * the pump thread — possibly before this call has returned.
         */
        channelListener: Listener? = null,
        /**
         * Which reply means this channel is open, when it is not a PTY.
         *
         * A per-channel rule rather than a looser default: a nex channel opens
         * with `{"reply":"streaming"}`, and a terminal that took "streaming"
         * for "attached" would draw whatever followed as terminal output.
         * Null keeps this connection's own rule.
         */
        accepted: ((ByteArray) -> Boolean)? = null,
    ): ByteArray {
        require(id != Frame.CONTROL_CHANNEL) { "channel 0 is the control channel" }
        if (channelListener != null) synchronized(lock) { channelListeners[id] = channelListener }
        try {
            val future = enqueueAndSend(id, Frame.Open(id, attach), accepted)
            return await(future, timeoutMs)
        } catch (e: Throwable) {
            synchronized(lock) { channelListeners.remove(id) }
            throw e
        }
    }

    /**
     * Queue a reply slot and write the frame that asks for it, as one step.
     * See [sendOrder].
     *
     * A write that FAILS takes its slot back out of the queue. Left in, it
     * would be a head no reply is coming for, and the next request's reply
     * would resolve it — every reply after that one behind. (The connection
     * is almost certainly going anyway; this makes that the only outcome.)
     */
    private fun enqueueAndSend(
        forChannel: UInt?,
        frame: Frame,
        accepted: ((ByteArray) -> Boolean)? = null,
    ): CompletableFuture<ByteArray> {
        val future = CompletableFuture<ByteArray>()
        val slot = Pending(forChannel, future, accepted)
        synchronized(sendOrder) {
            synchronized(lock) {
                if (disconnected || closed) {
                    throw Disconnected("this connection to ${channel.machine} is closed")
                }
                pending.addLast(slot)
            }
            try {
                channel.send(frame)
            } catch (e: Throwable) {
                synchronized(lock) { pending.remove(slot) }
                throw e
            }
        }
        return future
    }

    private fun await(future: CompletableFuture<ByteArray>, timeoutMs: Long): ByteArray = try {
        future.get(timeoutMs, TimeUnit.MILLISECONDS)
    } catch (e: java.util.concurrent.ExecutionException) {
        when (val cause = e.cause) {
            is ChannelRefused -> throw cause
            is Disconnected -> throw cause
            else -> throw Disconnected(cause?.message ?: "the connection ended", cause)
        }
    } catch (e: java.util.concurrent.TimeoutException) {
        // The queue is FIFO and this request is no longer in a state anybody
        // can reason about: a reply that arrives later would be handed to the
        // NEXT request. So the connection goes, which is the honest response
        // to having lost track of where in the conversation we are.
        close()
        throw TimeoutException("${channel.machine} did not answer within ${timeoutMs}ms")
    }

    /** Terminal bytes to an open channel. */
    fun send(id: UInt, bytes: ByteArray) {
        if (bytes.isEmpty()) return
        channel.sendData(id, bytes)
    }

    /** Detach: the session keeps running on the machine, which is the point. */
    fun closeChannel(id: UInt, reason: String = "") {
        synchronized(lock) {
            open.remove(id)
            channelListeners.remove(id)
        }
        runCatching { channel.send(Frame.Close(id, reason)) }
    }

    override fun close() {
        closed = true
        runCatching { channel.close() }
        // Closing the stream is what wakes the pump, which then calls
        // `finish`. But a `Mux` closed before `pump` ever ran would leave
        // callers waiting forever, so the outstanding requests are failed here
        // as well; `finish` is idempotent.
        finish(null)
    }

    companion object {
        /**
         * Five minutes, matching `rime-remoted`'s `CONTROL_TIMEOUT`.
         *
         * Not a guess: the proxy on the far side sets exactly this, for the
         * requests `Request::waits_on_a_human` names. Timing out sooner than
         * the machine does would abandon a request that was still being
         * decided.
         */
        const val CONTROL_TIMEOUT_MS: Long = 300_000
    }
}

/**
 * `rime-agentd` said `attached`, and therefore this channel is a PTY.
 *
 * Read out of the text rather than parsed: key order is not fixed on the wire,
 * so the two halves are looked for independently, and `:core` has no business
 * knowing the reply's full shape.
 */
fun attachAccepted(reply: ByteArray): Boolean {
    val text = reply.toString(Charsets.UTF_8)
    return text.contains("\"reply\"") && text.contains("\"attached\"")
}

/** The connection ended, with or without a reason. */
class Disconnected(message: String, cause: Throwable? = null) : Exception(message, cause)

/** The machine refused to open a channel, in its own words. */
class ChannelRefused(message: String) : Exception(message)
