package com.rimeos.remote.core.input

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.double
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.math.abs

class InputBatcherTest {
    private fun JsonObject.d(name: String): Double = this[name]!!.jsonPrimitive.double

    /** Every frame [b] has to give from [from], [stepMs] apart, and the time after. */
    private fun drain(b: InputBatcher, from: Long = 0, stepMs: Long = 1, max: Int = 100_000): Pair<List<String>, Long> {
        val out = mutableListOf<String>()
        var t = from
        var guard = 0
        while (b.pending && guard++ < max) {
            b.next(t)?.let { out += it }
            t += stepMs
        }
        return out to t
    }

    @Test
    fun `moves in one display frame become one move, and the sums are kept to the hundredth`() {
        val b = InputBatcher()
        repeat(3) { b.motion(0.333, -0.111) }
        val f = b.next(0)!!
        val ev = Frames.events(f)
        assertEquals(1, ev.size)
        assertEquals(1.0, ev[0].d("dx"), 1e-9)
        assertEquals(-0.33, ev[0].d("dy"), 1e-9)
        assertNull(b.next(1))
    }

    @Test
    fun `sub-hundredth remainders are carried, so slow drift arrives in full`() {
        val b = InputBatcher()
        var sent = 0.0
        for (i in 0 until 1_000) {
            b.motion(0.004, 0.0)
            b.next(i * 10L)?.let { f -> sent += Frames.events(f).sumOf { it.d("dx") } }
        }
        assertEquals(4.0, sent, 0.011)
    }

    @Test
    fun `a move larger than the computer allows is split, never clamped away`() {
        val b = InputBatcher()
        b.motion(5_000.0, -4_500.5)
        val (frames, _) = drain(b)
        val ev = frames.flatMap(Frames::events)
        assertTrue(ev.all { abs(it.d("dx")) <= Input.MAX_DELTA && abs(it.d("dy")) <= Input.MAX_DELTA })
        assertEquals(5_000.0, ev.sumOf { it.d("dx") }, 0.01)
        assertEquals(-4_500.5, ev.sumOf { it.d("dy") }, 0.01)
        assertTrue(ev.size >= 3)
    }

    @Test
    fun `a frame never carries more than the computer's event limit`() {
        val b = InputBatcher()
        repeat(200) { b.tap(30) }
        val (frames, _) = drain(b)
        assertTrue(frames.all { Frames.events(it).size <= Input.MAX_EVENTS })
        assertEquals(200, frames.sumOf { Frames.events(it).size })
        assertEquals(Input.MAX_EVENTS, Frames.events(frames[0]).size)
    }

    @Test
    fun `sequence numbers start at one and always grow`() {
        val b = InputBatcher()
        repeat(500) { b.tap(30); b.motion(1.0, 1.0) }
        b.button(MouseButton.LEFT, true)
        val (frames, t) = drain(b)
        val holds = (1..5).mapNotNull { b.next(t + it * 300L) }
        val seqs = (frames + holds).map(Frames::seq)
        assertEquals(1L, seqs.first())
        assertEquals((1L..seqs.size).toList(), seqs)
    }

    @Test
    fun `no more frames in a second than this phone allows itself, well under the computer's limit`() {
        val b = InputBatcher()
        var sent = 0
        val times = mutableListOf<Long>()
        // A move every millisecond for three seconds.
        for (t in 0L until 3_000L) {
            b.motion(1.0, 0.0)
            if (b.next(t) != null) {
                sent++
                times += t
            }
        }
        for (i in times.indices) {
            val inWindow = times.count { it >= times[i] && it < times[i] + 1_000 }
            assertTrue(inWindow <= Input.FRAMES_PER_SEC, "$inWindow frames in a second from ${times[i]}")
        }
        // Two seconds bunched into one arrival window still pass the computer.
        for (i in times.indices) {
            val inTwo = times.count { it >= times[i] && it < times[i] + 2_000 }
            assertTrue(inTwo < Input.MAX_FRAMES_PER_SEC)
        }
        assertTrue(Input.FRAMES_PER_SEC * 2 < Input.MAX_FRAMES_PER_SEC)
        assertTrue(sent > 300)
    }

