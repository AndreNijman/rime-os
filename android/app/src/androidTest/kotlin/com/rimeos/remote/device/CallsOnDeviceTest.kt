package com.rimeos.remote.device

import android.content.Context
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Calls
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.PhoneCalls
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
 * This phone's calls on the computer (contract §2.12), on an EMULATOR,
 * against the runner's rime-remoted: a call rung through the emulator's
 * console shows as a notification on the computer, is answered from its
 * button, ended, declined and dialled from `rime remote calls`, and lands in
 * the history the computer reads.
 *
 * Emulator only, and asserted before anything rings: the runner leaves this
 * class out on any other serial, the broker refuses `emu_gsm` on one, and
 * the first thing [setUp] does is check this is an emulator. Every number is
 * an emulator number (5556).
 *
 * The call permissions are granted with `pm grant`, as the person would in
 * the prompt, and left granted: revoking one kills this process mid-run.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class CallsOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private lateinit var machine: PairedMachine

    @Before
    fun setUp() {
        assertTrue(
            "the call tests run only on an emulator (hardware ${Build.HARDWARE})",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish" || shell("getprop ro.kernel.qemu").trim() == "1",
        )
        for (p in listOf("READ_PHONE_STATE", "READ_CALL_LOG", "ANSWER_PHONE_CALLS", "CALL_PHONE")) {
            shell("pm grant ${context.packageName} android.permission.$p")
        }
        endAnyCall()
        setCalls(false)
        machine = Paired.machine("calls-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Calls.FEATURE}: ${remote.features}", remote.has(Calls.FEATURE))
    }

    @After
    fun tidy() {
        endAnyCall()
        hub.lock()
        setCalls(false)
    }

    @Test
    fun a_ringing_call_shows_on_the_computer_is_answered_from_its_button_and_hung_up_from_the_cli() {
        val first = awaitValue(15_000, "the first report") { report() }
        assertFalse("the report said calls with the switch off: $first", can(first).contains(Phone.CAN_CALLS))
        turnOn()
        val before = notifyLog().size

        Desktop.emuGsm("call", "5556")
        val shown = awaitValue(20_000, "the computer to show the ringing call") {
            notifyLog().drop(before).firstOrNull { it.optString("op") == "notify" && it.optString("summary").startsWith("Call from") }
        }
        assertTrue("the notification names the caller: $shown", shown.getString("summary").endsWith("5556"))
        assertTrue("the notification says it is a call: $shown", shown.getString("app").endsWith("· Calls"))
        assertEquals("[[\"answer\",\"Answer\"],[\"decline\",\"Decline\"]]", shown.getJSONArray("actions").toString())
        assertTrue("a ringing call stays until it stops ringing: $shown", shown.optBoolean("sticky"))
        val ringing = Desktop.calls(machine.deviceId)
        assertEquals("rime remote calls failed: $ringing", "calls", ringing.optString("reply"))
        assertEquals("ringing", ringing.getJSONObject("current").getString("state"))

        // The person presses Answer on the computer's notification.
        val id = shown.getInt("id")
        Desktop.notifyAction(id, "answer")
        awaitValue(15_000, "the phone to answer") { phoneState().takeIf { it == Calls.ACTIVE } }
        awaitValue(15_000, "the computer to close the ringing notification") {
            notifyLog().firstOrNull { it.optString("op") == "close" && it.optInt("id") == id }
        }
        awaitValue(15_000, "the computer to see the call active") {
            Desktop.calls(machine.deviceId).optJSONObject("current")?.takeIf { it.optString("state") == "active" }
        }

        val hangup = Desktop.call(machine.deviceId, "hangup")
        assertEquals("the hang-up was not confirmed: $hangup", "call_done", hangup.optString("reply"))
        awaitValue(15_000, "the phone to end the call") { phoneState().takeIf { it == Calls.IDLE } }
        val history = awaitValue(15_000, "the answered call in the computer's history") {
            Desktop.calls(machine.deviceId).takeIf { it.optJSONObject("current") == null }
                ?.let { objects(it, "calls").firstOrNull() }
                ?.takeIf { it.optString("type") == "incoming" }
        }
        assertTrue("the history has the caller: $history", history.getString("number").endsWith("5556"))
    }

    @Test
    fun a_call_is_declined_from_the_computer_and_a_number_dialled_from_it() {
        turnOn()
        Desktop.emuGsm("call", "5556")
        awaitValue(20_000, "the computer to see the call ring") {
            Desktop.calls(machine.deviceId).optJSONObject("current")?.takeIf { it.optString("state") == "ringing" }
        }
        val decline = Desktop.call(machine.deviceId, "decline")
        assertEquals("the decline was not confirmed: $decline", "call_done", decline.optString("reply"))
        awaitValue(15_000, "the phone to stop ringing") { phoneState().takeIf { it == Calls.IDLE } }
        val declined = awaitValue(15_000, "the declined call in the history") {
            objects(Desktop.calls(machine.deviceId), "calls").firstOrNull()?.takeIf { it.optString("type") != "outgoing" }
        }
        assertTrue("a declined call reads as rejected or missed: $declined", declined.getString("type") in setOf("rejected", "missed"))

        // Dial from the computer: the phone places the call itself.
        val dial = Desktop.call(machine.deviceId, "dial", "5556")
        assertEquals("the dial was not confirmed: $dial", "call_done", dial.optString("reply"))
        awaitValue(15_000, "the phone to place the call") { phoneState().takeIf { it == Calls.ACTIVE } }
        awaitValue(15_000, "the computer to see the call it placed") {
            Desktop.calls(machine.deviceId).optJSONObject("current")?.takeIf { it.optString("state") == "active" }
        }
        // A second dial while that call goes is refused, by the computer first.
        val again = Desktop.call(machine.deviceId, "dial", "5556")
        assertEquals("a dial during a call went through: $again", "error", again.optString("reply"))

        assertEquals("call_done", Desktop.call(machine.deviceId, "hangup").optString("reply"))
        awaitValue(15_000, "the phone to end the placed call") { phoneState().takeIf { it == Calls.IDLE } }
        val placed = awaitValue(15_000, "the placed call in the history") {
            objects(Desktop.calls(machine.deviceId), "calls").firstOrNull()?.takeIf { it.optString("type") == "outgoing" }
        }
        assertTrue("the placed call went to the number asked: $placed", placed.getString("number").endsWith("5556"))
    }

    @Test
    fun with_the_switch_off_nothing_rings_on_the_computer_and_nothing_is_done() {
        turnOn()
        setCalls(false)
        awaitValue(15_000, "the computer to learn Calls went off") {
            report()?.takeIf { !can(it).contains(Phone.CAN_CALLS) }
        }
        val before = notifyLog().size
        Desktop.emuGsm("call", "5556")
        awaitValue(15_000, "the phone to ring") { phoneState().takeIf { it == Calls.RINGING } }
        Thread.sleep(3_000)
        val shown = notifyLog().drop(before).filter { it.optString("summary").startsWith("Call from") || it.optString("summary") == "Incoming call" }
        assertEquals("a call showed on the computer with the switch off: $shown", 0, shown.size)

        val answer = Desktop.call(machine.deviceId, "answer")
        assertEquals("a phone with the switch off took an answer: $answer", "error", answer.optString("reply"))
        val read = Desktop.calls(machine.deviceId)
        assertEquals("a phone with the switch off was read: $read", "error", read.optString("reply"))
        Thread.sleep(1_000)
        assertEquals("the call was answered with the switch off", Calls.RINGING, phoneState())
        Desktop.emuGsm("cancel", "5556")
        awaitValue(15_000, "the call to end") { phoneState().takeIf { it == Calls.IDLE } }
    }

    // ---- helpers --------------------------------------------------------

    private fun turnOn() {
        setCalls(true)
        val r = awaitValue(15_000, "the computer to learn it may show calls") {
            report()?.takeIf { can(it).contains(Phone.CAN_CALLS) }
        }
        for (w in listOf(Phone.CAN_CALLS_HISTORY, Phone.CAN_CALLS_ANSWER, Phone.CAN_CALLS_DIAL)) {
            assertTrue("the report lacks $w with everything granted: $r", can(r).contains(w))
        }
    }

    /** "Calls on your computer", turned on or off as the screen does. */
    private fun setCalls(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(callsOn = on)) }
        hub.setCallsOn(on)
    }

    /** End whatever call a failed test left, from the far end, then here. */
    private fun endAnyCall() {
        // The far end always hangs up, even with the phone idle: a call the
        // emulator's modem dropped as it was answered stays in the modem,
        // which then ignores the next `gsm call` from that number until a
        // `gsm cancel` (measured: the next test's call never rings).
        runCatching { Desktop.emuGsm("cancel", "5556") }
        if (phoneState() == Calls.IDLE) return
        val end = System.currentTimeMillis() + 5_000
        while (phoneState() != Calls.IDLE && System.currentTimeMillis() < end) Thread.sleep(250)
        if (phoneState() != Calls.IDLE) PhoneCalls.act(context, Calls.HANGUP, null)
    }

    private fun phoneState(): String = Calls.stateOf(PhoneCalls.telephonyState(context))

    private fun notifyLog(): List<JSONObject> = Desktop.notifyLog()

    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun can(report: JSONObject): List<String> =
        report.optJSONArray("can")?.let { a -> (0 until a.length()).map { a.optString(it) } } ?: emptyList()

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
