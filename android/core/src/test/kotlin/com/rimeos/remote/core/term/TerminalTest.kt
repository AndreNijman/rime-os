package com.rimeos.remote.core.term

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The emulator, one property at a time.
 *
 * P1-055 asks for "real PTY output with ANSI 16/256/truecolor and text
 * attributes". Each of those three colour depths is a different code path with
 * a different way of being wrong, so each has its own test that names the
 * colour it expects rather than asserting "some colour was set".
 */
class TerminalTest {
    private fun term(cols: Int = 20, rows: Int = 5) = Terminal(cols, rows)

    private val esc = Ansi.ESC

    // ---- colour ---------------------------------------------------------

    @Test
    fun `the sixteen ANSI colours land as palette indices`() {
        val t = term()
        t.feed("$esc[31mR$esc[42mG$esc[0mP")
        assertEquals(Colour.indexed(1), t.screen.cellAt(0, 0).fg, "SGR 31 is palette 1")
        assertEquals(Colour.DEFAULT, t.screen.cellAt(0, 0).bg)
        assertEquals(Colour.indexed(2), t.screen.cellAt(0, 1).bg, "SGR 42 is background palette 2")
        assertEquals(Colour.indexed(1), t.screen.cellAt(0, 1).fg, "the foreground survived a background change")
        assertEquals(Colour.DEFAULT, t.screen.cellAt(0, 2).fg, "SGR 0 put it back")
        assertEquals(Colour.DEFAULT, t.screen.cellAt(0, 2).bg)
    }

    @Test
    fun `the bright ANSI colours are the upper eight and not bold`() {
        // The bug this catches is old and common: 90..97 treated as "bold plus
        // 30..37", which paints bright red as bold dark red on any renderer
        // that does not conflate the two.
        val t = term()
        t.feed("$esc[91mx$esc[101my")
        assertEquals(Colour.indexed(9), t.screen.cellAt(0, 0).fg)
        assertFalse(Attrs.has(t.screen.cellAt(0, 0).attrs, Attrs.BOLD), "91 is a colour, not a weight")
        assertEquals(Colour.indexed(9), t.screen.cellAt(0, 1).bg)
    }

    @Test
    fun `256-colour indices arrive in both the semicolon and the colon form`() {
        val t = term()
        t.feed("$esc[38;5;196ma")
        t.feed("$esc[38:5:46mb")
        t.feed("$esc[48;5;17mc")
        assertEquals(Colour.indexed(196), t.screen.cellAt(0, 0).fg)
        assertEquals(Colour.indexed(46), t.screen.cellAt(0, 1).fg, "the colon form is what modern TUIs emit")
        assertEquals(Colour.indexed(17), t.screen.cellAt(0, 2).bg)
    }

    @Test
    fun `truecolor arrives in all three forms that appear in the fixtures`() {
        val t = term()
        // Semicolon: what Claude Code emits (`ESC[38;2;215;119;87m`).
        t.feed("$esc[38;2;215;119;87ma")
        // Colon with the colour-space slot, as ITU T.416 defines it.
        t.feed("$esc[38:2::1:2:3mb")
        // Colon without it, which several libraries emit.
        t.feed("$esc[48:2:9:8:7mc")
        assertEquals(Colour.rgb(215, 119, 87), t.screen.cellAt(0, 0).fg)
        assertEquals(Colour.rgb(1, 2, 3), t.screen.cellAt(0, 1).fg)
        assertEquals(Colour.rgb(9, 8, 7), t.screen.cellAt(0, 2).bg)
        assertEquals(Colour.KIND_RGB, Colour.kind(t.screen.cellAt(0, 0).fg))
    }

    @Test
    fun `an extended colour consumes exactly its own parameters`() {
        // `38;5;9;1m` is an indexed red AND THEN bold. A parser that swallowed
        // a fixed count would lose the bold, and one that resynchronised
        // wrongly would paint the rest of the line in parameter 1's colour.
        val t = term()
        t.feed("$esc[38;5;9;1mx")
        assertEquals(Colour.indexed(9), t.screen.cellAt(0, 0).fg)
        assertTrue(Attrs.has(t.screen.cellAt(0, 0).attrs, Attrs.BOLD), "the trailing 1 is still bold")
    }

    @Test
    fun `an unknown extended colour form advances rather than looping`() {
        // `38;9` names neither 2 nor 5. The far end is a remote peer, so a
        // parser that did not advance here would be a hang it could trigger.
        val t = term()
        t.feed("$esc[38;9;1mx")
        assertEquals('x', t.screen.cellAt(0, 0).text.single())
    }

