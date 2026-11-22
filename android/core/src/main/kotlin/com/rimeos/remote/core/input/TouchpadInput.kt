package com.rimeos.remote.core.input

/**
 * Everything the touchpad screen turns into input, in one place: fingers on
 * the pad (through [TouchGestures] and the [PointerCurve]), the buttons
 * along its bottom, the key strip with its sticky modifiers, text from the
 * soft keyboard and keys from a hardware one, all into one [InputBatcher].
 *
 * The screen calls in on the main thread and takes frames out with
 * [next]; nothing here touches the network or a clock.
 */
class TouchpadInput(
    /** Content follows the fingers, as on a phone; off, it moves as a scroll wheel would. */
    var natural: Boolean = true,
    speed: Double = 1.0,
) {
    val batcher = InputBatcher()
    private val gestures = TouchGestures()
    private var curve = PointerCurve(scale = speed)

    /** Modifiers armed for the next key or click. */
    var armed: Set<Mod> = emptySet()
        private set

    /** Modifiers kept armed until tapped again (a modifier tapped twice). */
    var locked: Set<Mod> = emptySet()
        private set

    private val downs = mutableSetOf<Long>()

    fun speed(scale: Double) {
        curve = PointerCurve(scale = scale)
    }

    /** Whether the pad holds the left button for a drag. */
    val dragging: Boolean get() = gestures.dragging

    /** When something is next due without new input: a waiting tap's click, or a `hold`. */
    fun dueAt(now: Long): Long? = listOfNotNull(gestures.dueAt(), batcher.dueAt(now)).minOrNull()

    fun down(id: Long, x: Double, y: Double, at: Long): List<Gesture> {
        downs += id
        return apply(gestures.down(id, x, y, at))
    }

    fun move(id: Long, x: Double, y: Double, at: Long): List<Gesture> = apply(gestures.move(id, x, y, at))

    fun up(id: Long, at: Long): List<Gesture> {
        downs -= id
        val out = apply(gestures.up(id, at))
        if (downs.isEmpty()) curve.reset()
        return out
    }

    fun tick(at: Long): List<Gesture> = apply(gestures.tick(at))

    /** One of the buttons along the pad's bottom went down or up. */
    fun button(button: MouseButton, pressed: Boolean) {
        if (pressed) {
            pressMods()
            batcher.button(button, true)
        } else {
            batcher.button(button, false)
            releaseMods()
        }
    }

    /** A modifier on the key strip: armed for the next key, locked when tapped twice, then off. */
    fun toggle(mod: Mod) {
        when (mod) {
            in locked -> {
                locked = locked - mod
                armed = armed - mod
            }
            in armed -> locked = locked + mod
            else -> armed = armed + mod
        }
    }

    /** A key on the strip (or Backspace and Enter from the soft keyboard): one tap, with what is armed. */
    fun tap(code: Int) {
        batcher.tap(code, armed)
        disarm()
    }

    /** A change of the hidden text field. */
    fun typed(read: TypedText.Read) {
        repeat(read.backspaces) { tap(KeyMap.BACKSPACE) }
        if (read.typed.isEmpty()) return
        val events = TypedText.events(read.typed)
        for (e in events) {
            when (e) {
                is InputEvent.Tap -> tap(e.code)
                is InputEvent.Text -> text(e.s)
                else -> {}
            }
        }
    }

    private fun text(s: String) {
        if (armed.isEmpty()) {
            batcher.text(s)
            return
        }
        // With a modifier armed the computer needs keys (Ctrl+C), not text.
        for (c in s) {
            val k = KeyMap.char(c)
            if (k == null || armed.isEmpty()) {
                batcher.text(c.toString())
            } else {
                batcher.tap(k.code, if (k.shift) armed + Mod.SHIFT else armed)
                disarm()
            }
        }
    }

    /**
     * A key from a hardware keyboard: pressed and released as it is, so the
     * computer repeats a held key itself. Android's own repeats are not
     * sent. True when the key was taken.
     */
    fun hardware(keyCode: Int, pressed: Boolean, repeat: Boolean): Boolean {
        val code = KeyMap.android(keyCode) ?: return false
        if (repeat) return true
        batcher.key(code, pressed)
        return true
    }

    /**
     * The screen is leaving or the channel is closing: the frame that lets go
     * of everything the pad, the buttons and the keyboard hold on the
     * computer, to send before the channel goes; null when nothing is held.
     */
    fun leave(now: Long): String? {
        gestures.cancel()
        pressedMods = emptySet()
        armed = emptySet()
        locked = emptySet()
        downs.clear()
        curve.reset()
        return batcher.letGo(now)
    }

    /** The computer paused (locked): it let go of everything already. */
    fun paused() {
        gestures.reset(downs.toSet())
        batcher.paused()
        pressedMods = emptySet()
    }

    /** The computer's lease ran out: it let go of everything already. */
    fun released() {
        gestures.reset(downs.toSet())
        batcher.released()
        pressedMods = emptySet()
    }

    /** The frame due now, or null. */
    fun next(now: Long): String? = batcher.next(now)

    private fun apply(gs: List<Gesture>): List<Gesture> {
        for (g in gs) {
            when (g) {
                is Gesture.Move -> {
                    val (px, py) = curve.move(g.dx, g.dy, g.atMs)
                    batcher.motion(px, py)
                }
                is Gesture.Scroll -> {
                    val sign = if (natural) -1.0 else 1.0
                    batcher.scroll(sign * g.dx * SCROLL_GAIN, sign * g.dy * SCROLL_GAIN)
                }
                Gesture.ScrollEnd -> batcher.scrollEnd()
                is Gesture.Click -> {
                    pressMods()
                    batcher.click(g.button)
                    releaseMods()
                }
                is Gesture.Press -> {
                    pressMods()
                    batcher.button(g.button, true)
                }
                is Gesture.Release -> {
                    batcher.button(g.button, false)
                    releaseMods()
                }
            }
        }
        return gs
    }

    private var pressedMods: Set<Mod> = emptySet()

    private fun pressMods() {
        if (armed.isEmpty() || pressedMods.isNotEmpty()) return
        pressedMods = armed
        for (m in pressedMods) batcher.key(KeyMap.of(m), true)
    }

    private fun releaseMods() {
        if (pressedMods.isEmpty()) return
        for (m in pressedMods) batcher.key(KeyMap.of(m), false)
        pressedMods = emptySet()
        disarm()
    }

    private fun disarm() {
        armed = locked
    }

    companion object {
        /** Scroll travel on the computer for each dp a finger travels. */
        const val SCROLL_GAIN: Double = 1.2
    }
}
