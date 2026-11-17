package com.rimeos.remote.core.link

import com.rimeos.remote.core.Frame
import com.rimeos.remote.core.FrameChannel
import com.rimeos.remote.core.agent.Agentd
import com.rimeos.remote.core.agent.Handoff
import java.io.InputStream

/**
 * One file, from this phone to a running agent (P1-059 criterion 2).
 *
 * ## Why this is a channel and not a request
 *
 * `Request::Receive` is the second verb on this protocol that takes a
 * connection over; `attach` is the other. The daemon answers `receiving`, then
 * reads exactly `len` bytes off the same connection, then answers again with
 * the `injected` reply. So it travels as a `Frame.Open` payload — and
 * `rime-remoted` refuses it on channel zero by name, for the same reason it
 * refuses `attach` there: the daemon would be blocked reading an upload that
 * never comes while the proxy blocks reading a reply that never comes.
 *
 * ## Why it gets a connection of its own
 *
 * [Mux] is strictly FIFO on the control channel — `rime-remoted` answers from
 * a single-threaded loop and there is no request id on the wire, so the only
 * thing pairing a reply with its request is arrival order. That ordering is
 * fine for an upload's own two replies. What is not fine is what an upload
 * does to everything ELSE on that connection: two megabytes of `Data` frames
 * ahead of a `list` is a session list that arrives when the photo finishes.
 * `PtyAttachment` already takes a connection per terminal for a related
 * reason, so this is the existing shape rather than a new one.
 *
 * ## The bytes, and the one number that is checked twice
 *
 * `len` is declared before anything is sent and the daemon commits to it in
 * the reply — it refuses an oversize upload BEFORE the takeover, so nothing
 * crosses a mobile connection to be discarded. This class checks the same cap
 * first ([Handoff.Files.MAX_BYTES]) so a phone does not spend a round trip
 * learning a number it already had, and then sends exactly that many bytes: a
 * source that turns out to be longer is a refusal, not a truncation, because a
 * truncated screenshot that the agent is told is a screenshot is worse than
 * an error.
 *
 * ## What it does NOT do
 *
 * It does not name the destination, reduce the file's name, or decide what is
 * typed into the terminal. All three are the daemon's, and a second
 * implementation here would be a second thing to keep in step —
 * [Handoff.Files.preview] shows the user what the daemon will do without
 * being what does it.
 *
 * ## Threading
 *
 * Every method here blocks on a socket, so every one of them is a
 * `NetworkOnMainThreadException` waiting to happen: Android installs
 * `StrictMode.enableDeathOnNetwork()` for any app targeting API 11 or later
 * and a debug build does not relax it. Callers run this inside
 * `withContext(Dispatchers.IO)`. It is not a `suspend` function because
 * `:core` has no coroutines dependency and the rest of the link layer is
 * blocking for the same reason.
 */
object Upload {

    /** What came back, once the daemon had every byte. */
    data class Landed(
        /** The session the file was handed to. */
        val session: Int,
        /**
         * Where the agent will find it, and — verbatim — the text typed into
         * its terminal. One field because the daemon sends one: a caller
         * cannot be shown a path different from the one the agent was given.
         */
        val path: String,
        /** Whether the daemon wrapped the path as a bracketed paste. */
        val bracketed: Boolean,
    )

    /**
     * What came back from a `save_file`: the file is in the computer's
     * Downloads folder (contract §1.8).
     */
    data class Saved(
        /** The name it was saved under, numbered when another file had it. */
        val name: String,
        /** The full path on the computer, for the screen to show. */
        val path: String,
    )

    /** The machine refused, in its own words. */
    class Refused(message: String) : Exception(message)

    /**
     * The channel an upload opens.
     *
     * Not [PtyAttachment.DEFAULT_CHANNEL]'s neighbour by accident: this
     * connection carries nothing else, so the number only has to be something
     * other than zero, which the control channel owns.
     */
    const val CHANNEL: UInt = 1u

