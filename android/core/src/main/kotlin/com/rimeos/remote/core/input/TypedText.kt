package com.rimeos.remote.core.input

/**
 * The hidden text field the soft keyboard types into, read as the computer
 * should see it: committed text only, never what the keyboard is still
 * composing (a word it may yet change, or a suggestion the person never
 * chose).
 *
 * The field holds [SENTINEL] and, after it, the text still being composed.
 * Committed text is taken out the moment it appears. The sentinel is there
 * because some keyboards answer Backspace in an empty field with nothing at
 * all; with one character to delete they delete it, and that is read as a
 * Backspace.
 *
 * Nothing here keeps text: each [read] returns what to send and what the
 * field should hold next, and the caller sends it and forgets it.
 */
object TypedText {
    /** A zero-width space: nothing to see, nothing a keyboard capitalises after. */
    const val SENTINEL: Char = '​'

    /** What an empty field holds. */
    val EMPTY: Field = Field(SENTINEL.toString(), 1, null)

    /** The field's text, cursor and composing range (start inclusive, end exclusive). */
    data class Field(val text: String, val cursor: Int, val composing: IntRange?)

    /** What one change of the field means. */
    data class Read(
        /** Backspaces to send before [typed]. */
        val backspaces: Int,
        /** Committed text, in order. */
        val typed: String,
        /** What the field holds next. */
        val next: Field,
    )

    /**
     * The field after the keyboard changed it to [text] with [composing]
     * (null when nothing is being composed).
     */
    fun read(text: String, composing: IntRange?): Read {
        val hasSentinel = text.startsWith(SENTINEL)
        val backspaces = if (hasSentinel) 0 else 1
        val offset = if (hasSentinel) 1 else 0
        val body = text.substring(offset)
        val comp = composing
            ?.let { (it.first - offset)..(it.last - offset) }
            ?.takeIf { it.first >= 0 && it.last < body.length && !it.isEmpty() }
        val typed: String
        val still: String
        if (comp == null) {
            typed = body
            still = ""
        } else {
            typed = body.substring(0, comp.first) + body.substring(comp.last + 1)
            still = body.substring(comp.first, comp.last + 1)
        }
        val clean = typed.filter { it != SENTINEL }
        val shown = SENTINEL + still.filter { it != SENTINEL }
        val next = Field(shown, shown.length, if (shown.length > 1) 1 until shown.length else null)
        return Read(backspaces, clean, next)
    }

    /**
     * Committed text as events: a line break is the Enter key and a tab the
     * Tab key, as on the computer's own keyboard; other control characters
     * are dropped (the computer refuses a frame carrying one); the rest is
     * text.
     */
    fun events(typed: String): List<InputEvent> {
        val out = mutableListOf<InputEvent>()
        val run = StringBuilder()
        fun flush() {
            if (run.isNotEmpty()) {
                out += InputEvent.Text(run.toString())
                run.clear()
            }
        }
        var i = 0
        while (i < typed.length) {
            val c = typed[i]
            when {
                c == '\r' && i + 1 < typed.length && typed[i + 1] == '\n' -> {}
                c == '\n' || c == '\r' -> {
                    flush()
                    out += InputEvent.Tap(KeyMap.ENTER)
                }
                c == '\t' -> {
                    flush()
                    out += InputEvent.Tap(KeyMap.TAB)
                }
                c.isISOControl() || c == SENTINEL -> {}
                else -> run.append(c)
            }
            i++
        }
        flush()
        return out
    }
}
