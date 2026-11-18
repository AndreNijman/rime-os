package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Mux
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The phone itself, as the paired computer sees it: its battery, ring
 * (`docs/remote-live-contract.md` §2.5), what the computer sends it (§2.6),
 * and its network and Do Not Disturb (§2.9).
 *
 * The phone tells the machine its battery and what it can do with
 * `phone_report`, and keeps a read-only `device_events` channel open so the
 * machine can ask it to ring. A machine offers both when its `remote_hello`
 * lists [EVENTS] and [REPORT]; against one that does not, nothing here is sent.
 */
object Phone {
    /** The channel a machine writes events on, and the feature naming it. */
    const val EVENTS: String = "device_events"

    /** The verb, and the feature, for the phone's report. */
    const val REPORT: String = "phone_report"

    /** The word in `can` that lets the machine ring this phone through its push service. */
    const val CAN_RING: String = "ring"

    /** The word in `can` that says this phone takes files and text the computer sends. */
    const val CAN_RECEIVE: String = "receive"

    /**
     * The word in `can` that says this phone dismisses a notification it
     * mirrored once the person dismisses it on the computer (contract §2.7).
     */
    const val CAN_DISMISS: String = "dismiss"

    /**
     * The word in `can` that says the person turned on replying from the
     * computer: this phone sends a reply typed there through the
     * notification's own reply action (contract §2.7).
     */
    const val CAN_REPLY: String = "reply"

    /**
     * The feature a machine offers once it follows this phone's Do Not
     * Disturb and sends its own (contract §2.9). Only such a machine is sent
     * `dnd`, and only to it does the report say [CAN_FOCUS].
     */
    const val FOCUS: String = "phone_focus"

    /**
     * The word in `can` that says the person turned on "Do Not Disturb
     * follows your computer": this phone applies a `focus` line, and the
     * machine may follow its `dnd` (contract §2.9).
     */
    const val CAN_FOCUS: String = "focus"

    /**
     * The word in `can` that says the person turned on "Messages on your
     * computer" and granted reading SMS (§2.11): the computer may ask for
     * this phone's conversations, and, over an unlocked link, send one.
     */
    const val CAN_MESSAGES: String = "messages"

    /**
     * The words in `can` that say the person turned on "Calls on your
     * computer" and the phone can see its calls (§2.12), and, each with its
     * own permission, what the computer may do with them: read the history,
     * answer, decline and hang up, and place a call. Dialling is never
     * offered on a companion link.
     */
    const val CAN_CALLS: String = "calls"
    const val CAN_CALLS_HISTORY: String = "calls_history"
    const val CAN_CALLS_ANSWER: String = "calls_answer"
    const val CAN_CALLS_DIAL: String = "calls_dial"

    /** The channel, and the feature, a sent file or text is taken on. */
    const val TAKE: String = "take_offer"

    /** The most text one offer carries, and the largest file (contract §2.6). */
    const val MAX_TEXT_BYTES: Long = 8192
    const val MAX_FILE_BYTES: Long = 256L * 1024 * 1024

    /** The longest file name an offer carries, in characters. */
    const val NAME_CAP: Int = 200

    /** A push ring older than this is dropped: a late push must not ring a phone. */
    const val STALE_PUSH_S: Long = 60

    /**
     * The longest event line accepted. The longest event is a reply: 2000
     * characters of up to four bytes each, a quote escaped to two, so about
     * 8 KB with its key; twice that leaves room and still bounds the buffer.
     */
    const val MAX_LINE: Int = 16 * 1024

    private val json = Json { ignoreUnknownKeys = true }

    /**
     * The report (§2.5), with how the phone is connected and its Do Not
     * Disturb when known (§2.9). A field that is not known is left out, never
     * guessed: a signal left out is not a signal of 0.
     */
    fun report(
        battery: Int?,
        charging: Boolean?,
        can: List<String>,
        network: NetworkState? = null,
        dnd: Boolean? = null,
    ): String {
        require(battery == null || battery in 0..100) { "a battery level is 0 to 100, not $battery" }
        return buildJsonObject {
            put("cmd", REPORT)
            battery?.let { put("battery", it) }
            charging?.let { put("charging", it) }
            network?.let { n ->
                put(
                    "network",
                    buildJsonObject {
                        put("type", n.type)
                        n.signal?.let { put("signal", it) }
                    },
                )
            }
            dnd?.let { put("dnd", it) }
            putJsonArray("can") { can.forEach { add(JsonPrimitive(it)) } }
        }.toString()
    }

