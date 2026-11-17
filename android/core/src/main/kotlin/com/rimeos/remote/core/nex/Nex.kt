package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentError
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.util.UUID

/**
 * nex, the agent harness, as `rime-remoted` carries it (contract §2.3).
 *
 * ## Two layers of "no"
 *
 * `rime-remoted` answers `{"reply":"nex","response":<JSON-RPC response>}`.
 * When nexd itself refused, the refusal is INSIDE `response` as a JSON-RPC
 * error ([NexRefused]): `-32002` is "not from a paired device", `-32601` is "not
 * built into this nexd". When the bridge could not reach nexd at all, the reply
 * is the ordinary `{"reply":"error","kind":"unavailable"}` and arrives as the
 * [AgentError] every other verb throws. The two mean different things to a
 * screen: the first is an answer about one action, the second says nex is not
 * there right now.
 *
 * ## Writes carry their key from the start
 *
 * Every nexd method that changes state needs a `request_id`, and nexd runs a
 * request with a key it has already seen exactly once. So the key is minted
 * when the person taps ([NexCall.write]), and the line is fixed from then on:
 * [MachineLink.request][com.rimeos.remote.core.agent.MachineLink.request]'s
 * retry after a dropped connection, and a retry as a channel after
 * `too_large`, send the same bytes. A turn submitted twice because a reply was
 * lost is one turn.
 */
object Nex {
    /** In `remote_hello.features` while nexd answers. */
    const val FEATURE: String = "nex"

    /** nexd: this method is not available from a paired device. */
    const val FORBIDDEN: Int = -32002

    /** nexd: this method is in the protocol and not in this build. */
    const val NOT_BUILT: Int = -32601

    val json: Json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    /**
     * The `Open` for nex's global event feed.
     *
     * [afterGseq] null with [tail] true is "from now on, no backlog"; a number
     * resumes after the last event this phone saw.
     */
    fun events(afterGseq: Long?, tail: Boolean): String = buildJsonObject {
        put("cmd", "nex_events")
        if (afterGseq != null) put("after_gseq", afterGseq)
        if (tail) put("tail", true)
    }.toString()

    /** A nex channel opened: `{"reply":"streaming"}`. Parsed, not substring-matched. */
    fun streaming(reply: ByteArray): Boolean =
        runCatching { json.parseToJsonElement(reply.toString(Charsets.UTF_8)) }.getOrNull()
            .let { (it as? JsonObject)?.str("reply") == "streaming" }

    /**
     * The JSON-RPC `result` out of a control reply.
     *
     * Throws [AgentError] for the bridge's own refusals and [NexRefused] for
     * nexd's.
     */
    fun result(reply: String): JsonElement {
        val outer = runCatching { json.parseToJsonElement(reply) }.getOrNull() as? JsonObject
            ?: throw AgentError("internal", "the machine sent something that is not a reply: ${reply.take(200)}")
        return when (val tag = outer.str("reply")) {
            "nex" -> response(outer["response"] ?: JsonNull)
            "error" -> throw AgentError(
                outer.str("kind") ?: "internal",
                outer.str("message") ?: "the machine refused and did not say why",
            )
            else -> throw AgentError("internal", "expected a `nex` reply and got `$tag`")
        }
    }

    /** The `result` of one JSON-RPC response line, or its error as [NexRefused]. */
    fun response(response: JsonElement): JsonElement {
        val r = response as? JsonObject
            ?: throw AgentError("internal", "nex sent a response that is not an object")
        (r["error"] as? JsonObject)?.let { e ->
            throw NexRefused(e.long("code")?.toInt() ?: 0, e.str("message") ?: "nex refused and did not say why")
        }
        return r["result"] ?: throw AgentError("internal", "nex sent a response with no result")
    }
}

/**
 * One nexd request, its line fixed when it is made.
 *
 * [write] adds the idempotency key there and then, so every send of this
 * object is the same request (see [Nex]).
 */
class NexCall private constructor(val method: String, val params: JsonObject) {
    val isWrite: Boolean get() = params.containsKey("request_id")

    /** The control line. Built once: a retry must be byte-identical. */
    val line: String = buildJsonObject {
        put("cmd", "nex")
        put(
            "request",
            buildJsonObject {
                put("method", method)
                put("params", params)
            },
        )
    }.toString()

    override fun toString(): String = method

    companion object {
        fun read(method: String, params: JsonObject = JsonObject(emptyMap())): NexCall = NexCall(method, params)

        fun write(
            method: String,
            params: JsonObject,
            requestId: String = UUID.randomUUID().toString(),
        ): NexCall {
            require(requestId.isNotEmpty()) { "a write needs a request id" }
            return NexCall(method, JsonObject(params + ("request_id" to JsonPrimitive(requestId))))
        }
    }
}

/** nexd answered, and the answer was no. [message] is nexd's own sentence. */
class NexRefused(val code: Int, override val message: String) : Exception(message) {
    /** Not from a phone: the action exists, on the computer. */
    val forbidden: Boolean get() = code == Nex.FORBIDDEN

    /** This nexd has no such thing. A screen hides the feature rather than erroring. */
    val notBuilt: Boolean get() = code == Nex.NOT_BUILT
}
