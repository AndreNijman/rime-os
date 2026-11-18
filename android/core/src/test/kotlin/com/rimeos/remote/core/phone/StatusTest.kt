package com.rimeos.remote.core.phone

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The phone's network and Do Not Disturb, and Focus both ways (contract §2.9). */
class StatusTest {
    @Test
    fun `the report carries the network and dnd as the contract spells them, or leaves them out`() {
        val full = Json.parseToJsonElement(
            Phone.report(62, null, listOf("ring", "focus"), NetworkState(NetworkState.WIFI, 3), true),
        ).jsonObject
        assertEquals("""{"type":"wifi","signal":3}""", full["network"].toString())
        assertEquals("true", full["dnd"].toString())
        assertEquals("""["ring","focus"]""", full["can"].toString())

        // No bars known is no `signal`, never 0.
        val noBars = Json.parseToJsonElement(Phone.report(null, null, emptyList(), NetworkState(NetworkState.ETHERNET), false)).jsonObject
        assertEquals("""{"type":"ethernet"}""", noBars["network"].toString())
        assertEquals("false", noBars["dnd"].toString())

        val bare = Json.parseToJsonElement(Phone.report(null, null, emptyList())).jsonObject
        assertFalse("network" in bare)
        assertFalse("dnd" in bare)
    }

    @Test
    fun `only the contract's network words and bars can be built, so a report is never refused for them`() {
        for (t in listOf("wifi", "cellular", "ethernet", "none", "other")) NetworkState(t)
        assertEquals(NetworkState.TYPES, setOf("wifi", "cellular", "ethernet", "none", "other"))
        assertThrows(IllegalArgumentException::class.java) { NetworkState("bluetooth") }
        assertThrows(IllegalArgumentException::class.java) { NetworkState("wifi", 5) }
        assertThrows(IllegalArgumentException::class.java) { NetworkState("wifi", -1) }
        NetworkState("cellular", 0)
        NetworkState("cellular", 4)
    }

    @Test
    fun `the default network's kind comes from its transports, first match winning`() {
        fun of(connected: Boolean = true, wifi: Boolean = false, cell: Boolean = false, eth: Boolean = false, rssi: Int? = null, level: Int? = null) =
            NetworkState.of(connected, wifi, cell, eth, rssi, level)
        assertEquals(NetworkState("none"), of(connected = false, wifi = true, rssi = -50))
        assertEquals(NetworkState("wifi", 4), of(wifi = true, rssi = -50))
        // A VPN over Wi-Fi lists both: it is the Wi-Fi the person is on.
        assertEquals(NetworkState("wifi", 4), of(wifi = true, cell = true, rssi = -50))
        assertEquals(NetworkState("cellular", 2), of(cell = true, level = 2))
        assertEquals(NetworkState("cellular"), of(cell = true, level = NetworkState.UNSPECIFIED))
        assertEquals(NetworkState("cellular"), of(cell = true, level = 5))
        assertEquals(NetworkState("cellular"), of(cell = true, level = -1))
        assertEquals(NetworkState("cellular"), of(cell = true))
        assertEquals(NetworkState("ethernet"), of(eth = true, level = 3, rssi = -50))
        assertEquals(NetworkState("other"), of())
        assertEquals(NetworkState("wifi"), of(wifi = true, rssi = NetworkState.UNSPECIFIED))
        assertEquals(NetworkState("wifi"), of(wifi = true))
    }

    @Test
    fun `Wi-Fi bars follow Android's own five-level scale`() {
        assertEquals(4, NetworkState.wifiBars(-30))
        assertEquals(4, NetworkState.wifiBars(-55))
        assertEquals(3, NetworkState.wifiBars(-60))
        assertEquals(2, NetworkState.wifiBars(-70))
        assertEquals(1, NetworkState.wifiBars(-85))
        assertEquals(0, NetworkState.wifiBars(-100))
        assertEquals(0, NetworkState.wifiBars(-110))
        assertNull(NetworkState.wifiBars(-127), "Android's invalid reading is no reading")
        assertNull(NetworkState.wifiBars(NetworkState.UNSPECIFIED))
        assertNull(NetworkState.wifiBars(0))
        assertNull(NetworkState.wifiBars(127))
    }

