package com.rimeos.remote.core.term

/**
 * One line of cells.
 *
 * Four parallel `IntArray`s rather than an array of [Cell]: see [Colour] for
 * the arithmetic. [wrapped] is the fifth field and the one that is easy to
 * leave out — it records that this line ran off the right-hand edge and
 * continues on the next one, which is the difference between copying a
 * paragraph out of a terminal and copying it out with a newline every eighty
 * columns.
 */
class Line(cols: Int) {
    var code: IntArray = IntArray(cols) { SPACE }
        private set
    var fg: IntArray = IntArray(cols)
        private set
    var bg: IntArray = IntArray(cols)
        private set
    var attrs: IntArray = IntArray(cols)
        private set

    /** True when this line continues onto the next because it filled the width. */
    var wrapped: Boolean = false

    val cols: Int get() = code.size

    fun set(col: Int, code: Int, fg: Int, bg: Int, attrs: Int) {
        if (col !in this.code.indices) return
        this.code[col] = code
        this.fg[col] = fg
        this.bg[col] = bg
        this.attrs[col] = attrs
    }

    fun cellAt(col: Int): Cell =
        if (col !in code.indices) Cell(SPACE)
        else Cell(code[col], fg[col], bg[col], attrs[col])

    /**
     * Blank a run, painting it with [bg] rather than with the default.
     *
     * The background matters and is the usual bug: `ED` and `EL` erase *with
     * the current background colour*, which is how a TUI paints a coloured
     * panel by moving the cursor and clearing to the end of the line. Erasing
     * to the default background would leave the panel striped.
     */
    fun clear(from: Int, to: Int, bg: Int) {
        for (i in maxOf(0, from)..minOf(cols - 1, to)) {
            code[i] = SPACE
            fg[i] = Colour.DEFAULT
            this.bg[i] = bg
            attrs[i] = Attrs.NONE
        }
    }

    /** Insert [n] blank cells at [col], pushing the tail right and off the edge. */
    fun insert(col: Int, n: Int, bg: Int) {
        if (n <= 0 || col >= cols) return
        val count = minOf(n, cols - col)
        val move = cols - col - count
        if (move > 0) {
            System.arraycopy(code, col, code, col + count, move)
            System.arraycopy(this.fg, col, this.fg, col + count, move)
            System.arraycopy(this.bg, col, this.bg, col + count, move)
            System.arraycopy(attrs, col, attrs, col + count, move)
        }
        clear(col, col + count - 1, bg)
    }

    /** Delete [n] cells at [col], pulling the tail left and blanking the end. */
    fun delete(col: Int, n: Int, bg: Int) {
        if (n <= 0 || col >= cols) return
        val count = minOf(n, cols - col)
        val move = cols - col - count
        if (move > 0) {
            System.arraycopy(code, col + count, code, col, move)
            System.arraycopy(this.fg, col + count, this.fg, col, move)
            System.arraycopy(this.bg, col + count, this.bg, col, move)
            System.arraycopy(attrs, col + count, attrs, col, move)
        }
        clear(cols - count, cols - 1, bg)
    }

    /**
     * Widen or narrow, keeping what fits.
     *
     * Truncating is the honest answer to a narrower terminal. Reflowing — a
     * real terminal's answer — would have to rewrite the scrollback as well,
     * and a reflow that is wrong is worse than a truncation that is
     * predictable. What the phone actually does on rotation is tell the far
     * end (`resize`), and a TUI repaints itself.
     */
    fun resize(newCols: Int) {
        if (newCols == cols) return
        val keep = minOf(newCols, cols)
        val c = IntArray(newCols) { SPACE }
        val f = IntArray(newCols)
        val b = IntArray(newCols)
        val a = IntArray(newCols)
        System.arraycopy(code, 0, c, 0, keep)
        System.arraycopy(fg, 0, f, 0, keep)
        System.arraycopy(bg, 0, b, 0, keep)
        System.arraycopy(attrs, 0, a, 0, keep)
        code = c
        fg = f
        bg = b
        attrs = a
        if (newCols < cols) wrapped = false
    }

    /** The printable text of this line, with a wide character's tail skipped. */
    fun text(): String = textOf(0, cols - 1)

    /**
     * The text of a run of **columns**, which is not the same as a run of
     * characters.
     *
     * A wide character occupies two columns and contributes one code point; an
     * astral one occupies one column and contributes two UTF-16 units. So a
     * caller holding a selection in grid coordinates — which is the only
     * coordinate a finger on a screen produces — cannot index into [text].
     * Every caller that means columns comes through here.
     */
    fun textOf(fromCol: Int, toCol: Int): String {
        val sb = StringBuilder(cols)
        for (i in maxOf(0, fromCol)..minOf(cols - 1, toCol)) {
            if (code[i] == Cell.WIDE_TAIL) continue
            sb.appendCodePoint(code[i])
        }
        return sb.toString()
    }

