package com.rimeos.remote.core.input

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PointerCurveTest {
    @Test
    fun `slow movement is close to one to one, a flick is several times faster`() {
        val c = PointerCurve()
        assertEquals(PointerCurve.SLOW_GAIN, c.gain(0.0), 1e-9)
        assertEquals(PointerCurve.SLOW_GAIN, c.gain(PointerCurve.SLOW_SPEED), 1e-9)
        assertEquals(PointerCurve.FAST_GAIN, c.gain(PointerCurve.FAST_SPEED), 1e-9)
        assertEquals(PointerCurve.FAST_GAIN, c.gain(50.0), 1e-9)
        assertTrue(PointerCurve.SLOW_GAIN in 0.8..1.5)
        assertTrue(PointerCurve.FAST_GAIN / PointerCurve.SLOW_GAIN >= 3)
    }

    @Test
    fun `the gain only ever rises with speed, with no step a hand could feel`() {
        val c = PointerCurve()
        var last = c.gain(0.0)
        var speed = 0.0
        while (speed < 3.0) {
            speed += 0.001
            val g = c.gain(speed)
            assertTrue(g >= last - 1e-12, "falls at $speed")
            assertTrue(g - last < 0.02, "steps at $speed")
            last = g
        }
    }

    @Test
    fun `a slow drag moves the pointer about as far as the finger, a fast one much further`() {
        val slow = PointerCurve()
        var px = 0.0
        // 100 dp over two seconds.
        for (i in 1..125) px += slow.move(0.8, 0.0, i * 16L).first
        assertEquals(100.0 * PointerCurve.SLOW_GAIN, px, 1.0)

        val fast = PointerCurve()
        var fx = 0.0
        // 100 dp in a tenth of a second: a flick.
        for (i in 1..6) fx += fast.move(100.0 / 6, 0.0, i * 16L).first
        assertTrue(fx > 3 * px, "$fx vs $px")

        // A brisk drag, 100 dp in a fifth of a second, sits between the two.
        val mid = PointerCurve()
        var mx = 0.0
        for (i in 1..12) mx += mid.move(100.0 / 12, 0.0, i * 16L).first
        assertTrue(mx > 2 * px && mx < fx, "$mx")
    }

    @Test
    fun `direction is kept exactly`() {
        val c = PointerCurve()
        val (x, y) = c.move(3.0, -4.0, 0)
        assertEquals(-4.0 / 3.0, y / x, 1e-12)
    }

    @Test
    fun `one jittery sample does not throw the pointer`() {
        val c = PointerCurve()
        for (i in 1..30) c.move(0.5, 0.0, i * 16L)
        // A sample that claims a huge speed (a timestamp one millisecond after the last).
        val (jx, _) = c.move(2.0, 0.0, 30 * 16L + 1)
        assertTrue(jx < 2.0 * PointerCurve.FAST_GAIN * 0.8, "$jx")
    }

    @Test
    fun `a pause starts a new movement`() {
        val c = PointerCurve()
        for (i in 1..20) c.move(20.0, 0.0, i * 16L)
        val (x, _) = c.move(0.5, 0.0, 20 * 16L + 500)
        assertEquals(0.5 * PointerCurve.SLOW_GAIN, x, 0.05)
    }

    @Test
    fun `the speed setting scales the whole curve`() {
        val c = PointerCurve(scale = 2.0)
        assertEquals(2 * PointerCurve.SLOW_GAIN, c.gain(0.0), 1e-9)
    }
}
