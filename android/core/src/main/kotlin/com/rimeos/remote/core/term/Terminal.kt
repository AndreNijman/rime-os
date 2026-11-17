package com.rimeos.remote.core.term

import java.io.ByteArrayOutputStream

/**
 * A terminal, as a state machine over a byte stream.
 *
 * ## Why this is a state machine and not a regular expression
 *
 * The bytes arrive from a phone's radio in whatever sizes the network chose.
 * A parser that matched escape sequences in a buffer would have to decide
 * what to do with `ESC [ 3` at the end of a packet, and every such parser
 * eventually decides wrongly — it either blocks waiting for a sequence that
 * was never coming, or it prints `[3` as text when the `8m` arrives in the
 * next frame. So every byte is fed through a machine whose entire state is
 * fields of this object, and feeding a stream one byte at a time must produce
 * exactly the screen that feeding it in one call produces. `TerminalSplitTest`
 * asserts that over every fixture at every boundary, which is the honest form
 * of "low bandwidth is tested": a slow link is one that splits everywhere.
 *
 * ## Why it answers questions
 *
 * Every agent TUI opens by interrogating the terminal — `ESC[6n` for the
 * cursor, `ESC[c` for device attributes, `OSC 10/11` for the foreground and
 * background colours, `ESC[?…$p` for modes it wants to use — and then waits.
 * Recorded against a real PTY on 2026-09-12, opencode emitted 284 bytes of
 * questions and then nothing at all; answered, it emitted 12665 bytes and
 * painted its interface. "Works with the TUIs without rewriting them" is, in
 * practice, this: [takeResponses] holds what must go back up the wire.
 *
 * ## What it deliberately does not do
 *
 * No reflow on resize: a narrower screen truncates rather than rewrapping its
 * history. The phone tells the far end its new size and the TUI repaints,
 * which is both what actually happens and better than a reflow that is subtly
 * wrong. No mouse encoding — the modes are tracked so the state is honest,
 * and a finger is not a mouse.
 */
