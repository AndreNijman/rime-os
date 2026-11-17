package com.rimeos.remote.device

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.nex.Nex
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexTranscript
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.ui.nex.NexUiState
import com.rimeos.remote.ui.nex.NexViewModel
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
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
 * The nex screens' view model, on a phone, against a real nexd behind a real
 * rime-remoted.
 *
 * `run-device-suite.sh` starts a nexd of its own when it is given one
 * (`RIME_DEVICE_SUITE_NEXD`): a scripted provider that edits `lib.sh` and runs
 * the tests, in a scratch git folder. This file drives the SAME
 * [NexViewModel] the screens use, through the app's own [LinkHub], so what is
 * asserted is what the screens would draw. Nothing here talks to nexd except
 * through the phone's link, apart from the two things only the person at the
 * computer may do: choosing a folder, and typing a message there (which is how
 * the feed is proved: an approval that arrives without the phone having sent
 * anything can only have come down the feed).
 *
 * The one fake is [Paired.PlainBox], named in `Paired.kt`.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class NexOnDeviceTest {
    private val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as Application
    private val hub = LinkHub.get(app)
    private lateinit var machine: PairedMachine
    private lateinit var model: NexViewModel
    private val watch = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private companion object {
        /** Longer than NexViewModel's 250 ms reload debounce plus a read. */
        const val QUIET_MS = 1_000L
    }

    @Before
    fun pairAndOpen() {
        machine = Paired.machine("nex-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.connectNow(machine)
        // What the app checks before it offers nex at all, and what tells the
        // view model this machine can follow a feed.
        // The state starts with an empty hello (RemoteHello.NONE) and the real
        // one lands after the connection, so wait for a hello with features.
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer nex while its nexd answers: ${remote.features}", remote.has(Nex.FEATURE))
        model = NexViewModel(app)
    }

    @After
    fun close() {
        watch.cancel()
        if (::model.isInitialized) model.leave()
        hub.forget(machine.deviceId)
    }

    private fun state(): NexUiState = model.state.value

    /** Poll [read] until it gives a value, or fail with the screen's state. */
    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(50)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what; the nex state was ${if (::model.isInitialized) state() else "not made"}")
    }

    private fun await(timeoutMs: Long, what: String, ok: (NexUiState) -> Boolean) {
        awaitValue(timeoutMs, what) { state().takeIf(ok) }
    }

    private fun openLive(): Unit {
        model.open(machine)
        await(15_000, "the live feed and a first load") { it.live && it.loaded }
    }

    /** Approve whatever [session] asks until its turn has ended; returns how many were approved. */
    private fun approveUntilDone(session: String, timeoutMs: Long): Int {
        var approved = 0
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            val s = state()
            assertNull("approving must not fail", s.failure)
            val t = s.transcript?.takeIf { it.session.id == session }
            if (t != null && !t.running && s.pendingFor(session).isEmpty() &&
                t.entries.any { it is NexTranscript.Entry.TurnEnd && it.state != "running" }
            ) {
                return approved
            }
            val next = s.pendingFor(session).firstOrNull { it.id !in s.deciding }
            if (next != null && !s.sending) {
                model.approve(next)
                approved++
                awaitValue(10_000, "approval ${next.id} to leave the list") {
                    state().takeIf { st -> st.pendingFor(session).none { it.id == next.id } }
                }
            }
            Thread.sleep(50)
        }
        throw AssertionError("the turn in $session did not end in $timeoutMs ms; the nex state was ${state()}")
    }

    @Test
    fun an_approval_asked_at_the_computer_reaches_the_phone_by_the_feed_and_the_phone_approves_it() {
        val title = "Fix add() ${System.nanoTime()}"
        val session = Desktop.nexSessionAtComputer(title)
        openLive()
        await(10_000, "the computer's session in the list") { s -> s.sessions.any { it.id == session && it.title == title } }

        // From here the phone sends nothing until the approval is on screen,
        // and a live feed is never polled (NexViewModel.follow), so the
        // approval can only arrive by an event. The watcher makes sure the
        // feed did not drop and fall back to polling in between.
        val dropped = AtomicBoolean(false)
        watch.launch { model.state.drop(1).collect { if (!it.live) dropped.set(true) } }
        model.select(session)
        // Let the read that select() asks for land, and the debounce go quiet,
        // BEFORE the message is typed: otherwise that read could be the one
        // that finds the approval, and the feed would be proved by nothing.
        await(10_000, "the open session's transcript") { it.transcript?.session?.id == session }
        Thread.sleep(QUIET_MS)
        Desktop.nexSubmitAtComputer(session, "add() subtracts; fix it and run the tests")
        val first: NexApproval = awaitValue(10_000, "an approval for $session") { state().pendingFor(session).firstOrNull() }
        assertFalse("the feed dropped while waiting, so a poll may have found the approval", dropped.get())
        // Build mode edits without asking and asks before running a command.
        assertEquals("run_command", first.tool)
        assertTrue("running a command asks for process.exec: ${first.scopes}", "process.exec" in first.scopes)
        assertNotNull("an approval carries the fingerprint the phone must send back", first.fingerprint)

        val approved = approveUntilDone(session, 30_000)
        assertTrue("the phone approved at least the test run", approved >= 1)
        val t = awaitValue(10_000, "the finished transcript") {
            state().transcript?.takeIf { tr ->
                tr.session.id == session && tr.entries.any { it is NexTranscript.Entry.Said && it.text.contains("all tests pass") }
            }
        }
        val run = t.entries.filterIsInstance<NexTranscript.Entry.ToolUse>().firstOrNull { it.name == "run_command" }
        assertNotNull("the transcript shows the command the phone approved: ${t.entries}", run)
        // The edit fixed add(), so the approved test run passes.
        assertFalse("the approved test run failed: $run", run!!.error)
        assertEquals("the approved test run's exit code: $run", 0L, run.exitCode)
        assertEquals("Approved: run `bash test.sh`", state().notice)
    }

    @Test
    fun the_phone_denies_and_nothing_is_left_waiting() {
        val session = Desktop.nexSessionAtComputer("Deny me ${System.nanoTime()}")
        openLive()
        model.select(session)
        Desktop.nexSubmitAtComputer(session, "fix it")
        val a = awaitValue(10_000, "an approval to deny") { state().pendingFor(session).firstOrNull() }
        model.deny(a)
        await(10_000, "the denied approval to go") { s -> s.pendingFor(session).none { it.id == a.id } && !s.sending }
        assertNull("denying must not fail", state().failure)
        assertEquals("Denied: run `bash test.sh`", state().notice)
        // nexd's record of it, read back through the phone.
        val t = awaitValue(10_000, "the denial in the transcript") {
            state().transcript?.takeIf { tr ->
                tr.session.id == session &&
                    tr.entries.any { it is NexTranscript.Entry.ToolUse && it.name == a.tool && it.state == "denied" }
            }
        }
        assertTrue(t.pending.none { it.id == a.id })
    }

    @Test
    fun a_session_started_on_the_phone_runs_its_first_message_and_the_phone_can_stop_it() {
        // A folder is offered only once the computer has used it: nexd's rule.
        Desktop.nexSessionAtComputer("Seed ${System.nanoTime()}")
        openLive()
        await(10_000, "the folder to be offered") { Desktop.nexRoot() in it.roots }

        val title = "From the phone ${System.nanoTime()}"
        model.create(Desktop.nexRoot(), "scripted", title, "add() subtracts; fix it")
        val made = awaitValue(15_000, "the new session") { state().created }
        assertNull("starting a session must not fail", state().failure)
        await(10_000, "the new session in the list") { s -> s.sessions.any { it.id == made && it.title == title } }
        assertEquals("the screen opens the session it made", made, state().selected)

        // Its first message reached the agent: the agent is asking to run the tests.
        awaitValue(10_000, "the new session's first approval") { state().pendingFor(made).firstOrNull() }

        model.stop(made)
        await(10_000, "the stop to land") { !it.sending }
        assertNull("stopping must not fail", state().failure)
        assertEquals("Stopped", state().notice)
        // nexd closes an ended turn's unanswered approvals, so the question goes too.
        await(10_000, "the stopped session to stop asking") { s ->
            s.pendingFor(made).isEmpty() && s.sessions.firstOrNull { it.id == made }?.running == false
        }
    }

    @Test
    fun a_folder_nobody_chose_at_the_computer_is_refused_in_nexds_words() {
        openLive()
        val title = "Nowhere ${System.nanoTime()}"
        val before = state().sessions
        model.create("/", "scripted", title, "")
        await(15_000, "the refusal") { it.failure != null && !it.sending }
        // nexd's own sentence, not a dropped link or a locked key.
        assertEquals(
            "a paired device can only use a workspace that already has a session on this machine",
            state().failure,
        )
        assertNull("nothing was made, so nothing opens", state().created)
        // The failed write asks for a reload; once a session made at the
        // computer shows up, that reload (or a later one) has landed.
        val marker = Desktop.nexSessionAtComputer("After the refusal ${System.nanoTime()}")
        await(10_000, "a reload after the refusal") { s -> s.sessions.any { it.id == marker } }
        assertTrue("no session was made in /", state().sessions.none { it.title == title })
        assertEquals("only the computer's session is new", before.size + 1, state().sessions.size)
    }
}
