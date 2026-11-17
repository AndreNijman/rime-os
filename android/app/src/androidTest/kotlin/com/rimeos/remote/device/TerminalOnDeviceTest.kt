package com.rimeos.remote.device

import android.content.pm.ActivityInfo
import android.content.res.Configuration
import androidx.activity.compose.setContent
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.rules.ActivityScenarioRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import com.rimeos.remote.core.Settings
import com.rimeos.remote.core.agent.AgentSession
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.ui.term.TerminalController
import com.rimeos.remote.ui.term.TerminalScreen
import com.rimeos.remote.ui.term.TerminalStatus
import com.rimeos.remote.ui.theme.RimeRemoteTheme
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The terminal, on a real screen, attached to a real PTY on the computer.
 *
 * ## Why this file is not part of `Screens.kt`
 *
 * `Screens.all` is the list the accessibility, large-text and landscape walks
 * share, and it is deliberately terminal-free: every screen on it composes
 * from data alone, and a `TerminalScreen` built on a half-controller would
 * assert something no user ever sees. The terminal's own answer is not a
 * stubbed composition — it is this: pair with the computer, start a session on
 * it, attach, and drive the real `TerminalController` the app builds.
 *
 * ## Why this file does not use the Compose test rule, which every other
 * ## on-device test here does
 *
 * **Historically because it could not**, and that has changed: what follows is
 * kept because it is the record of a defect, not a standing claim.
 *
 * `TerminalScreen` used to run a `withFrameNanos` loop for the life of the
 * composition, so the Recomposer always had pending work; `ComposeTestRule.
 * setContent` ends by waiting for idleness, and on this screen that wait never
 * returned. Measured on the Pixel 7a: all three tests below failed inside
 * `setContent` with `IdlingResourceTimeoutException: Wait for [Compose-Espresso
 * link] to become idle timed out … hadRecomposerChanges = true`. That reads
 * as a limit of the harness and it was not — an idle terminal asking the
 * Choreographer for sixty frames a second is a battery defect on a phone
 * whether or not anything is testing it. It is fixed in `TerminalFrames`,
 * together with a second perpetual animation nobody had noticed: the hidden
 * field's transparent cursor, which foundation blinks for ever because a
 * transparent brush is a *specified* one. `TerminalHarnessOnDeviceTest` is the
 * on-device assertion that the standard rule can now host this screen.
 *
 * This file keeps its own host anyway, and for a better reason than it had
 * before: it drives a real PTY across a real network, so every wait here is on
 * a condition about the MACHINE — attached, echoed, resized — with its own
 * deadline. Espresso's idleness would answer a different question.
 *
 * ## What it closes, and in whose words
 *
 * P1-060 C1 names **PTY input/output** end to end, and round 1's evidence said
 * plainly what it still lacked: "no PTY output read back over an attach from
 * the device". Every byte asserted below crossed Wi-Fi twice — through
 * `rime-remoted`, into `rime-agentd`, onto a PTY master, through `/bin/cat`,
 * and back up the same path into `Terminal.feed` on this phone.
 *
 * P1-060 C2 names **rotation** and **large text**, and `LayoutOnDeviceTest`
 * answers those for the six data screens. The terminal is the screen where
 * they matter most and it was the one with no device evidence at all: text
 * size decides how many columns fit, and orientation decides it again.
 * `onSizeChanged` turns both into a `resize` the computer is told about, so
 * what is asserted is that the grid this phone measured in landscape at font
 * scale 2.0 is the grid the PTY on the other end is running at.
 *
 * ## What it does NOT claim
 *
 * Anything about a screen reader. `TerminalView` is still a bare `Canvas` and
 * still composes not one glyph, so what TalkBack reads is what
 * `TerminalReading` puts in the tree beside it — asserted on the values in
 * `TerminalSemanticsTest`, and on a phone in `TerminalHarnessOnDeviceTest`,
 * neither of which needs a machine at the other end. Nothing below looks at
 * the semantics tree at all.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class TerminalOnDeviceTest {

    @get:Rule
    val activity = ActivityScenarioRule(ComposeHostActivity::class.java)

    /** The largest scale Android's accessibility settings offer. */
    private val hugeText = 2.0f

    private var link: MachineLink? = null
    private var controller: TerminalController? = null
    private var session: AgentSession? = null

    @After
    fun detach() {
        // The terminal's `close` writes a `Close` frame on its own connection;
        // `stop` goes down the control link. A session left behind would
        // outlive this suite on the computer — `rime-agentd` is built so a
        // session survives its attachers, which is exactly what makes a
        // forgotten one easy to forget.
        runCatching { controller?.close() }
        val id = session?.id
        val live = link
        if (id != null && live != null) runCatching { live.stop(id) }
        runCatching { live?.close() }
    }

    /**
     * A terminal attached to a fresh `/bin/cat` on the computer.
     *
     * `/bin/cat` because it needs nothing, does nothing and echoes stdin — so
     * what comes back is proof that the bytes reached a PTY, not merely that a
     * frame was accepted somewhere.
     */
    private fun attachToCat(name: String): TerminalController {
        val machine = Paired.machine(name)
        val identity = Paired.identityOf(machine)
        val live = MachineLink({ Paired.session(machine, identity) })
        link = live
        val started = live.run(
            cwd = "/tmp",
            cols = 80,
            rows = 24,
            agent = "generic",
            args = listOf("/bin/cat"),
        )
        session = started
        // The controller the app builds, with the app's own connect function.
        // `TerminalScreen`'s `DisposableEffect` calls `start()`; nothing here
        // starts the pump by hand.
        val term = TerminalController(
            sessionId = started.id,
            connect = { Paired.session(machine, identity) },
        )
        controller = term
        return term
    }

    /** Compose the real screen into the host activity, and wait until it attaches. */
    private fun show(term: TerminalController, fontScale: Float?) {
        activity.scenario.onActivity { host ->
            host.setContent {
                val density = LocalDensity.current
                val scaled = if (fontScale == null) density else Density(density.density, fontScale)
                CompositionLocalProvider(LocalDensity provides scaled) {
                    RimeRemoteTheme {
                        TerminalScreen(
                            controller = term,
                            title = "cat on l16",
                            settings = Settings(),
                            onBack = {},
                        )
                    }
                }
            }
        }
        await("the terminal never reported itself attached; it says ${term.status}", ATTACH_TIMEOUT_MS) {
            term.status is TerminalStatus.Live
        }
    }

    /**
     * Poll [condition] until it holds, or fail with [why].
     *
     * The state read is written by the attachment's own thread — Compose's
     * `mutableStateOf` is readable from any thread through the global snapshot
     * — and by the frame loop on the main one, so a plain sleep-and-look is
     * both sufficient and the only thing available: see the class note on why
     * an idleness wait cannot be used on this screen.
     */
    private fun await(why: String, timeoutMs: Long, condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (condition()) return
            Thread.sleep(POLL_MS)
        }
        throw AssertionError("$why (waited ${timeoutMs}ms)")
    }

    /** Wait until [text] is somewhere in what the renderer would paint, and return that. */
    private fun awaitOnScreen(term: TerminalController, text: String): String {
        var painted = ""
        // `viewport.snapshot()` is exactly what the repaint loop hands
        // `TerminalView`, so asserting on it is asserting on the frame — and
        // on the same bytes the semantics node is built from, one layer below
        // the string a reader would be handed.
        runCatching {
            await("nothing arrived", ECHO_TIMEOUT_MS) {
                painted = term.viewport.snapshot().text()
                painted.contains(text)
            }
        }
        return painted
    }

    private fun configuration(): Configuration {
        lateinit var config: Configuration
        activity.scenario.onActivity { config = Configuration(it.resources.configuration) }
        return config
    }

    // ── the round trip ──────────────────────────────────────────────────────

    @Test
    fun what_is_typed_on_the_phone_comes_back_from_the_computers_pty() {
        val term = attachToCat("terminal-echo-test")
        show(term, fontScale = null)

        // Distinctive per run, so a stale scrollback replayed by the daemon
        // cannot answer for this one. `sendText` is the path a keystroke takes
        // from the hidden `BasicTextField`, io thread included — the path that
        // would have thrown `NetworkOnMainThreadException` and killed the
        // process outright if the controller did not have one.
        val sentinel = "rime-pty-${System.nanoTime()}"
        assertTrue(
            "the screen reported attached, so a keystroke must not be dropped",
            term.sendText("$sentinel\n"),
        )
        val painted = awaitOnScreen(term, sentinel)
        assertTrue(
            "the sentinel never came back from the computer's PTY; the screen held:\n$painted",
            painted.contains(sentinel),
        )
        assertTrue("nothing typed while attached may be reported dropped", !term.droppedInput)
    }

    @Test
    fun the_grid_the_phone_measures_in_landscape_at_font_scale_two_is_the_grid_the_pty_runs() {
        // Requested BEFORE the composition, so the first `onSizeChanged` is
        // already the landscape one and the `resize` asserted below is not a
        // race between a rotation and a first frame.
        activity.scenario.onActivity {
            it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        }
        await("the phone never rotated into landscape", ROTATE_TIMEOUT_MS) {
            val c = configuration()
            c.screenWidthDp > c.screenHeightDp
        }

        val term = attachToCat("terminal-landscape-test")
        show(term, fontScale = hugeText)

        // The screen measured a cell, divided the box by it, and called
        // `resize`. Both halves of that have to be true: the terminal in
        // memory, and the PTY on the computer, which `handle_attach` and
        // `Request::Resize` set from what this phone asked for.
        //
        // Polled, and BOTH sides are re-read each turn. `resize` is submitted
        // to the terminal's io thread and the daemon answers on its own
        // schedule, and the grid itself may move once more — the software
        // keyboard this screen asks for takes height from the box when it
        // appears. Comparing a number read before the wait against one read
        // after it is a race, and a race in a test is a flake somebody later
        // deletes the assertion to fix.
        val id = requireNotNull(session).id
        val live = requireNotNull(link)
        var cols = term.terminal.cols
        var rows = term.terminal.rows
        var seen = live.info(id)
        val deadline = System.currentTimeMillis() + RESIZE_TIMEOUT_MS
        while (System.currentTimeMillis() < deadline) {
            cols = term.terminal.cols
            rows = term.terminal.rows
            seen = live.info(id)
            if (seen.cols == cols && seen.rows == rows) break
            Thread.sleep(POLL_MS)
        }
        assertEquals("the PTY's width must be the width this phone measured", cols, seen.cols)
        assertEquals("the PTY's height must be the height this phone measured", rows, seen.rows)
        assertTrue(
            "a landscape terminal is wider than it is tall, and this one is ${cols}x$rows",
            cols > rows,
        )
        assertTrue(
            "a grid of exactly 80x24 is the shape this terminal is constructed at, so nothing " +
                "was measured on this phone at all",
            cols != 80 || rows != 24,
        )

        // And it is still a terminal somebody can use at that size: what is
        // typed has to come back through a grid this shape.
        val sentinel = "rime-landscape-${System.nanoTime()}"
        assertTrue("a keystroke on an attached landscape terminal", term.sendText("$sentinel\n"))
        val painted = awaitOnScreen(term, sentinel)
        assertTrue(
            "nothing echoed back at ${cols}x$rows, font scale $hugeText; the screen held:\n$painted",
            painted.contains(sentinel),
        )
    }

    @Test
    fun the_screen_reports_the_machine_it_attached_to_and_counts_the_attachment() {
        val term = attachToCat("terminal-attach-test")
        show(term, fontScale = null)
        assertEquals(
            "the screen counts its own attachments, and this is the first",
            1,
            term.attachments,
        )
        val status = term.status
        assertTrue("a live terminal names the machine it is on: $status", status is TerminalStatus.Live)
        assertEquals(Desktop.arg("machine"), (status as TerminalStatus.Live).machine)
    }

    private companion object {
        /** Pair, handshake, `run`, a second handshake and an `attach`, over Wi-Fi. */
        const val ATTACH_TIMEOUT_MS = 60_000L

        /** A byte to the computer and back. Generous because the radio is real. */
        const val ECHO_TIMEOUT_MS = 30_000L

        const val RESIZE_TIMEOUT_MS = 20_000L

        const val ROTATE_TIMEOUT_MS = 10_000L

        const val POLL_MS = 200L
    }
}
