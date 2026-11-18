package com.rimeos.remote.device

import android.app.NotificationManager
import android.content.Context
import android.os.ParcelFileDescriptor
import android.telephony.TelephonyManager
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.MirrorService
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The phone's network and Do Not Disturb on the computer, and Focus both
 * ways (contract §2.9), on a phone, against the runner's rime-remoted.
 *
 * The computer side is the real daemon: its device record through the
 * broker's forwarded `devices`, `rime remote focus` as the forwarded `focus`
 * request, and `focus.json` read by the broker's `focus_file` out of the
 * run's own runtime directory. The Shell, and the file the desktop's own
 * Shell watches, are never touched.
 *
 * Do Not Disturb on the phone is set from outside the app with
 * `cmd notification set_dnd`, as the shade would, and read back from
 * NotificationManager. Every test starts and ends with it off: a phone left
 * in Do Not Disturb would silence the ring tests.
 *
 * Measured here (Android 16, this app targeting 36): a computer's "off" does
 * not end a Do Not Disturb the person turned on at the phone, and the
 * shell's `set_dnd off` does not end one this app turned on. Android keeps
 * the two as separate modes. So the "off" tests turn off only what the
 * computer turned on.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class FocusOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val listener = MirrorService.component(context).flattenToString()
    private val notifications: NotificationManager get() = context.getSystemService(NotificationManager::class.java)
    private lateinit var machine: PairedMachine

    @Before
    fun pairAndListen() {
        shell("cmd notification allow_listener $listener")
        awaitValue(15_000, "Android to grant notification access") { if (MirrorService.granted(context)) true else null }
        // A Do Not Disturb this app turned on is its own mode, which the
        // shell's `set_dnd off` does not end (measured, Android 16): only the
        // listener takes it back. Bound by now, or the first test fails here.
        awaitValue(15_000, "the listener to bind") { if (MirrorService.focus(false)) true else null }
        dndOff()
        setFocus(false)
        machine = Paired.machine("focus-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Phone.FOCUS}: ${remote.features}", remote.has(Phone.FOCUS))
    }

    @After
    fun tidy() {
        // Whatever this app turned on through the listener, then whatever the shell did.
        MirrorService.focus(false)
        dndOff()
        hub.lock()
        setFocus(false)
        shell("cmd notification disallow_listener $listener")
    }

    @Test
    fun the_computer_hears_the_network_and_do_not_disturb_and_focus_only_once_switched_on() {
        val before = awaitValue(15_000, "the first report with the network and dnd") {
            report()?.takeIf { it.has("network") && it.has("dnd") }
        }
        assertFalse("the report said focus with the switch off: $before", can(before).contains(Phone.CAN_FOCUS))
        assertEquals("Do Not Disturb is off, the report says otherwise: $before", false, before.getBoolean("dnd"))

        setFocus(true)
        val after = awaitValue(15_000, "the computer to learn this phone follows it") {
            report()?.takeIf { can(it).contains(Phone.CAN_FOCUS) }
        }
        val network = after.getJSONObject("network")
        val type = network.getString("type")
        assertTrue("the emulator is on Wi-Fi or mobile data, not $type: $after", type == "wifi" || type == "cellular")
        if (type == "wifi") {
            // The emulator's Wi-Fi has a signal (-50 dBm): leaving it out
            // would mean Android hid it, which it must not without location.
            assertTrue("Wi-Fi without bars: $after", network.has("signal"))
            assertTrue("bars out of range: $after", network.getInt("signal") in 0..4)
        }
        assertEquals(false, after.getBoolean("dnd"))
        // The cellular bars need no permission: the read is not refused.
        val level = context.getSystemService(TelephonyManager::class.java).signalStrength?.level
        assertTrue("cellular bars out of range: $level", level == null || level in 0..4)

        setFocus(false)
        awaitValue(15_000, "the computer to learn this phone no longer follows it") {
            report()?.takeIf { !can(it).contains(Phone.CAN_FOCUS) }
        }
    }

    @Test
    fun the_computer_turns_the_phones_do_not_disturb_on_and_off() {
        setFocus(true)
        awaitValue(15_000, "the computer to learn this phone follows it") { report()?.takeIf { can(it).contains(Phone.CAN_FOCUS) } }

        assertTrue("the focus line reached no phone", Desktop.focus(true) >= 1)
        awaitValue(15_000, "Do Not Disturb on the phone") { if (dnd()) true else null }
        awaitValue(15_000, "the phone to report Do Not Disturb on") { report()?.takeIf { it.optBoolean("dnd") } }

        assertTrue("the focus line reached no phone", Desktop.focus(false) >= 1)
        awaitValue(15_000, "Do Not Disturb off on the phone") { if (!dnd()) true else null }
        awaitValue(15_000, "the phone to report Do Not Disturb off") {
            report()?.takeIf { it.has("dnd") && !it.getBoolean("dnd") }
        }
    }

    @Test
    fun do_not_disturb_turned_on_at_the_phone_reaches_the_computers_focus_file() {
        setFocus(true)
        awaitValue(15_000, "the computer to learn this phone follows it") { report()?.takeIf { can(it).contains(Phone.CAN_FOCUS) } }

        shell("cmd notification set_dnd on")
        val on = awaitValue(15_000, "focus.json to say this phone's Do Not Disturb is on") {
            Desktop.focusFile()?.takeIf { it.optString("device") == machine.deviceId && it.optBoolean("dnd") }
        }
        shell("cmd notification set_dnd off")
        awaitValue(15_000, "focus.json to say it is off again") {
            Desktop.focusFile()?.takeIf {
                it.optString("device") == machine.deviceId && !it.getBoolean("dnd") && it.getLong("at_ms") >= on.getLong("at_ms")
            }
        }
    }

    @Test
    fun with_the_switch_off_neither_side_follows_the_other() {
        // On, then off: the machine has heard both, so it must drop this phone.
        setFocus(true)
        awaitValue(15_000, "the computer to learn this phone follows it") { report()?.takeIf { can(it).contains(Phone.CAN_FOCUS) } }
        setFocus(false)
        awaitValue(15_000, "the computer to learn this phone no longer follows it") {
            report()?.takeIf { !can(it).contains(Phone.CAN_FOCUS) }
        }

        assertEquals("a phone that does not follow the computer was sent a focus line", 0, Desktop.focus(true))
        Thread.sleep(2_000)
        assertFalse("Do Not Disturb came on with the switch off", dnd())

        // The first report that said "focus" wrote this phone's `false`
        // already; nothing after the switch went off may write again.
        val before = Desktop.focusFile()?.toString()
        shell("cmd notification set_dnd on")
        awaitValue(15_000, "the phone to report Do Not Disturb on") { report()?.takeIf { it.optBoolean("dnd") } }
        assertEquals(
            "focus.json followed a phone that does not follow the computer",
            before,
            Desktop.focusFile()?.toString(),
        )
    }

    @Test
    fun turning_the_switch_off_ends_a_do_not_disturb_the_computer_turned_on() {
        computerTurnsDndOn()
        setFocus(false)
        awaitValue(15_000, "the switch going off to end the computer's Do Not Disturb") { if (!dnd()) true else null }
    }

    @Test
    fun locking_ends_a_do_not_disturb_the_computer_turned_on() {
        computerTurnsDndOn()
        hub.lock()
        awaitValue(15_000, "locking to end the computer's Do Not Disturb") { if (!dnd()) true else null }
    }

    @Test
    fun the_persons_own_do_not_disturb_survives_the_switch_going_off() {
        computerTurnsDndOn()
        // The person turns theirs on too, at the phone.
        shell("cmd notification set_dnd on")
        setFocus(false)
        Thread.sleep(2_000)
        assertTrue("the switch going off ended the person's own Do Not Disturb", dnd())
        // The shell's `off` ends only the person's (see the class comment): the
        // phone leaves Do Not Disturb only if the app's own was ended already.
        shell("cmd notification set_dnd off")
        awaitValue(15_000, "the phone out of Do Not Disturb, the app's own ended by the switch") { if (!dnd()) true else null }
    }

    // ---- helpers --------------------------------------------------------

    /** The switch on, the computer told, and its `rime remote focus on` applied here. */
    private fun computerTurnsDndOn() {
        setFocus(true)
        awaitValue(15_000, "the computer to learn this phone follows it") { report()?.takeIf { can(it).contains(Phone.CAN_FOCUS) } }
        assertTrue("the focus line reached no phone", Desktop.focus(true) >= 1)
        awaitValue(15_000, "Do Not Disturb on the phone") { if (dnd()) true else null }
    }

    /** "Do Not Disturb follows your computer", turned on or off as the screen does. */
    private fun setFocus(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(focusOn = on)) }
        hub.setFocusOn(on)
    }

    private fun dnd(): Boolean = notifications.currentInterruptionFilter != NotificationManager.INTERRUPTION_FILTER_ALL

    private fun dndOff() {
        shell("cmd notification set_dnd off")
        awaitValue(10_000, "Do Not Disturb off") { if (!dnd()) true else null }
    }

    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun can(report: JSONObject): List<String> =
        report.optJSONArray("can")?.let { a -> (0 until a.length()).map { a.optString(it) } } ?: emptyList()

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
}
