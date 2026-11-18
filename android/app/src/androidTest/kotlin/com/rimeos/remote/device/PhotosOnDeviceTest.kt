package com.rimeos.remote.device

import android.content.Context
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.core.phone.Photos
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
 * This phone's photos on the computer (contract §2.13), on an EMULATOR,
 * against the runner's rime-remoted: three images the broker made and pushed
 * show in `rime remote photos`, each thumbnail sits in the run's runtime
 * folder readable by its owner only and carries no EXIF, a saved original is
 * the phone's file byte for byte, and turning the switch off takes the list
 * and the thumbnails away.
 *
 * Emulator only, and asserted before anything is pushed: the runner leaves
 * this class out on any other serial, the broker refuses to push to one, and
 * the first thing [setUp] does is check this is an emulator. The images are
 * made on the computer for this run and deleted after it; no photo of
 * anybody's is read.
 *
 * Photo access is granted with `pm grant`, as the person would in the prompt,
 * and left granted: revoking it kills this process mid-run.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class PhotosOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private lateinit var machine: PairedMachine
    private lateinit var pushed: List<Desktop.TestPhoto>

    @Before
    fun setUp() {
        assertTrue(
            "the photo tests run only on an emulator (hardware ${Build.HARDWARE})",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish" || shell("getprop ro.kernel.qemu").trim() == "1",
        )
        val permission = if (Build.VERSION.SDK_INT >= 33) "READ_MEDIA_IMAGES" else "READ_EXTERNAL_STORAGE"
        shell("pm grant ${context.packageName} android.permission.$permission")
        setPhotos(false)
        pushed = Desktop.photosPush()
        assertEquals("the broker made three test images: $pushed", 3, pushed.size)
        assertEquals("exactly one test image has a location: $pushed", 1, pushed.count { it.gps })
        machine = Paired.machine("photos-${System.nanoTime() % 100_000}")
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Photos.FEATURE}: ${remote.features}", remote.has(Photos.FEATURE))
    }

    @After
    fun tidy() {
        hub.lock()
        setPhotos(false)
        Desktop.photosCleanup()
    }

    @Test
    fun the_computer_lists_the_photos_with_clean_thumbnails_and_saves_an_original() {
        val first = awaitValue(15_000, "the first report") { report() }
        assertFalse("the report said photos with the switch off: $first", can(first).contains(Phone.CAN_PHOTOS))
        turnOn()

        val list = awaitValue(40_000, "the computer to list the three test images") {
            Desktop.photosList(machine.deviceId, 10).takeIf { r ->
                r.getInt("code") == 0 && pushed.all { p -> listed(r).any { it.optString("name") == p.name } }
            }
        }
        val out = list.getJSONObject("out")
        assertFalse("full access was reported as partial: $out", out.optBoolean("partial"))
        val ids = HashMap<String, Long>()
        for (p in pushed) {
            val item = listed(list).first { it.optString("name") == p.name }
            assertEquals("its length: $item", p.len, item.getLong("size"))
            assertEquals("its width: $item", 1600, item.getInt("width"))
            assertEquals("its height: $item", 1200, item.getInt("height"))
            assertEquals("its type: $item", "image/jpeg", item.getString("mime"))
            assertTrue("no thumbnail for ${p.name}: $item", item.optString("thumb").isNotEmpty())
            ids[p.name] = item.getLong("id")
        }
        // Newest first: nothing in the answer is newer than what follows it.
        val dates = listed(list).map { it.getLong("date_ms") }
        assertEquals("the list is not newest first: $dates", dates.sortedDescending(), dates)

        val thumbs = objects(list, "thumbs").associateBy { it.getLong("id") }
        for (p in pushed) {
            val t = checkNotNull(thumbs[ids.getValue(p.name)]) { "no facts about ${p.name}'s thumbnail: $list" }
            assertTrue("the thumbnail is not on the computer: $t", t.getBoolean("exists"))
            assertTrue("the thumbnail is outside the run's runtime folder: $t", t.getBoolean("under_run"))
            assertEquals("the thumbnail's mode: $t", "600", t.getString("mode"))
            assertTrue("the thumbnail is not a JPEG: $t", t.getBoolean("jpeg"))
            assertFalse("the thumbnail carries EXIF: $t", t.getBoolean("exif"))
            assertFalse("the thumbnail carries an APP1 segment: $t", t.getBoolean("app1"))
        }

        val plain = pushed.first { !it.gps }
        val saved = Desktop.photosSave(machine.deviceId, ids.getValue(plain.name))
        assertEquals("the save failed: $saved", 0, saved.getInt("code"))
        assertEquals("the saved length: $saved", plain.len, saved.getJSONObject("out").getLong("len"))
        assertEquals("the saved photo is not the phone's file: $saved", plain.sha256, saved.getJSONObject("saved").getString("sha256"))

        // What Android does with a location in an original this app reads
        // without ACCESS_MEDIA_LOCATION. From Android 10 on it must be gone:
        // the docs and the in-app help say so. Before, Android hands the file
        // over as it is, and a location may come through only untouched.
        val located = pushed.first { it.gps }
        val withGps = Desktop.photosSave(machine.deviceId, ids.getValue(located.name))
        assertEquals("the save of the image with a location failed: $withGps", 0, withGps.getInt("code"))
        val facts = withGps.getJSONObject("saved")
        val identical = facts.getString("sha256") == located.sha256
        val gps = facts.getBoolean("gps")
        Log.i(TAG, "the image with a location came back ${if (identical) "byte for byte" else "changed"}, location ${if (gps) "present" else "absent"}")
        if (Build.VERSION.SDK_INT >= 29) {
            assertFalse("Android ${Build.VERSION.SDK_INT} let a photo's location reach the computer: $withGps", gps)
        } else {
            assertTrue("a location came through in a file Android changed: $withGps", !gps || identical)
        }
    }

    @Test
    fun with_the_switch_off_the_computer_lists_nothing_and_its_thumbnails_go() {
        turnOn()
        awaitValue(40_000, "a listing to leave thumbnails on the computer") {
            Desktop.photosList(machine.deviceId, 10).takeIf { it.getInt("code") == 0 && objects(it, "thumbs").isNotEmpty() }
        }
        assertTrue("no thumbnail folder after a listing", Desktop.photosThumbsDir(machine.deviceId))

        setPhotos(false)
        awaitValue(15_000, "the computer to learn Photos went off") {
            report()?.takeIf { !can(it).contains(Phone.CAN_PHOTOS) }
        }
        val refused = Desktop.photosList(machine.deviceId, 10)
        assertEquals("a phone with the switch off was listed: $refused", 1, refused.getInt("code"))
        val why = refused.optJSONObject("out")?.optString("error").orEmpty() + " " + refused.optString("stderr")
        assertTrue("the refusal does not say why: $refused", why.contains("has not turned on Photos on your computer"))
        assertTrue("photos were listed with the switch off: $refused", listed(refused).isEmpty())
        awaitValue(15_000, "the computer to delete this phone's thumbnails") {
            true.takeIf { !Desktop.photosThumbsDir(machine.deviceId) }
        }
    }

    // ---- helpers --------------------------------------------------------

    private fun turnOn() {
        setPhotos(true)
        awaitValue(15_000, "the computer to learn it may ask for photos") {
            report()?.takeIf { can(it).contains(Phone.CAN_PHOTOS) }
        }
    }

    /** "Photos on your computer", turned on or off as the screen does. */
    private fun setPhotos(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(photosOn = on)) }
        hub.setPhotosOn(on)
    }

    private fun listed(reply: JSONObject): List<JSONObject> =
        reply.optJSONObject("out")?.let { objects(it, "photos") } ?: emptyList()

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
            Thread.sleep(500)
        }
        throw AssertionError("timed out after $timeoutMs ms waiting for $what")
    }

    private fun shell(command: String): String {
        val fd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(fd).bufferedReader().use { it.readText() }
    }

    private companion object {
        const val TAG = "RimePhotosTest"
    }
}
