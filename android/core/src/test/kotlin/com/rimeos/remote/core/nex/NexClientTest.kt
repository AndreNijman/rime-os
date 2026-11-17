package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.link.FakeMachine
import com.rimeos.remote.core.link.Mux
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * nex over a machine connection, against `FakeMachine` doing what
 * `rime-remoted`'s bridge does: `nex` answered on the control channel, and a
 * nex channel opened with `{"reply":"streaming"}`, its lines as `Data`.
 */
class NexClientTest {
    private val approved = """{"reply":"nex","response":${Fixtures.text("tools.approve.json").replace("\n", "")}}"""
    private val streaming = """{"reply":"streaming"}"""

    private fun submit() = NexCall.write("turns.submit", JsonObject(mapOf("text" to JsonPrimitive("go"))))

    @Test
    fun `a call is one control round trip`() {
        val machine = FakeMachine(control = { approved })
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val r = NexClient(link, shared = true).call(submit()).jsonObject
            assertEquals("approved", r["state"]!!.jsonPrimitive.content)
            assertEquals(1, machine.requests.size)
        }
    }

    @Test
    fun `a write lost with its connection is sent again as the same request`() {
        lateinit var machine: FakeMachine
        val n = AtomicInteger()
        machine = FakeMachine(control = { line ->
            if (n.getAndIncrement() == 0) {
                machine.hangUp()
                Thread.sleep(100)
            }
            approved
        })
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val call = submit()
            NexClient(link, shared = true).call(call)
            val sent = machine.requests.toList()
            assertEquals(2, sent.size, "$sent")
            // Byte-identical: the same request_id, so nexd runs it once.
            assertEquals(sent[0], sent[1])
            assertEquals(call.line, sent[1])
        }
    }

    @Test
    fun `an answer too big for a control frame is fetched as a channel with the same line`() {
        val machine = FakeMachine(
            control = { """{"reply":"error","kind":"too_large","message":"send it as a channel"}""" },
            attachReply = { streaming },
            scrollback = (Fixtures.text("sessions.get.json").replace("\n", "") + "\n").toByteArray(),
            chunkBytes = 512,
            closeAfterScrollback = true,
        )
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val call = NexCall.read("sessions.get", JsonObject(mapOf("session_id" to JsonPrimitive("s"))))
            val t = NexTranscript.parse(NexClient(link, shared = true).call(call))
            assertEquals("Fix add()", t.session.title)
            val sent = machine.requests.toList()
            assertEquals(listOf(call.line, call.line), sent)
        }
    }

    @Test
    fun `without mux_attach a too-large answer stays an error, and no channel is opened`() {
        val machine = FakeMachine(control = { """{"reply":"error","kind":"too_large","message":"big"}""" })
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(AgentError::class.java) {
                NexClient(link, shared = false).large(NexCall.read("sessions.get"))
            }
            assertEquals("too_large", e.kind)
            assertEquals(1, machine.requests.size)
        }
    }

    @Test
    fun `a large read goes straight to a one-shot channel and ends when it closes`() {
        val machine = FakeMachine(
            attachReply = { streaming },
            scrollback = (Fixtures.text("sessions.get.waiting.json").replace("\n", "") + "\n").toByteArray(),
            chunkBytes = 300,
            closeAfterScrollback = true,
        )
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val t = NexTranscript.parse(NexClient(link, shared = true).large(NexCall.read("sessions.get")))
            assertEquals(1, t.pending.size)
        }
    }

    @Test
    fun `a refused channel is the bridge's refusal, not a hang`() {
        val machine = FakeMachine(
            attachReply = { """{"reply":"error","kind":"unavailable","message":"nex is not running"}""" },
        )
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val e = assertThrows(AgentError::class.java) {
                NexClient(link, shared = true, oneShotTimeoutMs = 5_000).large(NexCall.read("sessions.get"))
            }
            assertEquals("unavailable", e.kind)
            // The connection is still good for the next request.
            assertEquals("""{"reply":"ok"}""", link.request("""{"cmd":"hello"}"""))
        }
    }

    @Test
    fun `the feed delivers events as they arrive, from where it was asked to start`() {
        val feed = Fixtures.text("feed.first.json").trim() + "\n" + Fixtures.text("feed.event.json").trim() + "\n"
        val machine = FakeMachine(attachReply = { streaming }, scrollback = feed.toByteArray(), chunkBytes = 17)
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            val got = LinkedBlockingQueue<NexEvent>()
            val started = CountDownLatch(1)
            val sub = NexClient(link, shared = true).subscribe(
                null,
                object : NexClient.FeedListener {
                    override fun onEvents(events: List<NexEvent>, startedNow: Boolean) {
                        if (startedNow) started.countDown()
                        got.addAll(events)
                    }

                    override fun onEnd(cause: Throwable?) = Unit
                },
            )
            assertTrue(started.await(5, TimeUnit.SECONDS))
            assertEquals(25L, got.poll(5, TimeUnit.SECONDS)?.gseq)
            assertEquals(25L, sub.lastGseq)
            sub.close()
            val open = machine.requests.single { it.contains("nex_events") }
            assertEquals(Nex.json.parseToJsonElement(Nex.events(null, tail = true)), Nex.json.parseToJsonElement(open))
        }
    }

    @Test
    fun `a terminal still refuses a streaming channel, the nex rule is per channel`() {
        val machine = FakeMachine(attachReply = { streaming }, scrollback = "abc".toByteArray(), closeAfterScrollback = true)
        MachineLink({ machine.open() }, silenceMs = 0).use { link ->
            fun open(accepted: ((ByteArray) -> Boolean)?): String {
                val got = ByteArrayOutputStream()
                val closed = CountDownLatch(1)
                val mux = link.shared()
                mux.openChannel(
                    mux.nextChannelId(),
                    """{"cmd":"attach","id":1}""".toByteArray(),
                    channelListener = object : Mux.Listener {
                        override fun onData(channel: UInt, bytes: ByteArray) = synchronized(got) { got.write(bytes) }
                        override fun onClose(channel: UInt, reason: String) = closed.countDown()
                        override fun onDisconnect(cause: Throwable?) = closed.countDown()
                    },
                    accepted = accepted,
                )
                closed.await(1, TimeUnit.SECONDS)
                // A request after it proves its frames have all been dispatched.
                link.request("""{"cmd":"hello"}""")
                return synchronized(got) { got.toString(Charsets.UTF_8) }
            }
            assertEquals("", open(null), "the connection's own rule: streaming is not attached")
            assertEquals("abc", open(Nex::streaming))
        }
    }
}
