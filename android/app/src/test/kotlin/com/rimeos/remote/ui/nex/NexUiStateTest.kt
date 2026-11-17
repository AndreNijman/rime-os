package com.rimeos.remote.ui.nex

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexEvent
import com.rimeos.remote.core.nex.NexMission
import com.rimeos.remote.core.nex.NexRefused
import com.rimeos.remote.core.nex.NexSession
import com.rimeos.remote.core.nex.NexTranscript
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The rules the nex screens follow, without a phone. */
class NexUiStateTest {
    private fun ev(kind: String, stream: String?, vararg payload: Pair<String, String>) =
        NexEvent(1, kind, stream, JsonObject(payload.associate { it.first to JsonPrimitive(it.second) }))

    private fun session(id: String, state: String = "idle", created: String = "2026-01-01") =
        NexSession(id, id, state, "build", "scripted", "/r", created)

    private fun approval(id: String, session: String?) =
        NexApproval(id, session, "run_command", "run `x`", listOf("process.exec"), emptyList(), "fp", null, false)

    @Test
    fun `the reply being written is drawn as it arrives, for the open session only`() {
        val s = NexUiState(selected = "s1")
        val (a, r1) = s.withEvents(listOf(ev("message.delta", "s1", "text" to "Fix"), ev("message.delta", "s1", "text" to "ing")))
        assertEquals("Fixing", a.typing)
        assertFalse(r1.any, "words arriving are not a reason to re-read anything")
        val (b, _) = a.withEvents(listOf(ev("message.delta", "s2", "text" to "elsewhere")))
        assertEquals("Fixing", b.typing)
        // Complete: the transcript has the words now, so the live copy goes and it is re-read.
        val (c, r2) = b.withEvents(listOf(ev("message.completed", "s1")))
        assertEquals("", c.typing)
        assertTrue(r2.transcript)
    }

    @Test
    fun `events say which reads are out of date`() {
        val s = NexUiState(selected = "s1")
        assertTrue(s.withEvents(listOf(ev("session.state_changed", "s9"))).second.lists)
        assertFalse(s.withEvents(listOf(ev("session.state_changed", "s9"))).second.transcript)
        val tool = s.withEvents(listOf(ev("tool.approval_requested", "s1"))).second
        assertTrue(tool.lists && tool.transcript, "an approval shows in the list and in the session")
        val (ended, r) = s.copy(typing = "half").withEvents(listOf(ev("turn.cancelled", "s1")))
        assertEquals("", ended.typing, "a stopped turn leaves no half reply on the screen")
        assertTrue(r.transcript)
        assertFalse(s.withEvents(listOf(ev("message.delta", "s1", "text" to "x"))).second.lists)
    }

    @Test
    fun `a reply that never ends keeps its tail, not all of it`() {
        val big = "x".repeat(NexUiState.TYPING_CAP)
        val (s, _) = NexUiState(selected = "s1", typing = big).withEvents(listOf(ev("message.delta", "s1", "text" to "END")))
        assertEquals(NexUiState.TYPING_CAP, s.typing.length)
        assertTrue(s.typing.endsWith("END"))
    }

    @Test
    fun `sessions waiting on you come first, then running ones`() {
        val s = NexUiState(
            sessions = listOf(session("idle-new", created = "2026-03"), session("run", "running"), session("wait")),
            pending = listOf(approval("a", "wait")),
        )
        assertEquals(listOf("wait", "run", "idle-new"), s.ordered.map { it.id })
        assertTrue(s.waitingOnYou("wait"))
        assertFalse(s.waitingOnYou("run"))
    }

    @Test
    fun `a session's approvals come from the list and its transcript, once each`() {
        val t = NexTranscript(session("s1", "running"), emptyList(), listOf(approval("a", "s1"), approval("b", "s1")), 0)
        val s = NexUiState(transcript = t, pending = listOf(approval("a", "s1"), approval("c", "s2")))
        assertEquals(listOf("a", "b"), s.pendingFor("s1").map { it.id })
        assertEquals(listOf("c"), s.pendingFor("s2").map { it.id })
    }

    @Test
    fun `a section nexd has not built is left out, not shown empty`() {
        val s = NexUiState(unbuilt = setOf(NexUiState.MISSIONS))
        assertFalse(s.shows(NexUiState.MISSIONS))
        assertTrue(s.shows(NexUiState.USAGE))
    }

    @Test
    fun `each failure lands where it belongs`() {
        assertEquals(
            NexTrouble.Away("nex is not running on this computer"),
            NexTrouble.of(AgentError("unavailable", "nex is not running on this computer")),
        )
        assertEquals(NexTrouble.NotBuilt, NexTrouble.of(NexRefused(-32601, "no such method")))
        assertEquals(NexTrouble.Refused("not from a phone"), NexTrouble.of(NexRefused(-32002, "not from a phone")))
        assertEquals(NexTrouble.Refused("too big"), NexTrouble.of(AgentError("too_large", "too big")))
        assertTrue(NexTrouble.of(Disconnected("gone")) is NexTrouble.Dropped)
    }

    @Test
    fun `every mission action has words, and paths read as their folder`() {
        for (state in listOf("draft", "planned", "approved", "active", "paused", "blocked", "verifying")) {
            for (verb in NexMission.actions(state)) {
                assertFalse(missionVerbLabel(verb).startsWith("missions"), "$verb has no words")
            }
        }
        assertEquals("rime-os", folderName("/home/u/src/rime-os/"))
        assertEquals(null, folderName("/"))
        assertEquals("1.2k", compactCount(1_234.0))
        assertEquals("812", compactCount(812.0))
    }
}
