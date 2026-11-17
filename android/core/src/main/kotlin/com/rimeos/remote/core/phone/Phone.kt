package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Mux
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The phone itself, as the paired computer sees it: its battery, and ring
 * (`docs/remote-live-contract.md` §2.5).
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

    /** A push ring older than this is dropped: a late push must not ring a phone. */
    const val STALE_PUSH_S: Long = 60

    /** The longest event line accepted. The only event today is under 300 bytes. */
    const val MAX_LINE: Int = 4096

    private val json = Json { ignoreUnknownKeys = true }

    fun report(battery: Int?, charging: Boolean?, can: List<String>): String {
        require(battery == null || battery in 0..100) { "a battery level is 0 to 100, not $battery" }
        return buildJsonObject {
            put("cmd", REPORT)
            battery?.let { put("battery", it) }
            charging?.let { put("charging", it) }
            putJsonArray("can") { can.forEach { add(JsonPrimitive(it)) } }
        }.toString()
    }

    fun events(): String = """{"cmd":"$EVENTS"}"""

    fun ringAck(id: Long): String =
        buildJsonObject {
            put("cmd", "ring_ack")
            put("id", id)
        }.toString()

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

    private fun parse(text: String) = runCatching { json.parseToJsonElement(text) }.getOrNull()

    private fun JsonObject.prim(key: String): JsonPrimitive? = (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }

    private fun JsonObject.text(key: String): String? = prim(key)?.takeIf { it.isString }?.content

    private fun JsonObject.long(key: String): Long? = prim(key)?.takeIf { !it.isString }?.longOrNull
}

sealed interface DeviceEvent {
    /** Ring, asked for by [machine] at [at] (its clock, unix seconds). */
    data class Ring(val id: Long, val at: Long, val machine: String) : DeviceEvent
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
 * When the phone tells the machine its battery again.
 *
 * The first report always goes. After that, a report goes when charging
 * starts or stops, when the level moved by [step] points or more since the
 * last one sent, or when [maxAgeMs] passed. Android sends a battery broadcast
 * every few seconds while charging; a report for each would be traffic for
 * nothing.
 */
class BatteryThrottle(private val step: Int = 5, private val maxAgeMs: Long = 10 * 60_000L) {
    private var level: Int? = null
    private var charging: Boolean? = null
    private var atMs: Long = 0
    private var sentOnce = false

    @Synchronized
    fun due(level: Int?, charging: Boolean?, nowMs: Long): Boolean {
        if (!sentOnce) return true
        if (charging != this.charging) return true
        val last = this.level
        if ((level == null) != (last == null)) return true
        if (level != null && last != null && kotlin.math.abs(level - last) >= step) return true
        return nowMs - atMs >= maxAgeMs
    }

    @Synchronized
    fun sent(level: Int?, charging: Boolean?, nowMs: Long) {
        this.level = level
        this.charging = charging
        atMs = nowMs
        sentOnce = true
    }

    /** A new connection: its machine has heard nothing from this one yet. */
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
    fun report(battery: Int?, charging: Boolean?, can: List<String>) =
        Phone.readOk(link.request(Phone.report(battery, charging, can), retry = false))

    /**
     * Acknowledge a ring. Not retried: an ack after a reconnect is too late
     * for the machine, which has already tried the push.
     */
    fun ack(id: Long) = Phone.readOk(link.request(Phone.ringAck(id), retry = false))

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
