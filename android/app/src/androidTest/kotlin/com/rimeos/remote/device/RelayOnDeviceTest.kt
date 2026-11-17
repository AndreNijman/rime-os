package com.rimeos.remote.device

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.PairingOffer
import com.rimeos.remote.core.RelayEndpoint
import com.rimeos.remote.core.Rendezvous
import com.rimeos.remote.pairing.GatedBox
import com.rimeos.remote.pairing.RelayDialler
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The relay leg, from a real phone, through the real deployed relay.
 *
 * ## What makes this the relay leg and not the LAN one
 *
 * The phone and the computer are on the same Wi-Fi — that is what makes the
 * rest of this suite possible — so simply connecting would take the direct
 * path every time and prove nothing about the relay. The offer the daemon
 * mints is therefore copied with **`lan = emptyList()`**, which `Pairing.kt`
 * documents as a complete payload, and the copy is what this phone is given.
 * `PairingService` then has one path available and has to take it.
 *
 * Nothing else is weakened. The key is the real key, the token is the real
 * one-time token, the rendezvous is derived from the key this phone pins, and
 * the Noise handshake runs end to end through a relay that holds neither half
 * of it. The desktop is `rime-remoted` holding the same rendezvous as the host,
 * started with `--relay` by `run-device-suite.sh`.
 *
 * ## What it can conclude, and what it cannot
 *
 * It concludes that a phone reaches this computer through Cloudflare's Worker,
 * its Durable Object and a custom-domain TLS termination — none of which any
 * double stands in for — and that the session that comes back is labelled as a
 * relayed one. It does **not** conclude that the phone would work from another
 * network: it is still on this one, and the relay leg is being taken because
 * the LAN addresses were withheld rather than because they were unreachable.
 * Proving the second needs a phone on mobile data and a person to hold it.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class RelayOnDeviceTest {

    private fun relayUrl(): String = Desktop.arg("relay")

    /** The offer the daemon just minted, with its LAN addresses withheld. */
    private fun relayOnlyOffer(): PairingOffer {
        val offer = Pairing.decodeOffer(Desktop.offerPayload())
        assertNotNull(
            "rime-remoted minted an offer with no relay in it, so it was not started with " +
                "--relay and this test would silently be testing nothing. The runner passes " +
                "it; see run-device-suite.sh.",
            offer.relay,
        )
        assertEquals(
            "the daemon is holding a rendezvous at a different relay from the one this " +
                "phone was told to expect",
            RelayEndpoint.parse(relayUrl()),
            RelayEndpoint.parse(offer.relay!!),
        )
        return offer.copy(lan = emptyList())
    }

    @Test
    fun a_phone_pairs_through_the_relay_when_no_lan_address_is_offered() {
        val offer = relayOnlyOffer()
        val machine = runBlocking {
            Paired.service.pair(
                offer = offer,
                deviceName = "relay-paired",
                boxFor = { GatedBox(Paired.PlainBox(), userVerification = false) },
                nowMs = System.currentTimeMillis(),
            )
        }
        assertEquals("the desktop names itself in the answer", Desktop.arg("machine"), machine.machine)
        // The record carries the relay and no LAN hint, so every later
        // connection from this record goes the same way with no second trick.
        assertEquals(emptyList<String>(), machine.lan)
        assertNotNull(machine.relay)

        // And the DESKTOP wrote the pairing down, which is the half a phone
        // cannot fake: these bytes crossed Cloudflare and were decrypted by a
        // daemon holding the other end of the handshake.
        val recorded = Desktop.device(machine.deviceId)
        assertNotNull(
            "the desktop must hold a record for ${machine.deviceId}; it holds " +
                Desktop.deviceIds(),
            recorded,
        )
        assertEquals("relay-paired", recorded!!.getString("name"))
    }

    @Test
    fun a_session_through_the_relay_carries_traffic_and_says_which_path_it_took() {
        val machine = pairOverRelay("relay-session")
        val identity = MachineStore().identityFor(machine, Paired.PlainBox())
        val connected = runBlocking { Paired.service.open(machine, identity) }
        connected.session.use { session ->
            assertEquals(Desktop.arg("machine"), session.machine)
            // A frame all the way to the daemon and back, through the relay.
            // `measureRoundTrip` sends a `Ping` and waits for the answer, so a
            // number here is a byte that was encrypted on this phone, copied by
            // a Worker that could not read it, decrypted on the computer, and
            // answered.
            val rtt = session.measureRoundTrip()
            assertNotNull("no round trip came back through the relay", rtt)
            assertTrue("a round trip of $rtt ms is not a measurement", rtt!! >= 0)
        }
        assertEquals(
            "a session that went through the relay was labelled a LAN one",
            Rendezvous.Path.RELAY,
            connected.path,
        )
        // And the words the user is shown name the third party that is now on
        // the path. `docs/remote.md` requires the disclosure to be printed
        // rather than assumed, and this is the string the banner prints.
        assertTrue(
            "the relay disclosure does not name the relay: ${connected.path.disclosure()}",
            connected.path.disclosure().contains("relay"),
        )
    }

    @Test
    fun the_relay_refuses_a_rendezvous_no_desktop_is_waiting_at() {
        // The real Worker's own rule (`relay/src/room.js`: "no desktop is
        // waiting at this rendezvous"), which a loopback double can imitate but
        // only the deployment can confirm. It is also the error a person gets
        // when their computer is off, so it has to arrive as a refusal and not
        // as a hang.
        val endpoint = RelayEndpoint.parse(relayUrl())
        val nobody = Rendezvous.idFor(ByteArray(32) { 0x5a })
        val failure = try {
            RelayDialler.dial(endpoint, nobody).link.close()
            null
        } catch (e: Exception) {
            e
        }
        assertNotNull("the relay joined a guest to a desktop that does not exist", failure)
        assertTrue(
            "the refusal does not carry the relay's own status: ${failure!!.message}",
            failure.message!!.contains("409"),
        )
    }

    private fun pairOverRelay(name: String): PairedMachine = runBlocking {
        Paired.service.pair(
            offer = relayOnlyOffer(),
            deviceName = name,
            boxFor = { GatedBox(Paired.PlainBox(), userVerification = false) },
            nowMs = System.currentTimeMillis(),
        )
    }
}
