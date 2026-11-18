package com.rimeos.remote.core.phone

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** This phone's calls on a computer (`docs/remote-live-contract.md` §2.12). */
class CallsTest {
    private val all = Calls.Grants(switchOn = true, state = true, history = true, answer = true, dial = true)

    private fun parse(line: String): JsonObject = Json.parseToJsonElement(line).jsonObject

    private fun row(
        n: Long,
        number: String? = "5556",
        type: Int = Calls.TYPE_INCOMING,
        presentation: Int = Calls.PRESENTATION_ALLOWED,
        name: String? = null,
    ) = Calls.Row(number, presentation, name, type, 1_000L * n, n)

    @Test
    fun `a report says calls only with the switch and the phone state permission, and never dial on a companion link`() {
        assertEquals(listOf("calls", "calls_history", "calls_answer", "calls_dial"), Calls.can(all, companion = false))
        assertEquals(listOf("calls", "calls_history", "calls_answer"), Calls.can(all, companion = true))
        // The switch off: nothing, whatever is granted.
        assertEquals(emptyList<String>(), Calls.can(all.copy(switchOn = false), companion = false))
        // The phone cannot see its calls: nothing, whatever else is granted.
        assertEquals(emptyList<String>(), Calls.can(all.copy(state = false), companion = false))
        // Each permission is its own word.
        assertEquals(listOf("calls"), Calls.can(all.copy(history = false, answer = false, dial = false), companion = false))
        assertEquals(listOf("calls", "calls_dial"), Calls.can(all.copy(history = false, answer = false), companion = false))
        // Every word fits a report the computer keeps.
        for (w in Calls.can(all, companion = false)) assertTrue(w.length in 1..32, w)
    }

    @Test
    fun `the switch, the permissions, the lock and the call decide, in that order`() {
        val r = { g: Calls.Grants, unlocked: Boolean, action: String, state: String -> Calls.refusal(g, unlocked, action, state) }
        // The switch first: an older report does not let the machine act.
        for (a in Calls.ACTIONS) assertEquals("off", r(all.copy(switchOn = false), true, a, Calls.RINGING))
        for (a in Calls.ACTIONS) assertEquals("no_permission", r(all.copy(state = false), true, a, Calls.RINGING))
        assertEquals("no_permission", r(all.copy(answer = false), true, Calls.ANSWER, Calls.RINGING))
        assertEquals("no_permission", r(all.copy(answer = false), true, Calls.HANGUP, Calls.ACTIVE))
        assertEquals("no_permission", r(all.copy(dial = false), true, Calls.DIAL, Calls.IDLE))
        // Answering, declining and hanging up run on a locked phone, as on its lock screen.
        assertNull(r(all, false, Calls.ANSWER, Calls.RINGING))
        assertNull(r(all, false, Calls.DECLINE, Calls.RINGING))
        assertNull(r(all, false, Calls.HANGUP, Calls.ACTIVE))
        assertNull(r(all, false, Calls.HANGUP, Calls.RINGING))
        // Dialling never does.
        assertEquals("locked", r(all, false, Calls.DIAL, Calls.IDLE))
        assertNull(r(all, true, Calls.DIAL, Calls.IDLE))
        assertEquals("busy", r(all, true, Calls.DIAL, Calls.ACTIVE))
        assertEquals("busy", r(all, true, Calls.DIAL, Calls.RINGING))
        // Nothing to act on.
        assertEquals("no_call", r(all, true, Calls.ANSWER, Calls.IDLE))
        assertEquals("no_call", r(all, true, Calls.ANSWER, Calls.ACTIVE))
        assertEquals("no_call", r(all, true, Calls.DECLINE, Calls.ACTIVE))
        assertEquals("no_call", r(all, true, Calls.HANGUP, Calls.IDLE))
        assertEquals("failed", r(all, true, "mute", Calls.ACTIVE))
        // The history: the switch, both permissions, and the device features running.
        assertNull(Calls.historyRefusal(all, live = true))
        assertEquals("off", Calls.historyRefusal(all.copy(switchOn = false), live = true))
        assertEquals("no_permission", Calls.historyRefusal(all.copy(history = false), live = true))
        assertEquals("no_permission", Calls.historyRefusal(all.copy(state = false), live = true))
        assertEquals("locked", Calls.historyRefusal(all, live = false))
    }