    fun events(): String = """{"cmd":"$EVENTS"}"""

    fun ringAck(id: Long): String =
        buildJsonObject {
            put("cmd", "ring_ack")
            put("id", id)
        }.toString()

    fun offerAck(id: Long): String =
        buildJsonObject {
            put("cmd", "offer_ack")
            put("id", id)
        }.toString()

    fun offerDone(id: Long): String =
        buildJsonObject {
            put("cmd", "offer_done")
            put("id", id)
        }.toString()

    fun take(id: Long): String =
        buildJsonObject {
            put("cmd", TAKE)
            put("id", id)
        }.toString()

    /** Whether the machine accepted a `take_offer` channel. */
    fun taking(reply: ByteArray): Boolean =
        (parse(reply.toString(Charsets.UTF_8)) as? JsonObject)?.text("reply") == "offer"

    /**
     * The machine's answer to `take_offer`: what is coming. Throws
     * [AgentError] for a refusal (an offer that expired, or was taken back)
     * and for an answer that does not describe something an offer may carry.
     */
    fun taken(reply: ByteArray): Taken {
        val text = reply.toString(Charsets.UTF_8)
        val o = parse(text) as? JsonObject
            ?: throw AgentError("internal", "the computer sent something that is not a reply: ${text.take(200)}")
        if (o.text("reply") != "offer") {
            readOk(text)
            throw AgentError("internal", "expected an `offer` reply")
        }
        val kind = OfferKind.of(o.text("kind")) ?: throw AgentError("internal", "the computer offered a kind this phone does not take")
        val len = o.long("len") ?: throw AgentError("internal", "the offer did not say how long it is")
        if (!kind.fits(len)) throw AgentError("too_large", "the computer offered ${len} bytes, more than a phone takes")
        return Taken(kind, cleanName(o.text("name")), len)
    }

    /** Whether the machine accepted the `device_events` channel. */
    fun opened(reply: ByteArray): Boolean =
        (parse(reply.toString(Charsets.UTF_8)) as? JsonObject)?.text("reply") == "events"

    /** Throws [AgentError] unless [reply] is `{"reply":"ok"}`. */
    fun readOk(reply: String) {
        val o = parse(reply) as? JsonObject
            ?: throw AgentError("internal", "the computer sent something that is not a reply: ${reply.take(200)}")
        when (val tag = o.text("reply")) {
            "ok" -> Unit
            "error" -> throw AgentError(
                o.text("code") ?: o.text("kind") ?: "internal",
                o.text("message") ?: "the computer refused and did not say why",
            )
            else -> throw AgentError("internal", "expected an `ok` reply and got `$tag`")
        }
    }