    @Test
    fun `the 256-colour cube resolves the way xterm defines it`() {
        assertEquals(null, Colour.resolveCube(9), "0..15 belong to the renderer's theme")
        assertEquals(Colour.rgb(0, 0, 0), Colour.resolveCube(16))
        assertEquals(Colour.rgb(255, 255, 255), Colour.resolveCube(231))
        assertEquals(Colour.rgb(8, 8, 8), Colour.resolveCube(232))
        assertEquals(Colour.rgb(238, 238, 238), Colour.resolveCube(255))
        assertEquals(Colour.rgb(95, 135, 175), Colour.resolveCube(67))
    }

    // ---- attributes -----------------------------------------------------

    @Test
    fun `every text attribute sets and clears independently`() {
        val t = term(cols = 40)
        t.feed("$esc[1;2;3;4;5;7;8;9mA")
        val on = t.screen.cellAt(0, 0).attrs
        for (flag in listOf(
            Attrs.BOLD, Attrs.DIM, Attrs.ITALIC, Attrs.UNDERLINE,
            Attrs.BLINK, Attrs.INVERSE, Attrs.HIDDEN, Attrs.STRIKE,
        )) {
            assertTrue(Attrs.has(on, flag), "${Attrs.describe(flag)} from one combined SGR")
        }
        // 22 clears bold AND dim together, which is the one people get wrong.
        t.feed("$esc[22;23;24;25;27;28;29mB")
        assertEquals(Attrs.NONE, t.screen.cellAt(0, 1).attrs, Attrs.describe(t.screen.cellAt(0, 1).attrs))
    }

    @Test
    fun `underline with a style subparameter is still an underline and zero turns it off`() {
        val t = term()
        t.feed("$esc[4:3mA$esc[4:0mB")
        assertTrue(Attrs.has(t.screen.cellAt(0, 0).attrs, Attrs.UNDERLINE), "4:3 is a curly underline")
        assertFalse(Attrs.has(t.screen.cellAt(0, 1).attrs, Attrs.UNDERLINE), "4:0 is none")
    }

    // ---- geometry -------------------------------------------------------

    @Test
    fun `a character in the last column does not move the cursor off the edge`() {
        val t = term(cols = 3, rows = 3)
        t.feed("abc")
        assertEquals(2, t.cursorCol, "the cursor stays on the last column with the wrap pending")
        assertEquals(0, t.cursorRow)
        t.feed("d")
        assertEquals("abc\nd", t.screen.visibleText().trimEnd())
        assertEquals(1, t.cursorRow)
        assertEquals(1, t.cursorCol)
    }

    @Test
    fun `a backspace after the last column lands on the character just printed`() {
        val t = term(cols = 3, rows = 2)
        t.feed("abc")
        t.feed("\b")
        t.feed("X")
        assertEquals("abX", t.screen.row(0).text())
    }

    @Test
    fun `autowrap off overwrites the last column instead of moving on`() {
        val t = term(cols = 3, rows = 2)
        t.feed("$esc[?7l")
        t.feed("abcdef")
        assertEquals("abf", t.screen.row(0).text())
        assertEquals("", t.screen.row(1).text().trim())
    }

    @Test
    fun `a wrapped line is marked so copying it does not insert a newline`() {
        val t = term(cols = 4, rows = 3)
        t.feed("abcdefg")
        assertTrue(t.screen.row(0).wrapped, "the first line ran off the edge")
        assertEquals("abcdefg", t.screen.textBetween(Pos(0, 0), Pos(1, 2)))
    }

    // ---- erasing --------------------------------------------------------

    @Test
    fun `erasing paints the current background rather than the default`() {
        // This is how a TUI draws a coloured panel, and erasing to the default
        // background is what leaves it striped.
        val t = term(cols = 6, rows = 2)
        t.feed("$esc[44m$esc[2J")
        assertEquals(Colour.indexed(4), t.screen.cellAt(0, 0).bg)
        assertEquals(Colour.indexed(4), t.screen.cellAt(1, 5).bg)
        assertEquals(Colour.DEFAULT, t.screen.cellAt(0, 0).fg)
    }

    @Test
    fun `erase in line and in display cover the three regions each`() {
        val t = term(cols = 5, rows = 3)
        t.feed("abcde\r\nfghij\r\nklmno")
        t.feed("$esc[2;3H$esc[K")          // row 2, col 3, clear to end of line
        assertEquals("fg", t.screen.row(1).text().trimEnd())
        t.feed("$esc[3;3H$esc[1K")          // clear to start of line
        assertEquals("   no", t.screen.row(2).text())
        t.feed("$esc[1;3H$esc[0J")          // clear to end of display
        assertEquals("ab", t.screen.row(0).text().trimEnd())
        assertEquals("", t.screen.row(1).text().trim())
        assertEquals("", t.screen.row(2).text().trim())
    }

