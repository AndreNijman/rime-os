package com.rimeos.remote.core.input

import kotlin.math.abs

/**
 * What the touchpad has to say, gathered into frames the computer will
 * take.
 *
 * Events go into one queue in the order they happened. Moves next to each
 * other become one move, and so do scrolls, so a frame carries what a
 * display frame's worth of finger did, not every touch sample. [next] makes
 * the frame due now, if one is: within every limit in [Input] (events,
 * bytes, deltas, text length, frames, characters and keymaps a second), with
 * a sequence number one higher than the last. Anything that does not fit
 * waits in order for the next frame; nothing is ever dropped to fit and
 * nothing is sent that the computer would refuse.
 *
 * It also knows what is held (buttons, keys, a two-finger scroll in
 * progress), so it can keep the computer's lease alive with `hold` and let
 * go of everything when the touchpad leaves the screen.
 *
 * Pure: time comes in with every call that needs it.
 */
class InputBatcher {
    private val queue = ArrayDeque<InputEvent>()
    private val heldButtons = LinkedHashSet<MouseButton>()
    private val heldKeys = LinkedHashSet<Int>()
    private var scrolling = false

    // What the computer holds: what has been SENT pressed and not yet released.
    private val sentButtons = LinkedHashSet<MouseButton>()
    private val sentKeys = LinkedHashSet<Int>()
    private var sentScrolling = false

    private var carryMx = 0.0
    private var carryMy = 0.0
    private var carrySx = 0.0
    private var carrySy = 0.0

    private var seq = 0L
    private var lastSentAt: Long? = null

    private val frameTimes = ArrayDeque<Long>()
    private val textTimes = ArrayDeque<Pair<Long, Int>>()
    private val keymapTimes = ArrayDeque<Pair<Long, Int>>()

    /** The computer's text keymap as this phone works it out. */
    private var keymap: Keymap = Keymap.EMPTY

    /** The last sequence number sent; the first frame is 1. */
    val lastSeq: Long get() = seq

    /** Whether a button, key or scroll is held on the computer. */
    val holding: Boolean get() = heldButtons.isNotEmpty() || heldKeys.isNotEmpty()

    /** Whether events are waiting for a frame. */
    val pending: Boolean get() = queue.isNotEmpty()

    fun isHeld(button: MouseButton): Boolean = button in heldButtons

    fun isHeld(key: Int): Boolean = key in heldKeys

    fun motion(dx: Double, dy: Double) {
        if (!dx.isFinite() || !dy.isFinite() || (dx == 0.0 && dy == 0.0)) return
        val last = queue.lastOrNull()
        if (last is InputEvent.Motion) {
            queue[queue.lastIndex] = InputEvent.Motion(last.dx + dx, last.dy + dy)
        } else {
            queue.addLast(InputEvent.Motion(dx, dy))
        }
    }

    /** A two-finger scroll moved. */
    fun scroll(dx: Double, dy: Double) {
        if (!dx.isFinite() || !dy.isFinite() || (dx == 0.0 && dy == 0.0)) return
        scrolling = true
        val last = queue.lastOrNull()
        if (last is InputEvent.Scroll && last.finger && !last.stop) {
            queue[queue.lastIndex] = InputEvent.Scroll(last.dx + dx, last.dy + dy)
        } else {
            queue.addLast(InputEvent.Scroll(dx, dy))
        }
    }

    /** The fingers lifted from a scroll: the computer may carry it on kinetically. */
    fun scrollEnd() {
        if (!scrolling) return
        scrolling = false
        val last = queue.lastOrNull()
        if (last is InputEvent.Scroll && last.finger && !last.stop) {
            queue[queue.lastIndex] = last.copy(stop = true)
        } else {
            queue.addLast(InputEvent.Scroll(0.0, 0.0, stop = true))
        }
    }

    fun button(button: MouseButton, pressed: Boolean) {
        val changed = if (pressed) heldButtons.add(button) else heldButtons.remove(button)
        if (changed) queue.addLast(InputEvent.Button(button, pressed))
    }

    fun click(button: MouseButton) {
        if (button in heldButtons) return
        queue.addLast(InputEvent.Button(button, true))
        queue.addLast(InputEvent.Button(button, false))
    }

    fun key(code: Int, pressed: Boolean) {
        if (!Input.keyAllowed(code)) return
        val changed = if (pressed) heldKeys.add(code) else heldKeys.remove(code)
        if (changed) queue.addLast(InputEvent.Key(code, pressed))
    }

    fun tap(code: Int, mods: Set<Mod> = emptySet()) {
        if (!Input.keyAllowed(code)) return
        queue.addLast(InputEvent.Tap(code, mods))
    }

