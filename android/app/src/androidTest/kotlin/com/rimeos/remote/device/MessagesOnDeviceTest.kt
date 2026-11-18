package com.rimeos.remote.device

import android.content.Context
import android.os.Build
import android.os.ParcelFileDescriptor
import android.provider.Telephony
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Messages
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
 * This phone's SMS on the computer (contract §2.11), on an EMULATOR, against
 * the runner's rime-remoted: an SMS injected through the emulator's console
 * shows in `rime remote messages`, and one sent from the computer lands in
 * this phone's sent box.
 *
 * Emulator only, and asserted before anything is read or sent: the runner
 * leaves this class out on any other serial, the broker refuses to inject on
 * one, and the first thing [setUp] does is check this is an emulator. Every
 * number is an emulator number (5554, 5556, and the emulator's own
 * 15555215556) and every text is made up here.
 *
 * The SMS permissions are granted with `pm grant`, as the person would in the
 * prompt, and left granted: revoking one kills this process mid-run.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class MessagesOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private lateinit var machine: PairedMachine

    @Before
    fun setUp() {
        assertTrue(
            "the SMS tests run only on an emulator (hardware ${Build.HARDWARE})",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish" || shell("getprop ro.kernel.qemu").trim() == "1",
        )
        for (p in listOf("READ_SMS", "SEND_SMS")) shell("pm grant ${context.packageName} android.permission.$p")
        setMessages(false)
        machine = Paired.machine("sms-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Messages.FEATURE}: ${remote.features}", remote.has(Messages.FEATURE))
    }

    @After
    fun tidy() {
        hub.lock()
        setMessages(false)
    }

    @Test
    fun an_sms_arriving_on_the_phone_is_listed_and_read_on_the_computer() {
        val first = awaitValue(15_000, "the first report") { report() }
        assertFalse("the report said messages with the switch off: $first", can(first).contains(Phone.CAN_MESSAGES))
        turnOn()

        val word = "arrived ${nonce()}"
        Desktop.emuSms("5556", word)
        // The emulator gives the sender its own form of the number.
        val thread = awaitValue(20_000, "the computer to list the conversation with the new SMS") {
            val reply = Desktop.messages(machine.deviceId)
            check(reply.optString("reply") == "messages") { "rime remote messages failed: $reply" }
            objects(reply, "threads").firstOrNull { it.optString("snippet") == word }
        }
        assertTrue("the thread has the sender's number: $thread", strings(thread, "addresses").any { it.endsWith("5556") })
        assertTrue("the new SMS is unread: $thread", thread.getInt("unread") >= 1)

        val reply = Desktop.messages(machine.deviceId, thread = thread.getLong("id"))
        assertEquals("messages", reply.optString("reply"))
        val newest = objects(reply, "messages").first()
        assertEquals(word, newest.getString("body"))
        assertFalse(newest.getBoolean("from_me"))
        assertEquals("received", newest.getString("status"))

        // The phone told the computer its store changed, and nothing else.
        awaitValue(15_000, "the computer to hear the store changed") {
            Desktop.messages(machine.deviceId).optLong("changed_ms").takeIf { it > 0 }
        }
    }

    @Test
    fun the_computer_sends_an_sms_and_it_is_in_the_phones_sent_box() {
        turnOn()
        val word = "sent ${nonce()}"
        val reply = Desktop.messagesSend(machine.deviceId, "5556", word)
        assertEquals("the send was not confirmed: $reply", "message_sent", reply.optString("reply"))
        val row = awaitValue(15_000, "the SMS in the phone's sent box") { sentBox(word) }
        assertTrue("sent to another number: $row", row.endsWith("5556"))
        // And the computer reads it back as its own.
        val thread = awaitValue(15_000, "the sent SMS in the computer's list") {
            objects(Desktop.messages(machine.deviceId), "threads").firstOrNull { it.optString("snippet") == word }
        }
        val mine = objects(Desktop.messages(machine.deviceId, thread = thread.getLong("id")), "messages").first()
        assertTrue(mine.getBoolean("from_me"))
        assertTrue("a sent SMS is sent or delivered: $mine", mine.getString("status") in setOf("sent", "delivered", "sending"))
    }

    @Test
    fun with_the_switch_off_the_computer_reads_nothing_and_sends_nothing() {
        turnOn()
        setMessages(false)
        awaitValue(15_000, "the computer to learn Messages went off") {
            report()?.takeIf { !can(it).contains(Phone.CAN_MESSAGES) }
        }
        val read = Desktop.messages(machine.deviceId)
        assertEquals("a phone with the switch off was read: $read", "error", read.optString("reply"))
        assertTrue(read.toString(), read.optJSONArray("threads") == null)

        val word = "refused ${nonce()}"
        val send = Desktop.messagesSend(machine.deviceId, "5556", word)
        assertEquals("a phone with the switch off sent: $send", "error", send.optString("reply"))
        Thread.sleep(2_000)
        assertEquals("an SMS went out with the switch off", null, sentBox(word))
    }

    // ---- helpers --------------------------------------------------------

    private fun turnOn() {
        setMessages(true)
        awaitValue(15_000, "the computer to learn it may ask for messages") {
            report()?.takeIf { can(it).contains(Phone.CAN_MESSAGES) }
        }
    }

    /** "Messages on your computer", turned on or off as the screen does. */
    private fun setMessages(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(messagesOn = on)) }
        hub.setMessagesOn(on)
    }

    /** The address of the sent-box row with exactly [body], or null. */
    private fun sentBox(body: String): String? =
        context.contentResolver.query(
            Telephony.Sms.Sent.CONTENT_URI,
            arrayOf(Telephony.Sms.ADDRESS),
            "${Telephony.Sms.BODY} = ?",
            arrayOf(body),
            null,
        )?.use { if (it.moveToFirst()) it.getString(0) ?: "" else null }

    private fun nonce(): String = (System.nanoTime() % 1_000_000_000).toString()

    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun can(report: JSONObject): List<String> = strings(report, "can")

    private fun strings(o: JSONObject, key: String): List<String> =
        o.optJSONArray(key)?.let { a -> (0 until a.length()).map { a.optString(it) } } ?: emptyList()

    private fun objects(o: JSONObject, key: String): List<JSONObject> =
        o.optJSONArray(key)?.let { a -> (0 until a.length()).map { a.getJSONObject(it) } } ?: emptyList()

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(250)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what")
    }

    private fun shell(command: String): String {
        val fd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(fd).bufferedReader().use { it.readText() }
    }
}
