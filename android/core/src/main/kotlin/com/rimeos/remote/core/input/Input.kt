package com.rimeos.remote.core.input

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * The phone as the computer's touchpad and keyboard: the wire
 * (`docs/remote-live-contract.md` §2.16).
 *
 * A channel opened with [OPEN] and answered `input_ready`, then one Data
 * frame per batch, `{"seq":N,"ev":[…]}`, and one JSON line per event back.
 * The computer's limits are here as the computer states them, and beside
 * them the lower ones this phone keeps to: the computer counts frames as
 * they ARRIVE, in one-second windows, so frames a stalled network held back
 * land together, and a phone that sent at the limit would be cut off by its
 * own resends.
 */
object Input {
    /** In `remote_hello.features` on a computer that takes the channel. */
    const val FEATURE: String = "phone_input"

    /** The channel's `Open` request. */
    const val OPEN: String = """{"cmd":"input","v":1}"""

    // The computer's limits (rime-remoted's input.rs). Over any of them ends the channel.
    const val MAX_EVENTS: Int = 64
    const val MAX_FRAME_BYTES: Int = 16 * 1024
    const val MAX_DELTA: Double = 2_000.0
    const val MAX_TEXT_CHARS: Int = 1_000
    const val MAX_TEXT_PER_SEC: Int = 4_000
    const val MAX_FRAMES_PER_SEC: Int = 250
    const val MAX_KEYMAPS_PER_SEC: Int = 20

    /** Distinct characters in one text keymap on the computer. */
    const val KEYMAP_CHARS: Int = 200

    /** The computer lets go of everything after this long without a frame while anything is held. */
    const val LEASE_MS: Long = 750

    /** How often a `hold` goes while anything is held: three to a lease. */
    const val HOLD_EVERY_MS: Long = 250

    /** A touchpad stopped at the computer is refused for this long. */
    const val COOL_DOWN_MS: Long = 30_000

    // What this phone keeps to: about half of each, so a stall that bunches
    // a second's frames into one arrival is still under the computer's count.
    const val FRAMES_PER_SEC: Int = 120
    const val TEXT_PER_SEC: Int = 2_000
    const val KEYMAPS_PER_SEC: Int = 10
    const val FRAME_BYTES: Int = 12 * 1024

    /** Evdev buttons, as the computer's stub logs them. */
    const val BTN_LEFT: Int = 272
    const val BTN_RIGHT: Int = 273
    const val BTN_MIDDLE: Int = 274

    /** Keys a phone may never press: power, sleep, wake-up, suspend. */
    val FORBIDDEN_KEYS: Set<Int> = setOf(116, 142, 143, 205)

    /** The computer's rule for `k` and `tap`, said again so nothing it would refuse is ever sent. */
    fun keyAllowed(code: Int): Boolean = code in 1..0xff && code !in FORBIDDEN_KEYS

    /** Whether the reply to [OPEN] means the channel is open. */
    fun accepted(reply: ByteArray): Boolean {
        val text = reply.toString(Charsets.UTF_8)
        return text.contains("\"reply\"") && text.contains("\"input_ready\"")
    }

    private val json = Json { ignoreUnknownKeys = true }

    /** The reply to [OPEN], read. */
    fun readOpen(reply: ByteArray): Opened {
        if (accepted(reply)) return Opened.Ready
        val o = runCatching { json.parseToJsonElement(reply.toString(Charsets.UTF_8)) as? JsonObject }.getOrNull()
        val kind = Refusal.of(o?.get("kind")?.str())
        val message = o?.get("message")?.str().orEmpty()
        return Opened.Refused(kind, message)
    }

    /** One line the computer sent on the channel, or null when it is not one this phone knows. */
    fun readEvent(line: String): Notice? {
        val o = runCatching { json.parseToJsonElement(line) as? JsonObject }.getOrNull() ?: return null
        val why = o["why"]?.str().orEmpty()
        return when (o["event"]?.str()) {
            "input_paused" -> Notice.Paused(why)
            "input_resumed" -> Notice.Resumed
            "input_released" -> Notice.Released(why)
            "input_closed" -> Notice.Closed(Refusal.of(why), o["message"]?.str().orEmpty())
            else -> null
        }
    }