    /**
     * `rime-agentd` said `receiving`, so this channel is now a byte sink.
     *
     * The counterpart of [attachAccepted], read the same way — out of the
     * text, because key order is not fixed on the wire and `:core` has no
     * business knowing the reply's full shape.
     */
    fun receivingAccepted(reply: ByteArray): Boolean {
        val text = reply.toString(Charsets.UTF_8)
        return text.contains("\"reply\"") && text.contains("\"receiving\"")
    }

    /** `rime-agentd` said `saving`: the `save_file` counterpart of [receivingAccepted]. */
    fun savingAccepted(reply: ByteArray): Boolean {
        val text = reply.toString(Charsets.UTF_8)
        return text.contains("\"reply\"") && text.contains("\"saving\"")
    }

    /**
     * The largest file "Send to Rime" sends, the daemon's `MAX_SAVE_BYTES`.
     * Checked here for the same reason as [Handoff.Files.MAX_BYTES]: a phone
     * should not pay a round trip to learn a number it already has.
     */
    const val SAVE_MAX_BYTES: Long = 256L * 1024 * 1024

    /**
     * Send [len] bytes read from [source] to session [id] as [name].
     *
     * [connect] opens the connection this upload owns; it is closed before
     * this returns, whatever happened. [onProgress] is called with the number
     * of bytes sent so far, on this thread.
     *
     * @throws Refused when the machine answered anything but `receiving`, or
     *   refused after the bytes arrived.
     * @throws Disconnected when the connection ended mid-upload.
     * @throws IllegalArgumentException when [len] is past the cap or zero.
     */
    @Throws(Refused::class, Disconnected::class)
    fun send(
        connect: () -> FrameChannel,
        id: Int,
        name: String,
        len: Long,
        source: () -> InputStream,
        onProgress: (Long) -> Unit = {},
    ): Landed {
        require(len > 0) { "an upload of no bytes is not a file" }
        require(len <= Handoff.Files.MAX_BYTES) {
            "$len bytes is past the ${Handoff.Files.MAX_BYTES}-byte limit for handing a file " +
                "to a session"
        }

        return transfer(connect, Agentd.receive(id, name, len), ::receivingAccepted, len, source, onProgress, ::read)
    }

    /**
     * Save [len] bytes read from [source] into the computer's Downloads folder
     * as [name] ("Send to Rime", contract §1.8).
     *
     * The same connection, channel and byte loop as [send]; only the request
     * and its two replies differ. The name crosses as the share sheet gave
     * it: the daemon reduces it and numbers it on a clash, and the [Saved]
     * reply says what it chose. Never retried by this class: after a drop
     * nobody knows whether the file landed, and a second send would save a
     * second copy.
     *
     * @throws Refused when the machine answered anything but `saving`, or
     *   refused after the bytes arrived (a full disk, a name clash it could
     *   not number past).
     * @throws Disconnected when the connection ended mid-upload.
     * @throws IllegalArgumentException when [len] is past [SAVE_MAX_BYTES] or zero.
     */
    @Throws(Refused::class, Disconnected::class)
    fun save(
        connect: () -> FrameChannel,
        name: String,
        len: Long,
        source: () -> InputStream,
        onProgress: (Long) -> Unit = {},
    ): Saved {
        require(len > 0) { "an upload of no bytes is not a file" }
        require(len <= SAVE_MAX_BYTES) {
            "$len bytes is past the $SAVE_MAX_BYTES-byte limit for sending a file to a computer"
        }
        return transfer(connect, Agentd.saveFile(name, len), ::savingAccepted, len, source, onProgress, ::readSaved)
    }

    /**
     * The takeover both verbs share: open [CHANNEL] with [request], check the
     * first reply with [accepted], send exactly [len] bytes, then read the
     * closing reply off the channel with [parse] (null while incomplete).
     */
    private fun <T : Any> transfer(
        connect: () -> FrameChannel,
        request: String,
        accepted: (ByteArray) -> Boolean,
        len: Long,
        source: () -> InputStream,
        onProgress: (Long) -> Unit,
        parse: (String) -> T?,
    ): T {
        val replies = java.util.concurrent.LinkedBlockingQueue<Any>()
        val listener = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                if (channel == CHANNEL) replies.put(bytes)
            }