    /**
     * For each column, the index into [text] where that column's character
     * starts; `-1` for a wide character's tail.
     *
     * Search works on the text a reader sees and reports where it is on the
     * grid, and this is the join between the two.
     */
    fun columnOfTextIndex(): IntArray {
        val map = IntArray(cols) { -1 }
        var at = 0
        for (i in 0 until cols) {
            if (code[i] == Cell.WIDE_TAIL) continue
            map[i] = at
            at += Character.charCount(code[i])
        }
        return map
    }

    override fun toString(): String = "Line(\"${text().trimEnd()}\"${if (wrapped) ", wrapped" else ""})"

    companion object {
        const val SPACE: Int = ' '.code
    }
}

/**
 * A match found by [Screen.search].
 *
 * [line] is absolute — scrollback first, then the visible rows — so a caller
 * can scroll to it without knowing where the viewport currently is.
 */
data class Match(val line: Int, val col: Int, val length: Int)

/** A position in the same absolute coordinates [Match] uses. */
data class Pos(val line: Int, val col: Int) : Comparable<Pos> {
    override fun compareTo(other: Pos): Int =
        if (line != other.line) line.compareTo(other.line) else col.compareTo(other.col)
}

/**
 * The visible grid plus everything that has scrolled off the top of it.
 *
 * Scrollback is a ring with a cap, and the cap is a memory decision rather
 * than a taste one: this runs on a phone, and a build that let a `yes` loop
 * grow the buffer without limit would be killed by the low-memory killer
 * mid-session. Lines dropped off the front shift every absolute index, which
 * is why [dropped] is published — a search result or a selection taken before
 * a scroll has to be able to find out it has moved.
 */
class Screen(cols: Int, rows: Int, val scrollbackLimit: Int = DEFAULT_SCROLLBACK) {
    var cols: Int = cols
        private set
    var rows: Int = rows
        private set

    private var grid: ArrayList<Line> = ArrayList<Line>(rows).apply {
        repeat(rows) { add(Line(cols)) }
    }
    private val scrollback: ArrayDeque<Line> = ArrayDeque()

    /** How many lines have been evicted from the front of the scrollback, ever. */
    var dropped: Long = 0L
        private set

    val scrollbackSize: Int get() = scrollback.size

    /** Total addressable lines: scrollback then screen. */
    val totalLines: Int get() = scrollback.size + rows

    /** A line by absolute index, or `null` past either end. */
    fun lineAt(index: Int): Line? = when {
        index < 0 -> null
        index < scrollback.size -> scrollback[index]
        index < scrollback.size + rows -> grid[index - scrollback.size]
        else -> null
    }

    /** A line of the visible grid, by row. */
    fun row(row: Int): Line = grid[row.coerceIn(0, rows - 1)]

    fun cellAt(row: Int, col: Int): Cell = row(row).cellAt(col)

    /**
     * Move the whole region up by [n], sending the displaced lines to
     * scrollback **only when the region is the whole screen**.
     *
     * A scroll region is how a TUI keeps a status bar still while a pane
     * scrolls under it; the lines leaving that region were never the
     * transcript and putting them in the scrollback would interleave a status
     * bar's history with the output.
     */
    fun scrollUp(top: Int, bottom: Int, n: Int, bg: Int, keepHistory: Boolean) {
        if (n <= 0 || top > bottom) return
        val count = minOf(n, bottom - top + 1)
        repeat(count) {
            val leaving = grid.removeAt(top)
            if (keepHistory) {
                scrollback.addLast(leaving)
                while (scrollback.size > scrollbackLimit) {
                    scrollback.removeFirst()
                    dropped++
                }
            }
            val fresh = Line(cols)
            fresh.clear(0, cols - 1, bg)
            grid.add(bottom, fresh)
        }
    }

    /** Move the region down by [n]. Nothing is ever kept: these lines are the future. */
    fun scrollDown(top: Int, bottom: Int, n: Int, bg: Int) {
        if (n <= 0 || top > bottom) return
        val count = minOf(n, bottom - top + 1)
        repeat(count) {
            grid.removeAt(bottom)
            val fresh = Line(cols)
            fresh.clear(0, cols - 1, bg)
            grid.add(top, fresh)
        }
    }

    fun insertLines(top: Int, bottom: Int, at: Int, n: Int, bg: Int) {
        if (at < top || at > bottom) return
        scrollDown(at, bottom, n, bg)
    }

    fun deleteLines(top: Int, bottom: Int, at: Int, n: Int, bg: Int) {
        if (at < top || at > bottom) return
        // Never to history: these lines are being deleted, not scrolled away.
        scrollUp(at, bottom, n, bg, keepHistory = false)
    }

    /** Blank the whole grid, leaving the scrollback alone. */
    fun clearGrid(bg: Int) {
        for (line in grid) {
            line.clear(0, cols - 1, bg)
            line.wrapped = false
        }
    }

    /** Throw away the history. `ESC[3J`, and what the app's "clear" button does. */
    fun clearScrollback() {
        dropped += scrollback.size
        scrollback.clear()
    }

