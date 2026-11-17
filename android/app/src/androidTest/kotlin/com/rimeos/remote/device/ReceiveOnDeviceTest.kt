package com.rimeos.remote.device

import android.app.Notification
import android.app.NotificationManager
import android.content.ClipboardManager
import android.content.ContentUris
import android.content.Context
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import android.util.Base64
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.Receiver
import com.rimeos.remote.ui.Notifier
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * `rime remote send`, received on a phone, against the runner's rime-remoted
 * (contract §2.6).
 *
 * The sends are made the way the CLI makes them: a control-socket request
 * from the computer, forwarded by the broker; a file goes with its descriptor
 * passed. What lands is this phone's: its clipboard and its Downloads. Every
 * file this test saves is named for the run and deleted after.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class ReceiveOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val manager: NotificationManager get() = context.getSystemService(NotificationManager::class.java)
    private lateinit var machine: PairedMachine
    private val nonce = System.nanoTime() % 1_000_000
    private val saved = mutableListOf<Uri>()

    @Before
    fun pairAndHold() {
        instrumentation.uiAutomation.grantRuntimePermission(context.packageName, Notifier.PERMISSION)
        machine = Paired.machine("recv-$nonce")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        for (f in listOf(Phone.EVENTS, Phone.REPORT, Phone.TAKE, Features.MUX_ATTACH)) {
            assertTrue("rime-remoted must offer $f: ${remote.features}", remote.has(f))
        }
        awaitValue(10_000, "the phone to say it receives") {
            report()?.takeIf { r -> (0 until r.getJSONArray("can").length()).any { r.getJSONArray("can").getString(it) == Phone.CAN_RECEIVE } }
        }
    }

    @After
    fun tidy() {
        saved.forEach { runCatching { context.contentResolver.delete(it, null, null) } }
        manager.cancelAll()
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

    /** What the app does on unlock: the key back from the keystore, then hold. `lock` forgot it. */
    private fun unlockAgain() {
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
    }

    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun sendText(text: String): JSONObject =
        Desktop.ask(JSONObject().put("cmd", "send").put("device", machine.deviceId).put("kind", "text").put("text", text).toString())

    private fun sendFile(name: String, bytes: ByteArray): JSONObject =
        Desktop.ask(
            JSONObject().put("cmd", "send_file_at_computer").put("device", machine.deviceId).put("name", name)
                .put("data", Base64.encodeToString(bytes, Base64.NO_WRAP)).toString(),
        )

    private fun posted(title: String): Notification? =
        manager.activeNotifications.firstOrNull {
            it.notification.channelId == Receiver.CHANNEL &&
                it.notification.extras.getCharSequence(Notification.EXTRA_TITLE)?.toString() == title
        }?.notification

    /**
     * The clipboard, read from an activity of this app in front: Android 10
     * and later let only the focused app read it. Writing it, which the app
     * did with no activity at all, has no such rule; that is what this proves.
     */
    private fun clipboardText(): String? {
        var text: String? = null
        ActivityScenario.launch(ComposeHostActivity::class.java).use { scenario ->
            val end = System.currentTimeMillis() + 5_000
            while (text == null && System.currentTimeMillis() < end) {
                scenario.onActivity { a ->
                    text = a.getSystemService(ClipboardManager::class.java)?.primaryClip
                        ?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString()
                }
                if (text == null) Thread.sleep(100)
            }
        }
        return text
    }

    /** This app's own Downloads entries named [name] (MediaStore), newest last. */
    private fun downloads(prefix: String): List<Pair<Uri, String>> {
        val out = mutableListOf<Pair<Uri, String>>()
        context.contentResolver.query(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            arrayOf(MediaStore.Downloads._ID, MediaStore.Downloads.DISPLAY_NAME),
            "${MediaStore.Downloads.DISPLAY_NAME} LIKE ?",
            arrayOf("$prefix%"),
            "${MediaStore.Downloads._ID} ASC",
        )?.use { c ->
            while (c.moveToNext()) {
                out += ContentUris.withAppendedId(MediaStore.Downloads.EXTERNAL_CONTENT_URI, c.getLong(0)) to c.getString(1)
            }
        }
        return out
    }

    @Test
    fun text_from_the_computer_lands_on_the_clipboard_and_the_notification_does_not_show_it() {
        val text = "rime send test $nonce: door code 4417"
        val reply = awaitValue(10_000, "a send over the link") { sendText(text).takeIf { it.optString("path") == "link" } }
        assertEquals("sent", reply.getString("reply"))
        val n = awaitValue(10_000, "the text notification") { posted("Text from ${machine.machine}") }
        val shown = listOf(Notification.EXTRA_TITLE, Notification.EXTRA_TEXT, Notification.EXTRA_BIG_TEXT)
            .joinToString(" ") { n.extras.getCharSequence(it)?.toString() ?: "" }
        assertFalse("the notification showed the text: $shown", shown.contains("4417"))
        assertEquals(Notification.VISIBILITY_PRIVATE, n.visibility)
        assertEquals(text, clipboardText())
    }

    @Test
    fun a_file_from_the_computer_lands_in_downloads_whole_and_a_second_with_its_name_replaces_nothing() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return // MediaStore Downloads; Android 9 saves to the app's own folder
        val name = "rime-send-$nonce.bin"
        val first = ByteArray(300_000) { (it % 241).toByte() }
        val r1 = awaitValue(10_000, "a file send over the link") { sendFile(name, first).takeIf { it.optString("reply") == "sent" } }
        assertEquals("link", r1.getString("path"))
        awaitValue(15_000, "the file's notification") { posted(name) }
        val one = awaitValue(5_000, "the file in Downloads") { downloads("rime-send-$nonce").takeIf { it.size == 1 } }
        saved += one.map { it.first }
        assertEquals(name, one[0].second)
        assertArrayEquals(first, context.contentResolver.openInputStream(one[0].first)!!.use { it.readBytes() })

        val second = ByteArray(1000) { 9 }
        assertEquals("link", sendFile(name, second).getString("path"))
        val two = awaitValue(15_000, "the second file in Downloads") { downloads("rime-send-$nonce").takeIf { it.size == 2 } }
        saved += two.map { it.first }.filter { it !in saved }
        assertTrue("the second file took the first one's name: $two", two[1].second != name)
        assertArrayEquals("the first file was changed", first, context.contentResolver.openInputStream(two[0].first)!!.use { it.readBytes() })
        assertArrayEquals(second, context.contentResolver.openInputStream(two[1].first)!!.use { it.readBytes() })
        // The open action points at the saved file.
        val open = posted(name)!!.contentIntent
        assertTrue("the notification opens nothing", open != null)
    }

    @Test
    fun text_sent_while_the_app_is_locked_waits_and_is_taken_when_it_unlocks() {
        hub.lock()
        awaitValue(10_000, "the link to drop") { if (hub.state(machine.deviceId)?.status != LinkHub.Status.CONNECTED) true else null }
        val text = "rime waiting test $nonce"
        val reply = sendText(text)
        assertEquals("sent", reply.getString("reply"))
        assertEquals("nothing connected and no push: it waits", "waiting", reply.getString("path"))
        Thread.sleep(500)
        assertTrue("taken while locked", posted("Text from ${machine.machine}") == null)

        unlockAgain()
        awaitValue(20_000, "the waiting text to be taken on unlock") { posted("Text from ${machine.machine}") }
        assertEquals(text, clipboardText())
        // Done: opening the channel again offers nothing.
        manager.cancelAll()
        hub.lock()
        awaitValue(10_000, "the link to drop") { if (hub.state(machine.deviceId)?.status != LinkHub.Status.CONNECTED) true else null }
        unlockAgain()
        awaitValue(15_000, "the phone's report after reconnecting") { report() }
        Thread.sleep(2_000)
        assertTrue("an offer taken and done was taken again", posted("Text from ${machine.machine}") == null)
    }
}
