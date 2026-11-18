package com.rimeos.remote.core.phone

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.Agentd
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.link.FakeMachine
import com.rimeos.remote.core.machine.Machine
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The companion link's gate: a session dialled with the companion key never
 * puts a non-device verb on the wire.
 *
 * Against [FakeMachine], whose `requests` is every line that actually reached
 * the "computer". A refusal that was raised AFTER sending would still pass a
 * test that only looked at the exception; these look at the wire.
 */
class CompanionLinkTest {
    private fun machine(helloScope: String? = null, answer: (String) -> String = { """{"reply":"ok"}""" }) =
        FakeMachine(
            control = { line ->
                when (CompanionLink.cmdOf(line.toByteArray())) {
                    "remote_hello" -> buildString {
                        append("""{"reply":"remote","version":1,"features":["companion_link","phone_report"]""")
                        if (helloScope != null) append(""","scope":"$helloScope"""")
                        append("}")
                    }
                    else -> answer(line)
                }
            },
            attachReply = { """{"reply":"offer","id":1}""" },
        )

    private fun link(fake: FakeMachine, scope: Scope) =
        MachineLink(connect = fake::open, silenceMs = 0, dialled = scope)

    /** Every request a companion session must never send, one of each family. */
    private val disallowed: List<Pair<String, (MachineLink) -> Unit>> = listOf(
        "hello" to { it.hello() },
        "list" to { it.sessions() },
        "signal" to { it.stop(1) },
        "input" to { it.input(1, "x") },
        "run" to { it.run("/tmp", 80, 24) },
        "clipboard" to { it.clipboard() },
        "clipboard_write" to { it.clipboardWrite("x") },
        "push_register" to { it.pushRegister("https://example.invalid/p", "k") },
        "push_unregister" to { it.pushUnregister() },
        "companion_register" to { it.companionRegister(ByteArray(32)) },
        "companion_forget" to { it.companionForget() },
        "machine_lock" to { it.request(Machine.lock(), retry = false) },
        "machine_status" to { it.request(Machine.status()) },
        "nex" to { it.request("""{"cmd":"nex","method":"session.list"}""") },
        "no cmd" to { it.request("""{"verb":"list"}""") },
    )

    @Test
    fun `a companion link refuses every non-device verb without sending it`() {
        val fake = machine()
        val link = link(fake, Scope.COMPANION)
        link.remoteHello()
        for ((name, call) in disallowed) {
            val e = assertThrows(AgentError::class.java, { call(link) }, name)
            assertEquals(CompanionLink.NOT_ALLOWED, e.kind, name)
            assertTrue(CompanionLink.isRefusal(e), name)
        }
        assertEquals(disallowed.size, link.refused)
        // Only the hello reached the computer.
        assertEquals(listOf("remote_hello"), fake.requests.map { CompanionLink.cmdOf(it.toByteArray()) })
        link.close()
    }

    @Test
    fun `a companion link sends the device verbs`() {
        val fake = machine()
        val link = link(fake, Scope.COMPANION)
        val phone = PhoneLink(link)
        link.remoteHello()
        phone.report(50, false, listOf(Phone.CAN_RING))
        phone.ack(3)
        link.request(Mirror.removed("k"))
        assertEquals(0, link.refused)
        val sent = fake.requests.map { CompanionLink.cmdOf(it.toByteArray()) }
        assertEquals(listOf("remote_hello", Phone.REPORT, "ring_ack", "phone_notification_removed"), sent)
        link.close()
    }

    @Test
    fun `a companion link opens the events channel and refuses a terminal`() {
        val fake = machine()
        val link = link(fake, Scope.COMPANION)
        val mux = link.shared()
        val e = assertThrows(AgentError::class.java) {
            mux.openChannel(mux.nextChannelId(), Agentd.attach(1, 80, 24).toByteArray())
        }
        assertEquals(CompanionLink.NOT_ALLOWED, e.kind)
        assertThrows(AgentError::class.java) {
            mux.openChannel(mux.nextChannelId(), """{"cmd":"nex","method":"feed"}""".toByteArray())
        }
        mux.openChannel(mux.nextChannelId(), """{"cmd":"${Phone.EVENTS}"}""".toByteArray())
        assertEquals(2, link.refused)
        assertEquals(listOf(Phone.EVENTS), fake.requests.map { CompanionLink.cmdOf(it.toByteArray()) })
        link.close()
    }

    @Test
    fun `an upload never dials on a companion link`() {
        val fake = machine()
        val link = link(fake, Scope.COMPANION)
        assertThrows(AgentError::class.java) { link.upload(1, "a.png", 1, { "x".byteInputStream() }) }
        assertEquals(0, fake.connections.get())
        assertEquals(1, link.refused)
    }

    @Test
    fun `a full link whose hello says companion narrows`() {
        val fake = machine(helloScope = "companion")
        val link = link(fake, Scope.FULL)
        assertEquals(Scope.FULL, link.scope)
        link.remoteHello()
        assertEquals(Scope.COMPANION, link.scope)
        assertThrows(AgentError::class.java) { link.sessions() }
        assertFalse(fake.requests.any { it.contains("\"list\"") })
        link.close()
    }

    @Test
    fun `a companion link whose hello says full stays companion`() {
        val fake = machine(helloScope = "full")
        val link = link(fake, Scope.COMPANION)
        link.remoteHello()
        assertEquals(Scope.COMPANION, link.scope)
        assertThrows(AgentError::class.java) { link.sessions() }
        link.close()
    }

    @Test
    fun `a full link sends everything, the companion verbs included`() {
        val fake = machine()
        val link = link(fake, Scope.FULL)
        link.remoteHello()
        link.companionRegister(ByteArray(32) { 7 })
        link.companionForget()
        assertEquals(0, link.refused)
        val sent = fake.requests.toList()
        assertEquals("companion_register", CompanionLink.cmdOf(sent[1].toByteArray()))
        assertTrue(sent[1].contains("\"public_key\":\"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc\""), sent[1])
        assertEquals("companion_forget", CompanionLink.cmdOf(sent[2].toByteArray()))
        link.close()
    }

    @Test
    fun `the computer's not_allowed arrives as a refusal, not a crash`() {
        // A computer stricter than this phone's lists (a newer build that
        // dropped a verb): its sentence reaches the caller as the AgentError
        // it is, and the link stays up for the next request.
        val fake = machine { """{"reply":"error","kind":"not_allowed","message":"phone_report is not allowed on a companion link; unlock the phone to use it"}""" }
        val link = link(fake, Scope.COMPANION)
        val e = assertThrows(AgentError::class.java) {
            PhoneLink(link).report(1, false, emptyList())
        }
        assertTrue(CompanionLink.isRefusal(e))
        assertTrue(link.connected)
        assertEquals(0, link.refused)
        link.close()
    }

    @Test
    fun `scope only narrows`() {
        assertEquals(Scope.FULL, CompanionLink.effective(Scope.FULL, null))
        assertEquals(Scope.FULL, CompanionLink.effective(Scope.FULL, "full"))
        assertEquals(Scope.COMPANION, CompanionLink.effective(Scope.FULL, "companion"))
        assertEquals(Scope.COMPANION, CompanionLink.effective(Scope.COMPANION, "full"))
        assertEquals(Scope.COMPANION, CompanionLink.effective(Scope.COMPANION, null))
    }

    @Test
    fun `a companion session runs the device features and nothing else`() {
        assertEquals(
            setOf(
                CompanionLink.Feature.REPORT,
                CompanionLink.Feature.MIRROR,
                CompanionLink.Feature.MEDIA,
                CompanionLink.Feature.EVENTS,
            ),
            CompanionLink.features(Scope.COMPANION),
        )
        assertEquals(CompanionLink.Feature.entries.toSet(), CompanionLink.features(Scope.FULL))
    }

    @Test
    fun `the lists are the computer's`() {
        // rime-remoted/src/companion.rs ALLOWED_VERBS and ALLOWED_CHANNELS.
        assertEquals(
            setOf(
                "remote_hello", "phone_report", "ring_ack", "offer_ack", "offer_done",
                "phone_notification", "phone_notification_removed", "phone_media",
                "messages_reply", "messages_changed",
                "call_state", "call_done", "call_history_reply",
            ),
            CompanionLink.VERBS,
        )
        assertEquals(setOf("device_events", "take_offer"), CompanionLink.CHANNELS)
    }

    @Test
    fun `the boot start needs the switch and a key`() {
        assertTrue(CompanionLink.shouldStartAtBoot(switchOn = true, companionKeys = 1))
        assertFalse(CompanionLink.shouldStartAtBoot(switchOn = false, companionKeys = 2))
        assertFalse(CompanionLink.shouldStartAtBoot(switchOn = true, companionKeys = 0))
    }
}
