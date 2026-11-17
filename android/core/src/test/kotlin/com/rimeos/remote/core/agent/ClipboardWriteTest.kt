package com.rimeos.remote.core.agent

import com.rimeos.remote.core.link.FakeMachine
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout

/**
 * Putting text on the computer's clipboard ("Send to Rime", contract §1.8),
 * from the request on the wire to the byte count that comes back.
 */
class ClipboardWriteTest {
    @Test
    fun `the reply is read from the clipboard_written shape the daemon writes`() {
        assertEquals(12, Agentd.readClipboardWritten("""{"reply":"clipboard_written","bytes":12}"""))
    }

    @Test
    fun `a refusal is thrown in the daemon's words, and an old runtime reads as one`() {
        val refused = assertThrows(AgentError::class.java) {
            Agentd.readClipboardWritten(
                """{"reply":"error","kind":"permission_denied","message":"session 3 may not set this machine's clipboard"}""",
            )
        }
        assertTrue(refused.message.contains("may not set this machine's clipboard"), refused.message)
        val old = assertThrows(AgentError::class.java) {
            Agentd.readClipboardWritten(
                """{"reply":"error","kind":"bad_request","message":"unparseable request: unknown variant `clipboard_write`, expected one of `hello`"}""",
            )
        }
        assertTrue(Agentd.isTooOld(old), "an old runtime's refusal was not recognised as one")
    }

    @Test
    fun `a reply for another verb is refused`() {
        assertThrows(AgentError::class.java) { Agentd.readClipboardWritten("""{"reply":"clipboard","text":"x"}""") }
    }

    @Test
    @Timeout(30)
    fun `MachineLink sends exactly the request the daemon parses, once`() {
        val machine = FakeMachine(name = "l16", control = { """{"reply":"clipboard_written","bytes":5}""" })
        MachineLink(connect = { machine.open() }).use { link ->
            assertEquals(5, link.clipboardWrite("héllo".substring(0, 4)))
        }
        assertEquals(listOf("""{"cmd":"clipboard_write","text":"héll"}"""), machine.requests.toList())
    }
}
