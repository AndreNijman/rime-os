package com.rimeos.remote.core.input

import kotlin.math.hypot

/** What a gesture on the pad means. Moves are in dp on the phone; the curve turns them into pixels. */
sealed class Gesture {
    data class Move(val dx: Double, val dy: Double, val atMs: Long) : Gesture()
    data class Click(val button: MouseButton) : Gesture()
    data class Press(val button: MouseButton) : Gesture()
    data class Release(val button: MouseButton) : Gesture()
    data class Scroll(val dx: Double, val dy: Double) : Gesture()
    data object ScrollEnd : Gesture()
}

/**
 * The pad's fingers, read as a touchpad: one finger moves the pointer, a tap
 * is a left click, two fingers tapped together a right click and three a
 * middle click, two fingers dragged scroll, and a tap followed at once by a
 * touch that moves holds the left button for the drag (lifting lets go;
 * a second tap without moving is a double click).
 *
 * A tap's click waits [DOUBLE_TAP_MS] to see whether a drag follows, as
 * touchpads on the computer do; [tick] delivers it when nothing did.
 *
 * Pure: positions in dp and times in milliseconds come in with each call,
 * and each call returns what the touch meant.
 */
class TouchGestures {
    private sealed class State {
        data object Idle : State()

        /** One finger down. [dragging] when it is the second touch of tap-and-drag (left held). */
        data class One(
            val id: Long,
            val startX: Double,
            val startY: Double,
            val lastX: Double,
            val lastY: Double,
            val downAt: Long,
            val moving: Boolean,
            val travel: Double,
            val dragging: Boolean,
        ) : State()

        /** A tap was made; its click waits to see whether a drag follows. */
        data class TapPending(val upAt: Long, val x: Double, val y: Double) : State()

        /** Two or more fingers. */
        data class Multi(
            val fingers: Map<Long, Pair<Double, Double>>,
            val most: Int,
            val downAt: Long,
            val startCx: Double,
            val startCy: Double,
            val lastCx: Double,
            val lastCy: Double,
            val scrolling: Boolean,
        ) : State()

        /** A gesture is over but fingers are still down: wait for them all to lift. */
        data class Drain(val fingers: Set<Long>) : State()
    }

    private var state: State = State.Idle

    /** Whether the pad holds the left button for a drag. */
    val dragging: Boolean get() = (state as? State.One)?.dragging == true

    /** When [tick] has something to deliver, or null. */
    fun dueAt(): Long? = (state as? State.TapPending)?.let { it.upAt + DOUBLE_TAP_MS }

    fun down(id: Long, x: Double, y: Double, at: Long): List<Gesture> {
        return when (val s = state) {
            State.Idle -> {
                state = State.One(id, x, y, x, y, at, moving = false, travel = 0.0, dragging = false)
                emptyList()
            }
            is State.TapPending -> {
                if (at - s.upAt <= DOUBLE_TAP_MS && hypot(x - s.x, y - s.y) <= DOUBLE_TAP_SLOP_DP) {
                    state = State.One(id, x, y, x, y, at, moving = false, travel = 0.0, dragging = true)
                    listOf(Gesture.Press(MouseButton.LEFT))
                } else {
                    state = State.One(id, x, y, x, y, at, moving = false, travel = 0.0, dragging = false)
                    listOf(Gesture.Click(MouseButton.LEFT))
                }
            }
            is State.One -> {
                if (s.dragging || s.moving) {
                    // A finger resting beside a moving one is not a new gesture.
                    emptyList()
                } else {
                    val fingers = mapOf(s.id to (s.lastX to s.lastY), id to (x to y))
                    val (cx, cy) = centre(fingers)
                    state = State.Multi(fingers, 2, s.downAt, cx, cy, cx, cy, scrolling = false)
                    emptyList()
                }
            }
            is State.Multi -> {
                if (s.scrolling) return emptyList()
                val fingers = s.fingers + (id to (x to y))
                val (cx, cy) = centre(fingers)
                state = s.copy(
                    fingers = fingers,
                    most = maxOf(s.most, fingers.size),
                    startCx = cx,
                    startCy = cy,
                    lastCx = cx,
                    lastCy = cy,
                )
                emptyList()
            }
            is State.Drain -> {
                state = State.Drain(s.fingers + id)
                emptyList()
            }
        }
    }