    /**
     * One event line, or null for an event this build does not know or a line
     * that is not one. Unknown events are dropped, not errors: a newer machine
     * may send events an older phone has no use for.
     */
    fun event(line: String): DeviceEvent? {
        val o = parse(line) as? JsonObject ?: return null
        return when (o.text("event")) {
            "ring" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                val at = o.long("at") ?: return null
                DeviceEvent.Ring(id, at, (o.text("machine") ?: "").take(MACHINE_CAP))
            }
            "offer" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                val at = o.long("at") ?: return null
                val kind = OfferKind.of(o.text("kind")) ?: return null
                val len = o.long("len")?.takeIf { kind.fits(it) } ?: return null
                DeviceEvent.Offer(id, at, (o.text("machine") ?: "").take(MACHINE_CAP), kind, cleanName(o.text("name")), len)
            }
            "notification_dismissed" -> o.text("key")?.takeIf { Mirror.validKey(it) }?.let { DeviceEvent.Dismissed(it) }
            "notification_reply" -> {
                val key = o.text("key")?.takeIf { Mirror.validKey(it) } ?: return null
                // Counted in characters, as the computer caps it.
                val text = o.text("text")?.takeIf { it.isNotEmpty() && it.codePointCount(0, it.length) <= Mirror.REPLY_TEXT_CAP }
                    ?: return null
                DeviceEvent.Replied(key, text)
            }
            // A real boolean only: a quoted "true" is not one.
            "focus" -> o.prim("dnd")?.takeIf { !it.isString }?.booleanOrNull?.let { DeviceEvent.Focus(it) }
            "media_control" -> {
                val action = o.text("action")?.takeIf { it in Media.CONTROL_ACTIONS } ?: return null
                // A seek says where to; nothing else carries a position.
                val position = if (action == "seek") {
                    o.long("position_ms")?.takeIf { it in 0..Media.MAX_MS } ?: return null
                } else {
                    null
                }
                DeviceEvent.MediaControl(action, position)
            }
            "messages_request" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                // Anything present must be right; a thread or page that is
                // not a whole number is not quietly read as "all of them".
                val thread = if (o.containsKey("thread")) o.long("thread")?.takeIf { it >= 0 } ?: return null else null
                val before = if (o.containsKey("before")) o.long("before")?.takeIf { it >= 0 } ?: return null else null
                val limit = o.long("limit")?.takeIf { it >= 1 } ?: return null
                DeviceEvent.MessagesRequest(id, thread, before, limit.coerceAtMost(Messages.MAX_LIMIT.toLong()).toInt())
            }
            "messages_send" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                // The text as typed: the sender checks it and says why it
                // will not send it, rather than the event vanishing here.
                val text = o.text("text") ?: return null
                val to = o["to"]?.let { el ->
                    val list = el as? kotlinx.serialization.json.JsonArray ?: return null
                    list.map { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content ?: return null }
                }
                val thread = if (o.containsKey("thread")) o.long("thread")?.takeIf { it >= 0 } ?: return null else null
                if ((to == null) == (thread == null)) return null
                DeviceEvent.MessagesSend(id, to, thread, text)
            }
            "call_action" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                val action = o.text("action")?.takeIf { it in Calls.ACTIONS } ?: return null
                // A number only with a dial, and a dial always with one.
                val number = if (action == Calls.DIAL) o.text("number") ?: return null else null
                DeviceEvent.CallAction(id, action, number)
            }
            "call_history_request" -> {
                val id = o.long("id")?.takeIf { it >= 0 } ?: return null
                val before = if (o.containsKey("before")) o.long("before")?.takeIf { it >= 0 } ?: return null else null
                val limit = o.long("limit")?.takeIf { it >= 1 } ?: return null
                DeviceEvent.CallHistoryRequest(id, before, limit.coerceAtMost(Calls.MAX_LIMIT.toLong()).toInt())
            }
            else -> null
        }
    }

    /**
     * Whether a push ring raised at [startedS] (unix seconds, the machine's
     * clock) is fresh enough to ring at [nowS] (this phone's clock).
     *
     * A ring from the future is allowed up to the same margin, because the two
     * clocks are not the same clock.
     */
    fun freshPush(startedS: Long, nowS: Long): Boolean = kotlin.math.abs(nowS - startedS) <= STALE_PUSH_S

    private const val MACHINE_CAP = 120

    /**
     * A name from the computer, made safe to show and to save under: no path,
     * no control characters, at most [NAME_CAP] characters. The computer
     * already reduces it; this phone does not rely on that.
     */
    fun cleanName(raw: String?): String {
        val base = (raw ?: "").substringAfterLast('/').substringAfterLast('\\')
        val clean = base.filter { !it.isISOControl() && it != '\u2028' && it != '\u2029' }.trim().trimStart('.')
        return clean.take(NAME_CAP).ifEmpty { "shared file" }
    }

    private fun parse(text: String) = runCatching { json.parseToJsonElement(text) }.getOrNull()

    private fun JsonObject.prim(key: String): JsonPrimitive? = (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }

    private fun JsonObject.text(key: String): String? = prim(key)?.takeIf { it.isString }?.content

    private fun JsonObject.long(key: String): Long? = prim(key)?.takeIf { !it.isString }?.longOrNull
}

sealed interface DeviceEvent {
    /** Ring, asked for by [machine] at [at] (its clock, unix seconds). */
    data class Ring(val id: Long, val at: Long, val machine: String) : DeviceEvent

    /** The person dismissed, on the computer, the mirrored notification with this [key]. */
    data class Dismissed(val key: String) : DeviceEvent

    /**
     * The person used the phone's player on the computer (§2.8): [action] is
     * one of [Media.CONTROL_ACTIONS]; [positionMs] is where a `seek` goes.
     */
    data class MediaControl(val action: String, val positionMs: Long?) : DeviceEvent

    /** The computer asks this phone's Do Not Disturb to be on ([dnd]) or off (§2.9). */
    data class Focus(val dnd: Boolean) : DeviceEvent

    /** The person replied [text], on the computer, to the mirrored notification with this [key]. */
    data class Replied(val key: String, val text: String) : DeviceEvent {
        // Somebody's message: never in a log line by accident.
        override fun toString(): String = "Replied(key=$key, text=<${text.length} chars>)"
    }

