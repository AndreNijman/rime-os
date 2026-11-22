package com.rimeos.remote.core.input

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class TypedTextTest {
    private val s = TypedText.SENTINEL

    @Test
    fun `committed text is sent and the field goes back to empty`() {
        val r = TypedText.read("${s}hi", null)
        assertEquals(0, r.backspaces)
        assertEquals("hi", r.typed)
        assertEquals(TypedText.EMPTY, r.next)
    }

    @Test
    fun `text still being composed is never sent`() {
        val r = TypedText.read("${s}hel", 1..3)
        assertEquals("", r.typed)
        assertEquals(TypedText.Field("${s}hel", 4, 1..3), r.next)
        // The keyboard changes its mind: still nothing.
        val r2 = TypedText.read("${s}help", 1..4)
        assertEquals("", r2.typed)
        // It commits: now the word goes, once.
        val r3 = TypedText.read("${s}help ", null)
        assertEquals("help ", r3.typed)
    }

    @Test
    fun `committed text before a new composition goes, the composition stays`() {
        val r = TypedText.read("${s}hello wo", 7..8)
        assertEquals("hello ", r.typed)
        assertEquals(TypedText.Field("${s}wo", 3, 1..2), r.next)
    }

    @Test
    fun `deleting the sentinel is a backspace`() {
        val r = TypedText.read("", null)
        assertEquals(1, r.backspaces)
        assertEquals("", r.typed)
        assertEquals(TypedText.EMPTY, r.next)
    }

    @Test
    fun `a backspace inside a composition only changes the composition`() {
        val r = TypedText.read("${s}he", 1..2)
        assertEquals(0, r.backspaces)
        assertEquals("", r.typed)
    }

    @Test
    fun `a broken composing range is read as nothing composed`() {
        val r = TypedText.read("${s}ab", 5..9)
        assertEquals("ab", r.typed)
    }

    @Test
    fun `line breaks and tabs are keys, other control characters are dropped`() {
        assertEquals(
            listOf(
                InputEvent.Text("a"),
                InputEvent.Tap(KeyMap.ENTER),
                InputEvent.Text("b"),
                InputEvent.Tap(KeyMap.TAB),
                InputEvent.Text("cd"),
                InputEvent.Tap(KeyMap.ENTER),
            ),
            TypedText.events("a\r\nb\tc\u0007d\n"),
        )
        assertEquals(emptyList<InputEvent>(), TypedText.events("$s\u0000"))
    }
}