    fun move(id: Long, x: Double, y: Double, at: Long): List<Gesture> {
        return when (val s = state) {
            is State.One -> {
                if (id != s.id) return emptyList()
                val dx = x - s.lastX
                val dy = y - s.lastY
                val travel = maxOf(s.travel, hypot(x - s.startX, y - s.startY))
                if (!s.moving && travel < MOVE_START_DP) {
                    state = s.copy(travel = travel)
                    return emptyList()
                }
                // The first move carries what the finger travelled before it counted.
                val (mx, my) = if (!s.moving) (x - s.startX) to (y - s.startY) else dx to dy
                state = s.copy(lastX = x, lastY = y, moving = true, travel = travel)
                if (mx == 0.0 && my == 0.0) emptyList() else listOf(Gesture.Move(mx, my, at))
            }
            is State.Multi -> {
                if (id !in s.fingers) return emptyList()
                val fingers = s.fingers + (id to (x to y))
                val (cx, cy) = centre(fingers)
                if (!s.scrolling) {
                    if (fingers.size == 2 && hypot(cx - s.startCx, cy - s.startCy) >= SCROLL_START_DP) {
                        state = s.copy(fingers = fingers, lastCx = cx, lastCy = cy, scrolling = true)
                        return listOf(Gesture.Scroll(cx - s.startCx, cy - s.startCy))
                    }
                    state = s.copy(fingers = fingers)
                    return emptyList()
                }
                state = s.copy(fingers = fingers, lastCx = cx, lastCy = cy)
                val dx = cx - s.lastCx
                val dy = cy - s.lastCy
                if (dx == 0.0 && dy == 0.0) emptyList() else listOf(Gesture.Scroll(dx, dy))
            }
            else -> emptyList()
        }
    }

    fun up(id: Long, at: Long): List<Gesture> {
        return when (val s = state) {
            is State.One -> {
                if (id != s.id) return emptyList()
                val quick = at - s.downAt <= TAP_MS && s.travel <= TAP_SLOP_DP
                state = State.Idle
                when {
                    s.dragging && quick -> listOf(Gesture.Release(MouseButton.LEFT), Gesture.Click(MouseButton.LEFT))
                    s.dragging -> listOf(Gesture.Release(MouseButton.LEFT))
                    quick -> {
                        state = State.TapPending(at, s.startX, s.startY)
                        emptyList()
                    }
                    else -> emptyList()
                }
            }
            is State.Multi -> {
                if (id !in s.fingers) return emptyList()
                val rest = s.fingers.keys - id
                val out = when {
                    s.scrolling -> listOf(Gesture.ScrollEnd)
                    at - s.downAt <= MULTI_TAP_MS && s.most == 2 -> listOf(Gesture.Click(MouseButton.RIGHT))
                    at - s.downAt <= MULTI_TAP_MS && s.most == 3 -> listOf(Gesture.Click(MouseButton.MIDDLE))
                    else -> emptyList()
                }
                state = if (rest.isEmpty()) State.Idle else State.Drain(rest)
                out
            }
            is State.Drain -> {
                val rest = s.fingers - id
                state = if (rest.isEmpty()) State.Idle else State.Drain(rest)
                emptyList()
            }
            else -> emptyList()
        }
    }

    /** Time passed: a tap's click when no drag followed it. */
    fun tick(at: Long): List<Gesture> {
        val s = state as? State.TapPending ?: return emptyList()
        if (at - s.upAt < DOUBLE_TAP_MS) return emptyList()
        state = State.Idle
        return listOf(Gesture.Click(MouseButton.LEFT))
    }

    /**
     * The touch was taken away (the screen left, the system took the
     * gesture): let go of what the pad holds. A tap still waiting is not
     * clicked.
     */
    fun cancel(): List<Gesture> {
        val out = when (val s = state) {
            is State.One -> if (s.dragging) listOf(Gesture.Release(MouseButton.LEFT)) else emptyList()
            is State.Multi -> if (s.scrolling) listOf(Gesture.ScrollEnd) else emptyList()
            else -> emptyList()
        }
        state = State.Idle
        return out
    }

    /**
     * The computer let go of everything itself (locked, or its lease ran
     * out): forget the gesture without saying anything. Fingers still down
     * are ignored until they lift.
     */
    fun reset(fingersDown: Set<Long> = emptySet()) {
        state = if (fingersDown.isEmpty()) State.Idle else State.Drain(fingersDown)
    }

    private fun centre(fingers: Map<Long, Pair<Double, Double>>): Pair<Double, Double> {
        val n = fingers.size.toDouble()
        return fingers.values.sumOf { it.first } / n to fingers.values.sumOf { it.second } / n
    }

    companion object {
        const val MOVE_START_DP: Double = 3.0
        const val TAP_SLOP_DP: Double = 10.0
        const val DOUBLE_TAP_SLOP_DP: Double = 48.0
        const val SCROLL_START_DP: Double = 6.0
        const val TAP_MS: Long = 200
        const val MULTI_TAP_MS: Long = 260
        const val DOUBLE_TAP_MS: Long = 180
    }
}