    // ---- scrolling and history ------------------------------------------

    @Test
    fun `lines that scroll off the top become scrollback`() {
        val t = term(cols = 8, rows = 2)
        t.feed("one\r\ntwo\r\nthree\r\nfour")
        assertEquals(2, t.screen.scrollbackSize)
        assertEquals("one", t.screen.lineAt(0)?.text()?.trimEnd())
        assertEquals("two", t.screen.lineAt(1)?.text()?.trimEnd())
        assertEquals("three\nfour", t.screen.visibleText())
        assertEquals("one\ntwo\nthree\nfour", t.screen.transcript())
    }

    @Test
    fun `a scroll region keeps its lines out of the history`() {
        // The lines leaving a region were never the transcript: a region is how
        // a TUI keeps a status bar still while a pane scrolls under it.
        val t = term(cols = 8, rows = 4)
        t.feed("$esc[2;3r")            // region is rows 2..3
        t.feed("$esc[2;1H")
        t.feed("a\r\nb\r\nc\r\nd")
        assertEquals(0, t.screen.scrollbackSize, "a region's lines are not history")
    }

    @Test
    fun `the alternate screen has no history and gives the first one back`() {
        val t = term(cols = 8, rows = 2)
        t.feed("keep\r\nthis\r\nand this")
        val before = t.screen.transcript()
        t.feed("$esc[?1049h")
        assertTrue(t.onAlternate)
        t.feed("full screen app\r\nsecond line\r\nthird")
        assertEquals(0, t.screen.scrollbackSize, "an editor's redraws are not a transcript")
        t.feed("$esc[?1049l")
        assertFalse(t.onAlternate)
        assertEquals(before, t.screen.transcript(), "the first screen came back unchanged")
    }

    @Test
    fun `the scrollback is capped and says how much it dropped`() {
        val t = Terminal(cols = 8, rows = 2, scrollbackLimit = 3)
        repeat(10) { t.feed("l$it\r\n") }
        assertEquals(3, t.screen.scrollbackSize)
        assertTrue(t.screen.dropped > 0, "a capped ring that never drops is not capped")
    }

    // ---- insert and delete ----------------------------------------------

    @Test
    fun `insert and delete move a line sideways without touching its neighbours`() {
        val t = term(cols = 6, rows = 2)
        t.feed("abcdef\r\nZZZZZZ")
        t.feed("$esc[1;3H$esc[2@")     // insert two blanks at column 3
        assertEquals("ab  cd", t.screen.row(0).text())
        assertEquals("ZZZZZZ", t.screen.row(1).text())
        t.feed("$esc[1;1H$esc[2P")     // delete two at the start
        assertEquals("  cd  ", t.screen.row(0).text())
    }

    @Test
    fun `insert and delete line work inside the scroll region only`() {
        val t = term(cols = 4, rows = 4)
        t.feed("aaaa\r\nbbbb\r\ncccc\r\ndddd")
        t.feed("$esc[2;3r$esc[2;1H$esc[L")
        assertEquals("aaaa", t.screen.row(0).text())
        assertEquals("", t.screen.row(1).text().trim())
        assertEquals("bbbb", t.screen.row(2).text())
        assertEquals("dddd", t.screen.row(3).text(), "outside the region, untouched")
    }

    // ---- text -----------------------------------------------------------

    @Test
    fun `UTF-8 decodes across all four lengths`() {
        val t = term(cols = 20)
        t.feed("aé→😀")
        assertEquals("a", t.screen.cellAt(0, 0).text)
        assertEquals("é", t.screen.cellAt(0, 1).text)
        assertEquals("→", t.screen.cellAt(0, 2).text)
        assertEquals("😀", t.screen.cellAt(0, 3).text)
    }

    @Test
    fun `a double-width character occupies two cells and copies out as one`() {
        val t = term(cols = 10)
        t.feed("日本")
        assertEquals("日", t.screen.cellAt(0, 0).text)
        assertEquals("", t.screen.cellAt(0, 1).text, "the tail of a wide cell contributes nothing")
        assertEquals("本", t.screen.cellAt(0, 2).text)
        assertEquals("日本", t.screen.row(0).text().trimEnd())
        assertEquals(4, t.cursorCol)
    }

    @Test
    fun `a truncated UTF-8 sequence does not eat the character after it`() {
        val t = term(cols = 10)
        // A two-byte lead followed by an ASCII byte: the sequence is broken,
        // and `x` must still appear.
        t.feed(byteArrayOf(0xC3.toByte(), 'x'.code.toByte()))
        assertTrue(t.screen.row(0).text().contains("x"), "got '${t.screen.row(0).text()}'")
    }

