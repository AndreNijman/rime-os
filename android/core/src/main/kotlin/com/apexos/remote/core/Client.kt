package com.apexos.remote.core

import java.io.InputStream
import java.io.OutputStream

/**
 * The two conversations a device has with an APEX machine, over any pair of
 * streams.
 *
 * Streams and not a socket, deliberately. The LAN leg is a TCP connection and
 * the relay leg is a WebSocket carrying the same byte stream; `apex-remoted`
 * makes exactly this separation (`relay.rs`: "nothing above the transport
 * knows which leg it is on") and copying it here means the same code, and the
 * same tests, cover both. It is also what lets a test drive a full pairing
 * against a recorded transcript with no network at all.
 */
object Client {
    /**
     * Pair with the machine whose QR code produced [offer].
     *
     * Returns the desktop's answer, which arrives **inside** the finished
     * handshake whether it is a yes or a no — a plaintext refusal would tell
     * anybody watching that the machine is not currently pairing. A refusal is
     * therefore a [PairingException] raised after a handshake that completed,
     * not a connection that failed.
     */
    fun pair(
        input: InputStream,
        output: OutputStream,
        offer: PairingOffer,
        identity: StaticKey,
        deviceName: String,
        userVerification: Boolean,
        version: Int = REMOTE_PROTOCOL_VERSION,
        ephemerals: EphemeralSource = EphemeralSource.Secure,
    ): PairingAnswer {
        if (offer.v != version) {
            throw PairingException(
                PairingError.Malformed(
                    "that machine speaks APEX Remote v${offer.v} and this app speaks v$version",
                ),
            )
        }
        // Refused here rather than by the desktop. Sending a name the desktop
        // will reject would burn the owner's pairing offer — `pairing::complete`
        // redeems the token before it validates the device — and cost them a
        // walk back to the machine for a new code.
        val name = Device.checkName(deviceName)
        val handshake = Noise.pairingInitiator(offer.desktopPublicKey(), version, ephemerals)
        val request = PairingRequest(
            key = identity.publicKeyText(),
            name = name,
            token = offer.token,
            userVerification = userVerification,
        )
        output.write(byteArrayOf(Transport.HELLO_PAIR))
        Transport.writeMessage(
            output,
            handshake.write(
                Pairing.json.encodeToString(PairingRequest.serializer(), request)
                    .toByteArray(Charsets.UTF_8),
            ),
        )
        val payload = try {
            handshake.read(Transport.readMessage(input))
        } catch (e: NoiseException) {
            // The handshake failing here is the QR code's whole purpose: the
            // machine on the other end does not hold the key that was on the
            // screen.
            throw PairingException(
                PairingError.BadDevice(
                    "that machine did not prove it holds the key from the QR code (${e.message})",
                ),
            )
        }
        val answer = try {
            Pairing.json.decodeFromString(PairingAnswer.serializer(), payload.toString(Charsets.UTF_8))
        } catch (e: Exception) {
            throw PairingException(PairingError.Malformed(e.message ?: "the answer did not parse"))
        }
        if (!answer.ok) {
            throw PairingException(PairingError.Refused(answer.error ?: PairingError.BadToken.message))
        }
        return answer
    }

    /**
     * Open a session with a machine this device is already paired with.
     *
     * Returns the machine's name and the encrypted channel. An unpaired or
     * revoked key gets **no second handshake message at all** — the desktop
     * closes the connection instead, because completing the handshake and then
     * refusing would confirm to a scanner that this machine's key is what they
     * think it is. So end-of-stream here is "not paired, or revoked", and the
     * two are deliberately not distinguishable.
     */
    fun openSession(
        input: InputStream,
        output: OutputStream,
        identity: StaticKey,
        desktopPublic: ByteArray,
        version: Int = REMOTE_PROTOCOL_VERSION,
        ephemerals: EphemeralSource = EphemeralSource.Secure,
    ): Session {
        val handshake = Noise.sessionInitiator(identity, desktopPublic, version, ephemerals)
        output.write(byteArrayOf(Transport.HELLO_SESSION))
        Transport.writeMessage(output, handshake.write(ByteArray(0)))
        val machine = try {
            handshake.read(Transport.readMessage(input)).toString(Charsets.UTF_8)
        } catch (e: java.io.EOFException) {
            throw SessionRefused(
                "this machine does not accept this device: it is not paired, or it has been revoked",
                e,
            )
        } catch (e: java.net.SocketException) {
            // The same refusal, arriving as a reset rather than as a clean
            // close. Which of the two a phone sees depends on the network in
            // between, not on what the desktop decided, so they must mean the
            // same thing here — otherwise a revoked device reports "not paired"
            // on Wi-Fi and "connection reset" on mobile data.
            throw SessionRefused(
                "this machine does not accept this device: it is not paired, or it has been revoked",
                e,
            )
        }
        return Session(machine, handshake.intoTransport(), input, output)
    }
}

