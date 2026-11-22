package com.rimeos.remote.core.input

import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min

/**
 * The pointer's acceleration: finger travel on the phone, in dp, to pointer
 * travel on the computer, in pixels.
 *
 * The computer applies none (a virtual pointer gets no libinput profile), so
 * this curve is the whole feel. Slow movement is close to one to one for
 * aiming at small things; a quick flick crosses a large screen. Between the
 * two the gain follows a smoothstep on the finger's speed, which has no
 * corner a hand can feel.
 *
 * Speed is smoothed across moves so one jittery sample does not throw the
 * pointer, and a pause long enough to be a new movement starts the
 * smoothing again. Pure: time comes in with each move.
 */
class PointerCurve(
    /** Gain at or below [slowSpeed] dp/ms. */
    private val slowGain: Double = SLOW_GAIN,
    /** Gain at or above [fastSpeed] dp/ms. */
    private val fastGain: Double = FAST_GAIN,
    private val slowSpeed: Double = SLOW_SPEED,
    private val fastSpeed: Double = FAST_SPEED,
    /** A multiplier the person chooses (Settings); 1.0 is the curve as drawn. */
    private val scale: Double = 1.0,
) {
    private var smoothed = 0.0
    private var lastAt: Long? = null

    /** The gain at a finger speed in dp per millisecond. */
    fun gain(speed: Double): Double {
        val t = ((speed - slowSpeed) / (fastSpeed - slowSpeed)).coerceIn(0.0, 1.0)
        val s = t * t * (3 - 2 * t)
        return (slowGain + (fastGain - slowGain) * s) * scale
    }

    /**
     * One move of [dx], [dy] dp, sampled at [atMs]: the pointer's move in
     * pixels. The first move of a gesture, or one after a long pause, has
     * no previous sample, so its speed is read from a nominal interval.
     */
    fun move(dx: Double, dy: Double, atMs: Long): Pair<Double, Double> {
        val dist = hypot(dx, dy)
        val prev = lastAt
        val dt = if (prev == null) NOMINAL_DT_MS else (atMs - prev).toDouble()
        lastAt = atMs
        val speed = dist / max(MIN_DT_MS, min(dt, MAX_DT_MS))
        smoothed = if (prev == null || dt > RESTART_MS) speed else smoothed + ALPHA * (speed - smoothed)
        val g = gain(smoothed)
        return dx * g to dy * g
    }

    /** The finger lifted: the next move starts a new movement. */
    fun reset() {
        lastAt = null
        smoothed = 0.0
    }

    companion object {
        const val SLOW_GAIN: Double = 1.1
        const val FAST_GAIN: Double = 5.0
        const val SLOW_SPEED: Double = 0.05
        const val FAST_SPEED: Double = 1.0

        private const val NOMINAL_DT_MS = 16.0
        private const val MIN_DT_MS = 4.0
        private const val MAX_DT_MS = 50.0
        private const val RESTART_MS = 120.0
        private const val ALPHA = 0.5
    }
}