    /**
     * Committed text. Line endings become `\n` as the computer counts them;
     * any other control character is left out, since one would make the
     * computer refuse the whole frame.
     */
    fun text(s: String) {
        val clean = clean(s)
        if (clean.isEmpty()) return
        val last = queue.lastOrNull()
        if (last is InputEvent.Text) {
            queue[queue.lastIndex] = InputEvent.Text(last.s + clean)
        } else {
            queue.addLast(InputEvent.Text(clean))
        }
    }

    /**
     * The touchpad is leaving (another screen, the app in the background, the
     * channel closing): what is waiting is dropped, and the frame returned
     * lets go of everything the computer holds, by what was sent. Null when
     * it holds nothing. Sent even past this phone's own frame rate: one
     * frame more cannot reach the computer's.
     */
    fun letGo(now: Long): String? {
        queue.clear()
        heldButtons.clear()
        heldKeys.clear()
        scrolling = false
        val out = mutableListOf<InputEvent>()
        if (sentScrolling) out += InputEvent.Scroll(0.0, 0.0, stop = true)
        for (b in sentButtons) out += InputEvent.Button(b, false)
        for (k in sentKeys) out += InputEvent.Key(k, false)
        sentButtons.clear()
        sentKeys.clear()
        sentScrolling = false
        carrySx = 0.0
        carrySy = 0.0
        if (out.isEmpty()) return null
        seq += 1
        lastSentAt = now
        frameTimes.addLast(now)
        return Input.frame(seq, out)
    }

    /**
     * The computer paused (its screen locked): it has let go of everything
     * and drops what arrives until it resumes, so nothing waiting is worth
     * sending, and which text it typed is no longer known.
     */
    fun paused() {
        forget()
        keymap = keymap.doubted()
    }

    /**
     * A new channel: a new session on the computer, holding nothing, with an
     * empty text keymap. Sequence numbers keep growing; a new session takes
     * any first one.
     */
    fun fresh() {
        forget()
        keymap = Keymap.EMPTY
    }

    /** The computer let go of everything (its lease ran out): so does this phone. */
    fun released() = forget()

    private fun forget() {
        queue.clear()
        heldButtons.clear()
        heldKeys.clear()
        scrolling = false
        sentButtons.clear()
        sentKeys.clear()
        sentScrolling = false
    }

    /** When a frame will next be due if nothing new happens, for a timer, or null. */
    fun dueAt(now: Long): Long? = when {
        queue.isNotEmpty() -> now
        holding -> (lastSentAt ?: now) + Input.HOLD_EVERY_MS
        else -> null
    }