/** The desktop hung up during the session handshake. */
class SessionRefused(message: String, cause: Throwable? = null) : Exception(message, cause)

/**
 * An open session: the machine's name, and frames over an encrypted channel.
 *
 * Not thread-safe for writing, and that is a property of the protocol rather
 * than an omission. The Noise nonce is a message counter and neither end
 * chooses it, so two threads sealing concurrently would produce two messages
 * claiming the same one — `apex-remoted` funnels everything it sends through a
 * single `Sealer` for exactly this reason, and a client must do the same.
 */
class Session internal constructor(
    val machine: String,
    private val channel: NoiseChannel,
    private val input: InputStream,
    private val output: OutputStream,
) {
    private val writeLock = Any()
    private val outstanding = java.util.concurrent.ConcurrentHashMap<Long, Long>()
    private val nextToken = java.util.concurrent.atomic.AtomicLong(0)

    /**
     * The last measured round trip, in milliseconds, or `null` before the first
     * [ping] comes back.
     *
     * P1-052 asks for the connection path and its quality to be visible at
     * *both* ends. This is this end's half, and it is measured on a frame that
     * crosses the same path as everything else — a ping to the relay's front
     * door would report the health of a machine nobody is talking to.
     */
    @Volatile
    var roundTripMs: Long? = null
        private set

    /** Send one frame. Safe to call from several threads; serialised here. */
    fun send(frame: Frame) {
        val plaintext = frame.encode()
        synchronized(writeLock) {
            Transport.writeMessage(output, channel.seal(plaintext))
        }
    }

    /** Send a run of terminal bytes, split into as many frames as it needs. */
    fun sendData(channelId: UInt, bytes: ByteArray) {
        for (frame in Frame.dataFrames(channelId, bytes)) send(frame)
    }

    /** Measure the connection. The answer lands in [roundTripMs]. */
    fun ping() {
        val token = nextToken.incrementAndGet()
        outstanding[token] = System.nanoTime()
        send(Frame.Ping(token))
    }

    /**
     * Read one frame, answering keepalives on the way.
     *
     * `apex-remoted` sends a `Ping` every fifteen seconds — it is the only
     * traffic an idle session has, and an idle session with no traffic is one a
     * NAT eventually forgets — and it measures the round trip from the `Pong`
     * that comes back. A client that handed those frames to its caller would
     * make every caller handle them, and a client that ignored them would leave
     * the desktop reporting an unknown connection quality forever. So they are
     * answered here and never surface: exactly what the desktop's own frame
     * loop does with the mirror image of this.
     *
     * One reader only. The receiving nonce is a counter too, and two readers
     * would each advance it past the other's message.
     */
    fun receive(): Frame {
        while (true) {
            val frame = Frame.decode(channel.open(Transport.readMessage(input)))
            when (frame) {
                is Frame.Ping -> send(Frame.Pong(frame.token))
                is Frame.Pong -> {
                    // A token this end never sent is ignored rather than timed.
                    // The desktop applies the same rule in the other direction
                    // and for the same reason: a peer that echoed a number of
                    // its own choosing could otherwise report any quality it
                    // liked, including a good one for a connection that is
                    // unusable.
                    val sent = outstanding.remove(frame.token)
                    if (sent != null) {
                        roundTripMs = (System.nanoTime() - sent) / 1_000_000
                    }
                }
                else -> return frame
            }
        }
    }
}

/**
 * The protocol revision this build speaks.
 *
 * Exchanged in the pairing payload and again in the handshake prologue, so a
 * device and a desktop that disagree find out before either has sent a frame
 * the other would misread. Separate from `apex-agentd`'s own protocol version,
 * which versions what travels *inside* a control frame.
 */
const val REMOTE_PROTOCOL_VERSION: Int = 1
