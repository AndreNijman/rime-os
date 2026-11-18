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
        // §2.8: a control from the computer, strictly.
        assertEquals(DeviceEvent.MediaControl("play_pause", null), Phone.event("""{"event":"media_control","action":"play_pause"}"""))
        assertEquals(DeviceEvent.MediaControl("next", null), Phone.event("""{"event":"media_control","action":"next","position_ms":5}"""))
        assertEquals(DeviceEvent.MediaControl("seek", 42_000), Phone.event("""{"event":"media_control","action":"seek","position_ms":42000}"""))
        assertNull(Phone.event("""{"event":"media_control","action":"seek"}"""), "a seek says where to")
        assertNull(Phone.event("""{"event":"media_control","action":"seek","position_ms":-1}"""))
        assertNull(Phone.event("""{"event":"media_control","action":"seek","position_ms":"42000"}"""))
        assertNull(Phone.event("""{"event":"media_control","action":"seek","position_ms":604800001}"""))
        assertNull(Phone.event("""{"event":"media_control","action":"stop"}"""))
        assertNull(Phone.event("""{"event":"media_control"}"""))
        assertNull(Phone.event("""{"event":"ring","at":1}"""), "a ring with no id cannot be acknowledged")
        assertNull(Phone.event("""{"event":"ring","id":-1,"at":1}"""))
        assertNull(Phone.event("""{"event":"ring","id":"3","at":1}"""))
        assertNull(Phone.event("""{"event":"ring","id":3}"""))
        assertNull(Phone.event("garbage"))
        val key = Mirror.key("0|org.example.chat|1|tag|10001")
        assertEquals(DeviceEvent.Dismissed(key), Phone.event("""{"event":"notification_dismissed","key":"$key"}"""))
        assertNull(Phone.event("""{"event":"notification_dismissed","key":"../etc"}"""))
        assertNull(Phone.event("""{"event":"notification_dismissed"}"""))
    }

    @Test
    fun `a reply event carries a key this phone gives and some text, within the cap`() {
        val key = Mirror.key("0|org.example.chat|1|tag|10001")
        assertEquals(
            DeviceEvent.Replied(key, "on my way\nsoon"),
            Phone.event("""{"event":"notification_reply","key":"$key","text":"on my way\nsoon"}"""),
        )
        // Exactly the cap, in characters: four bytes each, and the line still fits.
        val most = "😀".repeat(Mirror.REPLY_TEXT_CAP)
        val line = """{"event":"notification_reply","key":"$key","text":"$most"}"""
        assertEquals(DeviceEvent.Replied(key, most), Phone.event(line))
        assertEquals(listOf(line), EventLines().feed((line + "\n").toByteArray(Charsets.UTF_8)))
        assertNull(Phone.event("""{"event":"notification_reply","key":"$key","text":"${most}x"}"""))
        assertNull(Phone.event("""{"event":"notification_reply","key":"../etc","text":"hi"}"""))
        assertNull(Phone.event("""{"event":"notification_reply","key":"$key","text":""}"""))
        assertNull(Phone.event("""{"event":"notification_reply","key":"$key","text":7}"""))
        assertNull(Phone.event("""{"event":"notification_reply","key":"$key"}"""))
        assertNull(Phone.event("""{"event":"notification_reply","text":"hi"}"""))
        // Not in a log line by accident.
        assertTrue("on my way" !in DeviceEvent.Replied(key, "on my way").toString())
    }

    @Test
    fun `a clipboard line carries its text verbatim, sensitive only as a real boolean, up to the cap`() {
        assertEquals(
            DeviceEvent.Clipboard(" two\nlines ", false),
            Phone.event("""{"event":"clipboard","text":" two\nlines "}"""),
        )
        assertEquals(DeviceEvent.Clipboard("pw", true), Phone.event("""{"event":"clipboard","text":"pw","sensitive":true}"""))
        assertEquals(DeviceEvent.Clipboard("pw", false), Phone.event("""{"event":"clipboard","text":"pw","sensitive":false}"""))
        assertNull(Phone.event("""{"event":"clipboard","text":"pw","sensitive":"true"}"""))
        assertNull(Phone.event("""{"event":"clipboard","text":""}"""))
        assertNull(Phone.event("""{"event":"clipboard","text":7}"""))
        assertNull(Phone.event("""{"event":"clipboard"}"""))
        // The cap is in bytes of UTF-8, as the computer counts it.
        val most = "a".repeat(Phone.MAX_CLIPBOARD_BYTES)
        assertEquals(DeviceEvent.Clipboard(most, false), Phone.event("""{"event":"clipboard","text":"$most"}"""))
        assertNull(Phone.event("""{"event":"clipboard","text":"${most}a"}"""))
        assertNull(Phone.event("""{"event":"clipboard","text":"${"\u00e9".repeat(Phone.MAX_CLIPBOARD_BYTES / 2 + 1)}"}"""))
        // The longest line the computer sends (192 KiB with its newline) is read whole.
        val quotes = "\\\"".repeat(Phone.MAX_CLIPBOARD_BYTES) // every byte a quote, escaped
        val big = """{"event":"clipboard","text":"$quotes"}"""
        val fed = EventLines().feed((big + "\n").toByteArray(Charsets.UTF_8))
        assertEquals(DeviceEvent.Clipboard("\"".repeat(Phone.MAX_CLIPBOARD_BYTES), false), Phone.event(fed.single()))
        val longest = ByteArray(192 * 1024 - 1) { 'x'.code.toByte() } + '\n'.code.toByte()
        assertEquals(1, EventLines().feed(longest).size)
        // Not in a log line by accident.
        assertTrue("hunter2" !in DeviceEvent.Clipboard("hunter2", true).toString())
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
        val t = ReportThrottle()
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
        assertNull(Push.kindOf(12))
    }
}
