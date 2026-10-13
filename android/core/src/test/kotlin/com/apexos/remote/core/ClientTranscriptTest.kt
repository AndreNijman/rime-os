package com.apexos.remote.core

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream

/**
 * The whole client conversation, against bytes a real desktop would have sent.
 *
 * `NoiseVectorTest` proves the handshake matches `snow`; this proves the bytes
 * that go *around* it do too — the plaintext hello byte, the `[u32][message]`
 * framing, the JSON the request is serialised to, and the answer being read
 * from inside the finished handshake rather than from a plaintext reply. The
 * far end is a recorded transcript and not a Kotlin responder, so nothing here
 * can pass by agreeing with itself.
 */
class ClientTranscriptTest {
    private val version = Vectors.version()
    private val desktopPublic = Vectors.hex("desktop_public_hex")
    private val deviceSecret = Vectors.hex("device_secret_hex")
    private val initiatorEphemeral = EphemeralSource.fixedForTestingOnly(Vectors.hex("initiator_ephemeral_hex"))

    /** The offer whose token the recorded pairing request was built with. */
    private fun recordedOffer() = PairingOffer(
        v = version,
        machine = "l16",
        key = Base64Url.encode(desktopPublic),
        token = Base64Url.encode(ByteArray(Pairing.TOKEN_BYTES) { 7 }),
        lan = listOf("192.168.1.10:7717"),
        relay = null,
        expiresMs = Long.MAX_VALUE,
    )

    private fun framed(message: ByteArray): ByteArray {
        val buf = ByteArrayOutputStream()
        Transport.writeMessage(buf, message)
        return buf.toByteArray()
    }

    @Test
    fun `pairing writes the exact bytes a desktop would read, and reads its answer`() {
        val input = ByteArrayInputStream(framed(Vectors.patternHex("nk", "m2_hex")))
        val output = ByteArrayOutputStream()
        val answer = Client.pair(
            input = input,
            output = output,
            offer = recordedOffer(),
            identity = InMemoryStaticKey(deviceSecret),
            deviceName = "pixel-8",
            userVerification = true,
            version = version,
            ephemerals = initiatorEphemeral,
        )
        // The whole outbound stream, byte for byte: `P`, then the length
        // prefix, then snow's own message 1.
        val expected = byteArrayOf(Transport.HELLO_PAIR) + framed(Vectors.patternHex("nk", "m1_hex"))
        assertEquals(Vectors.encodeHex(expected), Vectors.encodeHex(output.toByteArray()))
        assertTrue(answer.ok)
        assertEquals("l16", answer.machine)
        assertEquals("abcdefghijklmnop", answer.device)
    }

    @Test
    fun `a session writes the exact bytes a desktop would read, and learns the machine`() {
        val input = ByteArrayInputStream(framed(Vectors.patternHex("ik", "m2_hex")))
        val output = ByteArrayOutputStream()
        val session = Client.openSession(
            input = input,
            output = output,
            identity = InMemoryStaticKey(deviceSecret),
            desktopPublic = desktopPublic,
            version = version,
            ephemerals = initiatorEphemeral,
        )
        val expected = byteArrayOf(Transport.HELLO_SESSION) + framed(Vectors.patternHex("ik", "m1_hex"))
        assertEquals(Vectors.encodeHex(expected), Vectors.encodeHex(output.toByteArray()))
        assertEquals("l16", session.machine)
    }

    @Test
    fun `an app on the wrong protocol version refuses before it burns the offer`() {
        // A version mismatch is caught from the offer rather than from a
        // handshake that cannot complete, because a handshake attempt against a
        // live offer is an attempt the owner has to redo.
        val output = ByteArrayOutputStream()
        val e = assertThrows<PairingException> {
            Client.pair(
                input = ByteArrayInputStream(ByteArray(0)),
                output = output,
                offer = recordedOffer().copy(v = version + 1),
                identity = InMemoryStaticKey(deviceSecret),
                deviceName = "pixel-8",
                userVerification = true,
                version = version,
                ephemerals = initiatorEphemeral,
            )
        }
        assertTrue(e.error is PairingError.Malformed, "${e.error}")
        assertEquals(0, output.size(), "a refused pairing still opened a connection")
    }

