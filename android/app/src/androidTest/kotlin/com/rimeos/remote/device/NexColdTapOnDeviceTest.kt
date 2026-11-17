package com.rimeos.remote.device

import android.app.Application
import android.content.Intent
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import com.rimeos.remote.MainActivity
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.pairing.GatedBox
import com.rimeos.remote.security.KeystoreSecretBox
import com.rimeos.remote.ui.Notifier
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * A nex notification tapped while no machine is connected, through the whole
 * app: the activity the notification opens, the front door, the connection
 * the tap asks for, the hello that says the machine offers nex, and the nex
 * screen.
 *
 * Unlike the other suites this one does not use [Paired.PlainBox]: the machine
 * is paired with the app's own keystore box, created without an
 * authentication gate, and saved through the app's own [MachineRepository].
 * That is exactly what the app does on a phone with no screen lock (the
 * emulator has none), so no prompt stands between the tap and the screen and
 * nothing here reaches around the activity.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class NexColdTapOnDeviceTest {
    private val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as Application
    private val repository = MachineRepository(app)
    private val device = UiDevice.getInstance(InstrumentationRegistry.getInstrumentation())
    private lateinit var machine: PairedMachine

    @Before
    fun pairAsTheAppWould() = runBlocking {
        val offer = Pairing.decodeOffer(Desktop.offerPayload())
        machine = Paired.service.pair(
            offer = offer,
            deviceName = "cold-${System.nanoTime() % 100_000}",
            boxFor = { id ->
                val box = KeystoreSecretBox.forDevice(id, requireAuthentication = false)
                GatedBox(box, box.authenticationIsRequired)
            },
            nowMs = System.currentTimeMillis(),
        )
        repository.remember(machine)
        Unit
    }

    @After
    fun forget() = runBlocking {
        LinkHub.get(app).forget(machine.deviceId)
        repository.forget(machine)
        KeystoreSecretBox.forget(machine.deviceId)
    }

    @Test
    fun a_nex_tap_with_nothing_connected_connects_and_lands_on_nex() {
        // What the notification's PendingIntent carries (Notifier.post).
        val tap = Intent(app, MainActivity::class.java)
            .setAction(Notifier.ACTION_OPEN_ALERT)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
            .putExtra(Notifier.EXTRA_MACHINE, machine.deviceId)
            .putExtra(Notifier.EXTRA_SESSION, Alert.NO_SESSION)
            .putExtra(Notifier.EXTRA_NEX, true)
        ActivityScenario.launch<MainActivity>(tap).use {
            // The nex screen's subtitle, and nothing else's: the machine the
            // tap named, with nex's live feed open on it.
            val landed = device.wait(Until.hasObject(By.text("${machine.machine} · live")), 45_000)
            assertTrue(
                "the tap did not reach the nex screen; on screen: " +
                    device.findObjects(By.textContains("")).mapNotNull { o -> o.text }.take(20),
                landed == true,
            )
            assertTrue(
                "nex opened before the tapped machine was connected",
                LinkHub.get(app).state(machine.deviceId)?.status == LinkHub.Status.CONNECTED,
            )
        }
    }
}