    @Test
    fun `a dial goes to a written number, never to an emergency number`() {
        val none = { _: String -> false }
        assertEquals("5556", Calls.dialNumber("5556", none).getOrThrow())
        assertEquals("+15555215556", Calls.dialNumber("+1 (555) 521-5556", none).getOrThrow())
        assertEquals("failed", (Calls.dialNumber("call me", none).exceptionOrNull() as Messages.Refused).word)
        assertEquals("failed", (Calls.dialNumber(null, none).exceptionOrNull() as Messages.Refused).word)
        assertEquals("failed", (Calls.dialNumber("12", none).exceptionOrNull() as Messages.Refused).word)
        val emergency = { n: String -> n == "112" }
        assertEquals("emergency", (Calls.dialNumber("112", emergency).exceptionOrNull() as Messages.Refused).word)
    }

    @Test
    fun `a call is followed through Android's states, keeping who it is with`() {
        var now = Calls.Now.IDLE_NOW
        // Android says ringing first without the number, then with it.
        now = Calls.next(now, Calls.CALL_STATE_RINGING, null, 100)
        assertEquals(Calls.Now(Calls.RINGING, 100), now)
        now = Calls.next(now, Calls.CALL_STATE_RINGING, "5556", 101)
        assertEquals(Calls.Now(Calls.RINGING, 100, "5556"), now)
        // And the other order: a later numberless saying keeps the number and the time.
        now = Calls.next(now, Calls.CALL_STATE_RINGING, null, 102)
        assertEquals(Calls.Now(Calls.RINGING, 100, "5556"), now)
        now = now.copy(name = "Test Contact")
        // Answered: the same call, now active since then.
        now = Calls.next(now, Calls.CALL_STATE_OFFHOOK, null, 200)
        assertEquals(Calls.Now(Calls.ACTIVE, 200, "5556", "Test Contact"), now)
        now = Calls.next(now, Calls.CALL_STATE_IDLE, "5556", 300)
        assertEquals(Calls.Now(Calls.IDLE, 300), now)
        // A call placed from here goes straight to active, with nobody known.
        assertEquals(Calls.Now(Calls.ACTIVE, 400), Calls.next(now, Calls.CALL_STATE_OFFHOOK, null, 400))
        assertEquals(Calls.IDLE, Calls.stateOf(99))
    }

    @Test
    fun `the lines are the computer's`() {
        val ringing = parse(Calls.stateLine(Calls.Now(Calls.RINGING, 100, "5556", "Test Contact")))
        assertEquals("call_state", ringing["cmd"]!!.jsonPrimitive.content)
        assertEquals("ringing", ringing["state"]!!.jsonPrimitive.content)
        assertEquals("5556", ringing["number"]!!.jsonPrimitive.content)
        assertEquals("Test Contact", ringing["name"]!!.jsonPrimitive.content)
        assertEquals(100, ringing["since_ms"]!!.jsonPrimitive.long)
        // A numberless call leaves both out; idle names nobody.
        val bare = parse(Calls.stateLine(Calls.Now(Calls.RINGING, 5)))
        assertFalse("number" in bare || "name" in bare)
        val idle = parse(Calls.stateLine(Calls.Now(Calls.IDLE, 7, "5556", "x")))
        assertEquals(setOf("cmd", "state", "since_ms"), idle.keys)
        // A name that is only control characters is no name.
        assertFalse("name" in parse(Calls.stateLine(Calls.Now(Calls.RINGING, 5, "5556", "\u0007"))))

        assertEquals("""{"cmd":"call_done","id":3,"ok":true}""", Calls.done(3, null))
        assertEquals("""{"cmd":"call_done","id":3,"ok":false,"error":"no_call"}""", Calls.done(3, Calls.NO_CALL))
        assertEquals("""{"cmd":"call_history_reply","id":4,"error":"off"}""", Calls.refused(4, Calls.OFF))
        // A number never reaches a log line.
        assertFalse("5556" in Calls.Now(Calls.RINGING, 1, "5556", "Test Contact").toString())
        assertFalse("5556" in DeviceEvent.CallAction(1, "dial", "5556").toString())
        assertFalse("5556" in row(1).toString())
    }

