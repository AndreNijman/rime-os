package com.rimeos.remote.device

import android.app.Application
import android.content.ClipData
import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.provider.MediaStore
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.share.Shared
import com.rimeos.remote.ui.machine.MachineViewModel
import com.rimeos.remote.ui.send.SendScreen
import com.rimeos.remote.ui.send.SendUiState
import com.rimeos.remote.ui.send.SendViewModel
import java.security.MessageDigest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * "Send to Rime" on a phone, against the runner's rime-agentd.
 *
 * That rime-agentd writes its clipboard and Downloads into the stub desktop
 * (`RIME_AGENTD_DESKTOP_STUB`, which `run-device-suite.sh` refuses to run
 * without), so nothing here touches the clipboard or the Downloads folder of
 * the computer the suite runs on. What arrived is read back through the
 * broker, with its sha256: proof the computer has it, not just the phone's
 * own state.
 *
 * The shared files are real `content://` URIs: a MediaStore Downloads row
 * this app inserts (no permission needed on API 29+), read through the same
 * resolver path a photo shared from the gallery takes.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class SendOnDeviceTest {
    private val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as Application
    private val hub = LinkHub.get(app)
    private lateinit var machine: PairedMachine
    private lateinit var model: SendViewModel
    private val made = ArrayList<Uri>()

    @get:Rule
    val compose = createAndroidComposeRule<ComposeHostActivity>()

    @Before
    fun pair() {
        Desktop.deskReset()
        machine = Paired.machine("send-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.connectNow(machine)
        val hello = awaitValue(15_000, "rime-agentd's hello") {
            hub.state(machine.deviceId)?.hello
        }
        assertTrue("rime-agentd must offer save_file: ${hello.features}", hello.has(Features.SAVE_FILE))
        assertTrue("rime-agentd must offer clipboard_write: ${hello.features}", hello.has(Features.CLIPBOARD_WRITE))
        model = SendViewModel(app)
    }

    @After
    fun close() {
        for (u in made) runCatching { app.contentResolver.delete(u, null, null) }
        hub.forget(machine.deviceId)
        Desktop.deskReset()
    }

    private fun state(): SendUiState = model.state.value

    private fun <T : Any> awaitValue(timeoutMs: Long, what: String, read: () -> T?): T {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            read()?.let { return it }
            Thread.sleep(50)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what; the state was ${if (::model.isInitialized) state() else "not made"}")
    }

    private fun await(what: String, timeoutMs: Long = 20_000, ok: (SendUiState) -> Boolean) {
        awaitValue(timeoutMs, what) { state().takeIf(ok) }
    }

    /** A real shared file: a MediaStore Downloads row this app owns. */
    private fun sharedFile(name: String, bytes: ByteArray): Uri {
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, "application/octet-stream")
        }
        val uri = requireNotNull(app.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)) {
            "MediaStore did not make $name"
        }
        made.add(uri)
        app.contentResolver.openOutputStream(uri)!!.use { it.write(bytes) }
        return uri
    }

    private fun sha256(b: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }

    private fun body(n: Int): ByteArray = ByteArray(n) { (it * 31 % 251).toByte() }

    @Test
    fun shared_text_waits_for_the_tap_and_then_is_on_the_computers_clipboard() {
        val text = "copied on the phone\nsecond line \"quoted\" ${System.nanoTime()}"
        model.offer(Shared(text, emptyList(), 0), listOf(machine))
        compose.setContent {
            val s by model.state.collectAsStateWithLifecycle()
            SendScreen(state = s, onChoose = {}, onSend = { model.send(machine) }, onStop = {}, onDismiss = {}, onDone = {})
        }
        compose.onNodeWithText(text, substring = true).assertExists()
        compose.waitForIdle()
        Thread.sleep(500)
        assertNull("arriving must send nothing", Desktop.deskClipboard())

        compose.onNodeWithText("Copy to ${machine.machine}").performClick()
        await("the copy to land") { it.done }
        assertNull(state().failure)
        assertEquals("Copied to ${machine.machine}'s clipboard.", state().notice)
        assertEquals(text, Desktop.deskClipboard())
    }

    @Test
    fun a_shared_file_is_saved_whole_in_downloads_and_a_second_copy_is_numbered() {
        val name = "rime-send-${System.nanoTime() % 1_000_000}.bin"
        val bytes = body(300_000)
        val uri = sharedFile(name, bytes)

        model.offer(Shared(null, listOf(uri), 0), listOf(machine))
        await("the file to be described") { !it.describing }
        assertEquals(300_000L, state().files.single().size)
        assertEquals(name, state().files.single().name)
        Thread.sleep(300)
        assertTrue("arriving must save nothing", Desktop.deskDownloads().second.isEmpty())

        model.send(machine)
        await("the save to finish") { it.done }
        assertNull(state().failure)
        val saved = state().saved.single()
        assertEquals(name, saved.name)
        val (dir, files) = Desktop.deskDownloads()
        assertTrue("saved outside the stub: ${saved.path} vs $dir", saved.path.startsWith("$dir/"))
        assertEquals(listOf(Desktop.DeskFile(name, 300_000, sha256(bytes))), files)

        // The same share again: the computer keeps both.
        model.clear()
        model.offer(Shared(null, listOf(uri), 0), listOf(machine))
        await("the file to be described again") { !it.describing }
        model.send(machine)
        await("the second save") { it.done }
        val numbered = name.removeSuffix(".bin") + " (2).bin"
        assertEquals(numbered, state().saved.single().name)
        assertEquals(setOf(name, numbered), Desktop.deskDownloads().second.map { it.name }.toSet())
    }

    @Test
    fun several_files_go_in_order_and_each_lands_whole() {
        val a = body(70_000)
        val b = ByteArray(5_000) { 7 }
        val ua = sharedFile("rime-a-${System.nanoTime() % 100_000}.bin", a)
        val ub = sharedFile("rime-b-${System.nanoTime() % 100_000}.txt", b)
        model.offer(Shared(null, listOf(ua, ub), 0), listOf(machine))
        await("both described") { !it.describing }
        model.send(machine)
        await("both saved") { it.done }
        assertNull(state().failure)
        assertEquals(2, state().saved.size)
        val files = Desktop.deskDownloads().second.associateBy { it.name }
        assertEquals(sha256(a), files[state().saved[0].name]?.sha256)
        assertEquals(sha256(b), files[state().saved[1].name]?.sha256)
    }

    @Test
    fun stopping_mid_file_leaves_nothing_on_the_computer() {
        val bytes = body(24 * 1024 * 1024)
        val uri = sharedFile("rime-big-${System.nanoTime() % 100_000}.bin", bytes)
        model.offer(Shared(null, listOf(uri), 0), listOf(machine))
        await("described") { !it.describing }
        model.send(machine)
        await("bytes to start moving", 30_000) { it.sent > 0 || it.done }
        model.stop()
        await("the stop", 60_000) { it.done }
        val s = state()
        if (s.saved.isNotEmpty()) {
            // The whole file went over before the stop was seen: then it must be whole.
            assertEquals(sha256(bytes), Desktop.deskDownloads().second.single().sha256)
            return
        }
        assertTrue("the failure must say it stopped: ${s.failure}", s.failure!!.startsWith("Stopped."))
        // The daemon notices the early end and removes its temporary file.
        awaitValue(15_000, "the computer to drop the partial file") {
            Desktop.deskDownloads().second.takeIf { it.isEmpty() }
        }
    }

    @Test
    fun only_another_apps_content_is_taken_from_a_share() {
        val own = app.packageName
        val photo = Uri.parse("content://media/external/images/media/12")
        val send = Intent(Intent.ACTION_SEND).putExtra(Intent.EXTRA_STREAM, photo).putExtra(Intent.EXTRA_TEXT, "caption")
        val one = Shared.of(send, own)!!
        assertEquals(listOf(photo), one.streams)
        assertNull("the streams win over a caption", one.text)

        val multi = Intent(Intent.ACTION_SEND_MULTIPLE).putParcelableArrayListExtra(
            Intent.EXTRA_STREAM,
            arrayListOf(photo, Uri.parse("file:///sdcard/secret.txt"), Uri.parse("content://$own.androidx-startup/x")),
        )
        val mixed = Shared.of(multi, own)!!
        assertEquals(listOf(photo), mixed.streams)
        assertEquals(2, mixed.refused)

        val clipOnly = Intent(Intent.ACTION_SEND).apply { clipData = ClipData.newRawUri("", photo) }
        assertEquals(listOf(photo), Shared.of(clipOnly, own)!!.streams)

        val text = Shared.of(Intent(Intent.ACTION_SEND).putExtra(Intent.EXTRA_TEXT, "hello"), own)!!
        assertEquals("hello", text.text)
        assertNull(Shared.of(Intent(Intent.ACTION_VIEW), own))
        assertNull(Shared.of(Intent(Intent.ACTION_SEND), own))
    }

    @Test
    fun the_computer_screen_sends_this_phones_clipboard() {
        val m = MachineViewModel(app)
        m.open(machine)
        m.refresh()
        awaitValue(10_000, "the screen to see clipboard_write") { m.state.value.takeIf { it.offersClipboard } }
        val text = "from the phone ${System.nanoTime()}"
        m.sendClipboard(text)
        awaitValue(10_000, "the clipboard to land") { m.state.value.takeIf { it.acting == null && it.notice != null } }
        assertNull(m.state.value.failure)
        assertEquals(text, Desktop.deskClipboard())
        m.sendClipboard("")
        assertNotNull("an empty phone clipboard is said, not sent", m.state.value.failure)
        assertEquals(text, Desktop.deskClipboard())
    }
}
