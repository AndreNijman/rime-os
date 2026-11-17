package com.rimeos.remote.ui.machine

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.machine.Battery
import com.rimeos.remote.core.machine.MachineStatus
import com.rimeos.remote.core.machine.MediaState
import com.rimeos.remote.core.machine.Volume
import java.util.concurrent.TimeoutException
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The rules the "This computer" screen follows, without a phone. */
class MachineUiStateTest {
    private val status = MachineStatus(Battery(50, "discharging"), false, "balanced", false, null)

    @Test
    fun `each section is offered only when the hello names it`() {
        val none = MachineUiState().offering(listOf("nex", "mux_attach"))
        assertFalse(none.offersStatus || none.offersLock || none.offersMedia)
        val all = MachineUiState().offering(listOf("machine_status", "machine_lock", "media"))
        assertTrue(all.offersStatus && all.offersLock && all.offersMedia)
        val some = MachineUiState().offering(listOf("machine_status"))
        assertTrue(some.offersStatus)
        assertFalse(some.offersLock || some.offersMedia)
    }

    @Test
    fun `a read that worked clears the last failure, and one that asked nothing keeps what was there`() {
        val s = MachineUiState(away = "gone").read(status, null)
        assertNull(s.away)
        assertTrue(s.loaded)
        assertEquals(status, s.status)
        val media = MediaState(null, Volume(10, false))
        val t = s.read(null, media)
        assertEquals(status, t.status, "a read without status must not wipe the status")
        assertEquals(media, t.media)
    }

    @Test
    fun `a confirmed lock shows as locked, an unconfirmed one says so and changes nothing`() {
        val asking = MachineUiState(status = status, acting = MachineUiState.LOCK)
        val yes = asking.locked(true)
        assertNull(yes.acting)
        assertEquals(true, yes.status?.locked)
        assertEquals("Locked", yes.notice)
        assertNull(yes.failure)
        val no = asking.locked(false)
        assertNull(no.acting)
        assertEquals(false, no.status?.locked)
        assertEquals("The computer was asked to lock, but its screen did not confirm it", no.failure)
        assertNull(no.notice)
    }

    @Test
    fun `after a drop a media action is reported as unknown, never as done or not done`() {
        val dropped = Disconnected("the connection ended")
        assertEquals(
            "The connection to l16 dropped, so it may or may not have done that",
            MachineUiState.failureOf(dropped, "l16", moved = true),
        )
        assertEquals("The connection to l16 dropped", MachineUiState.failureOf(dropped, "l16"))
        assertEquals("there is no screen to lock on this computer", MachineUiState.failureOf(AgentError("unavailable", "there is no screen to lock on this computer"), "l16"))
        assertEquals("l16 did not answer in time", MachineUiState.failureOf(TimeoutException(), "l16"))
    }

    @Test
    fun `dismissing clears the failure and the notice, nothing else`() {
        val s = MachineUiState(status = status, failure = "x", notice = "y", away = "z").dismissed()
        assertNull(s.failure)
        assertNull(s.notice)
        assertEquals("z", s.away, "a read failure goes when a read works, not when dismissed")
        assertEquals(status, s.status)
    }
}