    @Test
    fun `dnd is on for every filter but all, and unknown while Android does not know`() {
        assertNull(Focus.dnd(0))
        assertEquals(false, Focus.dnd(1)) // ALL
        assertEquals(true, Focus.dnd(2)) // PRIORITY
        assertEquals(true, Focus.dnd(3)) // NONE: what `cmd notification set_dnd on` sets
        assertEquals(true, Focus.dnd(4)) // ALARMS
    }

    @Test
    fun `a focus event is a real boolean or nothing`() {
        assertEquals(DeviceEvent.Focus(true), Phone.event("""{"event":"focus","dnd":true}"""))
        assertEquals(DeviceEvent.Focus(false), Phone.event("""{"event":"focus","dnd":false}"""))
        assertNull(Phone.event("""{"event":"focus","dnd":"true"}"""))
        assertNull(Phone.event("""{"event":"focus","dnd":1}"""))
        assertNull(Phone.event("""{"event":"focus","dnd":null}"""))
        assertNull(Phone.event("""{"event":"focus"}"""))
        assertNull(Phone.event("""{"event":"focus","dnd":{"on":true}}"""))
    }

    @Test
    fun `a focus line is applied only with the switch on, unlocked, and when the phone is not already there`() {
        assertTrue(Focus.apply(switchOn = true, unlocked = true, current = false, wanted = true))
        assertTrue(Focus.apply(switchOn = true, unlocked = true, current = true, wanted = false))
        assertTrue(Focus.apply(switchOn = true, unlocked = true, current = null, wanted = false), "unknown is not 'already there'")
        // State, not a toggle: the phone's own state again changes nothing.
        assertFalse(Focus.apply(switchOn = true, unlocked = true, current = true, wanted = true))
        assertFalse(Focus.apply(switchOn = true, unlocked = true, current = false, wanted = false))
        // A machine that still has an old report saying "focus" is not obeyed once the switch is off.
        assertFalse(Focus.apply(switchOn = false, unlocked = true, current = false, wanted = true))
        assertFalse(Focus.apply(switchOn = false, unlocked = true, current = true, wanted = false))
        assertFalse(Focus.apply(switchOn = true, unlocked = false, current = false, wanted = true))
    }

    @Test
    fun `dnd and the kind of network are reported at once, the signal at most once a minute`() {
        val wifi3 = NetworkState("wifi", 3)
        val t = ReportThrottle()
        assertTrue(t.due(80, false, 0, wifi3, false))
        t.sent(80, false, 0, wifi3, false)
        assertFalse(t.due(80, false, 1_000, wifi3, false))
        assertTrue(t.due(80, false, 1_000, wifi3, true), "Do Not Disturb turning on waited")
        assertTrue(t.due(80, false, 1_000, wifi3, null), "Do Not Disturb becoming unknown waited")
        assertTrue(t.due(80, false, 1_000, NetworkState("cellular", 3), false), "a new kind of network waited")
        assertTrue(t.due(80, false, 1_000, null, false))
        assertTrue(t.due(80, false, 1_000, NetworkState("none"), false))
        // Only the bars: not for a minute after the last report.
        assertFalse(t.due(80, false, 1_000, NetworkState("wifi", 2), false))
        assertFalse(t.due(80, false, 59_999, NetworkState("wifi"), false))
        assertTrue(t.due(80, false, 60_000, NetworkState("wifi", 2), false))
        assertFalse(t.due(80, false, 60_000, wifi3, false), "the same bars a minute on are not news")

        t.sent(80, false, 60_000, wifi3, true)
        assertFalse(t.due(80, false, 61_000, wifi3, true))
        assertTrue(t.due(80, false, 61_000, wifi3, false), "Do Not Disturb turning off waited")
        t.reset()
        assertTrue(t.due(80, false, 61_000, wifi3, true), "a reset is told at once")
    }

    @Test
    fun `a change to can is due even when a report in flight marked everything sent`() {
        val t = ReportThrottle()
        t.sent(80, false, 0, null, false, listOf("ring"))
        // The switch went on while that report was on its way: same readings, new `can`.
        assertTrue(t.due(80, false, 1_000, null, false, listOf("ring", "focus")))
        assertFalse(t.due(80, false, 1_000, null, false, listOf("ring")))
    }
}