    /**
     * The computer asks for this phone's SMS conversations (§2.11): the list
     * when [thread] is null, or one conversation's messages; at most [limit]
     * of them, all older than [before] (unix milliseconds) when it is set.
     */
    data class MessagesRequest(val id: Long, val thread: Long?, val before: Long?, val limit: Int) : DeviceEvent

    /** The computer asks this phone to send [text] as an SMS, to [to] or into [thread] (§2.11). */
    data class MessagesSend(val id: Long, val to: List<String>?, val thread: Long?, val text: String) : DeviceEvent {
        // Somebody's message, and their number: never in a log line by accident.
        override fun toString(): String =
            "MessagesSend(id=$id, to=${to?.size?.let { "<$it>" }}, thread=$thread, text=<${text.length} chars>)"
    }

    /** [machine] sent this phone a file or text of [len] bytes; take it with `take_offer`. */
    /** Answer, decline, hang up or dial ([number]) the phone's call (§2.12). */
    data class CallAction(val id: Long, val action: String, val number: String?) : DeviceEvent {
        override fun toString(): String = "CallAction(id=$id, action=$action)"
    }

    /** The newest [limit] calls of the history older than [before] (§2.12). */
    data class CallHistoryRequest(val id: Long, val before: Long?, val limit: Int) : DeviceEvent

    data class Offer(
        val id: Long,
        val at: Long,
        val machine: String,
        val kind: OfferKind,
        val name: String,
        val len: Long,
    ) : DeviceEvent
}

enum class OfferKind(val wire: String, private val max: Long) {
    FILE("file", Phone.MAX_FILE_BYTES),
    TEXT("text", Phone.MAX_TEXT_BYTES),
    ;

    fun fits(len: Long): Boolean = len in 1..max

    companion object {
        fun of(wire: String?): OfferKind? = entries.firstOrNull { it.wire == wire }
    }
}

/** What a `take_offer` channel is about to carry. */
data class Taken(val kind: OfferKind, val name: String, val len: Long)

/**
 * Which offers this phone has taken, so the same offer arriving again (the
 * machine replays what is not done each time the channel opens, and a lost
 * `offer_done` leaves one not done) is not saved twice.
 *
 * An offer is claimed while it is being taken and kept once it is. One that
 * failed is let go, so the next replay tries it again.
 */
class OfferMemory(private val capacity: Int = 256) {
    private val taken = LinkedHashSet<String>()
    private val busy = HashSet<String>()

    private fun key(deviceId: String, id: Long, at: Long) = "$deviceId/$id/$at"

    /** True when this offer is new: the caller takes it and then calls [finish]. */
    @Synchronized
    fun claim(deviceId: String, id: Long, at: Long): Boolean {
        val k = key(deviceId, id, at)
        if (k in taken || k in busy) return false
        busy += k
        return true
    }

    /** True when it was taken already, so the caller only says it is done. */
    @Synchronized
    fun wasTaken(deviceId: String, id: Long, at: Long): Boolean = key(deviceId, id, at) in taken

    @Synchronized
    fun finish(deviceId: String, id: Long, at: Long, ok: Boolean) {
        val k = key(deviceId, id, at)
        busy -= k
        if (!ok) return
        taken += k
        while (taken.size > capacity) taken.remove(taken.first())
    }
}

/**
 * Splits a channel's bytes into lines. A line longer than [Phone.MAX_LINE]
 * breaks the feed, because nothing the machine sends is that long.
 */
class EventLines {
    private val buffer = ByteArrayOutputStream()

    fun feed(bytes: ByteArray): List<String> {
        val out = mutableListOf<String>()
        for (b in bytes) {
            if (b == '\n'.code.toByte()) {
                out += buffer.toString(Charsets.UTF_8)
                buffer.reset()
            } else {
                if (buffer.size() >= Phone.MAX_LINE) throw AgentError("too_large", "the computer sent an event line that is too long")
                buffer.write(b.toInt())
            }
        }
        return out
    }
}

/**
 * When the phone tells the machine its report again: its battery (§2.5), and
 * how it is connected and its Do Not Disturb (§2.9).
 *
 * The first report always goes. After that, a report goes at once when
 * charging starts or stops, when Do Not Disturb turns on or off, when the
 * kind of network changes, or when `can` changes; when the level moved by
 * [step] points or more since the last one sent; when only the signal's
 * bars moved and [signalGapMs] passed since the last report; or when
 * [maxAgeMs] passed.
 * Android sends a battery broadcast every few seconds while charging, and a
 * new signal reading with each step in a moving car; a report for each would
 * be traffic for nothing.
 */
