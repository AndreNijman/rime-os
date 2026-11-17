package com.rimeos.remote.device

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.Ringer
import com.rimeos.remote.ui.Notifier
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * "Ring my phone" and the phone's battery, on a phone, against the runner's
 * rime-remoted (contract §2.5).
 *
 * The ring is asked for the way `rime remote ring` asks: a control-socket
 * request, forwarded by the broker. Nothing on the computer the suite runs on
 * is touched; the sound is this phone's.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class RingOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val manager: NotificationManager get() = context.getSystemService(NotificationManager::class.java)
    private lateinit var machine: PairedMachine

    @Before
    fun pairAndHold() {
        instrumentation.uiAutomation.grantRuntimePermission(context.packageName, Notifier.PERMISSION)
        Ringer(context).stop()
        machine = Paired.machine("ring-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        // `hold`, as the app does once it is unlocked: it starts LinkService,
        // which is what hears the battery change.
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        for (f in listOf(Phone.EVENTS, Phone.REPORT, Features.MUX_ATTACH)) {
            assertTrue("rime-remoted must offer $f: ${remote.features}", remote.has(f))
        }
    }

    @After
    fun tidy() {
        Ringer(context).stop()
        shell("dumpsys battery reset")
        hub.lock()
    }

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(50)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what")
    }

    private fun shell(command: String): String {
        val fd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(fd).bufferedReader().use { it.readText() }
    }

    /** What the computer last heard from this phone, or null. */
    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun ringing(): android.service.notification.StatusBarNotification? =
        manager.activeNotifications.firstOrNull { it.id == Ringer.ID }

    @Test
    fun the_phone_tells_the_computer_its_battery_and_that_it_can_ring() {
        val r = awaitValue(10_000, "the phone's report on the computer") { report() }
        val battery = r.optInt("battery", -1)
        assertTrue("the battery the computer holds is not a level: $r", battery in 0..100)
        assertEquals("the phone must say it can ring: $r", "ring", r.getJSONArray("can").getString(0))
    }

    @Test
    fun a_battery_change_reaches_the_computer_without_a_reconnect() {
        awaitValue(10_000, "the first report") { report() }
        val hellos = hub.state(machine.deviceId)!!.hellos
        shell("dumpsys battery unplug")
        shell("dumpsys battery set level 42")
        val r = awaitValue(10_000, "the computer to hear battery 42") { report()?.takeIf { it.optInt("battery") == 42 } }
        assertFalse("unplugged, the phone said it was charging: $r", r.optBoolean("charging", true))
        assertEquals("the report came by reconnecting, not by the battery receiver", hellos, hub.state(machine.deviceId)!!.hellos)
    }

    @Test
    fun rime_remote_ring_rings_this_phone_as_an_alarm_over_the_link_and_stop_silences_it() {
        awaitValue(10_000, "the phone's report") { report() }
        // The event channel opens right after the report; give the machine a
        // moment to have it before asking.
        val reply = awaitValue(10_000, "a ring over the link") {
            Desktop.ask("""{"cmd":"ring","device":"${machine.deviceId}"}""").takeIf { it.optString("path") == "link" }
        }
        assertEquals("rang", reply.getString("reply"))

        val posted = awaitValue(5_000, "the ring in the shade") { ringing() }
        val n = posted.notification
        assertEquals(Ringer.CHANNEL, n.channelId)
        assertEquals(Notification.CATEGORY_ALARM, n.category)
        assertTrue("the ring plays once, not until stopped", n.flags and Notification.FLAG_INSISTENT != 0)
        assertEquals(Ringer.RING_MS, n.timeoutAfter)
        assertEquals("Ringing", n.extras.getCharSequence(Notification.EXTRA_TITLE)?.toString())
        val channel = manager.getNotificationChannel(Ringer.CHANNEL)
        assertNotNull("the ring channel was never made", channel)
        assertEquals(NotificationManager.IMPORTANCE_HIGH, channel.importance)
        assertEquals("the ring does not play as an alarm", AudioAttributes.USAGE_ALARM, channel.audioAttributes.usage)
        assertNotNull("the ring channel has no sound", channel.sound)

        val stop = n.actions.single { it.title.toString() == "Stop" }
        stop.actionIntent.send()
        awaitValue(5_000, "Stop to take the ring away") { if (ringing() == null) true else null }
    }

    @Test
    fun a_ring_by_push_rings_once_and_a_late_one_not_at_all() {
        val ringer = Ringer(context)
        val now = System.currentTimeMillis() / 1000
        assertFalse("a push two minutes late rang", ringer.ringFromPush(machine.deviceId, machine.machine, now - 120, now))
        Thread.sleep(500)
        assertEquals("a late push left a ring in the shade", null, ringing())

        assertTrue("a fresh push did not ring", ringer.ringFromPush(machine.deviceId, machine.machine, now, now))
        awaitValue(5_000, "the pushed ring in the shade") { ringing() }
        ringer.stop()
        awaitValue(5_000, "the ring to stop") { if (ringing() == null) true else null }

        assertFalse(
            "the same ring arriving again by push rang twice",
            ringer.ringFromPush(machine.deviceId, machine.machine, now, now),
        )
    }

    /** Whether Android is playing this app's ring sound now, by its own record. */
    private fun soundPlaying(): Boolean =
        shell("dumpsys notification").lineSequence()
            .firstOrNull { it.trim().startsWith("mSoundNotificationKey=") }
            ?.contains(context.packageName) == true

    @Test
    fun the_ring_sounds_in_silent_mode_and_through_do_not_disturb_except_total_silence() {
        val ringer = Ringer(context)
        val results = linkedMapOf<String, Boolean>()
        val modes = listOf(
            "normal" to listOf("cmd audio set-ringer-mode NORMAL", "cmd notification set_dnd off"),
            "silent" to listOf("cmd audio set-ringer-mode SILENT", "cmd notification set_dnd off"),
            "vibrate" to listOf("cmd audio set-ringer-mode VIBRATE", "cmd notification set_dnd off"),
            "dnd-priority" to listOf("cmd audio set-ringer-mode NORMAL", "cmd notification set_dnd priority"),
            "dnd-alarms" to listOf("cmd audio set-ringer-mode NORMAL", "cmd notification set_dnd alarms"),
            "dnd-total" to listOf("cmd audio set-ringer-mode NORMAL", "cmd notification set_dnd none"),
        )
        try {
            for ((name, commands) in modes) {
                commands.forEach { shell(it) }
                Thread.sleep(500)
                val now = System.currentTimeMillis() / 1000 + results.size
                assertTrue("$name: the ring was not posted", ringer.ringFromPush(machine.deviceId, machine.machine, now, now))
                awaitValue(5_000, "$name: the ring in the shade") { ringing() }
                Thread.sleep(700)
                results[name] = soundPlaying()
                ringer.stop()
                awaitValue(5_000, "$name: the ring to stop") { if (ringing() == null) true else null }
            }
        } finally {
            shell("cmd notification set_dnd off")
            shell("cmd audio set-ringer-mode NORMAL")
        }
        // MEASURED on the emulator (Android 16), and what docs/remote.md
        // promises: as an alarm, the ring sounds in silent and vibrate mode and
        // through Do Not Disturb unless DND is set to total silence. "Sounds"
        // is Android's own record that it started the sound; the alarm volume
        // is the person's, and at zero it is quiet.
        assertEquals(
            mapOf(
                "normal" to true, "silent" to true, "vibrate" to true,
                "dnd-priority" to true, "dnd-alarms" to true, "dnd-total" to false,
            ),
            results,
        )
    }
}
