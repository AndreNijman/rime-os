package com.rimeos.remote.core.phone

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.double
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** What this phone is playing, on a computer (`docs/remote-live-contract.md` §2.8). */
class MediaTest {
    private val now = 1_000_000L

    private fun facts(
        pkg: String = "org.example.music",
        state: PlayState = PlayState.PLAYING,
        title: String? = "Song",
        position: Long = 42_000,
        updatedAt: Long = now,
        speed: Float = 1f,
        duration: Long = 215_000,
    ) = SessionFacts(
        pkg = pkg,
        app = "Music",
        state = state,
        title = title,
        artist = "Band",
        album = "LP",
        durationMs = duration,
        positionMs = position,
        updatedAtMs = updatedAt,
        speed = speed,
    )

    private fun state(f: SessionFacts = facts(), at: Long = now) = MediaState.of(f, at)!!

    private fun body(line: String): JsonObject = Json.parseToJsonElement(line).jsonObject

    private fun playing(line: String): JsonObject = body(line)["playing"]!!.jsonObject

    // ---- the line -------------------------------------------------------

    @Test
    fun `the report carries exactly the contract's fields and nothing else`() {
        val o = body(Media.encode(state(), now))
        assertEquals(setOf("cmd", "playing"), o.keys)
        assertEquals("phone_media", o["cmd"]!!.jsonPrimitive.content)
        val p = o["playing"]!!.jsonObject
        assertEquals(setOf("app", "state", "title", "artist", "album", "duration_ms", "position_ms", "rate"), p.keys)
        assertEquals("Music", p["app"]!!.jsonPrimitive.content)
        assertEquals("playing", p["state"]!!.jsonPrimitive.content)
        assertEquals("Song", p["title"]!!.jsonPrimitive.content)
        assertEquals(215_000, p["duration_ms"]!!.jsonPrimitive.long)
        assertEquals(42_000, p["position_ms"]!!.jsonPrimitive.long)
        assertEquals(1.0, p["rate"]!!.jsonPrimitive.double)
        assertEquals("""{"cmd":"phone_media","playing":null}""", Media.encode(null, now))
        assertEquals(JsonNull, body(Media.encode(null, now))["playing"])
    }

    @Test
    fun `unknown artist, album and length are left out rather than sent empty`() {
        val p = playing(Media.encode(state(facts(duration = 0).copy(artist = null, album = " ")), now))
        assertEquals(setOf("app", "state", "title", "position_ms", "rate"), p.keys)
    }

    @Test
    fun `the position is where playback is when the line is written, in whole milliseconds`() {
        // Read at now, written 1.25 s later at 1.5x: 42 s + 1.875 s.
        val s = state(facts(speed = 1.5f))
        assertEquals(43_875, playing(Media.encode(s, now + 1_250))["position_ms"]!!.jsonPrimitive.long)
        // Android stamped the position 2 s before it was read: that counts too.
        assertEquals(44_000, state(facts(updatedAt = now - 2_000)).positionAt(now))
        // Paused, the clock moves nothing.
        assertEquals(42_000, state(facts(state = PlayState.PAUSED)).positionAt(now + 60_000))
        // Waiting (buffering): shown playing, held where it is.
        val waiting = state(facts(state = PlayState.WAITING))
        assertTrue(waiting.playing)
        assertEquals(0.0, waiting.rate)
        assertEquals(42_000, waiting.positionAt(now + 5_000))
    }

    @Test
    fun `values Android allows but the computer refuses are clamped, never sent`() {
        // Unknown position (-1), an anchor of 0 or in the future.
        assertEquals(0, state(facts(position = -1)).positionAt(now))
        assertEquals(42_000, state(facts(updatedAt = 0)).positionAt(now))
        assertEquals(42_000, state(facts(updatedAt = now + 10_000)).positionAt(now))
        // A position past the end is the end, and stays there.
        assertEquals(215_000, state(facts(position = 999_000)).positionAt(now))
        assertEquals(215_000, state(facts(position = 214_000)).positionAt(now + 10_000))
        // A live stream's "length" (negative, or past 7 days) is unknown.
        assertEquals(0, state(facts(duration = -1)).durationMs)
        assertEquals(0, state(facts(duration = Media.MAX_MS + 1)).durationMs)
        // ... and its position is capped at 7 days, not at a length it does not have.
        assertEquals(Media.MAX_MS, state(facts(duration = 0, position = Long.MAX_VALUE / 2)).positionAt(now))
        // Rewinding (negative speed), NaN, too fast.
        assertEquals(0.0, state(facts(speed = -2f)).rate)
        assertEquals(1.0, state(facts(speed = Float.NaN)).rate)
        assertEquals(Media.MAX_RATE, state(facts(speed = 32f)).rate)
        // Paused with speed 0 reads as rate 1, not as a stopped player.
        assertEquals(1.0, state(facts(state = PlayState.PAUSED, speed = 0f)).rate)
        for (s in listOf(facts(position = -1, duration = -5, speed = -3f), facts(speed = Float.POSITIVE_INFINITY))) {
            val p = playing(Media.encode(state(s), now + 1_000))
            assertTrue(p["position_ms"]!!.jsonPrimitive.long in 0..Media.MAX_MS)
            assertTrue(p["rate"]!!.jsonPrimitive.double in 0.0..Media.MAX_RATE)
        }
    }