    @Test
    fun `text is split at the computer's length, never inside a character`() {
        val b = InputBatcher()
        val s = "😀".repeat(1_500) // 1,500 characters, 3,000 UTF-16 units
        b.text(s)
        val (frames, _) = drain(b)
        val texts = frames.flatMap(Frames::events).filter { Frames.t(it) == "text" }.map { it["s"]!!.jsonPrimitive.content }
        assertTrue(texts.all { it.codePointCount(0, it.length) <= Input.MAX_TEXT_CHARS })
        assertTrue(texts.all { !it.first().isLowSurrogate() && !it.last().isHighSurrogate() })
        assertEquals(s, texts.joinToString(""))
    }

    @Test
    fun `text per second stays under the computer's limit`() {
        val b = InputBatcher()
        b.text("a".repeat(9_000))
        val sentAt = mutableListOf<Pair<Long, Int>>()
        var t = 0L
        while (b.pending && t < 20_000) {
            b.next(t)?.let { f ->
                val n = Frames.events(f).filter { Frames.t(it) == "text" }.sumOf { it["s"]!!.jsonPrimitive.content.length }
                sentAt += t to n
            }
            t += 5
        }
        assertFalse(b.pending)
        for ((start, _) in sentAt) {
            val inWindow = sentAt.filter { it.first >= start && it.first < start + 1_000 }.sumOf { it.second }
            assertTrue(inWindow <= Input.TEXT_PER_SEC, "$inWindow characters in a second")
        }
        assertEquals(9_000, sentAt.sumOf { it.second })
    }

    @Test
    fun `text needing many keymaps is held back to the keymaps a second this phone allows`() {
        val b = InputBatcher()
        // 4,000 distinct characters: each run of 200 needs its own keymap.
        val s = buildString { for (i in 0 until 4_000) appendCodePoint(0x4E00 + i) }
        b.text(s)
        var map: List<Int> = emptyList()
        val keymapsAt = mutableListOf<Pair<Long, Int>>()
        var t = 0L
        val typed = StringBuilder()
        while (b.pending && t < 60_000) {
            b.next(t)?.let { f ->
                var n = 0
                for (e in Frames.events(f).filter { Frames.t(it) == "text" }) {
                    val str = e["s"]!!.jsonPrimitive.content
                    typed.append(str)
                    // The computer's own count.
                    for ((km, _) in InputBatcher.runs(map, str.codePoints().toArray().toList())) {
                        if (km != map) {
                            n++
                            map = km
                        }
                    }
                }
                if (n > 0) keymapsAt += t to n
            }
            t += 5
        }
        assertEquals(s, typed.toString())
        for ((start, _) in keymapsAt) {
            val inWindow = keymapsAt.filter { it.first >= start && it.first < start + 1_000 }.sumOf { it.second }
            assertTrue(inWindow <= Input.KEYMAPS_PER_SEC, "$inWindow keymaps in a second")
        }
    }

    private fun cps(s: String) = s.codePoints().toArray().toList()

    @Test
    fun `the keymap count follows the computer's while every frame is applied`() {
        val a = TextKeymap.EMPTY.cost(cps("hello"))
        assertEquals(1, a.keymaps)
        assertEquals(0, a.after.cost(cps("hole")).keymaps)
        assertEquals(1, a.after.cost(cps("help")).keymaps)
        assertEquals(2, TextKeymap(emptyList(), emptySet(), known = false).cost(cps("hello")).keymaps)
    }

    @Test
    fun `after a pause a character the computer may have missed counts as a new keymap, once`() {
        val k = TextKeymap.EMPTY.cost(cps("hello")).after.doubted()
        val first = k.cost(cps("hole"))
        assertEquals(1, first.keymaps)
        assertEquals(0, first.after.cost(cps("hello")).keymaps)
    }

