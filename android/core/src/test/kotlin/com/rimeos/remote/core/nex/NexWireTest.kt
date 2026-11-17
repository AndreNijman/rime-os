package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentError
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The lines the phone sends to nex, and how it reads the two layers of answer.
 *
 * The answers are real: `core/src/test/resources/nex/` was recorded from a
 * scripted nexd talking to a declared device (`tools/record-nex-fixtures.py`),
 * so the refusals here are nexd's own words and codes, not ones written to
 * match this parser.
 */
class NexWireTest {
    private fun parse(line: String): JsonObject = Nex.json.parseToJsonElement(line).jsonObject

    @Test
    fun `a call is the bridge's nex verb around a method and its params`() {
        val c = NexCall.read("sessions.get", JsonObject(mapOf("session_id" to JsonPrimitive("s1"))))
        val line = parse(c.line)
        assertEquals("nex", line["cmd"]!!.jsonPrimitive.content)
        val request = line["request"]!!.jsonObject
        assertEquals("sessions.get", request["method"]!!.jsonPrimitive.content)
        assertEquals("s1", request["params"]!!.jsonObject["session_id"]!!.jsonPrimitive.content)
        // The bridge refuses anything else in the request and mints its own JSON-RPC id.
        assertEquals(setOf("method", "params"), request.keys)
        assertFalse(c.isWrite)
    }

    @Test
    fun `a write's key is made once, so every send of it is the same request`() {
        val c = NexCall.write("turns.submit", JsonObject(mapOf("text" to JsonPrimitive("go"))))
        val key = parse(c.line)["request"]!!.jsonObject["params"]!!.jsonObject["request_id"]!!.jsonPrimitive.content
        assertTrue(key.isNotEmpty())
        assertTrue(c.isWrite)
        assertEquals(c.line, c.line, "the line is a value, not rebuilt per send")
        val again = NexCall.write("turns.submit", JsonObject(mapOf("text" to JsonPrimitive("go"))))
        assertNotEquals(c.line, again.line, "a second tap is a second request")
        assertThrows(IllegalArgumentException::class.java) {
            NexCall.write("turns.submit", JsonObject(emptyMap()), requestId = "")
        }
    }

    @Test
    fun `the feed opens from a gseq, or from now with no backlog`() {
        assertEquals(parse("""{"cmd":"nex_events","after_gseq":41}"""), parse(Nex.events(41, tail = false)))
        assertEquals(parse("""{"cmd":"nex_events","tail":true}"""), parse(Nex.events(null, tail = true)))
    }

    @Test
    fun `only a streaming reply opens a nex channel`() {
        assertTrue(Nex.streaming("""{"reply":"streaming"}""".toByteArray()))
        assertFalse(Nex.streaming("""{"reply":"attached","id":1}""".toByteArray()))
        assertFalse(Nex.streaming("""{"reply":"error","kind":"unavailable","message":"streaming"}""".toByteArray()))
        assertFalse(Nex.streaming("not json".toByteArray()))
    }

    @Test
    fun `nexd's answer comes out of the bridge's envelope`() {
        val response = Fixtures.text("tools.approve.json")
        val r = Nex.result("""{"reply":"nex","response":$response}""").jsonObject
        assertEquals("approved", r["state"]!!.jsonPrimitive.content)
    }

    @Test
    fun `nexd's refusal is nexd's, with its code and its sentence`() {
        val local = assertThrows(NexRefused::class.java) {
            Nex.result("""{"reply":"nex","response":${Fixtures.text("error.local_only.json")}}""")
        }
        assertTrue(local.forbidden)
        assertTrue(local.message.contains("paired device"), local.message)
        val root = assertThrows(NexRefused::class.java) {
            Nex.result("""{"reply":"nex","response":${Fixtures.text("error.sessions.create.unknown_root.json")}}""")
        }
        assertTrue(root.forbidden)
        val notBuilt = assertThrows(NexRefused::class.java) {
            Nex.response(Nex.json.parseToJsonElement("""{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}"""))
        }
        assertTrue(notBuilt.notBuilt)
    }

    @Test
    fun `the bridge's own refusal is an AgentError, so nex being away reads as that`() {
        val e = assertThrows(AgentError::class.java) {
            Nex.result("""{"reply":"error","kind":"unavailable","message":"nex is not running on this computer"}""")
        }
        assertEquals("unavailable", e.kind)
        assertThrows(AgentError::class.java) { Nex.result("""{"reply":"sessions","sessions":[]}""") }
        assertThrows(AgentError::class.java) { Nex.result("garbage") }
    }
}

/** The recorded nexd answers in `core/src/test/resources/nex/`. */
internal object Fixtures {
    fun text(name: String): String =
        Fixtures::class.java.getResourceAsStream("/nex/$name")?.readBytes()?.toString(Charsets.UTF_8)
            ?: error("$name is missing from core/src/test/resources/nex/")

    /** The `result` of a recorded response. */
    fun result(name: String) = Nex.response(Nex.json.parseToJsonElement(text(name)))

    /** Where the recorded feed starts, and the gseq of the one event it then saw. */
    val feedStart: Long get() = result("feed.first.json").jsonObject["last_gseq"]!!.jsonPrimitive.content.toLong()
    val feedEvent: Long get() = Nex.json.parseToJsonElement(text("feed.event.json"))
        .jsonObject["params"]!!.jsonObject["gseq"]!!.jsonPrimitive.content.toLong()
}