    @Test
    fun `the history is the newest calls older than before, voicemail and unknown kinds left out`() {
        val rows = sequenceOf(
            row(9, type = Calls.TYPE_MISSED),
            row(8, type = Calls.TYPE_VOICEMAIL),
            row(7, type = Calls.TYPE_OUTGOING, name = "Test Contact"),
            row(6, type = Calls.TYPE_REJECTED),
            row(5, type = Calls.TYPE_BLOCKED),
            row(4, type = Calls.TYPE_ANSWERED_EXTERNALLY),
            row(3, type = 42),
            row(2, number = "", presentation = 2),
            row(1, number = "5554"),
        )
        val all = Calls.history(rows, before = null, limit = 50) { n -> if (n == "5554") "Second Contact" else null }
        assertEquals(listOf("missed", "outgoing", "rejected", "rejected", "incoming", "incoming", "incoming"), all.map { it.type })
        assertEquals("Test Contact", all[1].name)
        // Withheld: no number, no name looked up.
        assertEquals("", all[5].number)
        assertNull(all[5].name)
        assertEquals("Second Contact", all[6].name)
        // Page: older than 7,000 ms, two of them.
        val page = Calls.history(rows, before = 7_000, limit = 2)
        assertEquals(listOf(6_000L, 5_000L), page.map { it.dateMs })
        // A withheld number the log still holds is not shown.
        assertEquals("", Calls.history(sequenceOf(row(1, number = "5556", presentation = 3)), null, 5)[0].number)
    }

    @Test
    fun `an answer is cut into lines the link carries, all but the last saying more`() {
        val many = (1..50L).map { Calls.Entry("5556", "N".repeat(60), "incoming", it, it) }
        val small = Calls.historyReply(9, many.take(2))
        assertEquals(1, small.size)
        assertEquals(2, parse(small[0])["calls"]!!.jsonArray.size)
        assertFalse("more" in parse(small[0]))
        val empty = Calls.historyReply(9, emptyList())
        assertEquals("""{"cmd":"call_history_reply","id":9,"calls":[]}""", empty.single())
        // Fifty capped calls fit one line; oversized ones show the cut.
        assertEquals(1, Calls.historyReply(9, many).size)
        val big = (1..50L).map { Calls.Entry("5556", "N".repeat(2_000), "incoming", it, it) }
        val lines = Calls.historyReply(9, big)
        assertTrue(lines.size > 1, "${lines.size}")
        for (l in lines) assertTrue(l.toByteArray().size <= Calls.CHUNK_BYTES)
        val got = lines.flatMap { parse(it)["calls"]!!.jsonArray }
        assertEquals(50, got.size)
        lines.dropLast(1).forEach { assertTrue(parse(it)["more"]!!.jsonPrimitive.boolean) }
        val e = parse(small[0])["calls"]!!.jsonArray[0].jsonObject
        assertEquals(setOf("number", "name", "type", "date_ms", "duration_s"), e.keys)
    }

    @Test
    fun `the computer's events are read strictly`() {
        assertEquals(DeviceEvent.CallAction(3, "answer", null), Phone.event("""{"event":"call_action","id":3,"action":"answer"}"""))
        assertEquals(DeviceEvent.CallAction(3, "dial", "5556"), Phone.event("""{"event":"call_action","id":3,"action":"dial","number":"5556"}"""))
        // A dial needs a number; an action unknown here, or a bad id, is dropped.
        assertNull(Phone.event("""{"event":"call_action","id":3,"action":"dial"}"""))
        assertNull(Phone.event("""{"event":"call_action","id":3,"action":"dial","number":5556}"""))
        assertNull(Phone.event("""{"event":"call_action","id":3,"action":"mute"}"""))
        assertNull(Phone.event("""{"event":"call_action","id":-1,"action":"answer"}"""))
        assertEquals(DeviceEvent.CallHistoryRequest(4, null, 20), Phone.event("""{"event":"call_history_request","id":4,"limit":20}"""))
        assertEquals(DeviceEvent.CallHistoryRequest(4, 99, 50), Phone.event("""{"event":"call_history_request","id":4,"before":99,"limit":500}"""))
        assertNull(Phone.event("""{"event":"call_history_request","id":4,"limit":0}"""))
        assertNull(Phone.event("""{"event":"call_history_request","id":4,"before":"9","limit":5}"""))
    }
}
