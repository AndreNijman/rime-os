package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.agent.Push
import com.rimeos.remote.core.link.FakeMachine
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayOutputStream

/** What the computer sends this phone (`docs/remote-live-contract.md` §2.6). */
class OfferTest {
    @Test
    fun `an offer event parses, and one a phone would not take is dropped`() {
        val e = Phone.event("""{"event":"offer","id":4,"at":1789155000,"machine":"l16","kind":"file","name":"a.jpg","len":3000}""")
        assertEquals(DeviceEvent.Offer(4, 1789155000, "l16", OfferKind.FILE, "a.jpg", 3000), e)
        val t = Phone.event("""{"event":"offer","id":5,"at":1,"machine":"l16","kind":"text","name":"","len":8192}""")
        assertEquals(OfferKind.TEXT, (t as DeviceEvent.Offer).kind)

        // Too long for its kind, empty, an unknown kind, no id.
        assertNull(Phone.event("""{"event":"offer","id":5,"at":1,"kind":"text","len":8193}"""))
        assertNull(Phone.event("""{"event":"offer","id":5,"at":1,"kind":"file","len":0}"""))
        assertNull(Phone.event("""{"event":"offer","id":5,"at":1,"kind":"file","len":${Phone.MAX_FILE_BYTES + 1}}"""))
        assertNull(Phone.event("""{"event":"offer","id":5,"at":1,"kind":"app","len":3}"""))
        assertNull(Phone.event("""{"event":"offer","at":1,"kind":"file","len":3}"""))
    }

    @Test
    fun `a name from the computer cannot reach outside Downloads or carry control characters`() {
        assertEquals("a.jpg", Phone.cleanName("../../a.jpg"))
        assertEquals("evil.txt", Phone.cleanName("C:\\dir\\evil.txt"))
        assertEquals("bashrc", Phone.cleanName(".bashrc"))
        assertEquals("ab", Phone.cleanName("a\nb"))
        assertEquals("ab", Phone.cleanName("a\u0000b"))
        assertEquals("shared file", Phone.cleanName(""))
        assertEquals("shared file", Phone.cleanName(null))
        assertEquals("shared file", Phone.cleanName("dir/"))
        assertEquals(Phone.NAME_CAP, Phone.cleanName("x".repeat(500)).length)
    }

    @Test
    fun `the offer verbs are the lines the contract names`() {
        assertEquals("""{"cmd":"offer_ack","id":4}""", Phone.offerAck(4))
        assertEquals("""{"cmd":"offer_done","id":4}""", Phone.offerDone(4))
        assertEquals("""{"cmd":"take_offer","id":4}""", Phone.take(4))
        assertEquals(Taken(OfferKind.FILE, "a.jpg", 3), Phone.taken("""{"reply":"offer","kind":"file","name":"a.jpg","len":3}""".toByteArray()))
        val gone = assertThrows(AgentError::class.java) {
            Phone.taken("""{"reply":"error","code":"not_found","message":"that was taken back"}""".toByteArray())
        }
        assertEquals("not_found", gone.kind)
        assertThrows(AgentError::class.java) { Phone.taken("""{"reply":"offer","kind":"text","name":"","len":9000}""".toByteArray()) }
    }

    @Test
    fun `an offer is taken once, and one that failed is tried again`() {
        val m = OfferMemory(capacity = 2)
        assertTrue(m.claim("d", 1, 10))
        assertFalse(m.claim("d", 1, 10), "taken twice at once")
        m.finish("d", 1, 10, ok = false)
        assertTrue(m.claim("d", 1, 10), "a failed take was not let go")
        m.finish("d", 1, 10, ok = true)
        assertFalse(m.claim("d", 1, 10))
        assertTrue(m.wasTaken("d", 1, 10))
        // Same id, a different start (the machine restarted): a new offer. Another phone's: also new.
        assertTrue(m.claim("d", 1, 11))
        assertTrue(m.claim("e", 1, 10))
        m.finish("d", 1, 11, ok = true)
        m.finish("e", 1, 10, ok = true)
        assertFalse(m.wasTaken("d", 1, 10), "capacity 2 kept the oldest")
    }

    @Test
    fun `push kind 11 says something was sent`() {
        assertEquals(Alert.Kind.SENT, Push.kindOf(11))
        assertEquals(11, Push.codeOf(Alert.Kind.SENT))
    }

    private fun offering(body: ByteArray, kind: String = "file", len: Int = body.size, dropAfter: Int = -1) = FakeMachine(
        scrollback = body,
        attachReply = { """{"reply":"offer","kind":"$kind","name":"a.bin","len":$len}""" },
        closeAfterScrollback = true,
        dropAfterBytes = dropAfter,
        dropOnAttempts = if (dropAfter >= 0) setOf(1) else emptySet(),
        chunkBytes = 1000,
    )

    @Test
    fun `take hands over every byte in order, and only as many as were offered`() {
        val body = ByteArray(70_000) { (it % 251).toByte() }
        val machine = offering(body)
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val out = ByteArrayOutputStream()
            val taken = PhoneLink(link).take(4, { out.write(it) })
            assertEquals(Taken(OfferKind.FILE, "a.bin", body.size.toLong()), taken)
            assertArrayEquals(body, out.toByteArray())
        }
    }

    @Test
    fun `a take that ends short, or runs long, throws`() {
        val body = ByteArray(5000) { 7 }
        val short = offering(body, len = 6000)
        MachineLink({ short.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(Exception::class.java) { PhoneLink(link).take(4, {}) }
            assertTrue(e.message!!.contains("5000 of 6000"), e.message)
        }
        val long = offering(body, len = 4000)
        MachineLink({ long.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(AgentError::class.java) { PhoneLink(link).take(4, {}) }
            assertEquals("too_large", e.kind)
        }
    }

    @Test
    fun `a refused take throws the machine's words`() {
        val machine = FakeMachine(
            attachReply = { """{"reply":"error","code":"not_found","message":"no such offer"}""" },
        )
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(Exception::class.java) { PhoneLink(link).take(4, {}) }
            assertTrue(e.message!!.contains("no such offer"), e.message)
        }
    }

    @Test
    fun `a sink that fails stops the take`() {
        val machine = offering(ByteArray(10_000) { 1 })
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(java.io.IOException::class.java) {
                PhoneLink(link).take(4, { throw java.io.IOException("disk full") })
            }
            assertEquals("disk full", e.message)
        }
    }
}
