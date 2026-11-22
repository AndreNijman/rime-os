package com.rimeos.remote.core.input

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.double
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class TouchpadInputTest {
    private fun frames(t: TouchpadInput, now: Long): List<JsonObject> {
        val out = mutableListOf<JsonObject>()
        var at = now
        while (true) {
            val f = t.next(at) ?: break
            out += Frames.events(f)
            at += 1
        }
        return out
    }

    private fun JsonObject.brief(): String = when (Frames.t(this)) {
        "b" -> "b ${this["b"]!!.jsonPrimitive.content} ${this["p"]!!.jsonPrimitive.boolean}"
        "k" -> "k ${this["code"]!!.jsonPrimitive.int} ${this["p"]!!.jsonPrimitive.boolean}"
        "tap" -> "tap ${this["code"]!!.jsonPrimitive.int}" +
            (this["mods"]?.jsonArray?.joinToString(",", " ") { it.jsonPrimitive.content } ?: "")
        "text" -> "text ${this["s"]!!.jsonPrimitive.content}"
        "s" -> "s ${this["dy"]!!.jsonPrimitive.double} ${this["stop"]!!.jsonPrimitive.boolean}"
        else -> Frames.t(this)
    }

    @Test
    fun `leaving lets go of a drag, the buttons and the keys before the channel closes`() {
        val t = TouchpadInput()
        // A tap-and-drag holding left, the right button held on the bar, a hardware key held.
        t.down(1, 0.0, 0.0, 0)
        t.up(1, 50)
        t.down(2, 0.0, 0.0, 100)
        t.move(2, 30.0, 0.0, 120)
        t.button(MouseButton.RIGHT, true)
        t.hardware(29, pressed = true, repeat = false) // A
        frames(t, 130)
        assertTrue(t.batcher.holding)
        t.leave()
        val after = frames(t, 200).map { it.brief() }
        assertEquals(setOf("b left false", "b right false", "k 30 false"), after.toSet())
        assertFalse(t.batcher.holding)
    }

    @Test
    fun `a tap clicks, a two-finger tap right-clicks`() {
        val t = TouchpadInput()
        t.down(1, 0.0, 0.0, 0)
        t.up(1, 50)
        t.tick(50 + TouchGestures.DOUBLE_TAP_MS)
        t.down(1, 0.0, 0.0, 1_000)
        t.down(2, 30.0, 0.0, 1_010)
        t.up(1, 1_080)
        t.up(2, 1_090)
        assertEquals(
            listOf("b left true", "b left false", "b right true", "b right false"),
            frames(t, 2_000).map { it.brief() },
        )
    }

    @Test
    fun `natural scrolling moves the content with the fingers`() {
        val natural = TouchpadInput(natural = true)
        val wheel = TouchpadInput(natural = false)
        for (t in listOf(natural, wheel)) {
            t.down(1, 0.0, 100.0, 0)
            t.down(2, 40.0, 100.0, 5)
            t.move(1, 0.0, 80.0, 20)
            t.move(2, 40.0, 80.0, 20)
            t.up(1, 300)
            t.up(2, 300)
        }
        val n = frames(natural, 400)
        val w = frames(wheel, 400)
        // Fingers moved up (dy < 0): natural scrolls down the page (positive axis), a wheel the other way.
        assertTrue(n.sumOf { it["dy"]!!.jsonPrimitive.double } > 0)
        assertTrue(w.sumOf { it["dy"]!!.jsonPrimitive.double } < 0)
        assertTrue(n.last()["stop"]!!.jsonPrimitive.boolean)
    }

    @Test
    fun `a sticky modifier goes with the next key only, a locked one until tapped again`() {
        val t = TouchpadInput()
        t.toggle(Mod.CTRL)
        t.typed(TypedText.read("${TypedText.SENTINEL}c", null))
        t.typed(TypedText.read("${TypedText.SENTINEL}c", null))
        t.toggle(Mod.ALT)
        t.toggle(Mod.ALT) // locked
        t.tap(KeyMap.LEFT)
        t.tap(KeyMap.RIGHT)
        t.toggle(Mod.ALT) // off
        t.tap(KeyMap.UP)
        assertEquals(
            listOf("tap 46 ctrl", "text c", "tap 105 alt", "tap 106 alt", "tap 103"),
            frames(t, 0).map { it.brief() },
        )
    }

    @Test
    fun `a shifted character with a modifier armed sends shift too`() {
        val t = TouchpadInput()
        t.toggle(Mod.CTRL)
        t.typed(TypedText.read("${TypedText.SENTINEL}T", null))
        assertEquals(listOf("tap 20 ctrl,shift"), frames(t, 0).map { it.brief() })
    }

    @Test
    fun `a click with a modifier armed holds the modifier around the click`() {
        val t = TouchpadInput()
        t.toggle(Mod.CTRL)
        t.down(1, 0.0, 0.0, 0)
        t.up(1, 50)
        t.tick(1_000)
        assertEquals(
            listOf("k 29 true", "b left true", "b left false", "k 29 false"),
            frames(t, 1_000).map { it.brief() },
        )
        assertEquals(emptySet<Mod>(), t.armed)
    }

    @Test
    fun `backspace and enter from the soft keyboard are key taps`() {
        val t = TouchpadInput()
        t.typed(TypedText.read("", null))
        t.typed(TypedText.read("${TypedText.SENTINEL}ok\n", null))
        assertEquals(listOf("tap 14", "text ok", "tap 28"), frames(t, 0).map { it.brief() })
    }

    @Test
    fun `hardware keys press and release, android's repeats are not sent, phone keys are left alone`() {
        val t = TouchpadInput()
        assertTrue(t.hardware(29, true, false))
        assertTrue(t.hardware(29, true, true))
        assertTrue(t.hardware(29, false, false))
        assertFalse(t.hardware(24, true, false), "volume up stays with the phone")
        assertFalse(t.hardware(4, true, false), "back stays with the phone")
        assertEquals(listOf("k 30 true", "k 30 false"), frames(t, 0).map { it.brief() })
    }

    @Test
    fun `a pause forgets what was held without sending releases the computer already did`() {
        val t = TouchpadInput()
        t.button(MouseButton.LEFT, true)
        frames(t, 0)
        t.paused()
        assertEquals(emptyList<String>(), frames(t, 1_000).map { it.brief() })
        assertFalse(t.batcher.holding)
    }
}
