package com.rimeos.remote.core.input

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class TouchpadFlowTest {
    private val on = TouchpadFacts(connected = true, supported = true, inputOn = true, phoneName = "Pixel 8")

    private fun ready(): TouchpadFlow {
        val f = TouchpadFlow(on)
        assertTrue(f.enter(0).open)
        f.opened(10, Opened.Ready)
        assertEquals(TouchpadPhase.Ready, f.phase)
        return f
    }

    @Test
    fun `entering opens the channel and the reply makes it ready`() {
        val f = TouchpadFlow(on)
        assertEquals(TouchpadStep(open = true), f.enter(0))
        assertEquals(TouchpadPhase.Connecting, f.phase)
        f.opened(5, Opened.Ready)
        assertEquals(TouchpadPhase.Ready, f.phase)
    }

    @Test
    fun `a computer without the touchpad says so instead of trying`() {
        val f = TouchpadFlow(on.copy(supported = false))
        assertFalse(f.enter(0).open)
        assertEquals(TouchpadPhase.Unsupported, f.phase)
    }

    @Test
    fun `before the hello it waits, then opens when the hello comes`() {
        val f = TouchpadFlow(on.copy(supported = null, inputOn = null))
        assertFalse(f.enter(0).open)
        assertEquals(TouchpadPhase.Connecting, f.phase)
        assertTrue(f.facts(10, on).open)
    }

    @Test
    fun `a hello saying input is off shows the command, not a dead pad`() {
        val f = TouchpadFlow(on.copy(inputOn = false))
        assertFalse(f.enter(0).open)
        assertEquals(TouchpadPhase.GrantOff("rime remote devices grant 'Pixel 8' input on"), f.phase)
        // "Check again" asks the computer itself.
        assertTrue(f.retry(100).open)
    }

    @Test
    fun `not allowed at open shows the command with the name the computer used`() {
        val f = TouchpadFlow(on)
        f.enter(0)
        f.opened(
            5,
            Opened.Refused(
                Refusal.NOT_ALLOWED,
                "this phone may not use this computer as a touchpad and keyboard; at the computer run " +
                    "`rime remote devices grant Pixel 8 input on`",
            ),
        )
        assertEquals(TouchpadPhase.GrantOff("rime remote devices grant 'Pixel 8' input on"), f.phase)
        assertFalse(f.channel)
        assertNull(f.dueAt(), "no retry by itself")
    }

    @Test
    fun `a companion link's refusal is shown as the computer said it`() {
        val f = TouchpadFlow(on)
        f.enter(0)
        val m = "a touchpad needs the phone's own key; unlock the phone to use it"
        f.opened(5, Opened.Refused(Refusal.NOT_ALLOWED, m))
        assertEquals(TouchpadPhase.Ended(Refusal.NOT_ALLOWED, m), f.phase)
    }

    @Test
    fun `stopped at open waits out the seconds the computer gave, and never retries by itself`() {
        val f = TouchpadFlow(on)
        f.enter(0)
        f.opened(
            1_000,
            Opened.Refused(Refusal.STOPPED, "this touchpad was stopped at the computer; try again in 12 s, or allow it there at once with `rime remote devices grant Pixel 8 input on`"),
        )
        assertEquals(TouchpadPhase.Stopped(13_000), f.phase)
        assertNull(f.dueAt())
        assertFalse(f.tick(60_000).open)
    }

    @Test
    fun `stopped while in use survives leaving and coming back inside the cool-down`() {
        val f = ready()
        f.notice(5_000, Notice.Closed(Refusal.STOPPED, "stopped at the computer"))
        assertEquals(TouchpadPhase.Stopped(5_000 + Input.COOL_DOWN_MS), f.phase)
        assertFalse(f.channel)
        f.leave()
        assertFalse(f.enter(10_000).open, "no retry inside the cool-down")
        assertEquals(TouchpadPhase.Stopped(5_000 + Input.COOL_DOWN_MS), f.phase)
        // A person may still ask; the computer then says how long is left.
        assertTrue(f.retry(11_000).open)
    }

    @Test
    fun `a fresh visit after the cool-down opens again`() {
        val f = ready()
        f.notice(0, Notice.Closed(Refusal.STOPPED, "stopped at the computer"))
        f.leave()
        assertTrue(f.enter(Input.COOL_DOWN_MS + 1).open)
    }

    @Test
    fun `locked, busy and unavailable are tried again on a timer while in front`() {
        for ((kind, wait) in listOf(
            Refusal.LOCKED to TouchpadFlow.RETRY_LOCKED_MS,
            Refusal.BUSY to TouchpadFlow.RETRY_BUSY_MS,
            Refusal.UNAVAILABLE to TouchpadFlow.RETRY_UNAVAILABLE_MS,
        )) {
            val f = TouchpadFlow(on)
            f.enter(0)
            f.opened(100, Opened.Refused(kind, "m"))
            assertEquals(TouchpadPhase.Waiting(kind, "m", 100 + wait), f.phase)
            assertEquals(100 + wait, f.dueAt())
            assertFalse(f.tick(100 + wait - 1).open)
            assertTrue(f.tick(100 + wait).open, "$kind")
        }
    }

    @Test
    fun `no retry once the screen has left`() {
        val f = TouchpadFlow(on)
        f.enter(0)
        f.opened(100, Opened.Refused(Refusal.LOCKED, "m"))
        f.leave()
        assertFalse(f.tick(100_000).open)
        assertNull(f.dueAt())
    }

    @Test
    fun `paused and resumed follow the computer`() {
        val f = ready()
        f.notice(20, Notice.Paused("locked"))
        assertEquals(TouchpadPhase.Paused, f.phase)
        f.notice(30, Notice.Resumed)
        assertEquals(TouchpadPhase.Ready, f.phase)
        f.notice(40, Notice.Released("lease"))
        assertEquals(TouchpadPhase.Ready, f.phase)
    }

    @Test
    fun `the grant lowered mid-session shows the command`() {
        val f = ready()
        f.notice(20, Notice.Closed(Refusal.NOT_ALLOWED, "this phone is no longer allowed to use this computer as a touchpad"))
        assertEquals(TouchpadPhase.GrantOff("rime remote devices grant 'Pixel 8' input on"), f.phase)
    }

    @Test
    fun `Gaming Mode mid-session waits and tries again, a broken rule needs a person`() {
        val f = ready()
        f.notice(20, Notice.Closed(Refusal.UNAVAILABLE, "Gaming Mode"))
        assertEquals(TouchpadPhase.Waiting(Refusal.UNAVAILABLE, "Gaming Mode", 20 + TouchpadFlow.RETRY_UNAVAILABLE_MS), f.phase)
        val g = ready()
        g.notice(20, Notice.Closed(Refusal.REFUSED, "more than 250 frames in a second"))
        assertEquals(TouchpadPhase.Ended(Refusal.REFUSED, "more than 250 frames in a second"), g.phase)
        assertNull(g.dueAt())
    }

    @Test
    fun `a lost link reopens only once reconnected and only if still in front`() {
        val f = ready()
        f.facts(50, on.copy(connected = false))
        assertEquals(TouchpadPhase.LinkLost, f.phase)
        assertFalse(f.channel)
        assertTrue(f.facts(500, on).open)

        val g = ready()
        g.facts(50, on.copy(connected = false))
        g.leave()
        assertFalse(g.facts(500, on).open)
    }

    @Test
    fun `a hello that newly allows the phone opens from the command screen, once`() {
        val f = TouchpadFlow(on.copy(inputOn = false))
        f.enter(0)
        assertTrue(f.facts(10, on).open)
        f.opened(20, Opened.Refused(Refusal.NOT_ALLOWED, ""))
        // The same hello again (any change of link state repeats it) does not loop.
        assertFalse(f.facts(30, on).open)
    }

    @Test
    fun `leaving closes an open channel, and a reply after leaving closes it at once`() {
        val f = ready()
        assertEquals(TouchpadStep(close = true), f.leave())
        val g = TouchpadFlow(on)
        g.enter(0)
        g.leave()
        assertEquals(TouchpadStep(close = true), g.opened(10, Opened.Ready))
    }

    @Test
    fun `a channel closed without a word is tried again while connected`() {
        val f = ready()
        f.channelClosed(100)
        assertEquals(TouchpadPhase.Waiting(Refusal.OTHER, "", 100 + TouchpadFlow.RETRY_OTHER_MS), f.phase)
    }
}