    @Test
    fun `a bad device name is refused before a byte reaches the desktop`() {
        val output = ByteArrayOutputStream()
        assertThrows<DeviceException> {
            Client.pair(
                input = ByteArrayInputStream(ByteArray(0)),
                output = output,
                offer = recordedOffer(),
                identity = InMemoryStaticKey(deviceSecret),
                deviceName = "phone\nAPPROVED",
                userVerification = true,
                version = version,
                ephemerals = initiatorEphemeral,
            )
        }
        assertEquals(0, output.size(), "a name the desktop would refuse still burned the offer")
    }

    @Test
    fun `a desktop that hangs up during the session handshake reads as not paired`() {
        // `serve.rs` deliberately does NOT complete the handshake for an
        // unpaired or revoked key: completing it and then refusing would
        // confirm to a scanner that this machine's key is what they think it
        // is. So the device sees end-of-stream, and "never paired" and
        // "revoked" are indistinguishable — which is the design, not a gap.
        val e = assertThrows<SessionRefused> {
            Client.openSession(
                input = ByteArrayInputStream(ByteArray(0)),
                output = ByteArrayOutputStream(),
                identity = InMemoryStaticKey(deviceSecret),
                desktopPublic = desktopPublic,
                version = version,
                ephemerals = initiatorEphemeral,
            )
        }
        assertTrue(e.message!!.contains("revoked"), e.message!!)
    }

    @Test
    fun `a machine that does not hold the pinned key cannot complete the pairing`() {
        // The QR code's whole purpose, exercised through the client rather than
        // through the handshake: a man in the middle replies with a handshake
        // message of its own and the device refuses it.
        // The impostor cannot even read message 1 — it was encrypted to the
        // real desktop's key — so it can only guess at a reply.
        val garbage = framed(ByteArray(80) { it.toByte() })
        val e = assertThrows<PairingException> {
            Client.pair(
                input = ByteArrayInputStream(garbage),
                output = ByteArrayOutputStream(),
                offer = recordedOffer(),
                identity = InMemoryStaticKey(deviceSecret),
                deviceName = "pixel-8",
                userVerification = true,
                version = version,
                ephemerals = initiatorEphemeral,
            )
        }
        assertTrue(e.error is PairingError.BadDevice, "${e.error}")
    }

    @Test
    fun `a refusal arrives inside the handshake and is surfaced in the desktop's words`() {
        // A plaintext "no" would tell anybody watching that this machine is not
        // currently pairing, so `serve.rs` puts the refusal inside the finished
        // handshake. Built here by running the responder half with the real
        // desktop key and a refusing answer.
        val refusal = """{"ok":false,"error":"this machine is not offering to pair"}"""
        val responder = Noise.pairingResponder(
            InMemoryStaticKey(Vectors.hex("desktop_secret_hex")),
            version,
            EphemeralSource.fixedForTestingOnly(Vectors.hex("responder_ephemeral_hex")),
        )
        responder.read(Vectors.patternHex("nk", "m1_hex"))
        val m2 = responder.write(refusal.toByteArray())
        val e = assertThrows<PairingException> {
            Client.pair(
                input = ByteArrayInputStream(framed(m2)),
                output = ByteArrayOutputStream(),
                offer = recordedOffer(),
                identity = InMemoryStaticKey(deviceSecret),
                deviceName = "pixel-8",
                userVerification = true,
                version = version,
                ephemerals = initiatorEphemeral,
            )
        }
        assertEquals(PairingError.Refused("this machine is not offering to pair"), e.error)
    }

    @Test
    fun `frames sent on a session are sealed and framed the way the desktop reads them`() {
        // End to end at the top of the stack: a Control frame goes out as
        // [u32 length][noise ciphertext], and the desktop's own half opens it
        // back into the same frame.
        val desktop = Noise.sessionResponder(
            InMemoryStaticKey(Vectors.hex("desktop_secret_hex")),
            version,
            EphemeralSource.fixedForTestingOnly(Vectors.hex("responder_ephemeral_hex")),
        )
        desktop.read(Vectors.patternHex("ik", "m1_hex"))
        val m2 = desktop.write("l16".toByteArray())
        val desktopChannel = desktop.intoTransport()

        val output = ByteArrayOutputStream()
        val session = Client.openSession(
            input = ByteArrayInputStream(framed(m2)),
            output = output,
            identity = InMemoryStaticKey(deviceSecret),
            desktopPublic = desktopPublic,
            version = version,
            ephemerals = initiatorEphemeral,
        )
        val handshakeBytes = output.size()
        session.send(Frame.Control("""{"cmd":"list"}""".toByteArray()))
        session.sendData(4u, ByteArray(Frame.MAX_PAYLOAD + 1) { 0x41 })

        val stream = ByteArrayInputStream(output.toByteArray().copyOfRange(handshakeBytes, output.size()))
        val first = Frame.decode(desktopChannel.open(Transport.readMessage(stream)))
        assertEquals(Frame.Control("""{"cmd":"list"}""".toByteArray()), first)
        // Split across two frames, because one byte over the limit is a byte
        // the wire layer cannot carry.
        val rejoined = ByteArrayOutputStream()
        repeat(2) {
            val f = Frame.decode(desktopChannel.open(Transport.readMessage(stream))) as Frame.Data
            assertEquals(4u, f.channel)
            rejoined.write(f.bytes)
        }
        assertArrayEquals(ByteArray(Frame.MAX_PAYLOAD + 1) { 0x41 }, rejoined.toByteArray())
    }