    /** The computer's own count for one text, from its keymap [map]. */
    private fun serverCost(map: MutableList<Int>, text: List<Int>): Int {
        var n = 0
        for ((km, _) in InputBatcher.runs(map, text)) {
            if (km != map) {
                n++
                map.clear()
                map.addAll(km)
            }
        }
        return n
    }

    @Test
    fun `whatever a pause dropped, this phone never counts fewer keymaps than the computer`() {
        val rnd = java.util.Random(7)
        repeat(300) { round ->
            val alphabet = if (round % 3 == 0) 400 else 40
            var mine = TextKeymap.EMPTY
            val server = mutableListOf<Int>()
            val sent = mutableListOf<List<Int>>()
            repeat(60) { step ->
                val text = List(1 + rnd.nextInt(6)) { 0x100 + rnd.nextInt(alphabet) }
                if (step == 20) {
                    // The computer locked somewhere in what was sent: it applied a prefix only.
                    val cut = rnd.nextInt(sent.size + 1)
                    server.clear()
                    for (t in sent.take(cut)) serverCost(server, t)
                    mine = mine.doubted()
                    sent.clear()
                }
                val c = mine.cost(text)
                val truth = serverCost(server, text)
                assertTrue(truth <= c.keymaps, "round $round step $step: computer $truth, counted ${c.keymaps}")
                mine = c.after
                sent += text
                if (step < 20) {
                    // Before the pause the computer applies everything.
                    sent.clear()
                    server.clear()
                    // Rebuild from the exact mirror: identical while nothing is dropped.
                    server.addAll(mine.chars)
                }
            }
        }
    }

    @Test
    fun `text over the keymaps left waits for the next second`() {
        val b = InputBatcher()
        // Eleven texts each with a character never typed before: one keymap each.
        for (i in 0 until 11) {
            b.text(String(Character.toChars(0x4E00 + i)))
            b.tap(30)
        }
        val f = b.next(0)!!
        assertEquals(Input.KEYMAPS_PER_SEC, Frames.events(f).count { Frames.t(it) == "text" })
        assertTrue(b.pending)
        assertNull(b.next(999))
        assertNotNull(b.next(1_000))
    }

    @Test
    fun `a frame never passes this phone's byte limit`() {
        val b = InputBatcher()
        // A second's worth of characters that take four bytes each (two
        // text events of the computer's longest), then taps carrying every
        // modifier (a three-digit code): under the event and text limits,
        // over the byte budget.
        b.text("😀".repeat(Input.TEXT_PER_SEC))
        repeat(Input.MAX_EVENTS) { b.tap(248, setOf(Mod.CTRL, Mod.SHIFT, Mod.ALT, Mod.SUPER, Mod.ALTGR)) }
        val (frames, _) = drain(b)
        val sizes = frames.map { it.toByteArray().size }
        assertTrue(sizes.all { it <= Input.FRAME_BYTES }, "every frame within the budget: $sizes")
        assertTrue(sizes.max() > Input.FRAME_BYTES - 200, "the budget was reached, so it was what split them: $sizes")
        assertEquals(Input.MAX_EVENTS, frames.sumOf { f -> Frames.events(f).count { Frames.t(it) == "tap" } })
        assertTrue(Input.FRAME_BYTES < Input.MAX_FRAME_BYTES)
    }