    @Test
    fun `a tab goes to the next eight-column stop`() {
        val t = term(cols = 30)
        t.feed("a\tb\tc")
        assertEquals("a       b       c", t.screen.row(0).text().trimEnd())
    }

    @Test
    fun `repeat prints the last character again`() {
        val t = term(cols = 10)
        t.feed("-$esc[5b")
        assertEquals("------", t.screen.row(0).text().trimEnd())
    }

    // ---- strings that must not reach the grid ----------------------------

    @Test
    fun `an OSC title is taken and never printed`() {
        val t = term(cols = 30)
        t.feed("$esc]0;a machine\u0007done")
        assertEquals("a machine", t.title)
        assertEquals("done", t.screen.row(0).text().trimEnd(), "the OSC body must not leak onto the screen")
    }

    @Test
    fun `an APC or PM string is swallowed whole`() {
        // opencode sends `ESC _ G i=31337,... ESC \` — a kitty graphics probe.
        // A terminal that printed its body would fill the screen with it.
        val t = term(cols = 40)
        t.feed("${esc}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA$esc\\ok")
        assertEquals("ok", t.screen.row(0).text().trimEnd())
    }

    @Test
    fun `a character set designation is consumed rather than printed`() {
        val t = term(cols = 10)
        t.feed("$esc(Bhi")
        assertEquals("hi", t.screen.row(0).text().trimEnd(), "the B of `ESC ( B` is not a letter to print")
    }

    @Test
    fun `an unknown CSI is dropped without printing its parameters`() {
        val t = term(cols = 20)
        t.feed("$esc[42;9;1;;zhello")
        assertEquals("hello", t.screen.row(0).text().trimEnd())
    }

    // ---- reset ----------------------------------------------------------

    @Test
    fun `reset clears the screen the history and every mode`() {
        val t = term(cols = 8, rows = 2)
        t.feed("a\r\nb\r\nc")
        t.feed("$esc[?1h$esc[?2004h$esc[31m$esc[?1049h")
        t.reset()
        assertEquals(0, t.screen.scrollbackSize)
        assertEquals("", t.screen.transcript().trim())
        assertFalse(t.applicationCursorKeys)
        assertFalse(t.bracketedPaste)
        assertFalse(t.onAlternate)
        assertEquals(Colour.DEFAULT, t.fg)
        assertEquals(0, t.cursorRow)
    }

    @Test
    fun `the revision changes when bytes arrive and a renderer can key on it`() {
        val t = term()
        val before = t.revision
        t.feed("x")
        assertNotEquals(before, t.revision)
    }

    // ---- resize ---------------------------------------------------------

    @Test
    fun `a narrower terminal truncates and a taller one keeps the history`() {
        val t = term(cols = 10, rows = 2)
        t.feed("0123456789\r\nabcdefghij")
        t.resize(5, 2)
        assertEquals(5, t.cols)
        assertEquals("01234", t.screen.row(0).text())
        t.resize(5, 4)
        assertEquals(4, t.rows)
    }

    @Test
    fun `shrinking the height keeps the bottom on screen and the top in history`() {
        val t = term(cols = 8, rows = 4)
        t.feed("one\r\ntwo\r\nthree\r\nfour")
        t.resize(8, 2)
        assertEquals("three\nfour", t.screen.visibleText())
        assertEquals(2, t.screen.scrollbackSize)
        t.feed("!")
        assertEquals("four!", t.screen.row(1).text().trimEnd())
    }

    @Test
    fun `shrinking with the cursor near the top keeps the prompt where it is`() {
        // A fresh shell, and the keyboard opening takes three rows.
        val t = term(cols = 10, rows = 5)
        t.feed("prompt$ ")
        t.resize(10, 2)
        assertEquals("prompt$", t.screen.row(0).text().trimEnd())
        assertEquals(0, t.screen.scrollbackSize)
        t.feed("ls")
        assertEquals("prompt$ ls", t.screen.row(0).text().trimEnd())
    }

    @Test
    fun `shrinking drops the blank rows below the cursor before the top`() {
        val t = term(cols = 8, rows = 5)
        t.feed("a\r\nb\r\nc")
        t.resize(8, 2)
        assertEquals("b\nc", t.screen.visibleText())
        assertEquals(1, t.screen.scrollbackSize)
        t.feed("!")
        assertEquals("c!", t.screen.row(1).text().trimEnd())
    }

    @Test
    fun `a written row below the cursor goes to history rather than being lost`() {
        val t = term(cols = 8, rows = 4)
        // Cursor back on the top row with text written below it, as a
        // full-screen program leaves things.
        t.feed("top\r\nmid\r\nlow${esc}[1;1H")
        t.resize(8, 2)
        assertEquals("mid\nlow", t.screen.visibleText())
        assertEquals(1, t.screen.scrollbackSize)
    }
}
