package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.Push
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PhoneTest {
    @Test
    fun `the report carries what is known and leaves out what is not`() {
        val full = Json.parseToJsonElement(Phone.report(62, true, listOf("ring"))).jsonObject
        assertEquals("phone_report", full["cmd"]!!.jsonPrimitive.content)
        assertEquals("62", full["battery"]!!.jsonPrimitive.content)
        assertEquals("true", full["charging"]!!.jsonPrimitive.content)
        assertEquals("""["ring"]""", full["can"].toString())

        val bare = Json.parseToJsonElement(Phone.report(null, null, emptyList())).jsonObject
        assertFalse("battery" in bare)
        assertFalse("charging" in bare)
        assertEquals("[]", bare["can"].toString())

        assertThrows(IllegalArgumentException::class.java) { Phone.report(101, null, emptyList()) }
        assertThrows(IllegalArgumentException::class.java) { Phone.report(-1, null, emptyList()) }
    }

    @Test
    fun `the ack and the channel open are the lines the contract names`() {
        assertEquals("""{"cmd":"ring_ack","id":7}""", Phone.ringAck(7))
        assertEquals("""{"cmd":"device_events"}""", Phone.events())
        assertTrue(Phone.opened("""{"reply":"events"}""".toByteArray()))
        assertFalse(Phone.opened("""{"reply":"error","code":"bad_request","message":"no"}""".toByteArray()))
        assertFalse(Phone.opened("not json".toByteArray()))
    }

    @Test
    fun `an error reply is an AgentError with the machine's words`() {
        Phone.readOk("""{"reply":"ok"}""")
        val e = assertThrows(AgentError::class.java) {
            Phone.readOk("""{"reply":"error","code":"bad_request","message":"battery is 0 to 100"}""")
        }
        assertEquals("bad_request", e.kind)
        assertEquals("battery is 0 to 100", e.message)
        assertThrows(AgentError::class.java) { Phone.readOk("""{"reply":"events"}""") }
    }

    @Test
    fun `a ring event parses and anything else is dropped`() {
        assertEquals(
            DeviceEvent.Ring(3, 1_700_000_000, "katana"),
            Phone.event("""{"event":"ring","id":3,"at":1700000000,"machine":"katana"}"""),
        )
        assertEquals(
            DeviceEvent.Ring(3, 1_700_000_000, ""),
            Phone.event("""{"event":"ring","id":3,"at":1700000000}"""),
        )
        assertNull(Phone.event("""{"event":"fly","id":3}"""))
        assertNull(Phone.event("""{"event":"ring","at":1}"""), "a ring with no id cannot be acknowledged")
        assertNull(Phone.event("""{"event":"ring","id":-1,"at":1}"""))
        assertNull(Phone.event("""{"event":"ring","id":"3","at":1}"""))
        assertNull(Phone.event("""{"event":"ring","id":3}"""))
        assertNull(Phone.event("garbage"))
    }

    @Test
    fun `lines split across chunks come out whole, and a runaway line breaks the feed`() {
        val lines = EventLines()
        assertEquals(emptyList<String>(), lines.feed("""{"event":"ri""".toByteArray()))
        assertEquals(listOf("""{"event":"ring"}""", "b"), lines.feed("ng\"}\nb\nc".toByteArray()))
        assertEquals(listOf("c"), lines.feed("\n".toByteArray()))
        assertThrows(AgentError::class.java) { EventLines().feed(ByteArray(Phone.MAX_LINE + 1) { 'x'.code.toByte() }) }
    }

    @Test
    fun `a push ring is fresh for a minute either side, and no longer`() {
        val now = 1_700_000_000L
        assertTrue(Phone.freshPush(now, now))
        assertTrue(Phone.freshPush(now - 60, now))
        assertFalse(Phone.freshPush(now - 61, now))
        assertTrue(Phone.freshPush(now + 60, now))
        assertFalse(Phone.freshPush(now + 61, now))
    }

    @Test
    fun `the battery is reported first, on a charger change, on a 5 point move, or every 10 minutes`() {
        val t = BatteryThrottle()
        assertTrue(t.due(80, false, 0))
        t.sent(80, false, 0)
        assertFalse(t.due(80, false, 1_000))
        assertFalse(t.due(76, false, 1_000))
        assertTrue(t.due(75, false, 1_000))
        assertTrue(t.due(85, false, 1_000))
        assertTrue(t.due(80, true, 1_000))
        assertTrue(t.due(null, false, 1_000))
        assertFalse(t.due(80, false, 10 * 60_000L - 1))
        assertTrue(t.due(80, false, 10 * 60_000L))
        t.reset()
        assertTrue(t.due(80, false, 1_000), "a new connection reports at once")
    }

    @Test
    fun `the same ring from the same machine rings once`() {
        val m = RingMemory()
        assertTrue(m.first("a", 100))
        assertFalse(m.first("a", 100))
        assertTrue(m.first("b", 100))
        assertTrue(m.first("a", 101))
    }

    @Test
    fun `push kind 10 is a ring`() {
        assertEquals(Alert.Kind.RING, Push.kindOf(10))
        assertEquals(10, Push.codeOf(Alert.Kind.RING))
        assertNull(Push.kindOf(11))
    }
}
