package com.rimeos.remote.ui.send

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Upload
import java.util.concurrent.TimeoutException
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The rules the "Send to Rime" screen follows, without a phone. */
class SendUiStateTest {
    private val l16 = listOf(SendChoice("dev-1", "l16"))

    @Test
    fun `text over the computer's clipboard limit is refused with the numbers, never cut`() {
        assertNull(SendUiState.textRefusal("a".repeat(8192)))
        // 4097 two-byte characters: under 8192 characters, over 8192 bytes.
        val wide = "é".repeat(4097)
        assertEquals(
            "That text is 8194 bytes, and a computer's clipboard takes up to 8192 from a phone. Nothing was sent.",
            SendUiState.textRefusal(wide),
        )
        val s = SendUiState(text = wide, machines = l16, chosen = "dev-1")
        assertFalse(s.canSend)
    }

    @Test
    fun `nothing can be sent before a computer is chosen, while describing, or after it finished`() {
        val text = SendUiState(text = "hi", machines = l16)
        assertFalse(text.canSend, "no computer chosen")
        assertTrue(text.copy(chosen = "dev-1").canSend)
        val files = SendUiState(files = listOf(SendFile("a.jpg", 10, null)), machines = l16, chosen = "dev-1")
        assertTrue(files.canSend)
        assertFalse(files.copy(describing = true).canSend)
        assertFalse(files.copy(sending = true).canSend)
        assertFalse(files.copy(done = true).canSend, "a finished send is never sent again from the same screen")
    }

    @Test
    fun `files with a problem are left out and the rest are sent`() {
        val s = SendUiState(
            files = listOf(SendFile("a", 5, null), SendFile("b", null, "no size"), SendFile("c", 9, null)),
            machines = l16,
            chosen = "dev-1",
        )
        assertEquals(listOf(0, 2), s.sendable)
        val none = s.copy(files = listOf(SendFile("b", null, "no size")))
        assertFalse(none.canSend)
    }

    @Test
    fun `the four ways a file can fail are four different sentences`() {
        val dropped = Disconnected("the connection ended")
        assertEquals(
            "a.jpg was not saved: not enough free space",
            SendUiState.failureOf(Upload.Refused("not enough free space"), "a.jpg", "l16", lastByte = false, opened = false),
        )
        assertEquals(
            "The connection to l16 ended while a.jpg was going over. It was not saved.",
            SendUiState.failureOf(dropped, "a.jpg", "l16", lastByte = false, opened = true),
        )
        assertEquals(
            "The connection to l16 ended after all of a.jpg went over, so it may or may not be on l16. Check before sending it again.",
            SendUiState.failureOf(TimeoutException(), "a.jpg", "l16", lastByte = true, opened = true),
        )
        assertEquals(
            "a.jpg could not be read on this phone (this app is no longer allowed to read it). It was not sent.",
            SendUiState.failureOf(Unreadable("this app is no longer allowed to read it"), "a.jpg", "l16", lastByte = false, opened = true),
        )
        assertEquals("Stopped. a.jpg was not saved.", SendUiState.failureOf(Stopped(), "a.jpg", "l16", false, true))
        assertTrue(
            SendUiState.failureOf(java.io.IOException("no route"), "a.jpg", "l16", false, opened = false)
                .startsWith("Could not reach l16"),
        )
        assertEquals(
            "The text was not saved: session 3 may not set this machine's clipboard",
            SendUiState.failureOf(AgentError("permission_denied", "session 3 may not set this machine's clipboard"), null, "l16", false, false),
        )
    }

    @Test
    fun `the summary names what the computer chose, numbered names included`() {
        assertNull(SendUiState.summary(emptyList(), "l16"))
        assertEquals(
            "Saved in l16's Downloads as IMG (2).jpg.",
            SendUiState.summary(listOf(Upload.Saved("IMG (2).jpg", "/x/IMG (2).jpg")), "l16"),
        )
        assertEquals(
            "Saved 2 files in l16's Downloads: a, b.",
            SendUiState.summary(listOf(Upload.Saved("a", "/x/a"), Upload.Saved("b", "/x/b")), "l16"),
        )
    }

    @Test
    fun `sizes read as megabytes`() {
        assertEquals("2.0 MB", SendUiState.megabytes(2L * 1024 * 1024))
        assertEquals("256 MB", SendUiState.megabytes(Upload.SAVE_MAX_BYTES))
        assertEquals("It is 300 MB, and the most a phone can send is 256 MB.", SendUiState.tooBig(300L * 1024 * 1024))
    }
}
