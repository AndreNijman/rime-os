package com.rimeos.remote.device

import android.app.NotificationManager
import android.content.Context
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Calls
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.MirrorService
import com.rimeos.remote.phone.PhoneCalls
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * One phone and TWO computers, with what Rime Remote does today and no new
 * protocol (ECOSYSTEM-PLAN.md decision 11, M0): the phone paired with the
 * runner's rime-remoted ([Desktop], "A") and with a second one of its own
 * ([SecondDesktop], "B": its own machine key, state, stubs and login
 * session), both connections held at once.
 *
 * The phone is the hub that keeps the computers in step: a call answered
 * from A, a notification dismissed or replied to from A, a Do Not Disturb
 * turned on from A all change the PHONE, and B follows from what the phone
 * then tells it. These tests check that B does follow, that requests from
 * both at once each get their own answer, and that one computer going away
 * leaves the other alone.
 *
 * Emulator only, like the calls, SMS and photo tests whose verbs it uses:
 * emulator numbers (5554, 5556), test images the broker makes, the stub
 * notification servers, media players and focus files of the two runs.
 *
 * The Shell is not running. Where its part matters (Focus) the test plays it
 * as PhoneFocusService does: a change read from focus.json is applied and
 * never sent back, and only a person's own change is sent with `rime remote
 * focus`.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class TwoComputersOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val listener = MirrorService.component(context).flattenToString()
    private val notifications: NotificationManager get() = context.getSystemService(NotificationManager::class.java)
    private val shellTags = mutableListOf<String>()
    private val helperTags = mutableListOf<String>()
    private lateinit var a: PairedMachine
    private lateinit var b: PairedMachine
    private lateinit var nameA: String
    private lateinit var nameB: String
    private var notifyA = 0
    private var notifyB = 0
    private var mediaA = 0
    private var mediaB = 0
    private var bStopped = false
    private var photosPushed = false

    @Before
    fun pairBoth() {
        assertTrue(
            "the two-computer tests run only on an emulator (hardware ${Build.HARDWARE})",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish" || shell("getprop ro.kernel.qemu").trim() == "1",
        )
        val photos = if (Build.VERSION.SDK_INT >= 33) "READ_MEDIA_IMAGES" else "READ_EXTERNAL_STORAGE"
        for (p in listOf("READ_PHONE_STATE", "READ_CALL_LOG", "ANSWER_PHONE_CALLS", "CALL_PHONE", "READ_SMS", "SEND_SMS", photos)) {
            shell("pm grant ${context.packageName} android.permission.$p")
        }
        shell("cmd notification allow_listener $listener")
        awaitValue(15_000, "Android to grant notification access") { if (MirrorService.granted(context)) true else null }
        awaitValue(15_000, "the listener to bind") { if (MirrorService.focus(false)) true else null }
        dndOff()
        endAnyCall()
        allOff()

        notifyA = Desktop.notifyLog().size
        notifyB = SecondDesktop.notifyLog().size
        mediaA = Desktop.mediaLog().size
        mediaB = SecondDesktop.mediaLog().size
        val n = System.nanoTime() % 100_000
        nameA = "two-a-$n"
        nameB = "two-b-$n"
        a = Paired.machine(nameA, Desktop)
        b = Paired.machine(nameB, SecondDesktop)
        // Two pairings, two identities: LinkHub keys everything on the
        // device id, so one shared by both would fake every result here.
        assertNotEquals("the two pairings share a device id", a.deviceId, b.deviceId)
        assertNotEquals("the two computers share a machine key", a.desktopKey, b.desktopKey)
        assertTrue("A does not list this phone", a.deviceId in Desktop.deviceIds())
        assertTrue("B does not list this phone", b.deviceId in SecondDesktop.deviceIds())
        assertFalse("A lists the phone's pairing with B", b.deviceId in Desktop.deviceIds())
        assertFalse("B lists the phone's pairing with A", a.deviceId in SecondDesktop.deviceIds())

        hub.remember(a.deviceId, Paired.identityOf(a))
        hub.remember(b.deviceId, Paired.identityOf(b))
        hub.markUnlocked()
        hub.hold(a)
        hub.hold(b)
        for (m in listOf(a, b)) {
            awaitValue(15_000, "${m.machine} (${m.deviceId})'s remote hello") {
                hub.state(m.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
            }
        }
    }

    @After
    fun tidy() {
        if (bStopped) runCatching { SecondDesktop.restartRemoted() }
        endAnyCall()
        MirrorService.focus(false)
        hub.lock()
        allOff()
        runCatching { dndOff() }
        for (tag in shellTags) keyOf(SHELL, tag)?.let { shell("cmd notification snooze --for 86400000 $it") }
        for (tag in helperTags) shell("am start -n $HELPER/.PostActivity --es tag $tag --ez cancel true")
        if (photosPushed) runCatching { Desktop.photosCleanup() }
        shell("cmd notification disallow_listener $listener")
    }

    // ── 1. calls ────────────────────────────────────────────────────────────

    @Test
    fun a_call_rings_on_both_answering_on_one_closes_it_on_the_other_and_hanging_up_clears_both() {
        setCalls(true)
        awaitBoth("the computers to learn they may show calls") { can(it).contains(Phone.CAN_CALLS_ANSWER) }

        Desktop.emuGsm("call", "5556")
        val ringA = awaitNotify(Desktop, "A to show the ringing call") { isRinging(it) }
        val ringB = awaitNotify(SecondDesktop, "B to show the ringing call") { isRinging(it) }
        val idA = ringA.getInt("id")
        val idB = ringB.getInt("id")
        assertEquals("ringing", Desktop.calls(a.deviceId).optJSONObject("current")?.optString("state"))
        assertEquals("ringing", SecondDesktop.calls(b.deviceId).optJSONObject("current")?.optString("state"))

        // The person presses Answer on A's notification.
        Desktop.notifyAction(idA, "answer")
        awaitValue(15_000, "the phone to answer") { phoneState().takeIf { it == Calls.ACTIVE } }
        awaitNotify(Desktop, "A to close its ringing notification") { isClose(it, idA) }
        awaitNotify(SecondDesktop, "B to close its ringing notification once A answered") { isClose(it, idB) }
        awaitValue(15_000, "B to see the call active") {
            SecondDesktop.calls(b.deviceId).optJSONObject("current")?.takeIf { it.optString("state") == "active" }
        }

        // Hung up from B, the computer that did not answer.
        val hangup = SecondDesktop.call(b.deviceId, "hangup")
        assertEquals("B's hang-up was not confirmed: $hangup", "call_done", hangup.optString("reply"))
        awaitValue(15_000, "the phone to end the call") { phoneState().takeIf { it == Calls.IDLE } }
        awaitValue(15_000, "A to see no call") { Desktop.calls(a.deviceId).takeIf { it.optString("reply") == "calls" && it.optJSONObject("current") == null } }
        awaitValue(15_000, "B to see no call") { SecondDesktop.calls(b.deviceId).takeIf { it.optString("reply") == "calls" && it.optJSONObject("current") == null } }
        // Nothing rings anywhere afterwards: one ringing notification each, both closed.
        Thread.sleep(1_000)
        assertEquals("A showed the call more than once: ${notifyLog(Desktop)}", 1, notifyLog(Desktop).count { isRinging(it) })
        assertEquals("B showed the call more than once: ${notifyLog(SecondDesktop)}", 1, notifyLog(SecondDesktop).count { isRinging(it) })
    }

    // ── 2. notifications ────────────────────────────────────────────────────

    @Test
    fun a_notification_shows_on_both_and_dismissed_on_one_it_leaves_the_phone_and_the_other() {
        chooseMirror(setOf(SHELL))
        awaitValue(10_000, "the hub to be ready to mirror") { if (hub.mirroring) true else null }
        val tag = shellTag("both")
        post(tag, "Both", "on-two-computers")
        val idA = awaitNotify(Desktop, "A to show it") { it.optString("op") == "notify" && it.optString("summary") == "Both" }.getInt("id")
        val idB = awaitNotify(SecondDesktop, "B to show it") { it.optString("op") == "notify" && it.optString("summary") == "Both" }.getInt("id")

        Desktop.notifyClosed(idA, 2)
        awaitValue(15_000, "the phone to cancel what A dismissed") { if (keyOf(SHELL, tag) == null) true else null }
        awaitNotify(SecondDesktop, "B to close what A dismissed") { isClose(it, idB) }
        Thread.sleep(1_000)
        assertFalse(
            "A, which dismissed it, was sent a close for it",
            notifyLog(Desktop).any { isClose(it, idA) },
        )
    }

    @Test
    fun a_reply_from_one_computer_is_sent_and_the_notification_closes_on_the_other() {
        setReply(true)
        chooseMirror(setOf(HELPER, SHELL))
        awaitBoth("the computers to learn this phone takes replies") { can(it).contains(Phone.CAN_REPLY) }
        awaitValue(10_000, "the hub to be ready to mirror") { if (hub.mirroring) true else null }
        shell("logcat -c")
        val tag = "rime-two-reply-${System.nanoTime() % 1_000_000}".also { helperTags += it }
        shell("am start -n $HELPER/.PostActivity --es tag $tag --es title Ann --es text Hi-there")
        val shownA = awaitNotify(Desktop, "A to show it with a reply field") { it.optString("op") == "notify" && it.optString("summary") == "Ann" }
        val shownB = awaitNotify(SecondDesktop, "B to show it with a reply field") { it.optString("op") == "notify" && it.optString("summary") == "Ann" }
        assertEquals("Reply", shownA.optString("reply"))
        assertEquals("Reply", shownB.optString("reply"))
        val idA = shownA.getInt("id")
        val idB = shownB.getInt("id")

        // What A's Shell does: the reply, then the close the server sends after it.
        Desktop.notifyReply(idA, "on-my-way")
        Desktop.notifyClosed(idA, 2)
        awaitValue(15_000, "the helper app to get the reply") {
            if ("replied tag=$tag text=on-my-way" in shell("logcat -d -s $HELPER_LOG")) true else null
        }
        awaitValue(15_000, "the helper app to show its answer") {
            if ("reposted tag=$tag" in shell("logcat -d -s $HELPER_LOG")) true else null
        }
        awaitNotify(SecondDesktop, "B to close the notification A answered") { isClose(it, idB) }
        // A sentinel after the answer: each sender is serial and the listener
        // reports in order, so anything sent for the answer is in by then.
        Thread.sleep(1_000)
        post(shellTag("reply-sentinel"), "Sentinel", "after-the-answer")
        awaitNotify(Desktop, "the sentinel on A") { it.optString("summary") == "Sentinel" }
        awaitNotify(SecondDesktop, "the sentinel on B") { it.optString("summary") == "Sentinel" }
        for ((who, c) in listOf("A" to Desktop, "B" to SecondDesktop)) {
            val posts = notifyLog(c).filter { it.optString("op") == "notify" }
            assertTrue("the app's answer popped up on $who: $posts", posts.none { it.optString("body").startsWith("You:") })
            assertEquals("$who showed the replied notification again: $posts", 1, posts.count { it.optString("summary") == "Ann" })
        }
    }

    // ── 3. messages and photos, both at once ────────────────────────────────

    @Test
    fun messages_asked_by_both_at_once_each_get_their_own_complete_answer() {
        setMessages(true)
        awaitBoth("the computers to learn they may ask for messages") { can(it).contains(Phone.CAN_MESSAGES) }
        val wordA = "fora ${nonce()}"
        val wordB = "forb ${nonce()}"
        Desktop.emuSms("5556", wordA)
        Desktop.emuSms("5554", wordB)
        val threads = awaitValue(30_000, "both new conversations listed") {
            val r = Desktop.messages(a.deviceId, limit = 50)
            val t = objects(r, "threads")
            val ta = t.firstOrNull { it.optString("snippet") == wordA }
            val tb = t.firstOrNull { it.optString("snippet") == wordB }
            if (ta != null && tb != null) ta.getLong("id") to tb.getLong("id") else null
        }
        val (threadA, threadB) = threads
        assertNotEquals("the two SMS landed in one conversation", threadA, threadB)

        repeat(5) { round ->
            // A reads one conversation, B the other, released together.
            val (ra, rb) = together(
                { Desktop.messages(a.deviceId, thread = threadA) },
                { SecondDesktop.messages(b.deviceId, thread = threadB) },
            )
            assertEquals("round $round: A's read failed: $ra", "messages", ra.optString("reply"))
            assertEquals("round $round: B's read failed: $rb", "messages", rb.optString("reply"))
            assertEquals("round $round: A got another conversation: $ra", threadA, ra.optLong("thread"))
            assertEquals("round $round: B got another conversation: $rb", threadB, rb.optLong("thread"))
            assertEquals("round $round: A's newest message is not its own: $ra", wordA, objects(ra, "messages").first().getString("body"))
            assertEquals("round $round: B's newest message is not its own: $rb", wordB, objects(rb, "messages").first().getString("body"))
            assertTrue("round $round: B's answer carries A's message: $rb", objects(rb, "messages").none { it.optString("body") == wordA })
            assertTrue("round $round: A's answer carries B's message: $ra", objects(ra, "messages").none { it.optString("body") == wordB })

            // And both list at once, with different limits, so a swapped answer shows.
            val (la, lb) = together(
                { Desktop.messages(a.deviceId, limit = 2) },
                { SecondDesktop.messages(b.deviceId, limit = 1) },
            )
            assertEquals("round $round: A's list failed: $la", "messages", la.optString("reply"))
            assertEquals("round $round: B's list failed: $lb", "messages", lb.optString("reply"))
            assertEquals("round $round: A's list is not A's (limit 2): $la", 2, objects(la, "threads").size)
            assertEquals("round $round: B's list is not B's (limit 1): $lb", 1, objects(lb, "threads").size)
        }
    }

    @Test
    fun photos_asked_by_both_at_once_each_get_their_own_complete_answer() {
        val pushed = Desktop.photosPush().also { photosPushed = true }
        assertEquals("the broker made three test images: $pushed", 3, pushed.size)
        setPhotos(true)
        awaitBoth("the computers to learn they may ask for photos") { can(it).contains(Phone.CAN_PHOTOS) }

        // Both list at once, A three, B two.
        val (la, lb) = together({ Desktop.photosList(a.deviceId, 3) }, { SecondDesktop.photosList(b.deviceId, 2) })
        assertEquals("A's list failed: $la", 0, la.getInt("code"))
        assertEquals("B's list failed: $lb", 0, lb.getInt("code"))
        val listedA = listed(la)
        val listedB = listed(lb)
        assertEquals("A asked for three: $la", 3, listedA.size)
        assertEquals("B asked for two: $lb", 2, listedB.size)
        assertEquals("B's two are not the newest two A saw", listedA.take(2).map { it.getLong("id") }, listedB.map { it.getLong("id") })
        for (p in pushed) assertTrue("A's list lacks ${p.name}: $la", listedA.any { it.optString("name") == p.name })
        // Each computer's thumbnails are in its own runtime folder.
        assertTrue("A's thumbnails: ${la.optJSONArray("thumbs")}", objects(la, "thumbs").all { it.getBoolean("exists") && it.getBoolean("under_run") })
        assertTrue("B's thumbnails: ${lb.optJSONArray("thumbs")}", objects(lb, "thumbs").all { it.getBoolean("exists") && it.getBoolean("under_run") })

        // Both save a different original at once, three rounds.
        val plain = pushed.filter { !it.gps }
        val idOf = listedA.associate { it.getString("name") to it.getLong("id") }
        repeat(3) { round ->
            val (pa, pb) = if (round % 2 == 0) plain[0] to plain[1] else plain[1] to plain[0]
            val (sa, sb) = together(
                { Desktop.photosSave(a.deviceId, idOf.getValue(pa.name)) },
                { SecondDesktop.photosSave(b.deviceId, idOf.getValue(pb.name)) },
            )
            assertEquals("round $round: A's save failed: $sa", 0, sa.getInt("code"))
            assertEquals("round $round: B's save failed: $sb", 0, sb.getInt("code"))
            assertEquals("round $round: A did not get ${pa.name}: $sa", pa.sha256, sa.getJSONObject("saved").getString("sha256"))
            assertEquals("round $round: B did not get ${pb.name}: $sb", pb.sha256, sb.getJSONObject("saved").getString("sha256"))
        }
    }

    // ── 4. Focus, three parties ─────────────────────────────────────────────

    @Test
    fun focus_turned_on_from_one_computer_reaches_the_phone_then_the_other_and_settles() {
        setFocus(true)
        awaitBoth("the computers to learn this phone follows Focus") { can(it).contains(Phone.CAN_FOCUS) }
        val watch = FocusWatch().also { it.start() }
        try {
            // A's person turns Do Not Disturb on: A's Shell runs `rime remote focus on`.
            assertTrue("A's focus line reached no phone", Desktop.focus(true) >= 1)
            awaitValue(15_000, "Do Not Disturb on the phone") { if (dnd()) true else null }
            awaitValue(15_000, "B's focus.json to say this phone is in Do Not Disturb") {
                SecondDesktop.focusFile()?.takeIf { it.optString("device") == b.deviceId && it.optBoolean("dnd") }
            }
            Thread.sleep(10_000)
        } finally {
            watch.stop()
        }
        watch.assertSettled(phone = true, maxChanges = 1)
    }

    @Test
    fun focus_flipped_quickly_and_set_from_both_at_once_settles_on_one_state_everywhere() {
        setFocus(true)
        awaitBoth("the computers to learn this phone follows Focus") { can(it).contains(Phone.CAN_FOCUS) }
        val watch = FocusWatch().also { it.start() }
        try {
            // On, then straight off, from A.
            Desktop.focus(true)
            Desktop.focus(false)
            Thread.sleep(10_000)
        } finally {
            watch.stop()
        }
        watch.assertSettled(phone = false, maxChanges = 2)

        // The two persons at once: A on, B off. Whichever the phone takes
        // last, every party ends on it.
        val watch2 = FocusWatch().also { it.start() }
        try {
            together({ Desktop.focus(true) }, { SecondDesktop.focus(false) })
            Thread.sleep(10_000)
        } finally {
            watch2.stop()
        }
        watch2.assertSettled(phone = null, maxChanges = 2)
    }

    // ── 5. media ────────────────────────────────────────────────────────────

    @Test
    fun the_phones_media_shows_on_both_and_either_controls_it() {
        mediaSettings(on = true, apps = setOf(HELPER))
        awaitValue(10_000, "the hub to watch media sessions") { if (hub.mediaWatching) true else null }
        shell("logcat -c")
        helper("play", 1_000)
        val (atA, shownA) = awaitMedia(Desktop, mediaA, nameA, "A to show the helper playing, controllable") {
            it.optString("op") == "show" && it.optString("status") == "Playing" && it.optBoolean("can_control")
        }
        val (atB, shownB) = awaitMedia(SecondDesktop, mediaB, nameB, "B to show the helper playing, controllable") {
            it.optString("op") == "show" && it.optString("status") == "Playing" && it.optBoolean("can_control")
        }
        val playerA = shownA.getString("player")
        val playerB = shownB.getString("player")

        // Pause from A: the app is told, and BOTH computers see it paused.
        Desktop.mediaCall(JSONObject().put("player", playerA).put("method", "PlayPause"))
        val (cA, rA) = awaitPlayer(Desktop, mediaA, playerA, atA, "A's PlayPause outcome") { it.optString("op") == "call" && it.optString("method") == "PlayPause" }
        assertEquals("A's PlayPause: $rA", "ok", rA.getString("result"))
        val (pausedA, _) = awaitPlayer(Desktop, mediaA, playerA, cA, "A to see it paused") { it.optString("op") == "show" && it.optString("status") == "Paused" }
        val (pausedB, _) = awaitPlayer(SecondDesktop, mediaB, playerB, atB, "B to see it paused, after A paused it") { it.optString("op") == "show" && it.optString("status") == "Paused" }
        awaitValue(10_000, "the helper to be told to pause") { if ("pause" in controls()) true else null }

        // Play from B, once B shows it paused, as a person would see it.
        SecondDesktop.mediaCall(JSONObject().put("player", playerB).put("method", "PlayPause"))
        val (cB, rB) = awaitPlayer(SecondDesktop, mediaB, playerB, pausedB, "B's PlayPause outcome") { it.optString("op") == "call" && it.optString("method") == "PlayPause" }
        assertEquals("B's PlayPause: $rB", "ok", rB.getString("result"))
        awaitPlayer(SecondDesktop, mediaB, playerB, cB, "B to see it playing") { it.optString("op") == "show" && it.optString("status") == "Playing" }
        awaitPlayer(Desktop, mediaA, playerA, pausedA, "A to see it playing, after B played it") { it.optString("op") == "show" && it.optString("status") == "Playing" }
        awaitValue(10_000, "the helper to be told to play") { if ("play" in controls()) true else null }

        // Next from B: both show the new track.
        SecondDesktop.mediaCall(JSONObject().put("player", playerB).put("method", "Next"))
        awaitPlayer(SecondDesktop, mediaB, playerB, cB, "B to show the next track") { it.optString("op") == "show" && it.optString("title") == "Helper Song 2" }
        awaitPlayer(Desktop, mediaA, playerA, pausedA, "A to show the next track B skipped to") { it.optString("op") == "show" && it.optString("title") == "Helper Song 2" }
        assertEquals("each control reached the app once, in order: ${controls()}", listOf("pause", "play", "next"), controls())
    }

    // ── 6. one computer goes away ───────────────────────────────────────────

    @Test
    fun one_computer_going_away_leaves_the_other_working_and_each_link_says_where_it_stands() {
        chooseMirror(setOf(SHELL))
        awaitValue(15_000, "both links connected") {
            if (status(a) == LinkHub.Status.CONNECTED && status(b) == LinkHub.Status.CONNECTED) true else null
        }
        val hellosA = hub.state(a.deviceId)!!.hellos

        SecondDesktop.stopRemoted()
        bStopped = true
        awaitValue(20_000, "the phone to see B's link down") {
            status(b).takeIf { it == LinkHub.Status.RECONNECTING }
        }
        assertEquals("A's link moved when B went away", LinkHub.Status.CONNECTED, status(a))

        // A keeps working: a notification reaches it, and its report is live.
        val tag = shellTag("only-a")
        post(tag, "OnlyA", "while-b-is-gone")
        awaitNotify(Desktop, "A to show a notification while B is gone") { it.optString("summary") == "OnlyA" }
        assertTrue("A lost the phone's report", report(Desktop, a) != null)
        Thread.sleep(3_000)
        assertEquals("B, gone, is not reconnecting", LinkHub.Status.RECONNECTING, status(b))
        assertEquals("A's link moved while B was gone", LinkHub.Status.CONNECTED, status(a))
        assertEquals("A was reconnected while B was gone", hellosA, hub.state(a.deviceId)!!.hellos)

        // B comes back: the phone finds it by itself, and A never noticed.
        SecondDesktop.restartRemoted()
        bStopped = false
        awaitValue(40_000, "the phone to reconnect to B") { status(b).takeIf { it == LinkHub.Status.CONNECTED } }
        awaitValue(15_000, "B's report again") { report(SecondDesktop, b) }
        post(shellTag("both-again"), "BothAgain", "b-is-back")
        awaitNotify(Desktop, "A to show the next notification") { it.optString("summary") == "BothAgain" }
        awaitNotify(SecondDesktop, "B to show the next notification") { it.optString("summary") == "BothAgain" }
        assertEquals("A was reconnected when B came back", hellosA, hub.state(a.deviceId)!!.hellos)
    }

    // ---- Focus: the phone and both computers, watched ---------------------

    /**
     * Polls the phone's Do Not Disturb and both computers' focus.json every
     * 100 ms, and plays each computer's Shell as PhoneFocusService does: the
     * Shell's Do Not Disturb follows a focus.json written by this phone and
     * is never sent back.
     */
    private inner class FocusWatch {
        private val phoneSeen = mutableListOf<Boolean>()
        private val fileA = mutableListOf<Pair<Boolean, Long>>()
        private val fileB = mutableListOf<Pair<Boolean, Long>>()
        @Volatile private var running = true
        private val thread = Thread {
            while (running) {
                sample()
                Thread.sleep(100)
            }
        }

        private fun sample() {
            val d = dnd()
            if (phoneSeen.lastOrNull() != d) phoneSeen += d
            for ((list, c, m) in listOf(Triple(fileA, Desktop as Computer, a), Triple(fileB, SecondDesktop as Computer, b))) {
                val f = runCatching { c.focusFile() }.getOrNull() ?: continue
                if (f.optString("device") != m.deviceId) continue
                val v = f.getBoolean("dnd") to f.getLong("at_ms")
                if (list.lastOrNull() != v) list += v
            }
        }

        fun start() {
            sample()
            thread.start()
        }

        fun stop() {
            running = false
            thread.join(5_000)
            sample()
        }

        /** Each party's changes since [start], and the state they all ended on. */
        fun assertSettled(phone: Boolean?, maxChanges: Int) {
            val what = "phone $phoneSeen, A $fileA, B $fileB"
            val end = phoneSeen.last()
            if (phone != null) assertEquals("the phone ended in the wrong state: $what", phone, end)
            assertTrue("the phone's Do Not Disturb changed ${phoneSeen.size - 1} times: $what", phoneSeen.size - 1 <= maxChanges)
            for ((who, list) in listOf("A" to fileA, "B" to fileB)) {
                assertTrue("$who's focus.json was never written for this phone: $what", list.isNotEmpty())
                assertEquals("$who's Shell ended out of step with the phone: $what", end, list.last().first)
                assertTrue("$who's focus.json changed ${list.size - 1} times: $what", list.size - 1 <= maxChanges)
            }
        }

        override fun toString(): String = phoneSeen.toString()
    }

    // ---- helpers ----------------------------------------------------------

    private fun <X, Y> together(x: () -> X, y: () -> Y): Pair<X, Y> {
        val barrier = CyclicBarrier(2)
        val pool = Executors.newFixedThreadPool(2)
        try {
            val fx = pool.submit<X> { barrier.await(); x() }
            val fy = pool.submit<Y> { barrier.await(); y() }
            return fx.get(150, TimeUnit.SECONDS) to fy.get(150, TimeUnit.SECONDS)
        } finally {
            pool.shutdownNow()
        }
    }

    private fun status(m: PairedMachine): LinkHub.Status? = hub.state(m.deviceId)?.status

    private fun report(c: Computer, m: PairedMachine): JSONObject? =
        c.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(m.deviceId)

    private fun awaitBoth(what: String, ok: (JSONObject) -> Boolean) {
        awaitValue(15_000, "$what (A)") { report(Desktop, a)?.takeIf(ok) }
        awaitValue(15_000, "$what (B)") { report(SecondDesktop, b)?.takeIf(ok) }
    }

    private fun can(report: JSONObject): List<String> =
        report.optJSONArray("can")?.let { x -> (0 until x.length()).map { x.optString(it) } } ?: emptyList()

    private fun objects(o: JSONObject, key: String): List<JSONObject> =
        o.optJSONArray(key)?.let { x -> (0 until x.length()).map { x.getJSONObject(it) } } ?: emptyList()

    private fun listed(reply: JSONObject): List<JSONObject> =
        reply.optJSONObject("out")?.let { objects(it, "photos") } ?: emptyList()

    private fun notifyLog(c: Computer): List<JSONObject> =
        c.notifyLog().drop(if (c === Desktop) notifyA else notifyB)

    private fun awaitNotify(c: Computer, what: String, match: (JSONObject) -> Boolean): JSONObject =
        awaitValue(20_000, what) { notifyLog(c).firstOrNull(match) }

    private fun isRinging(e: JSONObject): Boolean = e.optString("op") == "notify" && e.optString("summary").startsWith("Call from")

    private fun isClose(e: JSONObject, id: Int): Boolean = e.optString("op") == "close" && e.optInt("id") == id

    /** The first media entry about [name]'s player after index [from] that matches. */
    private fun awaitMedia(c: Computer, skip: Int, name: String, what: String, match: (JSONObject) -> Boolean): Pair<Int, JSONObject> =
        awaitValue(20_000, what) {
            val all = c.mediaLog().drop(skip)
            all.indices.firstOrNull { all[it].optString("identity").startsWith("$name · ") && match(all[it]) }?.let { it to all[it] }
        }

    private fun awaitPlayer(c: Computer, skip: Int, player: String, from: Int, what: String, match: (JSONObject) -> Boolean): Pair<Int, JSONObject> =
        awaitValue(20_000, what) {
            val all = c.mediaLog().drop(skip)
            ((from + 1) until all.size).firstOrNull { all[it].optString("player") == player && match(all[it]) }?.let { it to all[it] }
        }

    private fun controls(): List<String> =
        shell("logcat -d -s $MEDIA_TAG:I").lines().mapNotNull { l -> l.substringAfter("control=", "").takeIf { it.isNotEmpty() }?.trim() }

    private fun helper(action: String, position: Long? = null) {
        val extra = position?.let { " --el position $it" }.orEmpty()
        val out = shell("am start -W -n $HELPER/.MediaActivity --es do $action$extra")
        assertFalse("the helper did not start: $out", out.contains("Error", ignoreCase = true))
    }

    private fun shellTag(what: String): String = "rime-two-$what-${System.nanoTime() % 1_000_000}".also { shellTags += it }

    private fun post(tag: String, title: String, text: String) {
        require(listOf(tag, title, text).none { it.any(Char::isWhitespace) })
        val out = shell("cmd notification post -t $title $tag $text")
        assertFalse("the shell did not post: $out", out.contains("Error", ignoreCase = true))
    }

    private fun keyOf(pkg: String, tag: String): String? =
        shell("cmd notification list").lineSequence().map { it.trim() }.firstOrNull { "|$pkg|" in it && "|$tag|" in it }

    private fun nonce(): String = (System.nanoTime() % 1_000_000).toString()

    private fun dnd(): Boolean = notifications.currentInterruptionFilter != NotificationManager.INTERRUPTION_FILTER_ALL

    private fun dndOff() {
        shell("cmd notification set_dnd off")
        awaitValue(10_000, "Do Not Disturb off") { if (!dnd()) true else null }
    }

    private fun phoneState(): String = Calls.stateOf(PhoneCalls.telephonyState(context))

    private fun endAnyCall() {
        // The far end always hangs up, even with the phone idle: a call the
        // emulator's modem dropped as it was answered stays in the modem,
        // which then ignores the next `gsm call` from that number until a
        // `gsm cancel` (measured: the next test's call never rings).
        runCatching { Desktop.emuGsm("cancel", "5556") }
        if (phoneState() == Calls.IDLE) return
        val end = System.currentTimeMillis() + 5_000
        while (phoneState() != Calls.IDLE && System.currentTimeMillis() < end) Thread.sleep(250)
        if (phoneState() != Calls.IDLE) PhoneCalls.act(context, Calls.HANGUP, null)
    }

    /** Every switch these tests use, off, written where the app keeps them and told to the hub. */
    private fun allOff() {
        runBlocking {
            repository.settings(
                repository.loadBlocking().settings.copy(
                    callsOn = false, messagesOn = false, photosOn = false, focusOn = false,
                    mirrorReply = false, mirrorApps = emptySet(), mediaOn = false, mediaApps = emptySet(), mediaControl = true,
                ),
            )
        }
        hub.setCallsOn(false)
        hub.setMessagesOn(false)
        hub.setPhotosOn(false)
        hub.setFocusOn(false)
        hub.setMirrorReply(false)
        hub.setMirrorApps(emptySet())
        hub.setMediaApps(emptySet())
        hub.setMediaControl(true)
        hub.setMediaOn(false)
    }

    private fun setCalls(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(callsOn = on)) }
        hub.setCallsOn(on)
    }

    private fun setMessages(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(messagesOn = on)) }
        hub.setMessagesOn(on)
    }

    private fun setPhotos(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(photosOn = on)) }
        hub.setPhotosOn(on)
    }

    private fun setFocus(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(focusOn = on)) }
        hub.setFocusOn(on)
    }

    private fun setReply(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mirrorReply = on)) }
        hub.setMirrorReply(on)
    }

    private fun chooseMirror(apps: Set<String>) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mirrorApps = apps)) }
        hub.setMirrorApps(apps)
    }

    private fun mediaSettings(on: Boolean, apps: Set<String>) {
        runBlocking {
            repository.settings(repository.loadBlocking().settings.copy(mediaOn = on, mediaApps = apps, mediaControl = true))
        }
        hub.setMediaApps(apps)
        hub.setMediaControl(true)
        hub.setMediaOn(on)
    }

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(100)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what")
    }

    private fun shell(command: String): String {
        val fd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(fd).bufferedReader().use { it.readText() }
    }

    private companion object {
        const val SHELL = "com.android.shell"
        /** android/testhelper: a chat notification with a Reply action, and a media session. */
        const val HELPER = "com.rimeos.remote.testhelper"
        const val HELPER_LOG = "RimeReplyHelper"
        const val MEDIA_TAG = "RimeMediaHelper"
    }
}