    @Test
    fun `text is cut to the computer's caps and a title with nothing to read shows nothing`() {
        val long = facts(title = "t".repeat(500)).copy(app = "A".repeat(100), artist = "b".repeat(500), album = "c".repeat(500))
        val s = state(long)
        assertEquals(Media.APP_CAP + 1, s.app.length)
        assertEquals(Media.TITLE_CAP + 1, s.title.length)
        assertEquals(Media.ARTIST_CAP + 1, s.artist.length)
        assertEquals(Media.ALBUM_CAP + 1, s.album.length)
        assertTrue(s.title.endsWith("…"))
        assertEquals("One line", state(facts(title = "One\nline")).title.replace("  ", " "))
        assertNull(MediaState.of(facts(title = null), now))
        assertNull(MediaState.of(facts(title = "   "), now))
        assertNull(MediaState.of(facts(title = "\u0007\n\t"), now))
        assertNull(MediaState.of(facts(state = PlayState.IDLE), now))
    }

    // ---- which session --------------------------------------------------

    private val allowed = setOf("a", "b", "c")

    private fun board(vararg sessions: Pair<String, SessionFacts>, at: Long = now) = MediaBoard().apply { sync(sessions.toList(), at) }

    @Test
    fun `a playing session wins, the first in the system's order among several`() {
        val b = board(
            "1" to facts(pkg = "a", state = PlayState.PAUSED, title = "Paused"),
            "2" to facts(pkg = "b", title = "First playing"),
            "3" to facts(pkg = "c", title = "Second playing"),
        )
        assertEquals("First playing", b.current(allowed, now)?.title)
        assertEquals("Second playing", b.current(setOf("a", "c"), now)?.title)
    }

