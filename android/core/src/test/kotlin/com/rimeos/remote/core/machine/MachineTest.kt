package com.rimeos.remote.core.machine

import com.rimeos.remote.core.agent.AgentError
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The phone's half of `docs/remote-live-contract.md` §2.4. */
class MachineTest {
    private fun obj(line: String) = Json.parseToJsonElement(line).jsonObject

    @Test
    fun `the lines are the contract's, field for field`() {
        assertEquals("""{"cmd":"machine_status"}""", Machine.status())
        assertEquals("""{"cmd":"machine_lock"}""", Machine.lock())
        assertEquals("""{"cmd":"media"}""", Machine.media())
        assertEquals(obj("""{"cmd":"media_control","action":"play_pause"}"""), obj(Machine.control(MediaAction.PLAY_PAUSE)))
        assertEquals(obj("""{"cmd":"media_control","action":"next"}"""), obj(Machine.control(MediaAction.NEXT)))
        assertEquals(obj("""{"cmd":"media_control","action":"previous"}"""), obj(Machine.control(MediaAction.PREVIOUS)))
        assertEquals(obj("""{"cmd":"volume","percent":55}"""), obj(Machine.volume(55)))
        assertEquals(obj("""{"cmd":"volume","muted":true}"""), obj(Machine.mute(true)))
    }

    @Test
    fun `a volume the machine would refuse is never sent`() {
        assertThrows(IllegalArgumentException::class.java) { Machine.volume(101) }
        assertThrows(IllegalArgumentException::class.java) { Machine.volume(-1) }
        assertEquals(obj("""{"cmd":"volume","percent":0}"""), obj(Machine.volume(0)))
        assertEquals(obj("""{"cmd":"volume","percent":100}"""), obj(Machine.volume(100)))
    }

    @Test
    fun `a full status reads as the contract's example`() {
        val s = Machine.readStatus(
            """{"reply":"machine_status","battery":{"percent":81,"state":"charging"},"on_ac":true,""" +
                """"power_mode":"balanced","locked":false,"update":"available"}""",
        )
        assertEquals(MachineStatus(Battery(81, "charging"), true, "balanced", false, "available"), s)
        assertEquals("81 %, charging", s.power)
        assertEquals("Balanced", s.mode)
        assertEquals("An update is ready", s.updateLine)
    }

    @Test
    fun `whatever the machine could not read stays unknown, not false or zero`() {
        val s = Machine.readStatus(
            """{"reply":"machine_status","battery":null,"on_ac":null,"power_mode":null,"locked":null,"update":null}""",
        )
        assertEquals(MachineStatus(null, null, null, null, null), s)
        assertNull(s.power)
        assertNull(s.mode)
        assertNull(s.updateLine)
        // Missing fields are read the same way as null ones.
        assertEquals(MachineStatus(null, null, null, null, null), Machine.readStatus("""{"reply":"machine_status"}"""))
    }

    @Test
    fun `a field of the wrong type is unknown, not a crash`() {
        val s = Machine.readStatus(
            """{"reply":"machine_status","battery":{"percent":"81","state":3},"on_ac":"yes",""" +
                """"power_mode":7,"locked":"no","update":{"x":1}}""",
        )
        assertEquals(MachineStatus(null, null, null, null, null), s)
    }

    @Test
    fun `power says what a person would`() {
        fun power(percent: Int?, state: String, ac: Boolean?) =
            MachineStatus(percent?.let { Battery(it, state) }, ac, null, null, null).power
        assertEquals("40 %", power(40, "discharging", false))
        assertEquals("100 %, full", power(100, "full", true))
        assertEquals("80 %, plugged in, not charging", power(80, "not charging", true))
        assertEquals("55 %, plugged in", power(55, "unknown", true))
        assertEquals("Plugged in", power(null, "", true))
        assertNull(power(null, "", false))
        // A percentage outside 0..100 is clamped, not shown as sent.
        assertEquals(Battery(100, "full"), Machine.readStatus("""{"reply":"machine_status","battery":{"percent":180,"state":"full"}}""").battery)
    }

    @Test
    fun `every update state rimed has either says something or nothing on purpose`() {
        fun line(u: String?) = MachineStatus(null, null, null, null, u).updateLine
        for (waiting in listOf("available", "downloading", "installing", "deferred", "restart", "failed")) {
            assertTrue(line(waiting) != null, "$waiting has no words")
        }
        assertNull(line("up-to-date"))
        assertNull(line("unknown"))
        assertNull(line(null))
        assertEquals("Power saver", MachineStatus(null, null, "power-saver", null, null).mode)
        assertEquals("Performance", MachineStatus(null, null, "performance", null, null).mode)
    }

    @Test
    fun `a lock is confirmed only when the screen said so`() {
        assertTrue(Machine.readLock("""{"reply":"machine_lock","locked":true}"""))
        assertFalse(Machine.readLock("""{"reply":"machine_lock","locked":false}"""))
        assertFalse(Machine.readLock("""{"reply":"machine_lock","locked":null}"""))
    }

    @Test
    fun `no screen to lock is the machine's own sentence`() {
        val e = assertThrows(AgentError::class.java) {
            Machine.readLock("""{"reply":"error","kind":"unavailable","message":"there is no screen to lock on this computer"}""")
        }
        assertEquals("unavailable", e.kind)
        assertEquals("there is no screen to lock on this computer", e.message)
    }

    @Test
    fun `media reads the player and the volume, and either may be missing`() {
        val m = Machine.readMedia(
            """{"reply":"media","player":{"name":"spotify","status":"playing","title":"Song","artist":"Band"},""" +
                """"volume":{"percent":40,"muted":false}}""",
        )
        assertEquals(MediaState(Player("spotify", "playing", "Song", "Band"), Volume(40, false)), m)
        assertTrue(m.player!!.playing)
        assertEquals(MediaState(null, null), Machine.readMedia("""{"reply":"media","player":null,"volume":null}"""))
        // A player with no name is not a player.
        assertNull(Machine.readMedia("""{"reply":"media","player":{"status":"playing"}}""").player)
    }

    @Test
    fun `a title longer than the machine's cap is cut to it`() {
        val long = "t".repeat(Machine.TEXT_CAP + 50)
        val m = Machine.readMedia("""{"reply":"media","player":{"name":"p","status":"paused","title":"$long","artist":""}}""")
        assertEquals(Machine.TEXT_CAP, m.player!!.title.length)
        assertFalse(m.player.playing)
    }

    @Test
    fun `the wrong reply, an error and garbage are each said as such`() {
        val wrong = assertThrows(AgentError::class.java) { Machine.readMedia("""{"reply":"machine_status"}""") }
        assertEquals("internal", wrong.kind)
        val refused = assertThrows(AgentError::class.java) {
            Machine.readMedia("""{"reply":"error","kind":"unavailable","message":"No players found"}""")
        }
        assertEquals("No players found", refused.message)
        val garbage = assertThrows(AgentError::class.java) { Machine.readStatus("not json") }
        assertEquals("internal", garbage.kind)
        val bare = assertThrows(AgentError::class.java) { Machine.readStatus("""{"reply":"error"}""") }
        assertEquals("the computer refused and did not say why", bare.message)
    }
}
