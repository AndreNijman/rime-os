package com.rimeos.remote.device

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
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
 * The computer's clipboard on this phone (contract §2.14), on a phone,
 * against the runner's rime-remoted.
 *
 * The computer side is the real daemon: `rime remote clipboard` as the
 * forwarded `clipboard` request (what the Shell runs on each copy), the
 * phone's report through the forwarded `devices`, and the phone's own
 * `clipboard_write` landing in the stub desktop's clipboard file. The
 * desktop's real clipboard is never touched. The phone's clipboard is read
 * from an activity of this app in front: Android 10 and later let only the
 * focused app read it.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class ClipboardOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private lateinit var machine: PairedMachine

    @Before
    fun pair() {
        setClipboard(false)
        machine = Paired.machine("clip-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Phone.CLIPBOARD}: ${remote.features}", remote.has(Phone.CLIPBOARD))
        awaitValue(15_000, "the first report") { report() }
    }

    @After
    fun tidy() {
        hub.lock()
        setClipboard(false)
    }

    @Test
    fun with_the_switch_on_the_computers_clipboard_lands_here_verbatim_and_sensitive_is_marked() {
        switchOnAndWait()
        val text = "  copied at the computer ${System.nanoTime()}\nsecond line  "
        assertEquals("the clipboard line reached no phone", 1, Desktop.clipboard(text))
        awaitValue(15_000, "the computer's text on this phone's clipboard") { clip()?.takeIf { it.first == text } }

        val secret = "pw-${System.nanoTime()}"
        assertEquals(1, Desktop.clipboard(secret, sensitive = true))
        val (got, sensitive) = awaitValue(15_000, "the sensitive text on this phone's clipboard") { clip()?.takeIf { it.first == secret } }
        assertEquals(secret, got)
        assertTrue("a sensitive clip was not marked sensitive", sensitive)
        // A plain one after it is not.
        val plain = "plain-${System.nanoTime()}"
        assertEquals(1, Desktop.clipboard(plain))
        assertFalse(awaitValue(15_000, "the plain text") { clip()?.takeIf { it.first == plain } }.second)
    }

    @Test
    fun with_the_switch_off_the_computer_sends_nothing_and_the_clipboard_stays() {
        // On, then off: the machine heard both, so it must drop this phone.
        switchOnAndWait()
        setClipboard(false)
        awaitValue(15_000, "the computer to learn this phone no longer wants its clipboard") {
            report()?.takeIf { !can(it).contains(Phone.CAN_CLIPBOARD) }
        }
        val mine = "mine-${System.nanoTime()}"
        writeClip(mine)
        assertEquals("a phone with the switch off was sent the clipboard", 0, Desktop.clipboard("not for you ${System.nanoTime()}"))
        Thread.sleep(2_000)
        assertEquals(mine, clip()?.first)
    }

    @Test
    fun text_this_phone_put_on_the_computer_is_not_sent_back_to_it() {
        switchOnAndWait()
        val mine = "mine-${System.nanoTime()}"
        writeClip(mine)
        // The phone sets the computer's clipboard, as Send does.
        val sent = "from the phone ${System.nanoTime()}"
        hub.link(machine).clipboardWrite(sent)
        assertEquals(sent, Desktop.deskClipboard())
        // The Shell sees the change and sends it on: not back to this phone.
        assertEquals("the phone's own text came straight back", 0, Desktop.clipboard(sent))
        Thread.sleep(2_000)
        assertEquals(mine, clip()?.first)
        // Anything else still comes.
        val other = "copied after ${System.nanoTime()}"
        assertEquals(1, Desktop.clipboard(other))
        awaitValue(15_000, "the next copy on this phone's clipboard") { clip()?.takeIf { it.first == other } }
    }

    // ---- helpers --------------------------------------------------------

    private fun switchOnAndWait() {
        setClipboard(true)
        awaitValue(15_000, "the computer to learn this phone wants its clipboard") {
            report()?.takeIf { can(it).contains(Phone.CAN_CLIPBOARD) }
        }
    }

    /** "Get the computer's clipboard", turned on or off as the screen does. */
    private fun setClipboard(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(clipboardOn = on)) }
        hub.setClipboardOn(on)
    }

    /** The clipboard's text and whether it is marked sensitive, read with this app in front. */
    private fun clip(): Pair<String, Boolean>? {
        var out: Pair<String, Boolean>? = null
        ActivityScenario.launch(ComposeHostActivity::class.java).use { scenario ->
            val end = System.currentTimeMillis() + 5_000
            while (out == null && System.currentTimeMillis() < end) {
                scenario.onActivity { a ->
                    val c = a.getSystemService(ClipboardManager::class.java)?.primaryClip?.takeIf { it.itemCount > 0 }
                    val text = c?.getItemAt(0)?.text?.toString()
                    if (text != null) {
                        out = text to (c.description.extras?.getBoolean("android.content.extra.IS_SENSITIVE") == true)
                    }
                }
                if (out == null) Thread.sleep(100)
            }
        }
        return out
    }

    private fun writeClip(text: String) {
        context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("test", text))
        awaitValue(5_000, "the test's own text on the clipboard") { clip()?.takeIf { it.first == text } }
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
}