    /**
     * Resize the grid; returns how many lines left the TOP for history, so the
     * caller can move its cursor with the text it was on.
     *
     * A shrink first drops blank rows below [cursorRow] and only then moves
     * lines off the top. Taking every removed row off the top was right only
     * with the cursor on the last row; with it near the top (a fresh shell, or
     * after `clear`) the software keyboard opening pushed the prompt into
     * history and left the cursor on a blank row (measured on an emulator,
     * 2026-11-17). xterm does the same: the blank tail goes first.
     */
    fun resize(newCols: Int, newRows: Int, bg: Int, cursorRow: Int = rows - 1): Int {
        if (newCols == cols && newRows == rows) return 0
        var pushed = 0
        if (newCols != cols) {
            for (l in grid) l.resize(newCols)
            for (l in scrollback) l.resize(newCols)
            cols = newCols
        }
        if (newRows != rows) {
            while (grid.size > newRows && grid.size - 1 > cursorRow && grid.last().text().isBlank()) {
                grid.removeAt(grid.size - 1)
            }
            while (grid.size > newRows) {
                // What is left to remove comes off the TOP and is kept, so no
                // written line is lost: the recent output stays on screen.
                pushed++
                val leaving = grid.removeAt(0)
                scrollback.addLast(leaving)
                while (scrollback.size > scrollbackLimit) {
                    scrollback.removeFirst()
                    dropped++
                }
            }
            while (grid.size < newRows) {
                val fresh = Line(cols)
                fresh.clear(0, cols - 1, bg)
                grid.add(fresh)
            }
            rows = newRows
        }
        return pushed
    }

    /**
     * Every occurrence of [needle], oldest first.
     *
     * Case-insensitive by default because a person searching a terminal for
     * `error` means `Error` too, and a search that made them get the case
     * right would be a search they stopped using.
     *
     * Matches are found in the line's *text*, which is what a reader sees:
     * colour changes leave no characters, so a word split by an SGR escape is
     * one word here, as it looks.
     */
    fun search(needle: String, ignoreCase: Boolean = true, limit: Int = 1000): List<Match> {
        if (needle.isEmpty()) return emptyList()
        val out = ArrayList<Match>()
        for (index in 0 until totalLines) {
            val line = lineAt(index) ?: continue
            val text = line.text()
            if (text.length < needle.length) continue
            // Text index -> column, built once per line that could match. The
            // two differ wherever a wide character or an astral one appears,
            // and a highlight drawn at the text index would sit in the wrong
            // column for every line after the first CJK character on it.
            val map = line.columnOfTextIndex()
            var from = 0
            while (from <= text.length - needle.length) {
                val at = text.indexOf(needle, from, ignoreCase)
                if (at < 0) break
                val col = map.indexOfFirst { it == at }
                out.add(Match(index, if (col >= 0) col else at, needle.length))
                if (out.size >= limit) return out
                from = at + 1
            }
        }
        return out
    }

    /**
     * The text between two positions, in the form a paste should carry.
     *
     * Two rules, both of which are what a person means rather than what the
     * grid holds:
     *
     * * Trailing blanks are dropped from each line. A terminal line is always
     *   [cols] cells wide; the spaces after the last character were never
     *   typed and pasting them into a shell would be wrong.
     * * A line that [Line.wrapped] joins to the next with no newline. It is
     *   one line that happened to be too long, and a copied command that came
     *   back with a newline in the middle would not run.
     */
    fun textBetween(start: Pos, end: Pos): String {
        val (from, to) = if (start <= end) start to end else end to start
        val sb = StringBuilder()
        for (index in maxOf(0, from.line)..minOf(totalLines - 1, to.line)) {
            val line = lineAt(index) ?: continue
            // Columns, not character indices: see [Line.textOf].
            val first = if (index == from.line) from.col.coerceIn(0, line.cols - 1) else 0
            val last = if (index == to.line) to.col.coerceIn(0, line.cols - 1) else line.cols - 1
            if (first > last) {
                if (index != to.line && !line.wrapped) sb.append('\n')
                continue
            }
            var piece = line.textOf(first, last)
            // Only a selection that reaches the right-hand edge is trimmed. A
            // terminal line is always [cols] cells wide and the blanks after
            // the last character were never typed; a selection that stops
            // mid-line keeps its interior spacing exactly.
            if (last >= line.cols - 1) piece = piece.trimEnd(' ')
            sb.append(piece)
            if (index != to.line && !line.wrapped) sb.append('\n')
        }
        return sb.toString()
    }

    /** Every line as text, oldest first. The whole transcript. */
    fun transcript(): String = buildString {
        for (i in 0 until totalLines) {
            val line = lineAt(i) ?: continue
            append(line.text().trimEnd())
            if (i != totalLines - 1) append('\n')
        }
    }

    /** The visible grid as text, for a test that wants to name what is on screen. */
    fun visibleText(): String = buildString {
        for (r in 0 until rows) {
            append(grid[r].text().trimEnd())
            if (r != rows - 1) append('\n')
        }
    }

    companion object {
        /**
         * How many lines of history to keep.
         *
         * The daemon's whole ring is 256 KiB, about 3200 lines at eighty
         * columns, and a phone asks for a quarter of it on attach
         * (`Agentd.DEFAULT_REPLAY`), so this holds any replay and a good deal
         * of what follows it.
         */
        const val DEFAULT_SCROLLBACK: Int = 4000
    }
}
