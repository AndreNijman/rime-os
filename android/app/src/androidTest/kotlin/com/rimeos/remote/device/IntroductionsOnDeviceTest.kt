package com.rimeos.remote.device

import android.content.Context
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.Base64Url
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.phone.Introductions
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.pairing.GatedBox
import java.security.SecureRandom
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * "Join my other computers automatically" (contract §2.15, ECOSYSTEM-PLAN.md
 * decision 11, M2) with one phone and the two real daemons of the suite:
 * [Desktop] ("A") and [SecondDesktop] ("B").
 *
 * What runs is the app's own [LinkHub] (the switch, Flow A on connect, a
 * `join_offer` kept for the screen and joined with the seal, a
 * `device_removed` followed) against two `rime-remoted`s, with the person at
 * each computer played by the broker's `rime remote …` (`allow`, `remove`,
 * `ping`, the listing). The one substitution is the seal: the screen's
 * prompt is [Paired.PlainBox] here, as in every suite.
 *
 * Every rule a computer enforces is checked on the computer's side of it:
 * a refusal is read from the daemon (its reply, its listing or its output),
 * never inferred from the phone not asking.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class IntroductionsOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val stored = mutableListOf<PairedMachine>()
    private val phonesOn = mutableListOf<Pair<Computer, String>>()
    private lateinit var a: PairedMachine
    private lateinit var b: PairedMachine
    private lateinit var nameA: String
    private lateinit var nameB: String
    private val n = System.nanoTime() % 100_000

    @Before
    fun pairBoth() {
        assertTrue(
            "the introduction tests run only on an emulator (hardware ${Build.HARDWARE})",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish" || shell("getprop ro.kernel.qemu").trim() == "1",
        )
        hub.setJoinSeal(null, "")
        setSwitch(null)
        // Offers, tries and reports are kept in memory: each test starts
        // with daemons that have none, and computers that know no others.
        Desktop.restartRemoted()
        SecondDesktop.restartRemoted()
        forgetEveryComputer()

        nameA = "intro-a-$n"
        nameB = "intro-b-$n"
        a = Paired.machine(nameA, Desktop)
        b = Paired.machine(nameB, SecondDesktop)
        phonesOn += Desktop to a.deviceId
        phonesOn += SecondDesktop to b.deviceId
        keep(a)
        keep(b)
        hub.markUnlocked()
        for (m in listOf(a, b)) {
            hub.remember(m.deviceId, Paired.identityOf(m))
            hub.hold(m)
        }
        for (m in listOf(a, b)) {
            awaitValue(15_000, "${m.machine}'s remote hello with ${Introductions.FEATURE}") {
                hub.state(m.deviceId)?.remote?.takeIf { it.has(Introductions.FEATURE) }
            }
        }
        assertNull("A knows B before any introduction", known(Desktop, b.desktopKey))
        assertNull("B knows A before any introduction", known(SecondDesktop, a.desktopKey))
    }

    @After
    fun tidy() {
        hub.setJoinSeal(null, "")
        for (m in stored.toList()) runCatching { runBlocking { hub.forgetMachine(m, tellIt = false) } }
        for ((c, id) in phonesOn) runCatching { c.revoke(id) }
        runCatching { forgetEveryComputer() }
        runCatching { hub.setAutoJoin(false) }
        setSwitch(null)
        hub.lock()
    }

    // ── 1. Flow A, removal authority, a removal followed and never undone ──

    @Test
    fun the_phone_introduces_its_computers_and_an_introduced_one_removes_nothing_but_itself_until_allowed() {
        // The switch off: nothing is introduced, and both computers say why.
        val refused = ask(hub.link(a), Introductions.cardRequest())
        assertEquals("A gave a card with the switch off: $refused", "not_allowed", refused.optString("kind"))

        // ---- Flow A: the switch on introduces A and B to each other ----
        hub.setAutoJoin(true)
        val bOnA = awaitValue(20_000, "A to list B, introduced by the phone") { live(Desktop, b.desktopKey) }
        val aOnB = awaitValue(20_000, "B to list A, introduced by the phone") { live(SecondDesktop, a.desktopKey) }
        for ((who, c, phone) in listOf(Triple("A", bOnA, nameA), Triple("B", aOnB, nameB))) {
            assertEquals("$who: not introduced by this phone: $c", phone, c.optJSONObject("introduced_by")?.optString("name"))
            assertTrue("$who: allowed without anybody allowing it: $c", c.isNull("allowed_ms") || !c.has("allowed_ms"))
            assertTrue("$who: listed as removed: $c", c.isNull("revoked_ms") || !c.has("revoked_ms"))
        }
        // Asked again, the phone introduces nothing: they know each other.
        assertEquals(emptyList<String>(), runBlocking(Dispatchers.IO) { hub.introduceAll() })

        // ---- an introduced computer passes on no removal but its own ----
        val cardA = card(a)
        val cardB = card(b)
        // A computer that does not exist, introduced to each and removed
        // there, so each holds a removal of a computer the other never had.
        val ghostX = ghost(cardB, "ghost-x-$n")
        val ghostY = ghost(cardA, "ghost-y-$n")
        assertEquals("added", ask(hub.link(a), introduce(ghostX)).optString("outcome"))
        assertEquals("added", ask(hub.link(b), introduce(ghostY)).optString("outcome"))
        removeOk(Desktop, idOn(Desktop, ghostX.getString("key")))
        removeOk(SecondDesktop, idOn(SecondDesktop, ghostY.getString("key")))

        // A connection between them carries each one's removals both ways.
        ping(Desktop, bOnA.getString("id"))
        Thread.sleep(1_000)
        assertNull("B took a removal from A, which nobody allowed there", SecondDesktop.computerByKey(ghostX.getString("key")))
        assertNull("A took a removal from B, which nobody allowed there", Desktop.computerByKey(ghostY.getString("key")))

        // Allowed on A only: A takes B's removals now; B still takes none of A's.
        val allowed = Desktop.remote("computers", "allow", bOnA.getString("id"))
        assertEquals("rime remote computers allow failed: $allowed", 0, allowed.optInt("code", -1))
        assertFalse("allowed_ms not set", Desktop.computerByKey(b.desktopKey)!!.isNull("allowed_ms"))
        ping(Desktop, bOnA.getString("id"))
        val learned = awaitValue(15_000, "A to take B's removal once B is allowed there") {
            Desktop.computerByKey(ghostY.getString("key"))?.takeIf { !it.isNull("revoked_ms") }
        }
        assertNotNull(learned)
        assertNull("B took a removal from A, still not allowed there", SecondDesktop.computerByKey(ghostX.getString("key")))

        // ---- its own key: A removes B, and B removes A, though A is not allowed on B ----
        removeOk(Desktop, bOnA.getString("id"))
        awaitValue(20_000, "B to remove A on A's removal of B (its own key)") {
            SecondDesktop.computerByKey(a.desktopKey)?.takeIf { !it.isNull("revoked_ms") }
        }

        // ---- device_removed: A tells the phone, and the phone forgets B ----
        awaitValue(20_000, "the phone to forget B on A's device_removed") {
            if (repository.loadBlocking().machines.none { it.desktopKey == b.desktopKey }) true else null
        }
        stored.removeAll { it.deviceId == b.deviceId }
        assertNotNull("the phone forgot A, the sender", repository.loadBlocking().find(a.deviceId))
        assertTrue(
            "no notice that B was forgotten: ${hub.notices.value}",
            hub.notices.value.any { it.contains("removed") && it.contains(b.machine) },
        )

        // ---- an introduction never undoes a removal ----
        val again = ask(hub.link(a), introduce(cardB))
        assertEquals("A took B back over its own removal: $again", "removed", again.optString("outcome"))
        val still = Desktop.computerByKey(b.desktopKey)!!
        assertFalse("B is no longer removed on A: $still", still.isNull("revoked_ms"))
    }

    // ── 2. Flow B: a computer brings the phone, and the phone is a companion there ──

    @Test
    fun a_computer_brings_the_phone_to_another_only_when_allowed_there_and_at_most_four_times() {
        hub.setAutoJoin(true)
        val bOnA = awaitValue(20_000, "A to list B (Flow A)") { live(Desktop, b.desktopKey) }
        val aOnB = awaitValue(20_000, "B to list A (Flow A)") { live(SecondDesktop, a.desktopKey) }
        // Phones brought here in this test only: an earlier run's stay listed, revoked.
        val since = aOnB.getLong("paired_ms")
        fun brought() = SecondDesktop.remoteDevicesJson().filter {
            it.optJSONObject("introduced_by")?.optString("id") == aOnB.getString("id") && it.optLong("paired_ms") >= since
        }

        // The phone forgets B, so A has a computer to bring it to.
        SecondDesktop.revoke(b.deviceId)
        runBlocking { hub.forgetMachine(b, tellIt = false) }
        stored.removeAll { it.deviceId == b.deviceId }

        // ---- B is allowed on A, A is NOT allowed on B: B refuses the offer ----
        assertEquals(0, Desktop.remote("computers", "allow", bOnA.getString("id")).optInt("code", -1))
        val notAllowed = "is not allowed yet, so it may not bring a phone"
        val before = Desktop.remotedLog(Long.MAX_VALUE).first
        hub.setAutoJoin(true) // a new report, saying join
        awaitValue(20_000, "A's output to say B refused the offer") {
            if (notAllowed in Desktop.remotedLog(before).second) true else null
        }
        assertNull("an offer reached the phone from a computer not allowed on B", hub.pendingJoin.value)
        assertEquals("B lists a phone A brought, unasked", emptyList<JSONObject>(), brought())

        // ---- allowed on B too: the offer comes, and waits for the screen ----
        assertEquals(0, SecondDesktop.remote("computers", "allow", aOnB.getString("id")).optInt("code", -1))
        val pending = bring()
        assertEquals("the offer is not for B", b.desktopKey, pending.offer.key)
        assertTrue("an offer was joined without the screen", repository.loadBlocking().machines.none { it.desktopKey == b.desktopKey })
        assertTrue("no notice of the offer: ${hub.notices.value}", hub.notices.value.last().contains("Open Rime Remote"))

        // ---- the screen seals: the phone joins B, added by A ----
        val joined = joinWithSeal("intro-j1-$n")
        assertEquals("joined_by", a.machine, joined.joinedBy)
        val listed = awaitValue(10_000, "B to list the joined phone") { SecondDesktop.remoteDevicesJson().firstOrNull { it.optString("id") == joined.deviceId } }
        assertEquals("not introduced by A: $listed", aOnB.getString("id"), listed.optJSONObject("introduced_by")?.optString("id"))
        assertTrue("allowed without anybody allowing it: $listed", listed.isNull("allowed_ms") || !listed.has("allowed_ms"))
        val note = awaitValue(10_000, "B's notification that the phone joined") {
            SecondDesktop.notifyLog().lastOrNull { it.optString("summary") == "intro-j1-$n joined this computer" }
        }
        assertEquals("the notification has no Remove", "remove", note.optJSONArray("actions")?.optJSONArray(0)?.optString(0))

        // ---- an introduced phone is a companion until allowed ----
        hub.remember(joined.deviceId, Paired.identityOf(joined))
        hub.hold(joined)
        awaitValue(15_000, "the joined phone's hello to say companion") {
            hub.state(joined.deviceId)?.remote?.takeIf { it.scope == "companion" }
        }
        // A link of its own, so the refusal is B's and not the phone's gate.
        val raw = Paired.linkTo(joined)
        try {
            val status = ask(raw, """{"cmd":"machine_status"}""")
            assertEquals("B let an introduced phone use a full verb: $status", "not_allowed", status.optString("kind"))
            val card = ask(raw, Introductions.cardRequest())
            assertEquals("B gave its card on a companion session: $card", "not_allowed", card.optString("kind"))
        } finally {
            raw.close()
        }
        assertEquals(0, SecondDesktop.remote("devices", "allow", joined.deviceId).optInt("code", -1))
        val full = Paired.linkTo(joined)
        try {
            val status = ask(full, """{"cmd":"machine_status"}""")
            assertEquals("allowed, the phone is still refused: $status", "machine_status", status.optString("reply"))
        } finally {
            full.close()
        }

        // ---- Remove on the notification takes the phone away ----
        SecondDesktop.notifyAction(note.getInt("id"), "remove")
        awaitValue(10_000, "Remove to revoke the joined phone on B") {
            SecondDesktop.device(joined.deviceId)?.takeIf { !it.isNull("revoked_ms") }
        }
        forgetHere(joined)

        // ---- three more, each revoked: four brought, revoked ones counted ----
        for (k in 2..4) {
            hub.setJoinSeal(sealer, "intro-j$k-$n")
            restartA()
            val next = awaitValue(30_000, "join $k") {
                repository.loadBlocking().machines.firstOrNull { it.desktopKey == b.desktopKey }
            }
            keep(next)
            phonesOn += SecondDesktop to next.deviceId
            SecondDesktop.revoke(next.deviceId)
            forgetHere(next)
        }
        // The fifth: B refuses, and says why.
        val capped = "has brought 4 phones here already"
        val seen = Desktop.remotedLog(Long.MAX_VALUE).first
        restartA()
        awaitValue(20_000, "A's output to say B refused a fifth phone") {
            if (capped in Desktop.remotedLog(seen).second) true else null
        }
        Thread.sleep(2_000)
        assertTrue("a fifth phone joined B", repository.loadBlocking().machines.none { it.desktopKey == b.desktopKey })
        assertEquals("B lists other than four phones brought by A: ${brought()}", 4, brought().size)
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    private val sealer = LinkHub.JoinSeal { _, _ -> GatedBox(Paired.PlainBox(), userVerification = false) }

    /** A restarted, the phone reconnected: a new report, a new try. Waits for the offer. */
    private fun bring(): LinkHub.PendingJoin {
        restartA()
        return awaitValue(30_000, "A to bring the phone to B") { hub.pendingJoin.value }
    }

    private fun joinWithSeal(name: String): PairedMachine {
        hub.setJoinSeal(sealer, name)
        val joined = awaitValue(20_000, "the phone to join B") {
            repository.loadBlocking().machines.firstOrNull { it.desktopKey == b.desktopKey }
        }
        keep(joined)
        phonesOn += SecondDesktop to joined.deviceId
        assertNull("the offer was not used up", hub.pendingJoin.value)
        assertTrue("no notice of the join: ${hub.notices.value}", hub.notices.value.last().startsWith("Joined"))
        return joined
    }

    private fun restartA() {
        Desktop.restartRemoted()
        awaitValue(20_000, "the phone to reconnect to A") {
            runCatching { hub.connectNow(a) }
            hub.state(a.deviceId)?.remote?.takeIf { it.has(Introductions.FEATURE) && hub.link(a).connected }
        }
    }

    private fun forgetHere(m: PairedMachine) {
        runBlocking { hub.forgetMachine(m, tellIt = false) }
        stored.removeAll { it.deviceId == m.deviceId }
    }

    private fun keep(m: PairedMachine) {
        repository.updateBlocking { it.with(m) }
        stored += m
    }

    private fun setSwitch(on: Boolean?) {
        repository.updateBlocking { it.copy(settings = it.settings.copy(autoJoin = on)) }
    }

    /** One computer's card, through the phone's own link. */
    private fun card(m: PairedMachine): JSONObject {
        val reply = ask(hub.link(m), Introductions.cardRequest())
        assertEquals("${m.machine} gave no card: $reply", "computer_card", reply.optString("reply"))
        return reply.getJSONObject("card")
    }

    /** `computer_introduce` with [card] as given, as [Introductions.introduce] builds it. */
    private fun introduce(card: JSONObject): String =
        JSONObject().put("cmd", "computer_introduce").put("card", card).toString()

    /** A card for a computer that does not exist: [like]'s addresses, a new key. */
    private fun ghost(like: JSONObject, name: String): JSONObject =
        JSONObject(like.toString()).put("key", Base64Url.encode(ByteArray(32).also { SecureRandom().nextBytes(it) })).put("name", name)

    private fun idOn(c: Computer, key: String): String =
        awaitValue(10_000, "the computer with key $key listed") { c.computerByKey(key)?.optString("id") }

    private fun removeOk(c: Computer, id: String) {
        val r = c.remote("computers", "remove", id)
        assertEquals("rime remote computers remove $id failed: $r", 0, r.optInt("code", -1))
    }

    private fun ping(c: Computer, id: String) {
        val r = c.remote("computers", "ping", id, "--via", "lan", "--json")
        assertEquals("rime remote computers ping $id failed: $r", 0, r.optInt("code", -1))
    }

    /** [c]'s record of the computer with [key] while paired (not removed), or null. */
    private fun live(c: Computer, key: String): JSONObject? = c.computerByKey(key)?.takeIf { it.isNull("revoked_ms") }

    /** [c]'s record of [key] unless it is a removal a person here forgot: what stops an introduction. */
    private fun known(c: Computer, key: String): JSONObject? =
        c.computerByKey(key)?.takeIf { it.isNull("revoked_ms") || it.isNull("forgotten_ms") }

    /**
     * Every computer either daemon knows, removed and its removal forgotten,
     * as a person would with `remove` and `forget`. A forgotten record stays
     * listed (it keeps a replayed removal out) but stops nothing.
     */
    private fun forgetEveryComputer() {
        val left = { c: Computer -> c.computers().filter { it.isNull("revoked_ms") || it.isNull("forgotten_ms") } }
        awaitValue(30_000, "both computers to have no other they would refuse or keep") {
            for (c in listOf(Desktop, SecondDesktop)) {
                for (x in left(c)) {
                    val id = x.optString("id")
                    if (x.isNull("revoked_ms")) c.remote("computers", "remove", id)
                    c.remote("computers", "forget", id)
                }
            }
            Thread.sleep(500)
            if (left(Desktop).isEmpty() && left(SecondDesktop).isEmpty()) true else null
        }
    }

    /** A request's reply, a refusal included, as the daemon gave it. */
    private fun ask(link: MachineLink, line: String): JSONObject = try {
        JSONObject(link.request(line, retry = false))
    } catch (e: AgentError) {
        JSONObject().put("reply", "error").put("kind", e.kind).put("message", e.message)
    }

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(200)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what")
    }

    private fun shell(command: String): String {
        val fd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(fd).bufferedReader().use { it.readText() }
    }
}