    @Test
    fun `control characters never reach a frame`() {
        val b = InputBatcher()
        b.text("a\u0000b\u001b[c\r\nd\te\u007f")
        val ev = Frames.events(b.next(0)!!)
        assertEquals("ab[c\nd\te", ev.single()["s"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a held button gets a hold every quarter second, so the computer never lets go`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, true)
        assertNotNull(b.next(0))
        val sentAt = mutableListOf(0L)
        for (t in 1L..5_000L) {
            b.next(t)?.let { f ->
                assertEquals(listOf("hold"), Frames.events(f).map(Frames::t))
                sentAt += t
            }
        }
        val gaps = sentAt.zipWithNext { a, c -> c - a }
        assertTrue(gaps.all { it <= Input.HOLD_EVERY_MS }, "$gaps")
        assertTrue(gaps.max() * 3 <= Input.LEASE_MS)
        assertEquals(Input.HOLD_EVERY_MS, b.dueAt(sentAt.last())!! - sentAt.last())
    }

    @Test
    fun `nothing held means no holds`() {
        val b = InputBatcher()
        b.click(MouseButton.LEFT)
        assertNotNull(b.next(0))
        for (t in 1L..2_000L) assertNull(b.next(t))
        assertNull(b.dueAt(2_000))
    }

    @Test
    fun `letting go releases every button and key the computer holds, and ends a scroll`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, true)
        b.key(KeyMap.LEFT_CTRL, true)
        b.key(30, true)
        b.scroll(0.0, 5.0)
        b.next(0)
        // Waiting, never sent: dropped, and nothing to release for it.
        b.button(MouseButton.RIGHT, true)
        b.text("unsent")
        val f = b.letGo(1)!!
        val ev = Frames.events(f)
        assertEquals(
            listOf("s" to null, "b" to false, "k" to false, "k" to false),
            ev.map { Frames.t(it) to it["p"]?.jsonPrimitive?.boolean },
        )
        assertTrue(ev[0]["stop"]!!.jsonPrimitive.boolean)
        assertEquals(2L, Frames.seq(f))
        assertFalse(b.holding)
        assertFalse(b.pending)
        assertNull(b.letGo(2), "nothing held: nothing to send")
    }

    @Test
    fun `letting go goes even when this phone's own frame rate is used up`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, true)
        var t = 0L
        while (t < 1_000) {
            b.motion(1.0, 0.0)
            b.next(t)
            t++
        }
        b.motion(1.0, 0.0)
        assertNull(b.next(999))
        assertNotNull(b.letGo(999))
    }

    @Test
    fun `a press released before it was sent leaves nothing to let go`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, true)
        b.button(MouseButton.LEFT, false)
        b.next(0)
        assertNull(b.letGo(1))
    }

    @Test
    fun `a scroll ends with stop, and its moves merge`() {
        val b = InputBatcher()
        b.scroll(0.0, 3.0)
        b.scroll(0.0, 4.0)
        b.scrollEnd()
        val ev = Frames.events(b.next(0)!!)
        assertEquals(1, ev.size)
        assertEquals(7.0, ev[0].d("dy"), 1e-9)
        assertEquals("finger", ev[0]["src"]!!.jsonPrimitive.content)
        assertTrue(ev[0]["stop"]!!.jsonPrimitive.boolean)
    }

    @Test
    fun `a paused computer let go itself, so nothing waiting is sent`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, true)
        b.next(0)
        b.tap(30)
        b.paused()
        assertFalse(b.holding)
        assertFalse(b.pending)
        assertNull(b.next(500))
    }

    @Test
    fun `a press already held and a release of nothing are not sent`() {
        val b = InputBatcher()
        b.button(MouseButton.LEFT, false)
        b.key(30, false)
        assertFalse(b.pending)
        b.button(MouseButton.LEFT, true)
        b.button(MouseButton.LEFT, true)
        assertEquals(1, Frames.events(b.next(0)!!).size)
    }

    @Test
    fun `forbidden keys are never queued`() {
        val b = InputBatcher()
        b.key(116, true)
        b.tap(142)
        assertFalse(b.pending)
    }

    @Test
    fun `every event in a long mixed run stays in order`() {
        val b = InputBatcher()
        b.tap(30)
        b.text("ab")
        b.tap(31)
        b.text("cd")
        val ev = Frames.events(b.next(0)!!)
        assertEquals(listOf("tap", "text", "tap", "text"), ev.map(Frames::t))
        assertEquals(30, ev[0]["code"]!!.jsonPrimitive.int)
    }
}