    private fun JsonElement.str(): String? = (this as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull

    /**
     * The seconds a `stopped` refusal says are left ("try again in 12 s"),
     * or null when it says none: the close of a live channel does not.
     */
    fun secondsLeft(message: String): Int? =
        Regex("""try again in (\d{1,4}) s""").find(message)?.groupValues?.get(1)?.toIntOrNull()

    /**
     * The phone's name as the computer printed it in a refusal's command
     * (`… grant Pixel 8 input on`), or null.
     */
    fun nameInRefusal(message: String): String? =
        Regex("""grant (.{1,80}?) input on""").find(message)?.groupValues?.get(1)?.trim()?.takeIf { it.isNotEmpty() }

    /**
     * The command a person at the computer types to let this phone in, with
     * the name quoted for a shell when it needs to be: a phone's name is
     * often its model, and "Pixel 8" unquoted is two words.
     */
    fun grantCommand(name: String): String = "rime remote devices grant ${shellWord(name)} input on"

    fun shellWord(s: String): String =
        if (s.isNotEmpty() && s.all { it.isLetterOrDigit() && it.code < 128 || it in "._-@+=:,/%" }) {
            s
        } else {
            "'" + s.replace("'", "'\\''") + "'"
        }

    /** One batch, as the computer reads it. */
    fun frame(seq: Long, events: List<InputEvent>): String =
        buildJsonObject {
            put("seq", seq)
            put("ev", buildJsonArray { for (e in events) add(e.json()) })
        }.toString()

    /** A motion or scroll amount on the wire: hundredths, so the sum sent is the sum meant. */
    internal fun hundredths(d: Double): Double = Math.round(d * 100.0) / 100.0
}

/** What the reply to `Open` said. */
sealed class Opened {
    data object Ready : Opened()
    data class Refused(val kind: Refusal, val message: String) : Opened()
}

/** The words the computer uses for a refusal, or for why it ended a channel. */
enum class Refusal(val word: String) {
    NOT_ALLOWED("not_allowed"),
    BAD_REQUEST("bad_request"),
    UNAVAILABLE("unavailable"),
    BUSY("busy"),
    LOCKED("locked"),
    STOPPED("stopped"),
    /** `input_closed` with `refused`: a frame broke a rule. */
    REFUSED("refused"),
    /** `input_closed` with `stopping`: Rime Remote on the computer is stopping. */
    STOPPING("stopping"),
    OTHER("");

    companion object {
        fun of(word: String?): Refusal = entries.firstOrNull { it.word == word && it != OTHER } ?: OTHER
    }
}

/** An event line from the computer. */
sealed class Notice {
    data class Paused(val why: String) : Notice()
    data object Resumed : Notice()
    data class Released(val why: String) : Notice()
    data class Closed(val why: Refusal, val message: String) : Notice()
}

enum class MouseButton(val word: String, val evdev: Int) {
    LEFT("left", Input.BTN_LEFT),
    RIGHT("right", Input.BTN_RIGHT),
    MIDDLE("middle", Input.BTN_MIDDLE),
}

/** A modifier a `tap` may hold around its key. */
enum class Mod(val word: String) {
    CTRL("ctrl"),
    SHIFT("shift"),
    ALT("alt"),
    SUPER("super"),
    ALTGR("altgr"),
}

/** One event of a batch (§2.16's table). */
sealed class InputEvent {
    abstract fun json(): JsonObject

    data class Motion(val dx: Double, val dy: Double) : InputEvent() {
        override fun json() = buildJsonObject {
            put("t", "m")
            put("dx", dx)
            put("dy", dy)
        }
    }

    data class Button(val button: MouseButton, val pressed: Boolean) : InputEvent() {
        override fun json() = buildJsonObject {
            put("t", "b")
            put("b", button.word)
            put("p", pressed)
        }
    }

    /** [finger] is a two-finger drag (kinetic on the computer, ended by [stop]); otherwise a wheel. */
    data class Scroll(val dx: Double, val dy: Double, val finger: Boolean = true, val stop: Boolean = false) : InputEvent() {
        override fun json() = buildJsonObject {
            put("t", "s")
            put("dx", dx)
            put("dy", dy)
            put("src", if (finger) "finger" else "wheel")
            put("stop", stop)
        }
    }

    data class Key(val code: Int, val pressed: Boolean) : InputEvent() {
        init {
            require(Input.keyAllowed(code)) { "key $code may not be sent" }
        }

        override fun json() = buildJsonObject {
            put("t", "k")
            put("code", code)
            put("p", pressed)
        }
    }

    data class Tap(val code: Int, val mods: Set<Mod> = emptySet()) : InputEvent() {
        init {
            require(Input.keyAllowed(code)) { "key $code may not be sent" }
        }

        override fun json() = buildJsonObject {
            put("t", "tap")
            put("code", code)
            if (mods.isNotEmpty()) put("mods", JsonArray(Mod.entries.filter { it in mods }.map { JsonPrimitive(it.word) }))
        }
    }

    /** Committed text, already free of control characters (see [TypedText]). */
    data class Text(val s: String) : InputEvent() {
        override fun json() = buildJsonObject {
            put("t", "text")
            put("s", s)
        }
    }

    data object Hold : InputEvent() {
        override fun json() = buildJsonObject { put("t", "hold") }
    }
}

/** Reads a boolean field, for tests and for nothing else on the hot path. */
internal fun JsonObject.flag(name: String): Boolean? = this[name]?.jsonPrimitive?.booleanOrNull
