package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Mux
import kotlinx.serialization.json.JsonElement
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * nex over one machine's control connection.
 *
 * [shared] is whether that machine advertises `mux_attach`: only then may a
 * channel ride the control connection (see [MachineLink]'s class note), and so
 * only then are there one-shot channels for large answers and a live feed.
 * Without it every call is a control round trip and a screen polls.
 *
 * ## Retrying is safe here, and that is not an accident
 *
 * [call] lets [MachineLink.request] send a request again after a dropped
 * connection, which that class refuses for an instruction. A nex read is a
 * question. A nex write carries the `request_id` its [NexCall] was made with,
 * and nexd answers a key it has seen with the first answer instead of acting
 * again. So the second send is the same request, not a second one.
 */
class NexClient(
    private val link: MachineLink,
    private val shared: Boolean,
    private val oneShotTimeoutMs: Long = ONE_SHOT_TIMEOUT_MS,
) {
    /** Whether [subscribe] can work against this machine. */
    val live: Boolean get() = shared

    /**
     * One request. An answer too big for a control frame is fetched again as a
     * channel, with the same line, so a write is still one write.
     */
    fun call(call: NexCall): JsonElement {
        val reply = link.request(call.line, retry = true)
        return try {
            Nex.result(reply)
        } catch (e: AgentError) {
            if (e.kind == "too_large" && shared) oneShot(call) else throw e
        }
    }

    /**
     * A request whose answer is usually large — a transcript, a diff — asked
     * on a channel from the start when the machine allows one.
     */
    fun large(call: NexCall): JsonElement = if (shared) {
        try {
            oneShot(call)
        } catch (e: Disconnected) {
            // As [MachineLink.request] does: one more, on a fresh connection.
            oneShot(call)
        }
    } else {
        call(call)
    }

    private fun oneShot(call: NexCall): JsonElement {
        val mux = link.shared()
        val id = mux.nextChannelId()
        val body = ByteArrayOutputStream()
        val done = CompletableFuture<Unit>()
        val listener = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                synchronized(body) {
                    if (body.size() + bytes.size > NexFeed.MAX_LINE) {
                        done.completeExceptionally(AgentError("too_large", "the answer from nex was too large to show"))
                        mux.closeChannel(id)
                        return
                    }
                    body.write(bytes)
                }
            }

            override fun onClose(channel: UInt, reason: String) {
                if (reason.isEmpty()) done.complete(Unit) else done.completeExceptionally(Disconnected(reason))
            }

            override fun onDisconnect(cause: Throwable?) {
                done.completeExceptionally(cause ?: Disconnected("the connection ended"))
            }
        }
        val reply = mux.openChannel(
            id,
            call.line.toByteArray(Charsets.UTF_8),
            channelListener = listener,
            accepted = Nex::streaming,
        )
        if (!Nex.streaming(reply)) {
            // The bridge's own refusal (`unavailable`, `bad_request`): thrown as one.
            Nex.result(reply.toString(Charsets.UTF_8))
            throw AgentError("internal", "the nex channel did not open")
        }
        try {
            done.get(oneShotTimeoutMs, TimeUnit.MILLISECONDS)
        } catch (e: ExecutionException) {
            throw e.cause ?: e
        } catch (e: TimeoutException) {
            // This channel only: the connection under it carries terminals too.
            mux.closeChannel(id)
            throw TimeoutException("nex did not finish answering within ${oneShotTimeoutMs / 1000} s")
        }
        return oneShotResult(synchronized(body) { body.toByteArray() })
    }

    /** Told about the feed. Called on the connection's reader thread: hand off, do not block. */
    interface FeedListener {
        /** [startedNow] is true for the batch that carried the feed's first line. */
        fun onEvents(events: List<NexEvent>, startedNow: Boolean)

        /** The feed ended by itself. Not called after [Subscription.close]. */
        fun onEnd(cause: Throwable?)
    }

    /** An open feed. [lastGseq] is where to resume a new one from. */
    class Subscription internal constructor(private val mux: Mux, private val id: UInt, private val feed: NexFeed) :
        Closeable {
        val lastGseq: Long get() = synchronized(feed) { feed.lastGseq }

        override fun close() = mux.closeChannel(id)
    }

    /**
     * nex's global feed, from after [afterGseq], or from now when it is null.
     *
     * Opening from the start of time is not offered: the backlog is every
     * event nex ever journalled, and a phone wants what changes from here.
     */
    fun subscribe(afterGseq: Long?, listener: FeedListener): Subscription {
        check(shared) { "this machine cannot carry a nex feed on its control connection" }
        val mux = link.shared()
        val id = mux.nextChannelId()
        val feed = NexFeed(after = afterGseq ?: 0)
        val ended = java.util.concurrent.atomic.AtomicBoolean(false)
        fun end(cause: Throwable?) {
            if (ended.compareAndSet(false, true)) listener.onEnd(cause)
        }
        val channelListener = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                val batch = try {
                    synchronized(feed) { feed.feed(bytes) }
                } catch (e: NexFeedBroken) {
                    mux.closeChannel(id)
                    end(e)
                    return
                }
                if (batch.events.isNotEmpty() || batch.startedNow) listener.onEvents(batch.events, batch.startedNow)
            }

            override fun onClose(channel: UInt, reason: String) =
                end(Disconnected(reason.ifEmpty { "the machine closed the nex feed" }))

            override fun onDisconnect(cause: Throwable?) = end(cause ?: Disconnected("the connection ended"))
        }
        val reply = mux.openChannel(
            id,
            Nex.events(afterGseq, tail = afterGseq == null).toByteArray(Charsets.UTF_8),
            channelListener = channelListener,
            accepted = Nex::streaming,
        )
        if (!Nex.streaming(reply)) {
            Nex.result(reply.toString(Charsets.UTF_8))
            throw AgentError("internal", "the nex feed did not open")
        }
        return Subscription(mux, id, feed)
    }

    companion object {
        /** A transcript is read from nex's journal; this is generous for one. */
        const val ONE_SHOT_TIMEOUT_MS: Long = 60_000
    }
}