    /** A live desktop half, sharing the vectors' fixed keys. */
    private fun desktopHalf(): Pair<Handshake, ByteArray> {
        val desktop = Noise.sessionResponder(
            InMemoryStaticKey(Vectors.hex("desktop_secret_hex")),
            version,
            EphemeralSource.fixedForTestingOnly(Vectors.hex("responder_ephemeral_hex")),
        )
        desktop.read(Vectors.patternHex("ik", "m1_hex"))
        return desktop to desktop.write("l16".toByteArray())
    }

    @Test
    fun `the keepalive is answered without the caller ever seeing it`() {
        // `apex-remoted` sends a Ping every fifteen seconds and measures the
        // round trip from the Pong. A client that ignored them would leave the
        // desktop reporting an unknown connection quality forever, and one that
        // surfaced them would make every caller handle a frame that is not
        // theirs. Neither happens: the ping is answered and the control frame
        // behind it is what `receive` returns.
        val (desktop, m2) = desktopHalf()
        val desktopChannel = desktop.intoTransport()
        val inbound = ByteArrayOutputStream()
        inbound.write(framed(m2))
        inbound.write(framed(desktopChannel.seal(Frame.Ping(0x0102030405060708L).encode())))
        inbound.write(framed(desktopChannel.seal(Frame.Control("""{"ok":true}""".toByteArray()).encode())))

        val output = ByteArrayOutputStream()
        val session = Client.openSession(
            input = ByteArrayInputStream(inbound.toByteArray()),
            output = output,
            identity = InMemoryStaticKey(deviceSecret),
            desktopPublic = desktopPublic,
            version = version,
            ephemerals = initiatorEphemeral,
        )
        val handshakeBytes = output.size()
        val frame = session.receive()
        assertEquals(Frame.Control("""{"ok":true}""".toByteArray()), frame)

        // And the Pong really went out, with the desktop's own token.
        val sent = ByteArrayInputStream(output.toByteArray().copyOfRange(handshakeBytes, output.size()))
        val pong = Frame.decode(desktopChannel.open(Transport.readMessage(sent)))
        assertEquals(Frame.Pong(0x0102030405060708L), pong)
    }

    @Test
    fun `this end measures its own round trip and ignores a token it never sent`() {
        // The other half of P1-052's "quality visible at both ends". A peer that
        // echoed a number of its own choosing could otherwise report any
        // quality it liked, including a good one for a connection that is
        // unusable — so an unrecognised token changes nothing.
        val (desktop, m2) = desktopHalf()
        val desktopChannel = desktop.intoTransport()
        val inbound = ByteArrayOutputStream()
        inbound.write(framed(m2))
        inbound.write(framed(desktopChannel.seal(Frame.Pong(9999L).encode())))
        inbound.write(framed(desktopChannel.seal(Frame.Pong(1L).encode())))
        inbound.write(framed(desktopChannel.seal(Frame.Control("""{"ok":true}""".toByteArray()).encode())))

        val session = Client.openSession(
            input = ByteArrayInputStream(inbound.toByteArray()),
            output = ByteArrayOutputStream(),
            identity = InMemoryStaticKey(deviceSecret),
            desktopPublic = desktopPublic,
            version = version,
            ephemerals = initiatorEphemeral,
        )
        assertEquals(null, session.roundTripMs, "a round trip was reported before one was measured")
        session.ping()
        session.receive()
        // Token 9999 was never sent and must not have been timed; token 1 was.
        assertTrue(session.roundTripMs != null, "the round trip this end asked for was not measured")
        assertTrue(session.roundTripMs!! >= 0)
    }
}