class ReportThrottle(
    private val step: Int = 5,
    private val maxAgeMs: Long = 10 * 60_000L,
    private val signalGapMs: Long = 60_000L,
) {
    private var level: Int? = null
    private var charging: Boolean? = null
    private var network: NetworkState? = null
    private var dnd: Boolean? = null
    private var can: List<String> = emptyList()
    private var atMs: Long = 0
    private var sentOnce = false

    @Synchronized
    fun due(
        level: Int?,
        charging: Boolean?,
        nowMs: Long,
        network: NetworkState? = null,
        dnd: Boolean? = null,
        can: List<String> = emptyList(),
    ): Boolean {
        if (!sentOnce) return true
        if (charging != this.charging) return true
        // Compared, not only reset for: a reset while a report is in flight
        // is undone by that report's `sent`, and the new `can` would wait.
        if (can != this.can) return true
        if (dnd != this.dnd) return true
        if (network?.type != this.network?.type) return true
        val last = this.level
        if ((level == null) != (last == null)) return true
        if (level != null && last != null && kotlin.math.abs(level - last) >= step) return true
        if (network?.signal != this.network?.signal && nowMs - atMs >= signalGapMs) return true
        return nowMs - atMs >= maxAgeMs
    }

    @Synchronized
    fun sent(
        level: Int?,
        charging: Boolean?,
        nowMs: Long,
        network: NetworkState? = null,
        dnd: Boolean? = null,
        can: List<String> = emptyList(),
    ) {
        this.can = can
        this.level = level
        this.charging = charging
        this.network = network
        this.dnd = dnd
        atMs = nowMs
        sentOnce = true
    }

    /** A new connection, or a change to `can`: its machine is told everything again. */
    @Synchronized
    fun reset() {
        sentOnce = false
    }
}

/**
 * Remembers the last ring rung from each machine, so the push that carries
 * the same ring after a lost ack does not ring a second time.
 */
class RingMemory {
    private val last = HashMap<String, Long>()

    /** True when this ring is new and should ring; it is then remembered. */
    @Synchronized
    fun first(deviceId: String, at: Long): Boolean {
        if (last[deviceId] == at) return false
        last[deviceId] = at
        return true
    }
}

/** The phone's two verbs and its event channel, on one machine's link. */
class PhoneLink(private val link: MachineLink) {
    /** Not retried: a newer report goes out on the next connection anyway. */
    fun report(battery: Int?, charging: Boolean?, can: List<String>, network: NetworkState? = null, dnd: Boolean? = null) =
        Phone.readOk(link.request(Phone.report(battery, charging, can, network, dnd), retry = false))

    /**
     * Acknowledge a ring. Not retried: an ack after a reconnect is too late
     * for the machine, which has already tried the push.
     */
    fun ack(id: Long) = Phone.readOk(link.request(Phone.ringAck(id), retry = false))

    /**
     * One line of an answer to a messages request, or a send's result
     * (§2.11), as [Messages] wrote it. Not retried: the computer's request
     * belongs to the connection it was asked on, and dies with it.
     */
    fun messages(line: String) = Phone.readOk(link.request(line, retry = false))

    /**
     * A `call_state`, `call_done` or `call_history_reply` line (§2.12), as
     * [Calls] wrote it. Not retried: an answer belongs to the connection it
     * was asked on, and a new connection is told the call as it is then.
     */
    fun calls(line: String) = Phone.readOk(link.request(line, retry = false))

    /** Tell the machine this phone has an offer, so it reports "link". Not retried, as for [ack]. */
    fun offerAck(id: Long) = Phone.readOk(link.request(Phone.offerAck(id), retry = false))

    /**
     * One notification, or its removal, for the computer (§2.7). Not retried:
     * a notification that missed its connection is not news on the next one.
     */
    fun mirror(item: MirrorItem) = Phone.readOk(
        link.request(
            when (item) {
                is MirrorItem.Post -> Mirror.post(item.key, item.content)
                is MirrorItem.Removed -> Mirror.removed(item.key)
            },
            retry = false,
        ),
    )

    /**
     * What this phone is playing, or `null` (§2.8), as [Media.encode] wrote
     * it. Not retried: the next connection is sent the state of its own.
     */
    fun media(line: String) = Phone.readOk(link.request(line, retry = false))

