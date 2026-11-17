package com.rimeos.remote.device

import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.SessionRefused
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.pairing.GatedBox
import com.rimeos.remote.pairing.NoRouteToMachine
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith

/**
 * P1-060 criterion 1, on a phone: pairing, LAN, reconnect, agent lifecycle and
 * device revocation against a real `rime-remoted` and a real `rime-agentd`
 * across real Wi-Fi.
 *
 * ## What is real here and what is not
 *
 * Real: the phone, the radio, the two daemons (built from this worktree, in
 * their own `XDG_RUNTIME_DIR` so the live runtime is untouched), the Noise
 * handshakes, the device store on disk, and `PairingService` — the production
 * class, not a copy of it.
 *
 * Not real: the keystore, and only the keystore. [Paired] owns that
 * substitution and names it: `KeystoreSecretBox` gates the device key behind a
 * `BiometricPrompt`, no amount of `adb` can present a fingerprint, so the box
 * handed to `PairingService` here protects nothing. P1-051's sixth criterion
 * is about exactly that gate — which is observed separately, by the app
 * refusing to open at all without it.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class LanEndToEndTest {

    private val service = Paired.service

    private fun pair(name: String = "pixel-under-test"): PairedMachine = Paired.machine(name)

    private fun identityOf(machine: PairedMachine) = Paired.identityOf(machine)

    private fun linkTo(machine: PairedMachine): MachineLink = Paired.linkTo(machine)

    // ── pairing ─────────────────────────────────────────────────────────────

    @Test
    fun a_phone_pairs_over_the_lan_and_the_desktop_records_it() {
        val machine = pair("pairing-test")
        assertEquals("the desktop names itself in the answer", Desktop.arg("machine"), machine.machine)

        val recorded = Desktop.device(machine.deviceId)
        assertNotNull(
            "the desktop must hold a record for ${machine.deviceId}; it holds " +
                Desktop.deviceIds(),
            recorded,
        )
        assertEquals("pairing-test", recorded!!.getString("name"))
        // The device's claim, as the DESKTOP recorded it — and this suite's
        // box gates nothing, so the claim is `false` and the desktop must have
        // written down `false`. What is asserted is that the flag travels and
        // is stored per device, not that a second factor exists.
        assertTrue(
            "the desktop must record the device's own second-factor claim",
            !recorded.getBoolean("requires_user_verification"),
        )
    }

    @Test
    fun a_pairing_token_is_spent_the_first_time_it_is_used() {
        val payload = Desktop.offerPayload()
        val offer = Pairing.decodeOffer(payload)
        runBlocking {
            service.pair(offer, "first-use", { GatedBox(Paired.PlainBox(), false) }, System.currentTimeMillis())
        }
        // The same code, scanned twice. A second phone must not get in.
        try {
            runBlocking {
                service.pair(
                    Pairing.decodeOffer(payload),
                    "second-use",
                    { GatedBox(Paired.PlainBox(), false) },
                    System.currentTimeMillis(),
                )
            }
            fail("a spent pairing token paired a second device")
        } catch (e: Exception) {
            assertTrue(
                "the refusal must be about the token, not about the network: ${e.message}",
                e !is NoRouteToMachine,
            )
        }
    }

    // ── the session, and what the daemon will do for it ─────────────────────

    @Test
    fun a_paired_phone_opens_a_session_and_the_daemon_answers() {
        val machine = pair("session-test")
        linkTo(machine).use { link ->
            // `sessions()` returning at all is the claim: it is a Noise
            // round trip through `rime-remoted` into `rime-agentd` and back.
            // What is IN the list is another test's business — these run in an
            // order JUnit 4 does not promise, so asserting emptiness here would
            // make this test depend on which one ran first.
            val sessions = link.sessions()
            assertNotNull(sessions)
            assertEquals("one connection was opened to answer that", 1, link.connections)
        }
    }

    @Test
    fun the_agent_lifecycle_runs_from_the_phone() {
        val machine = pair("lifecycle-test")
        linkTo(machine).use { link ->
            // `generic` with a program, which is the only way `generic` can
            // start: `rime-agentd` takes `args.first()` as the program for it.
            // `/bin/cat` because it needs nothing, does nothing, and reads
            // stdin — so `input` below is a real write into a real PTY.
            val started = link.run(
                cwd = "/tmp",
                cols = 80,
                rows = 24,
                agent = "generic",
                args = listOf("/bin/cat"),
            )
            assertTrue("the daemon assigned an id", started.id > 0)

            val listed = link.sessions().map { it.id }
            assertTrue(
                "the session this phone started must be in the daemon's list; it listed $listed",
                listed.contains(started.id),
            )

            // Who the daemon thinks asked. This is P1-057's third criterion
            // seen from the inside: a request that arrived over the phone is
            // recorded as a remote one, by this device's id, and not as a
            // human at the keyboard.
            val info = link.info(started.id)
            assertEquals(
                "a phone's request must be recorded as claude-remote-control",
                "claude-remote-control",
                info.requestOrigin,
            )
            assertEquals("and attributed to this device", machine.deviceId, info.actor)

            link.input(started.id, "hello-from-the-pixel\n")
            link.stop(started.id)
        }
    }

    @Test
    fun the_phone_refuses_to_start_an_adapter_it_has_no_program_for() {
        // Before `args` existed this was the ONLY outcome of choosing the
        // Generic adapter, and it arrived from the daemon after a round trip.
        // Now the phone knows the rule and says so without spending one.
        val machine = pair("generic-guard-test")
        linkTo(machine).use { link ->
            try {
                link.run(cwd = "/tmp", cols = 80, rows = 24, agent = "generic")
                fail("a generic session started with no program")
            } catch (e: IllegalArgumentException) {
                assertTrue(
                    "the refusal must name the program that is missing: ${e.message}",
                    e.message!!.contains("runs a program you name"),
                )
            }
        }
    }

    // ── reconnect ───────────────────────────────────────────────────────────

    @Test
    fun a_dropped_connection_is_replaced_on_the_next_question() {
        val machine = pair("reconnect-test")
        linkTo(machine).use { link ->
            link.sessions()
            assertEquals(1, link.connections)
            // The daemon hangs up on everything. `MachineLink` has no reconnect
            // thread by design — a dead connection is noticed when something is
            // asked — so the next question is what must rebuild it.
            // The desktop's service restarts — a `systemctl --user restart`,
            // a crash, a laptop that slept. Every open connection dies with it
            // and the device store on disk does not, which is exactly the case
            // a phone in a pocket meets.
            Desktop.ask("""{"cmd":"restart_remoted"}""")
            link.sessions()
            assertTrue(
                "the link must have opened a second connection, and opened ${link.connections}",
                link.connections >= 2,
            )
        }
    }

    // ── revocation ──────────────────────────────────────────────────────────

    @Test
    fun a_revoked_phone_is_refused_and_cannot_tell_why() {
        val machine = pair("revocation-test")
        // It works first, so that the refusal below is about the revocation.
        linkTo(machine).use { it.sessions() }

        Desktop.revoke(machine.deviceId)

        try {
            runBlocking { service.connect(machine, identityOf(machine)) }
            fail("a revoked device opened a session")
        } catch (e: SessionRefused) {
            assertTrue(
                "the refusal must not distinguish revoked from never-paired: ${e.message}",
                e.message!!.contains("unpaired or revoked"),
            )
        }
        val after = Desktop.device(machine.deviceId)
        assertNotNull("a revoked device is kept, not forgotten", after)
        assertTrue(
            "and it carries the moment it was revoked",
            after!!.optLong("revoked_ms", 0L) > 0L,
        )
    }

    // ── the address list in the offer ───────────────────────────────────────

    @Test
    fun every_address_the_offer_advertises_can_actually_be_dialled() {
        val offer = Pairing.decodeOffer(Desktop.offerPayload())
        assertTrue("an offer with no addresses is not a LAN offer", offer.lan.isNotEmpty())
        val unreachable = offer.lan.filter { !dialable(it) }
        assertEquals(
            "these addresses are in the pairing code and cannot be dialled from the phone " +
                "that scans it: $unreachable (the whole list was ${offer.lan})",
            emptyList<String>(),
            unreachable,
        )
    }

    private fun dialable(address: String): Boolean {
        // Through `PairingService.splitHostPort`, which is what the app uses to
        // turn an advertised address into a socket. Dialling the raw string
        // some other way would test a parser nobody ships.
        val (host, port) = service.splitHostPort(address)
        return try {
            java.net.Socket().use {
                it.connect(java.net.InetSocketAddress(host, port), 4_000)
                true
            }
        } catch (e: Exception) {
            false
        }
    }

}