class Terminal(
    cols: Int = 80,
    rows: Int = 24,
    scrollbackLimit: Int = Screen.DEFAULT_SCROLLBACK,
) {
    /** The normal screen, the one with the history. */
    private val normal = Screen(cols, rows, scrollbackLimit)

    /**
     * The alternate screen: no scrollback, by design and not by omission.
     *
     * A full-screen application owns the whole grid and repaints it; its
     * intermediate frames are not a transcript, and a terminal that kept them
     * would fill the history with one editor's redraws.
     */
    private val alternate = Screen(cols, rows, scrollbackLimit = 0)

    /** Which of the two is being written to. */
    var onAlternate: Boolean = false
        private set

    val screen: Screen get() = if (onAlternate) alternate else normal

    val cols: Int get() = screen.cols
    val rows: Int get() = screen.rows

    // ---- cursor -------------------------------------------------------

    var cursorRow: Int = 0
        private set
    var cursorCol: Int = 0
        private set

    /** Whether the far end asked for the cursor to be drawn (DECTCEM, `?25`). */
    var cursorVisible: Boolean = true
        private set

    /**
     * The cursor is one column past the right edge and the next character
     * wraps.
     *
     * A real terminal does not move the cursor off the edge when the last
     * column is written: it stays there with this flag set, so that a
     * backspace still lands on the character just printed. Writing `abc` into
     * a three-column terminal and then `d` must put `d` at the start of the
     * next line, and a naive implementation puts it in column 3 of a line
     * three wide, which does not exist.
     */
    private var pendingWrap = false

    private var savedRow = 0
    private var savedCol = 0
    private var savedFg = Colour.DEFAULT
    private var savedBg = Colour.DEFAULT
    private var savedAttrs = Attrs.NONE

    // ---- pen ----------------------------------------------------------

    var fg: Int = Colour.DEFAULT
        private set
    var bg: Int = Colour.DEFAULT
        private set
    var attrs: Int = Attrs.NONE
        private set

    // ---- scroll region ------------------------------------------------

    private var scrollTop = 0
    private var scrollBottom = rows - 1

    // ---- modes --------------------------------------------------------

    /**
     * Application cursor keys (DECCKM, `?1`).
     *
     * The single most consequential mode for this app. With it set, the arrow
     * keys must be sent as `ESC O A` and not `ESC [ A`, and a TUI that asked
     * for it will not see arrow presses encoded the other way. [Keys] reads
     * this.
     */
    var applicationCursorKeys: Boolean = false
        private set

    /** Autowrap (DECAWM, `?7`). On at reset, as every terminal has it. */
    var autoWrap: Boolean = true
        private set

    /** Bracketed paste (`?2004`): a paste must be wrapped in `ESC[200~`/`ESC[201~`. */
    var bracketedPaste: Boolean = false
        private set

    /** Origin mode (DECOM, `?6`): row addressing is relative to the scroll region. */
    var originMode: Boolean = false
        private set

    /** Insert mode (IRM, `4`): printing shifts the rest of the line right. */
    var insertMode: Boolean = false
        private set

    /** Whether the far end asked for mouse reporting; tracked so the state is honest. */
    var mouseReporting: Boolean = false
        private set

    /** The title the far end last set with `OSC 0` or `OSC 2`. */
    var title: String = ""
        private set

    /** Set when the far end rang the bell, cleared by [takeBell]. */
    private var bell = false

    // ---- tab stops ----------------------------------------------------

    private var tabs = BooleanArray(cols) { it % TAB_WIDTH == 0 }

    // ---- damage -------------------------------------------------------

    /**
     * Bumped by every change to the grid.
     *
     * A renderer that redrew on a timer would burn a phone's battery on an
     * idle terminal, and one that diffed four million cells would burn it
     * worse. A counter is what a Compose `State` can hold and what a
     * `derivedStateOf` can key on.
     */
    var revision: Long = 0L
        private set

    /**
     * Called after every change to the grid, on whichever thread made it.
     *
     * The counter above says a renderer "that redrew on a timer would burn a
     * phone's battery on an idle terminal", and that is exactly what a
     * renderer polling [revision] once per displayed frame does: `withFrameNanos`
     * asks the Choreographer for another frame, so a loop that looks every
     * frame keeps the display pipeline running at sixty hertz over a terminal
     * where nothing is happening. It also keeps Compose's `Recomposer`
     * permanently busy, which is what made this screen impossible to host in
     * the standard Compose test harness.
     *
     * So the pump says when something moved and the renderer sleeps otherwise.
     * Called with [lock] released: the listener reaches a coroutine and a
     * channel, the pump is blocked behind it, and back-pressure onto a
     * terminal is the one thing this class is careful not to add.
     *
     * `@Volatile` because the pump thread reads it and the main thread writes
     * it, once, when the screen is built.
     */
    @Volatile
    var onChanged: (() -> Unit)? = null

    // ---- parser state -------------------------------------------------

    private enum class St { GROUND, ESC, ESC_INT, CSI_ENTRY, CSI_PARAM, CSI_INT, CSI_IGNORE, OSC, DCS, DCS_PASS, STRING }

    private var state = St.GROUND
    private val params = ArrayList<IntArray>(8)
    private val group = ArrayList<Int>(4)
    private var digits = -1
    private var privateMarker = 0
    private var intermediate = 0
    private val stringBuf = StringBuilder()
    private var stringKind = 0
    private var utf8Remaining = 0
    private var utf8Acc = 0
    private var lastEscWasEsc = false

    private val responses = ByteArrayOutputStream()

    // ====================================================================
    // Feeding
    // ====================================================================

    /**
     * The lock every mutation takes and every reader must take.
     *
     * `Screen` moves whole lines between its grid and its scrollback, so a
     * renderer walking the rows while bytes arrive does not read a slightly
     * stale screen — it reads a torn one, or an index that stopped existing
     * between the bounds check and the read. On a phone the reader is the
     * main thread and the writer is a socket pump, so the two genuinely run
     * at once.
     *
     * Held for a whole draw pass rather than per cell: a frame that is
     * internally inconsistent is worse than a frame that is one pump-read
     * old. [read] is the way to take it.
     */
    val lock: Any = Any()

    /**
     * Look at the screen under [lock].
     *
     * Every renderer, every search and every selection goes through this.
     * The block should not be slow: the pump is blocked behind it, and a
     * blocked pump is back-pressure onto somebody's terminal.
     */
    fun <T> read(block: (Screen) -> T): T = synchronized(lock) { block(screen) }

    fun feed(text: String) = feed(text.toByteArray(Charsets.UTF_8))

    fun feed(bytes: ByteArray) = feed(bytes, 0, bytes.size)

    fun feed(bytes: ByteArray, offset: Int, length: Int) {
        synchronized(lock) {
            for (i in offset until offset + length) {
                step(bytes[i].toInt() and 0xFF)
            }
            revision++
        }
        onChanged?.invoke()
    }

    /**
     * Whatever the far end asked for, ready to be written back to the PTY.
     *
     * Taken rather than read, and emptied by the taking: these are answers,
     * and sending one twice is how a TUI reading `ESC[…R` finds a second
     * cursor report it never asked for in the middle of its next read.
     */
    fun takeResponses(): ByteArray {
        val out = responses.toByteArray()
        responses.reset()
        return out
    }

    /** Whether a bell arrived since this was last asked. */
    fun takeBell(): Boolean {
        val b = bell
        bell = false
        return b
    }

    // ====================================================================
    // The machine
    // ====================================================================

    private fun step(b: Int) {
        // C0 controls act from anywhere except inside a string, where they are
        // the string's own business — except ESC and the ST pair, which end it.
        if (state == St.OSC || state == St.DCS_PASS || state == St.STRING) {
            stringByte(b)
            return
        }
        if (b == ESC) {
            state = St.ESC
            intermediate = 0
            return
        }
        if (b < 0x20) {
            control(b)
            return
        }
        when (state) {
            St.GROUND -> ground(b)
            St.ESC -> esc(b)
            St.ESC_INT -> escIntermediate(b)
            St.CSI_ENTRY -> csiEntry(b)
            St.CSI_PARAM -> csiParam(b)
            St.CSI_INT -> csiIntermediate(b)
            St.CSI_IGNORE -> if (b in 0x40..0x7E) state = St.GROUND
            St.DCS -> dcs(b)
            else -> state = St.GROUND
        }
    }

    private fun control(b: Int) {
        when (b) {
            BEL -> bell = true
            0x08 -> backspace()
            0x09 -> tab()
            0x0A, 0x0B, 0x0C -> lineFeed()
            0x0D -> { cursorCol = 0; pendingWrap = false }
            0x0E, 0x0F -> Unit // SO/SI: charset shifts, and this build is UTF-8 only.
            else -> Unit
        }
    }

    private fun ground(b: Int) {
        if (utf8Remaining > 0) {
            if (b and 0xC0 != 0x80) {
                // A malformed continuation. The partial sequence is dropped and
                // this byte is reconsidered from the top, which is what every
                // careful decoder does: swallowing it would lose a printable
                // character for every truncated one.
                utf8Remaining = 0
                put(REPLACEMENT)
                ground(b)
                return
            }
            utf8Acc = (utf8Acc shl 6) or (b and 0x3F)
            utf8Remaining--
            if (utf8Remaining == 0) put(utf8Acc)
            return
        }
        when {
            b < 0x80 -> put(b)
            b and 0xE0 == 0xC0 -> { utf8Acc = b and 0x1F; utf8Remaining = 1 }
            b and 0xF0 == 0xE0 -> { utf8Acc = b and 0x0F; utf8Remaining = 2 }
            b and 0xF8 == 0xF0 -> { utf8Acc = b and 0x07; utf8Remaining = 3 }
            else -> put(REPLACEMENT)
        }
    }

    private fun esc(b: Int) {
        when (b) {
            in 0x20..0x2F -> { intermediate = b; state = St.ESC_INT }
            '['.code -> { params.clear(); group.clear(); digits = -1; privateMarker = 0; intermediate = 0; state = St.CSI_ENTRY }
            ']'.code -> { stringBuf.setLength(0); stringKind = ']'.code; state = St.OSC }
            'P'.code -> { params.clear(); group.clear(); digits = -1; privateMarker = 0; intermediate = 0; state = St.DCS }
            'X'.code, '^'.code, '_'.code -> { stringBuf.setLength(0); stringKind = b; state = St.STRING }
            '7'.code -> { saveCursor(); state = St.GROUND }
            '8'.code -> { restoreCursor(); state = St.GROUND }
            'D'.code -> { index(); state = St.GROUND }
            'E'.code -> { index(); cursorCol = 0; pendingWrap = false; state = St.GROUND }
            'M'.code -> { reverseIndex(); state = St.GROUND }
            'H'.code -> { if (cursorCol in tabs.indices) tabs[cursorCol] = true; state = St.GROUND }
            // RIS, and the LOCKED form deliberately: this runs inside `feed`,
            // which already holds the lock and already tells the renderer when
            // it is finished. Calling the public `reset` here would deliver a
            // change notification with the lock held, from inside the parser,
            // to a listener that is entitled to look at the screen.
            'c'.code -> { resetLocked(); state = St.GROUND }
            '='.code, '>'.code -> state = St.GROUND // keypad modes; nothing here depends on them
            '\\'.code -> state = St.GROUND // a stray ST
            else -> state = St.GROUND
        }
    }

    private fun escIntermediate(b: Int) {
        // `ESC ( B` and friends designate a character set. This build decodes
        // UTF-8 and nothing else, so the designation is consumed and ignored —
        // but it must be CONSUMED, or the `B` prints as a letter.
        //
        // `ESC # 8` (DECALN) is the one that has a visible effect, and it is
        // the screen-alignment test: every cell becomes an `E`. Terminal test
        // suites open with it, so it is cheaper to implement than to explain.
        if (intermediate == '#'.code && b == '8'.code) {
            for (r in 0 until rows) {
                val line = screen.row(r)
                for (c in 0 until cols) line.set(c, 'E'.code, fg, bg, attrs)
            }
        }
        state = St.GROUND
    }

    // ---- CSI ----------------------------------------------------------

    private fun csiEntry(b: Int) {
        when (b) {
            in 0x3C..0x3F -> { privateMarker = b; state = St.CSI_PARAM }
            in 0x30..0x39 -> { digits = b - '0'.code; state = St.CSI_PARAM }
            ':'.code -> { pushDigits(); state = St.CSI_PARAM }
            ';'.code -> { pushGroup(); state = St.CSI_PARAM }
            in 0x20..0x2F -> { intermediate = b; state = St.CSI_INT }
            in 0x40..0x7E -> { pushGroup(); csiDispatch(b); state = St.GROUND }
            else -> state = St.CSI_IGNORE
        }
    }

    private fun csiParam(b: Int) {
        when (b) {
            in 0x30..0x39 -> digits = (if (digits < 0) 0 else digits) * 10 + (b - '0'.code)
            ':'.code -> pushDigits()
            ';'.code -> pushGroup()
            in 0x20..0x2F -> { intermediate = b; state = St.CSI_INT }
            in 0x40..0x7E -> { pushGroup(); csiDispatch(b); state = St.GROUND }
            else -> state = St.CSI_IGNORE
        }
    }

    private fun csiIntermediate(b: Int) {
        when (b) {
            in 0x20..0x2F -> intermediate = b
            in 0x40..0x7E -> { pushGroup(); csiDispatch(b); state = St.GROUND }
            else -> state = St.CSI_IGNORE
        }
    }

    private fun pushDigits() {
        group.add(digits)
        digits = -1
    }

    private fun pushGroup() {
        group.add(digits)
        digits = -1
        params.add(group.toIntArray())
        group.clear()
    }

    /** Parameter [i] with [fallback] for absent or zero-length. */
    private fun p(i: Int, fallback: Int): Int {
        val g = params.getOrNull(i) ?: return fallback
        val v = g.getOrNull(0) ?: return fallback
        return if (v < 0) fallback else v
    }

    private fun csiDispatch(final: Int) {
        if (privateMarker == '?'.code) {
            privateCsi(final)
            return
        }
        if (privateMarker == '>'.code || privateMarker == '<'.code) {
            when (final) {
                'c'.code -> respond("$CSI>0;$SECONDARY_DA_VERSION;1c")
                'q'.code -> respond("${ESC.toChar()}P>|$XTVERSION${ESC.toChar()}\\")
                // `ESC[>4;Nm` (modifyOtherKeys) and `ESC[>Nu` (the kitty
                // keyboard stack) are consumed rather than implemented: this
                // client encodes keys the ordinary way, and a terminal that
                // ACKNOWLEDGED a protocol it does not speak would get keys it
                // cannot read back.
                else -> Unit
            }
            return
        }
        when (final) {
            '@'.code -> { currentLine().insert(cursorCol, p(0, 1), bg); pendingWrap = false }
            'A'.code -> moveCursor(cursorRow - p(0, 1), cursorCol)
            'B'.code, 'e'.code -> moveCursor(cursorRow + p(0, 1), cursorCol)
            'C'.code, 'a'.code -> moveCursor(cursorRow, cursorCol + p(0, 1))
            'D'.code -> moveCursor(cursorRow, cursorCol - p(0, 1))
            'E'.code -> moveCursor(cursorRow + p(0, 1), 0)
            'F'.code -> moveCursor(cursorRow - p(0, 1), 0)
            'G'.code, '`'.code -> moveCursor(cursorRow, p(0, 1) - 1)
            'H'.code, 'f'.code -> {
                val r = p(0, 1) - 1
                val c = p(1, 1) - 1
                moveCursor(if (originMode) scrollTop + r else r, c)
            }
            'I'.code -> repeat(p(0, 1)) { tab() }
            'J'.code -> eraseInDisplay(p(0, 0))
            'K'.code -> eraseInLine(p(0, 0))
            'L'.code -> { screen.insertLines(scrollTop, scrollBottom, cursorRow, p(0, 1), bg); cursorCol = 0 }
            'M'.code -> { screen.deleteLines(scrollTop, scrollBottom, cursorRow, p(0, 1), bg); cursorCol = 0 }
            'P'.code -> { currentLine().delete(cursorCol, p(0, 1), bg); pendingWrap = false }
            'S'.code -> screen.scrollUp(scrollTop, scrollBottom, p(0, 1), bg, keepHistory = !onAlternate && scrollTop == 0)
            'T'.code -> screen.scrollDown(scrollTop, scrollBottom, p(0, 1), bg)
            'X'.code -> {
                val n = p(0, 1)
                currentLine().clear(cursorCol, cursorCol + n - 1, bg)
            }
            'Z'.code -> repeat(p(0, 1)) { backTab() }
            'b'.code -> repeatLast(p(0, 1))
            'd'.code -> moveCursor(p(0, 1) - 1, cursorCol)
            'c'.code -> respond("$CSI?62;22c")
            'g'.code -> clearTabs(p(0, 0))
            'h'.code -> setAnsiModes(true)
            'l'.code -> setAnsiModes(false)
            'm'.code -> applySgr()
            'n'.code -> deviceStatus(p(0, 0))
            'r'.code -> {
                val top = p(0, 1) - 1
                val bottom = p(1, rows) - 1
                if (top < bottom && bottom < rows) {
                    scrollTop = maxOf(0, top)
                    scrollBottom = bottom
                } else {
                    scrollTop = 0
                    scrollBottom = rows - 1
                }
                moveCursor(if (originMode) scrollTop else 0, 0)
            }
            's'.code -> saveCursor()
            'u'.code -> restoreCursor()
            't'.code -> windowOp()
            else -> Unit
        }
    }

    private fun privateCsi(final: Int) {
        when (final) {
            'h'.code -> setPrivateModes(true)
            'l'.code -> setPrivateModes(false)
            'n'.code -> Unit // DECDSR variants nothing here reports on
            'u'.code -> {
                // The kitty keyboard protocol's "what flags are set?" query.
                // Answered with zero, honestly: none are, because this build
                // does not speak it.
                respond("$CSI?0u")
            }
            '$'.code -> Unit
            else -> {
                if (intermediate == '$'.code) decrqm()
            }
        }
    }

    /**
     * `ESC[?N$p` — "is mode N set?".
     *
     * Answered 1 (set) or 2 (reset) for the modes this build actually tracks
     * and **0 (not recognised)** for everything else. Zero is the whole point:
     * a terminal that answered "reset" for a mode it has never heard of would
     * be telling a TUI that the feature exists and is merely off, and the TUI
     * would then turn it on and rely on it.
     */
    private fun decrqm() {
        val mode = p(0, 0)
        val value = when (mode) {
            1 -> if (applicationCursorKeys) 1 else 2
            6 -> if (originMode) 1 else 2
            7 -> if (autoWrap) 1 else 2
            25 -> if (cursorVisible) 1 else 2
            47, 1047, 1049 -> if (onAlternate) 1 else 2
            1000, 1002, 1003, 1006 -> if (mouseReporting) 1 else 2
            2004 -> if (bracketedPaste) 1 else 2
            else -> 0
        }
        respond("$CSI?$mode;$value\$y")
    }

    private fun setPrivateModes(on: Boolean) {
        for (g in params) {
            when (g.getOrNull(0) ?: continue) {
                1 -> applicationCursorKeys = on
                6 -> {
                    originMode = on
                    moveCursor(if (on) scrollTop else 0, 0)
                }
                7 -> autoWrap = on
                25 -> cursorVisible = on
                1000, 1002, 1003, 1005, 1006, 1015, 1016 -> mouseReporting = on
                47, 1047 -> switchScreen(on, saveRestore = false, clearOnEnter = on)
                1049 -> switchScreen(on, saveRestore = true, clearOnEnter = on)
                2004 -> bracketedPaste = on
                // 1004 focus reporting, 2026 synchronised output, 2027, 2031:
                // consumed. This renderer paints a whole frame at a time, so
                // synchronisation is already what it does, and a phone has no
                // terminal focus to report.
                else -> Unit
            }
        }
    }

    private fun setAnsiModes(on: Boolean) {
        for (g in params) {
            when (g.getOrNull(0) ?: continue) {
                4 -> insertMode = on
                else -> Unit
            }
        }
    }

    private fun deviceStatus(what: Int) {
        when (what) {
            5 -> respond("${CSI}0n")
            6 -> {
                val r = if (originMode) cursorRow - scrollTop else cursorRow
                respond("$CSI${r + 1};${cursorCol + 1}R")
            }
            else -> Unit
        }
    }

    /**
     * `ESC[…t` — window manipulation.
     *
     * Only the two reports are answered, and both with the truth about a
     * *terminal*, not about a phone: opencode asks `ESC[14t` (pixel size) to
     * decide whether it can draw images, and a made-up large answer would have
     * it try. The numbers are derived from the grid so they stay consistent
     * with what has actually been sized.
     */
    private fun windowOp() {
        when (p(0, 0)) {
            14 -> respond("${CSI}4;${rows * CELL_PIXELS_H};${cols * CELL_PIXELS_W}t")
            18 -> respond("${CSI}8;$rows;${cols}t")
            else -> Unit
        }
    }

    private fun clearTabs(what: Int) {
        when (what) {
            0 -> if (cursorCol in tabs.indices) tabs[cursorCol] = false
            3 -> tabs.fill(false)
            else -> Unit
        }
    }

    // ---- SGR ----------------------------------------------------------

    private fun applySgr() {
        if (params.isEmpty()) {
            resetPen()
            return
        }
        var i = 0
        while (i < params.size) {
            val g = params[i]
            val code = g.getOrNull(0) ?: -1
            // The colon form carries the whole colour in one parameter group:
            // `38:2::r:g:b` and `38:5:n`. It is what modern TUIs emit, and a
            // parser that only understood the semicolon form would read
            // `38:2::215:119:87` as a single parameter 38 and paint the rest
            // of the line in the default foreground.
            if (g.size > 1 && (code == 38 || code == 48 || code == 58)) {
                val colour = colourFromGroup(g)
                if (colour != null) applyColour(code, colour)
                i++
                continue
            }
            when (code) {
                -1, 0 -> resetPen()
                1 -> attrs = attrs or Attrs.BOLD
                2 -> attrs = attrs or Attrs.DIM
                3 -> attrs = attrs or Attrs.ITALIC
                4 -> attrs = if (g.size > 1 && g[1] == 0) attrs and Attrs.UNDERLINE.inv() else attrs or Attrs.UNDERLINE
                5, 6 -> attrs = attrs or Attrs.BLINK
                7 -> attrs = attrs or Attrs.INVERSE
                8 -> attrs = attrs or Attrs.HIDDEN
                9 -> attrs = attrs or Attrs.STRIKE
                21 -> attrs = attrs or Attrs.UNDERLINE
                22 -> attrs = attrs and (Attrs.BOLD or Attrs.DIM).inv()
                23 -> attrs = attrs and Attrs.ITALIC.inv()
                24 -> attrs = attrs and Attrs.UNDERLINE.inv()
                25 -> attrs = attrs and Attrs.BLINK.inv()
                27 -> attrs = attrs and Attrs.INVERSE.inv()
                28 -> attrs = attrs and Attrs.HIDDEN.inv()
                29 -> attrs = attrs and Attrs.STRIKE.inv()
                in 30..37 -> fg = Colour.indexed(code - 30)
                38 -> { i = extendedColour(i) { fg = it }; continue }
                39 -> fg = Colour.DEFAULT
                in 40..47 -> bg = Colour.indexed(code - 40)
                48 -> { i = extendedColour(i) { bg = it }; continue }
                49 -> bg = Colour.DEFAULT
                58 -> { i = extendedColour(i) { }; continue }
                59 -> Unit
                in 90..97 -> fg = Colour.indexed(code - 90 + 8)
                in 100..107 -> bg = Colour.indexed(code - 100 + 8)
                else -> Unit
            }
            i++
        }
    }

    private fun applyColour(code: Int, colour: Int) {
        when (code) {
            38 -> fg = colour
            48 -> bg = colour
            else -> Unit // 58 is the underline colour, which this build does not draw
        }
    }

    /** `38:2::r:g:b`, `38:2:r:g:b` or `38:5:n`, all within one parameter group. */
    private fun colourFromGroup(g: IntArray): Int? = when (g.getOrNull(1)) {
        5 -> g.getOrNull(2)?.takeIf { it in 0..255 }?.let { Colour.indexed(it) }
        2 -> when {
            // With the colour-space id present, as ITU T.416 defines it.
            g.size >= 6 -> Colour.rgb(g[3].coerceAtLeast(0), g[4].coerceAtLeast(0), g[5].coerceAtLeast(0))
            g.size == 5 -> Colour.rgb(g[2].coerceAtLeast(0), g[3].coerceAtLeast(0), g[4].coerceAtLeast(0))
            else -> null
        }
        else -> null
    }

    /**
     * The semicolon form, which spills across parameter groups.
     *
     * Returns the index to continue from. It consumes what it needs and no
     * more: `38;5;9;1m` is an indexed red *and then* bold, and a parser that
     * swallowed a fixed number of parameters would lose the bold.
     */
    private inline fun extendedColour(at: Int, set: (Int) -> Unit): Int {
        return when (params.getOrNull(at + 1)?.getOrNull(0)) {
            5 -> {
                val idx = params.getOrNull(at + 2)?.getOrNull(0) ?: -1
                if (idx in 0..255) set(Colour.indexed(idx))
                at + 3
            }
            2 -> {
                val r = params.getOrNull(at + 2)?.getOrNull(0) ?: -1
                val g = params.getOrNull(at + 3)?.getOrNull(0) ?: -1
                val b = params.getOrNull(at + 4)?.getOrNull(0) ?: -1
                if (r >= 0 && g >= 0 && b >= 0) set(Colour.rgb(r, g, b))
                at + 5
            }
            // Neither 2 nor 5: a form this build does not know. One parameter
            // is consumed so the loop always advances — a `continue` on an
            // index that did not move is an infinite loop driven by the far
            // end, which is a remote peer hanging the app.
            else -> at + 2
        }
    }

    private fun resetPen() {
        fg = Colour.DEFAULT
        bg = Colour.DEFAULT
        attrs = Attrs.NONE
    }

    // ---- OSC / DCS / other strings -------------------------------------

    private fun stringByte(b: Int) {
        // `ESC \` is the String Terminator. BEL also ends an OSC, which is
        // xterm's older form and what most of these agents actually send.
        if (lastEscWasEsc) {
            lastEscWasEsc = false
            if (b == '\\'.code) {
                endString()
                return
            }
            // An ESC inside a string that is not an ST abandons the string and
            // is re-read as the start of a new sequence.
            endString(dispatch = false)
            state = St.ESC
            esc(b)
            return
        }
        when {
            b == ESC -> lastEscWasEsc = true
            b == BEL && state == St.OSC -> endString()
            b < 0x20 && b != 0x09 -> Unit
            else -> if (stringBuf.length < MAX_STRING) stringBuf.append(b.toChar())
        }
    }

    private fun endString(dispatch: Boolean = true) {
        if (dispatch) {
            when (stringKind) {
                ']'.code -> osc(stringBuf.toString())
                'P'.code -> dcsString(stringBuf.toString())
                else -> Unit // SOS, PM, APC: consumed, never rendered
            }
        }
        stringBuf.setLength(0)
        stringKind = 0
        state = St.GROUND
    }

    private fun osc(body: String) {
        val semi = body.indexOf(';')
        val code = (if (semi < 0) body else body.substring(0, semi)).toIntOrNull() ?: return
        val rest = if (semi < 0) "" else body.substring(semi + 1)
        when (code) {
            0, 2 -> title = rest.take(MAX_TITLE)
            1 -> Unit // icon name; a phone has no icon to name
            4 -> {
                // `OSC 4;n;?` — "what is palette entry n?". Answered from the
                // xterm cube for 16..255 and refused for 0..15, which are the
                // renderer's own theme colours and not this object's to claim.
                val bits = rest.split(';')
                var i = 0
                while (i + 1 < bits.size) {
                    val index = bits[i].toIntOrNull()
                    if (bits[i + 1] == "?" && index != null) {
                        val resolved = Colour.resolveCube(index)
                        if (resolved != null) {
                            respond("${ESC.toChar()}]4;$index;${rgbText(resolved)}${ESC.toChar()}\\")
                        }
                    }
                    i += 2
                }
            }
            10 -> if (rest.startsWith("?")) respond("${ESC.toChar()}]10;${rgbText(defaultForeground)}${ESC.toChar()}\\")
            11 -> if (rest.startsWith("?")) respond("${ESC.toChar()}]11;${rgbText(defaultBackground)}${ESC.toChar()}\\")
            // 8 is a hyperlink, 52 is the clipboard, 99/777 are notifications,
            // 1337 is iTerm's private channel, 66 is a styled-text extension.
            // All consumed. A clipboard write from the far end is deliberately
            // NOT honoured: a remote machine that could put text on the phone's
            // clipboard by printing bytes is a remote machine that can put
            // anything there.
            else -> Unit
        }
    }

    private fun dcs(b: Int) {
        when (b) {
            in 0x30..0x39 -> digits = (if (digits < 0) 0 else digits) * 10 + (b - '0'.code)
            ';'.code -> pushGroup()
            in 0x20..0x2F -> intermediate = b
            in 0x40..0x7E -> {
                pushGroup()
                stringBuf.setLength(0)
                stringKind = 'P'.code
                stringBuf.append(intermediate.toChar()).append(b.toChar())
                state = St.DCS_PASS
            }
            else -> state = St.CSI_IGNORE
        }
    }

    /**
     * A device-control string, of which only one is answered.
     *
     * `DCS + q <hex names> ST` is XTGETTINCAP: "what does your terminfo say
     * about these capabilities?". The answer `DCS 0 + r <name> ST` means "I do
     * not have that one" and is a complete, correct reply — a terminal is
     * allowed not to have a capability. Saying nothing at all is what makes a
     * TUI wait.
     */
    private fun dcsString(body: String) {
        if (!body.startsWith("+q")) return
        val names = body.removePrefix("+q").split(';').filter { it.isNotEmpty() }
        for (name in names) {
            respond("${ESC.toChar()}P0+r$name${ESC.toChar()}\\")
        }
    }

    private fun rgbText(colour: Int): String {
        val r = Colour.red(colour)
        val g = Colour.green(colour)
        val b = Colour.blue(colour)
        return "rgb:%02x%02x/%02x%02x/%02x%02x".format(r, r, g, g, b, b)
    }

    // ---- printing ------------------------------------------------------

    private var lastPrinted = -1

    private fun put(code: Int) {
        val width = charWidth(code)
        if (width == 0) {
            // A combining mark belongs to the cell before the cursor, and this
            // build does not compose: dropping it is wrong but bounded, where
            // printing it in its own cell would shift the rest of the line.
            return
        }
        if (pendingWrap && autoWrap) {
            currentLine().wrapped = true
            cursorCol = 0
            lineFeedInRegion()
            pendingWrap = false
        }
        if (cursorCol >= cols) {
            if (!autoWrap) cursorCol = cols - 1 else { cursorCol = 0; lineFeedInRegion() }
        }
        val line = currentLine()
        if (insertMode) line.insert(cursorCol, width, bg)
        line.set(cursorCol, code, fg, bg, attrs)
        if (width == 2 && cursorCol + 1 < cols) {
            line.set(cursorCol + 1, Cell.WIDE_TAIL, fg, bg, attrs)
        }
        lastPrinted = code
        cursorCol += width
        if (cursorCol >= cols) {
            cursorCol = cols - 1
            pendingWrap = true
        }
    }

    /** `CSI b` — repeat the last printed character. Used heavily by `less` and by box drawing. */
    private fun repeatLast(n: Int) {
        val code = lastPrinted
        if (code < 0) return
        repeat(minOf(n, cols * rows)) { put(code) }
    }

    private fun currentLine(): Line = screen.row(cursorRow)

    private fun backspace() {
        if (pendingWrap) {
            pendingWrap = false
            return
        }
        if (cursorCol > 0) cursorCol--
    }

    private fun tab() {
        pendingWrap = false
        var c = cursorCol + 1
        while (c < cols && !(c < tabs.size && tabs[c])) c++
        cursorCol = minOf(c, cols - 1)
    }

    private fun backTab() {
        pendingWrap = false
        var c = cursorCol - 1
        while (c > 0 && !(c < tabs.size && tabs[c])) c--
        cursorCol = maxOf(c, 0)
    }

    private fun lineFeed() {
        pendingWrap = false
        lineFeedInRegion()
    }

    private fun lineFeedInRegion() {
        if (cursorRow == scrollBottom) {
            screen.scrollUp(scrollTop, scrollBottom, 1, bg, keepHistory = !onAlternate && scrollTop == 0)
        } else if (cursorRow < rows - 1) {
            cursorRow++
        }
    }

    private fun index() = lineFeedInRegion()

    private fun reverseIndex() {
        if (cursorRow == scrollTop) {
            screen.scrollDown(scrollTop, scrollBottom, 1, bg)
        } else if (cursorRow > 0) {
            cursorRow--
        }
    }

    private fun eraseInDisplay(what: Int) {
        when (what) {
            0 -> {
                currentLine().clear(cursorCol, cols - 1, bg)
                for (r in cursorRow + 1 until rows) screen.row(r).clear(0, cols - 1, bg)
            }
            1 -> {
                currentLine().clear(0, cursorCol, bg)
                for (r in 0 until cursorRow) screen.row(r).clear(0, cols - 1, bg)
            }
            2 -> screen.clearGrid(bg)
            3 -> screen.clearScrollback()
            else -> Unit
        }
        pendingWrap = false
    }

    private fun eraseInLine(what: Int) {
        when (what) {
            0 -> currentLine().clear(cursorCol, cols - 1, bg)
            1 -> currentLine().clear(0, cursorCol, bg)
            2 -> currentLine().clear(0, cols - 1, bg)
            else -> Unit
        }
        pendingWrap = false
    }

    private fun moveCursor(row: Int, col: Int) {
        val lo = if (originMode) scrollTop else 0
        val hi = if (originMode) scrollBottom else rows - 1
        cursorRow = row.coerceIn(lo, hi)
        cursorCol = col.coerceIn(0, cols - 1)
        pendingWrap = false
    }

    private fun saveCursor() {
        savedRow = cursorRow
        savedCol = cursorCol
        savedFg = fg
        savedBg = bg
        savedAttrs = attrs
    }

    private fun restoreCursor() {
        cursorRow = savedRow.coerceIn(0, rows - 1)
        cursorCol = savedCol.coerceIn(0, cols - 1)
        fg = savedFg
        bg = savedBg
        attrs = savedAttrs
        pendingWrap = false
    }

    private fun switchScreen(toAlternate: Boolean, saveRestore: Boolean, clearOnEnter: Boolean) {
        if (toAlternate == onAlternate) return
        if (toAlternate) {
            if (saveRestore) saveCursor()
            onAlternate = true
            if (clearOnEnter) alternate.clearGrid(bg)
            cursorRow = 0
            cursorCol = 0
        } else {
            onAlternate = false
            if (saveRestore) restoreCursor()
        }
        scrollTop = 0
        scrollBottom = rows - 1
        pendingWrap = false
    }

    // ---- public operations ---------------------------------------------

    /**
     * Resize both screens and tell the caller nothing.
     *
     * Whoever calls this is also responsible for sending `resize` to the
     * daemon: a terminal that changed its own grid without telling the
     * program is a terminal whose next repaint is the wrong shape. The two
     * are separate because only one of them can fail.
     */
    fun resize(newCols: Int, newRows: Int) {
        resizeLocked(newCols, newRows)
        onChanged?.invoke()
    }

    private fun resizeLocked(newCols: Int, newRows: Int) = synchronized(lock) {
        if (newCols <= 0 || newRows <= 0) return@synchronized
        if (newCols == cols && newRows == rows) return@synchronized
        // Only the screen being shown has the cursor; the other keeps the
        // last-row rule, and its saved cursor is clamped as before.
        val shownNormal = !onAlternate
        val n = normal.resize(newCols, newRows, bg, if (shownNormal) cursorRow else normal.rows - 1)
        val a = alternate.resize(newCols, newRows, bg, if (shownNormal) alternate.rows - 1 else cursorRow)
        cursorRow -= if (shownNormal) n else a
        tabs = BooleanArray(newCols) { it % TAB_WIDTH == 0 }
        scrollTop = 0
        scrollBottom = newRows - 1
        cursorRow = cursorRow.coerceIn(0, newRows - 1)
        cursorCol = cursorCol.coerceIn(0, newCols - 1)
        pendingWrap = false
        revision++
    }

    /**
     * Back to the state of a freshly opened terminal.
     *
     * Called on reconnect, before the replay is fed in, and that is not
     * optional: the daemon replays its scrollback to every attaching client,
     * so a terminal that still held the previous attachment's screen would
     * show the last few hundred lines twice.
     */
    fun reset() {
        resetLocked()
        onChanged?.invoke()
    }

    private fun resetLocked() = synchronized(lock) {
        normal.clearGrid(Colour.DEFAULT)
        normal.clearScrollback()
        alternate.clearGrid(Colour.DEFAULT)
        onAlternate = false
        cursorRow = 0
        cursorCol = 0
        pendingWrap = false
        resetPen()
        savedRow = 0
        savedCol = 0
        savedFg = Colour.DEFAULT
        savedBg = Colour.DEFAULT
        savedAttrs = Attrs.NONE
        scrollTop = 0
        scrollBottom = rows - 1
        applicationCursorKeys = false
        autoWrap = true
        bracketedPaste = false
        originMode = false
        insertMode = false
        mouseReporting = false
        cursorVisible = true
        title = ""
        lastPrinted = -1
        state = St.GROUND
        params.clear()
        group.clear()
        digits = -1
        utf8Remaining = 0
        stringBuf.setLength(0)
        responses.reset()
        tabs = BooleanArray(cols) { it % TAB_WIDTH == 0 }
        revision++
    }

    private fun respond(text: String) {
        responses.write(text.toByteArray(Charsets.UTF_8))
    }

    /**
     * What `OSC 10`/`OSC 11` answer with.
     *
     * Settable, because the renderer's theme is the truth here and it is the
     * app that knows it. A TUI asks these to decide whether it is on a light
     * or a dark background, and a phone in light mode that answered with a
     * dark ground would get an interface drawn for the wrong scheme.
     */
    var defaultForeground: Int = Colour.rgb(0xCD, 0xD6, 0xF4)
    var defaultBackground: Int = Colour.rgb(0x1A, 0x28, 0x2A)

    companion object {
        private const val ESC = 0x1B
        private const val BEL = 0x07
        private const val CSI = "\u001b["
        private const val REPLACEMENT = 0xFFFD
        private const val TAB_WIDTH = 8
        private const val MAX_STRING = 4096
        private const val MAX_TITLE = 256

        /**
         * What `ESC[>c` reports. `0` is "a VT100-family terminal", the version
         * is this protocol's, and `1` is the ROM cartridge nobody has had
         * since 1983 and every terminal still sends.
         */
        private const val SECONDARY_DA_VERSION = 1

        /** What `ESC[>q` (XTVERSION) reports. Named, so a TUI's log says who it was. */
        private const val XTVERSION = "rime-remote(1)"

        /**
         * A cell's size in pixels, for `ESC[14t`.
         *
         * Made up, and it has to be: a phone's cell size depends on the font
         * the renderer picked and the density of the screen. What matters is
         * that the answer is consistent with the grid — an image-capable TUI
         * divides the pixel size by the grid to work out a cell, and two
         * answers that disagreed would have it draw at the wrong scale.
         */
        private const val CELL_PIXELS_W = 8
        private const val CELL_PIXELS_H = 17

        /**
         * How many columns a code point occupies.
         *
         * Not a full Unicode width table — that is a megabyte of data for a
         * phone to carry — but the ranges that actually appear: CJK, Hangul,
         * the fullwidth forms, and the emoji blocks. Combining marks and
         * format characters are zero. Everything else is one.
         */
        fun charWidth(code: Int): Int {
            if (code == Cell.WIDE_TAIL) return 1
            when (Character.getType(code).toByte()) {
                Character.NON_SPACING_MARK,
                Character.ENCLOSING_MARK,
                Character.COMBINING_SPACING_MARK,
                Character.FORMAT,
                -> if (code != 0x00AD) return 0
            }
            return when (code) {
                in 0x1100..0x115F, // Hangul Jamo
                in 0x2E80..0x303E, // CJK radicals, Kangxi, CJK symbols
                in 0x3041..0x33FF, // kana through CJK compatibility
                in 0x3400..0x4DBF, // CJK extension A
                in 0x4E00..0x9FFF, // CJK unified
                in 0xA000..0xA4CF, // Yi
                in 0xAC00..0xD7A3, // Hangul syllables
                in 0xF900..0xFAFF, // CJK compatibility ideographs
                in 0xFE30..0xFE6F, // CJK compatibility forms
                in 0xFF00..0xFF60, // fullwidth forms
                in 0xFFE0..0xFFE6,
                in 0x1F300..0x1F64F, // emoji
                in 0x1F900..0x1F9FF,
                in 0x20000..0x3FFFD, // CJK extensions B and beyond
                -> 2
                else -> 1
            }
        }
    }
}
