package com.rimeos.remote.core.input

/**
 * The computer's text keymap as this phone works it out, to count the
 * keymaps text will need before sending it (the computer allows
 * [Input.MAX_KEYMAPS_PER_SEC] and ends the channel past that).
 *
 * The computer types text through a keyboard of its own whose keymap holds
 * the characters typed so far ([InputBatcher.runs]); a run of text with a
 * character the keymap lacks needs a new one. Followed exactly while every
 * frame is applied, this count is exact.
 *
 * A locked computer drops frames, and this phone cannot tell which, so after
 * a pause any character this side counts as in the keymap may be missing
 * there: those are [doubt]. Text with one of them is counted as needing a
 * keymap; once typed, the computer has it. While the keymap has room for
 * everything, that is the most the computer can count. When it could fill
 * up (two hundred distinct characters), this phone stops following it
 * ([known] false) and counts each text at the most it could ever need.
 */
data class TextKeymap(val chars: List<Int>, val doubt: Set<Int>, val known: Boolean) {
    /** What text would cost: keymaps, the keymap after, and a shorter prefix that costs one fewer. */
    data class Cost(val keymaps: Int, val after: TextKeymap, val shorter: Int)

    /** After a pause: every character may be missing on the computer. */
    fun doubted(): TextKeymap = copy(doubt = chars.toSet())

    fun cost(text: List<Int>): Cost {
        if (!known) {
            // From any keymap: one for the first run, one for each run after (they start empty).
            val r = InputBatcher.runs(emptyList(), text)
            val shorter = if (r.size > 1) r[r.size - 2].second.last + 1 else 0
            return Cost(1 + r.size, this, shorter)
        }
        val adds = text.toSet() - chars.toSet()
        if (doubt.isNotEmpty()) {
            if (chars.size + adds.size > Input.KEYMAP_CHARS) {
                // It could fill: stop following it.
                return copy(known = false).cost(text)
            }
            val newChars = chars + text.distinct().filter { it in adds }
            val needs = if (adds.isNotEmpty() || text.any { it in doubt }) 1 else 0
            return Cost(needs, TextKeymap(newChars, doubt - text.toSet(), true), 0)
        }
        var m = chars
        var count = 0
        var lastNew = 0
        for ((km, range) in InputBatcher.runs(chars, text)) {
            if (km != m) {
                count++
                lastNew = range.first
                m = km
            }
        }
        return Cost(count, TextKeymap(m, emptySet(), true), lastNew)
    }

    companion object {
        val EMPTY: TextKeymap = TextKeymap(emptyList(), emptySet(), true)
    }
}
