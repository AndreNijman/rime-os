package com.rimeos.remote.core.nex

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The feed's bytes, as the bridge forwards them, back into events. */
class NexFeedTest {
    private val first = Fixtures.text("feed.first.json").trim()
    private val event = Fixtures.text("feed.event.json").trim()

    @Test
    fun `a tail starts at nexd's last gseq and then follows`() {
        val f = NexFeed()
        val a = f.feed("$first\n".toByteArray())
        assertTrue(a.startedNow)
        assertTrue(a.events.isEmpty())
        assertEquals(Fixtures.feedStart, f.lastGseq)
        val b = f.feed("$event\n".toByteArray())
        assertFalse(b.startedNow)
        val e = b.events.single()
        assertEquals(Fixtures.feedStart + 1, e.gseq)
        assertEquals("session.state_changed", e.kind)
        assertTrue(e.stream!!.startsWith("session_"))
        assertEquals("plan", (e.payload["mode"] as kotlinx.serialization.json.JsonPrimitive).content)
        assertTrue(e.changesLists)
    }

    @Test
    fun `lines split across frames, a byte at a time, come out whole`() {
        val f = NexFeed()
        val bytes = "$first\n$event\n".toByteArray()
        val got = bytes.flatMap { f.feed(byteArrayOf(it)).events }
        assertEquals(listOf(Fixtures.feedEvent), got.map { it.gseq })
    }

    @Test
    fun `a backlog is read in its own shape, and nothing is handed out twice`() {
        // The global stream's backlog: `{gseq, event}` items in a response.
        val global = Fixtures.text("sessions.stream.global.json").replace("\n", "")
        val f = NexFeed(after = 3)
        val events = f.feed("$global\n".toByteArray()).events
        assertEquals(4L, events.first().gseq, "after 3 means from 4")
        assertEquals((4L..events.last().gseq).toList(), events.map { it.gseq })
        // The same events again, as notifications after a reconnect: all dropped.
        val repeat = "{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{\"gseq\":5,\"kind\":\"turn.started\"}}\n"
        assertTrue(f.feed(repeat.toByteArray()).events.isEmpty())
    }

    @Test
    fun `a refused stream ends the feed with nexd's words`() {
        val f = NexFeed()
        val e = assertThrows(NexFeedBroken::class.java) {
            f.feed("{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32002,\"message\":\"not from a phone\"}}\n".toByteArray())
        }
        assertEquals("not from a phone", e.message)
    }

    @Test
    fun `a line that never ends does not grow forever`() {
        val f = NexFeed(maxLine = 64)
        f.feed("$first\n".toByteArray())
        assertThrows(NexFeedBroken::class.java) { f.feed(ByteArray(65) { 'x'.code.toByte() }) }
    }

    @Test
    fun `lines that are not events are skipped, not fatal`() {
        val f = NexFeed()
        f.feed("$first\n".toByteArray())
        val noise = "not json\n{\"jsonrpc\":\"2.0\",\"method\":\"other\",\"params\":{}}\n" +
            "{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{\"kind\":\"x\"}}\n$event\n"
        assertEquals(listOf(Fixtures.feedEvent), f.feed(noise.toByteArray()).events.map { it.gseq })
    }
}
