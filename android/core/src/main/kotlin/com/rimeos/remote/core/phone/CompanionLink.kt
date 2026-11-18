package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * What a session may do: everything, or only the device features.
 *
 * A session's scope is fixed by the key it was dialled with, and a machine's
 * hello can only narrow it (contract §2.1: `"scope":"companion"`). Nothing
 * here ever widens one: a companion session that became a full one would be
 * a session the person never unlocked.
 */
enum class Scope { FULL, COMPANION }

/**
 * The companion link (contract §2.10): a second key per machine that survives
 * a phone restart, scoped to the device features.
 *
 * ## The lists are the computer's, repeated
 *
 * [VERBS] and [CHANNELS] are `rime-remoted`'s `ALLOWED_VERBS` and
 * `ALLOWED_CHANNELS`, word for word. The computer refuses anything else with
 * [NOT_ALLOWED], and that refusal is its job; this side's job is never to ask.
 * A phone that sent a terminal verb on a companion session and relied on the
 * refusal would be a phone whose locked state depended on another machine's
 * build. So every request on a companion session is checked here first, and
 * a refused one is never written to the wire.
 *
 * Not on the lists, deliberately, as on the computer: `push_register` and
 * `push_unregister` (a push carries what agent sessions are doing), and
 * `companion_register` / `companion_forget` themselves (only the full key may
 * name or retire the companion key).
 */
object CompanionLink {
    /** The feature a machine's `remote_hello` lists when it takes a companion key. */
    const val FEATURE: String = "companion_link"

    /** The `kind` of the computer's refusal, and of this side's. */
    const val NOT_ALLOWED: String = "not_allowed"

    /** The control verbs a companion session may send. */
    val VERBS: Set<String> = setOf(
        "remote_hello",
        Phone.REPORT,
        "ring_ack",
        "offer_ack",
        "offer_done",
        "phone_notification",
        "phone_notification_removed",
        "phone_media",
        // Reading a phone's messages runs on a locked phone too, as the rest
        // of the device features do; `messages_sent` is not here, because a
        // send is the phone speaking for the person and waits for an unlock.
        "messages_reply",
        "messages_changed",
        // A ringing call is answered, declined and ended from a locked phone's
        // own lock screen, so from a computer too; a dial is refused on a
        // companion session, by this phone and by the computer.
        "call_state",
        "call_done",
        "call_history_reply",
    )

    /** The channels a companion session may open. */
    val CHANNELS: Set<String> = setOf(Phone.EVENTS, Phone.TAKE)

    private val json = Json { ignoreUnknownKeys = true }

    /** The `cmd` of a request line, or null when it has none. */
    fun cmdOf(line: ByteArray): String? = runCatching {
        val obj = json.parseToJsonElement(line.toString(Charsets.UTF_8)) as? JsonObject
        (obj?.get("cmd") as? JsonPrimitive)?.takeIf { it.isString }?.content
    }.getOrNull()

    /** May a session of [scope] send this control line? */
    fun allowsRequest(scope: Scope, line: ByteArray): Boolean =
        scope == Scope.FULL || cmdOf(line)?.let { it in VERBS } == true

    /** May a session of [scope] open a channel with this request? */
    fun allowsOpen(scope: Scope, request: ByteArray): Boolean =
        scope == Scope.FULL || cmdOf(request)?.let { it in CHANNELS } == true

    /** This side's refusal: the same kind and sentence the computer would give. */
    fun refusal(line: ByteArray): AgentError {
        val what = cmdOf(line) ?: "that request"
        return AgentError(NOT_ALLOWED, "$what is not allowed on a companion link; unlock the phone to use it")
    }

    /** Whether [e] is a companion refusal, from either side. */
    fun isRefusal(e: Throwable): Boolean = e is AgentError && e.kind == NOT_ALLOWED

    /** `companion_register`: [publicKey] is 32 bytes, sent base64url without padding. */
    fun register(publicKey: ByteArray): String {
        require(publicKey.size == 32) { "an X25519 public key is 32 bytes; this one is ${publicKey.size}" }
        val text = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(publicKey)
        return buildJsonObject {
            put("cmd", "companion_register")
            put("public_key", text)
        }.toString()
    }

    /** `companion_forget`: the machine retires this phone's companion key. */
    fun forget(): String = """{"cmd":"companion_forget"}"""

    /** The `kind` the computer answers when the key is another device's, or retired. */
    const val KEY_IN_USE: String = "key_in_use"

    /**
     * The scope a session is in: the key it was dialled with, narrowed by the
     * hello. A full key whose hello says "companion" is a companion session
     * (a computer that knows better); a companion key whose hello says "full"
     * is still a companion session (a computer that is wrong, or lying).
     */
    fun effective(dialled: Scope, helloScope: String?): Scope =
        if (dialled == Scope.COMPANION || helloScope == "companion") Scope.COMPANION else Scope.FULL

    /**
     * The phone-side features a session of [scope] runs.
     *
     * A companion session runs the device features only: the report, the
     * notification mirror, what is playing, and the events channel (ring,
     * offers, focus, dismissals). Never the agent screens, the terminal, nex,
     * machine control, or push registration.
     */
    fun features(scope: Scope): Set<Feature> = when (scope) {
        Scope.FULL -> Feature.entries.toSet()
        Scope.COMPANION -> setOf(Feature.REPORT, Feature.MIRROR, Feature.MEDIA, Feature.EVENTS)
    }

    /** What the phone runs over a session. */
    enum class Feature { REPORT, MIRROR, MEDIA, EVENTS, AGENT, TERMINAL, NEX, CONTROL, PUSH }

    /**
     * Whether the phone should start the companion link after a restart: only
     * when the person turned it on AND a companion key exists for at least one
     * machine. Either missing means the phone does nothing until it is opened.
     */
    fun shouldStartAtBoot(switchOn: Boolean, companionKeys: Int): Boolean = switchOn && companionKeys > 0
}
