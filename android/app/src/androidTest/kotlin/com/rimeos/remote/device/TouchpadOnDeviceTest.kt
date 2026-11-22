package com.rimeos.remote.device

import android.app.Application
import android.content.pm.ActivityInfo
import android.graphics.Bitmap
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import androidx.compose.runtime.getValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Settings
import com.rimeos.remote.core.input.Input
import com.rimeos.remote.core.input.KeyMap
import com.rimeos.remote.core.input.TouchpadPhase
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.ui.touchpad.TouchpadScreen
import com.rimeos.remote.ui.touchpad.TouchpadTags
import com.rimeos.remote.ui.touchpad.TouchpadUiState
import com.rimeos.remote.ui.touchpad.TouchpadViewModel
import java.io.File
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The phone as a touchpad and keyboard (contract §2.16), on a phone, against
 * the runner's rime-remoted.
 *
 * That rime-remoted is started with `RIME_REMOTED_INPUT_STUB` (the runner
 * refuses to start it without), so nothing here moves the pointer of, or
 * types into, the computer the suite runs on: every action lands in the
 * stub's log, which the broker reads back. An action proved by that log
 * reached the computer's input path, not just the phone's state.
 *
 * Touches are real MotionEvents, dispatched into the activity's window with
 * the clock the app reads (not the Compose test clock), so tap timing and
 * the pointer curve see what a finger would give them. Into the window and
 * not through the input system, because the runner turns TalkBack on, and
 * TalkBack's touch exploration would read an injected swipe as its own
 * gesture.
 *
 * Each test pairs a fresh phone, because the computer's 30-second cool-down
 * after a stop is per phone. The one fake is [Paired.PlainBox].
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class TouchpadOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val app = instrumentation.targetContext.applicationContext as Application
    private val hub = LinkHub.get(app)
    private lateinit var machine: PairedMachine
    private lateinit var model: TouchpadViewModel
    private lateinit var name: String

    @get:Rule
    val compose = createAndroidComposeRule<ComposeHostActivity>()

    @Before
    fun unlocked() {
        Desktop.inputStubGate(locked = false)
    }

    @After
    fun close() {
        if (::model.isInitialized) instrumentation.runOnMainSync { model.leave() }
        if (::machine.isInitialized) hub.forget(machine.deviceId)
        Desktop.inputStubGate(locked = false)
    }

    /** Pair a fresh phone, allowed or not, connect, and show the touchpad. */
    private fun open(allowed: Boolean) {
        name = "touchpad-${System.nanoTime() % 100_000}"
        machine = Paired.machine(name)
        if (allowed) Desktop.grantInput(machine.deviceId, true)
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.connectNow(machine)
        val remote = awaitValue(15_000, "the computer's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Input.FEATURE}: ${remote.features}", remote.has(Input.FEATURE))
        assertEquals(if (allowed) "on" else "off", remote.input)
        instrumentation.runOnMainSync {
            model = TouchpadViewModel(app)
            model.open(machine, name)
        }
        compose.setContent {
            val s by model.state.collectAsStateWithLifecycle()
            TouchpadScreen(model, s, Settings(), onNatural = {}, onSpeed = {}, onBack = {})
        }
        instrumentation.runOnMainSync { model.enter() }
    }

    private fun state(): TouchpadUiState = model.state.value

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(50)
        }
        throw AssertionError(
            "timed out after $timeoutMs ms waiting for $what; the touchpad was " +
                (if (::model.isInitialized) state().phase.toString() else "not made"),
        )
    }

    private fun await(what: String, timeoutMs: Long = 10_000, ok: (TouchpadPhase) -> Boolean) {
        awaitValue(timeoutMs, what) { state().phase.takeIf(ok) }
    }

    private fun actionsSince(before: Int): List<JSONObject> = Desktop.inputStubActions().drop(before)

    /** The stub's actions after [before], once [ok] holds for them. */
    private fun landed(what: String, before: Int, ok: (List<JSONObject>) -> Boolean): List<JSONObject> =
        awaitValue(10_000, what) { actionsSince(before).takeIf(ok) }

    private fun List<JSONObject>.of(a: String) = filter { it.optString("a") == a }

    // ── touches, injected as a finger's ─────────────────────────────────

    /** The pad's centre in the window, and pixels per dp. */
    private fun padCentre(): Pair<Offset, Float> {
        compose.waitForIdle()
        val node = compose.onNodeWithTag(TouchpadTags.PAD).fetchSemanticsNode()
        val at = node.positionInWindow
        return Offset(at.x + node.size.width / 2f, at.y + node.size.height / 2f) to
            app.resources.displayMetrics.density
    }

    private fun inject(down: Long, action: Int, points: List<Offset>) {
        val props = Array(points.size) { i -> MotionEvent.PointerProperties().apply { id = i; toolType = MotionEvent.TOOL_TYPE_FINGER } }
        val coords = Array(points.size) { i -> MotionEvent.PointerCoords().apply { x = points[i].x; y = points[i].y; pressure = 1f; size = 1f } }
        val e = MotionEvent.obtain(
            down, SystemClock.uptimeMillis(), action, points.size, props, coords,
            0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0,
        )
        try {
            instrumentation.runOnMainSync { compose.activity.window.decorView.dispatchTouchEvent(e) }
        } finally {
            e.recycle()
        }
    }

    /**
     * [fingers] fingers down at [start] (side by side), moved by [delta] in
     * [steps] steps of [stepMs], then lifted. Zero delta is a tap.
     */
    private fun touch(start: Offset, delta: Offset, fingers: Int = 1, steps: Int = 12, stepMs: Long = 12, holdMs: Long = 40) {
        val gap = 90f
        fun at(t: Float) = (0 until fingers).map { Offset(start.x + it * gap + delta.x * t, start.y + delta.y * t) }
        val down = SystemClock.uptimeMillis()
        inject(down, MotionEvent.ACTION_DOWN, at(0f).take(1))
        for (i in 1 until fingers) {
            inject(down, MotionEvent.ACTION_POINTER_DOWN or (i shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), at(0f).take(i + 1))
        }
        if (delta == Offset.Zero) {
            Thread.sleep(holdMs)
        } else {
            for (s in 1..steps) {
                Thread.sleep(stepMs)
                inject(down, MotionEvent.ACTION_MOVE, at(s / steps.toFloat()))
            }
        }
        val end = at(if (delta == Offset.Zero) 0f else 1f)
        for (i in fingers - 1 downTo 1) {
            inject(down, MotionEvent.ACTION_POINTER_UP or (i shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), end.take(i + 1))
        }
        inject(down, MotionEvent.ACTION_UP, end.take(1))
    }

    // ── the tests ────────────────────────────────────────────────────────

    @Test
    fun moving_tapping_scrolling_typing_and_keys_all_reach_the_computer() {
        open(allowed = true)
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        shot("ready")
        val (centre, dp) = padCentre()
        var before = Desktop.inputStubActions().size

        // One finger: the pointer moves, right and down.
        touch(centre - Offset(120 * dp, 0f), Offset(140 * dp, 60 * dp))
        val moved = landed("the pointer to move", before) { a -> a.of("motion").sumOf { it.getDouble("dx") } > 140.0 }
            .of("motion")
        assertTrue("it moved right: $moved", moved.sumOf { it.getDouble("dx") } > 0)
        assertTrue("it moved down: $moved", moved.sumOf { it.getDouble("dy") } > 0)
        assertTrue("no button was pressed by a move: $moved", actionsSince(before).of("button").isEmpty())

        // A tap: one left click, pressed then released.
        before = Desktop.inputStubActions().size
        touch(centre, Offset.Zero)
        val click = landed("the tap's click", before) { it.of("button").size >= 2 }.of("button")
        assertEquals(
            listOf("${Input.BTN_LEFT}:true", "${Input.BTN_LEFT}:false"),
            click.map { "${it.getInt("code")}:${it.getBoolean("p")}" },
        )

        // Two fingers tapped together: a right click.
        before = Desktop.inputStubActions().size
        touch(centre - Offset(45f, 0f), Offset.Zero, fingers = 2, holdMs = 60)
        val right = landed("the right click", before) { it.of("button").size >= 2 }.of("button")
        assertEquals(
            listOf("${Input.BTN_RIGHT}:true", "${Input.BTN_RIGHT}:false"),
            right.map { "${it.getInt("code")}:${it.getBoolean("p")}" },
        )

        // Two fingers dragged: a finger scroll that ends with a stop.
        before = Desktop.inputStubActions().size
        touch(centre - Offset(45f, 80 * dp), Offset(0f, 160 * dp), fingers = 2)
        val scrolls = landed("the scroll and its stop", before) { a -> a.of("scroll").any { it.getBoolean("stop") } }
            .of("scroll")
        assertTrue("every scroll is a finger's: $scrolls", scrolls.all { it.getString("src") == "finger" })
        assertTrue("the last is the stop: $scrolls", scrolls.last().getBoolean("stop"))
        // Natural scrolling: fingers moving down scroll the content down, which is up on the wheel's axis.
        assertTrue("it scrolled: $scrolls", scrolls.sumOf { it.getDouble("dy") } < 0)

        // Typing: committed text arrives as text, through the computer's keymap.
        before = Desktop.inputStubActions().size
        compose.onNodeWithTag(TouchpadTags.FIELD, useUnmergedTree = true).performTextInput("hi")
        val typed = landed("the typed text", before) { a -> a.of("text_key").count { it.getBoolean("p") } >= 2 }
        assertTrue("the keymap holds what was typed: $typed", typed.of("text_keymap").any { it.getString("chars").contains("h") && it.getString("chars").contains("i") })

        // A key on the strip: one tap of Esc.
        before = Desktop.inputStubActions().size
        compose.onNodeWithContentDescription("Escape").performClick()
        val esc = landed("Esc", before) { it.of("key").size >= 2 }.of("key")
        assertEquals(
            listOf("${KeyMap.ESC}:true", "${KeyMap.ESC}:false"),
            esc.map { "${it.getInt("code")}:${it.getBoolean("p")}" },
        )

        // Leaving closes the channel at the computer.
        before = Desktop.inputStubActions().size
        instrumentation.runOnMainSync { model.leave() }
        landed("the channel to close", before) { a -> a.any { it.optString("a") == "close" } }
    }

    @Test
    fun leaving_mid_drag_lets_go_of_the_button_on_the_computer() {
        open(allowed = true)
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        val (centre, dp) = padCentre()
        val before = Desktop.inputStubActions().size
        // Tap, then touch again at once and hold: the left button is held for the drag.
        touch(centre, Offset.Zero, holdMs = 30)
        val down = SystemClock.uptimeMillis()
        inject(down, MotionEvent.ACTION_DOWN, listOf(centre))
        for (s in 1..6) {
            Thread.sleep(16)
            inject(down, MotionEvent.ACTION_MOVE, listOf(centre + Offset(s * 8 * dp, 0f)))
        }
        landed("the drag to hold the button", before) { a ->
            a.of("button").any { it.getInt("code") == Input.BTN_LEFT && it.getBoolean("p") }
        }
        // Held past the computer's 750 ms lease: the phone's holds keep it.
        Thread.sleep(1_500)
        val held = actionsSince(before).of("button")
        assertEquals("the hold must keep the button down: $held", listOf("${Input.BTN_LEFT}:true"), held.map { "${it.getInt("code")}:${it.getBoolean("p")}" })

        // The screen goes, the finger still down.
        instrumentation.runOnMainSync { model.leave() }
        val after = landed("the release and close", before) { a -> a.any { it.optString("a") == "close" } }
        val buttons = after.of("button").map { "${it.getInt("code")}:${it.getBoolean("p")}" }
        assertEquals(listOf("${Input.BTN_LEFT}:true", "${Input.BTN_LEFT}:false"), buttons)
        val releaseAt = after.indexOfFirst { it.optString("a") == "button" && !it.getBoolean("p") }
        val closeAt = after.indexOfFirst { it.optString("a") == "close" }
        assertTrue("the release came from the phone, before the close: $after", releaseAt in 0 until closeAt)
        inject(down, MotionEvent.ACTION_UP, listOf(centre))
    }

    @Test
    fun a_locked_computer_pauses_the_touchpad_and_nothing_lands_until_it_is_unlocked() {
        open(allowed = true)
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        val (centre, dp) = padCentre()

        Desktop.inputStubGate(locked = true)
        await("the pause") { it == TouchpadPhase.Paused }
        compose.waitForIdle()
        compose.onNodeWithText("$machineName is locked").assertExists()
        shot("paused")
        val before = Desktop.inputStubActions().size
        touch(centre - Offset(100 * dp, 0f), Offset(160 * dp, 0f))
        touch(centre, Offset.Zero)
        compose.onNodeWithContentDescription("Escape").assertIsNotEnabled()
        Thread.sleep(1_500)
        val during = actionsSince(before).filter { it.optString("a") in setOf("motion", "button", "key", "scroll", "text_key") }
        assertEquals("nothing may reach a locked computer: $during", emptyList<JSONObject>(), during)

        Desktop.inputStubGate(locked = false)
        await("the resume") { it == TouchpadPhase.Ready }
        val again = Desktop.inputStubActions().size
        touch(centre - Offset(100 * dp, 0f), Offset(160 * dp, 0f))
        landed("the pointer to move after the unlock", again) { a -> a.of("motion").isNotEmpty() }
    }

    @Test
    fun stopped_at_the_computer_closes_and_a_reopen_inside_the_cool_down_is_refused() {
        open(allowed = true)
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        val before = Desktop.inputStubActions().size

        val reply = Desktop.stopInput()
        assertEquals(true, reply.optBoolean("stopped"))
        await("the stop to reach the phone") { it is TouchpadPhase.Stopped }
        landed("the channel to close at the computer", before) { a -> a.any { it.optString("a") == "close" } }
        val until = (state().phase as TouchpadPhase.Stopped).until
        val left = until - SystemClock.uptimeMillis()
        assertTrue("the cool-down is the computer's 30 s: $left ms", left in 20_000..30_500)
        compose.waitForIdle()
        compose.onNodeWithText("Stopped at the computer").assertExists()
        shot("stopped")

        // Not tried again by itself.
        val opensBefore = Desktop.inputStubActions().count { it.optString("a") == "open" }
        Thread.sleep(5_000)
        assertTrue("still stopped", state().phase is TouchpadPhase.Stopped)
        assertEquals("no open was tried", opensBefore, Desktop.inputStubActions().count { it.optString("a") == "open" })

        // Asked to, inside the cool-down: the computer refuses, and says how long is left.
        instrumentation.runOnMainSync { model.retry() }
        await("the computer's refusal") { it is TouchpadPhase.Stopped && it.until != until }
        val again = (state().phase as TouchpadPhase.Stopped).until - SystemClock.uptimeMillis()
        assertTrue("the refusal carries what is left: $again ms", again in 15_000..26_000)
        assertEquals("the stub was never opened", opensBefore, Desktop.inputStubActions().count { it.optString("a") == "open" })

        // Allowed again at the computer: the cool-down is lifted at once.
        Desktop.grantInput(machine.deviceId, true)
        instrumentation.runOnMainSync { model.retry() }
        await("the touchpad to open again") { it == TouchpadPhase.Ready }
    }

    @Test
    fun a_phone_that_is_not_allowed_sees_how_to_allow_it_and_opens_once_it_is() {
        open(allowed = false)
        await("the explanation") { it is TouchpadPhase.GrantOff }
        val command = (state().phase as TouchpadPhase.GrantOff).command
        assertEquals("rime remote devices grant $name input on", command)
        compose.waitForIdle()
        compose.onNodeWithText("Turn the touchpad on at the computer").assertExists()
        compose.onNodeWithTag(TouchpadTags.COMMAND, useUnmergedTree = true).assertExists()
        compose.onNodeWithText(command).assertExists()
        compose.onNodeWithContentDescription("Escape").assertIsNotEnabled()
        shot("grant-off")
        val before = Desktop.inputStubActions().size
        assertTrue("nothing was opened at the computer", actionsSince(before).none { it.optString("a") == "open" })

        Desktop.grantInput(machine.deviceId, true)
        compose.onNodeWithText("Check again").performClick()
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        landed("the stub to open", before) { a -> a.any { it.optString("a") == "open" } }
    }

    /** Screenshots for the review, only when asked for: `-e touchpad_shots 1`. */
    @Test
    fun screenshots() {
        if (InstrumentationRegistry.getArguments().getString("touchpad_shots").isNullOrBlank()) return
        open(allowed = true)
        await("the touchpad to open") { it == TouchpadPhase.Ready }
        Thread.sleep(600)
        shot("ready")
        // Two fingers resting on the pad, as they are drawn.
        val (centre, _) = padCentre()
        val down = SystemClock.uptimeMillis()
        inject(down, MotionEvent.ACTION_DOWN, listOf(centre - Offset(80f, 0f)))
        inject(down, MotionEvent.ACTION_POINTER_DOWN or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), listOf(centre - Offset(80f, 0f), centre + Offset(80f, 40f)))
        Thread.sleep(300)
        shot("fingers")
        inject(down, MotionEvent.ACTION_POINTER_UP or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), listOf(centre - Offset(80f, 0f), centre + Offset(80f, 40f)))
        inject(down, MotionEvent.ACTION_UP, listOf(centre - Offset(80f, 0f)))
        // A modifier armed, one locked, and the extra keys open.
        compose.onNodeWithText("Ctrl").performClick()
        compose.onNodeWithText("Shift").performClick()
        compose.onNodeWithText("Shift").performClick()
        compose.onNodeWithContentDescription("More keys").performClick()
        Thread.sleep(400)
        shot("keys")
        compose.onNodeWithText("Options").performClick()
        Thread.sleep(400)
        shot("options")
        compose.onNodeWithText("Done").performClick()
        compose.activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        Thread.sleep(2_000)
        shot("landscape")
        Desktop.inputStubGate(locked = true)
        await("the pause") { it == TouchpadPhase.Paused }
        Thread.sleep(600)
        shot("landscape-paused")
        compose.activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        Thread.sleep(2_000)
        shot("paused")
        Desktop.inputStubGate(locked = false)
        await("the resume") { it == TouchpadPhase.Ready }
        Desktop.stopInput()
        await("the stop") { it is TouchpadPhase.Stopped }
        Thread.sleep(600)
        shot("stopped")
    }

    private val machineName: String get() = machine.machine

    private fun shot(label: String) {
        if (InstrumentationRegistry.getArguments().getString("touchpad_shots").isNullOrBlank()) return
        compose.waitForIdle()
        Thread.sleep(250)
        val bmp: Bitmap = instrumentation.uiAutomation.takeScreenshot() ?: return
        val dir = File(app.getExternalFilesDir(null), "touchpad-shots").apply { mkdirs() }
        File(dir, "$label.png").outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
}
