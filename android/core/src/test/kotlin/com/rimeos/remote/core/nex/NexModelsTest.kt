package com.rimeos.remote.core.nex

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** nexd's answers, recorded from a real nexd, read into what the screens show. */
class NexModelsTest {
    @Test
    fun `sessions list with their state and the folder they work in`() {
        // The first made on the computer, the second by the phone in the same folder.
        val list = NexSession.list(Fixtures.result("sessions.list.waiting.json"))
        assertEquals(setOf("Fix add()", "Second look"), list.map { it.label }.toSet())
        val s = list.single { it.label == "Fix add()" }
        assertTrue(s.id.startsWith("session_"))
        assertTrue(s.running)
        assertEquals("build", s.mode)
        // The folders a phone may start a new session in: those, once each.
        assertEquals(listOf(s.root), NexSession.roots(list))
    }

    @Test
    fun `a session with no title is named after its folder`() {
        val s = NexSession("session_x", null, "idle", null, null, "/home/u/src/rime-os/", null)
        assertEquals("rime-os", s.label)
        assertEquals("session_x", s.copy(root = null).label)
    }

    @Test
    fun `a waiting approval says what it would do and what it needs`() {
        val a = NexApproval.pending(Fixtures.result("tools.pending.json")).single()
        assertTrue(a.id.startsWith("appr_"))
        assertEquals("run_command", a.tool)
        assertEquals("run `bash test.sh`", a.summary)
        assertEquals(listOf("process.exec"), a.scopes)
        assertFalse(a.native)
        assertTrue(a.sessionId!!.startsWith("session_"))
    }

    @Test
    fun `a transcript reads in order, tools beside the words that asked for them`() {
        val t = NexTranscript.parse(Fixtures.result("sessions.get.json"))
        assertEquals("Fix add()", t.session.title)
        val kinds = t.entries.map {
            when (it) {
                is NexTranscript.Entry.Said -> "said:${it.role}"
                is NexTranscript.Entry.ToolUse -> "tool:${it.name}"
                is NexTranscript.Entry.TurnEnd -> "end:${it.state}"
            }
        }
        assertEquals(
            listOf(
                "said:user", "said:assistant", "tool:edit_file",
                "said:assistant", "tool:run_command", "said:assistant", "end:completed",
            ),
            kinds,
        )
        val run = t.entries.filterIsInstance<NexTranscript.Entry.ToolUse>().single { it.name == "run_command" }
        assertEquals("succeeded", run.state)
        assertEquals(0L, run.exitCode)
        assertFalse(run.error)
        assertEquals("command: bash test.sh", run.args)
        val end = t.entries.last() as NexTranscript.Entry.TurnEnd
        assertEquals("verified", end.verification)
        assertEquals("Fixed add(); all tests pass.", end.text)
        assertFalse(t.running)
        assertTrue(t.pending.isEmpty())
    }

    @Test
    fun `a transcript stopped at an approval is running and carries the approval`() {
        val t = NexTranscript.parse(Fixtures.result("sessions.get.waiting.json"))
        assertTrue(t.running)
        val a = t.pending.single()
        assertEquals("run_command", a.tool)
        assertEquals("run `bash test.sh`", a.summary)
        assertEquals(t.session.id, a.sessionId)
        val end = t.entries.last() as NexTranscript.Entry.TurnEnd
        assertEquals("running", end.state)
    }

    @Test
    fun `missions list, and what a phone may do with each state`() {
        val m = NexMission.list(Fixtures.result("missions.list.json")).single()
        assertEquals("Add a subtract() helper", m.objective)
        assertEquals("planned", m.state)
        assertEquals(listOf("missions.approve", "missions.cancel"), NexMission.actions(m.state))
        assertEquals(listOf("missions.pause", "missions.cancel"), NexMission.actions("active"))
        assertEquals(listOf("missions.resume", "missions.cancel"), NexMission.actions("blocked"))
        assertTrue(NexMission.actions("complete").isEmpty())
        assertTrue(NexMission.terminal("cancelled"))
        val d = NexMissionDetail.parse(Fixtures.result("missions.get.json"))
        assertEquals(m.id, d.mission.id)
        assertEquals(listOf("impl"), d.tasks.map { it.id })
        assertEquals(listOf("true"), d.verify)
    }

    @Test
    fun `usage shows what was used and leaves out connections with nothing`() {
        val u = NexUsage.list(Fixtures.result("usage.list.json"))
        val scripted = u.single { it.connection == "scripted" }
        assertTrue(scripted.worthShowing)
        // nexd sends token totals as floats.
        assertTrue(scripted.tokens.single { it.metric == "input_tokens" }.total > 0)
        assertTrue(scripted.tokens.all { it.estimate })
        assertNull(scripted.quota.single().usedPercent)
        assertTrue(u.filter { it.connection != "scripted" }.none { it.worthShowing })
    }

    @Test
    fun `fields nexd adds, nulls and wrong types do not blank a screen`() {
        val weird = Nex.json.parseToJsonElement(
            """[{"session_id":"s1","state":7,"title":null,"new_field":{"x":1}},{"state":"idle"},"not a row",
               {"session_id":"s2","state":"idle","root":42}]""",
        )
        val list = NexSession.list(weird)
        assertEquals(listOf("s1", "s2"), list.map { it.id })
        assertEquals("unknown", list[0].state)
        assertNull(list[1].root)
        assertTrue(NexTranscript.parse(JsonArray(emptyList())).entries.isEmpty())
        assertTrue(NexTranscript.parse(JsonObject(emptyMap())).pending.isEmpty())
        val parts = Nex.json.parseToJsonElement(
            """{"session":{"id":"s","state":"idle","data":{}},"messages":[
                 {"seq":0,"turn_id":"t","message":{"role":"assistant","parts":[{"type":"image","url":"x"},{"type":"text","text":"hi"}]}}],
               "turns":[{"id":"t","state":"strange_new_state"}]}""",
        )
        val t = NexTranscript.parse(parts)
        assertEquals(2, t.entries.size)
        assertEquals("strange_new_state", (t.entries.last() as NexTranscript.Entry.TurnEnd).state)
    }
}
