package com.rimeos.remote.core.input

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class TouchGesturesTest {
    private val left = MouseButton.LEFT

    /** A recorded run of touches: what each step returned, all in order. */
    private class Run(val g: TouchGestures = TouchGestures()) {
        val out = mutableListOf<Gesture>()
        fun down(id: Long, x: Double, y: Double, t: Long) = apply { out += g.down(id, x, y, t) }
        fun move(id: Long, x: Double, y: Double, t: Long) = apply { out += g.move(id, x, y, t) }
        fun up(id: Long, t: Long) = apply { out += g.up(id, t) }
        fun tick(t: Long) = apply { out += g.tick(t) }
        fun notMoves() = out.filter { it !is Gesture.Move }
    }

    @Test
    fun `a tap is a left click, once the time for a drag has passed`() {
        val r = Run().down(1, 100.0, 100.0, 0).up(1, 80)
        assertEquals(emptyList<Gesture>(), r.out)
        r.tick(80 + TouchGestures.DOUBLE_TAP_MS - 1)
        assertEquals(emptyList<Gesture>(), r.out)
        r.tick(80 + TouchGestures.DOUBLE_TAP_MS)
        assertEquals(listOf(Gesture.Click(left)), r.out)
        assertEquals(null, r.g.dueAt())
    }

    @Test
    fun `a slow press is not a tap`() {
        val r = Run().down(1, 0.0, 0.0, 0).up(1, TouchGestures.TAP_MS + 1).tick(5_000)
        assertEquals(emptyList<Gesture>(), r.out)
    }

    @Test
    fun `one finger moves the pointer, with nothing lost before the move counted`() {
        val r = Run().down(1, 0.0, 0.0, 0)
            .move(1, 1.0, 0.0, 10) // under the start distance: nothing yet
            .move(1, 5.0, 2.0, 20)
            .move(1, 9.0, 4.0, 30)
            .up(1, 400)
            .tick(5_000)
        val moves = r.out.filterIsInstance<Gesture.Move>()
        assertEquals(9.0, moves.sumOf { it.dx }, 1e-9)
        assertEquals(4.0, moves.sumOf { it.dy }, 1e-9)
        assertEquals(emptyList<Gesture>(), r.notMoves())
    }

    @Test
    fun `a moving tap is not a click`() {
        val r = Run().down(1, 0.0, 0.0, 0).move(1, 30.0, 0.0, 50).up(1, 100).tick(5_000)
        assertEquals(emptyList<Gesture>(), r.notMoves())
    }

    @Test
    fun `two fingers tapped together are a right click, three a middle click`() {
        val two = Run().down(1, 0.0, 0.0, 0).down(2, 40.0, 0.0, 20).up(1, 90).up(2, 100).tick(5_000)
        assertEquals(listOf(Gesture.Click(MouseButton.RIGHT)), two.out)
        val three = Run().down(1, 0.0, 0.0, 0).down(2, 40.0, 0.0, 10).down(3, 80.0, 0.0, 20)
            .up(1, 90).up(2, 95).up(3, 100).tick(5_000)
        assertEquals(listOf(Gesture.Click(MouseButton.MIDDLE)), three.out)
    }

    @Test
    fun `two fingers dragged scroll, and lifting them ends the scroll once`() {
        val r = Run().down(1, 0.0, 100.0, 0).down(2, 40.0, 100.0, 5)
            .move(1, 0.0, 90.0, 20).move(2, 40.0, 90.0, 20) // centre moved 10 dp
            .move(1, 0.0, 70.0, 40).move(2, 40.0, 70.0, 40)
            .up(1, 300).up(2, 310).tick(5_000)
        val scrolls = r.out.filterIsInstance<Gesture.Scroll>()
        assertEquals(-30.0, scrolls.sumOf { it.dy }, 1e-9)
        assertEquals(Gesture.ScrollEnd, r.out.last())
        assertEquals(1, r.out.count { it == Gesture.ScrollEnd })
        assertFalse(r.out.any { it is Gesture.Click })
    }

    @Test
    fun `tap then drag holds the left button for the drag and lets go on lift`() {
        val r = Run().down(1, 100.0, 100.0, 0).up(1, 60)
            .down(2, 104.0, 102.0, 150)
        assertEquals(listOf(Gesture.Press(left)), r.out)
        assertTrue(r.g.dragging)
        r.move(2, 140.0, 102.0, 200).move(2, 180.0, 102.0, 400).up(2, 900).tick(5_000)
        assertEquals(listOf(Gesture.Press(left), Gesture.Release(left)), r.notMoves())
        assertEquals(76.0, r.out.filterIsInstance<Gesture.Move>().sumOf { it.dx }, 1e-9)
        assertFalse(r.g.dragging)
    }

    @Test
    fun `two quick taps are a double click`() {
        val r = Run().down(1, 100.0, 100.0, 0).up(1, 60).down(2, 101.0, 100.0, 150).up(2, 200).tick(5_000)
        assertEquals(listOf(Gesture.Press(left), Gesture.Release(left), Gesture.Click(left)), r.out)
    }

    @Test
    fun `a second touch after the double-tap time is a click and a new touch`() {
        val r = Run().down(1, 100.0, 100.0, 0).up(1, 60).down(2, 100.0, 100.0, 60 + TouchGestures.DOUBLE_TAP_MS + 1)
        assertEquals(listOf(Gesture.Click(left)), r.out)
        assertFalse(r.g.dragging)
    }

    @Test
    fun `a second touch far away is not a drag`() {
        val r = Run().down(1, 0.0, 0.0, 0).up(1, 60).down(2, 300.0, 300.0, 100)
        assertEquals(listOf(Gesture.Click(left)), r.out)
        assertFalse(r.g.dragging)
    }

    @Test
    fun `cancelling a drag lets go of the button, cancelling a scroll ends it`() {
        val drag = Run().down(1, 0.0, 0.0, 0).up(1, 50).down(2, 0.0, 0.0, 100).move(2, 50.0, 0.0, 150)
        assertEquals(listOf(Gesture.Release(left)), drag.g.cancel())
        val scroll = Run().down(1, 0.0, 0.0, 0).down(2, 40.0, 0.0, 5).move(1, 0.0, 30.0, 20).move(2, 40.0, 30.0, 20)
        assertEquals(listOf(Gesture.ScrollEnd), scroll.g.cancel())
        val tap = Run().down(1, 0.0, 0.0, 0).up(1, 50)
        assertEquals(emptyList<Gesture>(), tap.g.cancel())
        assertEquals(emptyList<Gesture>(), tap.g.tick(5_000), "a cancelled tap is never clicked")
    }

    @Test
    fun `after a reset, fingers still down do nothing until they lift`() {
        val r = Run().down(1, 0.0, 0.0, 0).up(1, 50).down(2, 0.0, 0.0, 100).move(2, 50.0, 0.0, 150)
        r.g.reset(setOf(2))
        assertEquals(emptyList<Gesture>(), r.g.move(2, 90.0, 0.0, 200))
        assertEquals(emptyList<Gesture>(), r.g.up(2, 300))
        assertFalse(r.g.dragging)
        assertEquals(emptyList<Gesture>(), r.g.down(3, 0.0, 0.0, 400))
    }

    @Test
    fun `a finger resting beside a moving one does not turn the move into a scroll`() {
        val r = Run().down(1, 0.0, 0.0, 0).move(1, 20.0, 0.0, 20).down(2, 200.0, 200.0, 30)
            .move(1, 40.0, 0.0, 40).move(2, 200.0, 260.0, 40)
        assertTrue(r.out.all { it is Gesture.Move })
        assertEquals(40.0, r.out.filterIsInstance<Gesture.Move>().sumOf { it.dx }, 1e-9)
    }
}
