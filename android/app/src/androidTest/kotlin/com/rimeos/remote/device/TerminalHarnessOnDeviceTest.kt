package com.rimeos.remote.device

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.junit4.accessibility.enableAccessibilityChecks
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.tryPerformAccessibilityChecks
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import com.rimeos.remote.core.Settings
import com.rimeos.remote.ui.term.TerminalController
import com.rimeos.remote.ui.term.TerminalScreen
import com.rimeos.remote.ui.theme.RimeRemoteTheme
import org.junit.After
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch

/**
 * The terminal, in the standard Compose harness, with no machine on the other
 * end.
 *
 * ## Why this file exists
 *
 * Round 33 recorded that `TerminalScreen` **cannot** be hosted by
 * `createAndroidComposeRule`: the repaint loop left the `Recomposer`
 * permanently busy, so `setContent` never returned from its own
 * `waitForIdle` — `IdlingResourceTimeoutException … hadRecomposerChanges =
 * true`, measured on a Pixel 7a, on all three tests that tried. That was true,
 * and it was a defect in the screen rather than a limit of the harness. Two
 * things caused it and both are fixed:
 *
 * 1. the repaint loop asked for a frame unconditionally, every frame, for ever
 *    — now it waits for a change first (`TerminalFrames`, with the rules
 *    asserted in `:app`'s JVM suite);
 * 2. the hidden input field's `cursorBrush` was `SolidColor(Color.Transparent)`,
 *    and foundation only skips the blink animation for `Color.Unspecified`, so
 *    an invisible cursor was being animated for ever on a focused field.
 *
 * **This test is the assertion that both are really fixed on a phone.** Its
 * subject is not what the screen shows; it is that the harness returns at all.
 * A regression in either cause makes every method below hang inside
 * `setContent` and fail on the rule's own timeout.
 *
 * ## Why no machine
 *
 * `TerminalOnDeviceTest` drives a real PTY over Wi-Fi and is the evidence for
 * P1-055 C1 and C3. This one is about the harness and the semantics tree, and
 * both of those are decided before a byte crosses the network — so `connect`
 * blocks for ever on a latch, the screen sits in `Connecting…`, and the
 * terminal is fed directly. A `connect` that *threw* would be worse than
 * useless here: it would start the reconnect backoff and fill the run with
 * `Lost` events that have nothing to do with what is being asserted.
 *
 * ## Run on a device
 *
 * First run on a device on 2026-11-17 (Android 36 emulator): green, after
 * fixing a terminal resize that dropped the prompt and a hidden field that was
 * a stop for screen readers. TalkBack itself was not installed on that image.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class TerminalHarnessOnDeviceTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComposeHostActivity>()

    /** Held for the life of the test, so nothing ever connects. */
    private val never = CountDownLatch(1)

    private var controller: TerminalController? = null

    @After
    fun release() {
        never.countDown()
        runCatching { controller?.close() }
    }

    private fun show(): TerminalController {
        val term = TerminalController(
            sessionId = 1,
            connect = {
                never.await()
                error("unreachable: the latch is only released when the test is over")
            },
        )
        controller = term
        compose.setContent {
            RimeRemoteTheme {
                TerminalScreen(
                    controller = term,
                    title = "harness",
                    settings = Settings(),
                    onBack = {},
                )
            }
        }
        return term
    }

    @Test
    fun the_standard_harness_can_host_the_terminal() {
        // The whole test is the two calls below returning. `setContent` ends
        // by waiting for idleness and `waitForIdle` waits again; on the screen
        // as it was, neither ever came back.
        show()
        compose.waitForIdle()
        val nodes = compose.onAllNodes(isRoot()).fetchSemanticsNodes()
        if (nodes.isEmpty()) {
            throw AssertionError(
                "the terminal composed no semantics at all, so nothing below looked at " +
                    "anything. Check that the phone is awake and that ComposeHostActivity " +
                    "still declares showWhenLocked.",
            )
        }
    }

    @Test
    fun what_the_terminal_holds_is_in_the_tree_a_screen_reader_walks() {
        // `TerminalView` is a Canvas — the grid is painted, not composed — so
        // before the semantics were added this assertion could not have been
        // written at all: there was no node carrying a single glyph of it.
        val term = show()
        val sentinel = "rime-harness-${System.nanoTime()}"
        // No `invalidate` after it: `feed` notifies the loop by itself, and a
        // test that nudged the loop by hand would pass over a build where the
        // controller had stopped listening to its terminal.
        term.terminal.feed("$sentinel\r\n")
        compose.waitForIdle()
        compose.onNode(hasText(sentinel, substring = true)).assertIsDisplayed()
    }

    @Test
    fun the_terminal_is_something_a_screen_reader_can_activate() {
        // A tap raises the keyboard, and it used to be a bare `pointerInput`
        // with nothing in the semantics tree: no action to perform, and
        // nothing announced when a reader tried.
        val term = show()
        term.terminal.feed("prompt$ ")
        compose.waitForIdle()
        // `prompt$` and not `prompt$ `: the reading trims each row's trailing
        // blanks (a row is padded to the full width), so the space the shell
        // printed is not in it.
        compose.onAllNodes(hasText("prompt$", substring = true) and hasClickAction())
            .onFirst()
            .assertIsDisplayed()
    }

    @Test
    fun the_terminal_passes_the_accessibility_test_framework() {
        // Performed explicitly, because `enableAccessibilityChecks` runs ATF
        // as a side effect of `perform*` actions: a test that composed a
        // screen and asserted nothing would run no checks and pass.
        compose.enableAccessibilityChecks()
        val term = show()
        term.terminal.feed("cargo test\r\n3407 passed\r\n")
        compose.waitForIdle()
        try {
            compose.onAllNodes(isRoot()).tryPerformAccessibilityChecks()
        } catch (e: Throwable) {
            throw AssertionError("the terminal fails an accessibility check: ${e.message}", e)
        }
    }
}
