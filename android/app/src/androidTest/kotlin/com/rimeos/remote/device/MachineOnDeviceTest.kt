package com.rimeos.remote.device

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.machine.Battery
import com.rimeos.remote.core.machine.Machine
import com.rimeos.remote.core.machine.MediaAction
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.ui.machine.MachineScreen
import com.rimeos.remote.ui.machine.MachineUiState
import com.rimeos.remote.ui.machine.MachineViewModel
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The "This computer" screen's view model, on a phone, against the runner's
 * rime-remoted.
 *
 * That rime-remoted answers the machine verbs from its stub
 * (`RIME_REMOTED_MACHINE_STUB`, which `run-device-suite.sh` refuses to run
 * without), so nothing here locks the screen or pauses the music of the
 * computer the suite runs on. What the phone asked for is read back from the
 * stub's action log through the broker: an action proved by the log reached
 * the computer, not just the phone's own state.
 *
 * The one fake is [Paired.PlainBox], named in `Paired.kt`.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class MachineOnDeviceTest {
    private val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as Application
    private val hub = LinkHub.get(app)
    private lateinit var machine: PairedMachine
    private lateinit var model: MachineViewModel

    @get:Rule
    val compose = createAndroidComposeRule<ComposeHostActivity>()

    private val seed = JSONObject(
        """{"machine_status":{"battery":{"percent":64,"state":"discharging"},"on_ac":false,
           "power_mode":"power-saver","locked":false,"update":"restart"},
           "media":{"player":{"name":"stubplayer","status":"playing","title":"Stub song","artist":"Stub band"},
                    "volume":{"percent":30,"muted":false}}}""",
    )

    @Before
    fun pairAndOpen() {
        Desktop.machineStubSeed(seed)
        machine = Paired.machine("machine-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.connectNow(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        for (f in listOf(Machine.STATUS, Machine.LOCK, Machine.MEDIA)) {
            assertTrue("rime-remoted must offer $f: ${remote.features}", remote.has(f))
        }
        model = MachineViewModel(app)
        model.open(machine)
    }

    @After
    fun close() {
        hub.forget(machine.deviceId)
    }

    private fun state(): MachineUiState = model.state.value

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(50)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what; the state was ${if (::model.isInitialized) state() else "not made"}")
    }

    private fun await(what: String, ok: (MachineUiState) -> Boolean) {
        awaitValue(10_000, what) { state().takeIf(ok) }
    }

    private fun actionsSince(before: Int): List<JSONObject> = Desktop.machineStubActions().drop(before)

    @Test
    fun the_phone_reads_what_the_computer_says_about_itself() {
        model.refresh()
        await("the first read") { it.loaded && it.status != null && it.media != null }
        val s = state()
        assertNull("reading must not fail", s.away)
        assertEquals(Battery(64, "discharging"), s.status!!.battery)
        assertEquals("64 %", s.status!!.power)
        assertEquals("Power saver", s.status!!.mode)
        assertEquals("Restart to finish the update", s.status!!.updateLine)
        assertEquals(false, s.status!!.locked)
        assertEquals("Stub song", s.media!!.player!!.title)
        assertEquals(30, s.media!!.volume!!.percent)
    }

    @Test
    fun media_and_volume_reach_the_computer_and_the_screen_shows_what_it_read_back() {
        model.refresh()
        await("the first read") { it.media != null }
        val before = Desktop.machineStubActions().size

        model.control(MediaAction.PLAY_PAUSE)
        await("the pause to land") { it.acting == null && it.media?.player?.playing == false }
        model.control(MediaAction.NEXT)
        await("next to land") { it.acting == null }
        model.volume(55)
        await("the volume to land") { it.acting == null && it.media?.volume?.percent == 55 }
        model.mute(true)
        await("the mute to land") { it.acting == null && it.media?.volume?.muted == true }
        assertNull("no action may fail", state().failure)

        assertEquals(
            listOf("""{"action":"play_pause"}""", """{"action":"next"}""", """{"action":"volume","percent":55}""", """{"action":"volume","muted":true}""")
                .map { JSONObject(it).toString() },
            actionsSince(before).map { it.toString() },
        )
    }

    @Test
    fun tapping_lock_on_the_screen_locks_the_computer_once_and_then_the_button_stands_down() {
        compose.setContent {
            val s by model.state.collectAsStateWithLifecycle()
            MachineScreen(
                state = s,
                onLock = { model.lock() },
                onMedia = { model.control(it) },
                onVolume = { model.volume(it) },
                onMute = { model.mute(it) },
                onRefresh = { model.refresh() },
                onDismiss = { model.dismiss() },
                onBack = {},
            )
        }
        model.refresh()
        await("the first read") { it.status?.locked == false }
        val before = Desktop.machineStubActions().size
        compose.onNodeWithText("Lock the screen").performClick()
        await("the lock to be confirmed") { it.acting == null && it.status?.locked == true }
        assertEquals("Locked", state().notice)
        compose.waitForIdle()
        compose.onNodeWithText("The screen is locked").assertIsNotEnabled()
        assertEquals(listOf(JSONObject("""{"action":"lock"}""").toString()), actionsSince(before).map { it.toString() })
        // There is no unlock: the screen offers none, and nothing sent one.
        assertTrue(compose.onAllNodesWithTextCount("Unlock") == 0)
    }

    @Test
    fun a_computer_with_no_screen_to_lock_says_so_and_nothing_is_done() {
        Desktop.machineStubSeed(JSONObject(seed.toString()).apply { getJSONObject("machine_status").put("no_screen", true) })
        val before = Desktop.machineStubActions().size
        model.lock()
        await("the refusal") { it.acting == null && it.failure != null }
        assertEquals("there is no screen to lock on this computer", state().failure)
        assertEquals(emptyList<String>(), actionsSince(before).map { it.toString() })
    }

    private fun androidx.compose.ui.test.junit4.ComposeTestRule.onAllNodesWithTextCount(text: String): Int =
        onAllNodes(androidx.compose.ui.test.hasText(text, substring = false)).fetchSemanticsNodes().size
}
