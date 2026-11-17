package com.rimeos.remote.core.nex

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import java.io.ByteArrayOutputStream

/** One event from nex's global feed. */
data class NexEvent(
    /** The feed's own order, across every session. What a resume names. */
    val gseq: Long,
    val kind: String,
    /** The session (or mission) the event belongs to. */
    val stream: String?,
    val payload: JsonObject,
) {
    /** Changes what a list of sessions, approvals or missions shows. */
    val changesLists: Boolean
        get() = kind.startsWith("session.") || kind.startsWith("turn.") ||
            kind.startsWith("mission.") || kind.startsWith("task.") ||
            kind == "tool.approval_requested" || kind == "tool.approved" || kind == "tool.denied"
}

/**
 * The `nex_events` channel's bytes, turned back into events.
 *
 * ## What arrives
 *
 * The bridge forwards nexd's lines as they come: first the JSON-RPC response
 * to `sessions.stream` (`result.events` is the backlog, each `{gseq, event}`,
 * and `result.last_gseq`), then one `{"method":"event","params":{…}}`
 * notification per event, where the event is flat and `gseq` sits beside its
 * fields. Both shapes are read here; a fixture of each is in the tests.
 *
 * `Data` frames do not respect lines, so bytes are buffered until a newline,
 * and a line longer than [maxLine] ends the feed rather than growing without
 * bound.
 *
 * ## Never the same event twice
 *
 * [lastGseq] is the newest event handed out. Anything at or below it is
 * dropped, so a feed re-opened after a reconnect with `after_gseq` set to it
 * cannot show an approval twice, and a backlog that overlaps cannot either.
 */
class NexFeed(
    after: Long = 0,
    private val maxLine: Int = MAX_LINE,
) {
    /** The newest event handed out, or the `after` this feed began from. */
    var lastGseq: Long = after
        private set

    /** Whether the response line, the first one, has arrived. */
    var started: Boolean = false
        private set

    private val partial = ByteArrayOutputStream()

    /** What [feed] produced from one chunk. */
    data class Batch(val events: List<NexEvent>, val startedNow: Boolean)

    /** Hand over the next bytes. Throws [NexFeedBroken] when the feed cannot continue. */
    fun feed(bytes: ByteArray): Batch {
        val out = ArrayList<NexEvent>()
        var startedNow = false
        var from = 0
        for (i in bytes.indices) {
            if (bytes[i] != '\n'.code.toByte()) continue
            partial.write(bytes, from, i - from)
            from = i + 1
            val line = partial.toByteArray().toString(Charsets.UTF_8)
            partial.reset()
            if (line.isBlank()) continue
            if (!started) {
                started = true
                startedNow = true
                start(line, out)
            } else {
                notification(line)?.let { take(it, out) }
            }
        }
        partial.write(bytes, from, bytes.size - from)
        if (partial.size() > maxLine) {
            throw NexFeedBroken("a line on the nex feed was longer than ${maxLine / 1024} KiB")
        }
        return Batch(out, startedNow)
    }

    private fun start(line: String, out: MutableList<NexEvent>) {
        val element = runCatching { Nex.json.parseToJsonElement(line) }.getOrNull()
            ?: throw NexFeedBroken("the nex feed did not start with a response")
        val result = try {
            Nex.response(element)
        } catch (e: NexRefused) {
            throw NexFeedBroken(e.message)
        }
        val r = result as? JsonObject ?: return
        for (item in r.objects("events")) {
            val gseq = item.long("gseq") ?: continue
            val event = item.obj("event") ?: continue
            take(event(gseq, event) ?: continue, out)
        }
        // A tail starts here: nothing older is wanted, even if it was never seen.
        r.long("last_gseq")?.let { if (it > lastGseq) lastGseq = it }
    }

    private fun notification(line: String): NexEvent? {
        val o = runCatching { Nex.json.parseToJsonElement(line) }.getOrNull() as? JsonObject ?: return null
        if (o.str("method") != "event") return null
        val p = o.obj("params") ?: return null
        return event(p.long("gseq") ?: return null, p)
    }

    private fun take(e: NexEvent, out: MutableList<NexEvent>) {
        if (e.gseq <= lastGseq) return
        lastGseq = e.gseq
        out.add(e)
    }

    companion object {
        /** One event line. nexd's largest payloads are tool output summaries, well under this. */
        const val MAX_LINE: Int = 4 * 1024 * 1024

        private fun event(gseq: Long, e: JsonObject): NexEvent? = NexEvent(
            gseq = gseq,
            kind = e.str("kind") ?: return null,
            stream = e.str("stream_id"),
            payload = e.obj("payload") ?: JsonObject(emptyMap()),
        )
    }
}

/** The feed cannot go on: its first line was a refusal, or a line was too long. */
class NexFeedBroken(message: String) : Exception(message)

/** The response a one-shot `nex` channel carries: one line, then the channel closes. */
fun oneShotResult(bytes: ByteArray): JsonElement {
    val line = bytes.toString(Charsets.UTF_8).trim()
    val element = runCatching { Nex.json.parseToJsonElement(line) }.getOrNull()
        ?: throw com.rimeos.remote.core.agent.AgentError("internal", "the nex channel closed without a response")
    return Nex.response(element)
}
