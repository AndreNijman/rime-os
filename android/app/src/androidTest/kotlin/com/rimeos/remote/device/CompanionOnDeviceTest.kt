package com.rimeos.remote.device

import android.content.Context
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.machine.Machine
import com.rimeos.remote.core.phone.CompanionKeys
import com.rimeos.remote.core.phone.CompanionLink
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.core.phone.Scope
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.MirrorService
import com.rimeos.remote.security.KeystoreSecretBox
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.security.KeyStore

/**
 * "Keep working after the phone restarts" (contract §2.10), on a phone,
 * against the runner's rime-remoted.
 *
 * ## What stands in for a restart
 *
 * Instrumentation runs inside the app's process, so the process cannot be
 * killed and the phone cannot be rebooted under the test. A restart is played
 * as [LinkHub.shutdown] (every link closed, every key out of memory, the
 * service stopped: the state of a process that has just started) followed by
 * a real `BOOT_COMPLETED` broadcast, sent as root (`su 0 am broadcast`; the
 * shell user may not send it) to this package, so Android delivers it to the
 * manifest's receiver. What this does NOT prove is that Android lets a
 * receiver start the foreground service after a real boot; that rests on
 * Android's documented exemption (see `BootReceiver`).
 *
 * The machine is paired through [Paired] and saved through the app's own
 * [MachineRepository], because the companion lifecycle reads and writes the
 * stored record. The companion key goes through the app's own keystore box.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class CompanionOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val listener = MirrorService.component(context).flattenToString()
    private val tags = mutableListOf<String>()
    private lateinit var machine: PairedMachine
    private var notifyBefore = 0

    @Before
    fun pairAndConnect() {
        shell("cmd notification allow_listener $listener")
        awaitValue(15_000, "Android to bind the notification listener") { if (MirrorService.granted(context)) true else null }
        machine = Paired.machine("companion-${System.nanoTime() % 100_000}")
        runBlocking { repository.remember(machine) }
        choose(setOf(SHELL))
        notifyBefore = Desktop.notifyLog().size
        unlockAndHold()
        val remote = hub.state(machine.deviceId)!!.remote!!
        assertTrue("rime-remoted must offer ${CompanionLink.FEATURE}: ${remote.features}", remote.has(CompanionLink.FEATURE))
        assertTrue("rime-remoted must offer ${Mirror.FEATURE}: ${remote.features}", remote.has(Mirror.FEATURE))
    }

    @After
    fun tidy() {
        // Off first, while the full link may still be up, so the computer is
        // told to forget; the record and both keystore keys go after.
        runCatching {
            val store = repository.loadBlocking()
            if (store.settings.companionOn || store.find(machine.deviceId)?.companion != null) hub.setCompanionOn(false)
        }
        hub.shutdown()
        assertFalse("after the test, companion mode still runs", hub.companion.value)
        shell("dumpsys battery reset")
        for (tag in tags) keyOf(tag)?.let { shell("cmd notification snooze --for 86400000 $it") }
        setReply(false)
        choose(emptySet())
        shell("cmd notification disallow_listener $listener")
        hub.forget(machine.deviceId)
        runBlocking { repository.forget(machine) }
    }

    @Test
    fun locked_and_after_a_restart_the_device_features_keep_reaching_the_computer_and_nothing_else_does() {
        // ---- the switch on: a companion key, and the computer says so ----
        setReply(true)
        hub.setCompanionOn(true)
        assertEquals("the switch on registered no key", 1, hub.companionKeys())
        assertTrue("the switch on made no keystore key", companionKeyInKeystore())
        awaitValue(10_000, "rime remote devices --json to show companion true") {
            if (listed()?.optBoolean("companion") == true) true else null
        }

        // ---- lock: companion mode, not silence ----
        hub.lock()
        assertFalse(hub.unlocked.value)
        assertTrue("locked with the switch on, companion mode did not start", hub.companion.value)
        awaitValue(15_000, "the companion link to connect after the lock") {
            hub.companionLinkOf(machine.deviceId)?.takeIf { it.connected }
        }

        // ---- the restart ----
        hub.shutdown()
        assertFalse("shutdown left companion mode running", hub.companion.value)
        assertNull(hub.companionLinkOf(machine.deviceId))
        assertNull("shutdown left the full key in memory", hub.identity(machine.deviceId))
        // A fresh process knows nothing it did not read: the chosen apps and
        // the reply setting are wiped from memory (the store keeps them), so
        // the notification below reaches the computer only if the boot start
        // read them back from the store.
        hub.setMirrorApps(emptySet())
        hub.setMirrorReply(false)
        val reportedBefore = report()?.optLong("reported_ms") ?: 0L
        val out = shell("su 0 am broadcast -a android.intent.action.BOOT_COMPLETED -p ${context.packageName}")
        assertTrue("the boot broadcast was not delivered: $out", "Broadcast completed" in out)
        awaitValue(15_000, "the boot receiver to start companion mode") { if (hub.companion.value) true else null }
        val link = awaitValue(20_000, "the companion link to connect after the restart") {
            hub.companionLinkOf(machine.deviceId)?.takeIf { it.connected }
        }
        assertFalse("the restart unlocked the app", hub.unlocked.value)
        assertEquals("the link after the restart is not the companion key's", Scope.COMPANION, link.scope)

        // ---- while locked: the phone's report and a notification reach the computer ----
        shell("dumpsys battery unplug")
        shell("dumpsys battery set level 37")
        val r = awaitValue(15_000, "the computer to hear battery 37 while locked") {
            report()?.takeIf { it.optInt("battery") == 37 && it.optLong("reported_ms") > reportedBefore }
        }
        // Replying from the computer is on, but a reply is never sent over
        // the companion key, so the locked phone does not offer it.
        val can = r.optJSONArray("can")
        assertNotNull("the report has no can: $r", can)
        assertFalse(
            "locked, the report says this phone takes replies: $r",
            (0 until can!!.length()).any { can.optString(it) == Phone.CAN_REPLY },
        )
        val tag = tag("locked")
        post(tag, "Locked", "still-reaches-the-computer")
        awaitValue(15_000, "the notification posted while locked to reach the computer") {
            Desktop.notifyLog().drop(notifyBefore).firstOrNull { it.optString("summary") == "Locked" }
        }

        // ---- control: the app never tried, and the computer refuses it ----
        assertEquals("companion mode tried a verb its scope does not allow", 0, link.refused)
        val actionsBefore = Desktop.machineStubActions().size
        val identity = CompanionKeys(repository.companionVault()).identity(repository.loadBlocking().find(machine.deviceId)!!)
        assertNotNull("no companion key opened with nobody present", identity)
        // Dialled as FULL and no hello, so this phone's own gate stays open
        // and the line reaches the computer: what a phone without the gate
        // would send with the companion key.
        val raw = MachineLink({ Paired.session(machine, identity!!) })
        try {
            // `request` hands back the reply line; reading it is what turns
            // the computer's error into the AgentError the app would see.
            val e = runCatching { Machine.readLock(raw.request(Machine.lock(), retry = false)) }.exceptionOrNull()
            assertTrue("the computer did not refuse a lock on a companion session: $e", e is AgentError && e.kind == CompanionLink.NOT_ALLOWED)
        } finally {
            raw.close()
        }
        // And this phone's gate refuses the same line without sending it.
        val gated = MachineLink({ Paired.session(machine, identity!!) }, dialled = Scope.COMPANION)
        try {
            val e = runCatching { gated.request(Machine.lock(), retry = false) }.exceptionOrNull()
            assertTrue("the gate let a lock through: $e", e is AgentError && CompanionLink.isRefusal(e))
            assertEquals(1, gated.refused)
        } finally {
            gated.close()
        }
        Thread.sleep(500)
        assertEquals(
            "a lock reached the stub machine from a companion session",
            actionsBefore,
            Desktop.machineStubActions().size,
        )

        // ---- unlock: the full key takes over ----
        unlockAndHold()
        assertFalse("unlocked, companion mode still runs", hub.companion.value)
        assertNull("unlocked, the companion link is still open", hub.companionLinkOf(machine.deviceId))
        assertEquals(Scope.FULL, hub.link(machine).scope)

        // ---- the switch off: forgotten at the computer, deleted here ----
        hub.setCompanionOn(false)
        assertEquals("the switch off left a key", 0, hub.companionKeys())
        assertFalse("the switch off left the keystore key", companionKeyInKeystore())
        awaitValue(10_000, "rime remote devices --json to show companion false") {
            if (listed()?.optBoolean("companion") == false) true else null
        }
        val stored = repository.loadBlocking().find(machine.deviceId)!!
        assertNull(stored.companion)
        assertFalse("the forget was sent but is still owed", stored.companionForgetOwed)

        // ---- and locked again, with the switch off, nothing keeps running ----
        hub.lock()
        assertFalse("locked with the switch off, companion mode started", hub.companion.value)
    }

    @Test
    fun with_the_switch_off_a_restart_starts_nothing_even_with_a_key_left() {
        hub.setCompanionOn(true)
        assertEquals(1, hub.companionKeys())
        // The switch alone off, the registered key left in place: turning it
        // off for real deletes the key, which would hide a boot start that
        // looked only at the keys.
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(companionOn = false)) }
        assertEquals(1, hub.companionKeys())
        hub.shutdown()
        val out = shell("su 0 am broadcast -a android.intent.action.BOOT_COMPLETED -p ${context.packageName}")
        assertTrue("the boot broadcast was not delivered: $out", "Broadcast completed" in out)
        Thread.sleep(2_000)
        assertFalse("the switch is off, but the boot receiver started companion mode", hub.companion.value)
        assertNull(hub.companionLinkOf(machine.deviceId))
    }

    @Test
    fun unpairing_forgets_the_companion_key_at_the_computer_and_deletes_it_here() {
        hub.setCompanionOn(true)
        assertTrue(companionKeyInKeystore())
        awaitValue(10_000, "companion true") { if (listed()?.optBoolean("companion") == true) true else null }
        // What the screen's Forget does (RemoteViewModel.forget).
        hub.unpairCompanion(machine.deviceId)
        hub.forget(machine.deviceId)
        runBlocking { repository.forget(machine) }
        assertFalse("unpairing left the companion keystore key", companionKeyInKeystore())
        awaitValue(10_000, "rime remote devices --json to show companion false after the unpair") {
            if (listed()?.optBoolean("companion") == false) true else null
        }
    }

    // ---- helpers --------------------------------------------------------

    /** What the app does on unlock: the full key in memory, the machine held. */
    private fun unlockAndHold() {
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        awaitValue(15_000, "the machine's remote hello over the full key") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() && hub.link(machine).connected }
        }
    }

    /** Whether the companion key's keystore key exists: what makes a sealed copy openable. */
    private fun companionKeyInKeystore(): Boolean =
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            .containsAlias(KeystoreSecretBox.companionAliasFor(machine.deviceId))

    /** This device in `rime remote devices --json`, as the person at the computer sees it. */
    private fun listed(): JSONObject? = Desktop.remoteDevicesJson().firstOrNull { it.optString("id") == machine.deviceId }

    private fun report(): JSONObject? =
        Desktop.ask("""{"cmd":"devices"}""").optJSONObject("phones")?.optJSONObject(machine.deviceId)

    private fun choose(apps: Set<String>) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mirrorApps = apps)) }
        hub.setMirrorApps(apps)
    }

    private fun setReply(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mirrorReply = on)) }
        hub.setMirrorReply(on)
    }

    private fun tag(what: String): String = "rime-companion-$what-${System.nanoTime() % 1_000_000}".also { tags += it }

    /** One word each: `executeShellCommand` splits on whitespace. */
    private fun post(tag: String, title: String, text: String) {
        val out = shell("cmd notification post -t $title $tag $text")
        assertFalse("the shell did not post: $out", out.contains("Error", ignoreCase = true))
    }

    private fun keyOf(tag: String): String? =
        shell("cmd notification list").lineSequence().map { it.trim() }.firstOrNull { "|$SHELL|" in it && "|$tag|" in it }

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
    }
}
