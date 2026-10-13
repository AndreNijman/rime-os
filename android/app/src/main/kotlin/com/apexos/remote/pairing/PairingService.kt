package com.apexos.remote.pairing

import com.apexos.remote.core.Client
import com.apexos.remote.core.InMemoryStaticKey
import com.apexos.remote.core.MachineStore
import com.apexos.remote.core.PairedMachine
import com.apexos.remote.core.PairingOffer
import com.apexos.remote.core.SecretBox
import com.apexos.remote.core.Session
import com.apexos.remote.core.StaticKey
import com.apexos.remote.core.Transport
import java.net.InetSocketAddress
import java.net.Socket
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Pairing and connecting, over a real socket, off the main thread.
 *
 * `:core` is generic over streams because the LAN leg is TCP and the relay leg
 * is a WebSocket carrying the same bytes; this is the TCP half. Everything here
 * runs on [Dispatchers.IO] — a socket touched from the main thread is an
 * immediate `NetworkOnMainThreadException`, and it is a crash that never
 * happens on a laptop running the unit tests.
 */
class PairingService {
    /**
     * Try each LAN address the offer named, in order, and pair with the first
     * that answers.
     *
     * None of the addresses is authenticated and none needs to be: reaching the
     * wrong one produces a handshake that does not complete, not a connection
     * to the wrong machine. So "try them all" is safe in a way it would not be
     * in a protocol that trusted an address.
     */
    suspend fun pair(
        offer: PairingOffer,
        deviceName: String,
        box: SecretBox,
        userVerification: Boolean,
        nowMs: Long,
    ): PairedMachine = withContext(Dispatchers.IO) {
        val identity = InMemoryStaticKey.generate()
        var lastFailure: Exception? = null
        for (address in offer.lan) {
            val socket = try {
                connect(address)
            } catch (e: Exception) {
                lastFailure = e
                continue
            }
            socket.use {
                val answer = Client.pair(
                    input = it.getInputStream(),
                    output = it.getOutputStream(),
                    offer = offer,
                    identity = identity,
                    deviceName = deviceName,
                    userVerification = userVerification,
                )
                return@withContext MachineStore.record(identity, offer, answer, box, nowMs)
            }
        }
        throw NoRouteToMachine(
            "none of the addresses in the pairing code answered: ${offer.lan.joinToString()}" +
                if (offer.relay != null) " (and no relay client has been written yet)" else "",
            lastFailure,
        )
    }

    /** Open a session with a machine already paired. */
    suspend fun connect(machine: PairedMachine, identity: StaticKey): Session =
        withContext(Dispatchers.IO) {
            var lastFailure: Exception? = null
            for (address in machine.lan) {
                val socket = try {
                    connect(address)
                } catch (e: Exception) {
                    lastFailure = e
                    continue
                }
                // NOT `use`: the session owns the socket for as long as it
                // lives, and closing it here would end the session the moment
                // this function returned.
                return@withContext Client.openSession(
                    input = socket.getInputStream(),
                    output = socket.getOutputStream(),
                    identity = identity,
                    desktopPublic = machine.let { com.apexos.remote.core.Device.checkKey(it.desktopKey) },
                )
            }
            throw NoRouteToMachine("${machine.machine} did not answer on any known address", lastFailure)
        }

    private fun connect(address: String): Socket {
        val (host, port) = splitHostPort(address)
        val socket = Socket()
        socket.connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS)
        // The desktop drops an unauthenticated peer after thirty seconds. This
        // end sets its own deadline for the handshake and takes it off once a
        // session is open, because a PTY may sit idle for hours and a read
        // deadline on one would end the terminal.
        socket.soTimeout = HANDSHAKE_TIMEOUT_MS
        socket.tcpNoDelay = true
        return socket
    }

    /**
     * `host:port`, with an IPv6 literal in brackets.
     *
     * `substringAfterLast(':')` would take the last group of an unbracketed
     * IPv6 address and call it a port. The desktop writes them bracketed
     * (`net.rs` excludes link-local precisely because it cannot be written
     * down), so this reads brackets first and splits on the last colon only
     * when there are none.
     */
    internal fun splitHostPort(address: String): Pair<String, Int> {
        if (address.startsWith("[")) {
            val close = address.indexOf(']')
            require(close > 0) { "$address is an unclosed IPv6 literal" }
            val host = address.substring(1, close)
            val port = address.substring(close + 1).removePrefix(":")
            return host to (port.toIntOrNull() ?: DEFAULT_PORT)
        }
        val colon = address.lastIndexOf(':')
        if (colon < 0) return address to DEFAULT_PORT
        return address.substring(0, colon) to (address.substring(colon + 1).toIntOrNull() ?: DEFAULT_PORT)
    }

    private companion object {
        const val CONNECT_TIMEOUT_MS = 4_000
        const val HANDSHAKE_TIMEOUT_MS = 20_000

        /** `apex-remoted`'s listener. Only ever a fallback; the offer names the port. */
        const val DEFAULT_PORT = 7717
    }
}

/** Nothing the pairing code named could be reached. */
class NoRouteToMachine(message: String, cause: Throwable?) : Exception(message, cause)

private fun Socket.use(block: (Socket) -> Unit) {
    try {
        block(this)
    } finally {
        runCatching { close() }
    }
}

/** The hello byte is written by [Client]; named here so the import is not unused. */
internal val helloBytes = byteArrayOf(Transport.HELLO_PAIR, Transport.HELLO_SESSION)