    /**
     * The frame due [now], encoded, or null when there is none: nothing
     * waiting and no `hold` due, or a limit reached (the events wait).
     */
    fun next(now: Long): String? {
        prune(now)
        if (frameTimes.size >= Input.FRAMES_PER_SEC) return null
        val out = mutableListOf<String>()
        var bytes = OVERHEAD
        var textLeft = Input.TEXT_PER_SEC - textTimes.sumOf { it.second }
        var keymapsLeft = Input.KEYMAPS_PER_SEC - keymapTimes.sumOf { it.second }
        var textSent = 0
        var keymapsSent = 0
        var map = keymap

        fun fits(encoded: String): Boolean {
            val add = encoded.toByteArray(Charsets.UTF_8).size + if (out.isEmpty()) 0 else 1
            if (bytes + add > Input.FRAME_BYTES) return false
            bytes += add
            out += encoded
            return true
        }

        build@ while (queue.isNotEmpty() && out.size < Input.MAX_EVENTS) {
            when (val e = queue.first()) {
                is InputEvent.Motion -> {
                    val tx = e.dx + carryMx
                    val ty = e.dy + carryMy
                    val px = tx.coerceIn(-Input.MAX_DELTA, Input.MAX_DELTA)
                    val py = ty.coerceIn(-Input.MAX_DELTA, Input.MAX_DELTA)
                    val sx = Input.hundredths(px)
                    val sy = Input.hundredths(py)
                    if (sx != 0.0 || sy != 0.0) {
                        if (!fits(InputEvent.Motion(sx, sy).json().toString())) break@build
                    }
                    carryMx = px - sx
                    carryMy = py - sy
                    val rx = tx - px
                    val ry = ty - py
                    if (rx != 0.0 || ry != 0.0) queue[0] = InputEvent.Motion(rx, ry) else queue.removeFirst()
                }
                is InputEvent.Scroll -> {
                    val tx = e.dx + carrySx
                    val ty = e.dy + carrySy
                    val px = tx.coerceIn(-Input.MAX_DELTA, Input.MAX_DELTA)
                    val py = ty.coerceIn(-Input.MAX_DELTA, Input.MAX_DELTA)
                    val sx = Input.hundredths(px)
                    val sy = Input.hundredths(py)
                    val rx = tx - px
                    val ry = ty - py
                    val more = rx != 0.0 || ry != 0.0
                    val stop = e.stop && !more
                    if (sx != 0.0 || sy != 0.0 || stop) {
                        if (!fits(InputEvent.Scroll(sx, sy, e.finger, stop).json().toString())) break@build
                        if (e.finger) sentScrolling = !stop
                    }
                    carrySx = px - sx
                    carrySy = py - sy
                    if (stop) {
                        carrySx = 0.0
                        carrySy = 0.0
                    }
                    if (more) queue[0] = e.copy(dx = rx, dy = ry) else queue.removeFirst()
                }
                is InputEvent.Text -> {
                    val cps = e.s.codePoints().toArray().toList()
                    var take = minOf(cps.size, Input.MAX_TEXT_CHARS, textLeft)
                    if (take <= 0) break@build
                    // As much as the keymaps left this second allow.
                    var cost = map.cost(cps.subList(0, take))
                    while (cost.keymaps > keymapsLeft && take > 0) {
                        take = cost.shorter
                        cost = map.cost(cps.subList(0, take))
                    }
                    if (take <= 0) break@build
                    // As much as the frame's bytes allow.
                    while (!fits(InputEvent.Text(text(cps, 0, take)).json().toString())) {
                        take /= 2
                        if (take == 0) break@build
                        cost = map.cost(cps.subList(0, take))
                    }
                    textLeft -= take
                    textSent += take
                    keymapsLeft -= cost.keymaps
                    keymapsSent += cost.keymaps
                    map = cost.after
                    if (take < cps.size) queue[0] = InputEvent.Text(text(cps, take, cps.size)) else queue.removeFirst()
                }
                else -> {
                    if (!fits(e.json().toString())) break@build
                    queue.removeFirst()
                    when (e) {
                        is InputEvent.Button -> if (e.pressed) sentButtons += e.button else sentButtons -= e.button
                        is InputEvent.Key -> if (e.pressed) sentKeys += e.code else sentKeys -= e.code
                        else -> {}
                    }
                }
            }
        }

        if (out.isEmpty()) {
            val due = holding && queue.isEmpty() && now - (lastSentAt ?: Long.MIN_VALUE / 2) >= Input.HOLD_EVERY_MS
            if (!due) return null
            out += InputEvent.Hold.json().toString()
        }
        seq += 1
        lastSentAt = now
        frameTimes.addLast(now)
        if (textSent > 0) textTimes.addLast(now to textSent)
        if (keymapsSent > 0) keymapTimes.addLast(now to keymapsSent)
        keymap = map
        return """{"seq":$seq,"ev":[${out.joinToString(",")}]}"""
    }

    private fun prune(now: Long) {
        while (frameTimes.isNotEmpty() && now - frameTimes.first() >= WINDOW_MS) frameTimes.removeFirst()
        while (textTimes.isNotEmpty() && now - textTimes.first().first >= WINDOW_MS) textTimes.removeFirst()
        while (keymapTimes.isNotEmpty() && now - keymapTimes.first().first >= WINDOW_MS) keymapTimes.removeFirst()
    }

    companion object {
        private const val WINDOW_MS = 1_000L

        /** `{"seq":<up to 19 digits>,"ev":[]}` */
        private const val OVERHEAD = 40

        fun clean(s: String): String {
            val unified = s.replace("\r\n", "\n").replace('\r', '\n')
            return unified.filter { !it.isISOControl() || it == '\n' || it == '\t' }
        }

        /**
         * How the computer types [chars] (code points, as Rust's chars) when
         * its keymap is [map]: its own
         * rule (`text_runs` in rime-remoted's input.rs), said again. Each run
         * of text gets the keymap before it with the run's new characters
         * added while there is room; when that is full, a keymap of the
         * run's own.
         */
        fun runs(map: List<Int>, chars: List<Int>): List<Pair<List<Int>, IntRange>> {
            val out = mutableListOf<Pair<List<Int>, IntRange>>()
            var cur = map.toMutableList()
            var i = 0
            while (i < chars.size) {
                val next = cur.toMutableList()
                var end = i
                while (end < chars.size) {
                    if (chars[end] !in next) {
                        if (next.size == Input.KEYMAP_CHARS) break
                        next.add(chars[end])
                    }
                    end++
                }
                if (end == i) {
                    cur = mutableListOf()
                    continue
                }
                out += next to (i until end)
                cur = next
                i = end
            }
            return out
        }

        /** Code points [from] until [to] as a string. */
        private fun text(cps: List<Int>, from: Int, to: Int): String {
            val b = StringBuilder()
            for (k in from until to) b.appendCodePoint(cps[k])
            return b.toString()
        }
    }
}

/** For the tests: the absolute size of a delta on the wire never passes the computer's limit. */
internal fun withinDelta(d: Double): Boolean = abs(d) <= Input.MAX_DELTA