    /** The offer is taken (or never will be): the machine forgets it. */
    fun offerDone(id: Long) = Phone.readOk(link.request(Phone.offerDone(id), retry = true))

    /**
     * Take offer [id]: open a `take_offer` channel and hand each piece of it
     * to [sink], in order, on the connection's own thread, until the machine
     * closes it. Blocks until then.
     *
     * The bytes must be exactly as many as the reply said: fewer is a
     * connection that ended part way, more is a machine that lied about the
     * length, and either throws. [quietMs] is how long the channel may carry
     * nothing before it is given up on.
     */
    fun take(id: Long, sink: (ByteArray) -> Unit, quietMs: Long = 30_000): Taken {
        val mux = link.shared()
        val channel = mux.nextChannelId()
        val done = java.util.concurrent.CompletableFuture<Unit>()
        val got = java.util.concurrent.atomic.AtomicLong(0)
        val expected = java.util.concurrent.atomic.AtomicLong(-1)
        val lastData = java.util.concurrent.atomic.AtomicLong(System.currentTimeMillis())
        val listener = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                if (done.isDone) return
                lastData.set(System.currentTimeMillis())
                val total = got.addAndGet(bytes.size.toLong())
                val limit = expected.get().takeIf { it >= 0 } ?: Phone.MAX_FILE_BYTES
                if (total > limit) {
                    done.completeExceptionally(AgentError("too_large", "the computer sent more than it offered"))
                    return
                }
                try {
                    sink(bytes)
                } catch (e: Throwable) {
                    done.completeExceptionally(e)
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
            channel,
            Phone.take(id).toByteArray(Charsets.UTF_8),
            channelListener = listener,
            // Read the length as the reply arrives, on the connection's thread,
            // so it bounds the very first piece that follows it.
            accepted = { r ->
                Phone.taking(r).also { ok -> if (ok) runCatching { expected.set(Phone.taken(r).len) } }
            },
        )
        val taken = try {
            Phone.taken(reply)
        } catch (e: Throwable) {
            mux.closeChannel(channel)
            throw e
        }
        expected.set(taken.len)
        try {
            while (true) {
                try {
                    done.get(1_000, java.util.concurrent.TimeUnit.MILLISECONDS)
                    break
                } catch (_: java.util.concurrent.TimeoutException) {
                    if (System.currentTimeMillis() - lastData.get() > quietMs) {
                        throw Disconnected("the computer stopped sending part way")
                    }
                }
            }
        } catch (e: java.util.concurrent.ExecutionException) {
            throw e.cause ?: e
        } finally {
            mux.closeChannel(channel)
        }
        if (got.get() > taken.len) throw AgentError("too_large", "the computer sent more than it offered")
        if (got.get() < taken.len) throw Disconnected("the computer sent ${got.get()} of ${taken.len} bytes")
        return taken
    }

    fun interface Listener {
        fun onEvent(event: DeviceEvent)
    }

    /**
     * Open the `device_events` channel on the control connection. Only against
     * a machine whose hello lists `mux_attach` as well as [Phone.EVENTS].
     * [onEnd] runs once when the channel closes or the connection drops.
     */
    fun subscribe(listener: Listener, onEnd: (Throwable?) -> Unit): Closeable {
        val mux = link.shared()
        val id = mux.nextChannelId()
        val lines = EventLines()
        val ended = AtomicBoolean(false)
        fun end(cause: Throwable?) {
            if (ended.compareAndSet(false, true)) onEnd(cause)
        }
        val channelListener = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                val got = try {
                    synchronized(lines) { lines.feed(bytes) }
                } catch (e: AgentError) {
                    mux.closeChannel(id)
                    end(e)
                    return
                }
                got.forEach { line -> Phone.event(line)?.let(listener::onEvent) }
            }

            override fun onClose(channel: UInt, reason: String) =
                end(Disconnected(reason.ifEmpty { "the computer closed its event channel" }))

            override fun onDisconnect(cause: Throwable?) = end(cause ?: Disconnected("the connection ended"))
        }
        val reply = mux.openChannel(
            id,
            Phone.events().toByteArray(Charsets.UTF_8),
            channelListener = channelListener,
            accepted = Phone::opened,
        )
        if (!Phone.opened(reply)) {
            Phone.readOk(reply.toString(Charsets.UTF_8))
            throw AgentError("internal", "the computer's event channel did not open")
        }
        return Closeable { mux.closeChannel(id) }
    }
}
