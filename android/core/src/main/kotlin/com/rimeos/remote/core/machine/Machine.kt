package com.rimeos.remote.core.machine

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.put

/**
 * The computer itself: its status, its lock, its media and its volume.
 *
 * Five verbs rime-remoted answers on its own (`docs/remote-live-contract.md`
 * §2.4). None of them needs a grant and none of them can unlock anything:
 * they are what the person could do at the keyboard. A machine offers them
 * when its hello lists the features below.
 */
object Machine {
    /** `machine_status`: battery, power, lock and update. */
    const val STATUS: String = "machine_status"

    /** `machine_lock`: lock the screen. There is no unlock, and there will not be one. */
    const val LOCK: String = "machine_lock"

    /** `media`, `media_control` and `volume`. */
    const val MEDIA: String = "media"

    /** The longest title or artist the machine sends (its own cap). */
    const val TEXT_CAP: Int = 200

    private val json = Json { ignoreUnknownKeys = true }

    fun status(): String = """{"cmd":"machine_status"}"""

    fun lock(): String = """{"cmd":"machine_lock"}"""

    fun media(): String = """{"cmd":"media"}"""

    fun control(action: MediaAction): String =
        buildJsonObject {
            put("cmd", "media_control")
            put("action", action.wire)
        }.toString()

    fun volume(percent: Int): String {
        require(percent in 0..100) { "a volume is 0 to 100, not $percent" }
        return buildJsonObject {
            put("cmd", "volume")
            put("percent", percent)
        }.toString()
    }

    fun mute(muted: Boolean): String =
        buildJsonObject {
            put("cmd", "volume")
            put("muted", muted)
        }.toString()

    fun readStatus(reply: String): MachineStatus {
        val o = require(reply, "machine_status")
        val b = o["battery"] as? JsonObject
        return MachineStatus(
            battery = b?.let {
                val percent = it.int("percent") ?: return@let null
                Battery(percent.coerceIn(0, 100), it.text("state") ?: "unknown")
            },
            onAc = o.bool("on_ac"),
            powerMode = o.text("power_mode"),
            locked = o.bool("locked"),
            update = o.text("update"),
        )
    }

    /** What the screen said after the lock was asked for: `false` is "asked, not confirmed". */
    fun readLock(reply: String): Boolean = require(reply, "machine_lock").bool("locked") == true

    fun readMedia(reply: String): MediaState {
        val o = require(reply, "media")
        val p = o["player"] as? JsonObject
        val v = o["volume"] as? JsonObject
        return MediaState(
            player = p?.let {
                Player(
                    name = it.text("name") ?: return@let null,
                    status = it.text("status") ?: "stopped",
                    title = (it.text("title") ?: "").take(TEXT_CAP),
                    artist = (it.text("artist") ?: "").take(TEXT_CAP),
                )
            },
            volume = v?.let {
                val percent = it.int("percent") ?: return@let null
                Volume(percent.coerceIn(0, 100), it.bool("muted") == true)
            },
        )
    }

    /** Throws [AgentError] when the reply is one, and returns the object otherwise. */
    private fun require(reply: String, expected: String): JsonObject {
        val o = runCatching { json.parseToJsonElement(reply) }.getOrNull() as? JsonObject
            ?: throw AgentError("internal", "the computer sent something that is not a reply: ${reply.take(200)}")
        return when (val tag = o.text("reply")) {
            expected -> o
            "error" -> throw AgentError(
                o.text("kind") ?: "internal",
                o.text("message") ?: "the computer refused and did not say why",
            )
            else -> throw AgentError("internal", "expected a `$expected` reply and got `$tag`")
        }
    }

    private fun JsonObject.prim(key: String): JsonPrimitive? = (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }

    private fun JsonObject.text(key: String): String? = prim(key)?.takeIf { it.isString }?.content

    private fun JsonObject.int(key: String): Int? = prim(key)?.takeIf { !it.isString }?.intOrNull

    private fun JsonObject.bool(key: String): Boolean? = prim(key)?.takeIf { !it.isString }?.booleanOrNull
}

enum class MediaAction(val wire: String) {
    PLAY_PAUSE("play_pause"),
    NEXT("next"),
    PREVIOUS("previous"),
}

data class Battery(val percent: Int, val state: String) {
    val charging: Boolean get() = state == "charging"
}

/**
 * What the computer said about itself. Every field is null when the machine
 * could not read it in time, and the screen leaves that line out rather than
 * guessing.
 */
data class MachineStatus(
    val battery: Battery?,
    val onAc: Boolean?,
    val powerMode: String?,
    val locked: Boolean?,
    val update: String?,
) {
    /** "81 %, charging", "Plugged in", or null when there is nothing to say. */
    val power: String?
        get() = when {
            battery != null -> buildString {
                append("${battery.percent} %")
                when (battery.state) {
                    "charging" -> append(", charging")
                    "full" -> append(", full")
                    "not charging" -> append(", plugged in, not charging")
                    else -> if (onAc == true) append(", plugged in")
                }
            }
            onAc == true -> "Plugged in"
            else -> null
        }

    /** rimed's power tier in words, or null. */
    val mode: String?
        get() = powerMode?.let {
            when (it) {
                "performance" -> "Performance"
                "balanced" -> "Balanced"
                "power-saver" -> "Power saver"
                else -> it.replaceFirstChar(Char::uppercase)
            }
        }

    /** The update line, or null when there is nothing waiting. */
    val updateLine: String?
        get() = when (update) {
            "available" -> "An update is ready"
            "downloading" -> "Downloading an update"
            "installing" -> "Installing an update"
            "deferred" -> "An update is waiting"
            "restart" -> "Restart to finish the update"
            "failed" -> "The last update failed"
            else -> null
        }
}

data class Player(val name: String, val status: String, val title: String, val artist: String) {
    val playing: Boolean get() = status == "playing"
}

data class Volume(val percent: Int, val muted: Boolean)

data class MediaState(val player: Player?, val volume: Volume?)

/**
 * The five verbs over one machine's control connection.
 *
 * Which ones may be sent again after a dropped connection is decided per verb,
 * as [MachineLink.request] asks: reading is a question; locking and setting a
 * volume to a number land in the same place however often they arrive. A
 * media action moves something (play and pause toggles, next skips a track),
 * and a second copy would undo or repeat the first, so it is not retried and
 * the caller is told the connection went.
 */
class MachineControls(private val link: MachineLink) {
    fun status(): MachineStatus = Machine.readStatus(link.request(Machine.status()))

    fun lock(): Boolean = Machine.readLock(link.request(Machine.lock()))

    fun media(): MediaState = Machine.readMedia(link.request(Machine.media()))

    fun control(action: MediaAction): MediaState =
        Machine.readMedia(link.request(Machine.control(action), retry = false))

    fun volume(percent: Int): MediaState = Machine.readMedia(link.request(Machine.volume(percent)))

    fun mute(muted: Boolean): MediaState = Machine.readMedia(link.request(Machine.mute(muted)))
}
