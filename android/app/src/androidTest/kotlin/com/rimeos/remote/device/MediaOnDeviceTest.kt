package com.rimeos.remote.device

import android.content.Context
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.phone.Media
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.phone.MirrorService
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
 * What this phone is playing, on the computer (contract §2.8), on a phone,
 * against the runner's rime-remoted.
 *
 * The media session is the test helper's (android/testhelper, another app as
 * far as the listener can tell), driven with `am start`. The computer side is
 * rime-remoted's stub player (`RIME_REMOTED_MEDIA_STUB`, read through the
 * broker's `media_log`); nothing is put on the bus of the computer the suite
 * runs on.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class MediaOnDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context get() = instrumentation.targetContext
    private val hub = LinkHub.get(context)
    private val repository = MachineRepository(context)
    private val listener = MirrorService.component(context).flattenToString()
    private lateinit var machine: PairedMachine
    private lateinit var deviceName: String
    /** How many changes the stub had seen before this test: only later ones are this test's. */
    private var before = 0

    @Before
    fun pairAndListen() {
        shell("cmd notification allow_listener $listener")
        awaitValue(15_000, "Android to grant notification access") { if (MirrorService.granted(context)) true else null }
        // The switch on, no app chosen yet: each test chooses.
        settings(on = true, apps = emptySet())
        before = Desktop.mediaLog().size
        deviceName = "media-${System.nanoTime() % 100_000}"
        machine = Paired.machine(deviceName)
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val remote = awaitValue(15_000, "the machine's remote hello") {
            hub.state(machine.deviceId)?.remote?.takeIf { it.features.isNotEmpty() }
        }
        assertTrue("rime-remoted must offer ${Media.FEATURE}: ${remote.features}", remote.has(Media.FEATURE))
        awaitValue(10_000, "the hub to watch media sessions") { if (hub.mediaWatching) true else null }
    }

    @After
    fun tidy() {
        helper("stop")
        hub.lock()
        assertFalse("locked, the hub still watches media sessions", hub.mediaWatching)
        settings(on = false, apps = emptySet())
        shell("cmd notification disallow_listener $listener")
    }

    @Test
    fun a_chosen_apps_playback_shows_follows_pause_and_seek_and_goes_when_it_stops() {
        choose(setOf(HELPER))
        helper("play", 1_000)
        val (at, shown) = awaitEntry("the helper's song on the computer") {
            it.optString("op") == "show" && mine(it) && it.optString("title") == "Helper Song" && it.optString("status") == "Playing"
        }
        // "Rime test helper" when Android lets this app read the label, the
        // package name when it does not (no launcher icon, no QUERY_ALL_PACKAGES);
        // either way it says which phone and which app.
        val identity = shown.getString("identity")
        assertTrue("the identity names neither the helper nor its package: $identity", identity.endsWith(HELPER_LABEL) || identity.endsWith(HELPER))
        assertEquals("Helper Band", shown.optString("artist"))
        assertEquals("Helper LP", shown.optString("album"))
        assertEquals(180_000L, shown.getLong("duration_ms"))
        assertTrue("the first position is not where playback was: $shown", shown.getLong("position_ms") in 1_000L..10_000L)
        val player = shown.getString("player")

        helper("pause")
        val (pausedAt, _) = awaitEntry("the pause on the computer", from = at) {
            it.optString("op") == "show" && it.optString("player") == player && it.optString("status") == "Paused"
        }

        helper("seek", 120_000)
        val (soughtAt, sought) = awaitEntry("the seek on the computer", from = pausedAt) {
            it.optString("op") == "show" && it.optString("player") == player && it.getLong("position_ms") == 120_000L
        }
        assertEquals("Paused", sought.optString("status"))

        helper("stop")
        awaitEntry("the player to go when playback stops", from = soughtAt) { it.optString("op") == "gone" && it.optString("player") == player }
    }

    @Test
    fun an_app_not_chosen_never_shows_and_turning_media_off_takes_it_away() {
        // Playing, but not chosen: nothing reaches the computer.
        helper("play", 5_000)
        Thread.sleep(2_500)
        val early = log().filter { mine(it) }
        assertTrue("an app not chosen reached the computer: $early", early.isEmpty())

        // Chosen now, with no change on the phone: shown at once.
        choose(setOf(HELPER))
        val (at, shown) = awaitEntry("the helper once chosen") { it.optString("op") == "show" && mine(it) && it.optString("status") == "Playing" }
        val player = shown.getString("player")

        // The switch off: gone from the computer, and nothing more is sent.
        settings(on = false, apps = setOf(HELPER))
        val (goneAt, _) = awaitEntry("the player to go when media is turned off", from = at) { it.optString("op") == "gone" && it.optString("player") == player }
        awaitValue(5_000, "the hub to stop watching") { if (!hub.mediaWatching) true else null }
        helper("seek", 90_000)
        Thread.sleep(2_000)
        val later = log().drop(goneAt + 1).filter { it.optString("player") == player }
        assertTrue("switched off, the phone still sent: $later", later.isEmpty())
    }

    @Test
    fun a_connection_made_while_something_plays_is_shown_it_at_once() {
        choose(setOf(HELPER))
        helper("play", 30_000)
        val (at, first) = awaitEntry("the helper's song on the computer") {
            it.optString("op") == "show" && mine(it) && it.optString("status") == "Playing"
        }
        val firstSeen = System.currentTimeMillis()
        val player = first.getString("player")

        // The connection ends: the computer drops the player, the watcher stops.
        hub.lock()
        val (goneAt, _) = awaitEntry("the player to go with the connection", from = at) {
            it.optString("op") == "gone" && it.optString("player") == player
        }
        awaitValue(5_000, "the hub to stop watching") { if (!hub.mediaWatching) true else null }

        // A new connection, the helper untouched: the watcher's first read of
        // the sessions is all there is to go on, and it is sent at once.
        hub.remember(machine.deviceId, Paired.identityOf(machine))
        hub.markUnlocked()
        hub.hold(machine)
        val (_, again) = awaitEntry("the song on the new connection", from = goneAt) {
            it.optString("op") == "show" && it.optString("player") == player && it.optString("status") == "Playing"
        }
        val elapsed = System.currentTimeMillis() - firstSeen
        val moved = again.getLong("position_ms") - first.getLong("position_ms")
        assertTrue(
            "the new connection was not told where playback is now: moved $moved ms in $elapsed ms",
            moved in (elapsed - 3_000)..(elapsed + 3_000),
        )
    }

    @Test
    fun the_computer_plays_pauses_skips_and_seeks_what_it_shows_and_the_app_gets_each() {
        clearControls()
        choose(setOf(HELPER))
        helper("play", 1_000)
        val remote = hub.state(machine.deviceId)!!.remote!!
        assertTrue("rime-remoted must offer ${Media.CONTROL_FEATURE}: ${remote.features}", remote.has(Media.CONTROL_FEATURE))
        val (at, shown) = awaitEntry("the helper's song, controllable, on the computer") {
            it.optString("op") == "show" && mine(it) && it.optString("status") == "Playing" && it.optBoolean("can_control")
        }
        val player = shown.getString("player")
        val can = shown.getJSONArray("can").let { a -> (0 until a.length()).map { a.getString(it) } }
        assertEquals(listOf("play", "pause", "next", "previous", "seek"), can)

        // Play/pause while playing: the app is told to pause, and the computer sees it paused.
        call(player, "PlayPause")
        val (c1, r1) = outcome(player, "PlayPause", at)
        assertEquals("ok", r1)
        val (pausedAt, _) = awaitEntry("the pause on the computer", from = c1) {
            it.optString("op") == "show" && it.optString("player") == player && it.optString("status") == "Paused"
        }
        awaitValue(10_000, "the helper to be told to pause") { if ("pause" in controls()) true else null }

        // Next: a new track, from the start.
        call(player, "Next")
        assertEquals("ok", outcome(player, "Next", pausedAt).second)
        val (nextAt, next) = awaitEntry("the next track on the computer", from = pausedAt) {
            it.optString("op") == "show" && it.optString("player") == player && it.optString("title") == "Helper Song 2"
        }
        assertTrue("a new title is a new track: $shown / $next", next.getString("track") != shown.getString("track"))

        // A relative seek crosses as where to: paused at 0, +60 s is 60 000 ms.
        call(player, "Seek") { put("offset_us", 60_000_000L) }
        assertEquals("ok", outcome(player, "Seek", nextAt).second)
        val (soughtAt, _) = awaitEntry("the seek on the computer", from = nextAt) {
            it.optString("op") == "show" && it.optString("player") == player && it.getLong("position_ms") == 60_000L
        }
        // SetPosition on the current track, as the Shell's seek bar does.
        call(player, "SetPosition") { put("track", next.getString("track")); put("position_us", 90_000_000L) }
        assertEquals("ok", outcome(player, "SetPosition", soughtAt).second)
        awaitValue(10_000, "the helper to be told each control") {
            controls().takeIf { it.containsAll(listOf("pause", "next", "seek 60000", "seek 90000")) }
        }
        val got = controls()
        assertEquals("each call reached the app once, in order: $got", listOf("pause", "next", "seek 60000", "seek 90000"), got)
    }

    @Test
    fun with_control_off_the_computer_only_shows_and_nothing_reaches_the_app() {
        clearControls()
        settings(on = true, apps = setOf(HELPER), control = false)
        helper("play", 1_000)
        val (at, shown) = awaitEntry("the helper's song on the computer") {
            it.optString("op") == "show" && mine(it) && it.optString("status") == "Playing"
        }
        val player = shown.getString("player")
        assertFalse("control off, the computer was offered control: $shown", shown.optBoolean("can_control"))
        assertEquals(0, shown.getJSONArray("can").length())
        call(player, "PlayPause")
        val (c1, r1) = outcome(player, "PlayPause", at)
        assertEquals("not_supported", r1)
        call(player, "Next")
        assertEquals("not_supported", outcome(player, "Next", c1).second)
        Thread.sleep(1_500)
        assertTrue("control off, the app was told: ${controls()}", controls().isEmpty())
        assertTrue(
            "control off, playback changed: ${log()}",
            log().none { it.optString("player") == player && it.optString("op") == "show" && it.optString("status") != "Playing" },
        )

        // The switch follows at once, both ways, with no change on the phone.
        control(true)
        val (onAt, _) = awaitEntry("control turned on, on the computer", from = c1) {
            it.optString("op") == "show" && it.optString("player") == player && it.optBoolean("can_control")
        }
        control(false)
        awaitEntry("control turned off again, on the computer", from = onAt) {
            it.optString("op") == "show" && it.optString("player") == player && !it.optBoolean("can_control")
        }
    }

    // ---- helpers --------------------------------------------------------

    /** Is [entry] about this test's phone? A `show` carries its name; `gone` only the player. */
    private fun mine(entry: JSONObject): Boolean = entry.optString("identity").startsWith("$deviceName · ")

    private fun log(): List<JSONObject> = Desktop.mediaLog().drop(before)

    /** The switch and the apps, written where the app keeps them and told to the hub, as the screen does. */
    private fun settings(on: Boolean, apps: Set<String>, control: Boolean = true) {
        runBlocking {
            repository.settings(repository.loadBlocking().settings.copy(mediaOn = on, mediaApps = apps, mediaControl = control))
        }
        hub.setMediaApps(apps)
        hub.setMediaControl(control)
        hub.setMediaOn(on)
    }

    /** "Control from your computer", as the screen sets it. */
    private fun control(on: Boolean) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mediaControl = on)) }
        hub.setMediaControl(on)
    }

    /** What the helper's session was told to do since the last [clearControls]: `play`, `next`, `seek 60000`, … */
    private fun controls(): List<String> =
        shell("logcat -d -s $HELPER_TAG:I").lines().mapNotNull { l -> l.substringAfter("control=", "").takeIf { it.isNotEmpty() }?.trim() }

    private fun clearControls() {
        shell("logcat -c")
    }

    private fun call(player: String, method: String, extra: JSONObject.() -> Unit = {}) =
        Desktop.mediaCall(JSONObject().put("player", player).put("method", method).apply(extra))

    /** The outcome the stub recorded for this test's next call on [player], after index [from]. */
    private fun outcome(player: String, method: String, from: Int): Pair<Int, String> {
        val (at, e) = awaitEntry("the computer's $method call", from) {
            it.optString("op") == "call" && it.optString("player") == player && it.optString("method") == method
        }
        return at to e.getString("result")
    }

    private fun choose(apps: Set<String>) {
        runBlocking { repository.settings(repository.loadBlocking().settings.copy(mediaApps = apps)) }
        hub.setMediaApps(apps)
    }

    private fun helper(action: String, position: Long? = null) {
        val extra = position?.let { " --el position $it" }.orEmpty()
        val out = shell("am start -W -n $HELPER/.MediaActivity --es do $action$extra")
        assertFalse("the helper did not start: $out", out.contains("Error", ignoreCase = true))
    }

    /** The first of this test's entries after index [from] that matches, and its index. */
    private fun awaitEntry(what: String, from: Int = -1, match: (JSONObject) -> Boolean): Pair<Int, JSONObject> =
        awaitValue(15_000, what) {
            val all = log()
            ((from + 1) until all.size).firstOrNull { match(all[it]) }?.let { it to all[it] }
        }

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
        /** android/testhelper, installed by run-device-suite.sh: it holds a media session. */
        const val HELPER = "com.rimeos.remote.testhelper"
        const val HELPER_LABEL = "Rime test helper"

        /** The helper's log tag: it logs each transport control its session gets. */
        const val HELPER_TAG = "RimeMediaHelper"
    }
}
