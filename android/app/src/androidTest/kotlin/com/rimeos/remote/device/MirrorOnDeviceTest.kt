package com.rimeos.remote.device

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.MirrorService
import com.rimeos.remote.ui.Notifier
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
 * This phone's notifications on the computer (contract §2.7), on a phone,
 * against the runner's rime-remoted.
 *
 * The notifications are posted by `cmd notification post`, so they come from
 * `com.android.shell`: another app, as far as the listener can tell. The
 * computer side is rime-remoted's stub notification server
 * (`RIME_REMOTED_NOTIFY_STUB`, read through the broker's `notify_log`); the
 * notifications of the computer the suite runs on are never touched.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class MirrorOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val listener = MirrorService.component(context).flattenToString()
    private val tags = mutableListOf<String>()
    private lateinit var machine: PairedMachine
    private lateinit var deviceName: String
    /** How many calls the stub had seen before this test: only later ones are this test's. */
    private var before = 0

    @Before
    fun pairAndListen() {
        shell("cmd notification allow_listener $listener")
        awaitValue(15_000, "Android to bind the notification listener") { if (listenerBound()) true else null }
        assertTrue("notification access was granted but the app does not see it", MirrorService.granted(context))
        choose(setOf(SHELL))
        before = Desktop.notifyLog().size
        deviceName = "mirror-${System.nanoTime() % 100_000}"
        machine = Paired.machine(deviceName)
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Mirror.FEATURE}: ${remote.features}", remote.has(Mirror.FEATURE))
        awaitValue(10_000, "the hub to be ready to mirror") { if (hub.mirroring) true else null }
    }

    @After
    fun tidy() {
        hub.lock()
        assertFalse("locked, the hub still mirrors", hub.mirroring)
        // Shell notifications cannot be cancelled from a shell; snoozed for a
        // day they leave the shade, and the hub is locked when they return.
        for (tag in tags) keyOf(tag)?.let { shell("cmd notification snooze --for 86400000 $it") }
        context.getSystemService(NotificationManager::class.java).apply {
            cancel(OWN_ID)
            deleteNotificationChannel(OWN_CHANNEL)
        }
        choose(emptySet())
        shell("cmd notification disallow_listener $listener")
    }

    @Test
    fun a_chosen_apps_notification_shows_on_the_computer_updates_in_place_and_closes_when_it_goes() {
        val tag = tag("chat")
        post(tag, "Lunch", "Still-on-for-12?")
        val shown = awaitEntry("the notification on the computer") { it.optString("summary") == "Lunch" }
        val id = shown.getInt("id")
        assertEquals("a new notification replaced one: $shown", 0, shown.getInt("replaces"))
        assertEquals("Still-on-for-12?", shown.getString("body"))
        assertEquals("phone", shown.getString("icon"))
        // "Shell" when Android lets the listener read the label, the package
        // name when it does not; either way it says which phone it came from.
        val app = shown.getString("app")
        assertTrue("the app name does not name the phone: $app", app.startsWith("$deviceName · "))
        assertTrue("the app name names neither the app nor its package: $app", app.endsWith("Shell") || app.endsWith(SHELL))

        post(tag, "Lunch", "Running-late")
        val updated = awaitEntry("the update on the computer") { it.optString("body") == "Running-late" }
        assertEquals("an update made a second notification instead of replacing: $updated", id, updated.getInt("replaces"))

        val key = keyOf(tag) ?: error("Android lists no notification with tag $tag")
        shell("cmd notification snooze --for 86400000 $key")
        awaitEntry("the computer to close it") { it.optString("op") == "close" && it.getInt("id") == id }
    }

    @Test
    fun dismissed_on_the_computer_it_leaves_the_phone_and_timed_out_there_it_stays() {
        // Timed out on the computer (1): the phone keeps it.
        val stays = tag("stays")
        post(stays, "Stays", "timed-out-there")
        val kept = awaitEntry("the first notification on the computer") { it.optString("summary") == "Stays" }.getInt("id")
        Desktop.notifyClosed(kept, 1)

        // Dismissed on the computer (2): gone from the phone, as a swipe.
        val goes = tag("goes")
        post(goes, "Goes", "dismissed-there")
        val shown = awaitEntry("the second notification on the computer") { it.optString("summary") == "Goes" }.getInt("id")
        Desktop.notifyClosed(shown, 2)
        awaitValue(15_000, "the phone to cancel the dismissed notification") { if (keyOf(goes) == null) true else null }
        assertTrue("timed out on the computer, it left the phone too", keyOf(stays) != null)
        // The phone's own removal is not sent back to the computer that dismissed it.
        Thread.sleep(1_000)
        assertFalse(
            "the computer that dismissed it was sent a close for it",
            Desktop.notifyLog().drop(before).any { it.optString("op") == "close" && it.getInt("id") == shown },
        )
    }

    @Test
    fun an_app_not_chosen_and_this_app_itself_reach_nothing() {
        // This app chosen too, which the screen never offers: the listener
        // must still leave its own notifications on the phone.
        choose(setOf(context.packageName))
        post(tag("unchosen"), "Unchosen", "never-leaves-the-phone")
        val manager = context.getSystemService(NotificationManager::class.java)
        instrumentation.uiAutomation.grantRuntimePermission(context.packageName, Notifier.PERMISSION)
        manager.createNotificationChannel(NotificationChannel(OWN_CHANNEL, "Mirror test", NotificationManager.IMPORTANCE_DEFAULT))
        manager.notify(
            OWN_ID,
            Notification.Builder(context, OWN_CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle("Own")
                .setContentText("this app's own notification")
                .build(),
        )
        Thread.sleep(1_000)

        // A sentinel from a chosen app: the sender is serial and the listener
        // reports in order, so once it arrives anything earlier would have too.
        choose(setOf(SHELL))
        post(tag("sentinel"), "Sentinel", "after-the-others")
        awaitEntry("the sentinel") { it.optString("summary") == "Sentinel" }
        val mine = Desktop.notifyLog().drop(before).filter { it.optString("op") == "notify" }
        assertEquals(
            "something besides the sentinel reached the computer: $mine",
            listOf("Sentinel"),
            mine.map { it.optString("summary") },
        )
    }

    // ---- helpers --------------------------------------------------------

    /** The apps the person chose, written where the app keeps them and told to the hub. */
    private fun choose(apps: Set<String>) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mirrorApps = apps)) }
        hub.setMirrorApps(apps)
    }

    private fun tag(what: String): String = "rime-mirror-$what-${System.nanoTime() % 1_000_000}".also { tags += it }

    /**
     * `executeShellCommand` splits on whitespace and passes quotes through, so
     * [title] and [text] are single words here; spacing and markup are the
     * computer side's tests (rime-remoted's end_to_end).
     */
    private fun post(tag: String, title: String, text: String) {
        require(listOf(tag, title, text).none { it.any(Char::isWhitespace) })
        val out = shell("cmd notification post -t $title $tag $text")
        assertFalse("the shell did not post: $out", out.contains("Error", ignoreCase = true))
    }

    /** Android's key for the shell's notification with [tag], e.g. `0|com.android.shell|…|tag|2000`. */
    private fun keyOf(tag: String): String? =
        shell("cmd notification list").lineSequence().map { it.trim() }.firstOrNull { "|$SHELL|" in it && "|$tag|" in it }

    /** Whether Android lists this app's listener as bound now. */
    private fun listenerBound(): Boolean {
        val dump = shell("dumpsys notification").lines()
        val start = dump.indexOfFirst { it.trim().startsWith("Live notification listeners") }
        if (start < 0) return false
        return dump.drop(start + 1).takeWhile { it.startsWith("    ") || it.isBlank() }.any { "MirrorService" in it }
    }

    private fun awaitEntry(what: String, match: (JSONObject) -> Boolean): JSONObject =
        awaitValue(15_000, what) { Desktop.notifyLog().drop(before).firstOrNull(match) }

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
        const val OWN_CHANNEL = "rime-mirror-test"
        const val OWN_ID = 4_711
    }
}
