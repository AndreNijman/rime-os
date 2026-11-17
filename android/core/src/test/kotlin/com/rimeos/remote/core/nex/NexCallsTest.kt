package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentStates
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The requests the screens send, against requests a real nexd took.
 *
 * `requests.json` is what the recorder sent as a declared device: each
 * request's parameter names and the error code nexd answered, null for
 * success. Every builder must match a request nexd carried out for a phone.
 * A builder that names a parameter nexd never took fails here, and so does one
 * whose only recorded match was refused (forbidden, missing key). A test that
 * compared a builder with a string written beside it could catch neither.
 */
class NexCallsTest {
    /** method → every parameter-name set nexd accepted for it. */
    private val accepted: Map<String, Set<Set<String>>> =
        Nex.json.parseToJsonElement(Fixtures.text("requests.json")).jsonArray
            .map { it.jsonObject }
            .filter { it["error"] is kotlinx.serialization.json.JsonNull }
            .groupBy({ it["method"]!!.jsonPrimitive.content }) { r ->
                r["params"]!!.jsonArray.map { it.jsonPrimitive.content }.toSet()
            }
            .mapValues { it.value.toSet() }

    private fun keys(c: NexCall): Set<String> =
        Nex.json.parseToJsonElement(c.line).jsonObject["request"]!!.jsonObject["params"]!!.jsonObject.keys

    private fun assertTaken(c: NexCall) {
        val sets = accepted[c.method]
            ?: error("no successful ${c.method} from a device on record: add it to tools/record-nex-fixtures.py")
        assertTrue(keys(c) in sets, "${c.method} ${keys(c)} is not a request nexd carried out: $sets")
    }

    private val approval = NexApproval.pending(Fixtures.result("tools.pending.json")).single()

    @Test
    fun `every builder sends a request nexd accepted`() {
        listOf(
            NexCalls.sessions(), NexCalls.pending(), NexCalls.missions(), NexCalls.mission("m"),
            NexCalls.usage(), NexCalls.connections(), NexCalls.transcript("s"),
            NexCalls.approve(approval), NexCalls.deny(approval),
            NexCalls.submit("s", "go on"), NexCalls.cancel("s"),
            NexCalls.create("/r", "scripted", "A title"), NexCalls.create("/r", "scripted", null),
        ).forEach(::assertTaken)
        // Every mission action the screen can offer, from every state.
        listOf("draft", "planned", "approved", "active", "paused", "blocked", "verifying")
            .flatMap(NexMission::actions).toSet()
            .forEach { assertTaken(NexCalls.mission(it, "m")) }
    }

    @Test
    fun `writes carry a key and reads do not`() {
        assertTrue(NexCalls.approve(approval).isWrite)
        assertTrue(NexCalls.submit("s", "x").isWrite)
        assertTrue(!NexCalls.transcript("s").isWrite)
        assertTrue(!NexCalls.pending().isWrite)
    }

    @Test
    fun `an approval carries the fingerprint it was shown with`() {
        val params = Nex.json.parseToJsonElement(NexCalls.approve(approval).line)
            .jsonObject["request"]!!.jsonObject["params"] as JsonObject
        assertEquals(approval.fingerprint, params["fingerprint"]!!.jsonPrimitive.content)
    }

    @Test
    fun `nothing is sent that cannot be right`() {
        assertThrows(IllegalArgumentException::class.java) { NexCalls.submit("s", "  ") }
        assertThrows(IllegalArgumentException::class.java) { NexCalls.mission("system.shutdown", "m") }
    }

    @Test
    fun `a new session offers the provider that folder last used`() {
        val connections = NexConnection.list(Fixtures.result("connections.list.json"))
        assertTrue(connections.any { it.id == "scripted" && it.available && !it.native })
        val sessions = listOf(
            NexSession("a", null, "idle", null, "reviewer", "/r", "2026-01-01T00:00:00Z"),
            NexSession("b", null, "idle", null, "scripted", "/r", "2026-02-01T00:00:00Z"),
            NexSession("c", null, "idle", null, "gone", "/q", "2026-03-01T00:00:00Z"),
        )
        assertEquals("scripted", NexConnection.suggest("/r", sessions, connections))
        // A provider that is no longer there is not offered.
        assertEquals(connections.first { it.available }.id, NexConnection.suggest("/q", sessions, connections))
        assertNull(NexConnection.suggest("/r", sessions, emptyList()))
    }

    @Test
    fun `nex states wear the agent colours`() {
        assertEquals(AgentStates.PERMISSION_REQUEST, NexStates.agentState("running", waitingOnYou = true))
        assertEquals(AgentStates.WORKING, NexStates.agentState("running"))
        assertEquals(AgentStates.WORKING, NexStates.agentState("active"))
        assertEquals(AgentStates.COMPLETE, NexStates.agentState("idle"))
        assertEquals(AgentStates.WAITING_FOR_USER, NexStates.agentState("planned"))
        assertEquals(AgentStates.FAILED, NexStates.agentState("failed"))
        assertEquals("ready", NexStates.words("idle"))
        assertEquals("waiting for you", NexStates.words("awaiting_approval"))
    }
}