    @Test
    fun `with nothing playing, the session that paused most recently`() {
        val b = board(
            "1" to facts(pkg = "a", state = PlayState.PLAYING, title = "A"),
            "2" to facts(pkg = "b", state = PlayState.PLAYING, title = "B"),
        )
        b.sync(listOf("1" to facts(pkg = "a", state = PlayState.PAUSED, title = "A"), "2" to facts(pkg = "b", title = "B")), now + 1_000)
        b.sync(
            listOf("1" to facts(pkg = "a", state = PlayState.PAUSED, title = "A"), "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "B")),
            now + 2_000,
        )
        assertEquals("B", b.current(allowed, now + 3_000)?.title)
        // A pauses again later: now A.
        b.sync(listOf("1" to facts(pkg = "a", title = "A"), "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "B")), now + 4_000)
        b.sync(
            listOf("1" to facts(pkg = "a", state = PlayState.PAUSED, title = "A"), "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "B")),
            now + 5_000,
        )
        assertEquals("A", b.current(allowed, now + 6_000)?.title)
    }

    @Test
    fun `sessions already paused when first seen are dated by Android's last update`() {
        val b = board(
            "1" to facts(pkg = "a", state = PlayState.PAUSED, title = "Yesterday", updatedAt = now - 86_400_000),
            "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "A minute ago", updatedAt = now - 60_000),
        )
        assertEquals("A minute ago", b.current(allowed, now)?.title)
        // One that pauses while watched is newer than either.
        b.sync(
            listOf(
                "1" to facts(pkg = "a", state = PlayState.PAUSED, title = "Yesterday", updatedAt = now - 86_400_000),
                "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "A minute ago", updatedAt = now - 60_000),
                "3" to facts(pkg = "c", title = "Just now"),
            ),
            now + 1,
        )
        b.sync(
            listOf(
                "1" to facts(pkg = "a", state = PlayState.PAUSED, title = "Yesterday", updatedAt = now - 86_400_000),
                "2" to facts(pkg = "b", state = PlayState.PAUSED, title = "A minute ago", updatedAt = now - 60_000),
                "3" to facts(pkg = "c", state = PlayState.PAUSED, title = "Just now", updatedAt = now - 999_999),
            ),
            now + 2,
        )
        assertEquals("Just now", b.current(allowed, now + 3)?.title)
    }

    @Test
    fun `stopped, untitled and unchosen sessions are never shown`() {
        val b = board(
            "1" to facts(pkg = "a", state = PlayState.IDLE, title = "Stopped"),
            "2" to facts(pkg = "b", title = ""),
            "3" to facts(pkg = "x", title = "Not chosen"),
            "4" to facts(pkg = "c", state = PlayState.PAUSED, title = "Paused"),
        )
        assertEquals("Paused", b.current(allowed, now)?.title)
        assertNull(b.current(setOf("a", "b", "x").minus("x"), now))
        assertNull(b.current(emptySet(), now))
        assertEquals(listOf("a", "b", "x", "c"), b.packages)
    }

    @Test
    fun `a session that goes away is forgotten`() {
        val b = board("1" to facts(pkg = "a", title = "A"))
        assertNotNull(b.current(allowed, now))
        b.sync(emptyList(), now + 1)
        assertNull(b.current(allowed, now + 1))
        b.sync(listOf("1" to facts(pkg = "a", title = "A")), now + 2)
        b.clear()
        assertNull(b.current(allowed, now + 2))
    }

    // ---- what goes, and when --------------------------------------------

    /** Offer, then take what is due at [at], marking it sent. */
    private fun MediaOutbox.step(s: MediaState?, at: Long): MediaOutbox.Report? {
        offer(s)
        return next(at)?.also { sent(it, at) }
    }

    @Test
    fun `a new connection sends the current state at once, and nothing when nothing plays`() {
        val o = MediaOutbox()
        assertNull(o.step(null, now), "null sent to a computer that was shown nothing")
        assertEquals(0, o.waitMs(now))
        val r = o.step(state(), now)
        assertEquals("Song", r?.state?.title)
    }

    @Test
    fun `the clock alone sends nothing, a seek does`() {
        val o = MediaOutbox()
        o.step(state(), now)
        // 10 s later the phone reports the position the clock gives: nothing to send.
        val later = now + 10_000
        assertNull(o.step(state(facts(position = 52_000, updatedAt = later), later), later))
        // A little drift either way is still the clock.
        assertNull(o.step(state(facts(position = 53_400, updatedAt = later), later), later))
        assertNull(o.step(state(facts(position = 50_600, updatedAt = later), later), later))
        // Past the slack: a seek.
        assertNotNull(o.step(state(facts(position = 53_600, updatedAt = later), later), later))
        val further = later + 5_000
        assertNotNull(o.step(state(facts(position = 10_000, updatedAt = further), further), further), "a seek back")
    }

    @Test
    fun `a pause, a new track and a speed change are each sent`() {
        val o = MediaOutbox()
        o.step(state(), now)
        var t = now + 1_000
        assertNotNull(o.step(state(facts(state = PlayState.PAUSED, position = 43_000, updatedAt = t), t), t), "pause")
        t += 1_000
        assertNotNull(o.step(state(facts(position = 43_000, updatedAt = t), t), t), "resume")
        t += 1_000
        assertNotNull(o.step(state(facts(title = "Next song", position = 0, updatedAt = t), t), t), "new track")
        t += 1_000
        assertNotNull(o.step(state(facts(title = "Next song", position = 1_000, updatedAt = t, speed = 2f), t), t), "speed")
        t += 1_000
        assertNotNull(o.step(null, t), "nothing playing any more")
        t += 1_000
        assertNull(o.step(null, t), "null again")
    }

    @Test
    fun `at most one report per half second, the newest sent when the gap ends`() {
        val o = MediaOutbox()
        assertNotNull(o.step(state(), now))
        // Three changes inside the gap: none goes now.
        for ((i, title) in listOf("Two", "Three", "Four").withIndex()) {
            val t = now + 100L * (i + 1)
            o.offer(state(facts(title = title, updatedAt = t), t))
            assertNull(o.next(t))
        }
        assertEquals(200, o.waitMs(now + 300))
        assertNull(o.next(now + 499))
        val r = o.next(now + 500)
        assertEquals("Four", r?.state?.title, "the last change was lost")
        o.sent(r!!, now + 500)
        assertEquals(0, o.waitMs(now + 500))
        assertNull(o.next(now + 2_000))
    }

    @Test
    fun `a change reverted inside the gap sends nothing`() {
        val o = MediaOutbox()
        o.step(state(), now)
        o.offer(state(facts(state = PlayState.PAUSED), now + 100))
        o.offer(state(facts(updatedAt = now + 200), now + 200))
        assertNull(o.next(now + 600))
        assertEquals(0, o.waitMs(now + 600))
    }

    @Test
    fun `a failed report is not repeated until something new is offered`() {
        val o = MediaOutbox()
        val s = state()
        o.offer(s)
        val r = o.next(now)!!
        o.failed(r, now)
        assertNull(o.next(now + 5_000))
        assertEquals(0, o.waitMs(now + 5_000))
        o.offer(state(facts(updatedAt = now + 5_000, position = 47_000), now + 5_000))
        assertNotNull(o.next(now + 5_000), "a new offer after a failure is sent")
    }

    @Test
    fun `turning everything off takes back only what was shown`() {
        val o = MediaOutbox()
        o.step(state(), now)
        val r = o.step(null, now + 600)
        assertNotNull(r)
        assertNull(r!!.state)
        assertFalse(Media.encode(r.state, now).contains("title"))
    }
}