            override fun onClose(channel: UInt, reason: String) {
                if (channel == CHANNEL) replies.put(Ended(reason))
            }

            override fun onDisconnect(cause: Throwable?) {
                replies.put(Ended(cause?.message ?: ""))
            }
        }

        val mux = Mux(connect(), listener, accepted)
        val pump = Thread({ mux.pump() }, "rime-upload")
        pump.isDaemon = true
        pump.start()
        try {
            // The takeover. A refusal comes back as the daemon's own sentence
            // — "no session 4", or the cap with its number in it — and
            // `openChannel` turns that into `ChannelRefused` because
            // `receivingAccepted` said no.
            val opened = try {
                mux.openChannel(CHANNEL, request.toByteArray(Charsets.UTF_8))
            } catch (e: ChannelRefused) {
                throw Refused(e.message ?: "the machine refused the upload")
            }
            // [Mux] returns the daemon's reply rather than interpreting it —
            // deliberately, because "no session 4" is the machine's sentence
            // and belongs on the screen in its own words. Interpreting it is
            // therefore THIS class's job, and the first draft forgot: it
            // asserted `receivingAccepted` with `check`, so a routine refusal
            // came out as an `IllegalStateException` carrying a JSON blob.
            // `ChannelRefused` covers only the other shape, a `Close` with no
            // reply at all.
            if (!accepted(opened)) throw Refused(refusal(opened))

            var sent = 0L
            source().use { stream ->
                val buffer = ByteArray(CHUNK)
                while (sent < len) {
                    val want = minOf(buffer.size.toLong(), len - sent).toInt()
                    val n = stream.read(buffer, 0, want)
                    if (n <= 0) {
                        // The daemon committed to reading exactly `len` bytes.
                        // Stopping here would leave it waiting until its own
                        // stall timeout, and padding would hand the agent a
                        // corrupted file it was told was a screenshot.
                        throw Refused(
                            "the file ended after $sent of $len bytes; nothing was handed over"
                        )
                    }
                    // The last chunk is held until the source is seen to
                    // END. A source longer than it said (a provider whose
                    // SIZE column was stale) would otherwise arrive cut at
                    // `len` and be reported as delivered. Closing without the
                    // last chunk leaves the daemon an early end, and it keeps
                    // nothing.
                    if (sent + n == len && stream.read() != -1) {
                        throw Refused(
                            "the file is longer than the $len bytes it said it was; nothing was handed over"
                        )
                    }
                    // Only the SEND is wrapped. A phone that walked out of
                    // range throws `EOFException` from the transport, and the
                    // first draft let it out raw — so a caller that handled
                    // `Disconnected` saw an exception it had no case for. A
                    // failure to read the FILE is a different thing and is
                    // left alone.
                    try {
                        mux.send(CHANNEL, buffer.copyOf(n))
                    } catch (e: java.io.IOException) {
                        throw Disconnected(
                            "the connection ended after $sent of $len bytes; nothing was handed over",
                            e,
                        )
                    }
                    sent += n
                    onProgress(sent)
                }
            }

            // The second reply, on the upload's own channel: `rime-remoted`'s
            // pump does not know whether it is carrying a terminal or a JSON
            // line, so the daemon's answer arrives the same way a terminal's
            // output does — and may arrive in pieces.
            val answer = StringBuilder()
            while (true) {
                val item = replies.poll(REPLY_TIMEOUT_MS, java.util.concurrent.TimeUnit.MILLISECONDS)
                    ?: throw Disconnected("${mux.machine} never said what became of the file")
                if (item is Ended) {
                    throw Disconnected(
                        if (item.reason.isEmpty()) {
                            "the connection ended before the file was handed over"
                        } else {
                            item.reason
                        }
                    )
                }
                answer.append((item as ByteArray).toString(Charsets.UTF_8))
                val done = parse(answer.toString())
                if (done != null) return done
            }
        } finally {
            runCatching { mux.close() }
        }
    }

    /**
     * Parse the closing reply, or `null` while it is still incomplete.
     *
     * Incomplete rather than invalid: the reply arrives as `Data` frames and a
     * long path can land in two of them, so a parser that failed on the first
     * piece would report a refusal for a file that was delivered.
     */
    internal fun read(text: String): Landed? {
        val value = runCatching {
            Agentd.json.parseToJsonElement(text)
        }.getOrNull() ?: return null
        val obj = value as? kotlinx.serialization.json.JsonObject ?: return null
        val reply = (obj["reply"] as? kotlinx.serialization.json.JsonPrimitive)?.content
        if (reply == "error") {
            val message = (obj["message"] as? kotlinx.serialization.json.JsonPrimitive)?.content
            throw Refused(message ?: "the machine refused the file after receiving it")
        }
        if (reply != "injected") return null
        val path = (obj["path"] as? kotlinx.serialization.json.JsonPrimitive)?.content
            ?: throw Refused("the machine accepted the file and did not say where it put it")
        val id = (obj["id"] as? kotlinx.serialization.json.JsonPrimitive)?.content?.toIntOrNull()
            ?: throw Refused("the machine's answer names no session")
        val bracketed = (obj["bracketed"] as? kotlinx.serialization.json.JsonPrimitive)
            ?.content?.toBooleanStrictOrNull() ?: false
        return Landed(id, path, bracketed)
    }

    /** Parse `save_file`'s closing reply, or `null` while it is still incomplete. */
    internal fun readSaved(text: String): Saved? {
        val value = runCatching {
            Agentd.json.parseToJsonElement(text)
        }.getOrNull() ?: return null
        val obj = value as? kotlinx.serialization.json.JsonObject ?: return null
        val reply = (obj["reply"] as? kotlinx.serialization.json.JsonPrimitive)?.content
        if (reply == "error") {
            val message = (obj["message"] as? kotlinx.serialization.json.JsonPrimitive)?.content
            throw Refused(message ?: "the machine refused the file after receiving it")
        }
        if (reply != "saved") return null
        val name = (obj["name"] as? kotlinx.serialization.json.JsonPrimitive)?.content
            ?: throw Refused("the machine saved the file and did not say under what name")
        val path = (obj["path"] as? kotlinx.serialization.json.JsonPrimitive)?.content
            ?: throw Refused("the machine saved the file and did not say where")
        return Saved(name, path)
    }

    /**
     * The machine's own sentence out of a reply that was not `receiving`.
     *
     * Falls back to the raw line rather than to a sentence of this app's: a
     * refusal nobody can act on is worse than an ugly one, and the daemon
     * names the session, the limit and the reason.
     */
    private fun refusal(reply: ByteArray): String {
        val text = reply.toString(Charsets.UTF_8)
        val obj = runCatching { Agentd.json.parseToJsonElement(text) }
            .getOrNull() as? kotlinx.serialization.json.JsonObject
            ?: return text
        return (obj["message"] as? kotlinx.serialization.json.JsonPrimitive)?.content ?: text
    }

    private class Ended(val reason: String)

    /**
     * Bytes per `Frame.Data` this side writes.
     *
     * Well under `rime-remote-core`'s `MAX_PAYLOAD` of 65514, which is a
     * ceiling and not a target: the transport seals each frame separately and
     * a phone on a congested link does better with frames it can get out than
     * with one it cannot. The daemon reassembles a byte stream either way —
     * it counts bytes, not frames.
     */
    private const val CHUNK: Int = 32 * 1024

    /**
     * How long to wait for the closing reply once the last byte is out.
     *
     * The daemon writes the file, types the path and answers, all under the
     * session's lock. Thirty seconds is far longer than that and far shorter
     * than the daemon's own upload budget, so a phone that waits this long is
     * waiting on something that is not going to answer.
     */
    private const val REPLY_TIMEOUT_MS: Long = 30_000
}
